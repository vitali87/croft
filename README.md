<div align="center">
  <img src="assets/logo-tight.png" alt="croft" width="180">
</div>

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
| macOS, Linux, or Android (Termux) | The PTY layer is POSIX. On Windows, run the Linux build inside WSL2; see [WINDOWS.md](docs/WINDOWS.md). |
| Rust 1.85+ stable | To compile the binary (edition 2024). |
| A Nerd Font as your terminal font | File and activity-bar icons are Nerd Font glyphs. Without one they render as `[?]` boxes. |
| A 256 color or truecolor terminal | Terminal.app, iTerm2, Alacritty, kitty, WezTerm, Ghostty all qualify. |
| iTerm2, WezTerm, Ghostty, kitty, or a sixel terminal (optional) | Inline image / PDF / spreadsheet previews. Elsewhere croft shows a metadata header line instead. |
| `pdftoppm` from poppler-utils (optional) | Multi-page PDF preview with clickable links. Without it, page 1 only on macOS via `sips`. |
| Node.js + npm (optional) | TypeScript / JavaScript LSP; croft auto-installs the `vtsls` server on first use. |

Per-platform setup (Nerd Font, terminal keybindings, optional dependencies) lives in the platform guides linked under [Platform setup](#platform-setup).

### Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then open a new terminal — or run `. "$HOME/.cargo/env"` in the current one — so `cargo` is on `PATH`.

## Install

```bash
cargo install croft-software --locked
```

This compiles the latest [crates.io](https://crates.io/crates/croft-software) release into `~/.cargo/bin/croft`. Re-run to upgrade. To track the latest `main` instead:

```bash
cargo install --git https://github.com/vitali87/croft.git --locked
```

Or to build from a source checkout:

```bash
git clone https://github.com/vitali87/croft.git
cd croft
cargo build --release && cargo install --path . --locked
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
croft view report.pdf            # from any pane: open a file in the croft you are sitting in
cat data.csv | croft view -      # ...or pipe it in
croft theme-import theme.json    # use a VS Code colour theme in croft
croft theme-import dracula-theme.theme-dracula   # ...or fetch one from the marketplace
croft --help
```

`croft remote <host>` installs itself on the box on first connect with no manual prep, and a stock cloud image works out of the box. See [LINUX.md](docs/LINUX.md#remote-croft-remote-host) for how the cross-compile and host provisioning work.

## Collaboration

`croft attach` keeps a session alive after you close the window, and lets other people join it.
Add `--solo` and each participant gets an independent viewport on the same files, live. An AI
can take a seat too, either as an MCP guest or as a resident pair-programming navigator that
croft itself hosts (`croft pair`), including local open-weight models.

See **[COLLABORATION.md](docs/COLLABORATION.md)** for the full guide.

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