<div align="center">
  <img src="assets/logo-tight.png" alt="croft" width="180">
</div>

# croft

A VS Code style three pane workspace that runs entirely inside your terminal. Written in Rust for performance and ships as a single static binary.

## Tenets

The non-negotiables that shape every decision in croft:

1. **Speed is a must.** Written in Rust, shipped as a single static binary. Every feature is weighed against its cost on the hot path before it lands.
2. **Low latency is non-negotiable.** Keystrokes and clicks register instantly. Rendering is coalesced so even a noisy shell can never starve input.
3. **Local and remote parity always binds.** Behaviour on your Mac and on a Linux box over SSH is identical. There is no second-class remote mode.
4. **The gap between terminal and GUI stays minimal.** croft should look and feel like VS Code, down to the icons and motion, never a stripped back approximation.
5. **Everything has a shortcut.** Every action is reachable from the keyboard, and no menu item ships without an accelerator.
6. **Correctness beats workarounds.** Bugs are fixed at the root cause, never papered over with a fallback, a downgrade, or an older dependency.
7. **One binary, no ceremony.** croft ships as a single static binary and stays that way. Features are emulated in process rather than bolted on as heavyweight dependencies, so there is nothing to wire up after you install it.

## Layout

* **Left pane (sidebar):** Explorer with multi-select, cut / copy / paste, drag-and-drop file moves, and VS Code style icons. Search and a Remote (SSH) explorer swap in via the activity bar.
* **Activity bar:** the icon strip down the far left, with view icons (Explorer, Search, Source Control, Remote, Run and Debug) at the top and a settings gear at the bottom. Its Color Theme picker switches between Croft Black (`#000000`, default) and Croft Dark Blue (`#1e222e`), persisted in `~/.config/croft/config.json`.
* **Top right pane (editor):** code editor with `tree-sitter` syntax highlighting, an LSP semantic-token overlay, and inline preview tabs for images, PDFs, and spreadsheets. Splits side by side with `Cmd`+`\`, with an optional native vim modal mode on `Cmd`+`E`.
* **Bottom right pane (terminal):** a real interactive shell, your `$SHELL` running on a real PTY.
* All three panes resize by dragging the seams between them, including the seam between the two editor columns when the editor is split.

Built on [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm), with [portable-pty](https://docs.rs/portable-pty/) for the embedded shell, [alacritty_terminal](https://docs.rs/alacritty_terminal/) for terminal-state parsing, [tree-sitter](https://tree-sitter.github.io/tree-sitter/) for incremental, AST-based syntax highlighting, [calamine](https://docs.rs/calamine/) for spreadsheet parsing, and the iTerm2 OSC 1337 inline-image protocol for image / PDF previews.

## Requirements

| Requirement | Why |
|-------------|-----|
| macOS, Linux, or Android (Termux) | The PTY layer uses POSIX `forkpty`. Windows is not yet supported. On Termux croft runs as a native Android build; see [Termux / Android](#termux--android). |
| Rust 1.85+ stable | To compile the binary (the crate uses edition 2024, stabilized in Rust 1.85). |
| A Nerd Font as your terminal font | The file explorer icons are Private Use Area glyphs (Codicons, Devicons, Seti). Without a Nerd Font, icons render as `[?]` boxes. |
| A 256 color or truecolor terminal | macOS Terminal.app, iTerm2, Alacritty, kitty, WezTerm, Ghostty all qualify. |
| iTerm2, WezTerm, Ghostty, or kitty (optional) | Required for inline image / PDF / sheet preview rendering via OSC 1337. Other terminals (including Termux) fall back to a metadata header line so the feature is still informative. If you run a Termux build that supports the iTerm2 OSC 1337 protocol, opt in with `CROFT_FORCE_INLINE_IMAGES=1`. |
| `pdftoppm` from poppler-utils (optional) | Multi-page PDF preview. Install with `brew install poppler` (macOS) or `apt install poppler-utils` (Linux). Without it, croft falls back to macOS `sips` for page 1 only. |
| Node.js + npm (optional) | TypeScript / JavaScript LSP. croft auto-installs the `vtsls` server into `~/.croft/servers` the first time you open a `.ts`/`.tsx`/`.js` file. It finds `node` even when a version manager keeps it off croft's PATH: first by reading the on-disk layout directly (highest nvm `versions/node`, Volta, Homebrew, `/usr/local`), and as a universal fallback by asking your login shell where `node` resolves — run in a detached session (`setsid`) so it can never touch croft's terminal. That covers nvm / fnm / asdf / volta / custom setups, for both the install and running the server. Other language servers (`basedpyright`, `ruff`, `ty`, `rust-analyzer`, `gopls`) are used from your PATH if present. Each server is anchored at the file's own project root (the nearest ancestor with a `pyproject.toml` / `Cargo.toml` / `go.mod` / `package.json`, etc.), not croft's workspace root, so in a monorepo every sub-project's `.venv` and imports resolve correctly. croft runs one server instance per (language, project root), mirroring Zed. |

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
croft remote <host>  # launch croft over SSH on a Linux server (host from ~/.ssh/config)
croft --help
croft setup-terminal --help
```

