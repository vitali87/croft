//! The session host's own tests, moved out of session_host.rs (#2062).

use super::*;

/// Tests must never authenticate to the developer's live session host.
/// The host puts `CROFT_SESSION_SOCKET`/`CROFT_SESSION_TOKEN` in every
/// inner croft's environment, and that environment reaches a `cargo
/// test` run from a croft-hosted shell — so each test-built `App` used
/// to open a real `UnixStream` to the running host and send the
/// `Control::Inner { token }` frame as a construction side effect
/// (#60). Under `cfg(test)` the env read must be inert no matter what
/// the environment holds; tests that want a channel dial a socket they
/// own via `InnerChannel::connect`. (Asserted through `from_env`
/// itself rather than by mutating the environment: `set_var` races
/// sibling test threads, #37.)
#[test]
fn from_env_is_hermetic_under_test() {
    assert!(
        InnerChannel::from_env().is_none(),
        "a test-built App must not connect to the launching shell's session host"
    );
}

/// #229: the client identity must distinguish two terminals that opened
/// the SAME path. The relay key cannot do that alone — it is a pure hash
/// of the launch arg (`remote::relay_session_id`), so both launches
/// derive it identically and the host would read the second attach as
/// the first reconnecting and evict it: a ghost fix that kicks a real
/// participant off. The per-process nonce is what separates them.
#[test]
fn two_clients_sharing_a_launch_arg_get_distinct_identities() {
    // Same relay key on purpose: this is the colliding case.
    let key = "same-launch-arg-hash";
    let a = compose_client_identity(key, &crate::remote::client_process_nonce());
    let b = compose_client_identity(key, &crate::remote::client_process_nonce());
    assert_ne!(
        a, b,
        "two client processes sharing one launch arg must not share an identity"
    );
    // Same process reconnecting: the nonce is minted once and carried,
    // so the identity must be stable — that is what evicts the ghost.
    let nonce = crate::remote::client_process_nonce();
    assert_eq!(
        compose_client_identity(key, &nonce),
        compose_client_identity(key, &nonce),
        "one client's identity must survive its own reconnects"
    );
    // Either half missing yields empty, which displaces nothing. The
    // missing nonce is what `client_identity` reads when the variable is
    // unset (`unwrap_or_default`), and neither nonce is a literal: CodeQL
    // reports any constant passed to a parameter named `nonce` as a
    // hard-coded cryptographic nonce, and this one is not cryptographic.
    assert_eq!(compose_client_identity("", &nonce), "");
    assert_eq!(compose_client_identity(key, &String::default()), "");
}

#[test]
fn collab_socket_for_mux_swaps_only_the_socket_kind() {
    assert_eq!(
        collab_socket_for_mux(Path::new("/x/sessions/abc123.mux.sock")),
        PathBuf::from("/x/sessions/abc123.collab.sock")
    );
    // A non-mux name still lands on a .collab.sock sibling.
    assert_eq!(
        collab_socket_for_mux(Path::new("/x/sessions/abc123.sock")),
        PathBuf::from("/x/sessions/abc123.sock.collab.sock")
    );
}

/// A peer that stops draining its socket (stopped with SIGSTOP, or
/// itself blocked mid-write) used to wedge the writer forever: the
/// WouldBlock loop had no deadline, and on the UI thread that froze
/// croft with no escape. The write must give up and report failure.
#[test]
fn write_frame_blocking_gives_up_when_the_peer_never_drains() {
    use std::io::Write;
    let (mut a, b) = UnixStream::pair().unwrap();
    a.set_nonblocking(true).unwrap();
    // Fill the send buffer: the peer never reads.
    let chunk = [0u8; 8192];
    while a.write(&chunk).is_ok() {}
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ok = write_frame_blocking(&mut a, &[0u8; 65536]);
        let _ = tx.send(ok);
    });
    match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(ok) => assert!(!ok, "a wedged write must report failure, not success"),
        Err(_) => panic!("write_frame_blocking hung past its deadline on a wedged peer"),
    }
    drop(b);
}

/// Every retry path in the bounded write checks the deadline, not just
/// WouldBlock: a stream stuck returning Interrupted (a signal storm)
/// must also give up instead of spinning forever.
#[test]
fn write_frame_blocking_bounds_an_interrupt_storm() {
    struct InterruptForever;
    impl std::io::Write for InterruptForever {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ok = write_frame_blocking_with_deadline(
            &mut InterruptForever,
            &[0u8; 16],
            std::time::Instant::now() + Duration::from_millis(100),
        );
        let _ = tx.send(ok);
    });
    match rx.recv_timeout(Duration::from_secs(4)) {
        Ok(ok) => assert!(
            !ok,
            "an interrupt storm must report failure at the deadline"
        ),
        Err(_) => panic!("write_frame_blocking spun past its deadline on Interrupted"),
    }
}

#[test]
fn bytes_frame_round_trips_through_split_chunks() {
    let frame = encode_bytes_frame(b"hello \x1b[31mworld");
    let mut reader = FrameReader::new();
    // Feed byte-by-byte to prove partial-chunk buffering works.
    let mut out = Vec::new();
    for b in &frame {
        out.extend(reader.push(std::slice::from_ref(b)));
    }
    assert_eq!(out, vec![Frame::Bytes(b"hello \x1b[31mworld".to_vec())]);
}

#[test]
fn control_frame_round_trips_and_interleaves_with_bytes() {
    let hello = Control::Hello {
        name: String::from("vitali@mac"),
        cols: 120,
        rows: 40,
        version: String::from(env!("CARGO_PKG_VERSION")),
        client_id: String::from("relay-key-abc"),
    };
    let mut wire = encode_control_frame(&hello);
    wire.extend(encode_bytes_frame(b"ls\r"));
    wire.extend(encode_control_frame(&Control::Detach));
    let mut reader = FrameReader::new();
    let frames = reader.push(&wire);
    assert_eq!(
        frames,
        vec![
            Frame::Control(hello),
            Frame::Bytes(b"ls\r".to_vec()),
            Frame::Control(Control::Detach),
        ]
    );
}

#[test]
fn frame_reader_skips_malformed_control_json() {
    let mut wire = encode_frame(FRAME_CONTROL, b"not json at all");
    wire.extend(encode_bytes_frame(b"ok"));
    let mut reader = FrameReader::new();
    let frames = reader.push(&wire);
    assert_eq!(frames, vec![Frame::Bytes(b"ok".to_vec())]);
}

#[test]
fn min_winsize_takes_minimum_dimensions_across_clients() {
    assert_eq!(min_winsize(std::iter::empty()), None);
    assert_eq!(min_winsize([(120, 40)].into_iter()), Some((120, 40)));
    // Mins are taken per-dimension, not per-client: 100x30 here.
    assert_eq!(
        min_winsize([(120, 30), (100, 40)].into_iter()),
        Some((100, 30))
    );
}

#[test]
fn min_winsize_ignores_clients_with_unknown_size() {
    // A client whose terminal reports no size (0 in either dimension,
    // seen under `script` and some CI PTYs) must not shrink the shared
    // PTY to nothing.
    assert_eq!(
        min_winsize([(0, 0), (100, 40)].into_iter()),
        Some((100, 40))
    );
    assert_eq!(min_winsize([(120, 0), (0, 24)].into_iter()), None);
    assert_eq!(min_winsize([(0, 0)].into_iter()), None);
}

#[test]
fn presence_path_sits_next_to_socket() {
    let p = presence_path(Path::new("/x/sessions/ab.mux.sock"));
    assert_eq!(p, Path::new("/x/sessions/ab.mux.sock.presence.json"));
}

#[test]
fn detached_server_argv_replays_socket_workspace_and_inner() {
    let argv = detached_server_argv(
        Path::new("/x/s.mux.sock"),
        Some(Path::new("/work/repo")),
        &[String::from("croft"), String::from("/work/repo")],
    );
    assert_eq!(
        argv,
        vec![
            "session-host",
            "--serve",
            "--socket",
            "/x/s.mux.sock",
            "--workspace",
            "/work/repo",
            "--",
            "croft",
            "/work/repo",
        ]
    );
    let argv = detached_server_argv(Path::new("/x/s.mux.sock"), None, &[String::from("cat")]);
    assert_eq!(
        argv,
        vec![
            "session-host",
            "--serve",
            "--socket",
            "/x/s.mux.sock",
            "--",
            "cat"
        ]
    );
}

// ---- integration: a real server on a real socket around `cat` ----

struct TestClient {
    stream: UnixStream,
    reader: FrameReader,
}

impl TestClient {
    fn connect(socket: &Path, name: &str, cols: u16, rows: u16) -> Self {
        Self::connect_as(socket, name, cols, rows, "")
    }

