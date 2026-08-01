//! Replicated text document for multiplayer Phase D (independent viewports).
//!
//! croft's shared-viewport multiplayer (src/session_host.rs) broadcasts one
//! PTY verbatim, so every participant sees the same screen. Phase D lets
//! participants edit the same buffer from independent viewports, which needs
//! one croft process per participant holding its own replica of the buffer.
//! With no central authority to serialize edits, concurrent inserts and
//! deletes must still converge to the same text on every replica: that is the
//! job of a CRDT.
//!
//! [`CollabDoc`] wraps a [`cola::Replica`] (which tracks only edit *positions*)
//! next to the canonical linear text (which croft owns, exactly as it owns the
//! editor's `Vec<String>`). A local edit returns an [`Op`] to broadcast; a
//! remote [`Op`] is integrated back into the text at the position cola resolves
//! against concurrent edits.
//!
//! This module is the whole collab layer below the app:
//! - [`CollabDoc`]: the replicated buffer, with [`text_delta_ops`] extracting
//!   ops from arbitrary text-state transitions (diff-based, so the editor
//!   needs no apply-edit chokepoint).
//! - [`byte_offset`]/[`position`]: bridge the editor's `(row, char-column)` to
//!   `CollabDoc`'s linear byte offsets, UTF-8 aware.
//! - [`Envelope`]/[`CollabMsg`] + [`relay_serve`]: the wire messages and the
//!   dumb fan-out relay over a dedicated collab socket.
//! - [`CollabChannel`] + [`CollabSession`]: a participant's connection and
//!   per-file state machine (bootstrap handshake, site allocation, backlog).
//!
//! The app side (`App::poll_collab`, src/app/mod.rs) feeds buffer text in
//! and applies the returned [`CollabEvent`]s to open tabs.

use std::io::{Read, Write};

use cola::{Deletion, Insertion, Replica};
use serde::{Deserialize, Serialize};

/// One edit to send to (or receive from) other replicas. cola's operations
/// carry position metadata but not the inserted characters, so an insert also
/// carries the literal text; a delete needs only the range metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
    Insert { insertion: Insertion, text: String },
    Delete { deletion: Deletion },
}

/// One text change `apply_remote` resolved against the local replica: delete
/// `deleted` bytes at `at`, then insert `inserted` at `at`. Spans are emitted
/// in application order, each relative to the text after every earlier span
/// in the same batch was applied, so a consumer replays them sequentially
/// (the editor wiring converts each to line coordinates via [`position`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSpan {
    pub at: usize,
    pub deleted: usize,
    pub inserted: String,
}

/// A replicated text buffer: the canonical text plus the cola replica that
/// resolves concurrent edits into a convergent order.
pub struct CollabDoc {
    replica: Replica,
    text: String,
    /// Literal text of insertions cola backlogged (its backlog keeps only
    /// position metadata), keyed by the run's identity so the drain can
    /// splice the right characters back in. An entry for a duplicate
    /// delivery is never drained; the transport delivers each op once, so
    /// that leak never materializes.
    pending_insert_texts: std::collections::HashMap<(u64, usize), String>,
}

/// Identity of an insertion's text run: who inserted it and where the run
/// starts on that author's own timeline. Stable between the op arriving
/// (unmergeable) and the backlog yielding it (ready).
fn insert_key(text: &cola::Text) -> (u64, usize) {
    (text.inserted_by(), text.temporal_range().start)
}

impl CollabDoc {
    /// A fresh document with `id` as this replica's identity and `initial` as
    /// its starting contents. Every replica of the same document must start
    /// from the same text (bootstrap it once, then exchange [`Op`]s).
    pub fn new(id: u64, initial: &str) -> Self {
        Self {
            replica: Replica::new(id, initial.len()),
            text: initial.to_string(),
            pending_insert_texts: std::collections::HashMap::new(),
        }
    }

    /// A second replica of this document for a new peer `id`, sharing the
    /// current contents and edit history so their future ops integrate.
    /// In-process only: production peers join over [`Self::from_snapshot`];
    /// the convergence tests bootstrap through this.
    #[allow(dead_code)]
    pub fn fork(&self, id: u64) -> Self {
        Self {
            replica: self.replica.fork(id),
            text: self.text.clone(),
            pending_insert_texts: self.pending_insert_texts.clone(),
        }
    }

    /// The replica's edit history in cola's compact binary format, sent in a
    /// [`CollabMsg::SnapshotReply`] next to the canonical text so a joining
    /// peer's future ops integrate against the same history.
    pub fn encode(&self) -> Vec<u8> {
        self.replica.encode().as_bytes().to_vec()
    }

