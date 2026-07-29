# Collaboration: persistent sessions, live co-editing, and the AI seat

User guide for `croft attach`, `--solo` viewports, the MCP collab agent, and the
resident navigator. For the design and internals behind all of this, see
[MULTIPLAYER.md](MULTIPLAYER.md).

## Persistent sessions (`croft attach`)

```bash
croft attach                     # open the current folder as a persistent session
croft attach ~/projects          # ...for a specific folder
croft ls                         # list running persistent sessions
```

`croft attach` runs the session under croft's built-in session host, so its terminals,
language servers, debugger, and open files keep running after you close the window (or lose
the SSH connection). Close the window to detach; run `croft attach` again in the same folder
to reattach exactly where you left off. This is the same persistence `croft remote` gives you
over SSH, now available locally, with no external dependency.

Sessions started by an older croft under dtach keep reattaching through dtach until they end.

## Shared sessions

Several people (or several of your own windows) can `croft attach` the same folder and share
the session live. The first attacher holds write control and later attachers join as read-only
observers, enforced by the host, with everyone's window sized to the smallest participant.

Inside croft, the status bar shows an "N attached" badge whenever someone else is on, and
**Session: Participants** (`Cmd+K A`) lists everyone so you can grant or revoke write control
or disconnect a participant. When several people hold control and take turns typing, each keeps
their own caret: croft parks the previous typist's cursor, restores the new typist's, and shows
everyone else's position as a colored ghost caret in the editor.

## Independent viewports (`--solo`)

```bash
croft attach --solo ~/projects            # locally
croft remote <host> <path> --solo         # over SSH
```

`--solo` opens an independent viewport on the same workspace: your own croft process,
scrolling and navigating freely, while edits to shared files replicate live between
participants and always converge (a CRDT under the hood, no central server). Peers' cursors
appear as colored ghost carets wearing their owner's name while they move (VS Code Live Share
style), and the session owner is the single writer to disk, so saves, history, and the file
watcher see one author.

## An AI seat via MCP

Register croft's collab agent as an MCP server, for example with Claude Code:

```sh
claude mcp add croft-collab -- croft collab-agent --workspace /abs/path/to/project
```

The agent joins the running session as a guest with `collab_open` / `collab_read` /
`collab_replace` / `collab_caret` / `collab_status` tools: its edits stream into your editor
live, its caret shows up named (default `claude`, `--name` overrides), and it can never write
your disk — the session owner persists what lands. Croft ships no LLM; any MCP-speaking agent
can drive the seat.

## The resident navigator

A driver/navigator pair-programming seat that croft itself hosts, with no second terminal to
babysit:

```sh
croft pair --workspace /abs/path/to/project --model claude-haiku-4-5-20251001 --name navigator
```

The command records the activation and exits; the running croft (or the next one you start
there) seats the pilot within a second and wears a `◆ navigator seated` badge. While seated it
keeps its own orange caret in the file — parked wherever you last engaged it, at every comment
it leaves, and riding its edits as they stream. Then, inside the editor:

- **Ask it** about a line or a selection — right-click the gutter ("Ask Navigator"), right-click
  a selection ("Ask Navigator About Selection"), or press `Cmd+K Q`. Its edits stream into the
  buffer **token by token**, the way a human types, with a named caret riding the stream.
- **Yield it the turn** with `Cmd+K Y`: it reviews the active file *comment-only* — everything
  it says appears as **comment boxes** right in the file, unnumbered blocks between the lines
  they belong to (never part of the buffer, never saved). Each box carries a reply field and an
  **Ignore** button: type back and press `Enter` to continue the conversation in place, or
  dismiss it and keep working. `F4` hops to the next box, `Shift+F4` ignores one. Any edit it
  attempts on a yielded turn is discarded by the host, not just discouraged.
- **Cancel a stream mid-run**: click the orange `■` stop button in the gutter or press
  `Cmd+K X` — the streamed text is reverted and the conversation stays alive for your next
  instruction.
- **It re-engages on its own**: once it has seen a file, finishing a new function, struct, or
  markdown section and pausing for a moment hands it another comment-only look — a real pair
  partner glancing over, never able to edit uninvited. Turn it off with "Navigator: Toggle
  Proactive Comments".

The model's toolbox is read-only (`Read`/`Grep`/`Glob` plus a read-only collab seat); the only
way it can change a buffer is the visible, cancellable stream. `croft pair --off` (or the
"Navigator: Activate or Deactivate" palette entry) unseats it.

### Local models

The navigator is not claude-only. Point it at any local Anthropic-compatible endpoint — Ollama,
LM Studio, llama.cpp, vLLM — and your own open-weight model takes the seat, same fences, same
streaming, same cancel:

```sh
croft pair --provider ollama --model qwen3-coder:30b          # Ollama on localhost:11434
croft pair --base-url http://box:8080 --model qwen3-coder:30b # any compatible server (implies ollama)
```

Local turns go straight to `/v1/messages` with a minimal payload (the claude CLI's ~213 KB
tool-schema prefill is exactly what local servers choke on), and the badge names who is typing:
`◆ claude (qwen3-coder:30b) seated`.