    /// Attach with an explicit `client_id`, the stable per-client-process
    /// identity a reconnect reuses to displace its own ghost (#229).
    fn connect_as(socket: &Path, name: &str, cols: u16, rows: u16, client_id: &str) -> Self {
        let stream = UnixStream::connect(socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut c = Self {
            stream,
            reader: FrameReader::new(),
        };
        c.send(&encode_control_frame(&Control::Hello {
            name: String::from(name),
            cols,
            rows,
            version: String::from(env!("CARGO_PKG_VERSION")),
            client_id: String::from(client_id),
        }));
        c
    }

    fn send(&mut self, wire: &[u8]) {
        self.stream.write_all(wire).expect("send");
    }

    /// One best-effort read: the number of PTY bytes decoded, or None on
    /// a timeout. Unlike [`Self::read_until`] a quiet socket is a
    /// tolerable outcome, not a panic — used when the point of the test
    /// is to measure how promptly output flows.
    fn try_read_some(&mut self) -> Option<usize> {
        let mut buf = [0u8; 4096];
        let n = self.stream.read(&mut buf).ok()?;
        Some(
            self.reader
                .push(&buf[..n])
                .iter()
                .map(|f| match f {
                    Frame::Bytes(b) => b.len(),
                    _ => 0,
                })
                .sum(),
        )
    }

    /// Read frames until `pred` matches one, returning everything seen.
    fn read_until(&mut self, pred: impl Fn(&Frame) -> bool) -> Vec<Frame> {
        self.read_until_within(READ_UNTIL_DEADLINE, pred)
    }

    /// [`Self::read_until`] with an explicit budget. The socket carries a
    /// short read timeout and an expired read surfaces as `WouldBlock`
    /// (`TimedOut` on some platforms), which is "nothing yet", not a
    /// failure: unwrapping it turned a busy box into a panic reading
    /// `read: Os { code: 11 }` with no clue what the client was waiting
    /// for (issue #227). Only the budget below ends the wait, and it ends
    /// it with the frames seen so far.
    fn read_until_within(&mut self, budget: Duration, pred: impl Fn(&Frame) -> bool) -> Vec<Frame> {
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + budget;
        loop {
            // Never block past the budget: a read armed with the full poll
            // interval a hair before the deadline would overshoot it.
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out; saw {seen:?}");
            self.stream
                .set_read_timeout(Some(remaining.min(READ_POLL_INTERVAL)))
                .unwrap();
            let n = match self.stream.read(&mut buf) {
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(e) => panic!("read failed: {e}; saw {seen:?}"),
            };
            assert!(n > 0, "server closed early; saw {seen:?}");
            let frames = self.reader.push(&buf[..n]);
            let done = frames.iter().any(&pred);
            seen.extend(frames);
            if done {
                return seen;
            }
        }
    }
}

/// How long a `read_until` waits for the frame it wants. Generous: the
/// flood tests push megabytes through a real PTY, and a loaded box (the
/// whole suite running in parallel) is exactly when the old ten seconds
/// ran out.
const READ_UNTIL_DEADLINE: Duration = Duration::from_secs(30);
/// How long a single `read` blocks before the loop re-checks the deadline.
const READ_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Issue #227: a read that expires with nothing on the wire must keep
/// waiting until the budget runs out, and then report what the client was
/// waiting for. Waits for a frame the server never sends, on a budget far
/// shorter than one read timeout would have been, so the only way to reach
/// the timeout assert is by looping over `WouldBlock` instead of
/// unwrapping it.
#[test]
#[should_panic(expected = "timed out")]
fn read_until_waits_out_an_idle_socket_instead_of_unwrapping_would_block() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    let mut c = TestClient::connect(&socket, "idle", 80, 24);
    // `cat` echoes only what it is fed, and this client sends nothing, so
    // no Bytes frame can ever arrive.
    c.read_until_within(Duration::from_millis(300), |f| matches!(f, Frame::Bytes(_)));
}

fn output_text(frames: &[Frame]) -> String {
    let mut bytes = Vec::new();
    for f in frames {
        if let Frame::Bytes(b) = f {
            bytes.extend_from_slice(b);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn roster(frames: &[Frame]) -> Option<Vec<Participant>> {
    frames.iter().rev().find_map(|f| match f {
        Frame::Control(Control::Presence { participants }) => Some(participants.clone()),
        _ => None,
    })
}

const TEST_TOKEN: &str = "test-token";

fn spawn_test_server(socket: PathBuf) -> std::thread::JoinHandle<i32> {
    spawn_test_server_with(socket, vec![String::from("cat")])
}

fn spawn_test_server_with(socket: PathBuf, inner: Vec<String>) -> std::thread::JoinHandle<i32> {
    std::thread::spawn(move || serve_with_token(&socket, None, &inner, TEST_TOKEN).expect("serve"))
}

fn wait_alive(socket: &Path) {
    crate::test_budget::await_spawned(
        Duration::from_millis(1000),
        "the session host to bind its socket",
        || crate::session::is_alive(socket),
    );
}

/// A fresh host clears its predecessor's stale marker, but only once it
/// owns the socket: the removal has to follow the bind, or a host that
/// LOSES the bind race would wipe the marker belonging to the live host
/// that won it. So "the socket answers" does not imply "the marker is
/// gone" - there is a window between the two, and under load a test that
/// asserts on the instant after `wait_alive` lands inside it (#307).
fn wait_marker_cleared(socket: &Path) {
    crate::test_budget::await_spawned(
        Duration::from_millis(1000),
        "the fresh host to clear its predecessor's stale marker",
        || !stale_marker_path(socket).exists(),
    );
}

#[test]
fn serve_broadcasts_output_enforces_readonly_and_propagates_exit() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    // Socket must not be readable by other users.
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "socket mode must be 0600");
    }

    let mut a = TestClient::connect(&socket, "owner", 120, 40);
    // First client holds control and appears in presence.
    let frames = a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let ps = roster(&frames).unwrap();
    assert_eq!(ps.len(), 1);
    assert!(ps[0].control, "first client must hold write control");

    let mut b = TestClient::connect(&socket, "guest", 100, 50);
    let frames = b.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let ps = roster(&frames).unwrap();
    assert_eq!(ps.len(), 2);
    let guest = ps.iter().find(|p| p.name == "guest").unwrap();
    assert!(!guest.control, "second client must attach read-only");

    // Presence sidecar mirrors the roster.
    let sidecar = std::fs::read_to_string(presence_path(&socket)).unwrap();
    assert!(sidecar.contains("owner") && sidecar.contains("guest"));

    // Read-only input is dropped server-side; owner input reaches the
    // PTY (cat echoes it back to BOTH clients).
    b.send(&encode_bytes_frame(b"INTRUDER"));
    a.send(&encode_bytes_frame(b"hi\r"));
    let a_frames =
        a.read_until(|f| matches!(f, Frame::Bytes(b) if b.windows(2).any(|w| w == b"hi")));
    let b_frames =
        b.read_until(|f| matches!(f, Frame::Bytes(b) if b.windows(2).any(|w| w == b"hi")));
    assert!(!output_text(&a_frames).contains("INTRUDER"));
    assert!(!output_text(&b_frames).contains("INTRUDER"));

    // EOT at line start ends cat; server broadcasts Exit and returns
    // the inner exit code.
    a.send(&encode_bytes_frame(&[0x04]));
    let frames = a.read_until(|f| matches!(f, Frame::Control(Control::Exit { .. })));
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, Frame::Control(Control::Exit { code: 0 })))
    );
    assert_eq!(server.join().unwrap(), 0);
    assert!(!socket.exists(), "server must unlink its socket on exit");
}

/// #53 part 1 compat, old client -> new server: a pre-0.1.698 Hello
/// carries no version field and must keep parsing (serde `default`).
#[test]
fn a_versionless_hello_from_an_old_client_still_parses() {
    let old = br#"{"t":"hello","name":"v@mac","cols":120,"rows":40}"#;
    let parsed: Control = serde_json::from_slice(old).expect("old Hello parses");
    match parsed {
        Control::Hello {
            version,
            name,
            client_id,
            ..
        } => {
            assert_eq!(version, "", "the missing field defaults to empty");
            assert_eq!(name, "v@mac");
            // #229: the same tolerance covers client_id. An empty id
            // must never displace a seated participant, or an old
            // client attaching would kick every other old client off.
            assert_eq!(client_id, "", "the missing field defaults to empty");
        }
        other => panic!("expected Hello, got {other:?}"),
    }
}

/// #229: a client reconnecting after a dead transport must displace its
/// OWN earlier registration. The stale entry is undetectably half-open
/// over SSH, so without this the roster shows a phantom second
/// participant and it pins the shared winsize forever.
#[test]
fn a_reconnecting_client_displaces_its_own_ghost() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut first = TestClient::connect_as(&socket, "owner", 80, 24, "relay-key-1");
    first.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));

    // The transport dies but the socket stays open and undrained: this
    // is the ghost. The same client then reconnects with the same id.
    let mut back = TestClient::connect_as(&socket, "owner", 120, 40, "relay-key-1");
    // Take the last roster the server sends rather than waiting for a
    // specific size: without eviction the roster never reaches 1, and
    // the assertion below should be what reports that — naming the
    // phantom participant — instead of an opaque read timeout.
    let frames = back.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let ps = roster(&frames).expect("presence");
    let names: Vec<&str> = ps.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        ps.len(),
        1,
        "the reconnect must displace its ghost, not seat a second \
             participant; roster is {names:?}"
    );
    // The survivor is the NEW connection, carrying its new size — proof
    // the ghost no longer pins the shared winsize at 80x24.
    assert_eq!(
        (ps[0].cols, ps[0].rows),
        (120, 40),
        "the reconnected client's size must win once the ghost is gone"
    );
    assert!(
        ps[0].control,
        "a reconnecting owner must not be silently demoted to read-only"
    );
    drop(first);
}

/// #229 guard rail: eviction keys on the client id, so two genuinely
/// separate clients — including two terminals sharing a machine, which
/// have the same user@host name — must both stay seated. Only a client
/// with a MATCHING id displaces, and an empty id displaces nothing.
#[test]
fn distinct_clients_never_displace_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut a = TestClient::connect_as(&socket, "vitali@mac", 80, 24, "relay-key-a");
    a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    // Same human, same machine, different session: must NOT evict.
    let mut b = TestClient::connect_as(&socket, "vitali@mac", 80, 24, "relay-key-b");
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
    });
    assert_eq!(roster(&frames).unwrap().len(), 2, "distinct ids coexist");

    // Two id-less clients (pre-0.1.701, or a local attach) also coexist:
    // an empty id must never match another empty id.
    let mut c = TestClient::connect(&socket, "legacy", 80, 24);
    c.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut d = TestClient::connect(&socket, "legacy", 80, 24);
    let frames = d.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 4)
    });
    assert_eq!(
        roster(&frames).unwrap().len(),
        4,
        "an empty client_id must never displace anything"
    );
}

