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

Three panes in the VS Code arrangement: an **Explorer sidebar** on the left (git-ignored files and folders grey out, as in VS Code), a **code editor** top right, and a **bottom panel group** bottom right. The panel carries a tab strip: **PROBLEMS** (aggregated workspace diagnostics grouped by file, with a count badge; click a diagnostic to jump the editor to it), **OUTPUT** (a read-only log viewer with a channel dropdown, one channel per language server plus Debug Adapter, Git, and Server Provisioning), **TERMINAL** (a real shell, splittable into panes; drag a pane's name pill to reorder panes, and a `⛶` button or `Cmd`/`Ctrl`+`K` `M` maximizes one pane across the panel while the rest wait in a right-edge rail you can shuffle through or drag into a new order), and **PORTS** (loopback ports croft detects from terminal output and a periodic socket poll; on a remote session a detected port forwards home over the live SSH connection and opens in your local browser, and Cmd/Ctrl+click on a printed `http://localhost:PORT` does the same inline). An activity bar down the far left switches the sidebar between Explorer, Search, Source Control, a Remote (SSH) explorer, Run and Debug, Extensions, and Testing, and holds the theme picker. The Source Control icon carries a count badge over its corner whenever the working tree has changes, the Explorer icon carries one for the number of unsaved open files, the Remote icon carries one for the number of ports forwarded home over SSH, and the Testing icon carries a red one for the number of failing tests. Every seam drags to resize.

A **Customize Layout** popup (the `⛶` icon at the top-right of the editor, or the settings gear) mirrors VS Code's title-bar layout controls: show/hide the activity bar, primary and secondary side bars, panel, and status bar; dock the side bar left or right; align the panel; move the quick input; or enter Zen Mode (`Cmd`/`Ctrl`+`K` `Z`).

The editor brings tree-sitter highlighting, inline previews for images / PDFs / spreadsheets, side-by-side splits, a minimap (a rasterized file preview in a right-edge strip; click or drag to jump, right-click to toggle or re-side it), a breadcrumbs bar (the file path plus the enclosing symbol trail at the caret, click to jump), sticky scroll (the enclosing scope headers pinned to the top while you scroll), bracket-match highlighting, multi-cursor editing (`Cmd`/`Ctrl`+`D` for the next match, `Option`+click for a caret, `Shift`+`Option`+drag for a column selection), a git gutter (thin coloured bars marking lines added, modified, or deleted against HEAD), code folding (collapse any indented block to its header line from a gutter chevron or `Cmd`/`Ctrl`+`K` chords), format on save (toggle with `Cmd`/`Ctrl`+`K` `F`), an optional vim mode, and full LSP (completion, signature help / parameter hints, hover, go-to-definition, rename, formatting, quick fixes / code actions, diagnostics). A Command Palette (`Cmd`/`Ctrl`+`Shift`+`P`) runs every command, Go to Symbol (`Cmd`/`Ctrl`+`Shift`+`O`) fuzzy-jumps within a file (or to a line with a `:` prefix), and you can debug Python, JavaScript/TypeScript, Rust, C, and C++ with breakpoints over DAP or attach a `pdb` REPL to a live process.

The status bar mirrors VS Code's: branch and ahead/behind, error/warning counts (click to open Problems) and the active language server on the left; cursor position, indentation, encoding, line ending, and language mode on the right. Click the encoding, line ending, or language segment to change it (reopen with a different encoding, switch LF/CRLF, or override the language mode).

See **[LAYOUT.md](docs/LAYOUT.md)** for the full pane-by-pane reference, debugging workflow, and language-server setup.

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
croft --help
```

`croft remote <host>` installs itself on the box on first connect with no manual prep, and a stock cloud image works out of the box. See [LINUX.md](docs/LINUX.md#remote-croft-remote-host) for how the cross-compile and host provisioning work.

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
