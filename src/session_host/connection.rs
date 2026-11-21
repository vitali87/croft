//! One client's connection: its thread, whose typing reaches the PTY, granting and
//! revoking write control and the rules for it, and disconnecting a participant.

use super::*;

/// Serve one client connection until it detaches or its socket dies.
pub(super) fn client_thread(host: &Host, mut stream: UnixStream) {
    let tx = match stream.try_clone() {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(_) => return,
    };
    let mut reader = FrameReader::new();
    let mut buf = [0u8; 16384];
    let mut my_id: Option<u64> = None;
    let mut privileged = false;
    'conn: loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for frame in reader.push(&buf[..n]) {
            match frame {
                Frame::Control(Control::Inner { token })
                    if my_id.is_none() && token == host.token =>
                {
                    privileged = true;
                    host.privileged.lock().unwrap().push(Arc::clone(&tx));
                    // Catch the channel up on who is currently typing: a
                    // writer change announced before this registration would
                    // otherwise never be repeated for the same writer.
                    let current = *host.last_writer.lock().unwrap();
                    if let Some(id) = current {
                        write_frame_bounded(
                            &mut tx.lock().unwrap(),
                            &encode_control_frame(&Control::Typing { id }),
                            Instant::now() + WRITE_FRAME_DEADLINE,
                        );
                    }
                }
                Frame::Control(Control::Hello {
                    name,
                    cols,
                    rows,
                    client_id,
                    version,
                }) if my_id.is_none() && !privileged => {
                    // Answer with our version FIRST: registration below makes
                    // this client a broadcast target, and the ServerHello must
                    // beat any PTY bytes so the attaching client's version
                    // phase ends on the first frame (#53). Queueing it before
                    // the client is registered preserves that ordering
                    // without writing to the socket from this thread: the
                    // outbox is FIFO and its writer is the only producer on
                    // the wire, so a direct write here could otherwise
                    // interleave mid-frame with a broadcast and desync the
                    // peer's FrameReader.
                    let outbox = Outbox::new();
                    outbox.push(&encode_control_frame(&Control::ServerHello {
                        version: String::from(env!("CARGO_PKG_VERSION")),
                    }));
                    let fd = {
                        use std::os::fd::AsRawFd;
                        tx.lock().unwrap().as_raw_fd()
                    };
                    spawn_writer(Arc::clone(&tx), Arc::clone(&outbox), fd);
                    let id = host.next_id.fetch_add(1, Ordering::Relaxed);
                    let mut refused = false;
                    let displaced = {
                        let mut clients = host.clients.lock().unwrap();
                        // This host is replacing its process image (#238):
                        // seating this client would hand it a session that
                        // ends in an EOF at the exec, which reads as "the
                        // session is over" and drops the user out of a
                        // perfectly live session (#321). Send it round to
                        // the successor instead - the listening socket
                        // never unbinds, so the reconnect lands there.
                        // Checked under this lock, the same one the latch is
                        // written under, so the decision cannot straddle it.
                        if host.swapping.load(Ordering::SeqCst) {
                            refused = true;
                            // Queue the goodbye and enrol it in the swap's
                            // delivery fence WHILE HOLDING this lock: the
                            // swap seals the fence by taking the same lock,
                            // so the pair cannot straddle the exec and leave
                            // this client with a bare EOF.
                            if let Some(seq) =
                                outbox.push_seq(&encode_control_frame(&Control::HostSwap))
                            {
                                host.farewells
                                    .lock()
                                    .unwrap()
                                    .push((Arc::clone(&outbox), seq));
                            }
                        }
                        // Evict this client's own earlier registration
                        // (#229). A reconnect after a dead SSH transport
                        // arrives as a brand-new connection while the stale
                        // one is still seated and undetectably half-open;
                        // the client_id match is what lets us tell "the same
                        // client came back" from "a second person joined".
                        // An empty id matches nothing, so clients that send
                        // none are unaffected.
                        // Partition rather than clone field by field: the
                        // ghosts are MOVED out, so a `Client` field added
                        // later cannot be silently dropped on this path.
                        let displaced: Vec<Client> = if refused || client_id.is_empty() {
                            Vec::new()
                        } else {
                            let (taken, kept): (Vec<Client>, Vec<Client>) =
                                std::mem::take(&mut *clients)
                                    .into_iter()
                                    .partition(|c| c.client_id == client_id);
                            *clients = kept;
                            taken
                        };
                        // Write control auto-attaches only when nobody holds
                        // it: the first client, or an owner reattaching after
                        // every control holder left. Everyone else starts as
                        // a read-only observer until granted. A reconnecting
                        // client inherits the control its ghost held, so a
                        // dropped transport never silently demotes the owner
                        // to read-only.
                        // A 0x0 client is a pure observer (`croft record`,
                        // #356): it never shrinks the PTY, and it never takes
                        // control by default either, or a recorder attached
                        // first would leave the next real client read-only.
                        let pure_observer = cols == 0 && rows == 0;
                        let control = displaced.iter().any(|c| c.control)
                            || (!pure_observer && !clients.iter().any(|c| c.control));
                        if !refused {
                            clients.push(Client {
                                id,
                                name,
                                version,
                                cols,
                                rows,
                                control,
                                tx: Arc::clone(&tx),
                                fd,
                                outbox: Arc::clone(&outbox),
                                client_id,
                            });
                        }
                        displaced
                    };
                    if refused {
                        // The goodbye was queued above, under the clients
                        // lock. It rides this connection's own writer thread
                        // and the outbox is FIFO, so it lands after the
                        // ServerHello rather than interleaving mid-frame.
                        outbox.close_when_drained();
                        break 'conn;
                    }
                    my_id = Some(id);
                    // Tear the ghosts down outside the clients lock: killing
                    // an outbox wakes its writer, and shutdown unblocks the
                    // ghost's client thread so it deregisters (finding
                    // itself already gone) rather than lingering.
                    for ghost in displaced {
                        ghost.outbox.kill();
                        shutdown_now(ghost.fd);
                    }
                    apply_winsize(host, true);
                    update_presence(host);
                }
                Frame::Bytes(bytes) => {
                    let Some(id) = my_id else { continue };
                    let has_control = {
                        let clients = host.clients.lock().unwrap();
                        clients.iter().any(|c| c.id == id && c.control)
                    };
                    // Server-side enforcement: a read-only client's input
                    // never reaches the PTY (unlike abduco's advisory -r).
                    if has_control {
                        announce_typing(host, id);
                        let mut pty = host.pty_input.lock().unwrap();
                        if pty.write_all(&bytes).and_then(|_| pty.flush()).is_err() {
                            break 'conn;
                        }
                    }
                }
                Frame::Control(Control::Resize { cols, rows }) => {
                    let Some(id) = my_id else { continue };
                    {
                        let mut clients = host.clients.lock().unwrap();
                        if let Some(c) = clients.iter_mut().find(|c| c.id == id) {
                            c.cols = cols;
                            c.rows = rows;
                        }
                    }
                    apply_winsize(host, false);
                    update_presence(host);
                }
                Frame::Control(Control::Grant { id: target }) => {
                    set_control(host, privileged, my_id, target, true);
                }
                Frame::Control(Control::Revoke { id: target }) => {
                    set_control(host, privileged, my_id, target, false);
                }
                Frame::Control(Control::Kick { id: target }) => {
                    kick(host, privileged, my_id, target);
                }
                Frame::Control(Control::Claim) => {
                    if let Some(id) = my_id {
                        set_control(host, privileged, my_id, id, true);
                    }
                }
                Frame::Control(Control::Detach) => break 'conn,
                _ => {}
            }
        }
    }
    if let Some(id) = my_id {
        {
            let mut clients = host.clients.lock().unwrap();
            // Kill the outbox on the way out so this client's writer thread
            // wakes and exits instead of parking on an empty queue forever.
            if let Some(c) = clients.iter().find(|c| c.id == id) {
                c.outbox.kill();
            }
            clients.retain(|c| c.id != id);
        }
        apply_winsize(host, false);
        update_presence(host);
    }
}