/// #228 unit: the outbox admits frames until it passes the limit, then
/// latches dead and drops its backlog rather than growing without bound.
#[test]
fn an_outbox_over_its_limit_latches_dead_and_frees_its_backlog() {
    let outbox = Outbox::new();
    let chunk = vec![b'x'; 64 * 1024];
    let mut accepted = 0;
    while outbox.push(&chunk) {
        accepted += 1;
        assert!(accepted < 1000, "the outbox must not accept without bound");
    }
    assert!(
        accepted >= CLIENT_QUEUE_LIMIT / chunk.len(),
        "a healthy peer must be allowed to buffer up to the limit"
    );
    assert!(outbox.is_dead(), "passing the limit must latch dead");
    assert_eq!(
        outbox.queue.lock().unwrap().bytes,
        0,
        "a dead outbox must release its backlog"
    );
    assert!(!outbox.push(&chunk), "dead latches: it never revives");
    assert!(
        outbox.pop_blocking().is_none(),
        "a dead outbox must release its writer thread"
    );
}

/// #228 unit: a single frame larger than the whole limit still reaches a
/// healthy peer — one big repaint must not be mistaken for a wedge.
#[test]
fn an_outbox_delivers_a_frame_larger_than_its_limit() {
    let outbox = Outbox::new();
    let huge = vec![b'x'; CLIENT_QUEUE_LIMIT + 1];
    assert!(outbox.push(&huge), "the oversized frame is admitted");
    assert_eq!(
        outbox.pop_blocking().map(|f| f.len()),
        Some(huge.len()),
        "and is delivered intact"
    );
}

/// #53 part 1 compat, new server -> old client: the frame decoder must
/// skip a control variant it does not know (an even newer peer's, or
/// ServerHello reaching a pre-0.1.698 client) without poisoning the
/// stream for the frames that follow.
#[test]
fn an_unknown_control_variant_is_skipped_without_poisoning_the_stream() {
    let mut wire = encode_frame(FRAME_CONTROL, br#"{"t":"from_the_future","x":1}"#);
    wire.extend(encode_bytes_frame(b"still here"));
    let mut reader = FrameReader::new();
    let frames = reader.push(&wire);
    assert_eq!(
        frames,
        vec![Frame::Bytes(b"still here".to_vec())],
        "the unknown variant is dropped, the next frame survives"
    );
}

/// #53 part 1: the server answers every Hello with its own version,
/// before any PTY bytes reach the client.
#[test]
fn the_server_answers_hello_with_its_version() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    let mut c = TestClient::connect(&socket, "owner", 120, 40);
    let frames = c.read_until(|f| matches!(f, Frame::Control(Control::ServerHello { .. })));
    let version = frames.iter().find_map(|f| match f {
        Frame::Control(Control::ServerHello { version }) => Some(version.clone()),
        _ => None,
    });
    assert_eq!(version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
}

/// #53 part 1: the mismatch banner names both versions and the keys,
/// and the key mapping is continue/restart/detach with detach the
/// default for any other key.
#[test]
fn the_mismatch_banner_and_key_mapping_are_actionable() {
    let banner = mismatch_banner(Some("0.1.638"), "0.1.698");
    assert!(banner.contains("0.1.638") && banner.contains("0.1.698"));
    assert!(banner.contains("[C]") && banner.contains("[R]"));
    assert!(
        !banner.replace("\r\n", "").contains('\n'),
        "raw mode disables ONLCR: every newline must be CRLF or the \
             banner staircases"
    );
    let old = mismatch_banner(None, "0.1.698");
    assert!(
        old.contains("reports no version"),
        "a silent server is named as pre-version, not shown blank: {old}"
    );
    assert_eq!(mismatch_action(b'c'), MismatchAction::Continue);
    assert_eq!(mismatch_action(b'C'), MismatchAction::Continue);
    assert_eq!(mismatch_action(b'r'), MismatchAction::Restart);
    assert_eq!(mismatch_action(b'R'), MismatchAction::Restart);
    assert_eq!(mismatch_action(b'q'), MismatchAction::Detach);
    assert_eq!(mismatch_action(0x1b), MismatchAction::Detach);
}

/// #53 part 2: an attach from a 0x0 PTY (`script`, headless CI) is a
/// pure observer — and with no sized client connected, `apply_winsize`
/// used to return before the repaint jiggle, so the inner croft never
/// received a Resize and the observer stared at a blank screen. The
/// jiggle must fire at the current size for EVERY accepted attach.
#[test]
fn an_observer_only_attach_still_jiggles_the_inner_pty() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    // An inner command that reports every WINCH it receives: the only
    // observable proof the jiggle reached the PTY. The heartbeat dots
    // let the test stage on the trap being REGISTERED — the first
    // attach's jiggle races sh's startup (under suite load sh loses
    // and the WINCH is eaten by the default action), so the assertion
    // rides a SECOND attach, whose jiggle finds the trap live.
    let server = spawn_test_server_with(
        socket.clone(),
        vec![
            String::from("/bin/sh"),
            String::from("-c"),
            String::from("trap 'printf WINCHED' WINCH; while :; do printf .; sleep 0.05; done"),
        ],
    );
    wait_alive(&socket);
    let mut observer = TestClient::connect(&socket, "observer", 0, 0);
    observer.read_until(|f| matches!(f, Frame::Bytes(b) if b.contains(&b'.')));
    let second = TestClient::connect(&socket, "observer-two", 0, 0);
    observer.read_until(|f| matches!(f, Frame::Bytes(b) if b.windows(7).any(|w| w == b"WINCHED")));
    drop(second);
    drop(observer);
    drop(server);
}

/// #228: the invariant that matters is not merely that a wedged client
/// is eventually evicted, but that the survivors NEVER STALL while it
/// is wedged. With the queue-per-client design the pump only enqueues,
/// so the owner's echo must keep arriving promptly from the very first
/// round — long before the ghost's queue overflows and it is dropped.
///
/// The bounded-write version failed this: every broadcast paid up to
/// `WRITE_FRAME_DEADLINE` inline on the ghost while holding the clients
/// lock, so the owner's echo arrived in multi-second lurches.
#[test]
fn a_wedged_client_never_stalls_the_other_clients() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    let mut owner = TestClient::connect(&socket, "owner", 120, 40);
    owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    // The ghost attaches and never reads again.
    let ghost = TestClient::connect(&socket, "ghost", 80, 24);
    let frames = owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    assert_eq!(roster(&frames).unwrap().len(), 2, "staging: ghost seated");

    // First SATURATE the ghost's kernel socket buffer. Until it is full
    // a write to the ghost still completes immediately, so the inline
    // bounded-write design would look healthy here and this test would
    // pass against the very bug it exists to catch. Push enough through
    // `cat`'s echo to fill that buffer (a few hundred KB on Linux and
    // macOS) while the owner drains its own side.
    let filler = vec![b'x'; 32 * 1024];
    let mut sender = owner.stream.try_clone().expect("clone owner socket");
    let warm = std::thread::spawn(move || {
        let frame = encode_bytes_frame(&filler);
        for _ in 0..64 {
            if sender.write_all(&frame).is_err() {
                break;
            }
        }
    });
    // Drain roughly what was sent so the owner never wedges itself.
    // Reads are allowed to time out here: against a pump that IS blocked
    // on the ghost (the bug this test catches) the echo dries up
    // completely, and the timing assertions below are what should report
    // that — not a panic out of the warm-up.
    let mut drained = 0usize;
    let warm_deadline = Instant::now() + Duration::from_secs(20);
    while drained < 64 * 32 * 1024 && Instant::now() < warm_deadline {
        match owner.try_read_some() {
            Some(n) if n > 0 => drained += n,
            // A read that decoded no PTY bytes yet — a control frame, or
            // a frame still split across reads — is progress, not the
            // end. Breaking here would leave the ghost's buffer
            // unsaturated and quietly weaken the probe rounds below.
            Some(_) => continue,
            None => break,
        }
    }
    let _ = warm.join();

    // NOW the ghost is genuinely wedged: its buffer is full and it is
    // not reading. Every one of these round trips must still complete
    // quickly, because the pump only enqueues — the ghost's dead socket
    // never appears on the owner's critical path.
    for round in 0..8 {
        let probe = format!("probe-{round}\n");
        let started = Instant::now();
        owner.send(&encode_bytes_frame(probe.as_bytes()));
        // Wait for ANY output to come back. A healthy pump answers in
        // milliseconds; a pump blocked on the ghost answers only after
        // the ghost's write deadline expires, or not at all within the
        // socket's read timeout. Both show up as a large `elapsed`.
        while started.elapsed() < WRITE_FRAME_DEADLINE * 2 {
            match owner.try_read_some() {
                Some(n) if n > 0 => break,
                Some(_) => continue,
                None => break,
            }
        }
        let elapsed = started.elapsed();
        // Generous next to a blocked round (which pays the full
        // WRITE_FRAME_DEADLINE, 5s) but far below it, so the two cases
        // can never be confused by a slow CI machine.
        assert!(
            elapsed < Duration::from_millis(500),
            "round {round}: the owner waited {elapsed:?} behind a wedged peer; \
                 the pump must never block on a client socket"
        );
    }
    drop(ghost);
}

