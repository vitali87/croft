<div align="center">
  <img src="assets/logo-tight.png" alt="croft" width="180">
</div>

# croft

A VS Code style three pane workspace that runs entirely inside your terminal. Written in Rust for performance and ships as a single static binary.

* **Left pane (sidebar):** Explorer with multi-select, cut / copy / paste, drag and drop file moves, and VS Code style icons (Codicons / Devicons / Seti). Two more sidebar views switch in via the activity bar: full-text Search and a Remote (SSH) explorer.
* **Top right pane (editor):** code editor with `tree-sitter` syntax highlighting plus inline preview tabs for PNG / JPEG / GIF / BMP / WebP, PDFs (with page navigation), and CSV / TSV / XLSX / XLS / ODS spreadsheets. An optional native modal (vim) editing mode toggles on with `Cmd`+`E`.
* **Bottom right pane (terminal):** a real interactive shell, your `$SHELL` running on a real PTY.
* All three panes resize by dragging the seams between them.

Built on [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm), with [portable-pty](https://docs.rs/portable-pty/) for the embedded shell, [vt100](https://docs.rs/vt100/) for terminal-state parsing, [tree-sitter](https://tree-sitter.github.io/tree-sitter/) for incremental, AST-based syntax highlighting, [calamine](https://docs.rs/calamine/) for spreadsheet parsing, and the iTerm2 OSC 1337 inline-image protocol for image / PDF previews.

## Requirements

| Requirement | Why |
|-------------|-----|
| macOS or Linux | The PTY layer uses POSIX `forkpty`. Windows is not yet supported. |
| Rust 1.78+ stable | To compile the binary. |
| A Nerd Font as your terminal font | The file explorer icons are Private Use Area glyphs (Codicons, Devicons, Seti). Without a Nerd Font, icons render as `[?]` boxes. |
| A 256 color or truecolor terminal | macOS Terminal.app, iTerm2, Alacritty, kitty, WezTerm, Ghostty all qualify. |
| iTerm2, WezTerm, Ghostty, or kitty (optional) | Required for inline image / PDF / sheet preview rendering via OSC 1337. Other terminals fall back to a metadata header line so the feature is still informative. |
| `pdftoppm` from poppler-utils (optional) | Multi-page PDF preview. Install with `brew install poppler` (macOS) or `apt install poppler-utils` (Linux). Without it, croft falls back to macOS `sips` for page 1 only. |

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

> **PostScript name vs display name.** AppleScript needs the *PostScript* name, which is what's embedded in the .ttf, not what Terminal.app's font picker displays. The PostScript name for `MesloLGSNerdFontMono-Regular.ttf` is `MesloLGSNFM-Regular`. Passing the display name silently no-ops in AppleScript and Terminal.app keeps the previous font, which is why icons appear broken even after running the setup command.

## Install

The fastest path, no clone required:

```bash
cargo install --git https://codeberg.org/vitali87/croft.git
```

This compiles croft from the latest `main` and drops the binary in `~/.cargo/bin/croft`. Re-run the same command to upgrade.

### Build from source

```bash
git clone https://codeberg.org/vitali87/croft.git
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

### Global

| Keys | Action |
|------|--------|
| `Ctrl+s` | Save the open file |
| `Ctrl+q` | Quit |
| `F1` | Open the shortcuts modal (every binding grouped by pane, scrollable) |
| `F6` | Cycle focus across panes (tree → editor → terminal → tree) |
| `Ctrl+b` | Toggle the file tree / side panel |
| `Ctrl+j` | Toggle the terminal pane |
| `Ctrl+Shift+j` | Maximize the terminal pane (collapses the editor / welcome to zero rows so the terminal fills the right column next to the Explorer; press again to restore the previous split) |
| `Ctrl+p` (or `Cmd+p` with the iTerm2 setup below) | Quick Open: fuzzy-search workspace files by name and jump to the picked file (auto-expands the Explorer to reveal it) |
| `Ctrl+Shift+e` / `Cmd+Shift+e` | Jump to the Explorer sidebar from any pane |
| `Ctrl+Shift+f` / `Cmd+Shift+f` | Jump to the Search sidebar |
| `Ctrl+Shift+s` / `Cmd+Shift+s` | Jump to Source Control (works from the editor too) |
| `Ctrl+Shift+d` / `Cmd+Shift+d` | Jump to Run and Debug |
| `Ctrl+Shift+r` / `Cmd+Shift+r` | Jump to Remote (SSH) |
| `Ctrl+Shift+l` / `Cmd+Shift+l` | While connected to a remote, disconnect and drop back into the local croft at the directory you connected from (`Ctrl+q` still fully exits) |
| Click activity-bar icons (left edge) | Switch between Explorer, Search, Source Control, Run-Debug, and Remote sidebar views |
| Drag the vertical seam between sidebar and editor | Resize the sidebar |
| Drag the horizontal seam between editor and terminal | Resize the terminal pane |
| Mouse wheel | Scroll the pane under the pointer |

### Explorer (file tree)

| Keys | Action |
|------|--------|
| `↑` / `↓` | Move selection |
| `Enter` or `→` on a file | Open in editor; on a folder: expand or collapse |
| `←` on a folder | Collapse |
| `Shift`+`↑` / `↓` / `PageUp` / `PageDown` / `Home` / `End` | Extend multi-selection from the anchor row |
| `Shift`+click another row | Extend multi-selection across the range |
| `Alt`+click (Option+click on macOS) or `Ctrl`+click | Toggle a row in or out of the multi-selection |
| `Ctrl`+`A` / `Cmd`+`A` | Select every visible row |
| `Esc` | Clear the multi-selection |
| `Ctrl`+`C` / `Cmd`+`C` | Copy selected paths to the explorer clipboard |
| `Ctrl`+`X` / `Cmd`+`X` | Cut selected paths to the explorer clipboard |
| `Ctrl`+`V` / `Cmd`+`V` | Paste clipboard paths into the focused folder (move on Cut, copy on Copy) |
| `Cmd`+`Z` | Jump to a directory via zoxide: opens a fuzzy popup over your frecency-ranked dirs, then re-roots the workspace there and `cd`s the active terminal (same jump as `j` in the shell; `j` stays free, `Cmd`/`Ctrl`+`J` still does terminal toggle/maximize). When a strict zoxide query finds nothing, a typo-tolerant fallback kicks in: it edit-distance-matches your last keyword against directory names (forgiving transpositions like `spilt` → `pr-split`, which zoxide's own matcher cannot) and flags the list as approximate |
| Drag a row onto a folder | Move the selection into that folder |
| `Alt`-drag a row onto a folder | Copy the selection into that folder instead of moving |
| `Delete` / `Backspace` (or `Cmd`+`Backspace`) | Move every selected path to the OS Trash. On macOS the trash sound plays once for the whole batch. |
| Right-click | Context menu: Cut, Copy, Paste, Rename, Delete (with item count when multi-selected), and on empty space New File / New Folder |

### Search sidebar

| Keys | Action |
|------|--------|
| Type | Live `.gitignore`-aware search across the workspace; refreshes per keystroke (~120 ms debounce, off the UI thread). Capped at 200 hits. |
| Click `Aa`, `ab`, `.*` toggles | Flip case-sensitive / whole-word / regex; re-runs immediately. Active toggles render with a yellow background. |
| `↑` / `↓` + `Enter`, or click a result | Open the file at the matched line |

### Editor: text

| Keys | Action |
|------|--------|
| Arrows, Home, End | Navigate (clears any active selection) |
| `Shift`+arrows / `Shift`+`Home` / `End` / `PageUp` / `PageDown` | Extend the selection by the same motion |
| `PageUp` / `PageDown` (`fn`+`↑` / `fn`+`↓` on Mac) | Scroll exactly one viewport |
| Any printable char, Enter, Backspace, Delete, Tab | Edit (typing or deleting with an active selection replaces it) |
| Mouse drag | Select text; selection stays highlighted until you copy or click elsewhere |
| `Ctrl`+`C` / `Cmd`+`C` | Copy the selection to the system clipboard (native NSPasteboard on macOS; OSC 52 fallback on remote) |
| `Ctrl`+`X` / `Cmd`+`X` | Cut the selection |
| `Ctrl`+`V` / `Cmd`+`V` | Paste at the cursor; replaces selection if any |
| `Ctrl`+`Z` / `Cmd`+`Z` | Undo (typing bursts coalesce into one step; backspace, paste, cut, replace are each their own step) |
| `Cmd`+`A` | Select the entire buffer |
| `Ctrl`+`f` / `Cmd`+`f` | Open the inline Find bar at the top-right of the editor — pre-fills the query from the selection (single line) or the word under the cursor; typing jumps the cursor to the first match at-or-after the cursor and highlights the active match in orange and the rest in yellow; `Enter` / `F3` walks forward, `Shift+Enter` / `Shift+F3` walks back, `Esc` closes |
| `Ctrl`+`A` | Move to the start of the current line (readline-style) |
| `Ctrl`+`E` | Move to the end of the current line |
| `Ctrl`+`K` | Kill from cursor to end of line (yanks to the system clipboard) |
| `Ctrl`+`U` | Kill from cursor to start of line (yanks to the system clipboard) |
| `Cmd`+`o` | Open a new line below the current row, inheriting its indent |
| `Cmd`+`Shift`+`O` | Open a new line above the current row, inheriting its indent |
| `Cmd`+`g` `g` | Go to the top of the file |
| `Cmd`+`N` `Cmd`+`g` `g` | Go to line `N` (count can lead, `Cmd`+`5` `Cmd`+`g` `g` → line 5; count can also go after the first `Cmd`+`g`) |
| `Cmd`+`Shift`+`G` | Go to the bottom of the file (with a leading count, jumps to that line) |
| `Cmd`+`d` `d` | Delete the current line (yanks to the system clipboard) |
| `Cmd`+`N` `Cmd`+`d` `d` | Delete `N` lines |
| `Cmd`+`y` `y` | Yank (copy) the current line to the system clipboard |
| `Cmd`+`N` `Cmd`+`y` `y` | Yank `N` lines |
| `Esc` | Clear the current selection |
| `Cmd`+`E` | Toggle native modal (vim) editing for the editor pane (see below) |

### Editor: vim mode (modal editing)

`Cmd`+`E` toggles a native, Rust-implemented modal layer over the editor pane. It is an emulation of the common daily-driver subset, not embedded neovim, so it carries no `nvim` dependency, behaves identically on local and remote, and stays on the input hot path. When it is on, a coloured mode pill (`NORMAL` blue, `INSERT` green, `VISUAL` purple) and the active `:`/`/` line show in the status bar. While it is off the editor behaves exactly as the tables above describe, and every `Cmd`/`Ctrl` shortcut keeps working in Normal mode because modal editing only claims unmodified keys. For full vim with your own plugins and `init.lua`, run real `nvim` in the shell pane. The `Cmd`+`E` toggle reaches croft only after `croft setup-iterm2` and an iTerm2 relaunch: macOS otherwise binds the chord to Edit > Find > Use Selection for Find, so the setup step relocates that menu item to `Cmd`+`Opt`+`E` and forwards `Cmd`+`E` as CSI-u, exactly as it does for the other Mac-style Cmd chords.

| Keys | Action |
|------|--------|
| `i` `a` `I` `A` `o` `O` | Enter Insert mode (at cursor, after, first non-blank, end of line, open below, open above) |
| `Esc` | Leave Insert/Visual; clear a pending operator or count |
| `h` `j` `k` `l`, arrows | Move by one cell |
| `w` `b` `e` | Word forward / back / end (vim word classes, not VS Code word stops) |
| `0` `^` `$` | Line start / first non-blank / line end |
| `gg` `G` `{n}G` | File start / file end / absolute line |
| `f`/`t`/`F`/`T` `{char}`, `;` `,` | Find char on the line (on / till, forward / back), repeat / repeat-reversed |
| `{n}` prefix | Count for the next motion or operator (`3j`, `2dw`, `5G`) |
| `x` | Delete `{count}` chars under the cursor (yanks them) |
| `d` `y` `c` + motion / text object | Delete / yank / change over a motion (`dw`, `d$`, `dfx`), or `iw`/`aw` (`diw`, `ciw`, `daw`) |
| `dd` `yy` `cc` | Linewise delete / yank / change (`{n}dd` for several lines) |
| `p` `P` | Paste after / before (linewise when the yank was a whole line) |
| `u` | Undo |
| `v` `V` | Charwise / linewise Visual; then a motion extends and `d` `y` `c` operate |
| `/` `?` then `Enter`, `n` `N` | Search forward / back in the buffer, jump to next / previous match |
| `:w` `:q` `:wq` `:x` `:q!` `:qa`, `:{n}` | Write, close tab, write-and-close, quit-all, or jump to line `n` |

When vim mode is on it supersedes the always-on `Cmd`+`d` `d` / `Cmd`+`g` `g` chord shortcuts listed above; turn it off to get those back.

### Editor: image preview (`.png`, `.jpg`, `.jpeg`, `.gif`, `.bmp`, `.webp`)

Tabs are read-only. Every keystroke is swallowed so a stray key cannot corrupt a buffer the user cannot see.

### Editor: PDF preview (`.pdf`)

| Keys | Action |
|------|--------|
| `→` / `Page Down` / `Space` | Next page |
| `←` / `Page Up` | Previous page |
| `Home` | First page |
| `End` | Last page (when page count is known) |

### Editor: spreadsheet preview (`.csv`, `.tsv`, `.xlsx`, `.xls`, `.xlsb`, `.ods`)

| Keys | Action |
|------|--------|
| `↑` / `↓` / `←` / `→` | Pan one row / column |
| `PageUp` / `PageDown` | Pan a full viewport vertically |
| `Home` | Jump to row 1, column 1 |
| `End` | Jump to the last visible page |
| `Tab` / `Shift+Tab` | Switch worksheet (in multi-sheet workbooks) |

### Terminal

| Keys | Action |
|------|--------|
| Any key | Forwarded to the shell PTY (arrows, `Ctrl+letter`, `Alt+x`, function keys all translated to the proper VT escape sequences) |
| Mouse drag | Select text; selection stays highlighted until you copy or click elsewhere |
| Mouse wheel | Scroll through 5000 rows of scrollback. In alternate-screen mode (vim / less / htop) the wheel forwards arrow keys so the running app handles it. Any keystroke snaps back to the live bottom. |
| `Ctrl+Shift+c` (or `Cmd+c` with kitty-protocol terminals) | Explicit copy of the terminal's current selection |
| `Ctrl+Shift+t` | Open another terminal next to the current one (works from any pane). Each terminal has its own PTY, scrollback, and selection. |
| `Ctrl+Shift+w` | Close the active terminal (no-op when only one is left; use `Ctrl+J` to hide the pane). |
| `Ctrl+Shift+]` | Cycle to the next terminal in the pane. Click any terminal to switch focus directly. |

## iTerm2 setup for macOS users

Run Croft's iTerm2 setup once after building:

```bash
./target/release/croft setup-iterm2
```

This writes the default-profile font settings plus Croft's iTerm2 keyboard setup:

| iTerm2 keystroke | Installed mapping | What croft does |
|------------------|-------------------|-----------------|
| `⌘P` | `\x1b[112;9u` | Quick Open: fuzzy-search workspace files |
| `⌘F` | `\x1b[102;9u` | In-editor Find (jumps to next match as you type) |
| `⌘⇧E` | `\x1b[69;10u` | Jump to the Explorer sidebar |
| `⌘⇧F` | `\x1b[70;10u` | Jump to the Search sidebar |
| `⌘⇧S` | `\x1b[83;10u` | Jump to Source Control |
| `⌘⇧D` | `\x1b[68;10u` | Jump to Run and Debug |
| `⌘⇧R` | `\x1b[82;10u` | Jump to Remote (SSH) |
| `⌘⇧L` | `\x1b[76;10u` | Disconnect a remote session and drop back into the local croft |
| `⌘⇧N` | `\x1b[78;10u` | Explorer "New folder" prompt (when the tree is focused) |
| `⌃⇧J` | `\x1b[74;6u` | Maximize the terminal pane (collapses the editor / welcome; press again to restore the previous editor↔terminal split) |
| `⌘V` | `\x1b[118;9u` in global and profile key maps | Read the system clipboard and paste into the focused editor, or into Search when Search is active |

It also moves the following iTerm2 / macOS menu shortcuts out of the way (each goes to `Cmd+Opt+<letter>` so the original iTerm2 action stays reachable on a chord croft does not use): **Edit → Find → Find Globally...** off `⌘⇧F`, **Edit → Paste** off `⌘V`, **Shell → Split Vertically with Same Profile** off `⌘D`, **Shell → Split Horizontally with Same Profile** off `⌘⇧D`, **Edit → Find Next / Find Previous / Jump to Selection**, **File → Print** off `⌘P`, **Window → Select Tab 1..9** off `⌘1..⌘9`, and the macOS **Help → Show Help Menu** off `⌘⇧/`. Fully quit iTerm2 with `⌘Q` and reopen it after setup; iTerm2 caches its plist while running.

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
| `⌘C` | `0x03` | Copy current selection (editor or terminal) to the system clipboard (native NSPasteboard on macOS; OSC 52 fallback on remote) |
| `⌘X` | `0x18` | Cut the editor selection |
| `⌘Z` | `0x1a` | Undo the last editor edit |
| `⌘A` | `0x01` | Select all in the focused pane (editor: select whole buffer). Without this map iTerm2 runs **Edit → Select All** on the whole iTerm2 window instead. |

For `⌘⇧F` and `⌘V`, use `croft setup-iterm2`; it installs both mappings globally and into every profile.

### 3. Cmd+Shift+F to jump to the Search sidebar

After `setup-iterm2` and an iTerm2 relaunch, `⌘⇧F` jumps to the Search panel from anywhere in croft. The installed global mapping emits `\x1b[70;10u`, the kitty-protocol encoding for `Shift+Cmd+F`; crossterm decodes that as `KeyEvent { code: Char('F'), modifiers: SHIFT | SUPER }`, which croft handles as Search.

### 4. ⌘V paste

After `setup-iterm2` and an iTerm2 relaunch, the working flow is:

1. Press `⌘⇧F`; Search becomes active.
2. Press `⌘V`; iTerm2 sends `\x1b[118;9u`, the kitty/CSI-u encoding for `Cmd+V`.
3. Croft handles that as Search paste, reads the macOS clipboard via native `NSPasteboard`, and inserts it into the Search query.

When the editor is focused, that same `⌘V` path pastes into the editor, even if the Search sidebar is still visible. If another terminal sends a normal bracketed paste event instead of the CSI-u key event, croft routes it by focus the same way.

**Zero-setup alternative: ⌃⇧V.** If you don't want to touch System Settings, press `⌃⇧V` (Control+Shift+V) inside the Search input. iTerm encodes that as the `0x16` byte natively, with no menu conflict and no per-profile mapping needed. croft's search-paste handler matches it the same way as ⌘V.

Other terminals (kitty, Ghostty, WezTerm, Alacritty) deliver Cmd over the kitty protocol natively; croft already negotiates it on startup, so `Cmd+S`, `Cmd+Shift+F`, `Cmd+V`, and friends work there with no remap.

## How the embedded terminal works

`portable_pty::native_pty_system().openpty(...)` allocates a pseudoterminal and `spawn_command(...)` runs `$SHELL` on the slave side. A background thread drains the master fd into a `vt100::Parser`, which maintains the screen cell grid in memory. The render path walks `screen.cell(y, x)` for every cell in the pane and emits styled cells to the ratatui buffer with proper foreground / background / bold / italic / underline / reverse styles.

Resizes call `master.resize(...)` and `parser.set_size(...)` so programs like `htop`, `vim`, or your shell prompt redraw to fit the pane.

Keystrokes from `crossterm`'s `Event::Key` are translated back to the byte sequences real terminals send (arrow keys to `\x1b[A` etc., `Ctrl+letter` to `0x01..0x1a`, `Alt+x` to `\x1b<x>`) and written to the master writer.

## Project layout

```
src/
├── main.rs              entry point + module declarations
├── cli.rs               clap CLI: open path, setup-terminal / setup-iterm2 / setup-cross / remote / keys subcommands
├── clipboard.rs         native macOS clipboard read/write (NSPasteboard) with pbpaste fallback
├── git.rs               branch / dirty / ahead-behind status, plus anonymous git-protocol fetch for the welcome screen recents
├── highlight.rs         tree-sitter highlight registry per language
├── icons.rs             Codicon / Devicon / Seti glyphs and per-language colors
├── install_session.rs   streams install-progress events while a remote host builds / installs the croft binary
├── iterm2.rs            iTerm2 plist mutation helpers for fonts and Croft key mappings
├── iterm2_inline.rs     OSC 1337 inline-image baking pipeline (welcome wordmark, image / PDF preview, activity-bar icons, SSH empty-state hero)
├── pdf.rs               PDF rasteriser: prefers pdftoppm (poppler), falls back to macOS sips
├── remote.rs            remote (SSH) target metadata and launch dispatch
├── remote_connect.rs    interactive SSH connect flow (host + password prompt phases) behind the connect dialog
├── session_state.rs     captures open tabs / layout so a self-update re-exec can restore them
├── sheet.rs             CSV / TSV / XLSX / XLS / XLSB / ODS parsing via the csv and calamine crates
├── sysmon.rs            system-metrics sampler loop (CPU / memory / network / disk / temp)
├── update_watch.rs      remote self-update: watch for a newer binary installed under a running remote croft
├── vim.rs               native modal (vim-style) editing: a pure key state machine (modes, counts, operators, text objects, f/t, search, ex-commands) that emits editing intents the app applies; toggled with Cmd+E
├── zoxide.rs            zoxide integration: strict query + typo-tolerant fuzzy fallback (Damerau-Levenshtein) + cross-platform ensure-install backing the Cmd+Z jump popup
├── app/                 event loop, three-pane layout + activity bar, key dispatch, status bar, mouse, clipboard, splitters, preview overlays
│   ├── mod.rs           the main App: render, key / mouse dispatch, status bar, splitters
│   ├── click.rs         double / triple click detection
│   ├── cursor_blink.rs  caret blink timing
│   ├── fs_watch.rs      filesystem watch + poll fallback feeding tree / editor / terminal refresh
│   ├── git_worker.rs    off-thread git status / changes worker
│   ├── hover.rs         LSP hover dwell timing
│   ├── nav.rs           editor back / forward navigation history
│   ├── overlay.rs       OSC 1337 inline-image overlay state + clear-on-hide latches
│   ├── perf_hud.rs      F8 performance HUD
│   ├── sys_monitor.rs   background system-metrics poller driving the SYSTEM panel
│   ├── welcome.rs       welcome-screen state + async recent-repos drain
│   └── tests.rs         unit / integration tests
├── lsp/                 LSP client stack
│   ├── mod.rs
│   ├── client.rs        async-lsp client wrapper with router for unhandled notifications
│   ├── config.rs        per-language LSP config (basedpyright, ruff, ty, rust-analyzer)
│   ├── log_file.rs      LSP stderr / debug log sink at ~/.croft/lsp.log
│   ├── manager.rs       lifecycle: spawn / did_open / did_change / completion / shutdown
│   ├── registry.rs      language detection from file extension and shebang
│   └── runtime.rs       Tokio runtime owned by the LSP manager
└── widgets/
    ├── mod.rs
    ├── completion_popup.rs  LSP completion popup (anchored at the cursor, filterable)
    ├── connect_dialog.rs    remote SSH connect modal (host + auth prompt phases)
    ├── diff.rs          side-by-side file diff renderer used by the explorer's Compare action
    ├── editor.rs        tree-sitter highlighted editor with full write path, mouse-drag selection, native-clipboard copy / cut, plus image / PDF / spreadsheet preview tabs
    ├── editor_find.rs   VS Code-style inline Find bar (Cmd+F) with active-match orange highlight, Enter / Shift+Enter walk, case-sensitive / whole-word / regex toggles
    ├── file_finder.rs   VS Code-style Quick Open (Cmd+P) fuzzy file picker with tiered match ranking (exact filename > prefix > substring > path > subsequence)
    ├── file_tree.rs     ignore::WalkBuilder backed tree, lazy children, fs-watcher refresh, multi-select, drag-drop, bulk trash, reveal-path on Cmd+P open
    ├── hover_popup.rs   LSP hover popup (300 ms dwell, anchored at the cursor)
    ├── remote.rs        Remote (SSH) sidebar widget with empty-state hero illustration
    ├── run_debug.rs     Run and Debug sidebar widget: empty state plus Run [filename] button that spawns the active file in a fresh terminal
    ├── scrollbar.rs     shared vertical-scrollbar geometry
    ├── search.rs        sidebar search panel + .gitignore-aware substring walker
    ├── shortcuts.rs     F1 shortcuts modal: every binding grouped by pane, scrollable
    ├── source_control.rs Source Control sidebar widget: branch summary, commit input, change list, commit button, no-repo hero
    ├── system_panel.rs  collapsible SYSTEM metrics panel pinned to the sidebar bottom
    ├── terminal.rs      portable-pty + alacritty_terminal + ratatui integration with selection + scrollback
    └── zoxide_jump.rs   Cmd+Z zoxide jump popup: fuzzy directory jumper that re-roots + cd's the terminal
tests/cli.rs             integration tests for the CLI surface
```

## Status

What works:

* Three-pane layout with draggable splitters between sidebar / editor / terminal.
* File tree with expansion / collapse, multi-select (Shift+click range, Alt or Ctrl+click toggle), drag-and-drop file moves (Alt-drag for copy), explorer-scoped Cut / Copy / Paste, bulk delete to OS Trash with a single trash sound on macOS.
* Right-click context menu with Cut, Copy, Paste, Rename, count-aware Delete, plus New File / New Folder on empty space.
* Live filesystem watcher with a 50 ms polling fallback for missed startup or host events.
* File open with tree-sitter highlighting (Rust, Python, JS, TS, TSX, JSON, TOML, YAML, Markdown, Go, HTML, CSS, Bash).
* Full editor write path: insert / delete / Enter / Tab / Backspace / save round-trip with `●` dirty marker, auto-reload on external write when the buffer is clean (across every open tab, not just the focused one), a save-conflict guard that refuses to clobber an external change to a dirty buffer until you press Cmd+S again to overwrite, native-clipboard copy / cut (OSC 52 fallback on remote), undo with intelligent edit-step coalescing.
* Inline preview tabs that render directly in the editor pane via OSC 1337: PNG / JPEG / GIF / BMP / WebP, PDFs (with page navigation, multi-page when poppler is installed), and CSV / TSV / XLSX / XLS / XLSB / ODS spreadsheets.
* Search sidebar (live, `.gitignore`-aware, off the UI thread, regex / case / whole-word toggles).
* Remote (SSH) sidebar that lists hosts from `~/.ssh/config` and launches a remote croft session.
* Run and Debug sidebar (icon four): a Run [filename] button that picks a runner by file extension (Python, Node, Ruby, bash, zsh, fish, PHP, Perl, Lua, plus tsx for TS/TSX) and spawns the file in a fresh terminal at the right cwd. Python is venv-aware: walks from the file's directory up to the workspace root looking for `.venv/bin/python`, `venv/bin/python`, or `.env/bin/python` and uses the project's interpreter when found, falling back to system `python3` only when nothing is in scope.
* Embedded shell with full ANSI color, key forwarding, mouse-drag text selection, and 5000-row scrollback.
* Git status pill in the bottom bar (branch, dirty bullet, ahead / behind).
* Welcome recents fetched live via the anonymous git protocol so the panel works behind shared egress IPs (Tailscale, corporate NAT) where the Bitbucket / GitHub REST APIs are rate-limited.
* `setup-terminal` and `setup-iterm2` AppleScript / plist helpers.

The repo ships 568 unit tests plus CLI integration tests; run with `cargo test`.

Already working: the three-pane layout, file explorer, multi-tab editor with syntax highlighting, live embedded terminals, git status, fuzzy file finder, remote launch over SSH, and LSP-backed completion and diagnostics (Python, TypeScript/TSX, JavaScript, Rust, Go).

On the roadmap: command palette, settings, a stepping debugger, and a plugin system.

## Goal

A complete VS Code replacement in the terminal: the full IDE experience as a single fast Rust binary. Everything VS Code does, croft will do, without leaving the TUI.

## License

MIT.