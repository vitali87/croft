<div align="center">
  <img src="assets/logo-tight.png" alt="croft" width="180">
</div>

# croft

A VS Code style three pane workspace that runs entirely inside your terminal. Written in Rust for performance and ships as a single static binary.

* **Left pane:** file explorer with VS Code style file type icons (Codicons / Devicons / Seti), `.gitignore` aware
* **Top right pane:** code editor with `tree-sitter` syntax highlighting and edit / save
* **Bottom right pane:** a real interactive shell, your `$SHELL` running on a real PTY

Built on [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm), with [portable-pty](https://docs.rs/portable-pty/) for the embedded shell, [vt100](https://docs.rs/vt100/) for terminal-state parsing, and [tree-sitter](https://tree-sitter.github.io/tree-sitter/) for incremental, AST-based syntax highlighting.

> A previous Python prototype (Textual + pyte) lives on the `python-archive` branch. The Rust rewrite is the canonical implementation.

## Requirements

| Requirement | Why |
|-------------|-----|
| macOS or Linux | The PTY layer uses POSIX `forkpty`. Windows is not yet supported. |
| Rust 1.78+ stable | To compile the binary. |
| A Nerd Font as your terminal font | The file explorer icons are Private Use Area glyphs (Codicons, Devicons, Seti). Without a Nerd Font, icons render as `[?]` boxes. |
| A 256 color or truecolor terminal | macOS Terminal.app, iTerm2, Alacritty, kitty, WezTerm, Ghostty all qualify. |

### Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Install a Nerd Font (macOS)

```bash
brew install --cask font-meslo-lg-nerd-font
```

Then set Terminal.app's default profile font to it. The fastest way is the bundled command (after building, see below):

```bash
./target/release/croft setup-terminal
```

This sets the default profile font to PostScript name `MesloLGSNFM-Regular` at 13pt via AppleScript. Existing custom profiles are not modified. Quit Terminal.app entirely (cmd+Q) and reopen for the change to take effect.

If you prefer to do it by hand: Terminal.app → Settings → Profiles → your default profile → Text → Font → Change → MesloLGS Nerd Font Mono Regular 13pt.

**Why MesloLGS NF specifically:** macOS Terminal.app does not perform CoreText style font fallback for Private Use Area glyphs the way iTerm2 does. The Nerd Font glyphs only render if the *primary* terminal font has them. MesloLGS NF ships every Codicon, Devicon, and Seti glyph the explorer uses.

> **PostScript name vs display name.** AppleScript needs the *PostScript* name, which is what's embedded in the .ttf, not what Terminal.app's font picker displays. The PostScript name for `MesloLGSNerdFontMono-Regular.ttf` is `MesloLGSNFM-Regular`. This was a real bug in the Python prototype: the wrong name silently no-ops in AppleScript and Terminal.app keeps the previous font, which is why the icons appeared broken even after running the setup command.

## Build and install

```bash
git clone https://bitbucket.org/vitali_avagyan/croft.git
cd croft
cargo build --release
# optional, install into ~/.cargo/bin
cargo install --path .
```

## Run

```bash
croft                # opens the current directory
croft ~/projects     # opens a specific folder
croft --help
croft setup-terminal --help
```

## Keybindings

| Keys | Action |
|------|--------|
| `↑`/`↓` in tree | Move selection |
| `Enter` or `→` on a file | Open in editor; on a folder: expand or collapse |
| `←` on a folder | Collapse |
| `Ctrl+s` | Save the open file |
| `Ctrl+q` | Quit |
| `F6` | Cycle focus across panes (tree → editor → terminal → tree) |
| `Ctrl+b` | Toggle the file tree / side panel |
| `Ctrl+j` | Toggle the terminal pane |
| `Ctrl+Shift+f` (or `Cmd+Shift+f` with the iTerm2 setup below) | Jump to the Search sidebar view |
| Click activity-bar icons (left edge) | Switch between Explorer (file icon) and Search (magnifying glass) views |
| In Search view: type | Live `.gitignore`-aware search across the workspace; results refresh per keystroke (~120 ms debounce, runs off the UI thread). Capped at 200 hits. |
| In Search view: click `Aa`, `ab`, `.*` toggles | Flip case-sensitive / whole-word / regex modes; the search re-runs immediately. Active toggles render with a yellow background. |
| In Search view: ↑/↓ + Enter, or click a result | Open the file at the matched line |
| Mouse drag in terminal pane | Select text; selection stays highlighted until you copy or click elsewhere — no auto-copy |
| Mouse wheel in terminal pane | Scroll through 5000 rows of scrollback. While vim / less / htop is in alternate-screen mode, wheel forwards arrow keys instead so the running app handles it. Any keystroke snaps back to the live bottom. |
| `Ctrl+Shift+c` (or `Cmd+c` with kitty-protocol terminals) | Explicit copy of the terminal's current selection |
| Editor: arrows, Home, End | Navigate (clears any active selection) |
| Editor: `Shift`+arrows / `Shift`+Home / End / PageUp / PageDown | Extend the selection by the same motion |
| Editor: PageUp / PageDown (`fn+↑` / `fn+↓` on Mac) | Scroll exactly one viewport; the line just past the previous bottom becomes the new top |
| Editor: any printable char, Enter, Backspace, Delete, Tab | Edit (typing or deleting with an active selection replaces it) |
| Editor: mouse drag | Select text; selection stays highlighted until you copy or click elsewhere — no auto-copy |
| Editor: `Ctrl+C` / `Cmd+C` | Copy the current selection to the system clipboard via OSC 52 |
| Editor: `Ctrl+X` / `Cmd+X` | Cut the current selection |
| Editor: `Ctrl+V` / `Cmd+V` (host-terminal paste) | Paste system-clipboard contents at the cursor; replaces selection if any |
| Editor: `Ctrl+Z` / `Cmd+Z` | Undo the last edit (typing bursts coalesce into one step; backspace, paste, cut, replace are each their own step) |
| Editor: `Ctrl+A` / `Cmd+A` | Select the entire buffer |
| Editor: `Esc` | Clear the current selection |
| Mouse click in any pane | Focus and (in tree) select / open, (in editor) move cursor |
| Mouse right-click in tree | Open context menu (New File…, New Folder…, Delete) |
| `Delete` / `Backspace` (or `Cmd+Backspace`) in tree | Move the selected file or folder to the OS Trash, no confirmation. Mac keyboards label the Backspace key as "delete," so the obvious key works regardless of layout. |
| Mouse wheel | Scroll the pane under the pointer |
| Up/Down/Enter in context menu | Navigate / pick item; Esc dismisses |
| Type + Enter in create prompt | Create the file or folder; Esc cancels |
| Terminal pane: any key | Forwarded to the shell PTY (arrows, Ctrl+letter, Alt+x, function keys all translated to the proper VT escape sequences) |

## iTerm2 setup for macOS users

Two iTerm2 toggles make croft feel native on macOS. Both are opt-in because they affect every terminal session, not just croft.

### 1. Right-click reaches croft

By default iTerm2 shows its own context menu (Copy / Open URL / etc.) on right-click and never forwards it to the running app, so croft's New File / New Folder menu would never trigger.

iTerm2 → Settings (`⌘,`) → search **"right click"** → tick **"Right click reported to apps, does not open menu"**. After that, right-clicking inside croft's tree pane opens croft's menu. No iTerm2 restart needed.

Terminal.app does not expose this toggle, so right-click is iTerm2-only.

### 2. Cmd+S as save (and other Cmd shortcuts)

`Ctrl+S` saves out of the box in any terminal. Getting `Cmd+S` to save in croft on macOS takes one extra step that no terminal app can fix on its own. macOS reserves the Cmd modifier for application menus; both Terminal.app and iTerm2 follow this rule. iTerm2 ≥3.5 supports the kitty keyboard protocol (croft negotiates `\x1b[>3u` on startup), but iTerm2 still does not deliver `Cmd+letter` over CSI u even with **Apps can change how keys are reported** and **Report keys using CSI u** both enabled. Verified empirically.

The standard fix is a one-line key mapping that rewrites `Cmd+letter` to the byte `Ctrl+letter` already sends. Croft's existing tested `Ctrl+S` handler does the rest.

iTerm2 → Settings → **Profiles** → Default → **Keys** tab → **Key Mappings** sub-tab → click **+** → "Click to Set" → press **⌘S** → Action: **Send Hex Code** → Code: `0x13` → OK.

| iTerm2 keystroke to bind | Hex code | What croft does |
|--------------------------|----------|-----------------|
| `⌘S` | `0x13` | Save |
| `⌘Q` | `0x11` | Quit |
| `⌘B` | `0x02` | Toggle file tree |
| `⌘C` | `0x03` | Copy current selection (editor or terminal) to the system clipboard via OSC 52 |
| `⌘X` | `0x18` | Cut the editor selection |
| `⌘Z` | `0x1a` | Undo the last editor edit |
| `⌘A` | `0x01` | Select all in the focused pane (editor: select whole buffer). Without this map iTerm2 runs **Edit → Select All** on the whole iTerm2 window instead. |

For `⌘V` to paste into the croft Search input, see section 4 below — iTerm2's **Edit → Paste** menu eats the keystroke at the macOS app-menu level before any bracketed-paste sequence is generated, so the same App Shortcuts + Send Hex Codes workaround applies.

### 3. Cmd+Shift+F to jump to the Search sidebar

`Cmd+Shift+F` in iTerm2 collides twice with croft. First, iTerm2's **Edit → Find → Find Globally…** menu item owns `⌘⇧F` at the macOS application-menu level, so the chord never reaches the terminal session. Second, even after freeing the menu shortcut, iTerm2 doesn't deliver `⌘⇧letter` over the kitty protocol the way other terminals do — the same constraint that forces the `⌘S → Ctrl+S` workaround above.

To make `⌘⇧F` jump croft to the Search view:

1. **Free the menu shortcut.** System Settings → Keyboard → Keyboard Shortcuts → **App Shortcuts** → click `+` → Application: **iTerm**, Menu Title: `Find Globally...` (with the three dots), Keyboard Shortcut: any chord you don't use (e.g. `⌃⌥⇧⌘F`). Quit and reopen iTerm2.
2. **Bind `⌘⇧F` to emit the kitty CSI u sequence croft recognises.** iTerm2 → Settings → **Profiles** → Default → **Keys** → **Key Mappings** → click `+` → Click to Set: press `⌘⇧F` → Action: **Send Hex Code** → Codes:
   ```
   0x1b 0x5b 0x37 0x30 0x3b 0x31 0x30 0x75
   ```
   That's `\x1b[70;10u` — kitty-protocol-encoded `Shift+Cmd+F` (codepoint 70 for `F`, modifier mask 10 = base 1 + Shift 1 + Super 8). Crossterm decodes it as `KeyEvent { code: Char('F'), modifiers: SHIFT | SUPER }`, which croft's existing `is_search_jump_key` already matches.

After both steps `⌘⇧F` jumps to the Search panel from anywhere in croft.

### 4. Pasting into the Search input

The Search input row has a clickable **`[Paste]`** button on the right, just before the `Aa ab .*` toggles. Click it with the mouse and croft reads the macOS clipboard via `pbpaste` and inserts the contents into the query. This path is iTerm2-immune: mouse events come through the terminal regardless of any menu shortcut conflicts.

`⌘V` may also work as a keystroke if you bind iTerm2 → Settings → **Profiles** → Default → **Keys** → **Key Mappings** → click `+` → press `⌘V` → Action: **Send Hex Code** → Code: `0x16`. That's the `Ctrl+V` byte; croft's `is_search_paste_key` handler matches it. But many iTerm2 profile/version combinations ignore the per-profile Key Mapping for `⌘V` because the macOS Edit → Paste app-menu shortcut fires first. If you see the Edit menu flicker and nothing pastes, iTerm took the keystroke at the menu level and never sent the hex code; click the `[Paste]` button instead.

Other terminals (kitty, Ghostty, WezTerm, Alacritty) deliver Cmd over the kitty protocol natively; croft already negotiates it on startup, so `Cmd+S`, `Cmd+Shift+F`, `Cmd+V`, and friends work there with no remap.

## How the embedded terminal works

`portable_pty::native_pty_system().openpty(...)` allocates a pseudoterminal and `spawn_command(...)` runs `$SHELL` on the slave side. A background thread drains the master fd into a `vt100::Parser`, which maintains the screen cell grid in memory. The render path walks `screen.cell(y, x)` for every cell in the pane and emits styled cells to the ratatui buffer with proper foreground / background / bold / italic / underline / reverse styles.

Resizes call `master.resize(...)` and `parser.set_size(...)` so programs like `htop`, `vim`, or your shell prompt redraw to fit the pane.

Keystrokes from `crossterm`'s `Event::Key` are translated back to the byte sequences real terminals send (arrow keys to `\x1b[A` etc., `Ctrl+letter` to `0x01..0x1a`, `Alt+x` to `\x1b<x>`) and written to the master writer.

## Project layout

```
src/
├── main.rs              entry point
├── cli.rs               clap CLI: open path, setup-terminal / setup-iterm2 / keys subcommands
├── app.rs               event loop, three-pane layout + activity bar, key dispatch, status bar, mouse + clipboard
├── git.rs               branch / dirty / ahead-behind status by shelling out to git
├── highlight.rs         tree-sitter highlight registry per language
├── icons.rs             Codicon / Devicon / Seti glyphs and per-language colors
├── iterm2.rs            iTerm2 plist mutation helpers for setup-iterm2
└── widgets/
    ├── mod.rs
    ├── file_tree.rs     ignore::WalkBuilder backed tree, lazy children, fs-watcher refresh
    ├── editor.rs        tree-sitter highlighted editor with full write path, mouse-drag selection, OSC 52 copy/cut
    ├── search.rs        sidebar search panel + .gitignore-aware substring walker
    └── terminal.rs      portable-pty + vt100 + ratatui integration with selection + scrollback
tests/cli.rs             integration tests for the CLI surface
```

## Status

What works: three-pane layout, file tree expansion / collapse, mouse and keyboard, right-click context menu (New File / New Folder / Delete-to-Trash) with Delete-key shortcut, live filesystem watcher that picks up external changes within ~100ms, file open with tree-sitter highlighting (Rust, Python, JS, TS, TSX, JSON, TOML, YAML, Markdown, Go, HTML, CSS, Bash), full editor write path (insert / delete / Enter / Tab / Backspace / save round-trip with `●` dirty marker, auto-reload on external write when buffer is clean), embedded shell with full ANSI color and key forwarding, git status pill in the bottom bar (branch, dirty bullet, ahead/behind), `setup-terminal` and `setup-iterm2` AppleScript / plist helpers. The repo ships 155 tests; run with `cargo test`. What does not work yet: command palette, multi-tab editor, search, settings, LSP.

## Limitations

This is not an IDE. There is no LSP, no debugger, no plugin system. If you want those, use VS Code, Neovim, or Helix. This project's goal is the three-pane experience as a single fast binary and a building block for embeddable Rust TUI products.

## License

MIT.