/// A client whose queue grows past [`CLIENT_QUEUE_LIMIT`] is undeliverable
/// and must be dropped, so a permanently wedged peer cannot pin the
/// shared winsize or grow the server's memory without bound.
#[test]
fn a_wedged_client_is_evicted_instead_of_freezing_the_pump() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    let mut owner = TestClient::connect(&socket, "owner", 120, 40);
    owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    // The ghost attaches and never reads again.
    let ghost = TestClient::connect(&socket, "ghost", 80, 24);
    let frames = owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    assert_eq!(roster(&frames).unwrap().len(), 2, "staging: ghost seated");
    // Flood: cat echoes everything back through broadcast; the ghost's
    // socket buffer fills, then its outbox grows past CLIENT_QUEUE_LIMIT
    // and the ghost is dropped.
    //
    // The writes happen on their own thread and are NOT interleaved with
    // the owner's reads. Alternating send/read would throttle the flood
    // to one chunk in flight — the ghost's backlog could never build,
    // because the sender would be rate-limited by its own draining. Here
    // the owner drains continuously (so it never wedges too and is never
    // itself evicted) while the flood runs ahead of the ghost.
    let chunk = vec![b'x'; 32 * 1024];
    // Several times the limit: the ghost's kernel socket buffer swallows
    // the first megabyte or so before any of it reaches the outbox.
    let rounds = (CLIENT_QUEUE_LIMIT / chunk.len()) * 4;
    // Only the owner holds write control, so only the owner's bytes
    // reach the PTY and come back as echo. Clone its socket so the flood
    // can run on its own thread while the reads continue below.
    let mut sender = owner.stream.try_clone().expect("clone owner socket");
    let flood = std::thread::spawn(move || {
        let frame = encode_bytes_frame(&chunk);
        for _ in 0..rounds {
            if sender.write_all(&frame).is_err() {
                break;
            }
        }
    });
    let mut evicted = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let frames = owner.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { .. })) || matches!(f, Frame::Bytes(_))
        });
        if frames
            .iter()
            .any(|f| matches!(f, Frame::Control(Control::Presence { .. })))
            && roster(&frames).is_some_and(|ps| !ps.iter().any(|p| p.name == "ghost"))
        {
            evicted = true;
            break;
        }
    }
    let _ = flood.join();
    assert!(
        evicted,
        "the wedged ghost must be evicted once its backlog passes the limit"
    );
    drop(ghost);
}

/// #356 end to end: `croft record`'s loop against a real host. What the
/// owner types, echoed by the PTY, lands in the cast under a valid
/// header sized to the owner's window, with the recorder's 0x0 ignored.
#[test]
fn croft_record_writes_the_sessions_output_to_a_cast() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    let mut owner = TestClient::connect(&socket, "owner", 80, 24);
    owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));

    let cast = dir.path().join("out.cast");
    let rec_socket = socket.clone();
    let rec_cast = cast.clone();
    let recorder = std::thread::spawn(move || {
        let mut file = std::fs::File::create(&rec_cast).unwrap();
        let until = std::time::Instant::now() + Duration::from_millis(1500);
        crate::session_record::record_to(
            &rec_socket,
            &mut file,
            "ws",
            Some(until),
            &std::sync::atomic::AtomicBool::new(false),
        )
    });
    // Let the recorder attach before typing, then give the echo time.
    std::thread::sleep(Duration::from_millis(300));
    owner.send(&encode_frame(0, b"hello\n"));
    recorder.join().unwrap().expect("record_to");

    // For checking playback by hand (`agg`, `asciinema play`).
    if let Some(keep) = std::env::var_os("CROFT_KEEP_CAST") {
        std::fs::copy(&cast, keep).unwrap();
    }
    let text = std::fs::read_to_string(&cast).unwrap();
    let mut lines = text.lines();
    let header: serde_json::Value = serde_json::from_str(lines.next().expect("a header")).unwrap();
    assert_eq!(header["version"], 2);
    assert_eq!(
        (header["width"].as_u64(), header["height"].as_u64()),
        (Some(80), Some(24))
    );
    let events: Vec<serde_json::Value> = lines
        .map(|l| serde_json::from_str(l).expect("each event is JSON"))
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e[1] == "o" && e[2].as_str().is_some_and(|d| d.contains("hello"))),
        "the typed text is in the cast: {text}"
    );
}

/// #356: a 0x0 client (a recorder) attaching first takes no control, so
/// the next real client still gets it.
#[test]
fn a_pure_observer_attaching_first_leaves_control_for_the_next_client() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut rec = TestClient::connect(&socket, "recorder", 0, 0);
    let frames = rec.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let ps = roster(&frames).unwrap();
    assert!(!ps[0].control, "a pure observer takes no control");

    let mut real = TestClient::connect(&socket, "owner", 80, 24);
    let frames = real.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
    });
    let ps = roster(&frames).unwrap();
    let owner = ps.iter().find(|p| p.name == "owner").unwrap();
    assert!(owner.control, "the first real client still gets control");
}

#[test]
fn control_moves_to_next_attacher_after_holder_detaches() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut a = TestClient::connect(&socket, "first", 80, 24);
    a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut b = TestClient::connect(&socket, "second", 80, 24);
    b.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));

    // Holder detaches; the remaining read-only guest stays read-only
    // (control is never transferred implicitly)...
    a.send(&encode_control_frame(&Control::Detach));
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 1)
    });
    let ps = roster(&frames).unwrap();
    assert!(!ps[0].control, "guest must not inherit control implicitly");

    // ...but the next fresh attach (e.g. the owner coming back) gains
    // control because nobody holds it.
    let mut c = TestClient::connect(&socket, "returning", 80, 24);
    let frames = c.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
    });
    let ps = roster(&frames).unwrap();
    let returning = ps.iter().find(|p| p.name == "returning").unwrap();
    assert!(
        returning.control,
        "fresh attach with no holder gains control"
    );
}

#[test]
fn grant_and_revoke_move_write_control_between_clients() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut a = TestClient::connect(&socket, "owner", 80, 24);
    a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut b = TestClient::connect(&socket, "guest", 80, 24);
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
    });
    let guest_id = roster(&frames)
        .unwrap()
        .iter()
        .find(|p| p.name == "guest")
        .unwrap()
        .id;

    // Guests cannot grant themselves control.
    b.send(&encode_control_frame(&Control::Grant { id: guest_id }));
    // Owner grants; everyone sees the guest holding control.
    a.send(&encode_control_frame(&Control::Grant { id: guest_id }));
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.name == "guest" && p.control))
    });
    // The self-grant must not have taken effect before the owner's grant:
    // every roster before the owner-granted one shows the guest without
    // control.
    let grants: Vec<bool> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Control(Control::Presence { participants }) => participants
                .iter()
                .find(|p| p.name == "guest")
                .map(|p| p.control),
            _ => None,
        })
        .collect();
    assert!(grants.last() == Some(&true));

    // Guest types; input now reaches cat and echoes.
    b.send(&encode_bytes_frame(b"granted\r"));
    let echoed = b.read_until(
        |f| matches!(f, Frame::Bytes(bytes) if bytes.windows(7).any(|w| w == b"granted")),
    );
    assert!(output_text(&echoed).contains("granted"));

    // Owner revokes; guest input is dropped again.
    a.send(&encode_control_frame(&Control::Revoke { id: guest_id }));
    a.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.name == "guest" && !p.control))
    });
}

/// A raw connection that authenticates as the inner croft's privileged
/// control channel (no Hello, so it never becomes a participant).
// A successor image adopting a predecessor's session (#238): the resume
// plumbing must yield a fully working host over inherited raw fds - PTY
// pump, roster, input arbitration, winsize via the raw-fd ioctl path,
// and waitpid on an inner child this image never spawned.
#[test]
fn a_resumed_host_serves_the_inherited_session() {
    use std::os::fd::IntoRawFd;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("r.mux.sock");
    // Stand in for the predecessor image: spawn the inner command on a
    // real PTY and bind the socket, then strip everything down to the
    // raw fds/pid that actually ride through an exec.
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let child = pair
        .slave
        .spawn_command(CommandBuilder::new("cat"))
        .unwrap();
    drop(pair.slave);
    let master_fd = pair.master.as_raw_fd().expect("master fd");
    let child_pid = child.process_id().expect("child pid");
    // The predecessor never drops its handles across the exec.
    std::mem::forget(pair.master);
    std::mem::forget(child);
    let listener_fd = crate::session::bind_socket_0600(&socket)
        .unwrap()
        .into_raw_fd();

    let resumed = resume_from(|name| {
        [
            (RESUME_LISTENER_ENV, listener_fd.to_string()),
            (RESUME_MASTER_ENV, master_fd.to_string()),
            (RESUME_CHILD_ENV, child_pid.to_string()),
            (RESUME_TOKEN_ENV, String::from(TEST_TOKEN)),
        ]
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.clone())
    })
    .expect("resume vars must parse")
    .expect("resume vars are present");

    let sock = socket.clone();
    let server = std::thread::spawn(move || run_resumed(&sock, resumed).expect("resumed host"));
    wait_alive(&socket);

    let mut a = TestClient::connect(&socket, "owner", 120, 40);
    let frames = a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let ps = roster(&frames).unwrap();
    assert!(
        ps[0].control,
        "the roster restarts empty after a swap, so the first re-Hello takes control"
    );
    // Input reaches the adopted PTY and its echo comes back.
    a.send(&encode_bytes_frame(b"PING\n"));
    a.read_until(|f| matches!(f, Frame::Bytes(b) if b.windows(4).any(|w| w == b"PING")));
    // Resize travels the raw-fd ioctl path (fresh hosts use portable-pty).
    a.send(&encode_control_frame(&Control::Resize {
        cols: 100,
        rows: 30,
    }));

    // The child predates this "image"; waitpid must still own its exit.
    unsafe { libc::kill(child_pid as libc::pid_t, libc::SIGTERM) };
    let frames = a.read_until(|f| matches!(f, Frame::Control(Control::Exit { .. })));
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, Frame::Control(Control::Exit { code: 143 }))),
        "SIGTERM on the inner child must surface as exit 143 to clients"
    );
    assert_eq!(server.join().unwrap(), 143);
}

// Resume vars: absent = fresh start; present-but-broken = hard error,
// because falling through to bind-and-spawn beside a predecessor's
// still-open fds would run two hosts on one session.
#[test]
fn resume_vars_absent_start_fresh_and_broken_vars_refuse() {
    assert!(resume_from(|_| None).unwrap().is_none());
    let only_listener =
        |v: &'static str| move |n: &str| (n == RESUME_LISTENER_ENV).then(|| String::from(v));
    assert!(
        resume_from(only_listener("notanfd")).is_err(),
        "an unparseable fd must refuse, not fresh-start"
    );
    assert!(
        resume_from(only_listener("999999")).is_err(),
        "an fd that did not survive the exec must refuse"
    );
}

