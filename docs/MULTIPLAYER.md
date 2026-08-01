# SSH Multiplayer: design

Status: PR 3a (the `session-host` mux, src/session_host.rs) SHIPPED in
0.1.627: server, client pump, frame protocol, min-of-clients winsize,
server-side read-only with grant/revoke, presence sidecar plus frames, exit
code propagation, wired into `croft attach` and the remote launch tail with
a dtach fallback. PR 3b (participants UI) SHIPPED in 0.1.628: the host
exports a token-authenticated privileged control channel to the inner croft
(Inner/Kick verbs), which polls the presence sidecar, shows an "N attached"
status badge, announces joins and leaves, and drives grant/revoke/disconnect
from Session: Participants (Cmd+K A). PR 3c (attributed carets) SHIPPED in
0.1.629: the host sends Typing frames to the privileged channel whenever
the writing client changes (ordered before that client's bytes reach the
PTY), and croft hands the shared cursor over on each switch, parking the
previous typist's caret, restoring the new typist's, and painting everyone
else's position as a colored ghost caret. Phases 3a through 3c were then
field-tested over a real SSH session on a Linux host with the musl binary:
byte-transparent broadcast to two clients, the presence roster, first-attacher
write control with server-side read-only enforcement, the 0600 socket, the
zero-winsize guard, and inner exit-code propagation over SSH all verified end
to end (see "Field test" below).