`croft remote <host>` installs itself on the box on first connect, with no manual prep. The fast path cross-compiles a static musl binary on your Mac and copies it over; the source fallback compiles on the host and provisions its own build dependencies (a C toolchain and `pkg-config`) across `apt`, `dnf`/`yum`, `apk`, `pacman`, and `zypper`, using `sudo` when not already root. A stock cloud image (Ubuntu Server, minimal Fedora, Alpine) works out of the box.

## Keybindings

<details>
<summary>Click to expand the full keybinding tables.</summary>

### Global

| Keys | Action |
|------|--------|
| `Ctrl+s` | Save the open file |
| `Ctrl+q` | Quit |
| `F1` | Open the shortcuts modal (every binding grouped by pane, scrollable) |
| `F6` | Cycle focus across panes (tree → editor → terminal → tree) |
| `Ctrl+b` / `Cmd+b` | Toggle the primary side bar (left pane) visibility, matching VS Code's "Toggle Primary Side Bar" |
| `Ctrl+j` | Toggle the terminal pane |
| `Ctrl+Shift+j` | Maximize the terminal pane (collapses the editor / welcome to zero rows so the terminal fills the right column next to the Explorer; press again to restore the previous split) |
| `Cmd+\` (after the iTerm2 setup below) | Split the editor into two side-by-side columns (VS Code's Split Editor). The new right group duplicates the active file at the same cursor position; each column keeps its own tabs, scroll, and cursor. Closing the last tab in a column collapses the split. |
| `Cmd+Opt+←` / `Cmd+Opt+→` (after the iTerm2 setup below) | Move keyboard focus to the left / right editor group while split. Click either column to focus it directly. |
| `Ctrl+p` (or `Cmd+p` with the iTerm2 setup below) | Quick Open: fuzzy-search workspace files by name and jump to the picked file (auto-expands the Explorer to reveal it) |
| `Ctrl+Shift+e` / `Cmd+Shift+e` | Jump to the Explorer sidebar from any pane |
| `Ctrl+Shift+f` / `Cmd+Shift+f` | Jump to the Search sidebar |
| `Ctrl+Shift+s` / `Cmd+Shift+s` | Jump to Source Control (works from the editor too) |
| `Ctrl+Shift+d` / `Cmd+Shift+d` | Jump to Run and Debug |
| `Ctrl+Shift+r` / `Cmd+Shift+r` | Jump to Remote (SSH) |
| `Ctrl+Shift+l` / `Cmd+Shift+l` | While connected to a remote, disconnect and drop back into the local croft at the directory you connected from (`Ctrl+q` still fully exits) |
| Click activity-bar icons (left edge) | Switch between Explorer, Search, Source Control, Run-Debug, and Remote sidebar views |
| Click the settings gear (bottom of the activity bar) | Open the settings menu → **Color Theme** picker to switch between the Croft Dark (Blue) and Croft Black themes |
| Drag the vertical seam between sidebar and editor | Resize the sidebar |
| Drag the vertical seam between the two editor columns (when split) | Rebalance the side-by-side split |
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
| `Cmd`+`Z` | Jump to a directory via zoxide: opens a fuzzy popup over your frecency-ranked dirs, then re-roots the workspace there and `cd`s the active terminal (same jump as `j` in the shell; `Cmd`/`Ctrl`+`J` still does terminal toggle/maximize). On first launch croft installs the zoxide binary if missing **and** wires its shell hook into your `~/.zshrc` / `~/.bashrc` (as `zoxide init <shell> --cmd j`), so the terminal `j` command and the popup share one database that fills as you `cd` — identical on the local Mac and an SSH remote. The wiring is idempotent and is skipped if you already have a `zoxide init` line. When a strict zoxide query finds nothing, a typo-tolerant fallback kicks in: it edit-distance-matches your last keyword against directory names (forgiving transpositions like `spilt` → `pr-split`, which zoxide's own matcher cannot) and flags the list as approximate |
| Drag a row onto a folder | Move the selection into that folder |
| `Alt`-drag a row onto a folder | Copy the selection into that folder instead of moving |
| `Delete` / `Backspace` (or `Cmd`+`Backspace`) | Move every selected path to the OS Trash. On macOS the trash sound plays once for the whole batch. |
| `Cmd`+`Opt`+`R` (local macOS only) | Reveal the selected entry in Finder (`open -R`). Omitted on remote SSH sessions, where the host is headless and has no Finder. |
| Right-click | Context menu: Cut, Copy, Paste, Rename, Delete (with item count when multi-selected), Reveal in Finder (local macOS only), and on empty space New File / New Folder |

### Search sidebar

| Keys | Action |
|------|--------|
| Type | Live `.gitignore`-aware search across the workspace; refreshes per keystroke (~120 ms debounce, off the UI thread). Capped at 200 hits. Dirty-aware: files open with unsaved edits are searched from their in-memory buffer (and their stale disk copy is skipped), so an unsaved change such as a Rename Symbol is findable before you save, just like VS Code / Zed. |
| Click `Aa`, `ab`, `.*` toggles | Flip case-sensitive / whole-word / regex; re-runs immediately. Active toggles render with a yellow background. |
| `↑` / `↓` + `Enter`, or click a result | Open the file at the matched line |

### Editor: text

| Keys | Action |
|------|--------|
| Arrows, Home, End | Navigate (clears any active selection) |
| `Shift`+arrows / `Shift`+`Home` / `End` / `PageUp` / `PageDown` | Extend the selection by the same motion |
| `PageUp` / `PageDown` (`fn`+`↑` / `fn`+`↓` on Mac) | Scroll exactly one viewport |
| Two-finger horizontal swipe, or drag the bar | In code files, long lines that overflow the text column get a horizontal scrollbar on the editor's bottom row; swipe sideways to pan, or click and drag the thumb. Moving the cursor past either edge pans to follow it, and the scroll position can never strand the buffer off-screen. |
| (Markdown only) soft word-wrap | Markdown files wrap long lines onto the next visual row instead of scrolling sideways (VS Code default for Markdown); no horizontal scrollbar appears. `↑`/`↓` move by visual row, the line number shows once per paragraph, and a single paragraph taller than the pane still scrolls. |
| Any printable char, Enter, Backspace, Delete | Edit (typing or deleting with an active selection replaces it) |
| `Tab` | Indent: with a multi-line selection, indents every line the selection touches one level (empty lines untouched, VS Code-style); otherwise inserts spaces to the next tab stop (4, or 2 in YAML) |
| `Shift`+`Tab` | Outdent: strips one indent level from the current line, or from every line a selection touches; tab-stop aligned (6 spaces → 4, 3 spaces → 0), and a single leading tab counts as one level |
| Mouse drag | Select text; selection stays highlighted until you copy or click elsewhere. Every other occurrence of the selected text lights up in blue (VS Code-style selection highlight), for single-line, non-whitespace selections up to 200 characters |
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
| `Esc` | Clear the current selection, or collapse multi-cursors back to one |
| `F2` | Rename Symbol: LSP rename of the identifier under the cursor across every file it touches (open tabs edit in-memory and stay dirty; closed files are rewritten on disk) |
| `Cmd`+`F2` / `Ctrl`+`F2` | Change All Occurrences: drop a cursor on every textual match of the word in the current file and edit them all at once; type to replace, `Esc` to finish |
| `F12` / `Cmd`/`Option`+click | Go to Definition (jumps via the language server); `Cmd`+`Shift`+click navigates back |
| `Shift`+`F12` | Go to References: finds every project-wide use of the symbol under the cursor, via the language server's `textDocument/references` (declaration included, like VS Code). One use jumps straight there; several open a picker. Shown in the right-click menu for every language whose server implements it (all croft ships do). This is VS Code's real binding for the action |
| `Ctrl`+`Shift`+`F12` | Go to Declaration: jumps to where the symbol is declared, via the language server's `textDocument/declaration`. Shown in the right-click menu only for languages whose server implements it; hidden for TypeScript (vtsls advertises `declarationProvider: false`), exactly as VS Code does. VS Code leaves this unbound, so croft keeps it in the F12 family (it moved off bare `Shift`+`F12` when Go to References claimed that default) |
| `Ctrl`+`F12` | Go to Type Definition: jumps to where the type of the expression under the cursor is defined, via the language server's `textDocument/typeDefinition`. Shown in the right-click menu only for languages whose server implements it (vtsls, basedpyright, rust-analyzer, gopls all do) |
| `Cmd`+`F12` | Go to Implementations: jumps from a trait / interface / abstract method to its concrete implementors, via the language server's `textDocument/implementation` (often many; jumps to the first). Shown in the right-click menu only for languages whose server implements it (rust-analyzer, gopls, vtsls do). Requires `croft setup-iterm2` so iTerm2 forwards the Cmd chord |
| Hover (300 ms dwell) | Show the hover popup for whatever is under the pointer: any diagnostic (error / warning) covering the point is shown first (severity-labelled, errors before warnings), then the language server's type / signature hover below it, divided by a rule, exactly as VS Code stacks marker hovers above content. A squiggle on punctuation (e.g. a syntax error with no identifier) still shows its message; a symbol with no diagnostic shows just the type info |
| Hover a tab (300 ms dwell) | Show a tooltip with the tab's full file path, so two same-named files (e.g. two `app.ts`) can be told apart; diff tabs show both compared paths |
| Right-click | Editor symbol menu: Go to Definition, Go to Declaration, Go to Type Definition, Go to Implementations, Go to References, Rename Symbol, Change All Occurrences |
| `Cmd`+`E` | Toggle native modal (vim) editing for the editor pane (see below) |
| `Ctrl`+`W` / `Cmd`+`W` | Close the active editor tab (no-op on the last tab). The `Cmd` chord reaches croft only after `croft setup-iterm2`: macOS otherwise binds it to iTerm2's File → Close, which closes the session and quits iTerm2 |

### Editor: vim mode (modal editing)

`Cmd`+`E` toggles a native, Rust-implemented modal layer over the editor pane. It is an emulation of the common daily-driver subset, not embedded neovim, so it carries no `nvim` dependency, behaves identically on local and remote, and stays on the input hot path. The toggle is global: `Cmd`+`E` works from any pane and even with no file open, so you can arm vim mode ahead of time. It is a single app-wide state, not bound to one buffer, so once on it stays on as you open and switch between files. When it is on, a coloured mode pill (`NORMAL` blue, `INSERT` green, `VISUAL` purple) and the active `:`/`/` line show in the status bar. While it is off the editor behaves exactly as the tables above describe, and every `Cmd`/`Ctrl` shortcut keeps working in Normal mode because modal editing only claims unmodified keys. Toggling vim mode off also clears any lingering `/` `?` search-match highlights, which is the way to dismiss them. For full vim with your own plugins and `init.lua`, run real `nvim` in the shell pane. The `Cmd`+`E` toggle reaches croft only after `croft setup-iterm2` and an iTerm2 relaunch: macOS otherwise binds the chord to Edit > Find > Use Selection for Find, so the setup step relocates that menu item to `Cmd`+`Opt`+`E` and forwards `Cmd`+`E` as CSI-u, exactly as it does for the other Mac-style Cmd chords.

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
| `/` `?` then `Enter`, `n` `N` | Search forward / back in the buffer, jump to next / previous match; the active match is highlighted in orange and the rest in yellow |
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
| Mouse drag | Select text; selection stays highlighted until you copy or click elsewhere. Selections are pinned to the scrollback content, not the screen, so they stay put when you scroll and can span more rows than the pane is tall. Drag past the top or bottom edge and the viewport auto-scrolls through history, growing the selection as it goes. |
| Mouse wheel | Scroll through 5000 rows of scrollback. In alternate-screen mode (vim / less / htop) the wheel forwards arrow keys so the running app handles it. Any keystroke snaps back to the live bottom. |
| `Cmd+C` / `Ctrl+Shift+c` | Explicit copy of the terminal's current selection (`Cmd+C` reaches croft after `croft setup-iterm2`; native NSPasteboard locally, OSC 52 over SSH) |
| `Cmd+T` / `Ctrl+Shift+t` | Open another terminal next to the current one (works from any pane). `Cmd+T` is the primary chord (reaches croft after `croft setup-iterm2`, which relocates iTerm2's New Tab to `Cmd+Ctrl+T`). Each terminal has its own PTY, scrollback, and selection. |
| `Cmd+W` / `Ctrl+Shift+w` | Close the active terminal (no-op when only one is left; use `Ctrl+J` to hide the pane). `Cmd+W` is the primary chord (reaches croft after `croft setup-iterm2`); plain `Ctrl+W` is left alone so the shell's delete-word-backward keeps working. |
| `Cmd+]` / `Cmd+[` | Cycle to the next / previous terminal in the pane (reaches croft after `croft setup-iterm2`, which relocates iTerm2's Next/Previous Pane to `Cmd+Opt+]` / `Cmd+Opt+[`). Click any terminal to switch focus directly. |

</details>

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
| `⌘V` | left on iTerm2's native Paste (bracketed paste); any legacy croft `⌘V` hex binding is removed | iTerm2 sends a bracketed paste; croft reads the system clipboard and routes it by focus into the editor, or into Search when Search is active. Works identically over SSH, which is why it is not remapped to CSI-u |
| `⌥⌘R` | `\x1b[114;11u` | Reveal the selected Explorer entry in Finder (local macOS only) |
| `⌘B` | `\x1b[98;9u` | Toggle the primary side bar (left pane), matching VS Code |
| `⌘F12` | `\x1b[24;9~` | Go to Implementations (`Cmd+F12` is captured by macOS, so it needs forwarding; the bare-F12 family below does not) |
| `⌃⇧F12` | `\x1b[24;6~` | Go to Declaration (forwarded defensively; iTerm2 already emits this natively, like plain / `Shift` / `Ctrl` F12) |
| `⌘\` | `\x1b[92;9u` | Split the editor into two side-by-side columns |
| `⌥⌘←` / `⌥⌘→` | `\x1b[1;11D` / `\x1b[1;11C` | Focus the left / right editor group while split (modifier byte 11 = Alt+Super, disjoint from the bare `Opt+←/→` word-motion) |

It also moves several iTerm2 / macOS menu shortcuts out of the way, each relocated to an unused alternate chord so the original iTerm2 action stays reachable. Most are relocated to an unused `Cmd`-based chord (typically `Cmd+Opt+<key>`): **Edit → Find → Find...** off `⌘F` (to `⌘⌥F`) and **Find Globally...** off `⌘⇧F` (to `⌘⌥⌃F`), **Shell → Split Vertically / Horizontally with Same Profile** off `⌘D` / `⌘⇧D`, **Edit → Find Next / Find Previous / Jump to Selection**, **File → Print** off `⌘P`, **Edit → Find → Use Selection for Find** off `⌘E`, **Edit → Copy / Cut / Select All / Undo** off `⌘C` / `⌘X` / `⌘A` / `⌘Z`, **Window → Select Tab 1..9** off `⌘1..⌘9`, and the macOS **Help → Show Help Menu** off `⌘⇧/`. A few go elsewhere because `Cmd+Opt+<key>` is taken: **File → Close** off `⌘W` (to `⌘⌃W`, since `⌘⌥W` is already "Close All Panes in Tab"), **Window → New Tab** off `⌘T` (to `⌘⌃T`), and **Shell → Previous / Next Pane** off `⌘[` / `⌘]` (to `⌘⌥[` / `⌘⌥]`). `⌘V` is deliberately left on iTerm2's native Paste. Fully quit iTerm2 with `⌘Q` and reopen it after setup; iTerm2 caches its plist while running.

### 1. Right-click reaches croft

By default iTerm2 shows its own context menu (Copy / Open URL / etc.) on right-click and never forwards it to the running app, so croft's New File / New Folder menu would never trigger.

iTerm2 → Settings (`⌘,`) → search **"right click"** → tick **"Right click reported to apps, does not open menu"**. After that, right-clicking inside croft's tree pane opens croft's menu. No iTerm2 restart needed.

Terminal.app does not expose this toggle, so right-click is iTerm2-only.

### 2. Cmd+S as save (and other Cmd shortcuts)

`Ctrl+S` saves out of the box in any terminal. Getting `Cmd+S` to save in croft on macOS takes one extra step that no terminal app can fix on its own. macOS reserves the Cmd modifier for application menus; both Terminal.app and iTerm2 follow this rule. iTerm2 ≥3.5 supports the kitty keyboard protocol (croft negotiates `\x1b[>3u` on startup), but iTerm2 still does not deliver `Cmd+letter` over CSI u even with **Apps can change how keys are reported** and **Report keys using CSI u** both enabled. Verified empirically.

**`croft setup-iterm2` already does all of this for you** (it installs `⌘S` / `⌘C` / `⌘X` / `⌘Z` / `⌘A` and the rest as CSI-u GlobalKeyMap forwarders), so the manual recipe below is only needed if you choose not to run the setup command.

The manual fix is a one-line key mapping per chord that rewrites `Cmd+letter` to the byte `Ctrl+letter` already sends. Croft's existing tested `Ctrl+S` handler does the rest.

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

The sidebar-jump chords (`⌘⇧E` / `⌘⇧F` / `⌘⇧S` / `⌘⇧D` / `⌘⇧R`) and the rest of croft's Cmd chords are likewise installed by `croft setup-iterm2` globally and into every profile; `⌘V` is left on iTerm2's native Paste (see below).

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

## Termux / Android

croft installs and runs as a native Android binary inside [Termux](https://termux.dev) (`cargo install --git https://codeberg.org/vitali87/croft.git`). One thing works differently from the desktop, and it is automatic, no setup command:

- **The command key.** Android has no Cmd key, so on Termux **`Ctrl` is the command modifier** (VS Code's Linux convention). Every Cmd chord in the tables above is reachable as the same chord with `Ctrl`: `Ctrl+P` quick-open, `Ctrl+\` split editor, `Ctrl+T` new terminal, `Ctrl+]` / `Ctrl+[` cycle terminals, `Ctrl+Opt+←` / `Ctrl+Opt+→` move editor-group focus, and so on. One consequence of full `Ctrl` parity: `Ctrl+\` opens the editor split instead of sending `SIGQUIT` to the shell.

Inline image / PDF / spreadsheet previews and the activity-bar icons do **not** render on mainline Termux: its terminal does not implement the iTerm2 OSC 1337 inline-image protocol (the [termux-app PR](https://github.com/termux/termux-app/pull/2973) adding it is still unmerged), so croft falls back to the metadata-header line instead of emitting OSC 1337, which mainline Termux would print as raw base64 text. If you run a Termux build that does support OSC 1337, set `CROFT_FORCE_INLINE_IMAGES=1` to enable previews.

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
├── gradient.rs          shared orange→green corner gradient: the welcome activity box border and the Black-theme focused-pane border
├── highlight.rs         tree-sitter highlight registry per language
├── icons.rs             Codicon / Devicon / Seti glyphs and per-language colors
├── install_session.rs   streams install-progress events while a remote host builds / installs the croft binary
├── iterm2.rs            iTerm2 plist mutation helpers for fonts and Croft key mappings
├── iterm2_inline.rs     OSC 1337 inline-image baking pipeline (welcome wordmark, image / PDF preview, activity-bar icons incl. the settings gear, SSH empty-state hero)
├── pdf.rs               PDF rasteriser: prefers pdftoppm (poppler), falls back to macOS sips
├── prefs.rs             durable user preferences (color theme) persisted at ~/.config/croft/config.json
├── remote.rs            remote (SSH) target metadata and launch dispatch
├── remote_connect.rs    interactive SSH connect flow (host + password prompt phases) behind the connect dialog
├── session_state.rs     captures open tabs / layout so a self-update re-exec can restore them
├── sheet.rs             CSV / TSV / XLSX / XLS / XLSB / ODS parsing via the csv and calamine crates
├── sysmon.rs            system-metrics sampler loop (CPU / memory / network / disk / temp)
├── theme.rs             IDE color theme (Croft Dark / Croft Black): the background palette driving SetColors + baked-image fills
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
│   ├── config.rs        per-language LSP config (basedpyright, ruff, ty, vtsls, rust-analyzer, gopls)
│   ├── install.rs       croft-managed TypeScript server: lazy background install of vtsls into ~/.croft/servers
│   ├── log_file.rs      LSP stderr / debug log sink at ~/.croft/lsp.log
│   ├── manager.rs       lifecycle: spawn / did_open / did_change / completion / shutdown
│   ├── registry.rs      language detection from file extension and shebang
│   ├── runtime.rs       Tokio runtime owned by the LSP manager
│   └── semantic_cache.rs content-keyed disk cache of semantic-token batches at ~/.croft/sem-cache
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
    ├── scrollbar.rs     shared vertical- and horizontal-scrollbar geometry
    ├── search.rs        sidebar search panel + .gitignore-aware substring walker
    ├── shortcuts.rs     F1 shortcuts modal: every binding grouped by pane, scrollable
    ├── source_control.rs Source Control sidebar widget: branch summary, commit input, change list, commit button, no-repo hero
    ├── system_panel.rs  collapsible SYSTEM metrics panel pinned to the sidebar bottom
    ├── terminal.rs      portable-pty + alacritty_terminal + ratatui integration with selection + scrollback
    └── zoxide_jump.rs   Cmd+Z zoxide jump popup: fuzzy directory jumper that re-roots + cd's the terminal
tests/cli.rs             integration tests for the CLI surface
```

## Goal

A complete VS Code replacement in the terminal: the full IDE experience as a single fast Rust binary. Everything VS Code does, croft will do, without leaving the TUI.

## License

MIT.