/// Tell the privileged channels whose input is about to reach the PTY,
/// only when the writer changed (never per keystroke). Ordered before the
/// PTY write so the attribution precedes the keystrokes it covers.
pub(super) fn announce_typing(host: &Host, id: u64) {
    {
        let mut last = host.last_writer.lock().unwrap();
        if *last == Some(id) {
            return;
        }
        *last = Some(id);
    }
    let frame = encode_control_frame(&Control::Typing { id });
    let mut channels = host.privileged.lock().unwrap();
    // Bounded, like every other server-originated write (#228): this sits
    // directly in front of the PTY write on the keystroke path, so an inner
    // channel that stopped draining would otherwise block the typing client
    // forever on a plain `write_all`.
    channels.retain(|tx| {
        write_frame_bounded(
            &mut tx.lock().unwrap(),
            &frame,
            Instant::now() + WRITE_FRAME_DEADLINE,
        )
    });
}

/// Grant or revoke write control on `target`, but only when the requester
/// is the privileged inner channel or itself holds control: read-only
/// guests cannot promote themselves — with one exception. When NOBODY holds
/// control (the #234 lockout state, however a future bug arrives at it), any
/// participant may claim control for themselves (#235): a vacant roster
/// makes the self-claim safe, and without it the state is a one-way door
/// that only a host kill reopens. Promoting *others* from the floor stays
/// privileged.
pub(super) fn set_control(
    host: &Host,
    privileged: bool,
    requester: Option<u64>,
    target: u64,
    grant: bool,
) {
    let changed = {
        let mut clients = host.clients.lock().unwrap();
        let allowed = control_change_allowed(
            privileged,
            requester,
            target,
            grant,
            clients.iter().map(|c| (c.id, c.control)),
        );
        if !allowed {
            false
        } else {
            match clients.iter_mut().find(|c| c.id == target) {
                Some(c) if c.control != grant => {
                    c.control = grant;
                    true
                }
                _ => false,
            }
        }
    };
    if changed {
        update_presence(host);
    }
}

