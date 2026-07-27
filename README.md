<div align="center">
  <img src="assets/logo-tight.png" alt="croft" width="180">
</div>

> [!CAUTION]
> # ⚠️ THIS PROJECT IS MOVING TO A NEW HOME ⚠️
>
> **croft is leaving Codeberg.** Codeberg's Terms of Use now forbid hosting projects that
> "mostly consist of code written by 'generative AI'-tools (including services such as
> *Claude*, *OpenAI Codex*)", on the grounds of "unclear copyright status" and "little
> safeguards to ensure that they do not include harmful code" (§ 7).
>
> croft is written by AI, out in the open, with some human review. A hosting platform deciding
> *how* you are allowed to write your code, rather than *what* the code does, is not a
> community rule. It is a dictatorship dressed up as a vote. The code is public, auditable,
> and tested.
>
> **Watch this space for the new home. Clone the repo now if you want a copy.**

# croft

A VS Code style three pane workspace that runs entirely inside your terminal. Written in Rust and shipped as a single static binary.

## Tenets

The non-negotiables behind every decision in croft:

1. **Speed is a must.** Every feature is weighed against its cost on the hot path before it lands.
2. **Low latency is non-negotiable.** Keystrokes and clicks register instantly; rendering is coalesced so a noisy shell can never starve input.
3. **Local and remote parity always binds.** Behaviour on your Mac and on a Linux box over SSH is identical. There is no second-class remote mode.
4. **The gap between terminal and GUI stays minimal.** croft should look and feel like VS Code, down to the icons and motion.
5. **Everything has a shortcut.** Every action is reachable from the keyboard, and no menu item ships without an accelerator.
6. **Correctness beats workarounds.** Bugs are fixed at the root, never papered over with a fallback or a downgrade.
7. **One binary, no ceremony.** Features are emulated in process rather than bolted on, so there is nothing to wire up after you install.

## Layout

Three panes in the VS Code arrangement: an **Explorer sidebar** on the left, a **code editor** top right, and a **panel** bottom right with PROBLEMS, OUTPUT, TERMINAL, CAPTURES, and PORTS tabs. An activity bar down the far left switches the sidebar between Explorer, Search, Source Control, Remote (SSH), Run and Debug, Extensions, and Testing, and holds the theme picker. Every seam drags to resize, and a **Customize Layout** popup mirrors VS Code's title-bar layout controls.

The essentials are all in: full LSP editing (completion, hover, go-to-definition, rename, quick fixes, inlay hints) with tree-sitter highlighting, multi-cursor, minimap, git gutter, inline blame, and an optional vim mode; a real terminal with shell integration, splits, triggers, copy mode, and durable command history; Source Control with hunk staging and a commit graph; a Test Explorer; a zero-config task runner; and debugging for Python, JavaScript/TypeScript, Rust, C, and C++ over DAP.

See **[LAYOUT.md](docs/LAYOUT.md)** for the full pane-by-pane reference: every editor, terminal, Source Control, Testing, and status-bar feature, the debugging workflow, and language-server setup.

## Requirements

| Requirement | Why |
|-------------|-----|
| macOS, Linux, or Android (Termux) | The PTY layer is POSIX (pseudo-terminal + termios). On Windows, run croft inside WSL2 (it is the Linux build); native PowerShell / conhost is not supported. See [WINDOWS.md](docs/WINDOWS.md). |
| Rust 1.85+ stable | To compile the binary (edition 2024). |
| A Nerd Font as your terminal font | Explorer icons are Private Use Area Nerd Font glyphs (Codicons plus file-type icons). Without one, icons render as `[?]` boxes. |
| A 256 color or truecolor terminal | Terminal.app, iTerm2, Alacritty, kitty, WezTerm, Ghostty all qualify. |
| iTerm2, WezTerm, Ghostty, or kitty (optional) | Inline image / PDF / spreadsheet previews via OSC 1337 or the Kitty graphics protocol. Sixel terminals (detected at startup via a DA1 probe) are also supported. Other terminals fall back to a metadata header line, and croft shows a one-time startup nudge (dismissible, with "don't show again") recommending iTerm2 on macOS or Ghostty on macOS/Linux. |
| `pdftoppm` from poppler-utils (optional) | Multi-page PDF preview. Without it, croft falls back to macOS `sips` for page 1 only. |
| Node.js + npm (optional) | TypeScript / JavaScript LSP. croft auto-installs the `vtsls` server on first use and finds `node` even under nvm / fnm / asdf / volta. |