    /// Rebuild a document from a snapshot (`text` + [`encode`](Self::encode)d
    /// history) under a fresh site `id` — the joining side of bootstrap.
    /// Fails on corrupt bytes or a cola protocol mismatch between peers.
    pub fn from_snapshot(id: u64, text: &str, encoded: &[u8]) -> anyhow::Result<Self> {
        let replica = Replica::decode(id, &cola::EncodedReplica::from_bytes(encoded))?;
        Ok(Self {
            replica,
            text: text.to_string(),
            pending_insert_texts: std::collections::HashMap::new(),
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// This replica's site id (unique per participant per document).
    pub fn site_id(&self) -> u64 {
        self.replica.id()
    }

    /// Apply a local insertion at byte offset `at` and return the op to
    /// broadcast to the other replicas.
    pub fn local_insert(&mut self, at: usize, s: &str) -> Op {
        self.text.insert_str(at, s);
        let insertion = self.replica.inserted(at, s.len());
        Op::Insert {
            insertion,
            text: s.to_string(),
        }
    }

    /// Apply a local deletion of byte range `at..at+len` and return the op.
    pub fn local_delete(&mut self, at: usize, len: usize) -> Op {
        let deletion = self.replica.deleted(at..at + len);
        self.text.replace_range(at..at + len, "");
        Op::Delete { deletion }
    }

    /// Integrate an op from another replica into this document's text at the
    /// position cola resolves against any concurrent local edits, returning
    /// the text changes it caused (see [`ResolvedSpan`] for the replay
    /// contract). An op whose causal context has not arrived yet integrates
    /// to nothing now — cola backlogs its position metadata, this document
    /// stashes its text — and the drain after any later op that supplies the
    /// context applies it, so out-of-order delivery never diverges replicas.
    pub fn apply_remote(&mut self, op: &Op) -> Vec<ResolvedSpan> {
        let mut spans = Vec::new();
        match op {
            Op::Insert { insertion, text } => {
                match self.replica.integrate_insertion(insertion) {
                    Some(offset) => {
                        self.text.insert_str(offset, text);
                        spans.push(ResolvedSpan {
                            at: offset,
                            deleted: 0,
                            inserted: text.clone(),
                        });
                    }
                    None => {
                        // Backlogged (or a duplicate; see the field docs).
                        self.pending_insert_texts
                            .insert(insert_key(insertion.text()), text.clone());
                    }
                }
            }
            Op::Delete { deletion } => {
                let ranges = self.replica.integrate_deletion(deletion);
                self.splice_deletion_ranges(ranges, &mut spans);
            }
        }
        self.drain_backlog(&mut spans);
        spans
    }

    /// Splice one integrated deletion's ranges out of the text. The ranges
    /// come back ascending and non-overlapping, all relative to the text
    /// before any of them is applied, so splicing back-to-front keeps every
    /// offset valid — and the emitted spans, in that same order, still honor
    /// the sequential replay contract (removing a later range never shifts
    /// an earlier one).
    fn splice_deletion_ranges(
        &mut self,
        mut ranges: Vec<std::ops::Range<usize>>,
        spans: &mut Vec<ResolvedSpan>,
    ) {
        ranges.sort_by_key(|r| std::cmp::Reverse(r.start));
        for range in ranges {
            spans.push(ResolvedSpan {
                at: range.start,
                deleted: range.len(),
                inserted: String::new(),
            });
            self.text.replace_range(range, "");
        }
    }

    /// Apply every backlogged op that became ready. cola's backlog iterators
    /// merge as they yield; each yielded offset is relative to the text with
    /// all earlier yields applied, which is exactly how the text is updated.
    /// Insertions loop to a fixed point because an insertion's anchor can sit
    /// inside another author's run, so one merge can unlock an author's queue
    /// the same iterator pass already skipped. Deletions then drain once:
    /// merging a deletion never unlocks anything (it advances no version map
    /// and removes no anchors).
    fn drain_backlog(&mut self, spans: &mut Vec<ResolvedSpan>) {
        loop {
            let ready: Vec<(cola::Text, usize)> = self.replica.backlogged_insertions().collect();
            if ready.is_empty() {
                break;
            }
            for (run, offset) in ready {
                let inserted = match self.pending_insert_texts.remove(&insert_key(&run)) {
                    Some(s) => s,
                    None => {
                        // Only reachable when a snapshot's encoded history
                        // carried a backlog (the stash does not travel with
                        // it). Splice in placeholder spaces so text offsets
                        // stay aligned with the replica instead of corrupting
                        // every later edit; the characters are wrong until
                        // the owner's next resync.
                        debug_assert!(false, "backlogged insertion with no stashed text");
                        " ".repeat(run.temporal_range().len())
                    }
                };
                self.text.insert_str(offset, &inserted);
                spans.push(ResolvedSpan {
                    at: offset,
                    deleted: 0,
                    inserted,
                });
            }
        }
        let ready: Vec<Vec<std::ops::Range<usize>>> = self.replica.backlogged_deletions().collect();
        for ranges in ready {
            self.splice_deletion_ranges(ranges, spans);
        }
    }
}

/// Diff `doc`'s current text against `new` and apply the difference as local
/// ops, returned for broadcast. This is the op-extraction path for the live
/// editor (slice 4): croft's editor has no single apply-edit chokepoint (~46
/// scattered mutation sites), so instead of threading positional deltas
/// through every one, each shared file diffs its last-synced text against the
/// current buffer whenever its edit seq advances. Multi-cursor edits, paste,
/// undo, and wholesale reloads all reduce to a text-state transition here.
pub fn text_delta_ops(doc: &mut CollabDoc, new: &str) -> Vec<Op> {
    use similar::{ChangeTag, TextDiff};
    let old = doc.text().to_string();
    // A timed-out diff yields a coarser but still valid edit script, bounding
    // the cost of reload-sized changes on the interactive tick.
    let diff = TextDiff::configure()
        .timeout(std::time::Duration::from_millis(200))
        .diff_chars(old.as_str(), new);
    let mut ops = Vec::new();
    let mut cursor = 0usize; // byte offset into doc's evolving text
    let mut del = 0usize; // bytes of a pending delete run
    let mut ins = String::new(); // text of a pending insert run
    let mut flush = |doc: &mut CollabDoc, cursor: &mut usize, del: &mut usize, ins: &mut String| {
        if *del > 0 {
            ops.push(doc.local_delete(*cursor, *del));
            *del = 0;
        }
        if !ins.is_empty() {
            ops.push(doc.local_insert(*cursor, ins));
            *cursor += ins.len();
            ins.clear();
        }
    };
    for change in diff.iter_all_changes() {
        let bytes = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                flush(doc, &mut cursor, &mut del, &mut ins);
                cursor += bytes;
            }
            ChangeTag::Delete => del += bytes,
            ChangeTag::Insert => ins.push_str(change.value()),
        }
    }
    flush(doc, &mut cursor, &mut del, &mut ins);
    debug_assert_eq!(doc.text(), new);
    ops
}

/// Byte offset of the char-indexed position `(row, col)` within the text
/// formed by joining `lines` with `'\n'` — the linear coordinate CollabDoc and
/// cola operate in. croft's editor addresses the buffer as `(row, char-column)`
/// (`cursor_col` is a char index throughout src/widgets/editor.rs); cola
/// addresses it as one byte offset. The editor wiring only converts the other
/// way ([`position`]; extraction is diff-based); this direction anchors the
/// `croft pair` edit fences (src/pair.rs, `range_bytes`), whose coordinates
/// arrive as char positions.
pub fn byte_offset(lines: &[String], row: usize, col: usize) -> usize {
    let mut offset = 0;
    for line in lines.iter().take(row) {
        offset += line.len() + 1; // +1 for the '\n' separator
    }
    if let Some(line) = lines.get(row) {
        offset += line
            .char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
    }
    offset
}

/// Inverse of [`byte_offset`]: the `(row, char-column)` of a byte `offset`.
/// `offset` is assumed char-aligned (every offset cola or [`byte_offset`]
/// produces is); a mid-char offset would clamp at the enclosing char boundary
/// via `chars().count()`. Past-the-end clamps to the end of the last line.
pub fn position(lines: &[String], offset: usize) -> (usize, usize) {
    let mut remaining = offset;
    for (row, line) in lines.iter().enumerate() {
        let line_bytes = line.len();
        if remaining <= line_bytes {
            let end = line
                .char_indices()
                .map(|(b, _)| b)
                .chain(std::iter::once(line_bytes))
                .take_while(|&b| b <= remaining)
                .count()
                .saturating_sub(1);
            return (row, end);
        }
        remaining -= line_bytes + 1; // consume the line and its '\n'
    }
    let row = lines.len().saturating_sub(1);
    let col = lines.get(row).map(|l| l.chars().count()).unwrap_or(0);
    (row, col)
}

/// A per-file, per-site edit on the wire: which file (a workspace-relative
/// key), which participant produced it, and the op. Peers route it to their
/// `CollabDoc` for that file and integrate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub file: String,
    pub site: u64,
    pub op: Op,
}

impl Envelope {
    /// Serialize into one framed message for the collab socket, reusing the
    /// session-host wire framing (`[type][len][payload]`). Production sends
    /// [`CollabMsg::Op`] (via [`CollabMsg::encode`]); the transport tests
    /// drive the relay with bare envelopes.
    #[allow(dead_code)]
    pub fn encode(&self) -> Vec<u8> {
        let json = serde_json::to_vec(self).unwrap_or_default();
        crate::session_host::encode_bytes_frame(&json)
    }
}

/// Everything the collab socket carries: edit ops plus the bootstrap
/// handshake. The relay stays a dumb fan-out (every message is a
/// `Frame::Bytes` broadcast to the other participants); peers filter by
/// `file` and, for snapshots, by `nonce` — only the save owner answers a
/// [`SnapshotRequest`], so a request never draws competing replies.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CollabMsg {
    Op(Envelope),
    /// A joining peer (or one opening a file already shared) asks for the
    /// document's current state; `nonce` pairs the reply to this request.
    SnapshotRequest {
        file: String,
        nonce: u64,
    },
    /// The owner's atomic answer: the canonical text, the encoded replica
    /// history it corresponds to, and the site id the owner allocated for
    /// the joiner (site ids must be unique per participant; the owner is
    /// the single allocator, so ids never collide).
    SnapshotReply {
        file: String,
        nonce: u64,
        assigned_site: u64,
        text: String,
        replica: Vec<u8>,
    },
    /// A participant's caret moved: peers with the file open paint it as a
    /// ghost caret in that participant's color, with a name tag while it
    /// moves. `name` defaults empty so a 0.1.632 peer's carets still parse
    /// (site ids are per-file, so identity has to ride the wire).
    Caret {
        file: String,
        site: u64,
        row: usize,
        col: usize,
        #[serde(default)]
        name: String,
    },
    /// An AI pilot's token stream into `file` started (`active`) or ended
    /// (finished, cancelled, or reverted). Peers surface the state (badge,
    /// stop button) while active. `site` is the pilot's site id in `file`
    /// when the doc is live, 0 before bootstrap — identity is `name`, same
    /// as carets. 0.1.633 peers drop this variant in `drain`.
    StreamState {
        site: u64,
        name: String,
        file: String,
        active: bool,
    },
    /// Any participant asks the streaming pilot to stop and revert. Bare by
    /// design: at most one stream runs per relay (one pilot seat), so there
    /// is nothing to address.
    StreamCancel {},
}

impl CollabMsg {
    /// Serialize into one framed message for the collab socket, same framing
    /// as [`Envelope::encode`].
    pub fn encode(&self) -> Vec<u8> {
        let json = serde_json::to_vec(self).unwrap_or_default();
        crate::session_host::encode_bytes_frame(&json)
    }
}