/// Cmd+K as croft's keyboard flags (and iTerm2's mapping) send it.
const CMD_K: &[u8] = b"\x1b[107;9u";

#[test]
fn detach_chord_passes_ordinary_input_through_untouched() {
    let mut c = DetachChord::default();
    let input = b"ls -la\r\x1b[A\x0bQ\x1b[107;5u";
    assert_eq!(
        c.feed(input),
        (input.to_vec(), false),
        "typing, arrows, Ctrl+K (kill-line) and a lone Q are not the chord"
    );
    assert!(!c.holding());
}

#[test]
fn cmd_k_then_shift_q_detaches_and_forwards_nothing_of_the_chord() {
    let mut c = DetachChord::default();
    let mut input = b"echo".to_vec();
    input.extend_from_slice(CMD_K);
    input.extend_from_slice(b"Qtrailing");
    assert_eq!(c.feed(&input), (b"echo".to_vec(), true));
}

#[test]
fn the_chord_survives_release_events_split_reads_and_lock_keys() {
    // Leader release (REPORT_EVENT_TYPES) between the two keys, the
    // leader cut across two reads, and Caps Lock set (mods 9 + 64).
    let mut c = DetachChord::default();
    assert_eq!(c.feed(b"\x1b[107;7"), (Vec::new(), false));
    assert_eq!(c.feed(b"3u"), (Vec::new(), false));
    assert!(c.holding(), "the leader is withheld until the next key");
    assert_eq!(c.feed(b"\x1b[107;73:3u"), (Vec::new(), false));
    // Cmd still held on the second key: kitty Cmd+Shift+Q.
    assert_eq!(c.feed(b"\x1b[113;10u"), (Vec::new(), true));
}

#[test]
fn cmd_k_then_another_key_reaches_croft_unchanged() {
    let mut c = DetachChord::default();
    assert_eq!(c.feed(CMD_K), (Vec::new(), false));
    let mut expected = CMD_K.to_vec();
    expected.extend_from_slice(b"t");
    assert_eq!(
        c.feed(b"t"),
        (expected, false),
        "Cmd+K T (Color Theme) must arrive whole"
    );
    assert!(!c.holding());
}

#[test]
fn a_lone_escape_is_forwarded_at_once_not_held_for_the_next_key() {
    let mut c = DetachChord::default();
    assert_eq!(c.feed(b"\x1b"), (b"\x1b".to_vec(), false));
    assert!(!c.holding(), "Esc must never wait for another key");
    // A cut-off CSI that cannot become the chord goes through too.
    assert_eq!(c.feed(b"\x1b[1;5"), (b"\x1b[1;5".to_vec(), false));
    assert!(!c.holding());
    // One that can is held, and timed out by flush like a lone leader.
    assert_eq!(c.feed(b"\x1b[10"), (Vec::new(), false));
    assert!(c.holding());
    assert_eq!(c.flush(), b"\x1b[10".to_vec());
}

#[test]
fn a_lone_cmd_k_is_released_by_flush() {
    let mut c = DetachChord::default();
    assert_eq!(c.feed(CMD_K), (Vec::new(), false));
    assert_eq!(c.flush(), CMD_K.to_vec());
    assert!(!c.holding());
    assert_eq!(c.feed(b"q"), (b"q".to_vec(), false));
}

#[test]
fn the_teardown_undoes_every_mode_croft_turns_on() {
    let seq = String::from_utf8(client_teardown_seq()).unwrap();
    for (mode, what) in [
        ("\x1b[?1049l", "alternate screen"),
        ("\x1b[?1000l", "mouse clicks"),
        ("\x1b[?1003l", "mouse motion"),
        ("\x1b[?1006l", "SGR mouse"),
        ("\x1b[?2004l", "bracketed paste"),
        ("\x1b[?1004l", "focus reports"),
        ("\x1b[<1u", "kitty keyboard flags"),
        ("\x1b[?25h", "cursor visibility"),
    ] {
        assert!(seq.contains(mode), "teardown must reset {what}: {seq:?}");
    }
}

#[test]
fn the_end_note_says_whether_the_session_is_still_running() {
    assert!(attach_end_note(&PumpOutcome::Detached, true).starts_with("Detached"));
    assert!(
        attach_end_note(&PumpOutcome::Disconnected, true).contains("keeps running"),
        "a kicked client's session is still there to reattach"
    );
    assert_eq!(
        attach_end_note(&PumpOutcome::Disconnected, false),
        "The croft session ended."
    );
}

// #679: a host that closes the connection without an Exit frame (a kick,
// or the host dying) must read as a disconnect, which is what makes the
// client restore the terminal, not as the inner croft exiting cleanly.
#[test]
fn detach_targets_only_an_unambiguous_client() {
    let p = |id: u64, control: bool| Participant {
        id,
        name: String::from("x"),
        cols: 80,
        rows: 24,
        control,
        version: String::new(),
    };
    // The sole control holder, even beside read-only guests.
    assert_eq!(sole_detach_target(&[p(1, false), p(2, true)]), Some(2));
    // The sole client, whatever its control.
    assert_eq!(sole_detach_target(&[p(7, false)]), Some(7));
    // Two writers: the typist could be either, so no one.
    assert_eq!(sole_detach_target(&[p(1, true), p(2, true)]), None);
    assert_eq!(sole_detach_target(&[p(1, false), p(2, false)]), None);
    assert_eq!(sole_detach_target(&[]), None);
}

#[test]
fn a_connection_closed_without_exit_is_a_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("kick.mux.sock");
    let listener = crate::session::bind_socket_0600(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut reader = FrameReader::new();
        let mut buf = [0u8; 4096];
        'hello: loop {
            let n = c.read(&mut buf).unwrap_or(0);
            assert!(n > 0, "the client must Hello before anything else");
            for f in reader.push(&buf[..n]) {
                if matches!(f, Frame::Control(Control::Hello { .. })) {
                    break 'hello;
                }
            }
        }
        c.write_all(&encode_control_frame(&Control::ServerHello {
            version: String::from(env!("CARGO_PKG_VERSION")),
        }))
        .unwrap();
        c.write_all(&encode_bytes_frame(b"SCREEN")).unwrap();
        // The kick: the connection closes with no Exit frame.
        drop(c);
    });
    let mut stream = UnixStream::connect(&socket).unwrap();
    let outcome = attach_client_loop(&socket, &mut stream).expect("attach loop");
    assert!(
        matches!(outcome, PumpOutcome::Disconnected),
        "got {outcome:?}"
    );
    server.join().unwrap();
}

// The attach side of a host swap (#238): HostSwap followed by EOF must
// reconnect and re-Hello on the same socket, not exit; the session ends
// only on the successor's Exit frame.
#[test]
fn attach_reconnects_across_a_host_swap() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("swap.mux.sock");
    let listener = crate::session::bind_socket_0600(&socket).unwrap();
    let re_helloed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&re_helloed);
    let version = || String::from(env!("CARGO_PKG_VERSION"));
    let server = std::thread::spawn(move || {
        let read_hello = |stream: &mut UnixStream| -> bool {
            let mut reader = FrameReader::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return false;
                }
                for f in reader.push(&buf[..n]) {
                    if matches!(f, Frame::Control(Control::Hello { .. })) {
                        return true;
                    }
                }
            }
        };
        let (mut c1, _) = listener.accept().unwrap();
        assert!(read_hello(&mut c1), "first attach must Hello");
        c1.write_all(&encode_control_frame(&Control::ServerHello {
            version: version(),
        }))
        .unwrap();
        c1.write_all(&encode_bytes_frame(b"BEFORE")).unwrap();
        c1.write_all(&encode_control_frame(&Control::HostSwap))
            .unwrap();
        // The exec: this connection dies, the socket stays bound.
        drop(c1);
        let (mut c2, _) = listener.accept().unwrap();
        if read_hello(&mut c2) {
            seen.store(true, Ordering::SeqCst);
        }
        c2.write_all(&encode_control_frame(&Control::ServerHello {
            version: version(),
        }))
        .unwrap();
        c2.write_all(&encode_bytes_frame(b"AFTER")).unwrap();
        c2.write_all(&encode_control_frame(&Control::Exit { code: 7 }))
            .unwrap();
    });
    let mut stream = UnixStream::connect(&socket).unwrap();
    let outcome = attach_client_loop(&socket, &mut stream).expect("attach loop");
    assert!(
        matches!(outcome, PumpOutcome::Exit(7)),
        "the session must end on the SUCCESSOR's Exit, got {outcome:?}"
    );
    server.join().unwrap();
    assert!(
        re_helloed.load(Ordering::SeqCst),
        "the client must re-register after the swap"
    );
}

fn connect_inner(socket: &Path, token: &str) -> UnixStream {
    let mut stream = UnixStream::connect(socket).expect("connect inner");
    stream
        .set_read_timeout(Some(Duration::from_millis(600)))
        .unwrap();
    stream
        .write_all(&encode_control_frame(&Control::Inner {
            token: String::from(token),
        }))
        .expect("send inner hello");
    stream
}

