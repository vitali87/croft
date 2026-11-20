# SSH Multiplayer: design

Two or more people work in the same croft session over SSH: they see the
session, know who else is present, and a guest can be read-only or granted
control with an attributed, colored caret. The local (`croft attach`) and
remote (`croft <host>`) paths behave identically.

For usage, see [COLLABORATION.md](COLLABORATION.md).

## Field test

Verified over real SSH against a Linux host:

- `session-host --probe` (the remote launch-tail guard) exits 0 on Linux.
- Output emitted after two clients attach reaches both, verbatim.
- The presence sidecar shows `control: true` for the first attacher and
  `control: false` for the second.
- The control holder's keystrokes reach both clients; a read-only client's
  reach neither. Enforcement is server-side, not advisory.
- The socket is mode `0600` from creation: bound in a private 0700 staging
  dir and renamed into place (shared with the collab relay). The earlier umask
  save/restore was process-global, so racing binds could corrupt it.
- A per-target lock file, held across probe, stale removal, bind and publish,
  serializes creation. Two attach-or-create racers never both publish; the
  loser attaches to the winner.
- Clients reporting a `0x0` size do not shrink the shared PTY.
- An inner `exit 7` propagates through the mux and back over SSH as exit 7.

## Two invariants of the codebase

- **croft renders exactly one view per process.** One `App`, one terminal
  backend over the process `stdout()`, one blocking event loop, one global
  terminal size. Inline images use absolute cursor addressing every frame.
- There is no per-client state: a second independent viewport needs a second
  backend, event source and geometry.
- **Sessions survive detach and accept multiple clients.** The socket is
  keyed by a deterministic hash of the workspace path, so two people
  attaching the same path share a screen.
- **The editor is single-author.** The buffer is a `Vec<String>`, undo is one
  linear timeline of full-buffer snapshots, and LSP sync is full-text only.
- There is no single apply-edit chokepoint, and a split view is two
  independent copies re-read from disk.

## Shared process means no sync engine

- The shared session needs neither a CRDT nor OT. Those reconcile divergent
  replicas, and here there is one buffer and one PTY input stream.
- The mux serializes everyone's input into a total order before it touches
  the editor. Undo stays one timeline, as in a shared tmux pane.
- Only independent viewports need a sync engine: one croft process per
  participant means replicated buffers.

## Buy vs build

croft builds its own session host. No existing tool offers byte-transparent
multi-attach with server-side read-only, presence and attributed input
(verified externally 2026-07-13):

| Tool | Gap |
|---|---|
| dtach | Byte-transparent, and it broadcasts output and merges input, but no presence, no per-client input attribution, no read-only, and winsize is last-writer-wins. |
| abduco | Adds `-r` read-only attach, but client-side only (per its man page, a convenience and not a security feature); no presence or attribution. |
| tmate / upterm | Built on tmux, which reparses and re-emits the terminal stream and corrupts Kitty graphics. croft chose dtach over tmux for this reason. |
| screen -x | Same gaps as dtach, plus its own escape handling. |

## Architecture: `croft session-host` (the mux)

`croft session-host` is a hidden subcommand in the same binary, with no new
dependency. It takes over dtach's role and its socket path scheme. dtach
remains a fallback for old remote binaries.

```
guest tty ── croft attach ──┐
                            ├── unix socket ── session-host ── PTY master ── croft app
host  tty ── croft attach ──┘        (one accept loop,          (unchanged,
                                      per-client state)          one process)
```

- **Server**: opens a PTY pair, spawns the inner croft on the slave side, and
  listens on `~/.cache/croft/sessions/<hash>.sock`. The UNIX account is the
  trust boundary.
- **Client**: `croft attach <path>` (and the remote `-tt` command) is a thin
  raw-mode pump: stdin to socket, socket to stdout, WINCH to a resize frame.