/// Fan each participant's [`Envelope`] frames out to the *other* participants.
/// A dumb multiplexer, exactly like the PTY broadcast: `cola` makes ordering
/// the client's job, so the relay never inspects an op. Runs until the socket
/// closes; one thread per client. Distinct socket from the PTY mux
/// (`<hash>.collab.sock`), so it never carries terminal bytes.
pub fn relay_serve(socket: &std::path::Path) -> anyhow::Result<()> {
    // Attach-or-create: a live relay already owns the socket; binding over
    // it would strand that relay's connected clients mid-session. The
    // liveness check here is only a fast path - creation itself (stale-file
    // removal included) is serialized inside the binder, so two creators
    // racing past this line cannot both publish: the loser is told the
    // winner is alive and attaches, which is exactly these semantics.
    if crate::session::is_alive(socket) {
        return Ok(());
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = match crate::session::bind_socket_0600(socket) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let clients: std::sync::Arc<std::sync::Mutex<Vec<Peer>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    for stream in listener.incoming().flatten() {
        let tx = std::sync::Arc::new(std::sync::Mutex::new(stream.try_clone()?));
        clients.lock().unwrap().push(std::sync::Arc::clone(&tx));
        let clients = std::sync::Arc::clone(&clients);
        std::thread::spawn(move || relay_client(stream, &tx, &clients));
    }
    Ok(())
}

/// The argv (after the croft binary itself) that serves a relay on `socket`.
/// Pure so it can be unit-tested.
fn detached_relay_argv(socket: &std::path::Path) -> Vec<String> {
    vec![
        String::from("collab-relay"),
        String::from("--socket"),
        socket.to_string_lossy().into_owned(),
    ]
}

/// Ensure a relay is serving `socket`: done if one is alive, otherwise spawn
/// one detached (same null-stdio + setsid pattern as
/// `session_host::spawn_detached_server`, so it outlives this participant's
/// terminal) and wait for it to bind.
pub fn ensure_relay(socket: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    if crate::session::is_alive(socket) {
        return Ok(());
    }
    let exe = std::env::current_exe().context("resolving croft binary path")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(detached_relay_argv(socket));
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("spawning collab relay")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !crate::session::is_alive(socket) {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("collab relay did not start on {}", socket.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Ok(())
}

/// Which side of the collab session this process is (see docs/MULTIPLAYER.md):
/// the owner is the mux's inner croft — it answers bootstrap requests,
/// allocates site ids, and is the only participant that writes shared files
/// to disk; guests are the solo-viewport processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollabRole {
    Owner,
    Guest,
}

/// This process's handle on the collab relay: a non-blocking framed stream
/// of [`CollabMsg`]s, mirroring `session_host::InnerChannel` (the launch
/// side puts the socket and role in the environment; the app connects).
pub struct CollabChannel {
    stream: std::os::unix::net::UnixStream,
    reader: crate::session_host::FrameReader,
    pub role: CollabRole,
}

impl CollabChannel {
    /// The socket and role the launch tail exported, if this croft was
    /// started as a collab participant. Separate from [`from_env`] so the
    /// app can keep retrying the connect while the relay comes up.
    ///
    /// [`from_env`]: Self::from_env
    pub fn env_config() -> Option<(std::path::PathBuf, CollabRole)> {
        let socket = std::path::PathBuf::from(std::env::var_os("CROFT_COLLAB_SOCKET")?);
        let role = match std::env::var("CROFT_COLLAB_ROLE").ok()?.as_str() {
            "owner" => CollabRole::Owner,
            "guest" => CollabRole::Guest,
            _ => return None,
        };
        Some((socket, role))
    }

    pub fn connect(socket: &std::path::Path, role: CollabRole) -> Option<Self> {
        let stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
        // Non-blocking so the app's per-tick drain never stalls the render
        // loop, same as InnerChannel.
        stream.set_nonblocking(true).ok()?;
        Some(Self {
            stream,
            reader: crate::session_host::FrameReader::new(),
            role,
        })
    }

    /// Broadcast one message to the other participants. Returns false when
    /// the relay is gone (the caller drops the channel and reconnects).
    pub fn send(&mut self, msg: &CollabMsg) -> bool {
        crate::session_host::write_frame_blocking(&mut self.stream, &msg.encode())
    }

    /// Drain every message waiting on the channel, in arrival order.
    /// Non-blocking; malformed frames are skipped (a newer peer may speak a
    /// larger CollabMsg — ignoring what this side cannot parse beats dying).
    pub fn drain(&mut self) -> Vec<CollabMsg> {
        let mut msgs = Vec::new();
        let mut buf = [0u8; 16384];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for frame in self.reader.push(&buf[..n]) {
                        if let crate::session_host::Frame::Bytes(payload) = frame
                            && let Ok(msg) = serde_json::from_slice::<CollabMsg>(&payload)
                        {
                            msgs.push(msg);
                        }
                    }
                }
            }
        }
        msgs
    }
}

/// Resolve a wire-supplied file key against the workspace root. Keys arrive
/// from peers (snapshot requests, ops, carets) and the MCP collab agent
/// forwards caller input verbatim, so containment is enforced here, not by
/// the sender: every component must be a plain name — a traversing or
/// absolute key must never let a guest read or edit outside the workspace.
pub fn contained_path(root: &std::path::Path, file: &str) -> Option<std::path::PathBuf> {
    let rel = std::path::Path::new(file);
    let plain = !file.is_empty()
        && rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    plain.then(|| root.join(rel))
}

/// The owner's fixed site id; joiners are allocated ids from 2 up.
const OWNER_SITE: u64 = 1;

/// How long a guest waits for the owner's snapshot before giving up. A
/// workspace with no live owner (nobody answers bootstrap) degrades to
/// plain local editing instead of staying read-only forever.
const BOOTSTRAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// How long the first unanswered SnapshotRequest waits before a re-send.
/// The relay has no replay: a request broadcast before the owner's
/// connection is registered is otherwise lost forever. Every further
/// unanswered resend doubles the wait, capped at [`SNAPSHOT_RESEND_CAP`]:
/// a guest cannot tell a lost request from a large reply still arriving,
/// and a fixed cadence made the owner re-serialize and blocking-write the
/// whole document every 500ms while a slow snapshot was already in flight.
const SNAPSHOT_RESEND: std::time::Duration = std::time::Duration::from_millis(500);

/// Ceiling on the resend backoff. Unbounded doubling scheduled the third
/// resend past [`BOOTSTRAP_TIMEOUT`] (0.5s, 1.5s, then 3.5s), so an owner
/// whose relay connection registered in the back half of the bootstrap
/// window was never re-asked and the guest silently degraded the file to
/// local-only. Capped at 1s the schedule is 0.5s, 1.5s, 2.5s: the same
/// late-owner coverage the pre-backoff fixed cadence had, at half the
/// duplicate-snapshot spam.
const SNAPSHOT_RESEND_CAP: std::time::Duration = std::time::Duration::from_secs(1);

/// Per-file replication state on one participant.
enum DocState {
    /// Guest waiting for the owner's [`CollabMsg::SnapshotReply`]. Ops for
    /// the file arriving meanwhile are buffered and replayed after the
    /// snapshot lands (duplicates with ops already folded into the snapshot
    /// integrate as no-ops).
    Bootstrapping {
        nonce: u64,
        deadline: std::time::Instant,
        /// When to re-send the request if no reply has landed yet.
        resend_at: std::time::Instant,
        /// The wait after the next resend fires (doubles each time, capped:
        /// see [`SNAPSHOT_RESEND`] and [`SNAPSHOT_RESEND_CAP`]).
        resend_every: std::time::Duration,
        buffered: Vec<Envelope>,
    },
    // Boxed: CollabDoc is an order of magnitude larger than the bootstrap
    // variant, and per-file states live in a map for the session's life.
    Live(Box<CollabDoc>),
}

/// What [`CollabSession::poll`] resolved this tick, for the app to apply to
/// its buffers. Everything is data: the session never touches an editor.
#[derive(Debug)]
pub enum CollabEvent {
    /// Remote edits resolved against the local replica; replay the spans
    /// sequentially onto the open buffer for `file`.
    RemoteEdit {
        file: String,
        spans: Vec<ResolvedSpan>,
    },
    /// Bootstrap finished: swap the open buffer for `file` to `text` (the
    /// owner's canonical text with any buffered ops already folded in) and
    /// lift the input gate.
    Bootstrapped { file: String, text: String },
    /// No owner answered within the deadline: lift the input gate and treat
    /// the file as local-only.
    BootstrapTimedOut { file: String },
    /// A peer's caret moved in `file`.
    Caret {
        file: String,
        site: u64,
        row: usize,
        col: usize,
        name: String,
    },
    /// An AI pilot's stream into `file` started or ended.
    StreamState {
        file: String,
        site: u64,
        name: String,
        active: bool,
    },
    /// A participant asked the streaming pilot to stop and revert.
    StreamCancel,
}