Per-platform setup (Nerd Font, terminal keybindings, optional dependencies) lives in the platform guides linked under [Platform setup](#platform-setup).

### Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## Install

```bash
cargo install --git https://codeberg.org/vitali87/croft.git
```

This compiles croft from the latest `main` into `~/.cargo/bin/croft`. Re-run to upgrade. To build from source instead:

```bash
git clone https://codeberg.org/vitali87/croft.git
cd croft
cargo build --release && cargo install --path .
```

**macOS:** the build directory churns the Spotlight index and can spike your CPU and fans. Before building, point Cargo at a `.noindex` directory Spotlight ignores; see [MACOS.md](docs/MACOS.md#spotlight-indexing-and-the-build-directory).

## Run

```bash
croft                            # opens the current directory
croft ~/projects                 # opens a specific folder
croft ~/proj --open-file a.rs    # opens a folder with a file already open
croft ~/proj --open-file a.rs --zen  # ...focused on just the file (no sidebar/terminal)
croft remote <host>              # launch croft over SSH on a Linux server (host from ~/.ssh/config)
croft attach                     # open the current folder as a persistent session (survives closing the window)
croft attach ~/projects          # ...for a specific folder
croft attach --solo ~/projects   # join a shared folder in your own viewport (live co-editing)
croft ls                         # list running persistent sessions
croft --help
```

`croft remote <host>` installs itself on the box on first connect with no manual prep, and a stock cloud image works out of the box. See [LINUX.md](docs/LINUX.md#remote-croft-remote-host) for how the cross-compile and host provisioning work.

`croft attach` runs the session under croft's built-in session host, so its terminals, language servers, debugger, and open files keep running after you close the window (or lose the SSH connection). Close the window to detach; run `croft attach` again in the same folder to reattach exactly where you left off, and `croft ls` to see what is still running. This is the same persistence `croft remote` already gives you over SSH, now available locally, with no external dependency. It is also multiplayer: several people (or several of your own windows) can `croft attach` the same folder and share the session live. The first attacher holds write control and later attachers join as read-only observers, enforced by the host, with everyone's window sized to the smallest participant (see [docs/MULTIPLAYER.md](docs/MULTIPLAYER.md)). Inside croft, the status bar shows an "N attached" badge whenever someone else is on, and **Session: Participants** (`Cmd+K A`) lists everyone so you can grant or revoke write control or disconnect a participant. When several people hold control and take turns typing, each keeps their own caret: croft parks the previous typist's cursor, restores the new typist's, and shows everyone else's position as a colored ghost caret in the editor. Sessions started by an older croft under dtach keep reattaching through dtach until they end.

Prefer your own screen instead of the shared one? Add `--solo` (`croft attach --solo <folder>`, or `croft remote <host> <path> --solo` over SSH) to open an **independent viewport** on the same workspace: your own croft process, scrolling and navigating freely, while edits to shared files replicate live between participants and always converge (a CRDT under the hood, no central server). Peers' cursors appear as colored ghost carets wearing their owner's name while they move (VS Code Live Share style), and the session owner is the single writer to disk, so saves, history, and the file watcher see one author (see [docs/MULTIPLAYER.md](docs/MULTIPLAYER.md)).

An AI can take a seat at the same table. Register croft's collab agent as an MCP server, for example with Claude Code:

```sh
claude mcp add croft-collab -- croft collab-agent --workspace /abs/path/to/project
```

The agent joins the running session as a guest with `collab_open` / `collab_read` / `collab_replace` / `collab_caret` / `collab_status` tools: its edits stream into your editor live, its caret shows up named (default `claude`, `--name` overrides), and it can never write your disk — the session owner persists what lands. Croft ships no LLM; any MCP-speaking agent can drive the seat.

For a real pair-programming partner, activate the **resident navigator**: an AI driver/navigator seat that croft itself hosts — no second terminal to babysit:

```sh
croft pair --workspace /abs/path/to/project --model claude-haiku-4-5-20251001 --name navigator
```

The command records the activation and exits; the running croft (or the next one you start there) seats the pilot within a second and wears a `◆ navigator seated` badge. While seated it keeps its own orange caret in the file — parked wherever you last engaged it, at every comment it leaves, and riding its edits as they stream. Then, inside the editor:

- **Ask it** about a line or a selection — right-click the gutter ("Ask Navigator"), right-click a selection ("Ask Navigator About Selection"), or press `Cmd+K Q`. Its edits stream into the buffer **token by token**, the way a human types, with a named caret riding the stream.
- **Yield it the turn** with `Cmd+K Y`: it reviews the active file *comment-only* — everything it says appears as **comment boxes** right in the file, unnumbered blocks between the lines they belong to (never part of the buffer, never saved). Each box carries a reply field and an **Ignore** button: type back and press `Enter` to continue the conversation in place, or dismiss it and keep working. `F4` hops to the next box, `Shift+F4` ignores one. Any edit it attempts on a yielded turn is discarded by the host, not just discouraged.
- **Cancel a stream mid-run**: click the orange `■` stop button in the gutter or press `Cmd+K X` — the streamed text is reverted and the conversation stays alive for your next instruction.
- **It re-engages on its own**: once it has seen a file, finishing a new function, struct, or markdown section and pausing for a moment hands it another comment-only look — a real pair partner glancing over, never able to edit uninvited. Turn it off with "Navigator: Toggle Proactive Comments".

The model's toolbox is read-only (`Read`/`Grep`/`Glob` plus a read-only collab seat); the only way it can change a buffer is the visible, cancellable stream. `croft pair --off` (or the "Navigator: Activate or Deactivate" palette entry) unseats it (see [docs/MULTIPLAYER.md](docs/MULTIPLAYER.md)).

The navigator is not claude-only. Point it at any local Anthropic-compatible endpoint — Ollama, LM Studio, llama.cpp, vLLM — and your own open-weight model takes the seat, same fences, same streaming, same cancel:

```sh
croft pair --provider ollama --model qwen3-coder:30b          # Ollama on localhost:11434
croft pair --base-url http://box:8080 --model qwen3-coder:30b # any compatible server (implies ollama)
```

Local turns go straight to `/v1/messages` with a minimal payload (the claude CLI's ~213 KB tool-schema prefill is exactly what local servers choke on), and the badge names who is typing: `◆ claude (qwen3-coder:30b) seated`.

## Platform setup

croft runs on macOS, Linux, Android, and Windows (via WSL2). Cross-platform basics are above; each platform has a short guide for its Nerd Font, terminal keybindings, and optional dependencies:

* **[macOS](docs/MACOS.md)** — Nerd Font for Terminal.app, and `croft setup-iterm2` / `croft setup-ghostty` to deliver the `Cmd` chords that macOS otherwise reserves for menus.
* **[Linux](docs/LINUX.md)** — `Ctrl` as the command modifier, Nerd Font and poppler-utils, language servers, and the `croft remote <host>` cross-compile / provisioning flow.
* **[Android (Termux)](docs/ANDROID.md)** — `Ctrl` as the command modifier, `pkg`-based dependencies, the auto-installed activity-bar font, and the built-in on-screen keyboard for touch.
* **[Windows (WSL2)](docs/WINDOWS.md)** — run the Linux build inside WSL2, hosted in WezTerm for the full icon/image UI; why native PowerShell / conhost is not supported.

## Keybindings

Every action is reachable from the keyboard; press `F1` inside croft for the full reference. The complete tables (global, Explorer, Search, editor, vim mode, previews, terminal) live in **[KEYBINDINGS.md](docs/KEYBINDINGS.md)**. The command modifier is `Cmd` on macOS and `Ctrl` on Linux / Android; getting `Cmd` chords through your terminal is covered in the [platform guides](#platform-setup).

## Goal

A complete VS Code replacement in the terminal: the full IDE experience as a single fast Rust binary. Everything VS Code does, croft will do, without leaving the TUI.

Maintainers and developers: see [ARCHITECTURE.md](docs/ARCHITECTURE.md) for the project layout and internals, and [CONTRIBUTING.md](CONTRIBUTING.md) for the developer workflow (including keeping the `target/` build directory from filling your disk).

## License

MIT.