#[test]
fn inner_channel_grants_revokes_and_kicks_without_joining_the_roster() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut a = TestClient::connect(&socket, "owner", 80, 24);
    a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut inner = connect_inner(&socket, TEST_TOKEN);
    let mut b = TestClient::connect(&socket, "guest", 80, 24);
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
    });
    // The privileged channel is not a participant: the roster stays at 2.
    let ps = roster(&frames).unwrap();
    assert_eq!(ps.len(), 2);
    let guest_id = ps.iter().find(|p| p.name == "guest").unwrap().id;

    // Inner channel grants the guest control.
    inner
        .write_all(&encode_control_frame(&Control::Grant { id: guest_id }))
        .unwrap();
    b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.name == "guest" && p.control))
    });
    // ...and revokes it.
    inner
        .write_all(&encode_control_frame(&Control::Revoke { id: guest_id }))
        .unwrap();
    b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.name == "guest" && !p.control))
    });

    // The privileged channel never receives PTY broadcast bytes.
    a.send(&encode_bytes_frame(b"noise\r"));
    a.read_until(|f| matches!(f, Frame::Bytes(bytes) if bytes.windows(5).any(|w| w == b"noise")));
    // Typing attribution is the only traffic a privileged channel may
    // see: never PTY broadcast bytes, never presence frames.
    let mut probe = [0u8; 4096];
    let mut fr = FrameReader::new();
    loop {
        match inner.read(&mut probe) {
            Ok(0) | Err(_) => break, // read timeout: channel drained
            Ok(n) => {
                for f in fr.push(&probe[..n]) {
                    assert!(
                        matches!(f, Frame::Control(Control::Typing { .. })),
                        "privileged channel must only see typing attribution, got {f:?}"
                    );
                }
            }
        }
    }

    // Kick disconnects the guest; the roster shrinks to the owner.
    inner
        .write_all(&encode_control_frame(&Control::Kick { id: guest_id }))
        .unwrap();
    let frames = a.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 1)
    });
    assert_eq!(roster(&frames).unwrap()[0].name, "owner");
}

#[test]
fn inner_channel_api_drives_grant_and_presence_sidecar_reflects_it() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut a = TestClient::connect(&socket, "owner", 80, 24);
    a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut b = TestClient::connect(&socket, "guest", 80, 24);
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
    });
    let guest_id = roster(&frames)
        .unwrap()
        .iter()
        .find(|p| p.name == "guest")
        .unwrap()
        .id;

    let mut channel = InnerChannel::connect(&socket, TEST_TOKEN).expect("channel");
    assert_eq!(channel.presence, presence_path(&socket));
    assert!(channel.set_control(guest_id, true));
    b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.name == "guest" && p.control))
    });
    let ps = read_presence(&channel.presence).expect("sidecar");
    assert!(ps.iter().any(|p| p.name == "guest" && p.control));
    assert!(channel.kick(guest_id));
    a.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 1)
    });
}

#[test]
fn typing_markers_reach_privileged_channel_on_writer_change() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut a = TestClient::connect(&socket, "owner", 80, 24);
    a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut b = TestClient::connect(&socket, "guest", 80, 24);
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
    });
    let ps = roster(&frames).unwrap();
    let owner_id = ps.iter().find(|p| p.name == "owner").unwrap().id;
    let guest_id = ps.iter().find(|p| p.name == "guest").unwrap().id;
    let mut channel = InnerChannel::connect(&socket, TEST_TOKEN).expect("channel");

    let drain_marker = |channel: &mut InnerChannel| -> Option<u64> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(id) = channel.drain_typing().first().copied() {
                return Some(id);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    // Owner types: one marker for the writer change.
    a.send(&encode_bytes_frame(b"aa\r"));
    assert_eq!(drain_marker(&mut channel), Some(owner_id));

    // Same writer keeps typing: no marker for a different writer (wait
    // for the echo so the server has definitely processed the input; a
    // duplicate owner marker from the registration catch-up is allowed).
    a.send(&encode_bytes_frame(b"bb\r"));
    a.read_until(|f| matches!(f, Frame::Bytes(bytes) if bytes.windows(2).any(|w| w == b"bb")));
    let leftover = channel.drain_typing();
    assert!(
        leftover.is_empty() || leftover == [owner_id],
        "{leftover:?}"
    );

    // Control moves to the guest and the guest types: a marker for the
    // new writer.
    assert!(channel.set_control(guest_id, true));
    b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.name == "guest" && p.control))
    });
    b.send(&encode_bytes_frame(b"cc\r"));
    assert_eq!(drain_marker(&mut channel), Some(guest_id));
}

#[test]
fn inner_channel_with_wrong_token_is_powerless() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);

    let mut a = TestClient::connect(&socket, "owner", 80, 24);
    a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut impostor = connect_inner(&socket, "wrong-token");
    let mut b = TestClient::connect(&socket, "guest", 80, 24);
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
    });
    let guest_id = roster(&frames)
        .unwrap()
        .iter()
        .find(|p| p.name == "guest")
        .unwrap()
        .id;

    // A wrong-token channel cannot grant; the guest stays read-only
    // through a subsequent legitimate roster update (owner's resize).
    impostor
        .write_all(&encode_control_frame(&Control::Grant { id: guest_id }))
        .unwrap();
    a.send(&encode_control_frame(&Control::Resize {
        cols: 81,
        rows: 24,
    }));
    let frames = b.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.name == "owner" && p.cols == 81))
    });
    let ps = roster(&frames).unwrap();
    assert!(!ps.iter().find(|p| p.name == "guest").unwrap().control);
}

// 2026-08-22 (#234): a roster resting with no control holder froze the
// session — read-only input is dropped at the host, the participants UI
// lives in the inner croft behind that same gate, and set_control
// normally requires a holder, so the state was a one-way door. The
// escape hatch (#235): a client alone on a vacant roster claims control,
// the same acquisition a detach/reattach already grants.
#[test]
fn a_sole_readonly_survivor_recovers_control_by_claiming() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    let mut owner = TestClient::connect(&socket, "owner", 120, 40);
    owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut guest = TestClient::connect(&socket, "guest", 100, 50);
    guest.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    owner.send(&encode_control_frame(&Control::Detach));
    let frames = guest.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 1)
    });
    let ps = roster(&frames).unwrap();
    assert!(
        sole_participant_lacks_control(&ps),
        "precondition: the survivor rests read-only (control never moves implicitly)"
    );
    guest.send(&encode_control_frame(&Control::Claim));
    let frames = guest.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.control))
    });
    let ps = roster(&frames).unwrap();
    assert_eq!(ps.len(), 1);
    assert!(
        ps[0].control,
        "a sole read-only survivor's claim must be granted"
    );
}

// Revoking the last holder still legally rests the roster vacant
// (control never moves implicitly); the claim is the recovery there too.
#[test]
fn a_self_revoked_holder_can_reclaim_a_vacant_roster() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    let mut owner = TestClient::connect(&socket, "owner", 120, 40);
    let frames = owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let owner_id = roster(&frames).unwrap()[0].id;
    owner.send(&encode_control_frame(&Control::Revoke { id: owner_id }));
    let frames = owner.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().all(|p| !p.control))
    });
    assert!(sole_participant_lacks_control(&roster(&frames).unwrap()));
    owner.send(&encode_control_frame(&Control::Claim));
    let frames = owner.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.control))
    });
    assert!(roster(&frames).unwrap()[0].control);
}

// The #235 escape hatch must not become a privilege escalation: a claim
// while someone holds control is refused. The resize after the claim is
// a sync barrier — it forces a Presence broadcast that reflects every
// frame the host processed before it.
#[test]
fn a_claim_while_control_is_held_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    let _server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    let mut owner = TestClient::connect(&socket, "owner", 120, 40);
    owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    let mut guest = TestClient::connect(&socket, "guest", 100, 50);
    guest.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    guest.send(&encode_control_frame(&Control::Claim));
    guest.send(&encode_control_frame(&Control::Resize {
        cols: 101,
        rows: 50,
    }));
    let frames = guest.read_until(|f| {
        matches!(f, Frame::Control(Control::Presence { participants })
            if participants.iter().any(|p| p.cols == 101))
    });
    let ps = roster(&frames).unwrap();
    assert!(
        !ps.iter().find(|p| p.name == "guest").unwrap().control,
        "a claim against a held roster must be refused"
    );
    assert!(ps.iter().find(|p| p.name == "owner").unwrap().control);
}

// The set_control permission rule, exhaustively. The vacant-claim arm's
// precondition is unreachable through the public protocol once
// restore_control_holder guards every mutation path, so it is pinned
// here at the function level.
#[test]
fn control_change_permission_rules() {
    let held = [(1u64, true), (2u64, false)];
    let vacant = [(1u64, false), (2u64, false)];
    // The privileged inner channel may do anything.
    assert!(control_change_allowed(
        true,
        None,
        2,
        true,
        held.iter().copied()
    ));
    // A holder may grant others.
    assert!(control_change_allowed(
        false,
        Some(1),
        2,
        true,
        held.iter().copied()
    ));
    // A read-only guest may not grant itself while control is held...
    assert!(!control_change_allowed(
        false,
        Some(2),
        2,
        true,
        held.iter().copied()
    ));
    // ...nor grant OTHERS from a vacant floor...
    assert!(!control_change_allowed(
        false,
        Some(2),
        1,
        true,
        vacant.iter().copied()
    ));
    // ...nor revoke anything from a vacant floor.
    assert!(!control_change_allowed(
        false,
        Some(2),
        1,
        false,
        vacant.iter().copied()
    ));
    // The #235 escape hatch: self-grant when nobody holds control.
    assert!(control_change_allowed(
        false,
        Some(2),
        2,
        true,
        vacant.iter().copied()
    ));
}

// The pump claims only when ALONE on a vacant roster: with others
// attached, control transfer stays explicit (a guest's pump must never
// outrace a returning owner), and an empty roster has nothing to
// recover.
#[test]
fn the_pump_claims_only_as_the_sole_readonly_participant() {
    let p = |control| Participant {
        id: 1,
        name: String::from("x"),
        cols: 80,
        rows: 24,
        control,
        version: String::new(),
    };
    assert!(sole_participant_lacks_control(&[p(false)]));
    assert!(!sole_participant_lacks_control(&[p(true)]));
    assert!(!sole_participant_lacks_control(&[p(false), p(false)]));
    assert!(!sole_participant_lacks_control(&[p(false), p(true)]));
    assert!(!sole_participant_lacks_control(&[]));
}