/// One participant's collab state machine: per-file replicated documents
/// over a [`CollabChannel`], with the bootstrap handshake and site-id
/// allocation (docs/MULTIPLAYER.md, Phase D). Editor-agnostic — the app
/// feeds it buffer text and applies the events it returns — so the whole
/// engine is testable headlessly through a relay socket pair.
pub struct CollabSession {
    pub role: CollabRole,
    channel: CollabChannel,
    docs: std::collections::HashMap<String, DocState>,
    /// Owner: next site id to hand a joiner (the owner itself is site 1).
    /// Only the owner allocates, so ids never collide (a collision would
    /// permanently break convergence).
    next_site: u64,
    /// Guest: pairs a SnapshotReply to this session's own request. Seeded
    /// from the process id so two guests' nonces never collide (replies are
    /// broadcast, and adopting another guest's reply would clone its site).
    next_nonce: u64,
    /// This participant's display name; rides every caret broadcast so
    /// peers can tag the ghost caret (site ids are per-file and carry no
    /// identity across files).
    name: String,
}

impl CollabSession {
    pub fn new(channel: CollabChannel, name: String) -> Self {
        Self {
            role: channel.role,
            channel,
            docs: std::collections::HashMap::new(),
            next_site: OWNER_SITE + 1,
            next_nonce: (std::process::id() as u64) << 32,
            name,
        }
    }

    /// True when `file` is replicating (bootstrap finished).
    pub fn is_live(&self, file: &str) -> bool {
        matches!(self.docs.get(file), Some(DocState::Live(_)))
    }

    /// True while `file` waits for its snapshot (the app gates edit input).
    pub fn is_bootstrapping(&self, file: &str) -> bool {
        matches!(self.docs.get(file), Some(DocState::Bootstrapping { .. }))
    }

    /// The replicated text for `file`, when live. The collab agent serves
    /// reads through this (it has no editor buffers); the app reads its own.
    pub fn doc_text(&self, file: &str) -> Option<&str> {
        match self.docs.get(file) {
            Some(DocState::Live(doc)) => Some(doc.text()),
            _ => None,
        }
    }

    /// Every file this session tracks, with whether it is live yet (false =
    /// still bootstrapping). For status surfaces like `collab_status`.
    pub fn tracked_files(&self) -> Vec<(String, bool)> {
        self.docs
            .iter()
            .map(|(f, s)| (f.clone(), matches!(s, DocState::Live(_))))
            .collect()
    }

    /// Guest: start sharing `file` — ask the owner for its current state and
    /// buffer inbound ops until the snapshot lands. No-op if already tracked.
    pub fn request_file(&mut self, file: &str) {
        if self.role != CollabRole::Guest || self.docs.contains_key(file) {
            return;
        }
        let nonce = self.next_nonce;
        self.next_nonce += 1;
        self.channel.send(&CollabMsg::SnapshotRequest {
            file: file.to_string(),
            nonce,
        });
        let now = std::time::Instant::now();
        self.docs.insert(
            file.to_string(),
            DocState::Bootstrapping {
                nonce,
                deadline: now + BOOTSTRAP_TIMEOUT,
                resend_at: now + SNAPSHOT_RESEND,
                resend_every: SNAPSHOT_RESEND,
                buffered: Vec::new(),
            },
        );
    }

    /// The local buffer for `file` now reads `current`: diff it against the
    /// replica (invariant 1a, extract-before-apply) and broadcast the
    /// resulting ops. Returns whether anything was sent. Quiescent when the
    /// text is unchanged — an op emitted here for a remote edit the app just
    /// applied would echo between peers forever.
    pub fn local_change(&mut self, file: &str, current: &str) -> bool {
        let Some(DocState::Live(doc)) = self.docs.get_mut(file) else {
            return false;
        };
        if doc.text() == current {
            return false;
        }
        let site = doc.site_id();
        let ops = text_delta_ops(doc, current);
        let sent = !ops.is_empty();
        for op in ops {
            self.channel.send(&CollabMsg::Op(Envelope {
                file: file.to_string(),
                site,
                op,
            }));
        }
        sent
    }

    /// This participant's site id in every live file: its wire identity.
    /// The owner is always site 1; every guest file-join is allocated a
    /// fresh id from the owner's one counter, so a guest's ids are unique
    /// across the whole relay — unlike display names, which are neither
    /// unique nor length-stable.
    pub fn my_site_ids(&self) -> Vec<u64> {
        self.docs
            .values()
            .filter_map(|d| match d {
                DocState::Live(doc) => Some(doc.site_id()),
                _ => None,
            })
            .collect()
    }

    /// Broadcast this participant's caret position in a live file.
    pub fn send_caret(&mut self, file: &str, row: usize, col: usize) {
        let Some(DocState::Live(doc)) = self.docs.get(file) else {
            return;
        };
        let site = doc.site_id();
        self.channel.send(&CollabMsg::Caret {
            file: file.to_string(),
            site,
            row,
            col,
            name: self.name.clone(),
        });
    }

    /// Broadcast that this participant's AI stream into `file` started or
    /// ended. Not doc-gated (unlike carets): the pilot announces before the
    /// file may have bootstrapped; `site` is best-effort identity, `name` is
    /// the real one.
    pub fn send_stream_state(&mut self, file: &str, active: bool) {
        let site = match self.docs.get(file) {
            Some(DocState::Live(doc)) => doc.site_id(),
            _ => 0,
        };
        self.channel.send(&CollabMsg::StreamState {
            site,
            name: self.name.clone(),
            file: file.to_string(),
            active,
        });
    }

    /// Ask whoever is streaming on this relay to stop and revert.
    // Callers land with the cancel affordances (follow-up slice).
    #[allow(dead_code)]
    pub fn send_stream_cancel(&mut self) {
        self.channel.send(&CollabMsg::StreamCancel {});
    }

    /// Drain the channel and resolve everything into [`CollabEvent`]s
    /// (invariant 1b: the app extracts local edits before calling this).
    /// `owner_text` is the owner's buffer lookup for a file a guest wants
    /// bootstrapped: return its current text (opening it if needed) or None
    /// to decline (unreadable, binary). Guests ignore the callback.
    pub fn poll(&mut self, mut owner_text: impl FnMut(&str) -> Option<String>) -> Vec<CollabEvent> {
        let mut events = Vec::new();
        for msg in self.channel.drain() {
            match msg {
                CollabMsg::Op(env) => match self.docs.get_mut(&env.file) {
                    Some(DocState::Live(doc)) => {
                        let spans = doc.apply_remote(&env.op);
                        if !spans.is_empty() {
                            events.push(CollabEvent::RemoteEdit {
                                file: env.file,
                                spans,
                            });
                        }
                    }
                    Some(DocState::Bootstrapping { buffered, .. }) => buffered.push(env),
                    // Not open on this peer: ignored until opened, then the
                    // bootstrap snapshot carries this edit anyway.
                    None => {}
                },
                CollabMsg::SnapshotRequest { file, nonce } => {
                    // Only the owner answers, so a request never draws
                    // competing replies (and site allocation stays single-
                    // writer).
                    if self.role != CollabRole::Owner {
                        continue;
                    }
                    if !self.docs.contains_key(&file) {
                        let Some(text) = owner_text(&file) else {
                            continue;
                        };
                        self.docs.insert(
                            file.clone(),
                            DocState::Live(Box::new(CollabDoc::new(OWNER_SITE, &text))),
                        );
                    }
                    let Some(DocState::Live(doc)) = self.docs.get(&file) else {
                        continue;
                    };
                    let assigned_site = self.next_site;
                    self.next_site += 1;
                    self.channel.send(&CollabMsg::SnapshotReply {
                        file,
                        nonce,
                        assigned_site,
                        text: doc.text().to_string(),
                        replica: doc.encode(),
                    });
                }
                CollabMsg::SnapshotReply {
                    file,
                    nonce,
                    assigned_site,
                    text,
                    replica,
                } => {
                    let Some(DocState::Bootstrapping {
                        nonce: want,
                        buffered,
                        ..
                    }) = self.docs.get_mut(&file)
                    else {
                        continue;
                    };
                    // Replies are broadcast; adopt only the answer to this
                    // session's own request (another guest's reply carries
                    // another guest's site id).
                    if *want != nonce {
                        continue;
                    }
                    let buffered = std::mem::take(buffered);
                    let Ok(mut doc) = CollabDoc::from_snapshot(assigned_site, &text, &replica)
                    else {
                        // Corrupt snapshot: give up as if nobody answered.
                        self.docs.remove(&file);
                        events.push(CollabEvent::BootstrapTimedOut { file });
                        continue;
                    };
                    // Ops that raced the reply fold into the bootstrap text;
                    // ones the snapshot already contained integrate as
                    // duplicates (no-ops).
                    for env in &buffered {
                        let _ = doc.apply_remote(&env.op);
                    }
                    let final_text = doc.text().to_string();
                    self.docs
                        .insert(file.clone(), DocState::Live(Box::new(doc)));
                    events.push(CollabEvent::Bootstrapped {
                        file,
                        text: final_text,
                    });
                }
                CollabMsg::Caret {
                    file,
                    site,
                    row,
                    col,
                    name,
                } => events.push(CollabEvent::Caret {
                    file,
                    site,
                    row,
                    col,
                    name,
                }),
                CollabMsg::StreamState {
                    site,
                    name,
                    file,
                    active,
                } => events.push(CollabEvent::StreamState {
                    file,
                    site,
                    name,
                    active,
                }),
                CollabMsg::StreamCancel {} => events.push(CollabEvent::StreamCancel),
            }
        }
        // Give up on bootstraps nobody answered (no owner running).
        let now = std::time::Instant::now();
        let timed_out: Vec<String> = self
            .docs
            .iter()
            .filter_map(|(file, state)| match state {
                DocState::Bootstrapping { deadline, .. } if now > *deadline => Some(file.clone()),
                _ => None,
            })
            .collect();
        for file in timed_out {
            self.docs.remove(&file);
            events.push(CollabEvent::BootstrapTimedOut { file });
        }
        // Re-send unanswered requests: the relay has no replay, so the
        // original broadcast may have missed an owner whose connection was
        // not yet registered (or who connected moments later).
        let mut resend = Vec::new();
        for (file, state) in self.docs.iter_mut() {
            if let DocState::Bootstrapping {
                nonce,
                resend_at,
                resend_every,
                ..
            } = state
                && now >= *resend_at
            {
                *resend_every = (*resend_every * 2).min(SNAPSHOT_RESEND_CAP);
                *resend_at = now + *resend_every;
                resend.push((file.clone(), *nonce));
            }
        }
        for (file, nonce) in resend {
            self.channel
                .send(&CollabMsg::SnapshotRequest { file, nonce });
        }
        events
    }
}