- **Framing**: a 1-byte type tag and length, then bytes (type `0` = PTY
  bytes, type `1` = control). Control frames are NDJSON:
  `hello {name, cols, rows, want_control}`, `resize`,
  `presence {participants: [...]}`, `grant`/`revoke`, `detach`.
- **Input attribution**: one connection per client, so every input frame has
  a known sender.
- **Window size**: the minimum cols and rows across connected clients (the
  tmux rule). On any attach, detach or resize the server recomputes and sends
  SIGWINCH.
- **Repaint**: croft's resize-wipe plus `mode_reassert_seq()` path, gated on
  `CROFT_SESSION_PERSISTENT`, handles it, which covers a dead mouse after a
  resize.
- **Input policy**: server-side. A read-only client's PTY-byte frames are
  dropped at the server; only control frames pass.
- **Persistence**: clients dying leaves the server and inner croft running.
  `croft ls` checks liveness by connecting to the socket.
- **Detach**: an in-app command sends a `detach` frame; no escape key.
- **Planned**: once the host is stable the dtach dependency can go, including
  `croft_pkg_install dtach` in the remote installer.

### How the inner croft learns about participants

The inner croft only has a PTY and cannot see socket clients.

- The server writes a presence sidecar next to the socket
  (`~/.cache/croft/sessions/<hash>.presence.json`, atomic rename on change).
- The inner croft polls it on its existing tick when
  `CROFT_SESSION_PERSISTENT=1`.
- A file beats a second socket: file-based signaling works across every
  transport croft has and leaves the event loop untouched.
- The host also exports a token-authenticated privileged control channel to
  the inner croft (Inner/Kick verbs) for grant, revoke and disconnect.
  Presence and the participants UI need only the file.

## Transport

- The existing SSH plumbing is reused; nothing is added.
- A remote guest runs `croft <host> <path>`. The launch flow opens its own
  ControlMaster, sees croft installed, and the remote command attaches the
  existing session socket instead of creating one.
- No reverse tunnels, no daemon, no new auth surface: reaching the socket
  requires SSH access to the host account, the same trust boundary as the
  drop relay.
- A local guest attaches the socket directly.
- The session lives where the workspace lives, so no socket crosses machines.
  `ssh -O forward` tunneling exists if one ever must.

## Output delivery: one queue per client

The PTY pump broadcasts to everyone, so it must never wait on anyone.

- PTY output goes to every client verbatim: no reparsing, no re-emission.
- Each client owns a bounded outbox (`CLIENT_QUEUE_LIMIT`) drained by its own
  writer thread. `broadcast` only enqueues.
- A client that stops draining is dropped once its queue passes the limit.
  This happens over SSH: a half-open socket never returns EPIPE.

Reconnects:

- `hello` carries a `client_id`. On attach the host evicts any client already
  registered under that id, and the reconnecting client inherits its control.
  A dropped transport never demotes the owner to read-only.
- The id is `CROFT_RELAY_KEY` and `CROFT_CLIENT_NONCE`, and needs both. The
  relay key is `hash(launch arg)`: constant across reconnects, but identical
  for two terminals opening the same path.
- The nonce is minted once per client process, outside the reconnect loop. It
  is deliberately non-deterministic and nothing recomputes it.
- An empty id (older clients, and local attaches, where a dead unix socket
  reports EPIPE at once) matches nothing and displaces nothing.

## Self-update handover: the host swaps its own image

The launching machine ships a new binary to `~/.cargo/bin/croft` while the
remote session keeps running.

- The inner croft offers `Ctrl+Shift+F9` to re-exec into the new binary.
- The host replaces its own process image. The listening socket, the PTY
  master and the inner child's pid ride through the `exec` by number, so the
  successor adopts the running session.
- Client connections die at the `exec`, so the host broadcasts `HostSwap`
  first. A client seeing it reconnects instead of reading the EOF as the end
  of the session.