// #238: the marker sits next to the socket like the presence sidecar,
// and the update signal is the binary's inode moving — a plain touch of
// the same file must not read as an update.
#[test]
fn stale_marker_sits_next_to_socket_and_replacement_moves_the_identity() {
    let p = stale_marker_path(Path::new("/x/sessions/ab.mux.sock"));
    assert_eq!(p, Path::new("/x/sessions/ab.mux.sock.host-stale"));

    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("croft");
    std::fs::write(&bin, b"v1").unwrap();
    let start = image_identity(&bin).unwrap();
    // touch-equivalent: rewriting in place keeps the inode
    std::fs::write(&bin, b"v1-touched").unwrap();
    assert_eq!(
        image_identity(&bin).unwrap(),
        start,
        "an in-place rewrite is not a replacement"
    );
    // an install replaces: new file renamed over the old path
    let staged = dir.path().join("croft.new");
    std::fs::write(&staged, b"v2").unwrap();
    std::fs::rename(&staged, &bin).unwrap();
    assert_ne!(
        image_identity(&bin).unwrap(),
        start,
        "a rename-over is a replacement and must change the identity"
    );
}

/// A host wired to nothing: enough of one to drive [`client_thread`]
/// directly, which is the only way to observe the mid-swap window
/// without a real `exec` racing the assertions.
fn bare_host(socket: &Path) -> Arc<Host> {
    Arc::new(Host {
        clients: Mutex::new(Vec::new()),
        next_id: AtomicU64::new(0),
        pty_input: Mutex::new(Box::new(Vec::new())),
        master: Mutex::new(MasterHandle::Adopted(-1)),
        last_size: Mutex::new((80, 24)),
        socket: socket.to_path_buf(),
        token: String::from(TEST_TOKEN),
        privileged: Mutex::new(Vec::new()),
        last_writer: Mutex::new(None),
        swapping: AtomicBool::new(false),
        farewells: Mutex::new(Vec::new()),
    })
}

// #321: a client that arrives while the host is swapping into an updated
// image must be sent round to the successor, never seated. Seating it
// hands it a session that ends at the exec, and the EOF that follows is
// indistinguishable from "the session is over", which is how a
// background update kicked every attached client out of a live remote
// session instead of handing it over.
#[test]
fn a_client_arriving_mid_swap_is_told_to_reconnect_not_seated() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("swap.mux.sock");
    let listener = crate::session::bind_socket_0600(&socket).unwrap();
    let host = bare_host(&socket);
    host.swapping.store(true, Ordering::SeqCst);
    let served = Arc::clone(&host);
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        client_thread(&served, stream);
    });

    let mut c = TestClient::connect(&socket, "owner", 80, 24);
    let frames = c.read_until(|f| matches!(f, Frame::Control(Control::HostSwap)));
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, Frame::Control(Control::ServerHello { .. }))),
        "the goodbye must not cost the client its version handshake; saw {frames:?}"
    );
    assert!(
        host.clients.lock().unwrap().is_empty(),
        "a refused client must never join the roster"
    );
    // ...and its goodbye is enrolled in the swap's delivery fence. Without
    // this the exec can close the socket while HostSwap is still queued,
    // handing the refused client the bare EOF the refusal exists to spare
    // it - the roster snapshot the fence starts from cannot know about a
    // connection that arrived after the latch.
    let enrolled = host.farewells.lock().unwrap();
    assert_eq!(
        enrolled.len(),
        1,
        "the refusal must enrol its goodbye for the swap to wait on"
    );
    let (outbox, seq) = (Arc::clone(&enrolled[0].0), enrolled[0].1);
    drop(enrolled);
    // Bounded wait, not an instant assertion: the writer puts the frame on
    // the wire and marks it delivered as two steps, so a reader that has
    // already seen the frame can still observe the moment between them.
    // Asserting on that instant is the same defect as any other test that
    // assumes a spawned thread has finished - it passes until the machine
    // is busy (caught here under deliberate load).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !outbox.delivered_through(seq) {
        assert!(
            Instant::now() < deadline,
            "the client read the goodbye, so the fence must record it delivered"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    server.join().unwrap();
    // And the connection is actually let go: the writer thread delivers
    // the goodbye and exits rather than parking on a socket nobody owns.
    c.stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut buf = [0u8; 32];
    assert_eq!(
        c.stream.read(&mut buf).unwrap_or(0),
        0,
        "a refused connection must be closed, not left hanging"
    );
}

// Frames are numbered as they are accepted, and only the writer's
// successful write counts as delivery: the distinction is the whole basis
// of the swap barrier (#321).
#[test]
fn an_outbox_numbers_frames_and_counts_only_what_the_writer_wrote() {
    let o = Outbox::new();
    assert_eq!(o.push_seq(b"a"), Some(1));
    assert_eq!(o.push_seq(b"b"), Some(2));
    assert!(!o.delivered_through(1), "queued is not delivered");
    o.pop_blocking();
    o.mark_delivered();
    assert!(o.delivered_through(1));
    assert!(
        !o.delivered_through(2),
        "one write does not deliver two frames"
    );
    o.pop_blocking();
    o.mark_delivered();
    assert!(o.delivered_through(2));
}

// #321: the host must WAIT for its HostSwap invitation to land rather
// than sleep a hopeful beat past it. A writer parked on an earlier frame
// against a peer that stopped reading holds the invitation behind it for
// up to WRITE_FRAME_DEADLINE, and a client that never sees the invitation
// reads the exec's EOF as the session ending. The wait is still bounded:
// one wedged peer must not strand the update for everyone.
#[test]
fn the_swap_barrier_waits_for_delivery_and_still_bounds_a_parked_writer() {
    // A writer parked mid-frame is modelled by an outbox nothing drains:
    // the invitation is queued and never delivered.
    let parked = Outbox::new();
    let seq = parked.push_seq(b"invitation").expect("queued");
    let budget = Duration::from_millis(300);
    let started = Instant::now();
    await_delivery(&[(Arc::clone(&parked), seq)], Instant::now() + budget);
    assert!(
        started.elapsed() >= budget,
        "the barrier must actually wait for the invitation, not wave it through"
    );
    assert!(
        started.elapsed() < budget * 10,
        "but it must give up, not strand the update on one wedged peer"
    );

    // Delivery releases it at once, well inside the deadline.
    let live = Outbox::new();
    let seq = live.push_seq(b"invitation").expect("queued");
    let writer = {
        let o = Arc::clone(&live);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            o.pop_blocking();
            o.mark_delivered();
        })
    };
    let started = Instant::now();
    await_delivery(
        &[(Arc::clone(&live), seq)],
        Instant::now() + Duration::from_secs(5),
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a delivered invitation must release the barrier immediately"
    );
    writer.join().unwrap();

    // A peer that died mid-swap can never deliver anything, so it must
    // not hold the barrier for its full deadline either.
    let dead = Outbox::new();
    let seq = dead.push_seq(b"invitation").expect("queued");
    dead.kill();
    let started = Instant::now();
    await_delivery(&[(dead, seq)], Instant::now() + Duration::from_secs(5));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a dead peer must not hold the swap open"
    );
}

// The farewell fence must give a goodbye registered at the LAST moment a
// real window of its own. Sharing one deadline with the invitation fence
// meant a client refused a microsecond before that deadline was enrolled
// into a budget already spent, sealed past, and handed the bare EOF the
// refusal exists to prevent (#321).
#[test]
fn the_fence_waits_for_a_goodbye_registered_at_the_last_moment() {
    let dir = tempfile::tempdir().unwrap();
    let host = bare_host(&dir.path().join("late.mux.sock"));
    host.swapping.store(true, Ordering::SeqCst);

    // A goodbye whose writer has not run yet, exactly as a connection
    // refused on the deadline's edge leaves it.
    let outbox = Outbox::new();
    let seq = outbox.push_seq(b"goodbye").expect("queued");
    host.farewells
        .lock()
        .unwrap()
        .push((Arc::clone(&outbox), seq));
    let writer = {
        let o = Arc::clone(&outbox);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            o.pop_blocking();
            o.mark_delivered();
        })
    };

    // A hard cap far in the future: what must stop the seal here is the
    // undelivered goodbye, not the clock.
    let started = Instant::now();
    let sealed = seal_against_new_arrivals(&host, Instant::now() + Duration::from_secs(5));
    assert!(
        outbox.delivered_through(seq),
        "the fence must not seal while a goodbye is still queued"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "it must have actually waited for the writer, not raced past it"
    );
    drop(sealed);
    writer.join().unwrap();
}

// The wait is still bounded: a goodbye nothing ever delivers cannot hold
// the update open, or one wedged peer strands every other participant on
// the old image.
#[test]
fn the_fence_gives_up_at_its_hard_cap() {
    let dir = tempfile::tempdir().unwrap();
    let host = bare_host(&dir.path().join("capped.mux.sock"));
    host.swapping.store(true, Ordering::SeqCst);
    let outbox = Outbox::new();
    let seq = outbox.push_seq(b"goodbye").expect("queued");
    host.farewells
        .lock()
        .unwrap()
        .push((Arc::clone(&outbox), seq));

    let started = Instant::now();
    let _sealed = seal_against_new_arrivals(&host, Instant::now() + Duration::from_millis(200));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the cap must end the wait; took {:?}",
        started.elapsed()
    );
    assert!(
        !outbox.delivered_through(seq),
        "staging: this goodbye was never delivered, so the cap is what ended the wait"
    );
}