type Peer = std::sync::Arc<std::sync::Mutex<std::os::unix::net::UnixStream>>;

/// One connection: reassemble whole frames from this peer and forward each,
/// atomically, to every other peer. Reframing (not raw byte forwarding) is
/// what keeps two senders' frames from interleaving mid-message at a receiver.
fn relay_client(
    mut rx: std::os::unix::net::UnixStream,
    me: &Peer,
    clients: &std::sync::Arc<std::sync::Mutex<Vec<Peer>>>,
) {
    use crate::session_host::{Frame, FrameReader, encode_bytes_frame};
    let mut reader = FrameReader::new();
    let mut buf = [0u8; 16384];
    loop {
        let n = match rx.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for frame in reader.push(&buf[..n]) {
            let Frame::Bytes(payload) = frame else {
                continue;
            };
            let out = encode_bytes_frame(&payload);
            let mut peers = clients.lock().unwrap();
            peers.retain(|c| {
                std::sync::Arc::ptr_eq(c, me) || c.lock().unwrap().write_all(&out).is_ok()
            });
        }
    }
    clients
        .lock()
        .unwrap()
        .retain(|c| !std::sync::Arc::ptr_eq(c, me));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defining CRDT property: two replicas that make *concurrent* edits
    /// (each unaware of the other's) and then exchange ops must arrive at the
    /// identical text. A naive "apply the remote edit at its original offset"
    /// scheme diverges here (each side's earlier local edit shifted the other's
    /// intended position); cola resolving positions is what makes it converge.
    #[test]
    fn concurrent_edits_converge_on_both_replicas() {
        let mut a = CollabDoc::new(1, "abc");
        let mut b = a.fork(2);

        // Concurrent, position-conflicting inserts at opposite ends.
        let op_a = a.local_insert(0, "X"); // a: "Xabc"
        let op_b = b.local_insert(3, "Y"); // b: "abcY"

        a.apply_remote(&op_b);
        b.apply_remote(&op_a);

        assert_eq!(a.text(), b.text(), "replicas must converge");
        assert_eq!(a.text(), "XabcY");
    }

    /// Concurrent insert and delete over overlapping regions still converge.
    #[test]
    fn concurrent_insert_and_delete_converge() {
        let mut a = CollabDoc::new(1, "hello world");
        let mut b = a.fork(2);

        let op_a = a.local_insert(5, " dear"); // a: "hello dear world"
        let op_b = b.local_delete(6, 5); // b deletes "world" -> "hello "

        a.apply_remote(&op_b);
        b.apply_remote(&op_a);

        assert_eq!(a.text(), b.text(), "replicas must converge");
    }

    /// Causally dependent ops delivered out of order must still converge:
    /// cola backlogs an insertion whose context has not arrived, and the
    /// backlog must be drained (with the stashed text) once the context lands.
    /// Dropping the op instead permanently diverges the replicas.
    #[test]
    fn out_of_order_dependent_inserts_converge_via_the_backlog() {
        let mut a = CollabDoc::new(1, "ab");
        let mut b = a.fork(2);

        let op_c = a.local_insert(2, "c");
        let op_d = a.local_insert(3, "d");
        let op_e = a.local_insert(4, "e");
        assert_eq!(a.text(), "abcde");

        // The network reorders: e and d arrive before their prerequisite c.
        b.apply_remote(&op_e);
        b.apply_remote(&op_d);
        b.apply_remote(&op_c);

        assert_eq!(b.text(), a.text(), "backlogged inserts must drain");
    }

    /// A deletion arriving before the insertion it deletes from is backlogged
    /// and applied once the insertion lands (cola's own canonical example).
    #[test]
    fn deletion_arriving_before_its_insertion_converges_via_the_backlog() {
        let mut a = CollabDoc::new(1, "Hello");
        let mut b = a.fork(2);
        let mut c = a.fork(3);

        let ins = a.local_insert(5, " world!");
        b.apply_remote(&ins);
        let del = b.local_delete(5, 6); // b: "Hello!"

        // c receives the deletion first, without its context.
        c.apply_remote(&del);
        c.apply_remote(&ins);

        assert_eq!(c.text(), b.text(), "backlogged deletion must drain");
        assert_eq!(c.text(), "Hello!");
    }

    /// Replay every span a batch returns onto a shadow string, in order.
    /// Keeping the shadow equal to the doc pins the sequential replay
    /// contract the editor wiring depends on.
    fn replay(shadow: &mut String, spans: &[ResolvedSpan]) {
        for s in spans {
            shadow.replace_range(s.at..s.at + s.deleted, &s.inserted);
        }
    }

    /// `apply_remote` reports exactly what changed, as sequentially
    /// replayable spans — including the backlogged ops a drain applies.
    #[test]
    fn apply_remote_returns_sequentially_replayable_spans() {
        let mut a = CollabDoc::new(1, "ab");
        let mut b = a.fork(2);
        let mut shadow = String::from("ab");

        let op_c = a.local_insert(2, "c");
        let op_d = a.local_insert(3, "d");
        let del = a.local_delete(0, 1); // a: "bcd"

        // Out of order: d backlogs, then c's arrival drains it in one batch.
        replay(&mut shadow, &b.apply_remote(&op_d));
        replay(&mut shadow, &b.apply_remote(&op_c));
        replay(&mut shadow, &b.apply_remote(&del));

        assert_eq!(b.text(), a.text());
        assert_eq!(shadow, b.text(), "spans must replay the doc's changes");
    }

    /// The bootstrap round-trip: a joiner rebuilt from `encode` + text
    /// integrates ops both ways with the peer it joined.
    #[test]
    fn snapshot_bootstraps_a_new_peer_that_then_converges() {
        let mut a = CollabDoc::new(1, "fn main() {}\n");
        let _ = a.local_insert(0, "// header\n");

        let mut b = CollabDoc::from_snapshot(2, a.text(), &a.encode()).expect("decode snapshot");
        assert_eq!(b.text(), a.text());

        // Concurrent edits after the join still converge.
        let op_a = a.local_insert(0, "A");
        let at = b.text().len();
        let op_b = b.local_insert(at, "B");
        a.apply_remote(&op_b);
        b.apply_remote(&op_a);
        assert_eq!(a.text(), b.text());
    }

    /// Corrupt snapshot bytes fail loudly instead of building a replica that
    /// would silently diverge.
    #[test]
    fn snapshot_decode_rejects_corrupt_bytes() {
        let a = CollabDoc::new(1, "abc");
        let mut bytes = a.encode();
        if let Some(last) = bytes.last_mut() {
            *last ^= 0xff;
        }
        assert!(CollabDoc::from_snapshot(2, a.text(), &bytes).is_err());
    }

    /// The bootstrap handshake messages round-trip through the wire encoding.
    #[test]
    fn collab_msg_round_trips_through_serde() {
        let mut a = CollabDoc::new(1, "abc");
        let op = a.local_insert(0, "X");
        for msg in [
            CollabMsg::Op(Envelope {
                file: "src/f.rs".into(),
                site: 1,
                op,
            }),
            CollabMsg::SnapshotRequest {
                file: "src/f.rs".into(),
                nonce: 7,
            },
            CollabMsg::SnapshotReply {
                file: "src/f.rs".into(),
                nonce: 7,
                assigned_site: 2,
                text: a.text().to_string(),
                replica: a.encode(),
            },
        ] {
            let bytes = serde_json::to_vec(&msg).expect("serialize");
            let decoded: CollabMsg = serde_json::from_slice(&bytes).expect("deserialize");
            assert_eq!(
                std::mem::discriminant(&decoded),
                std::mem::discriminant(&msg)
            );
        }
    }

    /// A 0.1.632 peer's Caret (no `name` field on the wire) still parses —
    /// the name defaults to empty — and a named caret round-trips.
    #[test]
    fn caret_without_name_parses_and_name_round_trips() {
        let old = br#"{"Caret":{"file":"f","site":2,"row":0,"col":0}}"#;
        let decoded: CollabMsg = serde_json::from_slice(old).expect("old caret parses");
        match decoded {
            CollabMsg::Caret { name, .. } => assert_eq!(name, ""),
            other => panic!("expected caret, got {other:?}"),
        }
        let named = CollabMsg::Caret {
            file: "f".into(),
            site: 2,
            row: 1,
            col: 3,
            name: "claude".into(),
        };
        let bytes = serde_json::to_vec(&named).expect("serialize");
        match serde_json::from_slice::<CollabMsg>(&bytes).expect("deserialize") {
            CollabMsg::Caret { name, row, col, .. } => {
                assert_eq!((name.as_str(), row, col), ("claude", 1, 3));
            }
            other => panic!("expected caret, got {other:?}"),
        }
    }

    /// The AI-stream wire messages round-trip: StreamState carries the
    /// pilot's identity and target file, StreamCancel is a bare request any
    /// participant may broadcast.
    #[test]
    fn stream_state_and_cancel_round_trip_through_serde() {
        let state = CollabMsg::StreamState {
            site: 2,
            name: "claude".into(),
            file: "src/f.rs".into(),
            active: true,
        };
        let bytes = serde_json::to_vec(&state).expect("serialize");
        match serde_json::from_slice::<CollabMsg>(&bytes).expect("deserialize") {
            CollabMsg::StreamState {
                site,
                name,
                file,
                active,
            } => {
                assert_eq!(
                    (site, name.as_str(), file.as_str(), active),
                    (2, "claude", "src/f.rs", true)
                );
            }
            other => panic!("expected stream state, got {other:?}"),
        }
        let cancel = CollabMsg::StreamCancel {};
        let bytes = serde_json::to_vec(&cancel).expect("serialize");
        assert!(matches!(
            serde_json::from_slice::<CollabMsg>(&bytes).expect("deserialize"),
            CollabMsg::StreamCancel {}
        ));
    }

    /// A frame carrying a CollabMsg variant this build does not know (a newer
    /// peer on the same relay) is skipped by drain, never an error — this is
    /// the interop guarantee the new stream messages rely on against 0.1.633
    /// peers.
    #[test]
    fn drain_skips_unknown_wire_variants() {
        use std::io::Write;
        let (mut tx, rx) = std::os::unix::net::UnixStream::pair().unwrap();
        rx.set_nonblocking(true).unwrap();
        let mut channel = CollabChannel {
            stream: rx,
            reader: crate::session_host::FrameReader::new(),
            role: CollabRole::Guest,
        };
        let bogus = crate::session_host::encode_bytes_frame(br#"{"FutureVariant":{"whatever":1}}"#);
        tx.write_all(&bogus).unwrap();
        let known = CollabMsg::StreamCancel {};
        tx.write_all(&known.encode()).unwrap();
        tx.flush().unwrap();
        // Both frames are already in the socketpair buffer; drain sees them.
        let msgs = channel.drain();
        assert_eq!(msgs.len(), 1, "bogus variant skipped, known one kept");
        assert!(matches!(msgs[0], CollabMsg::StreamCancel {}));
    }

    /// An op serializes and deserializes across the wire (the control channel
    /// will carry these), and integrating the decoded op matches integrating
    /// the original.
    #[test]
    fn ops_round_trip_through_serde() {
        let mut a = CollabDoc::new(1, "abc");
        let mut b = a.fork(2);

        let op = a.local_insert(1, "ZZ"); // a: "aZZbc"
        let bytes = serde_json::to_vec(&op).expect("serialize op");
        let decoded: Op = serde_json::from_slice(&bytes).expect("deserialize op");

        b.apply_remote(&decoded);
        assert_eq!(b.text(), a.text());
        assert_eq!(b.text(), "aZZbc");
    }

    /// The slice-4 extraction path: an arbitrary text-state transition on one
    /// replica, expressed only as (old text, new text), produces ops that make
    /// a fork converge to the new text.
    #[test]
    fn text_delta_ops_converge_a_fork_to_the_new_text() {
        let mut a = CollabDoc::new(1, "hello world");
        let mut b = a.fork(2);

        let ops = text_delta_ops(&mut a, "hello brave new world");
        assert!(!ops.is_empty(), "a real edit must emit ops");
        assert_eq!(a.text(), "hello brave new world");
        for op in &ops {
            b.apply_remote(op);
        }
        assert_eq!(b.text(), a.text());
    }

    /// Multiple disjoint change runs (a delete at the front, a replace in the
    /// middle, an insert at the end) all extract in one pass.
    #[test]
    fn text_delta_ops_handle_multiple_change_runs() {
        let mut a = CollabDoc::new(1, "aaa bbb ccc ddd");
        let mut b = a.fork(2);

        let ops = text_delta_ops(&mut a, "bbb XYZ ddd eee");
        assert_eq!(a.text(), "bbb XYZ ddd eee");
        for op in &ops {
            b.apply_remote(op);
        }
        assert_eq!(b.text(), a.text());
    }

    /// Multibyte chars keep the byte cursor honest: the diff walks chars but
    /// ops address bytes.
    #[test]
    fn text_delta_ops_handle_multibyte_text() {
        let mut a = CollabDoc::new(1, "héllo wörld");
        let mut b = a.fork(2);

        let ops = text_delta_ops(&mut a, "héllo, schöne Wörld");
        assert_eq!(a.text(), "héllo, schöne Wörld");
        for op in &ops {
            b.apply_remote(op);
        }
        assert_eq!(b.text(), a.text());
    }

    /// No change, no ops: the tick-time extraction must be quiescent when the
    /// buffer has not moved (an op here would echo forever between peers).
    #[test]
    fn text_delta_ops_emit_nothing_when_unchanged() {
        let mut a = CollabDoc::new(1, "same text");
        let ops = text_delta_ops(&mut a, "same text");
        assert!(ops.is_empty());
        assert_eq!(a.text(), "same text");
    }

    /// A wholesale buffer swap (the owner's disk-reload path in slice 4) is
    /// just one big transition and still converges a fork.
    #[test]
    fn text_delta_ops_handle_wholesale_replacement() {
        let mut a = CollabDoc::new(1, "the quick brown fox\njumps over\n");
        let mut b = a.fork(2);

        let ops = text_delta_ops(&mut a, "an entirely different\nbuffer\nnow\n");
        assert_eq!(a.text(), "an entirely different\nbuffer\nnow\n");
        for op in &ops {
            b.apply_remote(op);
        }
        assert_eq!(b.text(), a.text());
    }

    /// Extraction composes with concurrency: both replicas diff-extract
    /// concurrent local edits, exchange, and converge.
    #[test]
    fn concurrent_text_delta_ops_converge() {
        let mut a = CollabDoc::new(1, "shared base line");
        let mut b = a.fork(2);

        let ops_a = text_delta_ops(&mut a, "shared MODIFIED base line");
        let ops_b = text_delta_ops(&mut b, "shared base line plus tail");

        for op in &ops_b {
            a.apply_remote(op);
        }
        for op in &ops_a {
            b.apply_remote(op);
        }
        assert_eq!(a.text(), b.text(), "replicas must converge");
    }

    #[test]
    fn char_column_maps_to_byte_offset_across_lines_and_multibyte() {
        // "héllo" is 5 chars / 6 bytes (é is 2 bytes); "wörld" likewise.
        let lines = vec![String::from("héllo"), String::from("wörld")];

        // Within the first line: char col 3 is the second 'l', at byte 4.
        assert_eq!(byte_offset(&lines, 0, 3), 4);
        // End of the first line: byte 6 (before the '\n').
        assert_eq!(byte_offset(&lines, 0, 5), 6);
        // Second line, char col 2 ('r'): 6 bytes + '\n' + "wö" (3 bytes) = 10.
        assert_eq!(byte_offset(&lines, 1, 2), 10);

        // Round-trips both directions for every valid position.
        for (row, line) in lines.iter().enumerate() {
            for col in 0..=line.chars().count() {
                let off = byte_offset(&lines, row, col);
                assert_eq!(position(&lines, off), (row, col), "row {row} col {col}");
            }
        }
    }

    /// The join text `byte_offset` addresses is exactly what `CollabDoc` holds,
    /// so an editor edit expressed in (row, col) integrates at the right place.
    #[test]
    fn editor_position_edit_integrates_into_collabdoc() {
        let lines = vec![String::from("foo"), String::from("bar")];
        let joined = lines.join("\n");
        let mut a = CollabDoc::new(1, &joined);
        let mut b = a.fork(2);

        // Insert "X" at editor position (row 1, col 0) = start of "bar".
        let at = byte_offset(&lines, 1, 0);
        let op = a.local_insert(at, "X");
        b.apply_remote(&op);

        assert_eq!(a.text(), "foo\nXbar");
        assert_eq!(b.text(), a.text());
    }

    #[test]
    fn detached_relay_argv_serves_the_socket() {
        assert_eq!(
            detached_relay_argv(std::path::Path::new("/x/abc.collab.sock")),
            vec!["collab-relay", "--socket", "/x/abc.collab.sock"]
        );
    }

    /// `ensure_relay` and `relay_serve` are attach-or-create: a live relay on
    /// the socket short-circuits both instead of being stolen from (a second
    /// bind would strand the first relay's clients mid-session).
    #[test]
    fn a_live_relay_is_not_stolen_from() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("c.collab.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        ensure_relay(&socket).expect("alive socket short-circuits");
        relay_serve(&socket).expect("alive socket short-circuits");
        // The original listener still owns the socket path.
        drop(listener);
        assert!(!crate::session::is_alive(&socket));
    }

    /// Start a relay on a temp socket and connect a session to it in `role`.
    fn session_pair() -> (tempfile::TempDir, CollabSession, CollabSession) {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.collab.sock");
        {
            let s = socket.clone();
            std::thread::spawn(move || {
                let _ = relay_serve(&s);
            });
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let owner = loop {
            if let Some(ch) = CollabChannel::connect(&socket, CollabRole::Owner) {
                break CollabSession::new(ch, "owner".into());
            }
            assert!(Instant::now() < deadline, "relay never came up");
            std::thread::sleep(Duration::from_millis(10));
        };
        let guest = CollabSession::new(
            CollabChannel::connect(&socket, CollabRole::Guest).expect("guest connects"),
            "guest".into(),
        );
        (dir, owner, guest)
    }

    /// Pump both sessions until `done` says so or the deadline passes,
    /// collecting every event per side. The owner serves one file.
    fn pump(
        owner: &mut CollabSession,
        guest: &mut CollabSession,
        owner_text: &str,
        done: impl Fn(&[CollabEvent], &[CollabEvent]) -> bool,
    ) -> (Vec<CollabEvent>, Vec<CollabEvent>) {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut oe, mut ge) = (Vec::new(), Vec::new());
        while !done(&oe, &ge) {
            assert!(Instant::now() < deadline, "sessions never settled");
            oe.extend(owner.poll(|_| Some(owner_text.to_string())));
            ge.extend(guest.poll(|_| None));
            std::thread::sleep(Duration::from_millis(5));
        }
        (oe, ge)
    }

    /// The full slice-4 handshake headlessly: a guest bootstraps a file from
    /// the owner through the relay, edits it, and the owner's replica
    /// converges; carets travel too.
    #[test]
    fn collab_sessions_bootstrap_and_converge_through_the_relay() {
        let (_dir, mut owner, mut guest) = session_pair();

        guest.request_file("src/f.rs");
        assert!(guest.is_bootstrapping("src/f.rs"));
        let (_, ge) = pump(&mut owner, &mut guest, "hello world", |_, ge| {
            ge.iter()
                .any(|e| matches!(e, CollabEvent::Bootstrapped { .. }))
        });
        let text = ge
            .iter()
            .find_map(|e| match e {
                CollabEvent::Bootstrapped { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(text, "hello world");
        assert!(guest.is_live("src/f.rs"));
        assert!(owner.is_live("src/f.rs"), "answering seeds the owner doc");

        // Guest edits; the owner's replica converges through the relay.
        assert!(guest.local_change("src/f.rs", "hello brave world"));
        guest.send_caret("src/f.rs", 0, 5);
        let (oe, _) = pump(&mut owner, &mut guest, "hello world", |oe, _| {
            oe.iter()
                .any(|e| matches!(e, CollabEvent::RemoteEdit { .. }))
                && oe.iter().any(|e| matches!(e, CollabEvent::Caret { .. }))
        });
        assert_eq!(owner.doc_text("src/f.rs"), Some("hello brave world"));
        assert!(oe.iter().any(
            |e| matches!(e, CollabEvent::Caret { row: 0, col: 5, name, .. } if name == "guest")
        ));
    }

    /// Stream state and cancel travel between sessions without any doc being
    /// live (they are session-level, not doc-gated: the pilot may broadcast
    /// before the file bootstraps, and anyone may cancel).
    #[test]
    fn collab_sessions_carry_stream_state_and_cancel() {
        let (_dir, mut owner, mut guest) = session_pair();

        guest.send_stream_state("src/f.rs", true);
        let (oe, _) = pump(&mut owner, &mut guest, "unused", |oe, _| {
            oe.iter()
                .any(|e| matches!(e, CollabEvent::StreamState { .. }))
        });
        assert!(oe.iter().any(|e| matches!(
            e,
            CollabEvent::StreamState { file, name, active: true, .. }
                if file == "src/f.rs" && name == "guest"
        )));

        owner.send_stream_cancel();
        let (_, ge) = pump(&mut owner, &mut guest, "unused", |_, ge| {
            ge.iter().any(|e| matches!(e, CollabEvent::StreamCancel))
        });
        assert!(ge.iter().any(|e| matches!(e, CollabEvent::StreamCancel)));
    }

    /// Concurrent edits on both sides converge to the identical text.
    #[test]
    fn collab_sessions_concurrent_edits_converge() {
        let (_dir, mut owner, mut guest) = session_pair();
        guest.request_file("src/f.rs");
        pump(&mut owner, &mut guest, "shared base", |_, ge| {
            ge.iter()
                .any(|e| matches!(e, CollabEvent::Bootstrapped { .. }))
        });

        owner.local_change("src/f.rs", "MODIFIED shared base");
        guest.local_change("src/f.rs", "shared base plus tail");
        pump(&mut owner, &mut guest, "shared base", |oe, ge| {
            oe.iter()
                .any(|e| matches!(e, CollabEvent::RemoteEdit { .. }))
                && ge
                    .iter()
                    .any(|e| matches!(e, CollabEvent::RemoteEdit { .. }))
        });
        assert_eq!(owner.doc_text("src/f.rs"), guest.doc_text("src/f.rs"));
    }

    /// No echo: once both sides converge and re-extract their (unchanged)
    /// buffers, the wire goes quiet — an op re-emitted for a remote edit
    /// would bounce between peers forever.
    #[test]
    fn collab_sessions_do_not_echo_applied_remote_edits() {
        let (_dir, mut owner, mut guest) = session_pair();
        guest.request_file("src/f.rs");
        pump(&mut owner, &mut guest, "abc", |_, ge| {
            ge.iter()
                .any(|e| matches!(e, CollabEvent::Bootstrapped { .. }))
        });

        owner.local_change("src/f.rs", "abcX");
        pump(&mut owner, &mut guest, "abc", |_, ge| {
            ge.iter()
                .any(|e| matches!(e, CollabEvent::RemoteEdit { .. }))
        });
        assert_eq!(guest.doc_text("src/f.rs"), Some("abcX"));

        // The app's tick re-extracts each side's buffer, which now equals
        // the replica text: nothing may be sent or received after that.
        assert!(!owner.local_change("src/f.rs", "abcX"));
        assert!(!guest.local_change("src/f.rs", "abcX"));
        for _ in 0..20 {
            assert!(owner.poll(|_| None).is_empty(), "owner must stay quiet");
            assert!(guest.poll(|_| None).is_empty(), "guest must stay quiet");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// A guest with no owner on the relay stops waiting after the deadline
    /// instead of gating input forever.
    #[test]
    fn bootstrap_times_out_without_an_owner() {
        let (_dir, _owner, mut guest) = session_pair();
        // The owner exists but never polls (never answers).
        guest.request_file("src/f.rs");
        let deadline =
            std::time::Instant::now() + BOOTSTRAP_TIMEOUT + std::time::Duration::from_secs(3);
        loop {
            let events = guest.poll(|_| None);
            if events
                .iter()
                .any(|e| matches!(e, CollabEvent::BootstrapTimedOut { .. }))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "bootstrap never timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!guest.is_bootstrapping("src/f.rs"));
        assert!(!guest.is_live("src/f.rs"));
    }

    /// An unanswered resend backs off (a guest cannot tell a lost request
    /// from a large reply still arriving, and a fixed 500ms cadence made
    /// the owner re-serialize the whole document while a slow snapshot was
    /// already in flight) - but the backoff is capped so the schedule still
    /// fits inside [`BOOTSTRAP_TIMEOUT`]. Unbounded doubling put the third
    /// resend past the deadline: an owner whose relay connection registered
    /// between 1.5s and 3s was never re-asked, and the guest silently
    /// degraded that file to local-only editing.
    #[test]
    fn snapshot_resend_backs_off_but_stays_inside_the_bootstrap_window() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.collab.sock");
        {
            let s = socket.clone();
            std::thread::spawn(move || {
                let _ = relay_serve(&s);
            });
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut guest = loop {
            if let Some(ch) = CollabChannel::connect(&socket, CollabRole::Guest) {
                break CollabSession::new(ch, "guest".into());
            }
            assert!(Instant::now() < deadline, "relay never came up");
            std::thread::sleep(Duration::from_millis(10));
        };
        guest.request_file("src/f.rs");
        // Fire three resends back to back (pulling the timer into the past
        // each time) and read how far out each one rescheduled itself.
        let mut waits: Vec<Duration> = Vec::new();
        for _ in 0..3 {
            {
                let Some(DocState::Bootstrapping {
                    resend_at,
                    deadline,
                    ..
                }) = guest.docs.get_mut("src/f.rs")
                else {
                    panic!("guest must still be bootstrapping");
                };
                *resend_at = Instant::now() - Duration::from_millis(1);
                *deadline = Instant::now() + Duration::from_secs(60);
            }
            let _ = guest.poll(|_| None);
            let Some(DocState::Bootstrapping { resend_at, .. }) = guest.docs.get("src/f.rs") else {
                panic!("guest must still be bootstrapping");
            };
            waits.push(resend_at.saturating_duration_since(Instant::now()));
        }
        assert!(
            waits[0] > SNAPSHOT_RESEND,
            "an unanswered resend must back off past the initial cadence, got {waits:?}"
        );
        for w in &waits {
            assert!(
                *w <= Duration::from_secs(1),
                "the backoff must cap at 1s so every resend fits inside the \
                 bootstrap window, got {waits:?}"
            );
        }
        // And the capped schedule provably fits: initial wait plus two
        // capped waits still lands the third resend before the deadline.
        assert!(
            SNAPSHOT_RESEND + Duration::from_secs(2) < BOOTSTRAP_TIMEOUT,
            "the resend schedule no longer fits inside BOOTSTRAP_TIMEOUT"
        );
    }

    /// A SnapshotRequest broadcast before the owner is reachable is not
    /// lost for good: the relay has no replay, so the guest must re-send
    /// while bootstrapping and an owner that appears moments later still
    /// answers.
    #[test]
    fn guest_resends_snapshot_request_until_an_owner_answers() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.collab.sock");
        {
            let s = socket.clone();
            std::thread::spawn(move || {
                let _ = relay_serve(&s);
            });
        }
        // A registered observer proves when the relay has actually
        // broadcast the request (frames broadcast when the relay reads
        // them, not when the guest writes them).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observer = loop {
            if let Some(ch) = CollabChannel::connect(&socket, CollabRole::Guest) {
                break ch;
            }
            assert!(Instant::now() < deadline, "relay never came up");
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut guest = CollabSession::new(
            CollabChannel::connect(&socket, CollabRole::Guest).expect("guest connects"),
            "guest".into(),
        );
        guest.request_file("src/f.rs");
        // Once the observer sees the request, the broadcast is over — and
        // no owner was connected to receive it. That copy is gone forever.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !observer
            .drain()
            .iter()
            .any(|m| matches!(m, CollabMsg::SnapshotRequest { .. }))
        {
            assert!(Instant::now() < deadline, "relay never broadcast");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut owner = CollabSession::new(
            CollabChannel::connect(&socket, CollabRole::Owner).expect("owner connects"),
            "owner".into(),
        );
        let (_, ge) = pump(&mut owner, &mut guest, "hello world", |_, ge| {
            ge.iter().any(|e| {
                matches!(
                    e,
                    CollabEvent::Bootstrapped { .. } | CollabEvent::BootstrapTimedOut { .. }
                )
            })
        });
        assert!(
            ge.iter()
                .any(|e| matches!(e, CollabEvent::Bootstrapped { .. })),
            "guest gave up instead of re-sending: {ge:?}"
        );
    }

    /// End-to-end transport: two participants connected through the relay
    /// exchange an op and converge, with the relay never inspecting it.
    #[test]
    fn ops_converge_through_the_relay() {
        use crate::session_host::{Frame, FrameReader};
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("c.collab.sock");
        {
            let s = socket.clone();
            std::thread::spawn(move || {
                let _ = relay_serve(&s);
            });
        }
        // Wait for the relay to bind.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ca = loop {
            if let Ok(s) = UnixStream::connect(&socket) {
                break s;
            }
            assert!(Instant::now() < deadline, "relay never came up");
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut cb = UnixStream::connect(&socket).expect("second client connects");
        ca.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        cb.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

        let mut a = CollabDoc::new(1, "abc");
        let mut b = a.fork(2);

        // Read framed envelopes off a stream until one arrives.
        fn read_envelope(stream: &mut UnixStream, reader: &mut FrameReader) -> Envelope {
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).expect("relay delivers a frame");
                for frame in reader.push(&buf[..n]) {
                    if let Frame::Bytes(payload) = frame {
                        return serde_json::from_slice(&payload).unwrap();
                    }
                }
            }
        }
        let mut reader_a = FrameReader::new();
        let mut reader_b = FrameReader::new();

        // B edits first and A waits for the forwarded frame: `connect`
        // returning does not mean the accept loop has registered a client
        // yet, but A *receiving* B's envelope proves both ends are
        // registered (B to be forwarded from, A to be forwarded to), so
        // A's own send below cannot fall into the registration window.
        // (Live participants tolerate that window by design: joining the
        // relay only promises messages sent after the join completes, and
        // the bootstrap handshake is what synchronizes real sessions.)
        let op_b = b.local_insert(0, "Y"); // b: "Yabc"
        cb.write_all(
            &Envelope {
                file: "f.rs".into(),
                site: 2,
                op: op_b,
            }
            .encode(),
        )
        .unwrap();
        a.apply_remote(&read_envelope(&mut ca, &mut reader_a).op);
        assert_eq!(a.text(), "Yabc");

        // Now the reverse direction: A edits, B integrates, both converge.
        let op_a = a.local_insert(4, "Z"); // a: "YabcZ"
        ca.write_all(
            &Envelope {
                file: "f.rs".into(),
                site: 1,
                op: op_a,
            }
            .encode(),
        )
        .unwrap();
        let got = read_envelope(&mut cb, &mut reader_b);
        b.apply_remote(&got.op);

        assert_eq!(got.file, "f.rs");
        assert_eq!(b.text(), a.text());
        assert_eq!(b.text(), "YabcZ");
    }
}