- Before broadcasting, the host latches `swapping` under the clients lock, so
  a registration in flight either receives the broadcast or sees the latch.
- A latched host answers any new connection with `HostSwap` and closes it, so
  no client is seated by a host about to vanish. The socket never unbinds, so
  the retry queues in the backlog and the successor takes it.
- If the `exec` fails the latch is released: the host serves on, stale, and
  says so through the `.host-stale` marker.

## Presence and permissions

- **Identity**: `hello` carries a name (default `$USER@hostname`, overridable
  by `--as <name>`). The server assigns a stable color index.
- **Owner**: the first client to create the session is the owner and attaches
  interactive.
- **Guests**: later clients attach read-only by default. The server drops a
  read-only client's PTY-byte frames; only control frames pass.
- **Control**: a read-only guest requests control; the owner sees a toast and
  grants or denies. Grants are revocable.
- **UI**: an "N attached" status badge, join and leave announcements, and
  grant/revoke/disconnect from Session: Participants (`Cmd+K A`). Every menu
  item in the popup has a shortcut.

## Attributed guest carets

- A guest with control drives the shared primary cursor or their own caret,
  rendered in the guest's color. A per-guest caret uses the editor's
  multi-caret machinery (`multi_apply`).
- It is still one buffer in one process, so undo, LSP sync, save and the
  file watcher work unchanged. Undo remains one shared timeline.
- In shared-cursor mode the host sends `Typing` frames on the privileged
  channel whenever the writing client changes, ordered before that client's
  bytes reach the PTY.
- On each switch croft parks the previous typist's caret, restores the new
  typist's, and paints everyone else's as a colored ghost caret.
- Everyone shares one viewport, so follow mode is built in.

## Independent viewports: the collab layer (Phase D)

An independent viewport gives each participant their own files, scroll and
cursor over a shared document set. `croft attach --solo` /
`croft remote <host> <path> --solo` is the opt-in launch; shared mode stays
the default.

Mechanism:

- One croft process per participant, with replicated buffers: a per-buffer
  sequence CRDT (the `cola` crate, wrapped as `CollabDoc`). OT is not used
  because croft has no central server authority to transform against.
- A coordinate bridge (`byte_offset`/`position`) maps the editor's
  `(row, char-column)` to `CollabDoc`'s linear byte offsets.
- A relay (`relay_serve`) on `<hash>.collab.sock`, sibling to `.mux.sock`,
  fans out per-file, per-site ops (`Envelope`) and presence. It reuses the
  `session_host` framing (`[type][len][payload]`, `FRAME_COLLAB`).
- The relay carries no PTY bytes and holds no CRDT: `cola` makes
  order-independence the client's job.
- Documents are per file, keyed by workspace-relative path. A file not open
  on a peer is ignored there until opened, then bootstrapped.
- Files outside the workspace are never shared (`collab::contained_path`). A
  key that is absolute, traversing, or a symlink leaving the workspace is
  declined, from any guest; intra-workspace symlinks keep working.
- Each tick, croft diffs local changes against the replica and sends them as
  ops. Diffing needs no single edit chokepoint: multi-cursor, paste, undo and
  reloads all reduce to text-state transitions.
- The diff (`similar::TextDiff`) has a 200ms timeout; a timed-out diff is
  coarser but still valid. Divergence self-heals at the next resync.
- Local edits are extracted before inbound ops are applied. The reverse order
  would rebroadcast a just-applied remote edit.
- Applying a remote edit re-pins the synced state at once, so it is not
  echoed back.
- Out-of-order ops wait in a causal backlog and are never dropped.
- Carets are broadcast on actual moves only (`CollabMsg::Caret`). Peers paint
  each as a ghost caret in the participant's color.
- The relay's forwarding writes are bounded (`SO_SNDTIMEO`, 2s) and a wedged
  peer is dropped; `write_frame_blocking` gives up after 5s. One stalled
  participant cannot freeze the others.