// A swap that cannot happen must reopen the door it closed. The host
// serves on, stale and marked so; a host that kept refusing every
// reconnect would be a worse failure than the staleness it reports.
#[test]
fn a_swap_that_cannot_exec_seats_clients_again() {
    use std::os::fd::AsRawFd;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("failed.mux.sock");
    let host = bare_host(&socket);
    let devnull = std::fs::File::open("/dev/null").unwrap();
    let handoff = HandoffFds {
        listener: devnull.as_raw_fd(),
        master: devnull.as_raw_fd(),
        child_pid: std::process::id(),
    };
    let err = swap_to_new_image(&host, Path::new("/nonexistent/croft-successor"), &handoff);
    assert!(
        format!("{err:#}").contains("exec of updated binary"),
        "the error must name the failed exec; got {err:#}"
    );
    assert!(
        !host.swapping.load(Ordering::SeqCst),
        "a host that could not swap must seat clients again"
    );
    // `Command::exec` resets SIGPIPE to its default for the new image,
    // and a failed exec left it there: the host (and the test binary in
    // CI) then died of its next write to a peer that had gone.
    assert_eq!(
        sigpipe_action().map(|a| a.sa_sigaction),
        Some(libc::SIG_IGN),
        "a host that could not swap must still ignore SIGPIPE"
    );
    // Negative: the host's own action comes back, not a fixed one. A
    // no-op handler stands in for it (as safe as ignoring, for the
    // other tests in this process).
    extern "C" fn noop(_: libc::c_int) {}
    let mut custom: libc::sigaction = unsafe { std::mem::zeroed() };
    custom.sa_sigaction = noop as *const () as libc::sighandler_t;
    // SAFETY: installs a handler that does nothing.
    unsafe { libc::sigaction(libc::SIGPIPE, &custom, std::ptr::null_mut()) };
    let _ = swap_to_new_image(&host, Path::new("/nonexistent/croft-successor"), &handoff);
    let after = sigpipe_action().map(|a| a.sa_sigaction);
    let mut ignore: libc::sigaction = unsafe { std::mem::zeroed() };
    ignore.sa_sigaction = libc::SIG_IGN;
    // SAFETY: puts back the Rust runtime's SIGPIPE action.
    unsafe { libc::sigaction(libc::SIGPIPE, &ignore, std::ptr::null_mut()) };
    assert_eq!(
        after,
        Some(noop as *const () as libc::sighandler_t),
        "the host's own SIGPIPE handler survives a failed swap"
    );
}

// The gate is the swap latch and nothing else: with no swap in flight the
// same path seats the client exactly as before.
#[test]
fn a_client_arriving_outside_a_swap_is_seated() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("noswap.mux.sock");
    let listener = crate::session::bind_socket_0600(&socket).unwrap();
    let host = bare_host(&socket);
    let served = Arc::clone(&host);
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        client_thread(&served, stream);
    });

    let mut c = TestClient::connect(&socket, "owner", 80, 24);
    c.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    assert_eq!(
        host.clients.lock().unwrap().len(),
        1,
        "an ordinary attach must still be seated"
    );
    drop(c);
    server.join().unwrap();
}

// The client half of the same handover: a reconnect that lands on the
// host still finishing its swap is answered with ServerHello + HostSwap,
// and must reconnect AGAIN rather than treating the goodbye (or the
// close behind it) as the end of the session.
#[test]
fn attach_retries_a_reconnect_the_dying_host_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("refuse.mux.sock");
    let listener = crate::session::bind_socket_0600(&socket).unwrap();
    let version = || String::from(env!("CARGO_PKG_VERSION"));
    let attaches = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&attaches);
    let server = std::thread::spawn(move || {
        let read_hello = |stream: &mut UnixStream| -> bool {
            let mut reader = FrameReader::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return false;
                }
                for f in reader.push(&buf[..n]) {
                    if matches!(f, Frame::Control(Control::Hello { .. })) {
                        return true;
                    }
                }
            }
        };
        // 1: the pre-swap host. 2: the same host, now latched, refusing
        // the reconnect it invited. 3: the successor, serving normally.
        for round in 1..=3 {
            let (mut c, _) = listener.accept().unwrap();
            assert!(read_hello(&mut c), "attach {round} must Hello");
            counted.fetch_add(1, Ordering::SeqCst);
            c.write_all(&encode_control_frame(&Control::ServerHello {
                version: version(),
            }))
            .unwrap();
            match round {
                1 | 2 => {
                    c.write_all(&encode_control_frame(&Control::HostSwap))
                        .unwrap();
                    // The exec: this connection dies, the socket stays
                    // bound and the client is expected to come back.
                    drop(c);
                }
                _ => {
                    c.write_all(&encode_bytes_frame(b"AFTER")).unwrap();
                    c.write_all(&encode_control_frame(&Control::Exit { code: 7 }))
                        .unwrap();
                }
            }
        }
    });

    let mut stream = UnixStream::connect(&socket).unwrap();
    let outcome = attach_client_loop(&socket, &mut stream).expect("attach loop");
    assert!(
        matches!(outcome, PumpOutcome::Exit(7)),
        "a refused reconnect must be retried, not read as session end; got {outcome:?}"
    );
    server.join().unwrap();
    assert_eq!(
        attaches.load(Ordering::SeqCst),
        3,
        "the client must attach three times: original, refused, successor"
    );
}

// A fresh host must clear a predecessor's stale marker: the new host IS
// the update the marker pointed at.
#[test]
fn a_fresh_host_clears_a_predecessors_stale_marker() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.mux.sock");
    std::fs::write(stale_marker_path(&socket), b"").unwrap();
    let server = spawn_test_server(socket.clone());
    wait_alive(&socket);
    wait_marker_cleared(&socket);
    assert!(
        !stale_marker_path(&socket).exists(),
        "the marker must not outlive the host it described"
    );
    let mut a = TestClient::connect(&socket, "owner", 80, 24);
    a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
    a.send(&encode_bytes_frame(&[0x04]));
    a.read_until(|f| matches!(f, Frame::Control(Control::Exit { .. })));
    let _ = server.join();
}

#[test]
fn an_older_client_in_the_roster_gets_one_notice_and_a_current_one_none() {
    let p = |version: &str| Participant {
        id: 1,
        name: String::from("x"),
        cols: 80,
        rows: 24,
        control: true,
        version: version.into(),
    };
    assert_eq!(stale_client_notice(&[p("0.1.959")], "0.1.959"), None);
    assert_eq!(
        stale_client_notice(&[p("")], "0.1.959"),
        None,
        "no version, no claim"
    );
    let n = stale_client_notice(&[p("0.1.900"), p("0.1.959"), p("0.1.900")], "0.1.959").unwrap();
    assert!(
        n.contains("croft 0.1.959") && n.contains("still runs 0.1.900."),
        "{n}"
    );
    assert_eq!(
        stale_client_notice(&[p("0.1.990")], "0.1.959"),
        None,
        "a newer client is not out of date"
    );
    // Numeric, not lexical: 0.1.99 < 0.1.959 and 0.1.1000 > 0.1.959.
    assert!(stale_client_notice(&[p("0.1.99")], "0.1.959").is_some());
    assert_eq!(stale_client_notice(&[p("0.1.1000")], "0.1.959"), None);
    // Listed oldest first, by number rather than by string.
    let n = stale_client_notice(&[p("0.1.900"), p("0.1.1000"), p("0.1.99")], "0.1.1001").unwrap();
    assert!(n.contains("still runs 0.1.99, 0.1.900, 0.1.1000."), "{n}");
    // A roster written by an older host carries no version field.
    let old: Vec<Participant> =
        serde_json::from_str(r#"[{"id":1,"name":"x","cols":80,"rows":24,"control":true}]"#)
            .unwrap();
    assert_eq!(old[0].version, "");
}

#[test]
fn the_stale_notice_is_raised_once_not_on_every_roster_churn() {
    let p = |id: u64, version: &str, cols: u16| Participant {
        id,
        name: String::from("x"),
        cols,
        rows: 24,
        control: true,
        version: version.into(),
    };
    let own = "0.1.959";
    let old = [p(1, "0.1.900", 80)];
    // First sight of an old client: raised.
    let first = stale_client_notice_on_change(None, &old, own).unwrap();
    // The same client resizing: the notice is unchanged, not re-raised.
    assert_eq!(
        stale_client_notice_on_change(Some(&first), &[p(1, "0.1.900", 120)], own),
        None
    );
    // A second, different old version changes the notice: raised again.
    let two = [p(1, "0.1.900", 80), p(2, "0.1.901", 80)];
    assert!(stale_client_notice_on_change(Some(&first), &two, own).is_some());
    // The old client reattached on the current version: nothing to say.
    assert_eq!(
        stale_client_notice_on_change(Some(&first), &[p(1, own, 80)], own),
        None
    );
    // A host swap raises the reference under an unchanged roster: the
    // notice changes, so it is raised even though no client moved.
    let swapped = [p(1, own, 80)];
    assert!(stale_client_notice_on_change(None, &swapped, "0.1.960").is_some());
}

#[test]
fn a_swapped_host_newer_than_the_inner_croft_still_flags_the_old_client() {
    // #652 review: the host swapped to 0.1.1005 while the inner croft and
    // the attach client both still run 0.1.1004 (no F9 yet). Measured
    // against the host, the client is out of date.
    let reference = session_version("0.1.1004", Some("0.1.1005"));
    assert_eq!(reference, "0.1.1005");
    let client = [Participant {
        id: 1,
        name: String::from("x"),
        cols: 80,
        rows: 24,
        control: true,
        version: String::from("0.1.1004"),
    }];
    let n = stale_client_notice(&client, reference).unwrap();
    assert!(
        n.contains("croft 0.1.1005") && n.contains("still runs 0.1.1004."),
        "{n}"
    );
    // No sidecar (an older host), or a host behind the inner croft: the
    // inner croft's own version is the reference.
    assert_eq!(session_version("0.1.1004", None), "0.1.1004");
    assert_eq!(session_version("0.1.1004", Some("0.1.1003")), "0.1.1004");
}

#[test]
fn the_host_version_sidecar_round_trips_and_rejects_junk() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ab.mux.sock");
    assert_eq!(
        host_version_path(&socket),
        dir.path().join("ab.mux.sock.host-version")
    );
    assert_eq!(read_host_version(&host_version_path(&socket)), None);
    write_host_version(&socket);
    assert_eq!(
        read_host_version(&host_version_path(&socket)).as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    std::fs::write(host_version_path(&socket), "not a version").unwrap();
    assert_eq!(read_host_version(&host_version_path(&socket)), None);
}
