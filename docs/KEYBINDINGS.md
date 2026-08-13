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
| `Opt+Ctrl+b` / `Opt+Cmd+b` | Toggle the secondary side bar (the active file's Outline, on the edge opposite the primary side bar) |
| `Ctrl+j` | Toggle the terminal pane |
| `Ctrl+Shift+j` | Maximize the terminal pane (press again to restore the split) |
| `Cmd+\` | Split the editor into two side-by-side columns; each keeps its own tabs, scroll, and cursor. Closing the last tab in a column collapses the split |
| `Cmd+Opt+←` / `Cmd+Opt+→` | Move focus to the left / right editor group while split (or click a column) |
| `Ctrl+p` / `Cmd+p` | Quick Open: fuzzy-search workspace files and jump to one (auto-reveals it in the Explorer) |
| `Ctrl+p` / `Cmd+p`, then `#` | Go to Symbol in Workspace: the query goes to every running language server as a `workspace/symbol` search; Enter opens the picked symbol's file at its definition (also Command Palette "Go to Symbol in Workspace") |
| `Ctrl+Shift+p` / `Cmd+Shift+p` | Command Palette: fuzzy-search every named command and run it, with its keybinding shown alongside |
| `Ctrl+Shift+e` / `Cmd+Shift+e` | Jump to the Explorer sidebar |
| `Ctrl+Shift+f` / `Cmd+Shift+f` | Jump to the Search sidebar |
| `Ctrl+Shift+s` / `Cmd+Shift+s` | Jump to Source Control |
| `Ctrl+Shift+b` / `Cmd+Shift+b` | Run the project's build task in a named terminal pane, auto-detected from its manifests (`.vscode/tasks.json`, Makefile, justfile, package.json, Cargo.toml, pyproject.toml). "Tasks: Run Task" and "Tasks: Rerun Last Task" live in the Command Palette |
| `Ctrl+Shift+d` / `Cmd+Shift+d` | Jump to Run and Debug |
| `Ctrl+Shift+r` / `Cmd+Shift+r` | Jump to Remote (SSH) |
| `Ctrl+Shift+x` / `Cmd+Shift+x` | Jump to Extensions |
| `Ctrl+Shift+l` / `Cmd+Shift+l` | While on a remote, disconnect and return to the local croft at the directory you connected from |
| Click activity-bar icons | Switch between Explorer, Search, Source Control, Run and Debug, Remote, Extensions, and Testing |
| Click the settings gear | Open settings → Color Theme picker (Croft Black / Croft Dark Blue plus ten editor-inspired dark themes: One Dark Pro, Dracula, Monokai, Nord, Gruvbox Dark, Tokyo Night, Catppuccin Mocha, Solarized Dark, GitHub Dark, Darcula) or Customize Layout |
| Click the layout icons (top-right of the editor / welcome) | Toggle the primary side bar, toggle the panel, or open the **Customize Layout** popup |
| Drag a seam | Resize the sidebar, the split between editor columns, or the editor/terminal split |

### `Cmd+K` chords

`Cmd`/`Ctrl`+`K` is a leader (VS Code's two-key model): press it, then a second key within 1.5s. Pressing anything that completes no chord cancels the leader and keeps its normal meaning.

| Keys | Action |
|------|--------|
| `Cmd+K` `Cmd+T` | Open the Color Theme picker |
| `Cmd+K` `A` | Session: Participants (who is attached to this persistent session); `Enter` on a row grants/revokes write control or disconnects them |
| `Cmd+K` `X` | Collab: Cancel AI Stream — stop a `croft pair` collaborator streaming into a shared file; the streamed text is reverted (same action as clicking the orange `■` stop button in the editor gutter while a stream is live) |
| `Cmd+K` `Q` | Navigator: ask the resident AI pair programmer about the caret line, or the selected lines when a selection is active (opens the instruction box; also on the gutter and body right-click menus) |
| `Cmd+K` `Y` | Navigator: yield the turn — it reviews the active file comment-only; its remarks land as orange `◆` note diamonds in the gutter and in the Navigator OUTPUT channel |
| `F4` | Focus the active file's next navigator comment box (wraps, jumping the caret to its line) |
| `Shift+F4` | Ignore the focused navigator comment box (or the next one from the caret) |
| (box focused) type / `Backspace` / `←` `→` | Edit the box's reply draft (the buffer is untouched) |
| (box focused) `Enter` | Send the reply to the navigator (a comment-only turn) |
| (box focused) `Esc` | Leave the box; the keyboard returns to the buffer |
| `Cmd+K` `→` | Close the editor tabs to the right of the active one |
| `Cmd+K` `S` | Select the active file as the compare anchor |
| `Cmd+K` `C` | Diff the active file against the compare anchor |
| `Cmd+K` `W` | Close all editor tabs |
| `Cmd+K` `U` | Close all saved (non-dirty) editor tabs, keeping unsaved ones |
| `Cmd+K` `E` | Reveal the active file in the Explorer tree (expand parents, select, focus) |
| `Cmd+K` `O` | Copy into New Window: open the active file in a new window of your terminal, focused on just the file (Explorer + terminal hidden; the current window is untouched; Ghostty / iTerm2 / Terminal; macOS only) |
| `Cmd+K` `Shift+O` | Move into New Window: same, and close the file's tab here (macOS only) |
| `Cmd+K` `Z` | Toggle Zen Mode: hide the activity bar, both side bars, the panel, and the status bar; press again to restore exactly what was shown before |
| `Cmd+K` `B` | Show the Testing view (beaker icon); it discovers tests on first open (`cargo test` for Rust, `pytest` for Python). In the view: Enter runs all tests, `r` re-discovers, click a test's play/status glyph to run just it, click its name to jump to its source, click a suite header's play glyph to run the whole suite, ↑/↓ scroll or drag the scrollbar |
| `Cmd+K` `Enter` | Run the test the editor caret sits in (also in the palette as "Testing: Run Test at Cursor") |
| `Cmd+K` `Shift+Enter` | Debug the test the editor caret sits in: pytest runs as a debugpy module launch under the project's venv, a cargo test binary launches under lldb-dap with the test name as its filter ("Testing: Debug Test at Cursor"); Alt+click a gutter ▷ does the same for that test |
| `Cmd+K` `P` | Pin / unpin the active editor tab (moved off `Cmd+K` `Shift+Enter`, which now debugs tests) |
| `Cmd+K` `Shift+P` | Keep the active preview tab open (promote the italic preview to a real tab; moved off `Cmd+K` `Enter`, which now runs tests) |
| `Cmd+K` `H` | Show incoming calls: a picker of everyone calling the symbol at the caret (LSP call hierarchy, one level per invocation; pick a caller and invoke again to walk up) |
| `Cmd+K` `Shift+H` | Show outgoing calls: everything the function at the caret calls, each entry jumping to the callee's definition |
| `Cmd+K` `M` | Maximize the active terminal pane across the panel width (the other terminals move to a right-edge rail); press again to restore the even split |
| `Cmd+K` `F` | Toggle Format on Save: when on, `Cmd+S` reformats through the language server before writing (also in the Command Palette) |
| `Cmd+K` `Cmd+L` | Toggle the code fold at the cursor: collapse the enclosing indented block (function, loop, struct) to its header line, or re-expand it |
| `Cmd+K` `Cmd+0` | Fold All: collapse every foldable block in the buffer |
| `Cmd+K` `Cmd+J` | Unfold All: expand every collapsed block |
| Mouse wheel | Scroll the pane under the pointer |

### Customize Layout

Click the **⛶** icon at the top-right of the editor (or the welcome screen), or the settings gear → **Customize Layout**. The popup mirrors VS Code's title-bar layout controls and stays open while you flip several toggles:

| Group | Options |
|-------|---------|
| Visibility | Activity Bar, Primary Side Bar (`Cmd`/`Ctrl`+`B`), Secondary Side Bar (`Opt`+`Cmd`/`Ctrl`+`B`), Panel (`Ctrl`+`J`), Status Bar, Minimap (`Opt`+`Cmd`/`Ctrl`+`M`) |
| Primary Side Bar Position | Left / Right (moves the activity bar with it) |
| Panel Alignment | Left / Center / Right / Justify (Justify spans the full width under the side bar) |
| Quick Input Position | Top / Center (where Command Palette / Go to File appears) |
| Zen Mode | `Cmd`/`Ctrl`+`K` `Z` |

Every choice except the side-bar / panel visibility persists across launches in `~/.config/croft/config.json`.

## Command Palette

`Cmd`/`Ctrl`+`Shift`+`P` opens the Command Palette: type to fuzzy-search every named command, `↑`/`↓` to move, `Enter` to run, `Esc` to close. Every command shows its chord on the right, and every command now carries one — the palette is a second way to reach them and a discovery surface for their accelerators. The feature commands that previously had none:

| Command | Chord | Action |
|---------|-------|--------|
| Debug: Restart | `Shift`+`Cmd`+`F5` | Restart the active debug session |
| Debug: Add Conditional Breakpoint | `Shift`+`F9` | Add or edit a conditional breakpoint at the cursor |
| Debug: Add Logpoint | `Shift`+`Alt`+`F9` | Add or edit a logpoint at the cursor: the adapter prints the message (`{expr}` interpolates) instead of pausing |
| Debug: Toggle Break on Raised Exceptions | `Alt`+`F9` | Break on raised (not just uncaught) exceptions |
| Debug: Attach to Python Process | `Ctrl`+`F5` | Pick a running CPython 3.14+ process and drop a `pdb` REPL into it (PEP 768 `sys.remote_exec`); the debugger runs in a croft terminal, elevating with `sudo` when the OS requires it |
| Preferences: Color Theme | `Cmd`+`K` `Cmd`+`T` | Pick the active color theme (also via the settings gear) |
| Session: Participants | `Cmd`+`K` `A` | List who is attached to this multiplayer session (docs/MULTIPLAYER.md); pick a participant to grant/revoke write control or disconnect them. The status bar shows an "N attached" badge whenever someone else is on |

**Terminal: Search & Replace from Last grep/rg** (palette-only) reads the last `grep`/`rg`/`git grep` command run in the focused terminal, seeds the Search sidebar with its pattern and matching flags (`-i`, `-w`, `-F`/`-E`, `-g`), and runs it. The terminal search becomes the Search panel's results list, so you can replace across every match at once (`:cdo`-style) with the replace-all it already has.

## Settings, custom keybindings & snippets

All three live under `~/.config/croft/` (XDG-resolved, so the same paths on macOS and Linux) and are reachable from the Command Palette. Opening one that doesn't exist yet seeds it with a working example.

| Command | Opens | Notes |
|---------|-------|-------|
| Preferences: Open Settings | a searchable settings hub | Fuzzy-search the toggleable settings; `Enter` flips a toggle and keeps the hub open. Also routes to the Color Theme picker and the JSON files below |
| Preferences: Open Settings (JSON) | `config.json` | The full preferences document; edits apply on the next launch |
| Preferences: Open Keyboard Shortcuts (JSON) | `keybindings.json` | Rebind any palette command; **applies on save** |
| Preferences: Configure User Snippets | `snippets.json` | Define snippets; **applies on save** |

**Custom keybindings.** `keybindings.json` is a JSON (with `//` comments) array of `{ "key": …, "command": … }`. The `key` is a chord like `ctrl+shift+p`, `cmd+,`, `alt+up`, or `f2`; modifiers are `ctrl`, `alt`/`opt`, `shift`, `cmd`/`super`, and `mod` (Cmd on macOS, Ctrl elsewhere). The `command` is a palette command id (see the ids in `Preferences: Open Settings`, e.g. `save_file`, `quick_open`, `toggle_terminal`). A bound chord wins over the built-in default for the same chord. Bindings apply while any pane except the terminal is focused, and only to chords that carry a modifier or are function keys, so plain typing and the terminal's control keys are never shadowed. In iTerm2, reserved `Cmd` chords must be forwarded first (`croft setup-iterm2`); `Ctrl`/`Alt`/function-key bindings always reach croft, and Ghostty forwards everything after `croft setup-ghostty`.

**User snippets.** `snippets.json` mirrors VS Code's global snippets file: an object keyed by a name, each with a `prefix`, a `body` (a string or an array of lines), and an optional `scope` (comma-separated language ids; omit for every language). Type a snippet's prefix and press `Tab` to expand it, or pick it from the completion popup (it appears there alongside language-server suggestions, accepted with `Enter`/`Tab`). The body uses VS Code tab-stop syntax: `$1`, `$2`, … are stops visited in order with `Tab`, `$0` is the final caret, and `${1:name}` seeds a stop with selected placeholder text. Continuation lines are re-indented to the caret. Language-server completions that arrive as snippets (rust-analyzer's `println!` and the like) expand the same way.

## Explorer (file tree)

| Keys | Action |
|------|--------|
| `↑` / `↓` | Move selection |
| `Enter` | Open a file and move the keyboard into the editor, so the new tab's own navigation keys work straight away (VS Code parity); on a folder, expand or collapse it |
| `→` | Preview a file without leaving the tree, so `↑`/`↓` keep walking the list; on a folder, expand it |
| Click a file | Open it in the replaceable preview tab and move the keyboard into the editor, so the tab's own navigation keys (PDF pages, spreadsheet rows) work at once; double-click pins the tab. Clicking a folder just expands it and keeps focus in the tree |
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
| `Delete` / `Backspace` / `Cmd`+`Backspace` | Move every selected path to the OS Trash (after a confirmation popup — `Enter` to trash, `Esc` to keep) |
| `Cmd`+`Opt`+`R` (local macOS only) | Reveal the selected entry in Finder |
| Right-click | Context menu: Cut, Copy, Paste, Rename, Delete, Reveal in Finder (local macOS), and New File / New Folder on empty space |
| Click the Explorer root-folder icons | New File, New Folder, Refresh Explorer, and Collapse Folders, right-aligned on the root folder row and shown only while the Explorer is focused, mirroring VS Code's workspace-folder actions (New File / New Folder also on `Cmd+F` / `Cmd+Shift+N`) |
| Click the `⋯` button on the EXPLORER title line | Open the "Views and More Actions" menu: toggle which sub-views stack in the Explorer (Open Editors, Folders, Outline, Timeline, and a language-aware Dependencies view that only appears when the workspace root has a recognized manifest), each with a checkmark when shown. Choices persist across launches |
| Click a row in OPEN EDITORS | Activate that editor's tab (dirty tabs show a dot, the active tab is highlighted) |
| Click a commit in TIMELINE | Open that commit's diff for the active file in a read-only editor tab |
| Click a local snapshot in TIMELINE | croft snapshots every save into local history; the TIMELINE lists those snapshots alongside git commits. Click one to diff it against the working file |
| Command Palette: `Local History: Restore Snapshot` | Write the snapshot shown in the open TIMELINE diff back over the working file |

## Search sidebar

| Keys | Action |
|------|--------|
| Type | Live `.gitignore`-aware workspace search, per keystroke (off the UI thread; every match is returned, no cap). Unsaved buffers are searched in-memory, so unsaved edits are findable before you save |
| Click `Aa` / `ab` / `.*` (inset at the right of the search field) | Toggle case-sensitive / whole-word / regex; active toggles show an accent chip |
| Click the header refresh icon | Re-run the current query |
| Click the header clear icon | Clear the search query and results |
| Click the left chevron (`▸`/`▾`) | Expand / collapse the Replace row |
| Type in Replace, then `Enter` or click the replace-all icon | Replace every match across all result files with the Replace text (regex mode honours `$1` capture references); the search re-runs afterward |
| Click the `...` icon | Expand / collapse the "files to include" and "files to exclude" glob inputs |
| Type globs into include / exclude | Restrict the search to / from matching files (comma-separated, VS Code style; a bare `*.rs` matches at any depth). Editing re-runs the search live |
| `Tab` | Cycle focus through the visible inputs (search → replace → include → exclude) |
| `↑` / `↓` + `Enter`, or click a result | Open the file at the matched line in the replaceable preview tab |
| Double-click a result | Pin its tab, so moving to the next result opens beside it instead of replacing it |

## Source Control sidebar

| Keys | Action |
|------|--------|
| Type in the message box | Edit the commit message (the box scrolls horizontally when the message outgrows it) |
| `Enter` | Commit all tracked changes with the message |
| Click ✓ Commit | Same as `Enter` |
| Click a change row | Open that file's diff against HEAD in a read-only editor tab |
| `S` in a diff tab | Stage only the change hunk under the cursor (click a row or `F7` to pick the hunk) |
| `U` in a diff tab | Unstage only the change hunk under the cursor |
| `R` in a diff tab | Revert only the change hunk under the cursor after a `Y`/`N` confirm modal |
| `F7` / `Shift`+`F7` in a diff tab | Jump to the next / previous change hunk |
| Command Palette: `Git: Toggle Inline Blame` | Show/hide the GitLens-style current-line blame annotation (author, age, summary trailing the cursor's line; on by default, persisted) |
| `Cmd`+`A` / `Ctrl`+`A`, then `Cmd`+`S` / `Ctrl`+`S` | Select every change, then stage the selection |
| Click `+` on a selected unstaged row | Stage that file |
| Click `↶` on a selected unstaged row | Discard that file (confirms first; deletes untracked files) |
| Click `−` on a selected staged row | Unstage that file |
| Click the branch name | Open the Checkout / Create Branch picker: type to filter branches, `↑`/`↓` to navigate, `Enter` to switch — or type a new name and `Enter` to create and switch to it |
| Click the `▾` caret next to Commit | Open the quick actions menu: Commit & Push, Push, Pull, Sync (Pull, Push), Checkout / Create Branch, Stash, Pop Stash, View Staged Changes, View Changes vs previous, View Changes vs `<default>` |
| Click the `⋯` icon in the header | Open the full Source Control actions menu (VS Code's title menu) with fly-out submenus: Pull · Push · Clone · Checkout to · Fetch · **Commit ›** (Commit, Commit Staged, Commit All, Amend, Commit & Push, Commit & Sync) · **Changes ›** (Stage All, Unstage All, Discard All) · **Pull, Push ›** (Sync, Pull Rebase, Push to, Push Force, Publish Branch) · **Branch ›** (Create, Create from, Rename, Delete, Merge, Rebase) · **Remote ›** (Add, Remove) · **Stash ›** (Stash, Include Untracked, Stash Staged, Apply, Pop Latest, Pop, Drop) · **Tags ›** (Create, Delete) · Show Git Output. Click a submenu row to fly it out; ops needing a value (clone URL, branch name, tag name, remote) open an input modal, ops choosing one of a list (apply/drop a stash, delete a tag, remove a remote) open a picker; `Esc` closes |
| Discard All Changes | Reverts every tracked file to HEAD after a `Y`/`N` confirm modal (untracked files are kept) |
| Click the header refresh icon | Force an immediate git re-scan |

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
| `Cmd`/`Ctrl`+`Shift`+`V` | Markdown: Toggle Preview — flip the active Markdown tab between its source and the rendered view (headings, emphasis, lists, quotes, tables, links, and fenced code blocks with the editor's own syntax colours). Arrows / PgUp / PgDn / Home / End / wheel scroll the preview; the same chord returns to the source (also Command Palette "Markdown: Toggle Preview") |
| `Cmd`+`Shift`+`\` | Go to Bracket: jump the cursor to the matching bracket. From an opening bracket it lands on its close (and vice versa); from inside a pair it lands on the enclosing close |
| `Cmd`+`Opt`+`\` | Select to Bracket: select the region between the matching brackets, including the brackets themselves |
| `Ctrl`+`T` | Transpose Characters: swap the character before the cursor with the one after it and step right; at the end of a line the last character moves across the line break |
| `Cmd`+`Opt`+`Shift`+`S` | Convert Indentation to Spaces: replace each tab in every line's leading indentation with one tab width of spaces (4, or 2 in YAML) |
| `Cmd`+`Opt`+`Shift`+`T` | Convert Indentation to Tabs: replace each leading run of one tab width of spaces with a tab, leaving any remainder |
| `Cmd`+`Opt`+`Shift`+`N` | Trim Final Newlines: drop trailing blank lines at the end of the file (always keeping at least one line) |
| `Cmd`+`Opt`+`Shift`+`J` | Join Lines: collapse the selected lines (or the current line with the next) into one, single-spaced |
| `Cmd`+`Opt`+`Shift`+`U` / `L` / `C` | Transform the selection (or the word under the cursor) to Uppercase / Lowercase / Title Case |
| `Cmd`+`Opt`+`Shift`+`A` / `D` | Sort the selected lines (or the whole file) Ascending / Descending |
| `Cmd`+`Opt`+`Shift`+`W` | Trim Trailing Whitespace: strip trailing spaces and tabs from every line |
| `Cmd`+`Opt`+`Shift`+`F` | Format Document: reformat the whole buffer through the language server (rustfmt, ruff, prettier, …); the edit lands as one undo step and leaves the tab dirty |
| `Cmd+K` `F` | Toggle Format on Save: when on, `Cmd+S` formats through the language server before writing (off by default, matching VS Code) |
| Click a breadcrumb symbol | The breadcrumbs bar above the editor shows the file path and the enclosing symbol trail at the caret; clicking a symbol crumb jumps to it |
| Click a sticky-scroll header | Sticky scroll pins the enclosing scope headers (class, function) to the top while you scroll; clicking one jumps to that line |
| Click the gutter fold chevron | Toggle the fold on that line: a `▾` marks an expanded foldable block (a function, loop, struct — any line whose body is more indented), a `▸` marks a collapsed one. Also on `Cmd+K` `Cmd+L` at the cursor, and Command Palette "Toggle Fold" / "Fold All" / "Unfold All" |
| Arrow keys over a fold | Up and Down step over a collapsed block rather than into it; an edit that lands inside one (from search, Go to Definition or Go to Line) opens it, so typing never changes lines you cannot see |
| Mouse drag | Select text; every other occurrence of a single-line selection highlights in blue |
| `Ctrl`+`C` / `Cmd`+`C` | Copy the selection to the system clipboard |
| `Cmd`+`Opt`+`C` / `Ctrl`+`Opt`+`C` | Copy Path: put the active file's absolute path on the system clipboard (also on the editor tab right-click menu) |
| `Ctrl`+`X` / `Cmd`+`X` | Cut the selection |
| `Ctrl`+`V` / `Cmd`+`V` | Paste at the cursor (replaces any selection) |
| `Ctrl`+`Z` / `Cmd`+`Z` | Undo (typing bursts coalesce; backspace, paste, cut, replace are each one step) |
| `Shift`+`Ctrl`+`Z` / `Shift`+`Cmd`+`Z` | Redo the most recently undone step; a fresh edit after an undo discards the redo branch (also Command Palette "Redo") |
| `Cmd`+`A` | Select the entire buffer |
| `Ctrl`+`f` / `Cmd`+`f` | Inline Find bar: pre-filled from the selection or word under the cursor; active match in orange, the rest in yellow; `Enter`/`F3` forward, `Shift+Enter`/`Shift+F3` back, `Esc` closes |
| `Ctrl`+`Alt`+`f` / `Cmd`+`Opt`+`f` | Replace in File: the Find bar expanded with a replace row; `Tab` switches field, `Enter` in the replace row replaces the current match and advances, `Cmd`+`Opt`+`Enter` (`Ctrl`+`Alt`+`Enter` on Linux) replaces all as one undo step; `$1` capture references work in regex mode |
| `Ctrl`+`A` / `Ctrl`+`E` | Move to start / end of line |
| `Ctrl`+`K` / `Ctrl`+`U` | Kill to end / start of line (yanks to clipboard) |
| `Cmd`+`o` / `Cmd`+`Shift`+`Enter` | Open a new line below / above, inheriting indent |
| `Ctrl`+`Shift`+`o` / `Cmd`+`Shift`+`O` | Go to Symbol in Editor: fuzzy-search the file's symbols and jump (type `:` then a number to go to a line) |
| `Cmd`+`g` `g` | Go to the top of the file (`Cmd`+`N` `Cmd`+`g` `g` for line N) |
| `Cmd`+`Shift`+`G` | Go to the bottom of the file (with a leading count, that line) |
| `Cmd`+`Shift`+`K` / `Ctrl`+`Shift`+`K` | Delete Line: remove every line touched by a cursor or its selection, so it composes with the extra cursors `Cmd`+`D` and `Opt`+click create; yanks to clipboard |
| `Cmd`+`y` `y` | Yank the current line (`Cmd`+`N` `Cmd`+`y` `y` for N lines) |
| `Cmd`+`D` / `Ctrl`+`D` | Add Selection to Next Find Match: select the word under the cursor, then grow the multi-cursor one occurrence per press |
| `Option`+click | Add (or toggle off) a secondary caret at the click |
| `Shift`+`Option`+drag | Column (box) selection: one caret per spanned row |
| `Cmd`+`F2` / `Ctrl`+`F2` | Change All Occurrences of the word under the cursor at once |
| `Esc` | Clear the selection, or collapse multi-cursors back to one |
| `Cmd`+`.` / `Ctrl`+`.` | Quick Fix: ask the language server for the code actions at the cursor (auto-import, fix-all, organize imports, refactors) and pick one from a menu; the diagnostics on the line ride along as context. One action with a deferred edit resolves and applies on pick (`codeAction/resolve`); also on the editor right-click menu and as Command Palette "Quick Fix"|
| `Cmd`+`.` / `Ctrl`+`.` inside a merge conflict | Resolve Merge Conflict picker: Accept Current / Accept Incoming / Accept Both (also Command Palette "Merge Conflict: Accept Current / Incoming / Both"); conflict regions render with VS Code's green (current) / blue (incoming) tints, and each conflict's header row carries clickable `[Accept Current] [Accept Incoming] [Accept Both]` actions |
| `F7` / `Shift`+`F7` in a buffer with conflicts | Jump to the next / previous conflict, wrapping (the status counts "Conflict k of n"); clicking a MERGE CONFLICTS entry in Source Control opens the file parked on its first conflict |
| Command Palette "Merge Conflict: Accept All Current / Incoming" | Resolve every block in the file one way |
| Command Palette "Merge: Complete Merge (stage file)" | Once zero conflicts remain: save the buffer and stage the file, moving it from MERGE CONFLICTS into the ordinary staged flow; refuses while blocks are unresolved |
| `F2` | Rename Symbol across every file it touches (open tabs edit in-memory and stay dirty) |
| `F12` / `Ctrl`+click | Go to Definition (`Shift`+`Ctrl`+click navigates back); `Ctrl` because mouse reports carry no `Cmd` bit and `Option` belongs to multi-cursor |
| `Ctrl`+`-` | Go Back: return to the location before the last navigation jump (the keyboard twin of `Shift`+`Ctrl`+click; also on the editor right-click menu and as Command Palette "Go Back") |
| `Shift`+`F12` | Go to References (project-wide; one use jumps, several open a picker) |
| `Ctrl`+`Shift`+`F12` | Go to Declaration (where the server implements it; hidden for TypeScript) |
| `Ctrl`+`F12` | Go to Type Definition |
| `Cmd`+`F12` | Go to Implementations (concrete implementors of a trait / interface) |
| Hover or click/tap (300 ms rest) | Hover popup: any diagnostic over the point first, then type / signature info. A click or tap arms the same dwell, so touch screens (Termux) get the popup by tapping an identifier and resting; releasing the press keeps it open |
| Hover a tab (300 ms dwell) | Tooltip with the tab's full path (tells two same-named files apart) |
| Hover a chrome control (300 ms dwell) | Button hint naming the control under the pointer: activity-bar icons (Explorer, Search, …), the EXPLORER header toolbar (New File, New Folder, Refresh Explorer, Collapse Folders, Views and More Actions), the Remote / Source Control header actions, and the SEARCH panel's actions and toggles (Refresh, Clear Search Results, Match Case, Use Regular Expression, …) |
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
| `Ctrl`+`r` | Redo |
| `v` `V` | Charwise / linewise Visual; a motion extends, `d` `y` `c` operate |
| `/` `?` then `Enter`, `n` `N` | Search forward / back, jump to next / previous match |
| `:w` `:q` `:wq` `:x` `:q!` `:qa`, `:{n}` | Write, close tab, write-and-close, quit-all, or jump to line n |

When vim mode is on it supersedes the always-on `Cmd`+`d` `d` / `Cmd`+`g` `g` chords; turn it off to get those back.

## Editor: previews

Image tabs (`.png`, `.jpg`, `.jpeg`, `.gif`, `.bmp`, `.webp`) are read-only; every keystroke is swallowed.

**PDF (`.pdf`)**

| Keys | Action |
|------|--------|
| `→` / `↓` / `Page Down` / `Space` | Next page |
| `←` / `↑` / `Page Up` | Previous page |
| `Home` / `End` | First / last page |
| Wheel down / up over the page | Next / previous page |
| Click a link on the page | Open it: an external URL opens in the browser, an internal target flips to its page (needs `pdftohtml` from poppler; only links anchored to text are detected) |

**Spreadsheet (`.csv`, `.tsv`, `.xlsx`, `.xls`, `.xlsb`, `.ods`)**

| Keys | Action |
|------|--------|
| `↑` / `↓` / `←` / `→` | Pan one row / column |
| `PageUp` / `PageDown` | Pan a full viewport vertically |
| `Home` | Jump to row 1, column 1 |
| `End` | Jump to the last visible page |
| Wheel down / up over the grid | Pan three rows |
| `Tab` / `Shift+Tab` | Switch worksheet |

## Run & Debug

Real breakpoint debugging over the Debug Adapter Protocol. Python is the verified adapter (debugpy, CPython 3.14+; croft provisions a private `~/.croft/debug-venv` with debugpy on first use, with no fallback to older interpreters). JavaScript / TypeScript run under Node via **vscode-js-debug** (the same engine VS Code, Zed and nvim use): croft auto-downloads the pinned `js-debug` server to `~/.croft/js-debug` on first use and talks to it over TCP, spawning the parent + child sessions js-debug requires; `node` must be on `PATH`, and TypeScript binds through source maps. Rust / C / C++ route to `lldb-dap` (binary built on launch; breakpoint binding additionally needs a permitted macOS `debugserver`). The lldb adapter is resolved from the Xcode Command Line Tools on macOS or from `PATH` elsewhere, matching the versioned names LLVM ships (`lldb-dap`, `lldb-dap-18`, legacy `lldb-vscode`); on Linux install it with the LLVM package (e.g. `apt install lldb`).

| Keys | Action |
|------|--------|
| `F5` | Start debugging the active file, or resume when paused at a breakpoint |
| `Shift+F5` | Stop the debug session |
| `Shift+Cmd+F5` | Restart the debug session |
| `Ctrl+F5` | Attach to a running Python process |
| `F6` | Pause (interrupt) a running program |
| `F9` | Toggle a breakpoint on the cursor's line (a red dot in the gutter); pushed live when a session is running |
| `Shift+F9` | Add or edit a conditional breakpoint on the cursor's line |
| `Shift+Alt+F9` | Add or edit a logpoint on the cursor's line (amber diamond; prints instead of pausing) |
| `Alt+F9` | Toggle break on raised (not just uncaught) exceptions |
| `F10` | Step over |
| `F11` / `Shift+F11` | Step into / out |

A bare `F5` / `F9` / `F10` / `F11` pressed while the **terminal pane** is focused and no debug session is live is forwarded to the app running in the shell (process-compose's `F10` Quit, htop's `F9` kill) instead of being claimed by the debugger. Modified chords keep their debug meaning everywhere, and `F9` still re-execs into a landed croft update.

Right-clicking the editor **gutter** (the glyph margin / line-number column) opens a breakpoint menu on the clicked line, mirroring VS Code's glyph-margin menu: **Add Breakpoint** / **Remove Breakpoint** and **Add Conditional Breakpoint** / **Edit Condition** (the cursor does not move).

When paused, the Run and Debug panel shows the **call stack** (click a frame to inspect it) and an expandable **variables** tree, plus a **debug console** of program output with a `❯` **REPL prompt** that evaluates expressions in the selected frame. When the session ends, the console output stays on screen (it isn't wiped back to the Run button), and if the program exited without ever hitting a breakpoint you set, the panel says so rather than just "Debug session ended" — the usual sign you ran a library module whose breakpointed code is never called, rather than its entry point. Hovering a variable in the editor shows its current value. The paused line is marked with a yellow `▶` in the gutter; a breakpoint is a red `●`, a conditional breakpoint a red `◆`, and a breakpoint the adapter could not bind a hollow `○`.

Also in the Command Palette: Start / Stop / Pause / Restart Debugging, Toggle Breakpoint, Add Conditional Breakpoint, Step Over, Toggle Break on Raised Exceptions (uncaught always breaks), and Attach to Python Process.

## Extensions sidebar

Jump here with `Cmd+Shift+X` (or the activity-bar icon, or the Command Palette's "View: Show Extensions"). The panel has three sections:

- **BUILT-IN** — the PDF and CSV viewers, Vim mode, the Color Themes, the Python / TypeScript / Rust / Go / YAML / JSON / HTML / CSS / Bash / TOML / C / C++ language servers, and the Python (debugpy), LLDB (Rust/C/C++), and JavaScript (vscode-js-debug) debuggers. Each has a **toggle switch** (blue = on, grey = off); disabling a debugger removes it as an F5 launch option for its file types.
- **INSTALLED** — extensions you added (from the catalog below or by hand-dropping a manifest into `~/.config/croft/extensions/<id>/`), also with enable/disable toggles.
- **AVAILABLE** — the curated **MCP server catalog**: vetted sidecars you can add. Each shows a **+Add** affordance instead of a toggle; adding one writes its manifest into your extensions dir, so it moves up into INSTALLED (enabled). Seeded with **Web Fetch**, **Time**, and **MarkItDown** (convert a PDF/DOCX/URL to Markdown).

So the lifecycle is **Available → Add → Installed → Enable → (provision on first use)**, each a distinct visible state, and it is reversible: an INSTALLED catalog extension shows a **trash button** (🗑, in the logo orange) just left of its toggle — click it, or select the row and press `Delete`, to uninstall. Either way croft **asks first**: a confirmation popup appears, and only `Enter` removes the manifest (it drops back to AVAILABLE to re-add); `Esc` keeps it. croft only uninstalls what it added from the catalog; a manifest you hand-dropped is your own file, so it shows no trash button and is never deleted (disable it instead). Hover any row to get a tooltip explaining the toggle's state, the `+Add` affordance, or the trash button.

**MCP sidecar extensions** contribute commands to the Command Palette (`Cmd+Shift+P`). Adding **Web Fetch** gives *"Web: Fetch URL as Markdown"*: it prompts for a URL, fetches it via the `mcp-server-fetch` sidecar (a separate vetted process croft speaks the Model Context Protocol to over stdio), and opens the result in a Markdown scratch buffer (a new editor tab). The first time you invoke a sidecar command, croft shows a one-time consent popup with the exact command it will run; the server is then provisioned pinned (via `uv`/`npm`, into croft's managed dir — never fetched-at-launch) and spawned lazily. The tool definition is fingerprinted on first use and re-verified on every spawn, so a silently-changed tool (a rug-pull) is refused. Disabling the extension removes its commands from the palette. A filter box at the top narrows all three sections by name, blurb, or id (a local filter — croft queries no remote marketplace; the catalog is curated and vetted).

| Keys | Action |
|------|--------|
| Type any character | Filter the list (matches name / description / id) |
| `Backspace` | Delete the last filter character |
| `Up` / `Down` | Move the selection |
| `Space` / `Enter` | Flip the selected extension's toggle (on AVAILABLE rows, add it) |
| `Delete` | Uninstall the selected INSTALLED catalog extension (back to AVAILABLE) |
| `Esc` | Clear the filter, or leave the view when it's already empty |
| Click a row | Select it |
| Click the toggle switch | Flip that extension on/off |
| Click the `✕` in the filter box | Clear the filter |
| Hover any row | Tooltip explaining the toggle / `+Add` / how to uninstall |

Disabling takes effect immediately for the viewers and Vim (a disabled PDF/CSV viewer opens that file type as plain text; a disabled Vim makes `Cmd+E` inert); a disabled language server stops spawning on the next launch. The choice persists in `~/.config/croft/config.json`.

## Terminal

| Keys | Action |
|------|--------|
| Any key | Forwarded to the shell PTY (arrows, `Ctrl+letter`, `Alt+x`, function keys translated to VT escapes) |
| Mouse drag | Select text, pinned to the scrollback content; drag past an edge to auto-scroll through history. Inside a full-screen app that scrolls by repainting (Claude Code, a pager), the highlight follows its text across the app's own scrolling — rows covered by the app's chrome (an input box, a floating pill) drop out — a row with an overlay in its middle keeps both intact ends — while the surviving rows stay highlighted, it hides entirely only when none remain (copy still yields the whole selection), and it reappears as the text scrolls back — and a drag held past an edge forwards wheel ticks so the app scrolls under the drag |
| `Shift`+click | Extend the existing selection to the clicked cell instead of starting a new one |
| Mouse wheel | At a shell prompt, scroll 5000 rows of scrollback (any keystroke snaps to the live bottom). When a full-screen app is tracking the mouse (Claude Code, htop, vim with `mouse=a`), the wheel scrolls that app's own content; `Shift`+wheel scrolls croft's scrollback instead. Full-screen apps that don't track the mouse still get arrow keys |
| `Cmd+F` / `Ctrl+F` | Find in terminal: search the active pane's screen + scrollback. Every hit is highlighted (active match in orange, the rest in yellow); `Enter`/`F3` next, `Shift+Enter`/`Shift+F3` previous (both scroll the match into view), `Esc` closes. Reserved by croft like iTerm2/Ghostty, so a full-screen app (Claude Code, vim) never sees the chord; in that mode it searches the visible screen, since the alternate screen has no scrollback |
| `Cmd+Opt+Up` / `Cmd+Opt+Down` (Linux: `Ctrl+Alt`) | Jump to the previous / next command prompt (VS Code's command navigation; the jumped-to prompt parks at the top of the pane). Powered by OSC 133 shell integration, auto-installed for zsh (a `ZDOTDIR` shim that sources your own dotfiles unchanged), bash 4.4+ (the `$ENV` + `--posix` bootstrap kitty and Ghostty use; macOS's system bash 3.2 spawns clean, without hooks), and fish (a `vendor_conf.d` script that defers to fish 4's native marks and only adds hooks on old fish). Opt out with `CROFT_SHELL_INTEGRATION=0` |
| Click a gutter dot on the pane's left border | Each finished command gets a VS Code-style decoration dot at its prompt row: blue for success, red for a non-zero exit. Clicking it opens the command's menu, headed by its exit code and runtime: **Copy Output**, **Select Output**, **Re-run Command**. A command over 10s finishing in a pane you're not focused on announces itself in the status bar on its own |
| `Cmd+K` `Shift+C` / `Shift+S` / `Shift+R` | Keyboard forms of the decoration menu, applied to the last finished command in the active pane: copy its output, select its output, re-run it |
| `Ctrl+Shift+Y` | Copy mode (WezTerm/tmux): a keyboard cursor walks the grid and scrollback with vi keys — `h`/`j`/`k`/`l` or arrows, `w`/`b`/`e` word motions, `0`/`$` line start/end, `g`/`G` oldest/newest line, `Ctrl+U`/`Ctrl+D` half page, `PageUp`/`PageDown` full page. `v` starts a character selection, `V` full lines, `Ctrl+V` a rectangular block; `y` (or `Enter`) copies it to the clipboard and exits, `Esc`/`q` exits without copying. The cursor paints as a green block |
| `Ctrl+Shift+H` | Command history (atuin's model, embedded): search every command croft's shell integration has seen this machine run — across sessions and restarts — each recorded with its directory, exit code, duration, and time. Type to filter (case-insensitive; duplicates collapse to the newest run), `↑`/`↓` select, `Ctrl+R` cycles the scope (all / this directory / failed only), `Enter` types the pick at your prompt without running it, `Esc` closes. Plain `Ctrl+R` still reaches your shell's own reverse search |
| `Cmd+K` `Shift+T` | Reopen the closed terminal: for 10s after closing a pane its process and scrollback stay alive in the background, and this brings it back exactly where it was (the browser reopen-tab convention). After the grace window the pane is disposed for real |
| `Ctrl+Shift+Space` | Quick select (WezTerm's hint mode): every URL, filesystem path (including `path:line:col`), git SHA, UUID, IP, hex colour/address, and long number on screen gets a short home-row label overlaid, with the bottom-most match labelled cheapest. Type a label to copy the match to the clipboard; type it in UPPERCASE to also paste it into the shell; `Backspace` erases a typed char, `Esc` cancels |
| Triggers (no chord) | iTerm2-style output triggers from `~/.config/croft/triggers.json` (palette: "Preferences: Open Terminal Triggers (JSON)", reloaded on save): each rule is a regex plus an action: `highlight` recolours every visible occurrence live (per-rule `#rrggbb` fg/bg, scrollback included), `notify` posts a status-bar notice (`\0` whole match, `\1`-`\9` capture groups), `bell` posts a bell notice, `capture` collects the whole matching line into the CAPTURES panel tab (iTerm2's Capture Output; click an entry there to jump its pane back to that line). notify/bell/capture fire once per completed output line, capped and never inside full-screen apps |
| `Cmd+K` `I` | Toggle broadcast input (iTerm2's `Cmd+Opt+I`): every keystroke and paste in the terminal goes to every pane at once. Enabling shows a confirm popup first; every receiving pane wears a red `⇶` name pill while it's on; switching off is instant, and closing down to one pane switches it off automatically |
| `Cmd+K` `Shift+I` | Exclude the active pane from broadcast input (or include it again). The focused pane always receives its own typing; exclusion mutes the mirrored copy while other panes are focused |
| `Shift+PageUp` / `Shift+PageDown` | Page through the pane's scrollback one screen at a time; `Shift+Home` jumps to the oldest line, `Shift+End` snaps back to the live bottom (xterm's convention, leaving the plain keys for the running program). Scrollback depth is 5000 lines by default, configurable via `terminal_scrollback` in `config.json` (applies to new panes) |
| Mouse selection (with Copy on Selection) | The Settings hub's "Terminal: Copy on Selection" toggle (VS Code's `terminal.integrated.copyOnSelection`, off by default) copies a finished drag-selection to the clipboard without the explicit `Cmd+C` |
| ANSI colors (no chord) | Pane colors render through the theme's own 16-color ANSI palette (VS Code's dark terminal defaults), so output looks the same in every host terminal; a theme extension overrides it with a 16-entry `ansi` array in its `[[themes]]` block |
| `imgcat`-style images (no chord) | A program printing an iTerm2 OSC 1337 inline image (`imgcat photo.png`) shows the picture anchored at its output row in the active pane, scrolling with the text and hiding once it scrolls off screen. The newest image per pane shows; text printed after it may sit underneath, since the grid never reserved rows for it |
| `Cmd+K` `D` | Open the pane's entire scrollback in a scratch editor tab named after the pane: find, vim mode, block selection, save-to-file, and `path:line` jumps all work on the captured log |
| `Cmd+K` `N` | Annotate the selection (iTerm2's annotations): pin a note to the selected output span. The span wears a dotted-amber underline that scrolls with the content and survives in scrollback; a plain click on it pops the note. `Cmd+K` `N` over an existing note edits it (a blank commit deletes); `Cmd+K` `Shift+N` deletes the note(s) under the selection. Session-scoped, like the scrollback itself |
| Click the prompt line | Move the shell cursor to the clicked column (Ghostty's click-to-move): croft synthesizes exactly the right number of arrow keys, so it works in zsh, bash, and fish line editors and never touches the typed text. Only at a prompt, only on the cursor's own row |
| Scrolled into long output (no chord) | The command that produced the output pins to the pane's top row with the scroll depth (Warp's sticky header), unpinning when the viewport crosses into another command's span or returns to the live bottom |
| Timestamps (palette toggle) | "Terminal: Toggle Timestamps" paints each row's arrival time (HH:MM:SS) down the right edge; a row that arrived 60s+ after its predecessor is tinted amber with a warning mark — the "where did the deploy stall" view |
| Host accents (config) | `host_accents` rules in `config.json` ({"pattern": "prod-*", "accent": "#f14c4c", "badge": "PROD"}) dress any pane whose shell reports a matching hostname over OSC 7: accent border, warning pill, translucent badge watermark. SSH inside the pane moves the hostname, so production shells are visually unmistakable; rules reload on saving config.json |
| Progress reports (no chord) | A program emitting ConEmu's OSC `9;4;state;percent` (systemd, winget, cargo wrappers; the sequence Ghostty and WezTerm render natively) fills the pane's bottom border into a live gauge: blue for normal progress, red frozen at the failure point on error, yellow when paused, and a sweeping segment while indeterminate. With two or more panes the name pill also shows the percent. Clears on the program's state-0 report or when the command finishes |
| `Cmd+C` / `Ctrl+Shift+c` | Copy the terminal's current selection |
| `Cmd+T` / `Ctrl+Shift+t` | Open another terminal beside the current one (each has its own PTY, scrollback, selection) |
| `Cmd+W` / `Ctrl+Shift+w` | Close the active terminal (no-op when one is left; `Ctrl+J` hides the pane) |
| `Cmd+]` / `Cmd+[` | Cycle to the next / previous terminal (or click one to focus it) |
| Click the `⌄` caret (beside `+`) | Drop the terminal profile menu anchored under the caret: pick a shell (from `/etc/shells` + `$SHELL`) to launch a new pane |
| `Cmd+K` `R` | Rename the active terminal pane (a blank name clears it, restoring the auto label) |
| `Cmd+K` `K` | Clear the active terminal's screen and scrollback (VS Code clears with `Cmd+K`) |
| `Cmd+K` `M` | Maximize the active terminal across the panel; press again to restore the even split |
| Click the `⛶` button (beside `-`) | Maximize that pane: it takes the panel's full width and the other terminals move to a rail down the right edge; the button becomes a restore glyph while maximized |
| Click a rail row | While maximized: hand that terminal the maximized pane (the highlight marks the active one), so you can shuffle between full-size terminals |
| Wheel over the rail | Scroll the rail when there are more terminals than it has rows; switching panes always scrolls the new one back into view |
| Right-click a terminal pane | Open the pane menu: **Rename Terminal**, **Clear**, **Quick Select**, **Copy Mode**, **Command History**, **Open Scrollback in Editor**, **Reopen Closed Terminal** (while one is in its undo window), **Maximize Terminal** (or **Restore Terminal Split**), **Broadcast Input** (and, while broadcasting, **Exclude from Broadcast**) |

With two or more panes open, each pane's header shows its live foreground process (`zsh`, `vim`, `node`…); a manual rename overrides that label.

The panel's layout survives restarts: the pane arrangement, each pane's directory and name, and which pane was focused are saved per workspace (on splits, closes, renames, reorders, and at quit) and restored as fresh shells the next time croft opens that workspace. A plain single-shell workspace stores nothing.
| `Cmd`/`Ctrl` + click a printed URL | Open it. A loopback dev-server URL on a remote session (`http://localhost:3000`) is forwarded home over the live SSH connection first, then opened in your local browser; any other link opens directly. Real OSC 8 hyperlinks (`ls --hyperlink`, modern CLIs) work too, even when the visible text isn't the URL |
| `Cmd`/`Ctrl` + click a printed `path:line[:col]` | Open that file in the editor at that line and column. Works on compiler errors (`src/x.rs:12:5`), test failures, grep hits, and Python tracebacks (`File "x.py", line 3`); relative paths resolve against the pane's current directory |

## Ports

The PORTS tab in the bottom panel group lists the loopback ports croft has noticed this session, from scraping terminal output (`http://localhost:PORT` banners, `listening on :PORT` lines) and a periodic socket poll of the shell's process subtree. A newly announced port also raises a transient, click-only toast in the bottom-right corner. The poll only surfaces ports owned by a pane's own processes, so one outside it (a port a container publishes, say) is found by the terminal scrape; it still drops off the list when it genuinely stops listening. On a remote session, forwarding rides the existing SSH master (no second connection); on a local session a port is already reachable, so the only action is to open it.

| Keys | Action |
|------|--------|
| `↑` / `↓` | Move the selection |
| `⏎` | Open the selected port in your browser (forwarding it home first on a remote session) |
| `f` | Forward the selected remote port without opening |
| `c` | Copy the selected port's address |
| `x` | On a forwarded port, stop forwarding it: the tunnel comes down, the row stays, and you can forward it again. Otherwise dismiss the row, which also stops croft re-detecting that port this session |
| Click a port row | Select it; **double-click** opens it in your browser (forwarding it home first on a remote session), same as `⏎` |
| Click the toast buttons | `Forward & Open` / `Forward` / `Open` / dismiss, depending on whether the session is remote |


## Captures

The CAPTURES tab collects output lines matched by `capture` triggers in `triggers.json` (iTerm2's Capture Output): point a rule at your compiler's error format and every hit funnels into one clickable list, however much output scrolled past.

| Keys | Action |
|------|--------|
| `↑` / `↓` | Move the selection |
| `Enter` / click a row | Jump the capturing pane back to that line: the TERMINAL tab activates, the pane scrolls the line into view and selects it |
| `x` | Remove the selected entry |
| `c` | Clear the list |
| `Esc` | Focus the editor |

## iTerm2 key mappings

`croft setup-iterm2` installs these `Cmd` chords as CSI-u forwarders (macOS otherwise reserves `Cmd` for menus) and relocates the conflicting iTerm2 / macOS menu shortcuts to unused alternates so their original actions stay reachable. Fully quit iTerm2 (`⌘Q`) and reopen after running it.

| iTerm2 keystroke | What croft does |
|------------------|-----------------|
| `⌘P` | Quick Open |
| `⌘⇧P` | Command Palette |
| `⌘F` | In-editor Find |
| `⌥⌘F` | Replace in File (find bar with the replace row) |
| `⌥⌘↩` | Replace All while the replace row is open |
| `⌘/` | Toggle line comment |
| `⌘.` | Quick Fix (code actions at the cursor; inside a merge conflict, the Accept Current / Incoming / Both picker) |
| `⌥⌘↑` / `⌥⌘↓` | Add a cursor above / below (multi-cursor) |
| `⌘⇧E` / `⌘⇧F` / `⌘⇧S` / `⌘⇧D` / `⌘⇧R` / `⌘⇧X` | Jump to Explorer / Search / Source Control / Run and Debug / Remote / Extensions |
| `⌘⇧L` | Disconnect a remote session |
| `⌘⇧B` | Run the project's build task (auto-detected); iTerm2's View ▸ Show Toolbelt is relocated to `⌃⌘B` |
| `⌘⇧N` | Explorer "New folder" prompt |
| `⌃⇧J` | Maximize the terminal pane |
| `⌥⌘R` | Reveal in Finder: the selected Explorer entry, or the active editor file when the editor is focused (local macOS only) |
| `⌘B` | Toggle the primary side bar |
| `⌘K` | Leader for the `Cmd+K` chords (Color Theme, Close to the Right, compare, Close All, Close Saved, Reveal in Explorer View, Move/Copy into New Window); iTerm2's Edit ▸ Clear Buffer is relocated to `⌥⌘K` |
| `⌘\` | Split the editor |
| `⌘⇧\` | Go to Bracket (jump to the matching bracket) |
| `⌘⌥\` | Select to Bracket |
| `⌘⌥⇧S` / `⌘⌥⇧T` | Convert Indentation to Spaces / Tabs |
| `⌘⌥⇧N` | Trim Final Newlines |
| `⌘⌥⇧J` | Join Lines |
| `⌘⌥⇧U` / `⌘⌥⇧L` / `⌘⌥⇧C` | Transform to Uppercase / Lowercase / Title Case |
| `⌘⌥⇧A` / `⌘⌥⇧D` | Sort Lines Ascending / Descending |
| `⌘⌥⇧W` | Trim Trailing Whitespace |
| `⌘⌥⇧F` | Format Document (reformat the whole buffer via the language server) |
| `⌘K` `F` | Toggle Format on Save |
| `⌘D` | Add Selection to Next Find Match (multi-cursor) |
| `⌘⇧K` | Delete Line |
| `⌥`+click / `⇧⌥`+drag | Add a caret / column (box) selection |
| `⌥⌘←` / `⌥⌘→` | Focus the left / right editor group |
| `⌘F12` | Go to Implementations |
| `⌃⇧F12` | Go to Declaration |
| `⌘S` / `⌘C` / `⌘X` / `⌘Z` / `⌘A` | Save / Copy / Cut / Undo / Select All |
| `⌘⇧Z` | Redo |
| `⌘T` / `⌘W` / `⌘]` / `⌘[` | New terminal / close terminal / cycle terminals |

`⌘V` is deliberately left on iTerm2's native Paste; croft reads the system clipboard and routes it by focus, so paste works identically over SSH. If you skip the setup command, the zero-setup `Ctrl`-based chords above still work, and you can map individual `Cmd` chords by hand in iTerm2 → Settings → Profiles → Keys → Key Mappings (e.g. `⌘S` → Send Hex Code `0x13`).

## Ghostty key mappings

Ghostty resolves its own keybinds (`new_tab`, `goto_tab`, ...) before it hands a key to croft, so by default `⌘T` opens a Ghostty tab and `⌘1`..`⌘9` switch Ghostty tabs instead of reaching croft. `croft setup-ghostty` adds a managed `keybind` block to your Ghostty config (`~/.config/ghostty/config`, or `~/Library/Application Support/com.mitchellh.ghostty/config`) that re-emits every croft chord as the same CSI-u sequence iTerm2 forwards, via Ghostty's `csi:` action. After running it, reload the config (`⌘⇧,`) or restart Ghostty.

The chord set is identical to the [iTerm2 key mappings](#iterm2-key-mappings) above (`⌘T` / `⌘W` / `⌘[` / `⌘]`, `⌘1`..`⌘9` / `⌘0`, the editor / Explorer / Source Control chords, the `⌘F12` family, and so on), so croft behaves the same under both terminals. `⌘V` is left on Ghostty's native paste for the same reason it is under iTerm2. Only the block between croft's marker comments is rewritten on each run; the rest of your Ghostty config is preserved.