Split panes:

- Panes of a live file mirror the session, and remote edits land on every
  attached pane.
- A pane created after the file went live (a split, a close-and-reopen) holds
  stale disk text. It never sends edits and refuses input until seeded from
  the replica next tick.
- Only text tabs sync, every matching one. Diff, image and sheet tabs carry
  the path but are skipped.
- A tab drops its attachment when its path changes and keeps it on a
  same-path reload.
- While the link is down the active pane mirrors onto its siblings each tick,
  so the reconnect replays exactly one offline text per file.

### Opening a file

- A guest opening a workspace file broadcasts `SnapshotRequest{file, nonce}`.
  Only the owner answers, with the canonical text, the cola-encoded replica
  and a site id.
- The owner is site 1 and the single allocator, so ids never collide. The
  counter is seeded from the owner's pid, so a restarted owner cannot
  re-issue an id a surviving guest holds.
- Nonces are seeded from the process id, so two guests never adopt each
  other's reply.
- The owner opens the file as a background tab, so guest edits land in a real
  buffer and saves, auto-save, history snapshots and reload suppression
  apply.
- The file refuses input until the snapshot lands. Ops arriving meanwhile are
  buffered and replayed; duplicates integrate as no-ops.
- The relay has no replay, so an unanswered request is re-sent, starting at
  500ms and doubling, capped at 1s (0.5s, 1.5s, 2.5s). The cap keeps a late
  owner covered inside the deadline; the doubling limits duplicate snapshots.
- With no owner on the relay the bootstrap times out after 3s and the file
  degrades to plain local editing, so a lone solo guest is not read-only
  forever.
- That give-up is a latch (`DocState::LocalOnly`). A local-only file is
  exempt from the guest save gate, and only the collab agent's `collab_open`
  rejoins past the latch.

### Saving: single writer

The session owner (the host) is the single writer to disk for all files, so
the file watcher and history snapshots see one author.

- Guests' unsaved edits live in their replica and flow to the owner as ops.
- Guest `Cmd+S` and auto-save are no-ops with a status hint for workspace
  files.
- Guest search Replace All is blocked. The owner's Replace All converges
  through its own reload-diff.
- Guest LSP-rename skips closed-file disk writes; open tabs flow as ops.
- Guests do not reload workspace files from disk. The owner's reload (a git
  checkout, say) replicates as an edit.

### Undo

- Undo is per process and holds only that participant's own steps: Ctrl+Z
  takes back your last change and keeps your partner's. Peers see it as an
  ordinary edit.
- A peer's edit opens no undo step.
- An AI stream is one step from start to finish, so one undo rejects a
  navigator edit. A cancelled stream leaves none.

### Losing the link or the owner

- **Link loss**: EOF, a hard error or a failed send on the relay channel
  marks it dead. croft drops the session with a status hint, reconnects with
  a 2s backoff, and guest files re-bootstrap.
- **No session**: the guest save gate stands down, so offline work is not
  stranded in RAM.
- **Offline edits**: they exist nowhere else, so the re-bootstrap keeps them.
  It is a line-level three-way merge with the owner's snapshot, and the
  guest's text wins a same-line conflict.
- **Owner leaves**: the relay outlives the owner, so the owner heartbeats
  every second and says goodbye when it drops. After the goodbye, or 15
  seconds of silence, a guest's files turn local-only: "The session owner
  left; saves now write to disk".
- **Owner returns**: when a heartbeat is heard again those files re-bootstrap,
  merging against the file on disk, so the guest's work reaches the new
  owner.
- A guest that never heard a heartbeat (an older owner) never decides the
  owner left.

## Named carets and the AI seat

