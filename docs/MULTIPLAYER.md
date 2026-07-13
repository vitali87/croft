# SSH Multiplayer: design

Status: PR 3a (the `session-host` mux, src/session_host.rs) SHIPPED in
0.1.627: server, client pump, frame protocol, min-of-clients winsize,
server-side read-only with grant/revoke, presence sidecar plus frames, exit
code propagation, wired into `croft attach` and the remote launch tail with
a dtach fallback. PR 3b (participants UI in the inner croft) and PR 3c
(attributed carets) are next; the rest of this document is the design they
follow. It exists so the multiplayer pillar starts from croft's real
architecture instead of from a Live Share mental model that does not fit a
single-process TUI.

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

## Phase D, explicitly deferred: independent viewports

True Live Share (per-participant files, scroll, and cursor with a shared
document set) requires one croft process per participant with replicated
buffers, and only here does a CRDT enter (per-buffer sequence CRDT; OT is
rejected outright since croft has no central server authority to transform
against and CRDTs are the settled answer in 2026). It collides with every
single-author assumption catalogued above: snapshot undo, no apply-edit
chokepoint, disk-writing code paths that bypass buffers
(`src/widgets/search.rs:1202`, `src/app/mod.rs:7851`), split views as copies, and
full-text LSP sync. It is weeks-to-months of foundation work and must not
gate Phases A through C, which deliver most of the practical value
(pairing, demos, rescue sessions, code review over SSH) for a fraction of
the cost. Revisit only if shared-viewport multiplayer proves insufficient
in real use.

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
