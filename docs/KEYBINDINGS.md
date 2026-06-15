# croft keybindings

Every action in croft is reachable from the keyboard. Press `F1` inside croft for the same reference, grouped by pane and scrollable.

On macOS, the `Cmd` chords below only reach croft after you run a one-time setup for your terminal: `croft setup-iterm2` (see [iTerm2 key mappings](#iterm2-key-mappings)) or `croft setup-ghostty` (see [Ghostty key mappings](#ghostty-key-mappings)). On Termux/Android there is no Cmd key, so `Ctrl` is the command modifier and every `Cmd` chord works as the same chord with `Ctrl`. Touch users without a hardware keyboard get every chord through croft's built-in on-screen keyboard: tap the editor, terminal, or Search input to raise it, then tap its one-shot `ctrl` / `alt` keys before a letter (e.g. `ctrl` then `p` opens Quick Open); `⌄` dismisses it.

## Global

| Keys | Action |
|------|--------|
| `Ctrl+s` / `Cmd+s` | Save the open file |
| `Ctrl+q` | Quit |
| `F1` | Open the shortcuts modal |
| `F6` | Cycle focus across panes (tree → editor → terminal → tree) |
| `Ctrl+b` / `Cmd+b` | Toggle the primary side bar (left pane) |
| `Ctrl+j` | Toggle the terminal pane |
| `Ctrl+Shift+j` | Maximize the terminal pane (press again to restore the split) |
| `Cmd+\` | Split the editor into two side-by-side columns; each keeps its own tabs, scroll, and cursor. Closing the last tab in a column collapses the split |
| `Cmd+Opt+←` / `Cmd+Opt+→` | Move focus to the left / right editor group while split (or click a column) |
| `Ctrl+p` / `Cmd+p` | Quick Open: fuzzy-search workspace files and jump to one (auto-reveals it in the Explorer) |
| `Ctrl+Shift+p` / `Cmd+Shift+p` | Command Palette: fuzzy-search every named command and run it, with its keybinding shown alongside |
| `Ctrl+Shift+e` / `Cmd+Shift+e` | Jump to the Explorer sidebar |
| `Ctrl+Shift+f` / `Cmd+Shift+f` | Jump to the Search sidebar |
| `Ctrl+Shift+s` / `Cmd+Shift+s` | Jump to Source Control |
| `Ctrl+Shift+d` / `Cmd+Shift+d` | Jump to Run and Debug |
| `Ctrl+Shift+r` / `Cmd+Shift+r` | Jump to Remote (SSH) |
| `Ctrl+Shift+l` / `Cmd+Shift+l` | While on a remote, disconnect and return to the local croft at the directory you connected from |
| Click activity-bar icons | Switch between Explorer, Search, Source Control, Run and Debug, and Remote |
| Click the settings gear | Open settings → Color Theme picker (Croft Dark Blue / Croft Black) |
| Drag a seam | Resize the sidebar, the split between editor columns, or the editor/terminal split |
| Mouse wheel | Scroll the pane under the pointer |

## Command Palette

`Cmd`/`Ctrl`+`Shift`+`P` opens the Command Palette: type to fuzzy-search every named command, `↑`/`↓` to move, `Enter` to run, `Esc` to close. Commands that have a dedicated chord show it on the right. Beyond the chord-bound actions above, these editor commands are reachable only here:

| Command | Action |
|---------|--------|
| Join Lines | Collapse the selected lines (or the current line with the next) into one, single-spaced |
| Transform to Uppercase / Lowercase / Title Case | Re-case the selection, or the word under the cursor when nothing is selected |
| Sort Lines Ascending / Descending | Sort the selected lines (or the whole file when nothing is selected) lexicographically |
| Trim Trailing Whitespace | Strip trailing spaces and tabs from every line |
| Debug: Attach to Python Process | Pick a running CPython 3.14+ process and drop a `pdb` REPL into it (PEP 768 `sys.remote_exec`); the debugger runs in a croft terminal, elevating with `sudo` when the OS requires it |

## Explorer (file tree)

| Keys | Action |
|------|--------|
| `↑` / `↓` | Move selection |
| `Enter` or `→` | Open a file; expand or collapse a folder |
| Double-click a file | Pin its tab (a single click opens the file in the replaceable preview tab) |
| `←` | Collapse a folder |
| `Shift`+`↑` / `↓` / `PageUp` / `PageDown` / `Home` / `End` | Extend multi-selection from the anchor row |
| `Shift`+click | Extend multi-selection across a range |
| `Alt`/`Option`+click or `Ctrl`+click | Toggle a single row in or out of the selection |
| `Ctrl`+`A` / `Cmd`+`A` | Select every visible row |
| `Esc` | Clear the multi-selection |
| `Ctrl`+`C` / `Cmd`+`C` | Copy selected paths to the explorer clipboard |
| `Ctrl`+`X` / `Cmd`+`X` | Cut selected paths |
| `Ctrl`+`V` / `Cmd`+`V` | Paste into the focused folder (move on Cut, copy on Copy) |
| `Cmd`+`Z` | Jump to a directory via zoxide: a fuzzy popup over your frecency-ranked dirs, then re-roots the workspace and `cd`s the terminal. Shares one database with the shell's `j` command; croft installs zoxide and wires the shell hook on first launch if needed |
| Drag a row onto a folder | Move the selection into it (`Alt`-drag to copy instead) |
| `Delete` / `Backspace` / `Cmd`+`Backspace` | Move every selected path to the OS Trash |
| `Cmd`+`Opt`+`R` (local macOS only) | Reveal the selected entry in Finder |
| Right-click | Context menu: Cut, Copy, Paste, Rename, Delete, Reveal in Finder (local macOS), and New File / New Folder on empty space |

## Search sidebar

| Keys | Action |
|------|--------|
| Type | Live `.gitignore`-aware workspace search, per keystroke (off the UI thread, capped at 200 hits). Unsaved buffers are searched in-memory, so unsaved edits are findable before you save |
| Click `Aa` / `ab` / `.*` | Toggle case-sensitive / whole-word / regex; active toggles show a yellow background |
| `↑` / `↓` + `Enter`, or click a result | Open the file at the matched line in the replaceable preview tab |
| Double-click a result | Pin its tab, so moving to the next result opens beside it instead of replacing it |

## Editor: text

| Keys | Action |
|------|--------|
| Arrows, Home, End | Navigate (clears any selection) |
| `Shift`+arrows / `Home` / `End` / `PageUp` / `PageDown` | Extend the selection by the same motion |
| `PageUp` / `PageDown` (`fn`+`↑` / `fn`+`↓` on Mac) | Scroll one viewport |
| Two-finger horizontal swipe, or drag the bar | Pan long lines in code files; the cursor pans the view when it passes either edge |
| Markdown soft-wrap | Markdown files wrap long lines onto the next visual row (no horizontal scrollbar); `↑`/`↓` move by visual row |
| Printable char, Enter, Backspace, Delete | Edit (typing or deleting with a selection replaces it) |
| `Tab` | Indent: a multi-line selection indents every touched line one level; otherwise inserts to the next tab stop (4, or 2 in YAML) |
| `Shift`+`Tab` | Outdent one level, tab-stop aligned, for the current line or every line a selection touches |
| `Alt`+`↑` / `Alt`+`↓` | Move the current line (or selected block) up / down, carrying the cursor and selection |
| `Shift`+`Alt`+`↑` / `↓` | Copy the current line (or selected block) up / down |
| `Cmd`/`Ctrl`+`Opt`+`↑` / `↓` | Add a cursor above / below the current one (multi-cursor) |
| `Cmd`+`/` / `Ctrl`+`/` | Toggle line comment for the current line or every line the selection touches (language-aware; comments at the block's common indent) |
| `Shift`+`Alt`+`A` | Toggle block comment around the selection (languages with a block comment) |
| `Alt`+`Z` | Toggle soft word wrap for this file (overrides the per-language default until the file is reopened) |
| Mouse drag | Select text; every other occurrence of a single-line selection highlights in blue |
| `Ctrl`+`C` / `Cmd`+`C` | Copy the selection to the system clipboard |
| `Ctrl`+`X` / `Cmd`+`X` | Cut the selection |
| `Ctrl`+`V` / `Cmd`+`V` | Paste at the cursor (replaces any selection) |
| `Ctrl`+`Z` / `Cmd`+`Z` | Undo (typing bursts coalesce; backspace, paste, cut, replace are each one step) |
| `Cmd`+`A` | Select the entire buffer |
| `Ctrl`+`f` / `Cmd`+`f` | Inline Find bar: pre-filled from the selection or word under the cursor; active match in orange, the rest in yellow; `Enter`/`F3` forward, `Shift+Enter`/`Shift+F3` back, `Esc` closes |
| `Ctrl`+`A` / `Ctrl`+`E` | Move to start / end of line |
| `Ctrl`+`K` / `Ctrl`+`U` | Kill to end / start of line (yanks to clipboard) |
| `Cmd`+`o` / `Cmd`+`Shift`+`O` | Open a new line below / above, inheriting indent |
| `Cmd`+`g` `g` | Go to the top of the file (`Cmd`+`N` `Cmd`+`g` `g` for line N) |
| `Cmd`+`Shift`+`G` | Go to the bottom of the file (with a leading count, that line) |
| `Cmd`+`d` `d` | Delete the current line (`Cmd`+`N` `Cmd`+`d` `d` for N lines; yanks to clipboard) |
| `Cmd`+`y` `y` | Yank the current line (`Cmd`+`N` `Cmd`+`y` `y` for N lines) |
| `Esc` | Clear the selection, or collapse multi-cursors back to one |
| `F2` | Rename Symbol across every file it touches (open tabs edit in-memory and stay dirty) |
| `Cmd`+`F2` / `Ctrl`+`F2` | Change All Occurrences in the current file; type to replace, `Esc` to finish |
| `F12` / `Cmd`/`Option`+click | Go to Definition (`Cmd`+`Shift`+click navigates back) |
| `Shift`+`F12` | Go to References (project-wide; one use jumps, several open a picker) |
| `Ctrl`+`Shift`+`F12` | Go to Declaration (where the server implements it; hidden for TypeScript) |
| `Ctrl`+`F12` | Go to Type Definition |
| `Cmd`+`F12` | Go to Implementations (concrete implementors of a trait / interface) |
| Hover or click/tap (300 ms rest) | Hover popup: any diagnostic over the point first, then type / signature info. A click or tap arms the same dwell, so touch screens (Termux) get the popup by tapping an identifier and resting; releasing the press keeps it open |
| Hover a tab (300 ms dwell) | Tooltip with the tab's full path (tells two same-named files apart) |
| Right-click | Editor symbol menu: the Go to / Rename / Change All actions above |
| `Cmd`+`E` | Toggle native modal (vim) editing (see below) |
| `Ctrl`+`W` / `Cmd`+`W` | Close the active editor tab (no-op on the last tab) |

## Editor: vim mode (modal editing)

`Cmd`+`E` toggles a native, Rust-implemented modal layer over the editor. It is an emulation of the common daily-driver subset, not embedded neovim, so it carries no `nvim` dependency and behaves identically local and remote. The toggle is global and app-wide: it works from any pane and with no file open, and stays on as you switch files. A coloured mode pill (`NORMAL` blue, `INSERT` green, `VISUAL` purple) and the active `:`/`/` line show in the status bar. While off, the editor behaves exactly as the tables above describe, and `Cmd`/`Ctrl` shortcuts keep working in Normal mode (modal editing only claims unmodified keys). For full vim with your own plugins, run `nvim` in the shell pane.

| Keys | Action |
|------|--------|
| `i` `a` `I` `A` `o` `O` | Enter Insert mode (at cursor, after, first non-blank, end of line, open below, open above) |
| `Esc` | Leave Insert/Visual; clear a pending operator or count |
| `h` `j` `k` `l`, arrows | Move by one cell |
| `w` `b` `e` | Word forward / back / end |
| `0` `^` `$` | Line start / first non-blank / line end |
| `gg` `G` `{n}G` | File start / file end / absolute line |
| `f`/`t`/`F`/`T` `{char}`, `;` `,` | Find char on the line, repeat / repeat-reversed |
| `{n}` prefix | Count for the next motion or operator (`3j`, `2dw`, `5G`) |
| `x` | Delete `{count}` chars under the cursor |
| `d` `y` `c` + motion / text object | Delete / yank / change over a motion (`dw`, `d$`, `dfx`) or text object (`diw`, `ciw`, `daw`) |
| `dd` `yy` `cc` | Linewise delete / yank / change |
| `p` `P` | Paste after / before |
| `u` | Undo |
| `v` `V` | Charwise / linewise Visual; a motion extends, `d` `y` `c` operate |
| `/` `?` then `Enter`, `n` `N` | Search forward / back, jump to next / previous match |
| `:w` `:q` `:wq` `:x` `:q!` `:qa`, `:{n}` | Write, close tab, write-and-close, quit-all, or jump to line n |

When vim mode is on it supersedes the always-on `Cmd`+`d` `d` / `Cmd`+`g` `g` chords; turn it off to get those back.

## Editor: previews

Image tabs (`.png`, `.jpg`, `.jpeg`, `.gif`, `.bmp`, `.webp`) are read-only; every keystroke is swallowed.

**PDF (`.pdf`)**

| Keys | Action |
|------|--------|
| `→` / `Page Down` / `Space` | Next page |
| `←` / `Page Up` | Previous page |
| `Home` / `End` | First / last page |

**Spreadsheet (`.csv`, `.tsv`, `.xlsx`, `.xls`, `.xlsb`, `.ods`)**

| Keys | Action |
|------|--------|
| `↑` / `↓` / `←` / `→` | Pan one row / column |
| `PageUp` / `PageDown` | Pan a full viewport vertically |
| `Home` | Jump to row 1, column 1 |
| `End` | Jump to the last visible page |
| `Tab` / `Shift+Tab` | Switch worksheet |

## Run & Debug

Real breakpoint debugging over the Debug Adapter Protocol. Python is the verified adapter (debugpy, CPython 3.14+; croft provisions a private `~/.croft/debug-venv` with debugpy on first use, with no fallback to older interpreters). JavaScript / TypeScript run under Node via **vscode-js-debug** (the same engine VS Code, Zed and nvim use): croft auto-downloads the pinned `js-debug` server to `~/.croft/js-debug` on first use and talks to it over TCP, spawning the parent + child sessions js-debug requires; `node` must be on `PATH`, and TypeScript binds through source maps. Rust / C / C++ route to `lldb-dap` (binary built on launch; breakpoint binding additionally needs a permitted macOS `debugserver`). The lldb adapter is resolved from the Xcode Command Line Tools on macOS or from `PATH` elsewhere, matching the versioned names LLVM ships (`lldb-dap`, `lldb-dap-18`, legacy `lldb-vscode`); on Linux install it with the LLVM package (e.g. `apt install lldb`).

| Keys | Action |
|------|--------|
| `F5` | Start debugging the active file, or resume when paused at a breakpoint |
| `Shift+F5` | Stop the debug session |
| `F6` | Pause (interrupt) a running program |
| `F9` | Toggle a breakpoint on the cursor's line (a red dot in the gutter); pushed live when a session is running |
| `F10` | Step over |
| `F11` / `Shift+F11` | Step into / out |

Right-clicking the editor **gutter** (the glyph margin / line-number column) opens a breakpoint menu on the clicked line, mirroring VS Code's glyph-margin menu: **Add Breakpoint** / **Remove Breakpoint** and **Add Conditional Breakpoint…** / **Edit Condition…** (the cursor does not move).

When paused, the Run and Debug panel shows the **call stack** (click a frame to inspect it) and an expandable **variables** tree, plus a **debug console** of program output with a `❯` **REPL prompt** that evaluates expressions in the selected frame. When the session ends, the console output stays on screen (it isn't wiped back to the Run button), and if the program exited without ever hitting a breakpoint you set, the panel says so rather than just "Debug session ended" — the usual sign you ran a library module whose breakpointed code is never called, rather than its entry point. Hovering a variable in the editor shows its current value. The paused line is marked with a yellow `▶` in the gutter; a breakpoint is a red `●`, a conditional breakpoint a red `◆`, and a breakpoint the adapter could not bind a hollow `○`.

Also in the Command Palette: Start / Stop / Pause / Restart Debugging, Toggle Breakpoint, Add Conditional Breakpoint, Step Over, Toggle Break on Raised Exceptions (uncaught always breaks), and Attach to Python Process.

## Terminal

| Keys | Action |
|------|--------|
| Any key | Forwarded to the shell PTY (arrows, `Ctrl+letter`, `Alt+x`, function keys translated to VT escapes) |
| Mouse drag | Select text, pinned to the scrollback content; drag past an edge to auto-scroll through history |
| Mouse wheel | Scroll 5000 rows of scrollback (forwards arrows to full-screen apps like vim/less/htop); any keystroke snaps to the live bottom |
| `Cmd+C` / `Ctrl+Shift+c` | Copy the terminal's current selection |
| `Cmd+T` / `Ctrl+Shift+t` | Open another terminal beside the current one (each has its own PTY, scrollback, selection) |
| `Cmd+W` / `Ctrl+Shift+w` | Close the active terminal (no-op when one is left; `Ctrl+J` hides the pane) |
| `Cmd+]` / `Cmd+[` | Cycle to the next / previous terminal (or click one to focus it) |

## iTerm2 key mappings

`croft setup-iterm2` installs these `Cmd` chords as CSI-u forwarders (macOS otherwise reserves `Cmd` for menus) and relocates the conflicting iTerm2 / macOS menu shortcuts to unused alternates so their original actions stay reachable. Fully quit iTerm2 (`⌘Q`) and reopen after running it.

| iTerm2 keystroke | What croft does |
|------------------|-----------------|
| `⌘P` | Quick Open |
| `⌘⇧P` | Command Palette |
| `⌘F` | In-editor Find |
| `⌘/` | Toggle line comment |
| `⌥⌘↑` / `⌥⌘↓` | Add a cursor above / below (multi-cursor) |
| `⌘⇧E` / `⌘⇧F` / `⌘⇧S` / `⌘⇧D` / `⌘⇧R` | Jump to Explorer / Search / Source Control / Run and Debug / Remote |
| `⌘⇧L` | Disconnect a remote session |
| `⌘⇧N` | Explorer "New folder" prompt |
| `⌃⇧J` | Maximize the terminal pane |
| `⌥⌘R` | Reveal in Finder (local macOS only) |
| `⌘B` | Toggle the primary side bar |
| `⌘\` | Split the editor |
| `⌥⌘←` / `⌥⌘→` | Focus the left / right editor group |
| `⌘F12` | Go to Implementations |
| `⌃⇧F12` | Go to Declaration |
| `⌘S` / `⌘C` / `⌘X` / `⌘Z` / `⌘A` | Save / Copy / Cut / Undo / Select All |
| `⌘T` / `⌘W` / `⌘]` / `⌘[` | New terminal / close terminal / cycle terminals |

`⌘V` is deliberately left on iTerm2's native Paste; croft reads the system clipboard and routes it by focus, so paste works identically over SSH. If you skip the setup command, the zero-setup `Ctrl`-based chords above still work, and you can map individual `Cmd` chords by hand in iTerm2 → Settings → Profiles → Keys → Key Mappings (e.g. `⌘S` → Send Hex Code `0x13`).

## Ghostty key mappings

Ghostty resolves its own keybinds (`new_tab`, `goto_tab`, ...) before it hands a key to croft, so by default `⌘T` opens a Ghostty tab and `⌘1`..`⌘9` switch Ghostty tabs instead of reaching croft. `croft setup-ghostty` adds a managed `keybind` block to your Ghostty config (`~/.config/ghostty/config`, or `~/Library/Application Support/com.mitchellh.ghostty/config`) that re-emits every croft chord as the same CSI-u sequence iTerm2 forwards, via Ghostty's `csi:` action. After running it, reload the config (`⌘⇧,`) or restart Ghostty.

The chord set is identical to the [iTerm2 key mappings](#iterm2-key-mappings) above (`⌘T` / `⌘W` / `⌘[` / `⌘]`, `⌘1`..`⌘9` / `⌘0`, the editor / Explorer / Source Control chords, the `⌘F12` family, and so on), so croft behaves the same under both terminals. `⌘V` is left on Ghostty's native paste for the same reason it is under iTerm2. Only the block between croft's marker comments is rewritten on each run; the rest of your Ghostty config is preserved.