- While a peer's caret moves or types, its name paints on the row above it
  (below on the viewport's top row) and disappears 2s after the caret rests.
- A parked caret's re-broadcast does not refresh its tag; only real moves
  do.
- Names are truncated to 24 chars at both send and ingest, so a hostile peer
  name cannot paint a whole row.
- The name rides the wire with the caret: site ids are allocated per file
  bootstrap, so carets cannot be joined across files by site id.
- The name field is optional on the wire. A peer that omits it renders with
  an empty name.
- The local name defaults to `user@host`, overridable via
  `CROFT_COLLAB_NAME`.

### `croft collab-agent`

`croft collab-agent` is a hidden headless subcommand. It joins the
workspace's relay as a regular guest and exposes the session as an MCP server
on stdio (JSON-RPC 2.0 over NDJSON). Register it once and any MCP-speaking
agent gets a seat:

    claude mcp add croft-collab -- croft collab-agent --workspace <abs path>

| Tool | Does |
|---|---|
| `collab_open` | Bootstraps a file and returns its text. Errors after ~4s when no croft owner serves the workspace. |
| `collab_read` | Reads the file. |
| `collab_replace` | Whole-text replace; the diff engine turns it into minimal convergent ops. |
| `collab_caret` | Moves the named caret (0-based row/col). |
| `collab_status` | Lists tracked files plus peers' last-seen carets. |

- A client on the 2026-07-28 revision probes with `server/discover` and sends
  its version in every request's `_meta`. Other clients get the `initialize`
  handshake.
- A background thread pumps the session every 30ms, so the replica stays
  fresh between tool calls.
- The agent has every guest property: per-file site ids, CRDT convergence and
  owner-only disk writes. It cannot touch files; the owner persists what it
  sees.
- Trust boundary: the 0600 relay socket under the same UNIX account.
- Default caret name `claude`; `--name` overrides. EOF on stdin ends the
  seat.
- croft contains no LLM code.

## croft pair: a real-time AI collaborator

An edit through the MCP seat lands as one bulk insert, because MCP tool
arguments arrive whole. `croft pair` owns the token stream itself.

- It spawns the `claude` CLI as a persistent stream-json conversation on
  stdio and reads token-level `text_delta` events.
- Flags: `--input-format stream-json`, `--output-format stream-json`,
  `--include-partial-messages`.

`--append-system-prompt` teaches the model a fenced edit protocol in its
ordinary streamed text:

    <<<EDIT <file>:<start_row>:<start_col>-<end_row>:<end_col>>>>
    <replacement text>
    <<<END>>>

- Coordinates are 0-based character positions against the file's current
  text. A column is never bytes.
- The stream is parsed incrementally and is exact about deltas split
  anywhere, even mid-marker.
- The fence's range is deleted, then each token is inserted through a regular
  collab guest seat, with the named caret riding the stream.
- Text outside a well-formed fence, or under a malformed header, is
  commentary and never applied, so bad model output cannot corrupt a buffer.
- Concurrent human edits are safe. The stream's position moves with each
  one: an edit before it shifts it by the size delta, a straddling edit
  clamps it to the edit's new end, and an edit after it changes nothing.

### Cancel

- An active stream shows an orange status badge and an orange `■` stop
  button in the gutter under the caret.
- Any participant cancels by clicking it, with `Cmd+K X`, or with the
  palette's "Collab: Cancel AI Stream".
- Two relay messages carry this. The pilot broadcasts
  `CollabMsg::StreamState { site, name, file, active }` at stream
  start/end/cancel; a cancel is `CollabMsg::StreamCancel {}`. Peers that do
  not know them drop them.
- The pilot sends a `control_request` interrupt on claude's stdin
  (feature-detected; the fallback drops the rest of the turn's deltas). The
  child is never killed, so the conversation survives.
- The streamed region is reverted, and a note prepended to the next user turn
  tells the model its edit was rejected.
- An unterminated fence at end of turn reverts the same way.

### Sandbox

- The model's only built-in tools are `Read`/`Grep`/`Glob` (`--tools`), plus
  a read-only `collab-agent` MCP seat named `<name>-reader` for live buffer
  queries (`--strict-mcp-config`). The fence is the only write path.
- The tools carry no allow rule, so `dontAsk` runs them inside the workspace
  and denies a read outside it (`~/.ssh`, `~/.aws`).
- Only the user's own Claude Code settings load (`--setting-sources user`). A
  workspace's `.claude/settings.json`, and the hooks in it, never load, so
  starting pair in a cloned repository runs none of that repository's
  commands.
- This needs a Claude Code that accepts `--tools` and `--setting-sources`;
  2.1.310 is the version these flags were checked against.
- End-to-end tests drive the pilot against a scripted fake claude (python3):
  streaming convergence, the StreamState lifecycle and the cancel drill.

## The resident navigator

The resident navigator is the `croft pair` seat hosted inside croft:
turn-based driver/navigator pairing.

### Activation

- `croft pair [--workspace <p>] [--model <m>] [--name <n>]` writes a
  `<hash>.pair.json` sidecar beside the workspace's collab socket, ensures
  the relay, and exits.
- A running croft checks the record on a 1s cadence and seats the navigator
  in-process.
- Instructions come in-editor (`Cmd+K Q`). A start task is not persisted: it
  would re-fire on every launch, and an `@file` task would freeze the tick
  thread on seat.
- The hidden `--repl` flag takes a one-shot task for the terminal driver. A
  task without `--repl` is a usage error.
- `croft pair --off` or the "Navigator: Activate or Deactivate" palette entry
  unseats it.
- The seat needs a collab owner to serve files, so a plain `croft` launch
  appoints itself owner when it seats the navigator. Solo guests never host.
- Exactly one croft hosts per workspace, guarded by an advisory `flock` on
  `<hash>.pair-host.lock`. Two owners would both claim site 1 and corrupt the
  buffer, so a second window is refused ("hosted by another croft window").
- A lock file that cannot be opened at all (a missing or read-only config
  directory, an fd limit) is reported with the path and the OS error, never
  as contention.
- The OS drops the lock when the holder exits, so a crashed host hands off
  automatically.

### Turns

Only one turn runs at a time. An ask or yield fired mid-stream is refused
("mid-turn; wait for it to finish").

| Turn | Trigger | May edit |
|---|---|---|
| Ask | Right-click the gutter ("Ask Navigator"), a selection ("Ask Navigator About Selection"), or `Cmd+K Q` | Yes |
| Yield | `Cmd+K Y` on the active file | No. The host discards any edit; the prompt rule alone is advisory. |
| Reply | `Enter` in a comment box's reply field | Yes |

- An ask opens an input box for the instruction. The turn carries the 0-based
  line/range with the 1-based gutter number beside it, the selected text, and
  the buffer numbered `N|` per line (a label, not content).
- Fence headers and `N|` are 0-based. Line numbers the model writes in note
  bodies and commentary are the gutter's 1-based ones.
- A yield carries the unified diff since the navigator's last look plus the
  numbered buffer.
- A reply carries the note's body, the reply and the numbered buffer. It is
  the driver's own turn, not a yield.

### Comment boxes

The model anchors a remark to a line with a NOTE fence
(`<<<NOTE <file>:<row>>>>` … `<<<END>>>`, 0-based row).

- Each note renders as a comment box: an unnumbered block below its anchor
  line, never part of the buffer and never saved. Boxes follow concurrent
  edits and persist across turns.
- A box shows the author, the body, a reply field and a `✕ Ignore` button.
  It keeps the running conversation as `you:` lines.
- Each note has a stable id, so Ignore removes exactly one and a reply
  appends to exactly one. Only the driver closes a box.
- Clicking a box or `F4` (next box, wrapping) focuses its reply field.
  `Enter` sends the reply; `Esc` releases the keyboard.
- `✕ Ignore` (or `Shift+F4`) dismisses one box. "Navigator: Clear Comments"
  drops them all.
- Prose outside fences lands as one comment box at the turn's origin (the
  asked line, the yield caret, or the replied note) when the turn ends.
- The OUTPUT channel ("Navigator") carries only diagnostics: prose whose
  origin file is not live, warnings like a dropped inverted fence range, host
  notices and the claude child's stderr.
- Host notices cover a suppressed edit on a comment-only turn and a file with
  no live owner.
- The seat runs in-process, so stderr is never written directly: it would
  corrupt the alternate screen.
- The NOTE fence is model-protocol only; nothing new rides the relay.

### Caret and status

- The navigator's caret is orange. It is identified by its site ids, never by
  display name, so a human who picks the same name keeps a palette color.
- It parks on the asked line, the answered note's row and every comment's
  anchor, and rides an edit as it streams.
- A park on a file still bootstrapping resolves when the snapshot lands. Rows
  past the end clamp to the last line.
- Unseating (deactivate, death, or a workspace re-root) removes exactly the
  seat's carets.
- Seated and idle, the status bar shows a `◆ <name> seated` badge. The orange
  streaming badge takes over while it types.
- If the claude child dies or fails to seat, croft reports it and unseats
  without respawning every second.
- The failure latch clears when the record is deactivated or rewritten:
  `croft pair --off` / `croft pair`, a palette off/on, or re-running
  `croft pair` all re-activate.
- A window whose spawn failed releases the host lock, so another window can
  host. The losing window announces the refusal once per reason and keeps
  polling silently for takeover.
- Teardown on unseat runs on a detached thread, so the 2s grace-kill never
  freezes the UI. The exit paths (drop-to-local, self-update exec) reap the
  child synchronously first.
- The claude binary resolves to an absolute path when off `PATH`.

### Proactive looks

The navigator re-engages on its own as the driver works.

- When tree-sitter sees a new completed construct in the active file and
  typing pauses
  for 2s, the navigator gets the same comment-only yield turn `Cmd+K Y`
  gives, anchored at the construct.
- In code a construct is an outline symbol. A half-typed `fn foo(` never
  fires; a struct field or enum variant is an edit inside its construct.
- In markdown a construct is a new heading or a purely added top-level
  paragraph. List bullets and block quotes are not paragraphs, and a
  paragraph split in two is a reshape.
- Only files the navigator has already looked at re-engage it. One buffer
  state is scanned at most once, and edits inside existing constructs never
  fire.
- Opt out with "Navigator: Toggle Proactive Comments" (persisted as
  `disable_proactive_navigator`).

### Local models

`--provider ollama` seats the same navigator on any local Anthropic-compatible
endpoint (Ollama, LM Studio, llama.cpp, vLLM):

```sh
croft pair --provider ollama --model qwen3-coder:30b            # localhost:11434
croft pair --base-url http://box:8080 --model qwen3-coder:30b   # implies ollama
```

- **Request**: a minimal `/v1/messages` call: system prompt plus
  conversation, no tools. It streams a valid fence in seconds.
- **Why not a passthrough**: the claude CLI pointed at a local endpoint
  (`ANTHROPIC_BASE_URL`, or `ollama launch claude`) sends the full agent
  turn: ~107 tool schemas, adaptive thinking, beta extensions, ~213 KB.
- **Measured 2026-07**: that prefill 500s on Ollama 0.18 and 0.32, at 7B and
  30B. `--allowedTools` does not shrink it (it gates execution, not schemas).
- **Shared path**: everything downstream of a text delta is shared: the fence
  machine, the apply path, notes, comment-only yields, cancel/revert.
- **Conversation**: the endpoint is stateless, so croft owns the conversation
  as a message list, with one SSE stream per turn.
- **Whole-line fences**: character columns trip weaker models, so they are
  steered to `<<<EDIT <file>:<start_row>-<end_row>>>>`, rows inclusive,
  replacing whole lines. Both forms parse on both backends. A truncated
  four-int header is rejected, never misread as a path.
- **Cancel**: reverts instantly, but there is no interrupt. The in-flight
  response drains and applies nothing.
- **Dead endpoint**: that turn fails (the status names the URL) and the seat
  stays; the next ask retries.
- **Badge**: `◆ claude (qwen3-coder:30b) seated`.
- **Auth**: keyed gateways read `ANTHROPIC_AUTH_TOKEN` from croft's
  environment. It is sent as `Authorization: Bearer` (Anthropic's convention
  for that variable) and mirrored into `x-api-key` for gateways of the other
  style, alongside `anthropic-version`.
- **Token safety**: the token only travels over https or to a loopback host.
  A cleartext remote `--base-url http://box:8080` gets a placeholder.
  `pair.json` records only provider, base_url and model, never a token.

## Recording a session (`croft record`)

`croft record [path] [--out file.cast]` writes a running persistent session
to an asciicast v2 file that `asciinema play` and `agg` read.

- The recorder is an ordinary client that says `Hello` with a **0x0** size,
  which makes it a pure observer. It never shrinks the shared PTY, its input
  is dropped, and it is never auto-granted control, even when it attaches
  first; the next real client still gets control.
- It records the full-colour byte stream every participant is painted with.
- The size comes from the roster: the recorder recomputes the host's minimum
  over the other participants and writes an `r` event when it changes. The
  cast header waits for at least one other participant.
- Lines are flushed one at a time, so the cast is valid whenever recording
  ends.
- Ctrl-C and `croft record --stop` (SIGINT, SIGTERM) set a flag the read loop
  checks, so the recorder removes its pidfile and exits 0.
- The pidfile beside the socket holds the recorder's pid, start time and cast
  path. A recorder killed outright leaves it behind.
- A second `croft record` for the same session is refused while the first
  runs: it would take over the pidfile, leaving the first unstoppable.
- A pid can be recycled, so `--stop` signals the recorded pid only while it
  is croft running `record`, started at the recorded time, then waits for it
  to exit. Otherwise it removes the pidfile and reports that no recording is
  running.

## Sticky notes

- `Cmd+K N` leaves a note on a line: overlay state, never buffer content.
- In a `--solo` session the note replicates over the collab relay as
  `CollabMsg::Note`, and a joining guest asks the owner for every note
  (`NotesRequest`).
- Every peer merges copies of a note the same way: fields from the higher
  `(rev, content)` copy, replies from both copies, and a deletion that no
  older copy can undo.
- So peers converge, and a concurrent reply is never lost.
- A note keeps its line's text and finds it again nearby, so it follows the
  code without tracking edits.
- The owner (or a lone croft) keeps the notes in `~/.cache/croft/notes/`, so
  they survive disconnects and restarts.

## Risks

- **Latency**: the mux sits on the interactive byte path and must add no
  perceptible latency (the remote input invariant is zero added
  milliseconds). It is a copy loop between FDs and needs a latency check
  before ship.
- **Mixed terminals**: Kitty graphics pass through untouched, but all clients
  receive the same bytes (a Ghostty guest and an iTerm2 host, say). The
  session renders for the protocol croft detected at startup, the owner's.
- **Window size**: one small guest window shrinks everyone. Size and control
  stay orthogonal (observers still need a readable screen), so the
  participants popup names the constraining participant and people resolve
  it.

### Limitations

- Independent viewports have no shared LSP (each croft syncs to its own
  servers) and no per-file save handoff.
- There is no participants-menu action to open a solo viewport: a viewport
  is a process on the guest's machine, which the host cannot start. Use the
  `--solo` flag on the guest's own launch command.
- A human edit strictly inside a streamed region is discarded if the stream
  is later cancelled, because the revert restores the original slice.
- A second croft window on a workspace whose navigator is hosted elsewhere
  does not see the owner's edits. Use `croft attach` to share.