Phase D (independent viewports) was deferred until shared-viewport multiplayer
proved itself; it now has and Phase D SHIPPED its first live co-editing cut in
0.1.632: [`src/collab.rs`] (a replicated text document over the `cola` CRDT,
diff-based op extraction, the bootstrap handshake, and the per-participant
session state machine) wired into the running app (`App::poll_collab`), with
`croft attach --solo` / `croft remote <host> <path> --solo` as the opt-in
launch. See "Slice 4 as shipped" below for what landed and the deliberate
deviations. 0.1.633 added named caret tags and `croft collab-agent`, an MCP
seat that lets an external AI co-edit with a visible named caret (see "Named
carets and the AI seat"). 0.1.634 added `croft pair`, a real-time AI
collaborator whose edits stream into the shared buffers token by token and
which any participant can cancel mid-run (see "croft pair"). 0.1.635 made
that pilot a croft-hosted **resident navigator**: activate once per
workspace, ask it on a line or selection, yield it comment-only review
turns, and read its line-anchored notes in the editor (see "The resident
navigator"). The rest of this document is the design the shipped phases
followed. It exists so the multiplayer pillar starts from croft's real
architecture instead of from a Live Share mental model that does not fit a
single-process TUI.

## Field test (0.1.630, real SSH, Linux musl)

Cross-built `x86_64-unknown-linux-musl` and driven over `ssh -tt` against a
Linux box, with a benign inner command in place of the croft TUI for
deterministic byte assertions:

- `session-host --probe` (the remote launch-tail guard) exits 0 on Linux.
- Two clients co-attached: a shared banner emitted after both attached reached
  both clients (verbatim broadcast).
- Presence sidecar showed both participants with `control: true` for the first
  attacher and `control: false` for the second (first-attacher control rule).
- The control holder's keystrokes reached the PTY and broadcast to both; the
  read-only client's keystrokes appeared in neither output (server-side
  enforcement, not advisory).
- The socket was mode `0600` from creation (bound in a private 0700 staging
  dir and renamed into place — `session::bind_socket_0600`, shared with the
  collab relay; the earlier umask save/restore was process-global and racing
  binds could corrupt it). Creation is serialized per target by a lock file
  held across probe, stale removal, bind and publish, so two attach-or-create
  racers can never both publish and strand the earlier listener pathless: the
  loser is told the winner is alive and attaches. And two clients that
  reported `0x0` size did not shrink the shared PTY (zero-winsize guard).
- An inner `exit 7` propagated through the mux and back over SSH as exit 7
  (the drop-to-local code path).

## Goal

Two or more people work in the same croft session over SSH: they see the
session, they know who else is present, a guest can be read-only or granted
control, and eventually each participant has an attributed, colored caret.
The local (`croft attach`) and remote (`croft <host>`) paths must behave
identically per the project's golden rule.

## What exists today (facts, with citations)

The design leans on two invariants of the current codebase.

**croft renders exactly one view per process.** `app::run`
(`src/app/mod.rs:29963`) constructs one `App`, one
`Terminal<CrosstermBackend>` over the process `stdout()`
(`src/app/mod.rs:30087`), one blocking crossterm event loop
(`src/app/mod.rs:30744`), and one global terminal size read via
`window_size()` (`src/app/mod.rs:3581`). Inline images are emitted with
absolute cursor addressing every frame (`src/app/mod.rs:30636`). There is no
per-client anything; a second independent viewport would need a second
backend, a second event source, and a second geometry, none of which exist.

**Sessions already survive detach and accept multiple clients.** Both the
remote launch (`src/remote.rs:1924`) and local `croft attach`
(`src/session.rs:126`) wrap croft in `dtach -A <socket> -E -z -r winch`,
socket keyed by a deterministic hash of the workspace path
(`src/session.rs:33`, `src/remote.rs:1934`). dtach broadcasts PTY output to
every attached client and merges every client's input into the one PTY.
Two people attaching the same path today already get a shared screen. What
dtach cannot do: say who is attached, distinguish which client typed a byte,
enforce read-only, or reconcile two different window sizes (last writer
wins).

The editor side (`src/widgets/editor.rs`) is equally single-author: the
buffer is a `Vec<String>`, undo is a stack of full-buffer snapshots that
assumes one linear timeline (`src/widgets/editor.rs:1111`), LSP sync is
full-text only (`src/lsp/client.rs:434`), there is no single apply-edit
chokepoint, and a split view is two independent copies of the file re-read
from disk (`src/app/mod.rs:9943`). There is no sync engine anywhere.

## The key insight: shared process means no sync engine

The classic multiplayer question is "CRDT or OT?". For croft the honest
answer is **neither, for a long time**. CRDTs and OT exist to reconcile
concurrent edits made against divergent replicas. In a shared-session model
there is exactly one `App`, one buffer, and one PTY input stream: the mux
serializes all participants' input into a total order before it ever
touches the editor. There are no replicas, so there is nothing to
reconcile. Undo stays a single timeline (documented, acceptable: that is
also what a shared tmux pane gives you, and it is what pairing users
expect).

A sync engine only becomes necessary for **independent viewports** (each
person scrolls and opens files independently), because that requires one
croft process per participant and therefore replicated buffers. That is the
Live Share tier, it is a rewrite-scale effort against every constraint
listed above, and it is deliberately deferred (see Phase D).

## Buy vs build

Verified externally 2026-07-13:

- **dtach** (in use today): byte-transparent, but no presence, no per-client
  input attribution, no read-only, winsize last-writer-wins.
- **abduco**: adds `-r` read-only attach, but it is client-side only ("not a
  security feature, but only a convenient way to avoid accidental keyboard
  input" per the man page); still no presence and no input attribution.
- **tmate / upterm**: real sharing products, but both are built on tmux,
  which reparses and re-emits the terminal stream and corrupts Kitty
  graphics; croft chose dtach over tmux for exactly this reason
  (`src/remote.rs:1917`). Disqualified.
- **screen -x**: shared screen, same gaps as dtach, plus its own escape
  handling.

No existing tool provides byte-transparent multi-attach with server-side
read-only, presence, and attributed input. Per the build-vs-buy rule, croft
builds it: a small session host that takes over dtach's role for croft
sessions.

## Architecture: `croft session-host` (the mux)

A hidden subcommand, spawned exactly where dtach is exec'd today
(`src/session.rs:126` locally, the shell tail at `src/remote.rs:1924`
remotely). One binary, no new dependency; dtach remains only as a fallback
for old remote binaries during the transition.

```
guest tty ── croft attach ──┐
                            ├── unix socket ── session-host ── PTY master ── croft app
host  tty ── croft attach ──┘        (one accept loop,          (unchanged,
                                      per-client state)          one process)
```

- **Server**: opens a PTY pair, spawns the inner croft on the slave side,
  listens on the same `~/.cache/croft/sessions/<hash>.sock` path scheme the
  dtach socket uses today (socket mode 0600; possession of the UNIX account
  is the trust boundary, same as dtach). Broadcasts PTY output bytes to
  every client verbatim: **no reparsing, no re-emission**, preserving the
  byte transparency that made dtach the original choice.
- **Client**: `croft attach <path>` (and the remote `-tt` command) becomes a
  thin raw-mode pump: stdin to socket, socket to stdout, WINCH to a resize
  frame. Functionally what dtach's client does now, plus identity.
- **Framing**: output to clients is raw bytes prefixed by a 1-byte type tag
  and length (type `0` = PTY bytes, type `1` = control). Control frames are
  NDJSON, reusing the exact pattern of `src/mcp/transport.rs:26` (compact
  JSON + newline): `hello {name, cols, rows, want_control}`, `resize`,
  `presence {participants: [...]}` , `grant`/`revoke`, `detach`. Client
  input frames to the server carry the client id implicitly (one socket
  connection per client), which is what gives **input attribution** for
  free.
- **Winsize policy**: the PTY winsize is the minimum cols and minimum rows
  across connected clients (the tmux rule). On any client attach, detach, or
  resize, the server recomputes and sends SIGWINCH; croft's existing
  resize-wipe plus `mode_reassert_seq()` path (`src/app/mod.rs:30772`,
  gated on `CROFT_SESSION_PERSISTENT`) already handles the repaint, so the
  dead-mouse class of bug is already covered by the shipped machinery.
- **Input policy**: server-side. A read-only client's PTY-byte frames are
  dropped at the server; only control frames pass. This is enforcement, not
  abduco's advisory client-side mute.
- **Persistence**: identical to dtach semantics. Clients dying leaves the
  server and inner croft running; `croft ls` (`src/session.rs:192`) keeps
  working unchanged because liveness is still "can I connect to the
  socket". Bonus once stable: the dtach external dependency can be dropped
  entirely, including the `croft_pkg_install dtach` step in the remote
  installer (`src/remote.rs:2182`).
- **Detach chord**: the mux finally provides what the code comment at
  `src/session.rs:9` deferred: a control channel exists, so an in-app
  detach command can send a `detach` control frame instead of needing a
  dtach escape key.

### How the inner croft learns about participants

The inner croft cannot see socket clients (it only has a PTY). The server
writes a presence sidecar next to the socket
(`~/.cache/croft/sessions/<hash>.presence.json`, atomic rename on change)
and the inner croft, when `CROFT_SESSION_PERSISTENT=1`, polls it on the
existing tick (the loop already drains a dozen pollers per iteration,
`src/app/mod.rs:30540`). A file is chosen over a second socket because the
relay protocol already proved file-based signaling works across every
transport croft has (`src/remote.rs:726`), and it keeps the inner croft's
event loop untouched. Guest input attribution (Phase C) upgrades this to a
control FD passed to the inner croft, but presence and the participants UI
need only the file.

## Transport

**Reuse the existing SSH plumbing wholesale; add nothing.** A remote guest
runs `croft <host> <path>` exactly as today: the launch flow
(`src/remote.rs:583`) opens its own ControlMaster, sees croft installed,
and the remote command attaches the existing session socket instead of
creating a new one. No reverse tunnels, no new daemon, no new auth surface:
reaching the socket requires SSH access to the host account, which is the
same trust boundary the drop relay and dtach already accept. A local guest
on the same machine attaches the socket directly. `ssh -O forward` style
tunneling (`src/remote.rs:1323`) is available if a socket ever needs to
cross machines, but the design needs no case for it: the session always
lives where the workspace lives.

## Presence and permissions

- Identity: `hello` carries a name (default `$USER@hostname`, overridable
  by `--as <name>`); the server assigns a stable color index.
- The first client to create the session is the **owner** and attaches
  interactive. Later clients attach **read-only by default**.
- A read-only guest requests control via a control frame; the owner sees a
  toast (existing toast machinery) and grants or denies; grants are
  revocable. The server is the enforcement point.
- The status line shows a participant count when more than one client is
  attached; a popup (following the existing popup conventions, every menu
  item gets a shortcut) lists participants, their state, and grant/revoke
  actions for the owner.

## Attributed guest carets (the co-editing step)

Once input is attributed, a guest with control can either drive the shared
primary cursor (Phase B, simplest, exactly like pairing over tmux but with
names and permission), or drive their **own caret** (Phase C). The editor
already has the machinery for the latter: `carets: Vec<EditorSelection>`
(`src/widgets/editor.rs:1378`) and `multi_apply`
(`src/widgets/editor.rs:3262`) fan an edit out across carets. Phase C
routes attributed guest keystrokes to a per-guest caret instead of the
primary cursor, rendering it in the guest's color. Because everything still
happens inside one buffer in one process, undo, LSP sync, save, and the FS
watcher all keep working unchanged. Undo remains one shared timeline;
that limitation is documented, not solved, until Phase D ever happens.

Follow mode falls out of Phase B for free: everyone shares one viewport by
construction.

## Phase D (green-lit, in progress): independent viewports

True Live Share (per-participant files, scroll, and cursor with a shared
document set) requires one croft process per participant with replicated
buffers, and only here does a CRDT enter (per-buffer sequence CRDT; OT is
rejected outright since croft has no central server authority to transform
against and CRDTs are the settled answer in 2026). The CRDT foundation is
built ([`src/collab.rs`], the `cola` crate wrapped as [`CollabDoc`], convergence
and serde round-trip tested). Slices, with status:

1. **Done.** `CollabDoc` over `cola`: concurrent inserts/deletes converge, ops
   serialize for the wire.
2. **Done.** Editor coordinate bridge: `byte_offset`/`position` map the editor's
   `(row, char-column)` to `CollabDoc`'s linear byte offsets, UTF-8 tested.
3. **Done.** `Envelope` (per-file, per-site op) and `relay_serve`
   (dumb fan-out over `<hash>.collab.sock`); convergence through the relay is
   tested headlessly.
4. **Done (0.1.632).** The live wiring: solo-viewport launch on both sides,
   diff-based op extraction against real buffers, bootstrap, save ownership,
   and peer carets. Details below.

### Slice 4 as shipped (0.1.632)

The design's per-edit chokepoint problem was solved by **diffing instead of
refactoring**: each shared file pins the `edit_seq` it last synced at
(`Editor::collab_synced_seq`); when the seq moves, `text_delta_ops` char-diffs
the replica's text against the buffer and emits convergent ops
(`similar::TextDiff`, bounded by a 200ms diff timeout — a timed-out diff is
coarser but still a valid edit script). Multi-cursor, paste, undo, and
wholesale reloads all reduce to text-state transitions, and any divergence
self-heals at the next resync. Three invariants hold, each pinned by tests:

1. **Extract-before-apply, per tick.** `App::poll_collab` diffs local edits
   into ops before draining inbound ones; the reverse order would re-diff a
   just-applied remote edit and rebroadcast it (echo storm).
2. **Echo suppression.** Applying a remote span bumps `edit_seq` (via
   `apply_span_edits` → `mark_buffer_changed`), so `collab_synced_seq` is
   re-pinned immediately after; the headless two-App test asserts wire
   quiescence after convergence.
3. **One reloader.** Guests suppress disk reload of workspace files (the
   replica is authoritative); the owner's reload flows through the tick diff,
   so a git checkout is just a large local edit.

The rest of the shipped shape:

- **Bootstrap.** A guest opening a workspace file broadcasts
  `SnapshotRequest{file, nonce}`; only the owner answers, with the canonical
  text plus the cola-encoded replica and an owner-allocated site id (owner is
  site 1 and the single allocator, so ids never collide; the allocation
  counter is seeded from the owner's pid, so a restarted owner cannot
  re-issue an id a surviving guest still holds; nonces are seeded
  from the process id so two guests never adopt each other's reply). The
  wire-supplied file key is contained on the owner side
  (`collab::contained_path`: every component a plain name), so a traversing
  or absolute key from any guest — the MCP collab agent forwards caller
  input verbatim — can never read or edit outside the workspace. Ops
  arriving mid-bootstrap are buffered and replayed (duplicates integrate as
  no-ops). The relay has no replay, so an unanswered request is **re-sent
  while bootstrapping, starting at 500ms and doubling on every further
  unanswered resend, capped at 1s** — the original broadcast can miss an
  owner whose connection the relay has not yet registered (RED-tested via a
  registered observer that proves the first broadcast completed ownerless),
  but a guest cannot tell a lost request from a large reply still in flight,
  and a fixed cadence made the owner re-serialize and blocking-write the
  whole document every 500ms while a slow snapshot was already arriving.
  The cap matters as much as the doubling: unbounded, the third resend
  landed past the 3s bootstrap deadline (0.5s, 1.5s, 3.5s) and an owner
  registering in the back half of the window was never re-asked; capped,
  the schedule is 0.5s, 1.5s, 2.5s — the same late-owner coverage as the
  old fixed cadence at half the duplicate-snapshot spam.
  The file is input-gated (one gate, at the editor key dispatch)
  until the snapshot lands; with **no owner on the relay the bootstrap times
  out after 3s** and the file degrades to plain local editing — a deviation
  from "read-only until ready", chosen so a lone solo guest is not read-only
  forever. The give-up is a **latch** (`DocState::LocalOnly`), not a
  removal: the app tick re-requests any open file that is neither live nor
  bootstrapping, so a forgotten file re-entered the input gate 16ms after
  every timeout, forever. A local-only file is exempt from the guest save
  gate (the guest is its only author; with no owner, a refused save meant
  the work could never persist), and only the collab agent's `collab_open`
  explicitly rejoins past the latch.
- **Backlog correctness.** `CollabDoc::apply_remote` drains cola's causal
  backlog (out-of-order delivery), stashing insertion text by run identity;
  silently dropping unmergeable ops (the slice-3 gap) permanently diverged
  replicas and is RED-tested.
- **Owner-side tabs.** Answering a request opens the file as a background tab
  through the normal open machinery, so guest edits land in a real buffer and
  saves, auto-save, history snapshots, and reload suppression all apply
  through the one code path.
- **Save ownership (v1: host owns all files).** Guest Cmd+S and auto-save are
  no-ops with a status hint for workspace files; guest search Replace All is
  blocked; guest LSP-rename skips closed-file disk writes (open tabs flow as
  ops). The owner's search Replace All converges through its own reload-diff
  rather than rerouting search internals through the buffer (invariant 3
  makes both equivalent).
- **Carets.** `CollabMsg::Caret` broadcasts each participant's cursor
  (throttled to actual moves); peers paint them through the existing
  `ghost_carets` machinery in the participant's color.
- **Split panes of a live file mirror the session.** Every buffer of a live
  file carries the `CollabDoc::text_gen` it last synced at (0 = never
  attached). A buffer created after the file went live — a split duplicate,
  a close-and-reopen — holds disk text that is stale by construction, so it
  never extracts (its diff against the doc would broadcast a
  delete-everything/reinsert-the-old-file op set and revert every peer) and
  is seeded from the replica on the next tick; attached panes whose
  generation falls behind mirror the doc text. Remote spans land on every
  attached pane. "Only the first-found tab stays synced" was unstable —
  focusing another group reorders which pane is found first — and a behind
  pane accepting a keystroke silently deleted the other pane's work.
- **Link loss is detected.** EOF or a hard error on the relay channel (and
  any failed send) latches the channel dead; the app drops the session with
  a status hint and reconnects through the normal 2s-backoff path, and
  guest files re-bootstrap. EOF used to be indistinguishable from an idle
  socket, so a killed relay left `is_live` true forever while every op
  vanished — with saves still gated, a guest's work existed only in RAM.
  The relay's forwarding writes are bounded too
  (`SO_SNDTIMEO`, 2s, wedged peer dropped with a shutdown so its reader
  sees EOF), and `write_frame_blocking` gives up after 5s: forwarding holds
  the global client mutex, so one SIGSTOPped peer used to deadlock the
  relay and freeze every participant's UI thread mid-send.
- **Not shipped, deliberately.** The participants-menu "open solo viewport"
  action: a viewport is a *process on the guest's machine*, which the host
  croft cannot conjure over the mux — the affordance is the `--solo` flag on
  the guest's own launch command. Same-file collab docs are
  keyed per workspace-relative path; files outside the workspace are never
  shared (enforced by `contained_path`, above).

Still deferred (documented, not blockers): shared undo timeline (undo stays
per-process; undoing a peer's edit is just an edit and converges), shared
LSP (each croft syncs full text to its own servers), per-file save handoff.

### Named carets and the AI seat (0.1.633)

Two follow-ups on the shipped slice 4:

- **Named caret tags.** `CollabMsg::Caret` now carries the sender's display
  name (`#[serde(default)]`, so 0.1.632 peers interoperate in both
  directions: their carets parse with an empty name and render as before).
  Site ids are allocated per file bootstrap, so identity has to ride the
  wire — never try to join carets across files by site id. While a peer's
  caret moves or types, its name paints on the visual row above the caret
  (below when the caret sits on the viewport's top row), VS Code Live Share
  style, and disappears 2s after the caret rests. The fade needs exactly one
  redraw with no accompanying wire traffic: `App::collab_labels_dirty` flips
  a visible-set bit and feeds the tick's OR of dirty flags. A parked caret's
  re-broadcast does not refresh its tag (only real moves do), and names are
  truncated to 24 chars at both send and ingest so a hostile peer name can
  never paint a whole row. The local name defaults to the mux identity
  (`user@host`), overridable via `CROFT_COLLAB_NAME`.

- **`croft collab-agent`: an AI participant with a visible caret.** A hidden
  headless subcommand (`src/collab_agent.rs`) that joins the workspace's
  relay as a regular guest and exposes the session as an MCP server on stdio
  (JSON-RPC 2.0 over NDJSON, the same shapes croft's own MCP client speaks).
  Register it once and any MCP-speaking agent gets a seat:

      claude mcp add croft-collab -- croft collab-agent --workspace <abs path>

  Five tools: `collab_open` (bootstrap a file and return its text; a clean
  error after ~4s when no croft owner serves the workspace), `collab_read`,
  `collab_replace` (whole-text replace; the diff engine turns it into minimal
  convergent ops), `collab_caret` (0-based row/col — the named caret humans
  watch), and `collab_status` (tracked files plus peers' last-seen carets).
  A background thread pumps the session every 30ms so the replica stays
  fresh between tool calls; the stdin loop answers requests; EOF ends the
  seat. Croft contains no LLM code — the intelligence is whatever drives the
  tools. The agent inherits every guest property for free: owner-only disk
  writes (it cannot touch files; the owner persists what it sees), per-file
  site ids, CRDT convergence, and the same trust boundary (the 0600 relay
  socket under the same UNIX account). Default caret name `claude`; `--name`
  overrides.

### croft pair: a real-time AI collaborator (0.1.634)

The MCP seat above proved the wiring but not the experience: MCP tool
arguments arrive whole, so an AI edit lands as one bulk insert. `croft pair`
(src/pair.rs) fixes that by owning the token stream itself. It spawns the
`claude` CLI as a persistent stream-json conversation on stdio
(`--input-format stream-json --output-format stream-json
--include-partial-messages`), reads token-level `text_delta` events, and
teaches the model — via `--append-system-prompt` — a fenced edit protocol in
its ordinary streamed text:

    <<<EDIT <file>:<start_row>:<start_col>-<end_row>:<end_col>>>>
    <replacement text>
    <<<END>>>

Coordinates are 0-based CHARACTER positions against the file's current text
(byte offsets only ever come from `collab::byte_offset`; a column is never
bytes). The pilot parses the stream incrementally (`FenceMachine`, exact
about deltas split anywhere, even mid-marker) and applies each body fragment
through a regular collab guest seat: the fence's range is deleted in one
replica change, then every `text_delta` inserts at a tracked byte `anchor`
via `local_change`, so peers watch the edit appear token by token with the
pilot's named caret riding the stream. Everything outside a well-formed
fence is commentary printed to the pilot's terminal, never applied; a
malformed header degrades the whole block to commentary, so bad model output
can never corrupt a buffer.

Concurrent human edits are safe: the pilot's pump transforms `start` and
`anchor` through every incoming `RemoteEdit` span (before: shift by the size
delta; straddling: clamp to the span's new end; after: unchanged). One
documented loss: a human edit strictly INSIDE the streamed region is
discarded if the stream is later cancelled (the revert restores the original
slice over the whole region).

Cancel is first-class, from any participant: two wire messages ride the
relay (`CollabMsg::StreamState { site, name, file, active }`, broadcast by
the pilot at stream start/end/cancel, and `CollabMsg::StreamCancel {}`; both
serde-tolerant, so 0.1.633 peers just drop them). While a stream is active,
croft shows an orange status badge and an orange `■` stop button in the
editor gutter on the row under the pilot's caret; clicking it, `Cmd+K X`,
or the palette's "Collab: Cancel AI Stream" broadcasts the cancel. The pilot
then sends a `control_request` interrupt on claude's stdin (feature-detected
via the init capabilities; the fallback simply drops the rest of the turn's
deltas — the child is never killed, the conversation survives), reverts the
streamed region in one `local_change`, broadcasts the stream inactive, and
prepends a note to the next user turn so the model knows its edit was
rejected. An unterminated fence at end of turn reverts the same way.

The claude child is sandboxed to a read-only toolbox (`Read`/`Grep`/`Glob`
plus a second, read-only `collab-agent` MCP seat named `<name>-reader` for
live buffer queries, `--strict-mcp-config`); the ONLY write path is the
fence through the pilot's seat. End-to-end tests drive the pilot against a
scripted fake claude (python3, like the LSP fakes): streaming convergence,
the StreamState lifecycle, and the cancel drill (revert + the interrupt
landing on the fake's stdin) all assert headlessly.

### The resident navigator (0.1.635)

0.1.634's pilot was a terminal REPL: a second terminal to keep open and a
`croft attach` prerequisite the user rightly called clanky. 0.1.635 makes
the pilot a workspace resident that croft itself hosts, and turns the
interaction into turn-based driver/navigator pairing:

- **Activation.** `croft pair [--workspace <p>] [--model <m>] [--name <n>]`
  now writes a `<hash>.pair.json` sidecar (next to the workspace's collab
  socket, `session::pair_record_path`), ensures the relay, and exits. A
  running croft stats the record on a 1s cadence
  (`App::maybe_seat_navigator`) and seats the pilot in-process
  (`src/pair_host.rs`, `PairHost` — the `run_pilot` bootstrap minus the
  REPL, its voice rerouted from stdio to `PairEvent`s the App drains each
  tick). The resident navigator takes its instructions in-editor
  (`Cmd+K Q`), so a start task is not persisted — it would re-fire on every
  launch, and an `@file` task would freeze the tick thread on seat (the
  owner that must serve the file *is* that thread). The hidden `--repl`
  flag still takes a one-shot task for the old terminal driver. `croft pair
  --off` or the "Navigator: Activate or Deactivate" palette entry unseats
  it.
- **The owner-seat requirement.** The pilot's guest seat bootstraps files
  via SnapshotRequest, and only a collab OWNER answers. A plain `croft`
  launch has no collab session at all, so activation self-appoints it: the
  App creates the owner-role channel itself (ensure_relay + direct
  connect) before seating the pilot. Solo guests never host — the owner's
  croft does. Exactly one croft may self-appoint per workspace: an advisory
  `flock` on `<hash>.pair-host.lock` (`session::try_acquire_pair_host_lock`)
  guards it, so a second window is refused ("hosted by another croft
  window") instead of both claiming owner site 1 and corrupting the buffer.
  The OS drops the lock when the holder exits, so a crashed host hands off
  automatically. (The non-hosting window does not yet observe the owner's
  edits as a guest — a documented follow-up; `croft attach` is the shared
  path for that today.)
- **Ask turns (may edit).** Right-click the gutter ("Ask Navigator"), a
  selection ("Ask Navigator About Selection"), or `Cmd+K Q`: an input box
  takes the instruction, and the turn carries the 0-based line/range, the
  selected text, and the buffer numbered `N|` per line (the prompt teaches
  that the prefix is a label, not content). Fence edits stream with the
  full 0.1.634 cancel machinery. Only one turn runs at a time: an ask or
  yield fired while another is still streaming is refused ("mid-turn; wait
  for it to finish"), so the shared `comment_only` flag is never clobbered
  mid-stream.
- **Yield turns (comment-only, host-enforced).** `Cmd+K Y` hands the
  navigator the floor on the active file: the turn carries the unified
  diff since its last look plus the numbered buffer, and the host DISCARDS
  any EDIT fence it emits (`PairState::comment_only`, reset at the turn's
  result) — the prompt rule is advisory, the gate is not.
- **Comment boxes (the navigator's single voice).** A NOTE fence
  (`<<<NOTE <file>:<row>>>>` … `<<<END>>>`, 0-based row) anchors a remark
  to a line. Each note renders as a **comment box** in the editor: an
  unnumbered block between its anchor line and the next, shifting the
  rows below it, owning no buffer position and never touching the saved
  file (`VisRow::Box` rows in the editor's layout). The box shows the
  author, the body, and a footer with a reply field and a `✕ Ignore`
  button. Notes live in the pilot as byte offsets shifted through every
  RemoteEdit span AND the pilot's own streamed edits (the same
  `shift_offset` replay as the stream region), so boxes track concurrent
  edits; each carries a stable id (`Note::id`) so Ignore removes exactly
  one and a reply appends to exactly one.
- **Replying and dismissing.** Clicking a box (or `F4`, which hops to the
  next box by row, wrapping) focuses its reply field: typing edits the
  draft, `Enter` sends it as a comment-only reply turn (the note's body,
  the reply, and the numbered buffer — `compose_reply_turn`), `Esc`
  releases the keyboard, and the box keeps the running conversation as
  `you:` lines. `✕ Ignore` (or `Shift+F4`) dismisses one box;
  "Navigator: Clear Comments" drops them all. Boxes persist across turns
  — the old turn-supersession rule is gone, only the driver closes a box.
- **Turn commentary.** The model's non-fence prose accumulates over the
  turn and lands as ONE comment box at the turn's origin (the asked line,
  the yield caret, or the replied note) when the turn ends. Commentary no
  longer streams to a Navigator OUTPUT channel; OUTPUT ("Navigator") now
  carries only diagnostics — prose whose origin file is no longer live
  (rather than losing it), warnings like a dropped inverted fence range,
  host notices (a suppressed edit on a comment-only turn, a file with no
  live owner), and the claude child's stderr chatter — none of which are the
  model's voice, so none of them may land in a comment box (and stderr
  must never be written directly: the seat runs in-process and stderr
  would corrupt the alternate screen).
  The NOTE fence is model-protocol only — nothing new rides the relay, so
  0.1.633/634 peers interop untouched.
- **The navigator's caret (0.1.639).** The seat has a persistent, visible
  caret in the editor, painted in the navigator's identity orange (the
  same accent as its comment boxes) instead of the join-order palette a
  human guest gets. It parks wherever the navigator's attention goes: the
  asked line on `Cmd+K Q`, the answered note's row on a reply, the anchor
  of every comment it lands, and it rides along token by token while an
  edit streams. Parks on a file still bootstrapping resolve as soon as
  the snapshot lands (`PairState::pending_caret`, resolved by the pump;
  a streamed edit or landed note taking over supersedes a stale park);
  rows past the end clamp to the last line. The navigator is identified
  by its site ids (`PairHost::caret_sites`), never by display name —
  names are neither unique nor length-stable — so a human who picks the
  same name keeps the palette. Unseating (deactivate, death, or a
  workspace re-root) removes exactly the seat's carets.
- **Proactive looks (0.1.640).** The navigator re-engages on its own as
  the driver works: when tree-sitter sees a NEW completed construct in the
  active file (an outline symbol for code — a half-typed `fn foo(` parses
  as an error and never fires, and member-level symbols like a struct
  field or enum variant count as edits inside their construct, not
  constructs — or a new heading / purely added top-level paragraph in
  markdown: list bullets and block quotes are not paragraphs, and a
  paragraph split in two is a reshape, `src/pair/proactive.rs`) and
  typing pauses for 2s, the App
  hands it the same comment-only yield turn `Cmd+K Y` would, anchored at
  the new construct. Only files the navigator has already looked at
  re-engage it (re-engagement, not ambush); one buffer state is scanned at
  most once; edits inside existing constructs never fire. Opt out with
  "Navigator: Toggle Proactive Comments" (persisted as
  `disable_proactive_navigator`).
- **Presence.** While seated and idle the status bar wears a quiet
  `◆ <name> seated` badge; the orange streaming badge takes over whenever
  it types. If the claude child dies (or fails to seat), the host surfaces
  it and unseats, and does NOT respawn it every second — the latch clears
  when the record is deactivated OR rewritten, so `croft pair --off` /
  `croft pair`, a palette off/on, or simply re-running `croft pair` after
  fixing the backend all re-activate. A window whose spawn failed also
  releases the single-host lock (and its same-tick owner self-appointment),
  so another croft window in the workspace can host instead; the losing
  window announces the refusal once and keeps polling silently for
  takeover. Teardown on unseat runs on a
  detached thread so the 2s grace-kill never freezes the UI, and the exit
  paths (drop-to-local, self-update exec) reap the child synchronously
  first, then join every teardown a recent unseat detached — a thread that
  died with the process used to leave the claude child running. The claude
  binary resolves to an absolute path when off `PATH`, so
  a stripped GUI-launch environment can still seat it.

### Local models (0.1.636)

The navigator is not claude-only: `--provider ollama` seats the same pilot on
any local Anthropic-compatible endpoint (Ollama, LM Studio, llama.cpp, vLLM):

```sh
croft pair --provider ollama --model qwen3-coder:30b            # localhost:11434
croft pair --base-url http://box:8080 --model qwen3-coder:30b   # implies ollama
```

- **Why a second transport, not a passthrough.** Pointing the claude CLI at a
  local endpoint (`ANTHROPIC_BASE_URL`, or `ollama launch claude`) sends the
  full agent turn: ~107 tool schemas, adaptive thinking, beta extensions —
  ~213 KB before the user says a word. Measured 2026-07: that prefill 500s on
  Ollama 0.18 AND 0.32, at 7B and 30B, and `--allowedTools` does not shrink it
  (it gates execution, not schemas). A minimal `/v1/messages` call — system
  prompt + conversation, no tools — streams a valid fence in seconds. croft's
  protocol is text fences, so local models do not need the agent at all.
- **The seam.** Everything downstream of a text delta is shared: the fence
  machine, the apply path, notes, comment-only yields, cancel/revert. Only
  turn injection differs (`TurnSink`: claude stdin vs an HTTP worker's queue)
  and the event source (`Transport`: the child's stdout reader vs one SSE
  stream per turn, `pair/local.rs`). The endpoint is stateless, so the worker
  owns the conversation as a message list; the model has no tools — the turn
  carries everything.
- **Whole-line fences.** Character columns trip weaker models, so the local
  system prompt steers them to a two-integer header form —
  `<<<EDIT <file>:<start_row>-<end_row>>>>`, rows inclusive — that replaces
  whole lines (a truncated four-int header is rejected, never misread as a
  path). Both forms parse on both backends.
- **Semantics that differ, by design.** Cancel still reverts instantly, but
  the local stream has no interrupt: the in-flight HTTP body drains and
  applies nothing. A dead endpoint fails that turn (status names the URL) and
  the seat stays; the next ask retries. The idle badge names the model:
  `◆ claude (qwen3-coder:30b) seated`.
- **Auth.** Keyed Anthropic-compatible gateways read `ANTHROPIC_AUTH_TOKEN`
  from croft's environment, sent as `Authorization: Bearer` (Anthropic's own
  convention for that variable) and mirrored into `x-api-key` for gateways
  of the other style, alongside `anthropic-version`. The credential only
  ever travels over https or to a loopback host — a cleartext remote
  `--base-url http://box:8080` gets a harmless placeholder instead, so the
  token cannot be sniffed off the wire. `pair.json` records only provider,
  base_url, and model — never a token.

### Slice 3 design: op transport and the process model

The single-broadcast mux cannot give two people different viewports: there is
one PTY and one rendered screen. Independent viewports therefore mean one inner
croft *process* per participant, each rendering its own screen, the processes
sharing edits rather than sharing a terminal. The design:

- **Two modes at attach.** The existing shared mode (everyone on one PTY) stays
  the default. A guest opts into an independent viewport with a flag (e.g.
  `croft <host> --solo` / a participants-menu action); that guest gets their own
  inner croft instead of attaching to the host's PTY. Shared mode is untouched,
  so nothing regresses.

- **A collab socket, separate from the mux socket.** A per-workspace unix socket
  (`<hash>.collab.sock`, sibling to `.mux.sock`) carries only [`Op`]s and
  presence, never PTY bytes. A tiny relay (reuse the `session_host` framing:
  `[type][len][payload]`, a new `FRAME_COLLAB`) fans each participant's ops out
  to the others. No CRDT lives in the relay; it is a dumb multiplexer, exactly
  like the PTY broadcast, because `cola` makes order-independence the client's
  job.

- **Per-file documents, keyed by workspace-relative path.** Each croft holds a
  `CollabDoc` per shared open file. An edit produces an `Op` tagged with the
  file key and the site id; peers integrate it via the slice-2 coordinate bridge
  and repaint. Files not open on a peer are ignored until opened, then
  bootstrapped.

- **Bootstrap.** A joining peer, or a peer opening a file already open
  elsewhere, requests the current document: the owner replies with the canonical
  text plus the encoded replica state (`cola`'s `encode` feature) so subsequent
  ops integrate. Until bootstrap completes the file opens read-only.

- **Save races.** Exactly one participant is the file's *save owner* (the host by
  default); only they write to disk, so the FS watcher and history snapshots see
  one writer, as today. Guests' unsaved edits live in their `CollabDoc` and flow
  to the owner over ops. This sidesteps the disk-writing bypass paths
  (`src/widgets/search.rs`, multi-file replace) for shared files: those stay
  owner-only.

- **What still defers.** Undo remains per-process (each croft's own timeline)
  until a shared-history design lands; LSP stays per-process against each croft's
  own buffer, which is correct since each already syncs full text to its own
  server. These are follow-ups, not blockers for first co-editing.

RED-first as everywhere: the relay and per-file routing are testable headlessly
(two in-process `CollabDoc`s plus a socket pair, asserting convergence through
the relay) before any editor wiring. This is still the largest slice by far and
is the point at which real editor-integration cost lands; it should ship as its
own reviewed PR, not bundled with the foundation.

## Phased PR plan

1. **PR 3a `session-host` mux.** The server and client pump, socket
   protocol, min-winsize policy, server-side read-only, presence sidecar.
   Wire `croft attach` and the remote launch tail to prefer it, dtach
   fallback retained. Fully testable headlessly (unix sockets plus a PTY
   pair; RED first: a test attaching two mock clients asserting broadcast,
   attribution, and min-size).
2. **PR 3b participants UI.** Presence polling in the inner croft, status
   line count, participants popup with shortcuts, request/grant/revoke
   control flow, detach command through the control channel.
3. **PR 3c attributed carets.** Control-FD input path into the inner croft,
   per-guest named colored caret via the existing caret machinery, guest
   edits through `multi_apply`.
4. **PR 3d (unscheduled).** Independent viewports and CRDT replication.
   Design doc first if ever green-lit.

## Risks

- The mux sits on the interactive byte path; it must add no perceptible
  latency (the remote input invariant is zero added milliseconds). It is a
  copy loop between FDs, the same work dtach does, but it needs the same
  care and a latency check before ship.
- Kitty graphics pass through untouched (broadcast is verbatim), but two
  clients on terminals with different graphics support (Ghostty guest,
  iTerm2 host) receive the same bytes; the session renders for the
  protocol croft detected at startup. Documented limitation: mixed-terminal
  sessions render with the owner's protocol.
- Min-winsize means one tiny guest window shrinks everyone. Size and
  control stay orthogonal (observers still need a readable screen), so the
  participants popup names the constraining participant and humans resolve
  it, the same social contract tmux users already live with.