/// The permission rule of [`set_control`], over `(id, holds_control)` pairs
/// so it is testable without a live host. The vacant-claim arm grants no
/// privilege that a vacant roster does not already offer: a fresh attach
/// when nobody holds control gains control by the registration rule, so a
/// claim is the same acquisition without the detach/reattach churn.
pub(super) fn control_change_allowed(
    privileged: bool,
    requester: Option<u64>,
    target: u64,
    grant: bool,
    roster: impl Iterator<Item = (u64, bool)> + Clone,
) -> bool {
    let vacant = !roster.clone().any(|(_, control)| control);
    privileged
        || requester.is_some_and(|id| roster.clone().any(|(cid, control)| cid == id && control))
        || (grant && vacant && requester == Some(target))
}

/// #234 defense in depth: while at least one participant is attached,
/// someone must hold write control. A roster resting all-read-only is a
/// total lockout — read-only input is dropped server-side, and
/// `set_control` normally requires a control holder, so no client could
/// recover the session (observed live 2026-08-22: a single attached
/// participant with `control: false`; only killing the host recovered it).
/// Called after every roster mutation that can remove a holder; grants to
/// the most recent attach (ids are monotonic).
/// The pump's claim trigger: this client is the ONLY attached participant
/// and holds no control — the #234 lockout state, where read-only input is
/// dropped at the host and the participants UI (inner croft, behind that
/// same gate) is unreachable, so no keystroke could ever recover the
/// session. Sole-participant only, on purpose: with others attached,
/// control transfer stays an explicit act (grant, or the fresh-attach rule
/// — see `control_moves_to_next_attacher_after_holder_detaches`), and a
/// guest's pump must never outrace a returning owner. A lone client
/// receiving the roster IS its one participant, so no id bookkeeping is
/// needed.
pub(super) fn sole_participant_lacks_control(participants: &[Participant]) -> bool {
    matches!(participants, [p] if !p.control)
}

/// Disconnect participant `target` (privileged channel or a control holder
/// only). Shutting the stream down unblocks the target's client thread,
/// which then deregisters and updates presence itself.
pub(super) fn kick(host: &Host, privileged: bool, requester: Option<u64>, target: u64) {
    let clients = host.clients.lock().unwrap();
    let allowed =
        privileged || requester.is_some_and(|id| clients.iter().any(|c| c.id == id && c.control));
    if !allowed {
        return;
    }
    if let Some(c) = clients.iter().find(|c| c.id == target) {
        // Kill the outbox too: the writer thread may be parked on a wedged
        // socket, and shutdown alone would leave it holding the stream lock
        // until its deadline expired.
        c.outbox.kill();
        shutdown_now(c.fd);
    }
}
