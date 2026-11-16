# croft keybindings

Every action in croft is reachable from the keyboard. Press `F1` inside croft for the same reference, grouped by pane and scrollable.

Per platform:

* **macOS** — `Cmd` chords need a one-time terminal setup: `croft setup-iterm2` ([iTerm2 key mappings](#iterm2-key-mappings)) or `croft setup-ghostty` ([Ghostty key mappings](#ghostty-key-mappings)).

* **Linux** — `Ctrl` is the command modifier: every `Cmd` chord works with `Ctrl`, the `Cmd+K` chords included. A few need `Super`; see [LINUX.md](LINUX.md#the-command-modifier).

* **Termux/Android** — `Ctrl` is the command modifier: every `Cmd` chord works with `Ctrl`.

* **Touch, no hardware keyboard** — tap the editor, terminal, or Search input to raise the on-screen keyboard, then tap its one-shot `ctrl` / `alt` keys before a letter (`ctrl` then `p` opens Quick Open). `⌄` dismisses it.

## Global

| Keys | Action |
|------|--------|
| `Ctrl+s` / `Cmd+s` | Save the open file. Tabs with no file behind them (past version, Search Editor, commit, Git Output) are never written; use `File: Save As…` |
| Command Palette: `File: Save As…` | Save the buffer to a prompted path; the tab becomes that file. Missing folders are created; an existing file is never overwritten. Command id: `save_as` |
| Command Palette: `File: Revert File` | Replace the open file's unsaved edits with the text on disk, as one undo step. Command id: `revert_file` |

| A file you have unsaved edits in changes on disk | A prompt asks. `reload` + Enter loads the disk version, saving yours to Local History (TIMELINE); Esc or any other answer keeps your edits |
| Typing `(` `[` `{` or a quote | Auto-closing pairs: the closer is inserted; typing it steps over; a selection is surrounded; backspace in an empty pair deletes both. Settings gear → "Auto Closing Pairs" toggles |
| `Ctrl+q` | Quit (not from a focused terminal, where the key goes to its app). Unsaved changes prompt: `S` / `Enter` save all, `D` discard, `Esc` / `N` cancel |
| `F1` | Open the shortcuts modal |
| `F6` | Cycle focus across panes (tree → editor → terminal → tree) |
| `Shift+F10` / the `Menu` key | Show Context Menu (`View: Show Context Menu`): open the right-click menu of what has the keys, just below it, as in VS Code. The text at the caret in the editor, the selected Explorer row, the picked COMMITS row, the active terminal pane, the selected capture, or the first problem shown. The menu then takes its own keys: arrows, `Enter`, `Esc`. The `Menu` key reaches croft only in a terminal that reports it on the kitty keyboard protocol; `Shift+F10` works everywhere. Command id: `show_context_menu` |
| `Shift+F8` | Screencast Mode (`Developer: Toggle Screencast Mode`): show each key, the command it ran, and clicks. Password and shell input are never shown |
| `Ctrl+b` / `Cmd+b` | Toggle the primary side bar (left pane) |
| `Opt+Ctrl+b` / `Opt+Cmd+b` | Toggle the secondary side bar (the active file's Outline) |
| `Ctrl+j` | Toggle the terminal pane |
| `Ctrl+Shift+j` | Maximize the terminal pane; press again to restore. The up chevron in the panel's tab row does the same |
| `Cmd+\` | Split the editor into two side-by-side columns, each with its own tabs. Closing a column's last tab collapses the split |
| `Ctrl+Shift+PageUp` / `Ctrl+Shift+PageDown` | Move the active tab left / right (palette: "View: Move Editor Left" / "View: Move Editor Right"). Pinned tabs stay ahead of the others |
| Drag an editor tab along the tab bar | Reorder the tab |
| Command Palette: `View: Move Editor into Next Group` / `View: Move Editor into Previous Group` | Move the active tab into the next / previous group, opening one if needed. Command ids: `move_editor_to_next_group` / `move_editor_to_previous_group` |
| `Cmd+Opt+←` / `Cmd+Opt+→` | Focus the left / right editor group (or click a column) |
| `Ctrl+p` / `Cmd+p` | Quick Open: fuzzy-search workspace files and open one. Append `:236` for a line, `:236:7` for a column, `:236-239` to select those lines |
| `Ctrl+p` / `Cmd+p`, then `#` | Go to Symbol in Workspace; Enter opens the definition |
| `Ctrl+p` / `Cmd+p`, then `>`, `@` or `:` | The first character picks the mode: `>` runs commands, `@` goes to a symbol in the open file, `:12` goes to line 12 |
| `Ctrl+Shift+p` / `Cmd+Shift+p` | Command Palette: fuzzy-search every command and run it; each shows its keybinding |
| `Ctrl+Shift+e` / `Cmd+Shift+e` | Jump to the Explorer sidebar |
| `Ctrl+Shift+f` / `Cmd+Shift+f` | Jump to the Search sidebar |
| `Ctrl+Shift+s` / `Cmd+Shift+s` | Jump to Source Control. `Ctrl+Shift+s` needs a terminal that reports `Shift` (kitty, Ghostty, WezTerm, Alacritty); others save instead. See [LINUX.md](LINUX.md#the-command-modifier) |
| `Ctrl+Shift+b` / `Cmd+Shift+b` | Run the build task detected from `.vscode/tasks.json`, Makefile, justfile, package.json, Cargo.toml, pyproject.toml, go.mod or CMakeLists.txt. Saves unsaved editors first (off: `"disable_save_before_debug": true`). Also "Tasks: Run Task", "Tasks: Rerun Last Task" |
| `Ctrl+Shift+d` / `Cmd+Shift+d` | Jump to Run and Debug |
| `Ctrl+Shift+r` / `Cmd+Shift+r` | Jump to Remote (SSH) |
| `Cmd+K` `L` / `Ctrl+K` `L` | Remote: Drop to Local: disconnect and return to the local croft, asking about unsaved changes first. In a focused terminal off macOS, use the palette |
| `Ctrl+Shift+x` / `Cmd+Shift+x` | Jump to Extensions |
| `Ctrl+Shift+m` / `Cmd+Shift+m` | Show the PROBLEMS tab of the bottom panel |
| `Ctrl+Shift+u` / `Cmd+Shift+u` | Show the OUTPUT tab; "Output: Select Channel…" picks its channel. If the desktop's input method takes `Ctrl+Shift+u`, use "View: Show Output" |
| `Cmd+Shift+t` | Focus the terminal pane, showing the panel and its TERMINAL tab (palette: "Terminal: Focus Terminal") |
| `Ctrl+Alt+s` / `Cmd+Opt+s` | Save All: save every tab with unsaved edits, in every split. Tabs that cannot be saved safely are left and counted in the status line |
| `Ctrl+Shift+h` / `Cmd+Shift+h` | Replace in Files: open the Search sidebar with its Replace row focused. In the terminal pane `Ctrl+Shift+h` stays command history; `Cmd+Shift+h` works there too |
| Click a pinned row in the Explorer's sticky band | Select that ancestor directory and scroll it to the top (right-click opens its context menu) |
| Click activity-bar icons | Switch between Explorer, Search, Source Control, Run and Debug, Remote, Extensions, and Testing |
| Click the settings gear | Open settings: Color Theme picker (Croft Black, Croft Loch, Croft Dark Blue, Croft Light, ten editor-inspired dark themes, High Contrast, High Contrast Light) or Customize Layout |
| Click the sponsor heart (just above the gear) | Open croft's GitHub Sponsors page in the browser (palette: "Help: Sponsor croft") |
| Click the layout icons (top-right of the editor / welcome) | Toggle the primary side bar, toggle the panel, or open the **Customize Layout** popup |
| Drag a seam | Resize the sidebar, the split between editor columns, or the editor/terminal split |

* `Ctrl+s` / `Cmd+s`: a folder of the file's path that is gone (removed by a `git checkout`, say) is created again.
  * A tab with no file behind it (a past version, a Search Editor, a commit, Git Output) is never written, by save or auto save. Saving it closes the tab without asking; `File: Save As…` keeps a copy.

* `Ctrl+q`: Save all stays open, naming each file, when something cannot be saved: an untitled buffer, a file changed on disk, text its encoding cannot hold, a merge with conflicts left, or a file open in two groups with different unsaved text.

* Hot exit: unsaved buffers are backed up a few seconds after the last edit under `~/.cache/croft/hot-exit/`, and the next launch restores them as unsaved tabs. A clean quit, `S` and `D` remove the backup.

* `Ctrl+Alt+s` / `Cmd+Opt+s`: a tab changed on disk, with an encoding that would lose characters, an unresolved merge, or a hex or sheet edit is left for its own `Cmd+S`.

* Quick-pick popups (Quick Open, Command Palette, Go to Symbol, the branch and directory pickers): click a row to open it, wheel to move the selection.

* `File: Save As…`: the prompt takes a workspace-relative path, prefilled with the file's own.

* `File: Revert File`: `Cmd`+`Z` brings the edits back; a tab with nothing unsaved is left alone.

* Tabs with no file behind them are never written by auto save either.

* Auto-closing pairs: openers never close before a word. Quotes never close after a word (except a Python string prefix such as `f"` or `rb'`), nor inside a string or comment.

* `Ctrl+q`: an untitled buffer has no file to go to; copy its text out or choose `D`. A file open in two groups with the same unsaved text is saved once.

* Hot exit: a failed backup write is reported in the status line and retried every 5 s. Restored tabs write nothing to their files, and the launch names any file changed on disk since.

* `Shift+F8`: each key shows in a box at the bottom for a moment. Typing gathers into one run, a held key counts (`↓ ×12`), and a `Cmd+K` chord reads as one (`Ctrl+K Z`).

* `Shift+F8`: a paste shows as `Paste`, never its text. Around password or shell input only the surrounding keys show. The box moves to the top while it would cover the caret.

* `Cmd+\`: each column keeps its own tabs, scroll and cursor.

* Dragging a tab: it moves the moment the pointer crosses another tab, and stays active.

* Move Editor into Next / Previous Group: the new group opens to the right / left, and a group left empty closes. The tab's right-click menu has Move Left / Right / Above / Below.

* Quick Open reveals the opened file in the Explorer. A file name with a line, `a.py:12`, is still a file search. The Debug: Attach to Python Process list takes clicks and the wheel too.

* `Ctrl+Shift+s`: under tmux only 3.2 to 3.4 with `extended-keys always` reports the `Shift`.

* `Ctrl+Shift+b`: the task runs in a named terminal pane. Every task and Testing run saves unsaved editors first (VS Code's `task.saveBeforeRun`); with `"disable_save_before_debug": true` the status line names the unsaved files it runs without.

* Remote: Drop to Local returns to the directory you connected from. `Cmd`/`Ctrl`+`Shift`+`L` selects all occurrences on a remote session as it does locally.

* Colour themes: Croft Black, Croft Loch (Croft Black drawn as layered surfaces without pane boxes), Croft Dark Blue, Croft Light (a light theme that recolors the whole session, host terminal included), High Contrast and High Contrast Light (for low vision).

* Dark themes: One Dark Pro, Dracula, Monokai, Nord, Gruvbox Dark, Tokyo Night, Catppuccin Mocha, Solarized Dark, GitHub Dark, Darcula.

* Explorer sticky band: scrolled deep in a directory, its ancestor rows pin to the top of the tree.

### `Cmd+K` chords

`Cmd`/`Ctrl`+`K` is a leader: press it, then a second key. As in VS Code, the status bar says it is waiting, the wait ends after 5s or when the window loses focus, and a key that completes no chord, `Esc` included, only cancels it with a "not a command" note. On Linux `Ctrl`+`K` leads everywhere except the terminal pane and the vim-mode editor, and `Cmd` on the second key is `Ctrl` too.

| Keys | Action |
|------|--------|
| `Cmd+K` `Cmd+T` | Open the Color Theme picker |
| `Cmd+K` `G` | Remote: accept the status bar's offer to open a workspace on the host a pane has `ssh`'d into; `Esc` dismisses it. Settings: `disable_remote_offer`, `remote_offer_excluded_hosts` |
| `Cmd+K` `Shift+L` | Agent: New Worktree Lane: creates a worktree on `agent/<slug>`, adds it as a workspace folder and opens a pane there, running the `lane_agent` setting's agent. Close: "Agent: Close Worktree Lane" |
| `Cmd+K` `Shift+C` | Git: Show Commit Actions: the Commit button's `▾` menu, driven by the keyboard. In the terminal `Cmd+K` `Shift+C` keeps copying the last command's output |
| `Cmd+K` `Shift+A` | Git: Show Source Control Actions: Source Control's `⋯` menu, driven by the keyboard (see [Source Control sidebar](#source-control-sidebar)) |
| `Cmd+K` `A` | Session: Participants: who is attached to this persistent session. `Enter` on a row grants/revokes write control or disconnects them |
| `Cmd+K` `Shift+Q` | Session: Detach: leave a persistent session (`croft attach`, `croft remote`) while it keeps running |
| `Cmd+K` `X` | Collab: Cancel AI Stream: stop a `croft pair` collaborator's stream into a shared file and revert its text (same as the gutter's orange `■`) |
| `Cmd+K` `N` | Notes: leave a sticky note on the caret line. `F4` reaches unresolved notes; "Notes: Delete Note" removes one; "Source Control: Export Comments to Pull Request" posts them |
| `Cmd+K` `Q` | Navigator: ask the AI pair programmer about the caret line or the selected lines (also on the gutter and body right-click menus) |
| `Cmd+K` `Y` | Navigator: yield the turn: it reviews the active file with comments only (orange `◆` in the gutter, and the Navigator OUTPUT channel) |
| PROBLEMS row right-click → **Fix with Navigator** (also the last row of the `Cmd+.` Quick Fix picker when the caret sits on a diagnostic, and the palette's **Problems: Fix With Navigator**) | Navigator: fix that diagnostic; its edit streams into the buffer. `Cmd+K X` cancels and reverts it |
| `F4` | Focus the active file's next navigator comment box (wraps) |
| `Shift+F4` | Ignore the focused navigator comment box (or the next one from the caret) |
| (box focused) type / `Backspace` / `←` `→` | Edit the box's reply draft |
| (box focused) `Enter` | Send the reply to the navigator |
| (box focused) `Esc` | Leave the box and return to the buffer |
| `Cmd+K` `→` | Close the tabs to the right of the active one; unsaved tabs stay open |
| `Cmd+K` `Cmd+\` / `Cmd+K` `Opt+↓` / `Cmd+K` `Opt+←` | Split the editor up / down / left: the active file in a new group on that side. `Cmd+K` `Opt+↑` and `Cmd+K` `Opt+→` split up and right too, as `Cmd+Opt+←` / `→` focus the group on that side |
| `Cmd+K` `Shift+Cmd+\` | Split in Group: a second view of the active file beside it, in the same group |
| `Cmd+K` `Shift+←` / `Cmd+K` `Shift+→` / `Cmd+K` `Shift+↑` / `Cmd+K` `Shift+↓` | Move the active tab into the group on that side, opening a group there if there is none |
| `Cmd+K` `Shift+M` | Move the minimap to the editor's other edge |
| `Cmd+K` `S` | Select the active file as the compare anchor |
| `Cmd+K` `C` | Diff the active file against the compare anchor |
| `Cmd+K` `W` | Close all editor tabs in every split, keeping pinned and unsaved tabs. `Cmd+K` `Shift+W` reopens them |
| `Cmd+K` `U` | Close the focused group's saved tabs, keeping unsaved and pinned ones |
| `Cmd+K` `E` | Reveal the active file in the Explorer tree |
| `Cmd+K` `O` | Copy into New Window: open the active file in a new terminal window showing just the file (Ghostty / iTerm2 / Terminal; macOS only) |
| `Cmd+K` `Shift+O` | Move into New Window: same, and close the file's tab here (macOS only; save unsaved changes first) |
| `Cmd+K` `Z` | Toggle Zen Mode: hide the activity bar, side bars, panel and status bar, and centre the editor; press again to restore |
| `Esc` `Esc` | Leave Zen Mode (two Escapes in the editor within half a second) |
| `Cmd+K` `V` | Toggle Live Run for the active Python file: re-runs as you pause typing and shows each line's values, output and exceptions (palette: "Python: Toggle Live Run") |
| `Cmd+K` `V` (Markdown) | Markdown: Open Preview to the Side: render the active Markdown file in the next editor group, updating as you type |
| `Cmd+K` `B` | Show the Testing view. There: Enter runs all, `s` stops, `r` re-discovers, `o` opens OUTPUT, `w` watches all; click a test's glyph to run it, its name to open it |
| `Cmd+K` `Enter` | Run the test at the caret (palette: "Testing: Run Test at Cursor") |
| `Cmd+K` `Shift+Enter` | Debug the test at the caret (palette: "Testing: Debug Test at Cursor"); Alt+click a gutter ▷ does the same. A launch.json debugpy configuration with `purpose` `"debug-test"` supplies pytest's settings |
| (palette) | Unbound by default: "Testing: Run All Tests" (`testing_run_all`), "Testing: Run Tests in Current File" (`testing_run_file`), "Testing: Rerun Failed Tests" (`testing_rerun_failed`), "Testing: Rerun Last Run" (`testing_rerun_last`) |
| `Cmd+K` `P` | Pin / unpin the active editor tab (palette: "View: Pin Editor" / "View: Unpin Editor") |
| `Cmd+K` `Shift+P` | Keep the active preview tab open as a real tab (palette: "View: Keep Editor") |
| `Cmd+K` `H` | Show incoming calls: a picker of the callers of the symbol at the caret; pick one and invoke again to walk up |
| `Cmd+K` `Shift+H` | Show outgoing calls: everything the function at the caret calls; each entry jumps to its definition |
| `Cmd+K` `Shift+U` | Show supertypes: the classes, traits or interfaces the type at the caret extends or implements |
| `Cmd+K` `Shift+D` | Show subtypes: the types that extend or implement the one at the caret |
| `Cmd+K` `Cmd+S` | Keyboard Shortcuts editor: search every command, choose one and press the new chord; croft writes it to `keybindings.json`. Also in Settings |
| `Cmd+K` `Shift+F` | Open the Search sidebar's query and results as a Search Editor tab; `Enter` or `F12` on a result line opens the match |
| `Cmd+K` `Shift+R` | Search Editor: Rerun: search again with the tab's `# Query:` header |
| `Cmd+K` `Shift+G` | Toggle inline AI suggestions (off by default). `Tab` accepts the ghost text, `Ctrl+Right` its next word, `Ctrl+Down` its next line, `Esc` dismisses. Uses the `croft pair` model or `ANTHROPIC_API_KEY` |
| Command Palette "Rust: …" | rust-analyzer commands: **Expand Macro Recursively at Caret**, **Locate Parent Module**, **Open Cargo.toml**, **Rebuild Proc Macros and Build Scripts**, **Reload Workspace** |
| `Cmd+K` `Shift+E` | Run the code lens on the caret line (a menu when there are several). "Editor: Toggle CodeLens" hides code lenses |
| `Cmd+K` `Shift+V` | Open the symbol at the caret in its own tab; edits land in the file. Also on an OUTLINE row's right-click menu and `Alt+Enter` in Go to Symbol in Workspace |
| `Cmd+K` `M` | Maximize the active terminal pane across the panel (other terminals move to a right-edge rail); press again to restore |
| `Cmd+K` `F` | Toggle Format on Save: `Cmd+S` formats the file before writing |
| `Cmd+K` `Cmd+L` | Toggle the fold at the cursor: collapse the enclosing block to its header line, or expand it |
| `Cmd+K` `Cmd+0` | Fold All: collapse every foldable block in the buffer |
| `Cmd+K` `Cmd+J` | Unfold All: expand every collapsed block |
| `Cmd+K` `Cmd+/` | Fold All Block Comments: collapse every comment fold |
| `Cmd+K` `Cmd+1` … `Cmd+K` `Cmd+7` | Fold Level 1–7: collapse every fold that many levels deep or deeper, leaving the outer ones open |
| Command Palette: `Fold Recursively` / `Unfold Recursively` | Collapse or expand the fold at the cursor and every fold inside it. Command ids: `fold_recursively` / `unfold_recursively` |
| `Cmd+K` `Cmd+8` / `Cmd+K` `Cmd+9` | Fold / Unfold All Regions: collapse or expand every `#region … #endregion` pair |
| Command Palette: `View: Toggle Indent Guides` | Show or hide indentation guides (on by default; also in Settings) |
| Command Palette: `Editor: Toggle Bracket Pair Colorization` | Colour brackets by nesting depth, unmatched closers in red (on by default; also in Settings) |
| Command Palette: `View: Toggle Render Whitespace` | Cycle whitespace glyphs (`·` spaces, `→` tabs): selection (default) → all → none; also in Settings |
| Command Palette: `View: Change Line Numbers (On, Relative, Interval, Off)` | Cycle the gutter's numbers: on (default) → relative → interval (every tenth line) → off. Setting: `line_numbers` |
| Command Palette: `Debug: Toggle Inline Values` | While stopped in the debugger, lines that mention a local show its current value (on by default; also in Settings) |
| Command Palette: `Workspaces: Add Folder to Workspace` | Pick a folder and add it as another workspace root. Also on a root row's context menu |
| Command Palette: `Workspaces: Remove Folder from Workspace` | Remove the Explorer-selected secondary root folder from the workspace. Also on its context menu |
| Command Palette: `Workspaces: Save Workspace As` | Save the current folder set as a `.code-workspace` file at a prompted path. Over an existing file it asks first and changes only `folders` |
| Command Palette: `Workspaces: Open Workspace from File` | Open the active editor's `.code-workspace`: re-root to its first folder and load its folder set. CLI: `croft project.code-workspace` |
| Mouse wheel | Scroll the pane under the pointer |

* `Cmd+K` `G`: the offer appears when a pane's foreground becomes `ssh <host>` for a host in `~/.ssh/config` and clears when the ssh ends or after 90 s.

* `Cmd+K` `Shift+L`: `lane_agent` in settings.json names an `agents.json` row (built in: claude, codex, aider, gemini); without it the pane is a plain shell. The lane comes back on relaunch, and closing it keeps the lane while a file in it has unsaved edits.

* `Cmd+K` `B`: tests are discovered on first open (`cargo test` for Rust, `pytest` for Python). Click a suite header's play glyph to run the suite, or a row's eye to watch that test or suite (it reruns on every save).

* `Cmd+K` `Shift+Enter`: when the test's last run failed in your own code, croft sets a temporary breakpoint at the assertion and removes it when the session ends.

* "Testing: Run Tests in Current File" runs a pytest file, a cargo module, a go package or a CodeQL test. For a cargo crate root, a `src/bin` file, an integration test and jest/vitest files, use Run All Tests.

* VS Code keybindings to `testing.runAll`, `testing.runCurrentFile`, `testing.reRunFailTests`, `testing.reRunLastRun`, and to `rust-analyzer.expandMacro`, `.parentModule`, `.openCargoToml`, `.rebuildProcMacros` and `.reloadWorkspace`, import onto the matching commands.

* `Cmd+K` `Shift+G`: `CROFT_COMPLETE_MODEL` / `CROFT_COMPLETE_URL` override the model; the key is sent only over https or to this machine. A language server that offers inline completions answers instead, with no model needed.

* `Cmd+K` `F`: a server that has not answered in 2 s, or a second `Cmd+S`, saves the file unformatted.

* `Cmd+K` `G`: the status bar says `Connected to <host> · Open workspace here?`; accepting runs "Remote: Open Workspace on This Pane's Host". A host whose provisioning failed is not offered again for a week.

* `Cmd+K` `Shift+L`: the worktree is cut beside the repo and the pane is named `Lane: <slug>`. The lane's Explorer root row shows the branch and seated agent (`agent/fix-login · ◆ claude ●`).

* "Agent: Close Worktree Lane" also closes the lane's pane (undoable with `Cmd+K Shift+T`) and its tabs.

* `Cmd+K` `Shift+Q`: a read-only participant, or a client of an unresponsive session, can always detach.

* `Cmd+K` `N` (editor focused): in a `--solo` session every participant sees the note under the same line. It follows the line as code moves, takes replies in its box, and its ✕ resolves it; the Explorer shows `✎N` per file.

* `Cmd+K` `Q` opens the instruction box.

* Fix with Navigator: the file opens at the diagnostic, and the navigator is told what was reported, by whom and where, with the server's quick fixes as hints. The row shows a spinner until the turn ends; with no navigator seated the action says so.

* Navigator comment boxes: `F4` moves the caret to the box's line, editing a reply leaves the buffer untouched, and `Enter` sends a comment-only turn.

* `Cmd+K` `W`: close an unsaved tab with `Cmd+W` to be asked about it.

* `Cmd+K` `O`: the new window hides the Explorer and terminal; the current window is untouched.

* Zen Mode restores exactly what was shown before. An `Esc` that closes a popup or clears a selection does not count towards `Esc` `Esc`.

* Live Run: line numbers go green for statements that ran and red for ones that never did.

* Markdown preview to the side: opens a new group to the right when there is none, follows the source as it scrolls, and stays rendered when the file is saved (VS Code's `markdown.showPreviewToSide`).

* Testing view: `s`, the ■ Stop control or "Testing: Cancel Test Run" stops a run or discovery. `o`, or the "see OUTPUT › Test Runner" pointer, opens the Test Runner channel; `↑`/`↓` or the scrollbar scroll.

* Debug Test: pytest runs under the project's venv with the `env`, `cwd`, `args`, `justMyCode` and other settings of the `"debug-test"` configuration. A cargo test runs under lldb-dap with the test name as its filter.

* Debug Test: a breakpoint you set yourself on the assertion line is left alone.

* Run Tests in Current File: a cargo module is named from its path (`src/a/b.rs` is `a::b`). Rerun Failed Tests runs exactly the tests the tree shows failed; Rerun Last Run repeats the last run as it was asked.

* While a test run is going, each Testing command says "Tests are already running".

* `Cmd+K` `Shift+U` / `Shift+D` need a language server with type hierarchy (clangd, gopls, jdtls and others).

* Keyboard Shortcuts editor: shows each command's shortcut (yours or the default). A change keeps your comments in `keybindings.json` and works at once.

* Search Editor uses VS Code's `.code-search` layout; several can stay open at once. Rerun uses the header's flags, includes and excludes too, and runs in the background.

* Inline AI suggestions appear after a pause in typing at the end of a line; a multi-line one hangs its further lines under the caret's line. An accepted suggestion is recorded as the navigator's writing.

* When a language server's inline completion rewrites what you typed, the old text is struck through with the new text beside it, and `Tab` replaces it.

* Rust commands are listed while rust-analyzer serves the active file. Expand Macro opens the expansion of the macro call or `#[derive]` under the caret in a Rust tab; Locate Parent Module jumps to the `mod` declaration; Open Cargo.toml opens the crate's manifest.

* Rebuild Proc Macros and Build Scripts, and Reload Workspace, refresh rust-analyzer after proc-macro output or a `Cargo.toml` edit went stale.

* Code lenses are the language server's clickable actions at the end of a symbol's line, such as "▶ Run Test" and "⚙ Debug"; click one to run it.

* `Cmd+K` `Shift+V`: the tab shows only that function or class and follows the symbol as the file changes.

* Folding: `// region` and similar markers count as regions. Without usable language-server fold ranges, folds come from indentation, `#region` pairs and comment runs; Fold All Block Comments then folds runs of two or more full-line comments.

* Indent guides highlight the cursor's block. Bracket colours leave brackets in strings and comments plain. Indent guides, bracket colours, whitespace, line numbers and inline values are persisted.

* Relative line numbers: the caret's line shows its number and the rest their distance from it (for Vim counts like `5j`). The caret's number is brighter.

* Inline values (`name = value`) cover the paused function from its header through the stop, refresh per stop and clear on resume.

* Add Folder: the Explorer shows it as a section, Quick Open and search span it with folder-name prefixes, and its language servers start per folder.

* Fold / Unfold Recursively are palette only: VS Code's `Cmd+K Cmd+[` / `]` collapse and restore terminal panes here.

* Remove Folder: the primary root is the launch identity; use Make Root to change it.

* Save Workspace As writes a VS Code-compatible file, with folder paths relative to it where possible.

### Customize Layout

Open it with the **⛶** icon at the top-right of the editor (or the welcome screen), the settings gear → **Customize Layout**, or **View: Customize Layout…** in the Command Palette. Every row is also a palette command; the palette query "layout" lists them all:

| Group | Options |
|-------|---------|
| Visibility | Activity Bar (**View: Toggle Activity Bar Visibility**), Primary Side Bar (`Cmd`/`Ctrl`+`B`), Secondary Side Bar (`Opt`+`Cmd`/`Ctrl`+`B`), Panel (`Ctrl`+`J`), Status Bar (**View: Toggle Status Bar Visibility**), Minimap (`Opt`+`Cmd`/`Ctrl`+`M`) |
| Auto-Hide Side Bar | Off by default. On, the primary side bar collapses when focus moves to the editor or a terminal (**View: Toggle Auto-Hide Side Bar**). See [LAYOUT.md](LAYOUT.md#auto-hide-side-bar) |
| Primary Side Bar Position | Left / Right (moves the activity bar with it); **View: Toggle Primary Side Bar Position** swaps it |
| Panel Alignment | Left / Center / Right / Justify (Justify spans the full width under the side bar); **View: Set Panel Alignment…** |
| Quick Input Position | Top / Center (where Command Palette / Go to File appears); **View: Set Quick Input Position…** |
| Zen Mode | `Cmd`/`Ctrl`+`K` `Z`; `Esc` `Esc` leaves it. Zen centres the editor unless `layout.zen_center_layout` is `false` |
| Centered Layout | Keeps the editor `layout.centered_layout_width` columns wide (120 by default) in the middle of its column; also **View: Toggle Centered Layout** |

Every choice except the side-bar / panel visibility persists across launches in `~/.config/croft/config.json`. While the terminal pane is focused, bare `Ctrl`+`B` goes to the app running there; `Cmd`+`B` always reaches croft.

* The popup stays open while you flip several toggles.

* Auto-Hide Side Bar: a reveal you asked for (`Cmd`/`Ctrl`+`B`, the activity bar, the popup row) is exempt from the next collapse.

## Command Palette

`Cmd`/`Ctrl`+`Shift`+`P` opens the Command Palette: type to fuzzy-search every command, `↑`/`↓` to move, `Enter` to run, `Esc` to close. Each command shows its chord on the right; off macOS it reads `Ctrl` for `Cmd`, `Alt` for `Opt`, and `Super` for the few chords [LINUX.md](LINUX.md#the-command-modifier) lists.

| Command | Chord | Action |
|---------|-------|--------|
| Debug: Select and Start Debugging | palette only | Pick a `launch.json` configuration (or "Debug active file") and start it; `F5` and restart then launch that pick. A compound launches every configuration it names |
| Debug: Add Configuration… | palette only (also the last row of the picker above) | Build a configuration from prompts: debugger (Python, Rust / C / C++, Go, JavaScript / TypeScript), launch or attach, then each field. Added to `.croft/launch.json` and selected for `F5` |
| Debug: Restart | `Shift`+`Cmd`+`F5` | Restart the active debug session |
| Debug: Add Conditional Breakpoint | `Shift`+`F9` | Add or edit a conditional breakpoint at the cursor |
| Debug: Add Logpoint | `Shift`+`Alt`+`F9` | Add or edit a logpoint at the cursor: prints the message (`{expr}` interpolates) instead of pausing |
| Debug: Toggle Break on Raised Exceptions | `Alt`+`F9` | Break on raised (not just uncaught) exceptions |
| Debug: Attach to Python Process | `Ctrl`+`F5` | Pick a running CPython 3.14+ process and attach a `pdb` REPL in a croft terminal, using `sudo` when the OS requires it |
| Preferences: Color Theme | `Cmd`+`K` `Cmd`+`T` | Pick the active color theme (also via the settings gear) |
| Session: Participants | `Cmd`+`K` `A` | List who is attached to this multiplayer session (docs/MULTIPLAYER.md); pick one to grant/revoke write control or disconnect. The status bar shows an "N attached" badge |
| Markdown: Run Code Block at Cursor | `Cmd`+`Enter` | Run the runnable fence under the caret in a Markdown source, after a confirm popup. Use the palette when the terminal sends `Ctrl`+`Enter` as `Enter` |
| Select All | `Cmd`+`A` | Select the whole active editor buffer. Use the palette on Linux, where `Ctrl`+`A` is line start |
| View: Focus Left Editor Group / View: Focus Right Editor Group | `Cmd`+`Opt`+`←` / `Cmd`+`Opt`+`→` | Focus the left / right editor group while split. Use the palette on Linux, where `Ctrl`+`Alt`+arrows switch desktop workspaces |
| Terminal: Focus Next Terminal / Terminal: Focus Previous Terminal | `Cmd`+`]` / `Cmd`+`[` | Focus the terminal pane and cycle to the next / previous terminal. Use the palette on Linux, where `Ctrl`+`[` is `Esc` |
| Go to Implementations | `Cmd`+`F12` | Go to the implementors of the trait / interface at the caret. Use the palette on Linux, where `Ctrl`+`F12` is Go to Type Definition |
| C/C++: Switch Header/Source | `Alt`+`O` | Open the counterpart of a C, C++ or Objective-C source or header |
| Explorer: Jump to Directory (zoxide) | `Cmd`+`Z` in the Explorer | Open the zoxide jump popup from any pane. Use the palette on Linux, where `Ctrl`+`Z` is kept out of the Explorer |
| Session: Detach | `Cmd`+`K` `Shift`+`Q` | Detach this client; the session keeps running. Run the same `croft attach` / `croft remote` command to reattach |
| File: New Untitled Text File | `Cmd`+`N` (from the editor or the side bar) | A scratch tab (`Untitled-1`, `Untitled-2`, …) with no file behind it; saving asks where to write it |
| File: New File… / File: New Folder… | palette only (`Cmd`+`F` / `Cmd`+`Shift`+`N` while the Explorer is focused) | The Explorer's create prompt from any pane: beside the active file, or in the Explorer selection when the Explorer has focus or no file is open |
| Select All | `Cmd`+`A` | Select the whole active editor buffer. Use the palette on Linux, where `Ctrl`+`A` is line start |
| View: Show Ports / View: Show Captures | palette only | Show and focus the PORTS / CAPTURES tab of the bottom panel |
| Output: Select Channel… | palette only | Pick an OUTPUT channel and show it |
| View: Close Other Editors in Group | `Cmd`+`K` `Shift`+`X` | The tab menu's **Close Others**: close every other tab of the focused group except pinned tabs; other splits are untouched |
| View: Close Editors to the Right in Group / View: Close Saved Editors in Group / View: Close All Editors | `Cmd`+`K` `→` / `Cmd`+`K` `U` / `Cmd`+`K` `W` | The tab menu's **Close to the Right**, **Close Saved** and **Close All** on the active tab (Close All covers every split) |
| View: Split Editor Up / View: Split Editor Down / View: Split Editor Left | `Cmd`+`K` `Cmd`+`\` / `Cmd`+`K` `Opt`+`↓` / `Cmd`+`K` `Opt`+`←` | The tab menu's **Split Up**, **Split Down** and **Split Left** (under **Split & Move**) on the active tab |
| View: Split Editor in Group | `Cmd`+`K` `Shift`+`Cmd`+`\` | The tab menu's **Split in Group** on the active tab |
| View: Move Editor into Group Above / Group Below / Left Group / Right Group | `Cmd`+`K` `Shift`+`↑` / `Shift`+`↓` / `Shift`+`←` / `Shift`+`→` | The tab menu's **Move Above**, **Move Below**, **Move Left** and **Move Right**: the active tab into the group on that side, or a new group there |
| View: Copy Editor into New Window / View: Move Editor into New Window | `Cmd`+`K` `O` / `Cmd`+`K` `Shift`+`O` | The tab menu's **Copy into New Window** / **Move into New Window** on the active tab (macOS only) |
| File: Reveal Active File in Explorer View | `Cmd`+`K` `E` | The tab menu's **Reveal in Explorer View** on the active tab |
| View: Toggle Minimap Side | `Cmd`+`K` `Shift`+`M` | The minimap menu's **Side** rows: the strip to the editor's other edge |
| View: Pin Editor / View: Unpin Editor | `Cmd`+`K` `P` | The tab menu's **Pin** / **Unpin** on the active tab; the palette lists only the one that applies |
| View: Keep Editor | `Cmd`+`K` `Shift`+`P` | The tab menu's **Keep Open**: the preview tab stays when the next file opens (also found as "keep open") |

**Terminal: Search & Replace from Last grep/rg** (palette-only) seeds the Search sidebar from the last `grep`/`rg`/`git grep` command run in the focused terminal (pattern, flags, paths and types) and runs it.

* A search that cannot be reproduced exactly (`-v`, `-L`, `-f FILE`, `-z`, piped input, an unknown type, a path outside the workspace) is refused with the reason.

* An output limit (`| head`, `-m NUM`) is not carried over: the Search panel can list more matches than the terminal showed, so check the results before Replace All.

* The grep/rg search is scoped to the paths and rg `-t`/`-T` types the command searched, resolved from the pane's directory. Case is sensitive unless the command said otherwise.

* Debug: Attach to Python Process uses PEP 768 `sys.remote_exec` (CPython 3.14+).

* View: Close Other Editors in Group is not on VS Code's `⌥⌘T`: iTerm2 keeps that for New Tab and many Linux desktops for a new terminal.

* The Keyboard Shortcuts view shows chords with the same per-platform modifiers as the palette.

* Debug: Add Configuration… asks for name, program, arguments, working directory, environment and pre-launch task to launch; port (Python, Node) or process id (lldb, delve) to attach.

* Debug: Add Configuration…: Enter on a blank optional field skips it; a value that does not fit is refused with the reason and offered back. `.croft/launch.json` is created if needed.

* A key bound in `keybindings.json` to Close Others shows in the tab menu. Bound Pin / Unpin leave a tab already in that state alone.

* View: Show Ports / Show Captures focus the tab, so its keys work at once.

* Search & Replace from Last grep/rg carries `-i`/`-s`/`-S`, `-w`, `-x`, `-F`/`-E`, `-g`; several `-e` patterns become one regex. Case matches the tool (smart case for `-S` and ag).

* In a pipeline or list (`clear; grep -rn x src | head`) it reads the last search. Replace All then covers every seeded match at once (`:cdo`-style).

## Settings, custom keybindings & snippets

All three live under `~/.config/croft/` and open from the Command Palette. Opening one that does not exist yet seeds it with a working example.

| Command | Opens | Notes |
|---------|-------|-------|
| Preferences: Open Settings | a searchable settings hub | Fuzzy-search the toggleable settings; `Enter` flips a toggle. Also opens the Color Theme picker and the JSON files below |
| Preferences: Open Settings (JSON) | `config.json` | Only the keys you set; every other setting stays at its default. Edits apply on the next launch |
| Preferences: Open Keyboard Shortcuts (JSON) | `keybindings.json` | Rebind any palette command; **applies on save** |
| Preferences: Configure User Snippets | `snippets.json` | Define snippets; **applies on save** |

**Custom keybindings.** `keybindings.json` is a JSONC array (`//` and `/* */` comments and trailing commas are fine) of `{ "key": …, "command": … }`:

* `key` is a chord like `ctrl+shift+p`, `cmd+,`, `alt+up`, or `f2`. Modifiers are `ctrl`, `alt`/`opt`, `shift`, `cmd`/`super`, and `mod` (Cmd on macOS, Ctrl elsewhere).

* `command` is a palette command id (see the ids in `Preferences: Open Settings`, e.g. `save_file`, `quick_open`, `toggle_terminal`).

* A bound chord wins over the built-in default for the same chord.

* `when` is optional: `"when": "editor"` fires the chord only while the editor has focus, `"when": "file_tree"` only while the Explorer has focus; there it wins over a row for the same chord with no `when`. `terminal` and `tab_strip` are mouse-only and are refused on a chord with a warning.

Keyboard chords apply while any pane except the terminal is focused, and only when they carry a modifier or are function keys. Mouse gestures bind in the same file under different rules (see [Mouse bindings in `keybindings.json`](#mouse-bindings-in-keybindingsjson)).

In iTerm2, reserved `Cmd` chords must be forwarded first (`croft setup-iterm2`); `Ctrl`/`Alt`/function-key bindings always reach croft. Ghostty forwards everything after `croft setup-ghostty`.

**User snippets.** `snippets.json` uses VS Code's global snippets format (JSONC): an object keyed by name, each with a `prefix` (a string or an array), a `body` (a string or an array of lines), and an optional `scope` (comma-separated language ids).

Type a prefix and press `Tab` to expand it, or pick it from the completion popup and accept with `Enter`/`Tab`. A snippet or file croft cannot read is skipped and named in OUTPUT · Snippets.

In a file no language server takes, completion also offers the words already in the open documents of its language (setting `word_based_suggestions`): Ctrl+Space in a Makefile, a `.env` or a note offers `BUILD_DIRECTORY` once it is typed anywhere.

**Project snippets.** Every `.vscode/*.code-snippets` file and `.croft/snippets.json` in each workspace folder loads too, in the same format. On a prefix both define, your `snippets.json` wins `Tab`; changes to these files apply within a couple of seconds.

The body uses VS Code tab-stop syntax: `$1`, `$2`, … are stops visited in order with `Tab`, `$0` is the final caret, and `${1:name}` seeds a stop with selected placeholder text. A body line's leading `\t` is one indent level, inserted as the file's own indent unit.

Variables: `$TM_FILENAME`, `$TM_FILENAME_BASE`, `$TM_DIRECTORY`, `$TM_FILEPATH`, `$RELATIVE_FILEPATH`, `$WORKSPACE_NAME`, `$WORKSPACE_FOLDER`, `$TM_SELECTED_TEXT`, `$TM_CURRENT_LINE`, `$TM_LINE_INDEX`, `$TM_LINE_NUMBER`, `$CLIPBOARD`, `$LINE_COMMENT`, `$BLOCK_COMMENT_START`, `$BLOCK_COMMENT_END`, `$RANDOM`, `$RANDOM_HEX` and `$UUID`.

Local date and time: `$CURRENT_YEAR`, `$CURRENT_YEAR_SHORT`, `$CURRENT_MONTH`, `$CURRENT_MONTH_NAME`, `$CURRENT_MONTH_NAME_SHORT`, `$CURRENT_DATE`, `$CURRENT_DAY_NAME`, `$CURRENT_DAY_NAME_SHORT`, `$CURRENT_HOUR`, `$CURRENT_MINUTE`, `$CURRENT_SECOND`, `$CURRENT_SECONDS_UNIX`, `$CURRENT_TIMEZONE_OFFSET`.

`$TM_SELECTED_TEXT` is empty; `${TM_SELECTED_TEXT:default}` inserts the default, as any empty variable does. A variable croft does not know becomes a placeholder holding its name.

Not supported yet: transforms such as `${TM_FILENAME/(.*)\\..+$/$1/}`, a picker for choices (`${1|a,b|}` inserts `a`), and live mirroring of a repeated stop.

* The settings paths are the same on macOS and Linux. A new `config.json` starts empty, and the settings hub stays open after `Enter`.

* Plain typing and the terminal's control keys are never shadowed by a custom chord.

* Snippets: an array `prefix` lets one snippet answer to several words; omit `scope` for every language. An unreadable snippet is skipped and the rest still load; a file that does not parse loads no snippets and says why in OUTPUT · Snippets.

* Project snippets on a shared prefix still show in completion. Their file changes apply whether made in croft or by a `git pull`.

* Snippet bodies: continuation lines are re-indented to the caret. Language-server completions that arrive as snippets expand the same way.

* Repeated stops are not mirrored: in `${1:x} = ${1:x}`, typing at the first leaves the second as `x`. Transforms are left unexpanded.

## Explorer (file tree)

| Keys | Action |
|------|--------|
| `↑` / `↓` | Move selection |
| `Enter` | Open a file and focus the editor; on a folder, expand or collapse it |
| `→` | Preview a file without leaving the tree; on a folder, expand it |
| Click a file | Open it in the preview tab and focus the editor; double-click pins the tab. Clicking a folder expands it and keeps focus in the tree |
| `←` | Collapse a folder |
| `Shift`+`↑` / `↓` / `PageUp` / `PageDown` / `Home` / `End` | Extend multi-selection from the anchor row |
| `Shift`+click | Extend multi-selection across a range |
| `Alt`/`Option`+click or `Ctrl`+click | Toggle a single row in or out of the selection |
| `Ctrl`+`A` / `Cmd`+`A` | Select every visible row |
| `Esc` | Clear the multi-selection |
| `Ctrl`+`C` / `Cmd`+`C` | Copy selected paths to the explorer clipboard |
| `Ctrl`+`X` / `Cmd`+`X` | Cut selected paths |
| `Ctrl`+`V` / `Cmd`+`V` | Paste into the focused folder (move on Cut, copy on Copy) |
| `Ctrl`+`Z` / `Cmd`+`Z` | Zoxide jump: pick a directory from a fuzzy popup; croft re-roots the workspace and `cd`s the terminal. If zoxide or its shell hook is missing, the popup offers setup |
| Drag a row onto a folder | Move the selection into it (`Alt`-drag to copy instead) |
| `Delete` / `Backspace` / `Cmd`+`Backspace` | Move every selected path to the OS Trash after a confirmation popup (`Enter` to trash, `Esc` to keep) |
| `Cmd`+`Opt`+`R` (local macOS only) | Reveal the selected entry in Finder |
| Right-click | Context menu: Cut, Copy, Paste, Rename, Delete, Reveal in Finder (local macOS), and New File / New Folder on empty space |
| Click the Explorer root-folder icons | New File, New Folder, Refresh Explorer, Collapse Folders; shown while the Explorer is focused. New File / New Folder are also `Cmd+F` / `Cmd+Shift+N` |
| Click the `⋯` button on the EXPLORER title line | Open the "Views and More Actions" menu: toggle the Explorer's sub-views (Open Editors, Folders, Outline, Timeline, and Dependencies when the root has a recognized manifest). Choices persist across launches |
| Click a row in OPEN EDITORS | Activate that editor's tab (unsaved tabs show a dot) |
| Click the Save All icon on the OPEN EDITORS header | Save All (as `Ctrl+Alt+S` / `Cmd+Opt+S`); shown while the section is expanded |
| Click a commit in TIMELINE | Open that commit's diff for the active file in a read-only editor tab |
| Click a local snapshot in TIMELINE | Diff that local-history snapshot (croft takes one on every save) against the working file |
| Command Palette: `Local History: Restore Snapshot` | Write the snapshot shown in the open TIMELINE diff back over the working file |

* Zoxide jump: the popup ranks directories by frecency and shares one database with the shell's `j` command.

* If zoxide is missing, the popup offers to install it and add the shell hook, showing the exact command and rc file; nothing happens until you press `y`.

* If zoxide is installed but your rc has no `zoxide init` line and its list is empty, the popup offers the hook alone, the same way.

* Root-folder icons sit right-aligned on the root folder row. In the `⋯` menu each shown sub-view has a checkmark.

* OPEN EDITORS highlights the active tab.

## Search sidebar

| Keys | Action |
|------|--------|
| Type | Live `.gitignore`-aware workspace search as you type, with no cap on matches. Unsaved buffers are searched too. The caption counts every occurrence ("4 results in 2 files") |
| Click `Aa` / `ab` / `.*` (inset at the right of the search field) | Toggle case-sensitive / whole-word / regex; active toggles show an accent chip |
| `Alt`+`C` / `Alt`+`W` / `Alt`+`R` | Same toggles from any Search input (palette: "Search: Toggle Match Case" / "Toggle Match Whole Word" / "Toggle Use Regular Expression"). On macOS the terminal must send Option as Alt |
| `Alt`+`P` | Toggle Preserve Case for Replace: a match's capitalisation carries over to its replacement (palette: "Search: Toggle Preserve Case") |
| Click the header refresh icon | Re-run the current query |
| Click the header clear icon | Clear the search query and results |
| Click the left chevron (`▸`/`▾`), or `Ctrl`/`Cmd`+`Shift`+`H` from anywhere | Expand / collapse the Replace row (the chord only opens it). While a replacement is typed, each result row previews the match crossed out beside its replacement |
| Type in Replace, then `Enter` or click the replace-all icon | Confirm "Replace N occurrence(s) across M file(s)?": `Enter`/`Y` rewrites on disk, `Esc` cancels. Files with unsaved changes are skipped and named. Regex mode honours `$1`, `$&` and `$$` |
| Click the `...` icon, or `Alt`+`D` in a Search input | Expand / collapse the "files to include" and "files to exclude" glob inputs (palette: "Search: Toggle Search Details") |
| Type globs into include / exclude | Restrict the search to / from matching files (comma-separated; `*.rs` matches at any depth, a folder name covers everything inside it). An invalid glob shows in red above its box |
| `Tab` | Cycle focus through the visible inputs (search → replace → include → exclude) |
| `↑` / `↓` + `Enter`, or click a result | Open the file at the matched line in the preview tab |
| Double-click a result | Pin its tab, so the next result opens beside it |

* Two matches on one line count as two results, the same count the Replace All confirmation gives.

* `Alt`+`C` / `W` / `R` on macOS: set iTerm2's Option key to "Esc+". A terminal that types `ç` / `∑` / `®` instead keeps typing them.

* Replace All: `$1` is a capture group, `$&` the whole match, `$$` a dollar. Any other `$NAME` or `$N` that names no group stays as typed; `$10` with fewer than ten groups is `$1` then `0`.

* After Replace All the search re-runs.

* Toggle Search Details is `Alt`+`D`: VS Code's `Ctrl+Shift+J` is croft's maximize-terminal.

* Include / exclude globs: a folder name such as `tests` or `src/widgets` covers everything inside it, `*.{ts,tsx}` is one glob, a glob that doesn't compile shows as `invalid glob: …`, an include box with no valid glob matches nothing, and editing re-runs the search live.

## Source Control sidebar

| Keys | Action |
|------|--------|
| Type in the message box | Edit the commit message; the box grows a row per line up to six, then scrolls. Pasted text keeps its line breaks |
| `Shift`+`Enter` / `Alt`+`Enter` | Start a new line in the commit message; `Up` / `Down` move between its lines |
| `Home` / `End` | Start / end of the caret's line in the commit message; with `Ctrl`, the start / end of the whole message |
| `Enter` | Commit with the message: only the staged changes when anything is staged, otherwise every tracked change. Commit & Push (`Ctrl`+`Enter`) and Commit & Sync follow the same rule |
| Click ✓ Commit | Same as `Enter` |
| `Tab` / `Shift`+`Tab` | Step between Source Control's REPOSITORIES (shown while several open folders are repositories), message box, change list and COMMITS, in that order (an empty change list is skipped). In REPOSITORIES: `Up` / `Down` and `Home` / `End` move, `Enter` or `Space` shows that repository, as a click on its row does. In COMMITS: `Up` / `Down`, `Home` / `End` and `PageUp` / `PageDown` pick a commit, `Enter` opens its changes, `Left` / `Right` collapse and expand the section. `R` in REPOSITORIES, the change list or COMMITS refreshes (Command Palette "Git: Refresh") |
| `Tab` / `Shift`+`Tab` in a folder with no repository | Step between the card's buttons (Initialize Repository, or one Open per repository found in a subfolder) and REPOSITORIES; `Up` / `Down` move between the buttons too. `Enter` or `Space` presses the one with the keys, `R` refreshes. Command Palette "Git: Initialize Repository" is the card's Initialize |
| `Tab`, or `Down` on the message's last line | Move to the change list: `Up` / `Down` select, `Enter` opens the diff, `Space` or `S` stages, `U` unstages, `Backspace`, `Delete` or `D` discards (confirms first), `Tab` returns |
| Command Palette "Git: Commit" / "Git: Push" / "Git: Pull" / "Git: Sync" / "Git: Checkout to…" / "Git: Stash" | The Commit button's and the menus' actions, each bindable in `keybindings.json` |
| Command Palette "Git: Open Changes" / "Git: Stage Changes" / "Git: Unstage Changes" / "Git: Discard Changes" | Open the diff of, stage, unstage or discard (confirms first) the open file, or a diff tab's file. In the change list they act on the selected row and their keys are the list's own `Enter`, `S`, `U` and `D`. A file with changes in both sections unstages its staged part and stages, opens or discards its unstaged part |
| Click a change row | Open that file's diff against HEAD in a read-only editor tab, reused on the next click. With unsaved edits, hunk actions ask for a save first |
| `S` in a diff tab | Stage the change hunk under the cursor (click a row or `F7` to pick it), or only the selected lines when a selection spans rows |
| `U` in a diff tab | Unstage the change hunk under the cursor, or only the selected lines when a selection spans rows |
| `R` in a diff tab | Revert the change hunk under the cursor after a `Y`/`N` confirm |
| `F7` / `Shift`+`F7` in a diff tab | Jump to the next / previous change hunk |
| `F7` / `Shift`+`F7` in an editor tab | Jump to the next / previous change bar in the gutter, wrapping. In a file with conflicts, F7 hops conflicts instead |
| Command Palette "Git: Stage Hunk" / "Git: Unstage Hunk" / "Git: Revert Hunk" in an editor tab | Act on the change bar under the caret, against HEAD, without opening a diff. Save first: the hunk is taken from the file on disk |
| Command Palette: `Git: Toggle Inline Blame` | Show/hide the current-line blame annotation (author, age, summary; on by default, persisted) |
| Command Palette: `Git: Open Commit for Line`, or click the inline blame | Open the changes the caret line's commit made to this file in a read-only side-by-side diff. An uncommitted line opens nothing |
| Command Palette: `Git: Copy Commit Hash for Line` | Copy the full hash of the caret line's commit |
| Rest the mouse on the inline blame | Show a card with the commit's author and email, date, short hash and whole message |
| Command Palette: `Editor: Toggle Provenance` | Show/hide who typed each line: a gutter bar coloured per seat (you, navigator, agent, peer, generated) and the seat's name on the inline blame. Off by default, session-only |
| Command Palette: `Diff: Toggle Group by Seat` | In a working-tree diff, mark who wrote each added line: a seat-coloured bar in the right gutter and a per-seat tally in the header. Off by default, per-view |
| `Cmd`+`A` / `Ctrl`+`A`, then `Cmd`+`S` / `Ctrl`+`S` | Select every change, then stage the selection |
| Click `+` on a selected unstaged row | Stage that file |
| Click `↶` on a selected unstaged row | Discard that file (confirms first; deletes untracked files) |
| Click `−` on a selected staged row | Unstage that file |
| Click the branch name | Checkout / Create Branch picker: type to filter, `Enter` to switch, or a new name and `Enter` to create it. A remote branch checks out as a local tracking branch |
| Click the `▾` caret next to Commit | Quick actions menu: Commit & Push, Push, Pull, Sync (Pull, Push), Checkout / Create Branch, Stash, Pop Stash, View Staged Changes, View Changes vs previous, View Changes vs `<default>`. A click outside it closes it and still lands where it falls; `Esc` closes it |
| `Cmd+K` `Shift+C` (Command Palette "Git: Show Commit Actions") | Open the same quick actions menu from anywhere but the terminal, Source Control coming up first: `Up` / `Down` move, `Enter` runs the row, `Esc` closes |
| Click the `⋯` icon in the header | Open the full Source Control actions menu with fly-out submenus (listed below the table); `Esc` closes |
| `Cmd+K` `Shift+A` (Command Palette "Git: Show Source Control Actions") | Open the same `⋯` menu from anywhere, Source Control coming up first: `Up` / `Down` move, `Enter` or `→` opens a fly-out, `←` leaves it, `Enter` runs the row, `Esc` closes. After a click on `⋯`, the first key shows the highlight |
| Right-click a row in COMMITS | The commit's actions: Cherry-Pick, Revert, Checkout (Detached), Create Branch Here, Create Tag Here, Reset Current Branch to Here (Soft / Mixed / Hard), Copy Commit Hash, Copy Commit Message |
| Discard All Changes | Revert every tracked file to HEAD after a `Y`/`N` confirm (untracked files are kept) |
| Click the header refresh icon | Force an immediate git re-scan |
| Stash, Stash (Include Untracked), Stash Staged | Ask for the stash's name, seeded with the commit box's first line: `Enter` names it, a blank name keeps git's `WIP on <branch>`, `Esc` stashes nothing |
| Command Palette "Git: View Stash…" (also **Stash › View Stash**) | Pick a stash and open its changes, untracked files included, as a patch in a read-only tab, without applying or dropping it |

* `Enter` (commit): if a tab under the repository has unsaved edits, every commit and stash asks first: `S`/`Enter` Save All & Commit, `C` Commit Anyway with the files as they are on disk, `Esc` cancels.

* The commit message stays in the box until the commit lands; a hook's rejection shows below it.

* View Changes vs `<default>`: everything this branch changed since it left the default branch, uncommitted edits included.

* `⋯` menu: Pull · Push · Clone · Checkout to · Fetch · Show Git Output, and the submenus below. Actions needing a value open an input; actions choosing from a list open a picker.

* **Commit ›** Commit, Commit Staged, Commit All, Amend, Undo Last Commit, Commit & Push, Commit & Sync.

* **Changes ›** Stage All, Unstage All, Discard All. **Pull, Push ›** Sync, Pull Rebase, Push to, Push Force, Publish Branch.

* **Branch ›** Create, Create from, Rename, Delete, Merge, Rebase. **Remote ›** Add, Remove.

* **Stash ›** Stash, Include Untracked, Stash Staged, Apply, Pop Latest, Pop, Drop. **Tags ›** Create, Delete, Push Tags, Delete Remote Tag.

* Undo Last Commit (`git reset --soft HEAD~1`; palette: "Git: Undo Last Commit") puts the commit's changes back in Staged and its message back in the box. It is refused mid-merge or mid-rebase and asks twice when the commit is already pushed.

* Tags: Create asks for an optional message, and one makes an annotated tag; Delete Remote Tag asks before deleting. Palette: "Git: Create Tag", "Git: Push Tags", "Git: Delete Remote Tag".

* COMMITS menu: Reset … Hard warns first and runs when chosen again for the same commit. A cherry-pick or revert that conflicts stops for Git: Continue / Skip / Abort.

* The message box scrolls horizontally when a line outgrows it. `Shift`+`Enter` starts the blank line before the body and trailers.

* Commit: hunks and lines staged by hand go in and the rest stays in the working tree. The commit runs in the background while hooks run; Save All & Commit goes ahead only once every tab is saved.

* Change list: `Up` on the first row also returns to the message box. Opening Source Control always starts in the message box.

* A change row's diff shows unsaved edits.

* Staging selected lines: unselected additions stay out of the index and unselected deletions stay in it.

* Inline blame trails the cursor's line; its hover card stays while the mouse moves along the annotation.

* Provenance: the seat's name on the blame needs `Git: Toggle Inline Blame` on. Lines with no record show nothing; conflict blocks keep their Accept buttons.

* Group by Seat: lines with no record count as `unrecorded`; unchanged lines are not marked.

* Branch picker: `↑`/`↓` navigate. A remote branch (`upstream/feat`) tracks the remote you picked, even when another remote has a branch of the same name.

* View Changes vs `<default>` leaves out the default branch's own later commits.

* Push Tags and Delete Remote Tag use the remote Publish Branch would; Delete Remote Tag picks from that remote's tags. Click a submenu row to fly it out.

* COMMITS menu: Revert uses `--no-edit` (git's own message).

* Stash Apply / Pop / Drop pickers show each stash's name, age and files (`stash@{1}: On main: retry tweak · 5 minutes ago · src/net.rs`).

## Editor: text

| Keys | Action |
|------|--------|
| Arrows, Home, End | Navigate (clears any selection). With word wrap on, Home and End go to the wrapped row's start / end first, and the line's on a second press |
| `Shift`+arrows / `Home` / `End` / `PageUp` / `PageDown` | Extend the selection by the same motion |
| `Ctrl`+`←` / `→` (`Opt`+`←` / `→` on Mac) | Move to the start of the previous word / the end of the next one; `Ctrl`+`Shift`+`←` / `→` selects by word |
| `Ctrl`+`Backspace` / `Ctrl`+`Delete` (`Opt`+`Backspace` / `Opt`+`Delete` on Mac) | Delete the word left / right of the caret (a selection is deleted instead). At the line's start or end it joins the neighbouring line |
| `Ctrl`+`Home` / `Ctrl`+`End` (`Cmd` on Mac) | Go to the start / end of the file; with `Shift`, select to there |
| `Shift`+`Alt`+`→` / `Shift`+`Alt`+`←` | Expand / Shrink Selection: grow every cursor to the next meaningful range, and shrink back the same way. Works without a language server |
| `PageUp` / `PageDown` (`fn`+`↑` / `fn`+`↓` on Mac) | Scroll one viewport |
| Two-finger horizontal swipe, or drag the bar | Pan long lines in code files; the cursor pans the view when it passes either edge |
| Markdown soft-wrap | Markdown files wrap long lines onto the next visual row (no horizontal scrollbar); `↑`/`↓` move by visual row |
| Printable char, Enter, Backspace, Delete | Edit (typing or deleting with a selection replaces it) |
| Typing inside a paired tag | Linked editing (servers like vtsls / html-language-server): editing one HTML/JSX tag name renames its pair; one Undo reverts both |
| `Tab` | Indent: a multi-line selection indents every touched line; otherwise inserts to the next tab stop. The status bar's indent pill shows the detected width and tabs-vs-spaces; click it to override |
| `Shift`+`Tab` | Outdent one level, tab-stop aligned, for the current line or every line a selection touches |
| `Alt`+`↑` / `Alt`+`↓` | Move the current line (or selected block) up / down, carrying the cursor and selection |
| `Shift`+`Alt`+`↑` / `↓` | Copy the current line (or selected block) up / down (palette: Copy Line Up / Down). Command ids: `copy_line_up` / `copy_line_down` |
| `Cmd`/`Ctrl`+`Opt`+`↑` / `↓` | Add a cursor above / below the current one (multi-cursor) |
| `Cmd`/`Ctrl`+`Shift`+`Opt`+`↑`/`↓`/`←`/`→` / `PageUp` / `PageDown` | Column (box) selection, like `Shift`+`Opt`+drag: grow the box one row, one column or one screen, with one selection per row |
| View: Toggle Column Selection Mode (palette) | While on, plain `Shift`+arrows and `Shift`+`PageUp` / `PageDown` grow a column selection; the status bar shows "Column Selection" beside Ln/Col |
| `Insert` | Switch between Insert and Overwrite typing for all tabs: in Overwrite a character replaces the one under the caret. The status bar shows ⎁ and the caret becomes a block |
| `Cmd`+`/` / `Ctrl`+`/` | Toggle line comment for the current line or every line the selection touches |
| `Shift`+`Alt`+`A` | Toggle block comment around the selection (languages with a block comment) |
| `Alt`+`Z` | Toggle soft word wrap for this file, until the file is reopened |
| `Alt`+`O` | C/C++: Switch Header/Source: open the header of the C, C++ or Objective-C source, or the source of the header. Command id: `switch_header_source` |
| `Cmd`/`Ctrl`+`Shift`+`V` | Markdown: Toggle Preview: switch the active Markdown tab between source and rendered view; arrows / PgUp / PgDn / Home / End / wheel scroll it |
| `Cmd`+`Shift`+`\` | Go to Bracket: jump to the matching bracket; from inside a pair, to the enclosing close. Brackets in strings and comments are skipped |
| `Cmd`+`Opt`+`\` | Select to Bracket: select the region between the matching brackets, including the brackets themselves |
| `Ctrl`+`T` | Transpose Characters: swap the characters before and after the cursor and step right. Several cursors swap in one undo step |
| `Cmd`+`Opt`+`Shift`+`S` | Convert Indentation to Spaces: replace each tab in leading indentation with one tab width of spaces (4, or 2 in YAML) |
| `Cmd`+`Opt`+`Shift`+`T` | Convert Indentation to Tabs: replace each leading run of one tab width of spaces with a tab, leaving any remainder |
| `Cmd`+`Opt`+`Shift`+`N` | Trim Final Newlines: drop trailing blank lines at the end of the file (always keeping at least one line) |
| `Cmd`+`Opt`+`Shift`+`J` | Join Lines: collapse the selected lines (or the current line with the next) into one, single-spaced |
| `Cmd`+`Opt`+`Shift`+`U` / `L` | Transform the selection (or the word under the cursor) to Uppercase / Lowercase |
| Command Palette "Transform to Title Case" | Capitalise each word of the selection (or the word under the cursor). It has no chord, as in VS Code: `Cmd`+`Opt`+`Shift`+`C` is Copy Relative Path |
| Command Palette "Transform to Snake Case" / "Camel Case" / "Pascal Case" / "Kebab Case" | Rewrite each identifier in the selection (or the word under each caret) as `user_name` / `userName` / `UserName` / `user-name` |
| `Cmd`+`Opt`+`Shift`+`A` / `D` | Sort the selected lines (or the whole file) Ascending / Descending |
| `Cmd`+`Opt`+`Shift`+`W` | Trim Trailing Whitespace: strip trailing spaces and tabs from every line |
| `Cmd`+`Opt`+`=` / `Cmd`+`Opt`+`-` | Increment / decrement the number under the cursor (`+` works for `=`) |
| `Cmd`+`Opt`+`Shift`+`E` | Emmet: Expand Abbreviation: in a markup file, expand the abbreviation before the cursor |
| `Cmd`+`Opt`+`Shift`+`F` | Format Document: reformat the buffer through the language server, as one undo step. In a Prettier project, JS/TS, JSON, CSS, HTML, Markdown and YAML use the project's Prettier |
| `Cmd`+`K` `Cmd`+`F` | Format Selection: reformat only the selected span through the language server, as one undo step. Needs a selection |
| Command Palette "Change Color Presentation" | With the caret on a color value (marked by a `■` swatch): pick an alternative spelling (`#rrggbb`, `rgb()`, `hsl()`, …); applied as one undo step |
| Command Palette "Preferences: Toggle Format on Type" | Off by default. On, typing one of the language server's trigger characters (rust-analyzer: `=`, `.`, `>`, `{`) formats at the caret. Also in Settings |
| `Cmd+K` `F` | Toggle Format on Save: `Cmd+S` formats through the language server before writing (off by default) |
| `Cmd`+`Opt`+`Shift`+`K` | Bookmarks: toggle a bookmark on the cursor's line |
| `Cmd`+`Opt`+`.` / `Cmd`+`Opt`+`,` | Bookmarks: jump to the next / previous bookmark, wrapping around the file |
| `Cmd`+`Opt`+`Shift`+`B` | Bookmarks: clear every bookmark in the file |
| `Cmd+K` `Cmd+M` | Bookmarks: list every bookmark; `Enter` jumps to one |
| Click a breadcrumb | Breadcrumbs: a folder or file crumb lists that folder, a symbol crumb lists the symbols beside it; Enter opens or jumps. Type to filter, Backspace widens, Esc clears then closes |
| `Cmd+Shift+.` / `Ctrl+Shift+.` | View: Focus and Select Breadcrumbs: the last crumb's list; Left / Right move to the neighbouring crumb's list |
| Click a sticky-scroll header | Jump to that pinned scope header's line (class, function) |
| Click the gutter fold chevron | Toggle the fold on that line (`▾` expanded, `▸` collapsed). Also `Cmd+K` `Cmd+L`, and palette "Toggle Fold" / "Fold All" / "Unfold All" |
| Arrow keys over a fold | Up and Down step over a collapsed block; an edit or jump that lands inside one opens it |
| Mouse drag | Select text; every other occurrence of a single-line selection highlights in blue |
| Double-click / triple-click | Select the word / the whole line, newline included; dragging after a triple-click extends the selection line by line |
| Click a line number | Select that line; drag down or up the line numbers to select whole lines |
| `Ctrl`+`C` / `Cmd`+`C` | Copy the selection to the system clipboard |
| `Cmd`+`Opt`+`C` / `Ctrl`+`Opt`+`C` | Copy Path: copy the active file's absolute path to the system clipboard (also on the editor tab right-click menu) |
| `Cmd`+`Opt`+`Shift`+`C` / `Ctrl`+`Opt`+`Shift`+`C` | Copy Relative Path: copy the active file's path relative to the workspace root |
| `Ctrl`+`X` / `Cmd`+`X` | Cut the selection |
| `Ctrl`+`V` / `Cmd`+`V` | Paste at the cursor (replaces any selection) |
| `Ctrl`+`Z` / `Cmd`+`Z` | Undo (typing bursts coalesce; backspace, paste, cut, replace are each one step) |
| `Shift`+`Ctrl`+`Z` / `Shift`+`Cmd`+`Z` | Redo the most recently undone step; a new edit after an undo discards the redo branch (palette: "Redo") |
| `Cmd`+`A` | Select the entire buffer (on Linux `Ctrl`+`A` is line start: the Command Palette's "Select All") |
| `Ctrl`+`f` / `Cmd`+`f` | Inline Find bar, pre-filled from the selection or word under the cursor: `Enter`/`F3` forward, `Shift+Enter`/`Shift+F3` back, `Esc` closes |
| `Ctrl`+`Alt`+`f` / `Cmd`+`Opt`+`f` | Replace in File: `Tab` switches field, `Enter` in the replace row replaces the current match, `Cmd`+`Opt`+`Enter` (`Ctrl`+`Alt`+`Enter` on Linux) replaces all as one undo step; regex mode supports `$1` |
| `Alt`+`L` in the Find bar | Find in Selection: limit matches and replaces to the selection (the caret's line with none); a lit `≡` shows it. Palette: "Find: Toggle Find in Selection" |
| `Ctrl`+`A` / `Ctrl`+`E` | Move to start / end of line |
| `Ctrl`+`K` / `Ctrl`+`U` | Kill to line end / start (yanks to clipboard). On Linux (outside vim mode) and Termux `Ctrl`+`K` is the `Cmd+K` leader; use "Kill to End of Line" or bind `kill_to_end_of_line` |
| `Cmd`+`Enter` (or `Cmd`+`o`) / `Cmd`+`Shift`+`Enter` | Insert Line Below / Above: open a new line under / over the current one, inheriting indent (in a Markdown fence `Cmd`+`Enter` runs the block instead) |
| `Enter` in a Markdown list or quote, or a doc comment | Continue it: bullets repeat, numbered items count up, task boxes return unticked, `>` quotes and doc comments carry over. `Enter` on an empty item ends the list |
| `Ctrl`+`Shift`+`o` / `Cmd`+`Shift`+`O` | Go to Symbol in Editor: fuzzy-search the file's symbols and jump (type `:` then a number to go to a line) |
| `Cmd`+`g` `g` | Go to the top of the file. For line N: `Cmd`+`g` N `g`, `Cmd`+`g` N `Enter`, or `Cmd`+`N` `Cmd`+`g` `g`. `Esc` cancels the chord |
| `Cmd`+`Shift`+`G` | Go to the bottom of the file (with a leading count, that line) |
| `Cmd`+`Shift`+`K` / `Ctrl`+`Shift`+`K` | Delete Line: remove every line touched by a cursor or its selection; yanks to clipboard |
| `Cmd`+`y` `y` | Yank the current line (`Cmd`+`N` `Cmd`+`y` `y` for N lines) |
| `Cmd`+`D` / `Ctrl`+`D` | Add Selection to Next Find Match: select the word under the cursor, then add one occurrence per press (whole words; a selection you made also matches inside other words) |
| `Cmd`+`Shift`+`L` / `Ctrl`+`Shift`+`L` | Select All Occurrences of Find Match: a cursor on every occurrence of the selection (or the word under the cursor) |
| `Shift`+`Alt`+`I` | Add Cursors to Line Ends: one cursor at the end of every line the selection touches |
| `Cmd`+`L` / `Ctrl`+`L` | Expand Line Selection: select the caret's whole line, and one more line per further press. Editor only: a focused terminal keeps `Ctrl`+`L` |
| `Option`+click | Add (or toggle off) a secondary caret at the click |
| `Shift`+`Option`+drag | Column (box) selection: one caret per spanned row |
| `Cmd`+`F2` / `Ctrl`+`F2` | Change All Occurrences of the word under the cursor at once |
| `Esc` | Clear the selection, or collapse multi-cursors back to one |
| `Ctrl`+`Space` | Trigger Suggest: open the completion list at the caret; typing filters, `Enter` / `Tab` accepts, `Esc` closes. Command id: `trigger_suggest`. `"disable_quick_suggestions": true` stops it opening by itself |
| `Cmd`+`.` / `Ctrl`+`.` | Quick Fix: pick one of the language server's code actions at the cursor (auto-import, fix-all, organize imports, refactors); also on the editor right-click menu |
| `Cmd`+`Enter` / `Ctrl`+`Enter` (in a `.http`/`.rest` file; iTerm2 maps `Cmd+Enter` to fullscreen and never delivers it, so use `Ctrl+Enter` there) | Send the request under the caret; the response opens as a tab. Palette: **HTTP: Send Request Under Caret**, **HTTP: Copy Request as curl**, **HTTP: Switch Environment** |
| `Cmd`+`.` / `Ctrl`+`.` inside a merge conflict | Resolve Merge Conflict picker: Accept Current / Accept Incoming / Accept Both (palette: "Merge Conflict: Accept Current / Incoming / Both"). Each conflict's header row has the same buttons |
| `F7` / `Shift`+`F7` in a buffer with conflicts | Jump to the next / previous conflict, wrapping. Clicking a MERGE CONFLICTS entry in Source Control opens the three-way merge editor |
| Command Palette "Merge Conflict: Accept All Current / Incoming" | Resolve every block in the file one way |
| Command Palette "Merge: Complete Merge (stage file)" | Once no conflicts remain: save the buffer and stage the file; refuses while blocks are unresolved |
| Commit while MERGE CONFLICTS lists a file | Refused with "Resolve N merge conflict(s) (…) and stage them before committing". Stage each resolved file (or use Complete Merge) first |
| Command Palette "Merge: Open Merge Editor" | Three-way merge editor: Current and Incoming panes above an editable Result. `F7`/`Shift`+`F7` hop conflicts, `Alt`+`↑`/`↓` scrolls the source panes; Accept buttons or `[ ]` checkboxes choose sides |
| Command Palette "Merge: Show Base" | Toggle the common-ancestor pane between Current and Incoming in the merge editor |
| Command Palette "Debug: Add Watch Expression" | Add a WATCH row in Run and Debug (also its "+ Add Expression" row), re-evaluated at every stop. `✕` removes a row; "Debug: Remove All Watch Expressions" clears all |
| `F2` | Rename Symbol across every file it touches (open tabs are edited in memory and stay unsaved). Command id: `rename_symbol` |
| `F12` / `Ctrl`+click | Go to Definition (`Shift`+`Ctrl`+click navigates back); on a link (a URL, an import specifier) the click follows it. Command id: `go_to_definition` |
| `q`{a-z} … `q` (vim Normal) | Record a keyboard macro into a register: `qa` starts, a bare `q` stops. A red `● REC @a` badge shows in the status bar while recording |
| `@`{a-z} / `@@` (vim Normal) | Replay a register, or repeat the last macro; `3@a` runs it three times. Replay aborts on the first key that errors; a replay longer than 100,000 keys is refused |
| Command Palette "Macro: Start/Stop Recording" | The same recorder without vim mode; its macro is the one Replay Last and `@@` run. Registers and the palette share one store |
| Command Palette "Macro: Replay Last" / "Macro: Replay N Times…" | Replay once, or prompt for a count. Replay aborts on the first key that errors; a replay longer than 100,000 keys is refused |
| `Ctrl`+`-` | Go Back: return to the location before the last navigation jump or file switch (also on the editor right-click menu) |
| `Ctrl`+`Shift`+`-` | Go Forward along the trail Go Back walked. Terminals without the extended keyboard protocol send `Ctrl`+`-`; use the palette there |
| `Cmd+K` `Cmd+Q` | Go to Last Edit Location: jump to the most recent edit across the open editors |
| `Alt`+`F12` | Peek Definition: an excerpt popup at the caret; `Enter` jumps to the definition, `Esc` closes (palette: "Peek Definition") |
| `Alt`+`Shift`+`F12` | Peek References: the same popup for each reference to the symbol at the caret; `Up` / `Down` step, `Enter` jumps, `Esc` closes |
| `Ctrl`+`F10` | Debug: Run to Cursor. With the debugger paused, resume and stop once at the caret line, without leaving a breakpoint behind |
| `Shift`+`F12` | Go to References (project-wide; one use jumps, several open a picker). Command id: `go_to_references` |
| `Ctrl`+`Shift`+`F12` | Go to Declaration (where the server implements it; hidden for TypeScript). Command id: `go_to_declaration` |
| `Ctrl`+`F12` | Go to Type Definition. Command id: `go_to_type_definition` |
| `Cmd`+`F12` | Go to Implementations of a trait / interface. Command id: `go_to_implementations`. On Linux use `Super`+`F12` or the palette |
| Hover or click/tap (300 ms rest) | Hover popup: any diagnostic at the point first, then type / signature info. On touch screens (Termux), tap an identifier and rest |
| `Cmd+K` `Cmd+I` | Show or Focus Hover for the symbol or problem at the caret; the next key closes it. Command id: `show_hover`. If the terminal sends `Ctrl`+`I` as `Tab`, use the palette |
| Hover a tab (300 ms dwell) | Tooltip with the tab's full path (tells two same-named files apart) |
| Hover a chrome control (300 ms dwell) | Hint naming the control under the pointer: activity-bar icons, the EXPLORER header toolbar, the Remote / Source Control header actions, and the SEARCH panel's actions and toggles |
| Right-click | Editor symbol menu: the Go to / Rename / Change All actions above |
| `Cmd`+`E` | Toggle vim mode (see below). On Linux use `Super`+`E` or the Command Palette's "Toggle Vim Mode" |
| `Ctrl`+`W` / `Cmd`+`W` | Close the active editor tab (no-op on the last tab). Unsaved changes ask first: `S` / `Enter` saves and closes, `D` closes without saving, `Esc` / `N` cancels |

* Arrows: `Down` on the last line goes to its end and `Up` on the first line to its start.

* `Tab`: the stop width and tabs-vs-spaces are detected from the file on open (VS Code's `editor.detectIndentation`), falling back to the language default (4, or 2 in YAML).

* Format Document: a Prettier project has a Prettier config above the file and `node_modules/.bin/prettier` installed. Format on Save uses Prettier the same way.

* `Enter` in a doc comment: in Rust, JS/TS, C/C++, Java and C#, `Enter` after `/**` opens the block with ` * ` and closes it with ` */`; Rust `///` / `//!` lines continue. Plain `//` comments and fenced code blocks are left alone.

* `.http` / `.rest` files: requests are separated by `###`. `{{variables}}` come from the file's `@name = value` lines, a `.http.env.json` (or `http-client.env.json` and `http-client.private.env.json`) beside the file, or `{{$env.NAME}}`; a variable with no value refuses to send.

* HTTP env files may be flat or split into named environments, `{"$shared": {…}, "dev": {…}}`; `$shared` always applies. The request lands in command history with its variables unresolved.

* Merge editor: non-overlapping edits are resolved for you. Both boxes checked is the combination; "Merge: Accept Combination (Incoming First)" flips the order, "Merge: Ignore (Keep Base)" keeps the ancestor text, and "Reopen as Text" shows the markers.

* Macros: a terminal paste into the editor is recorded and replays as the same paste. Each replay iteration is its own undo step.

* `Ctrl`+`Space`: the list also opens when you begin a word in code (not in a string, a comment or Markdown) and on the language server's trigger characters, such as `.`.

* Close Others, Close to the Right and Close All keep unsaved tabs open and say how many they kept. The tab's `✕`, its context menu's Close and View: Close Editor ask as `Ctrl`+`W` does.

* `Shift`+`Down` / `Shift`+`Up` select through the ends of the file. With word wrap on, `Ctrl+A` / `Ctrl+E` always go to the line's start / end.

* `Ctrl`+`Backspace` / `Ctrl`+`Delete` delete as far as `Ctrl`+`←` / `→` would move.

* Expand Selection: a plain-text file grows line → buffer. Any edit, click or caret change restarts it.

* Linked editing drops when the caret leaves the tag or the edit stops looking like a tag name (space, `>`, newline).

* Column selection: typing edits the same columns of every line. The box starts at the caret and keeps growing while the carets are the ones it made.

* Overwrite: at the end of a line a character appends; a selection is replaced as usual. The ⎁ sits after the workspace name at the status bar's left end.

* Line comments are language-aware and sit at the block's common indent. `Alt`+`Z` overrides the per-language wrap default.

* Switch Header/Source looks beside the file, across an `include/` ↔ `src/` split, then for the same-named file elsewhere that shares the most folders.

* Markdown preview renders headings, emphasis, lists, quotes, tables, links and fenced code with syntax colours. It opens at the part of the document the source showed and returns there.

* With source and preview side by side, the other pane follows as you scroll and type (`Cmd+K` `V` sets that up). A preview stays a preview when the file changes on disk.

* Go to Bracket: from an opening bracket to its close, and vice versa.

* Transpose: at the end of a line the last character moves across the line break.

* Case transforms split identifiers at `_`, `-` and case changes (`parseHTTPResponse` is `parse_http_response`).

* Format Document uses the server's formatter (rustfmt, ruff, …) and leaves the tab unsaved. Format Selection without a selection refuses with guidance.

* Colour swatches appear only for languages whose server reports colours.

* Format on Type is VS Code's `editor.formatOnType`; it does nothing for a language whose servers list no trigger characters.

* Breadcrumbs show the file path and the enclosing symbol trail at the caret. Filtering is case-insensitive and shows on the list's top border.

* A collapsed fold opens for an edit landing inside it from search, Go to Definition or Go to Line.

* Find highlights the active match in orange and the rest in yellow. Replace advances to the next match; Find in Selection also limits the count.

* Insert Line Below / Above does not split the current line.

* List continuation: `-`, `*` or `+` bullets, `1.` then `2.`, `- [ ] ` task boxes. `/**` closes with ` */` only when nothing below does; `Enter` on a ` * ` line adds the next one.

* Go to line: `Cmd`+`g` N `Enter` is VS Code's Go to Line (`Ctrl`+`G` `25` `Enter`). After N any other key cancels the chord, swallowed with the digits rather than typed.

* Expand Line Selection grows every cursor's selection; VS Code's `expandLineSelection` imports onto it.

* Suggest triggers include TypeScript's `"` and `/` in an import path. `Esc` closes the list for the rest of the word.

* HTTP responses show status, timing and headers as comments, pretty-printed JSON, and rendered images. **HTTP: Switch Environment** picks the environment applied on top of `$shared`; secrets stay in the env file.

* Merge conflicts are tinted green (current) and blue (incoming); a narrow row shows short button labels. The status counts "Conflict k of n".

* Complete Merge moves the file from MERGE CONFLICTS into the staged changes.

* Commit is refused during a merge because Commit, Commit All, Commit & Push and Commit & Sync can stage on your behalf, which would record the `<<<<<<<` markers.

* Kill to End of Line on Linux: bind `ctrl+k` to `kill_to_end_of_line` in `keybindings.json`.

* Merge editor: the Result is the real buffer, so language features, undo and save apply. Editing it by hand marks that conflict resolved; the header counts "k/n resolved" and Complete Merge refuses until n/n.

* Watch: a changed value highlights and a rejected expression reads `<not available>`. The list lasts for the croft run; values reset when a debug session ends.

* Rename Symbol pre-fills the server's exact symbol range, and an invalid position fails with the server's own message. A reply that arrives after you typed into a file it edits is refused, with a status saying so.

* Go to Definition into another file keeps the starting tab open, promoting it from preview; Go Back and Go Forward do the same.

* Links: web targets open in the system browser (your own browser in a remote session) and `file://` targets in the editor; any other scheme is refused.

* Macros: the gesture that starts or stops a recording is not recorded. A paste into another pane is skipped and noted in OUTPUT's Macros channel. An aborted replay leaves earlier iterations applied.

* Go Back also returns to the file and line you left when Quick Open, a Search result, the Explorer or a tab switched files.

* Go to Last Edit Location (`Cmd+K` `Cmd+Q`) is distinct from the navigator's `Cmd+K` `Q`.

* Peek popups: a `path:line` header, numbered lines, the definition marked `▶`; references are titled "Reference 2 of 7" and wrap. Any other key closes the popup and keeps its meaning.

* Hover appears after a 300 ms rest; releasing a press keeps it open. Show Hover also works on the word the caret ends; plain `Cmd+K` `I` stays broadcast input.

* Control hints cover the EXPLORER toolbar (New File, New Folder, Refresh Explorer, Collapse Folders, Views and More Actions) and the SEARCH actions and toggles (Refresh, Clear Search Results, Match Case, Use Regular Expression, …).

## Editor: vim mode (modal editing)

`Cmd`+`E` toggles a built-in modal layer over the editor, covering the common vim subset, the same local and remote. For full vim with your own plugins, run `nvim` in the shell pane.

The toggle works from any pane and stays on as you switch files. It is saved as `vim_mode` in `config.json`; Settings has the same switch as **Editor: Vim Mode**, and `croft import-vscode` turns it on when VSCodeVim is installed.

A mode pill (`NORMAL` blue, `INSERT` green, `VISUAL` purple) and the active `:`/`/` line show in the status bar. `Cmd`/`Ctrl` shortcuts keep working in Normal mode.

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
| `d` `y` `c` + motion / text object | Delete / yank / change over a motion (`dw`, `d$`, `dfx`) or text object (`diw`, `ciw`, `daw`). `cw` on a word keeps the space after it |
| `dd` `yy` `cc` | Linewise delete / yank / change |
| `p` `P` | Paste after / before |
| `u` | Undo. A change and everything typed before its `Esc` undo as one step |
| `Ctrl`+`r` | Redo |
| `v` `V` | Charwise / linewise Visual; a motion extends, `d` `y` `c` operate |
| `/` `?` then `Enter`, `n` `N` | Search forward / back, jump to next / previous match |
| `:w` `:q` `:wq` `:x` `:q!` `:qa`, `:{n}` | Write, close tab, write-and-close, quit-all, or jump to line n. `:q` and `:qa` ask about unsaved changes; `:q!` and `:qa!` discard them without asking |

* The toggle works with no file open, and the next launch starts in the mode you left.

* `u`: `cw`…, `ciw`…, `cc`…, `o`…, `i`… each undo as one step.

* `:wq` whose write did not land asks about unsaved changes, as `Cmd+W` and `Ctrl+q` do.

Not supported: `.` (repeat the last change) and `>>` / `<<` (indent). Pressing them says so in the status bar.

When vim mode is on it supersedes the always-on `Cmd`+`d` `d` / `Cmd`+`g` `g` chords; turn it off to get those back.

## Editor: previews

Image tabs (`.png`, `.jpg`, `.jpeg`, `.gif`, `.bmp`, `.webp`, `.ico`, `.tif`, `.tiff`) are read-only; every keystroke is swallowed.

**Rendered log (`.log` / `.ansi` / `.out` / `.txt` / extension-less files whose first 8 KiB carry ANSI colour)**

| Keys | Action |
|------|--------|
| `Up` / `Down`, `PageUp` / `PageDown`, `Home` / `End` (also with `Ctrl`) | Scroll the log a line, a screen, or to the top / the tail. Read-only; it reloads when the file changes, and a view at the tail follows it |
| Highlighting (palette toggle) | Colour plain lines with [tailspin](https://github.com/bensadeh/tailspin): dates, numbers, IPs, URLs, paths, severity keywords and more. "Log: Toggle Highlighting (tailspin)" switches it (setting `disable_log_highlight`, also in Settings) |

* The rendered view opens by itself only for a file carrying ANSI colour in its first 8 KiB. An uncoloured `.log` stays an editable file, coloured the same way in the editor (up to 4 MB).

* "File: Reopen as Preview" opens any plain text file with no grammar in the rendered view; "File: Reopen as Text" goes back.

* Highlighted: dates, durations, numbers, UUIDs, IPs, URLs, paths, quotes, severity keywords (ERROR/WARN/INFO/DEBUG), booleans and null, HTTP methods. Lines that carry their own colours are left as printed.

* An uncoloured `.log` shows as "Log" in the status bar. Reopen as Preview sticks for the tab across reloads. The highlighting toggle applies to every open log and logs opened later.

* A truncated log clamps to what is left.

**PDF (`.pdf`)**

| Keys | Action |
|------|--------|
| `→` / `↓` / `Page Down` / `Space` | Next page |
| `←` / `↑` / `Page Up` | Previous page |
| `Home` / `End` | First / last page |
| Wheel down / up over the page | Next / previous page |
| Click a link on the page | Open it: an external URL opens in the browser, an internal target flips to its page (needs `pdftohtml` from poppler; only links anchored to text are detected) |

**Spreadsheet (`.csv`, `.tsv`, `.xlsx`, `.xls`, `.xlsb`, `.ods`)**

CSV, TSV and xlsx grids are editable; in xlsx, a formula cell needs a second Cmd+S to confirm its overwrite. xls/ods/xlsb are read-only, and row/column structure commands are CSV/TSV only.

| Keys | Action |
|------|--------|
| `↑` / `↓` / `←` / `→` | Move the cell cursor (viewport follows) |
| `PageUp` / `PageDown` | Move a full viewport vertically |
| `Home` / `End` | First / last column; with `Cmd`/`Ctrl` also first / last row (in a SQLite table, of the whole table) |
| Type a character | Start editing the cell, replacing its value |
| `Enter` / `F2` | Edit the cell in place (caret at the end) |
| `Enter` (while editing) | Commit and move down; `Tab` commits and moves right; `Esc` cancels |
| `Delete` | Clear the cell |
| `Cmd+S` | Save. CSV/TSV: unchanged rows are written back byte for byte, and changed rows are quoted the way the file is. xlsx: only the touched cells are written |
| Click a cell | Select it; a second click on the selected cell opens the editor |
| Palette: Sheet: Insert Row Below / Delete Row / Insert Column Right / Delete Column | Structure edits anchored on the selected cell |
| Wheel down / up over the grid | Pan three rows |
| `Tab` / `Shift+Tab` | Switch worksheet (multi-sheet workbooks) or table (SQLite databases, which browse read-only through this grid) |

* `Cmd+S` in a sheet: after an external change it refuses once and the next `Cmd+S` overwrites. An xlsx formula cell is held back separately; the `Cmd+S` after its refusal replaces the formula. TSV is never quoted.

* xlsx edits preserve styles, widths and untouched formulas. A changed or added CSV row is quoted as the file is: every field, every field but numbers, or only cells that need it.

* SQLite: `Cmd`/`Ctrl`+`Home` / `End` load the table's first or last 500-row page.

**Hex viewer (any binary file; `File: Reopen as Hex` for the rest)**

Binary files open here automatically, at any size. `File: Reopen as Text` switches back when the content is text.

| Keys | Action |
|------|--------|
| `←` / `→` | Previous / next byte (`Shift` extends the selection) |
| `↑` / `↓` | One row up / down (`Shift` extends) |
| `PageUp` / `PageDown` | One viewport up / down (`Shift` extends) |
| `Home` / `End` | Start / end of the row; with `Cmd`/`Ctrl`: start / end of the file |
| `Cmd+F` (`Ctrl+F`) | Find bytes: hex pairs (`de ad be ef`) or literal text. To find hex-looking text (`1234`, `cafe`) as text, use double quotes (`"1234"`) or search from the text column (`Tab`) |
| `F3` | Find next match (wraps; selects the match) |
| `0-9 a-f` (hex grid) / any character (ASCII gutter) | Overwrite the byte under the cursor; two hex digits complete a byte and advance. Pending edits tint amber until saved |
| `Tab` | Switch typing between the hex grid and the ASCII gutter |
| `Cmd+Z` / `Cmd+Shift+Z` | Undo / redo byte edits |
| `Delete` | Revert the byte under the cursor to its on-disk value |
| `Cmd+S` | Write the changed bytes in place (never rewrites the whole file); refuses once if the file changed on disk, twice overwrites |
| `Esc` | Clear the selection and any half-typed nibble |
| Click / drag (hex grid or ASCII gutter) | Park the cursor / select a byte range |
| Wheel | Scroll three rows |

* Hex find: the status line says whether hex or text was searched for.

**Archive browser (`.zip`, `.jar`, `.whl` and other zip-based packages, `.tar`, `.tar.gz` / `.tgz`, `.tar.xz` / `.txz`, `.tar.bz2` / `.tbz2` / `.tbz`, `.tar.zst` / `.tzst`; misnamed containers detected by content)**

| Keys | Action |
|------|--------|
| `↑` / `↓` / `PageUp` / `PageDown` / `Home` / `End` | Move the member selection |
| `Enter` | Extract the selected member to a scratch copy and open it with its real viewer |
| `E` | Extract the selected member to a folder you pick |
| Click a member | Select it; a second click opens it |

**Audio / video (`.mp3`, `.wav`, `.flac`, `.m4a`, `.mp4`, `.m4v`, `.mov`)**

Open as a read-only info card: format, codec, duration, dimensions, sample rate, channels, bitrate and tags, plus a poster frame for video when `ffmpeg` is installed. `Media: Open in System Player` (palette) plays the file.

**Word documents (`.docx`, `.odt`, `.doc`)**

Open as a rendered read-only document: headings, emphasis, lists, tables and embedded pictures. `File: Reopen as Text` moves on to the underlying container (archive browser / hex).

A Word 97-2003 `.doc` shows its text, headings, lists and tables, without emphasis or pictures. A password-protected or Word 95 document says so instead.

**Jupyter notebooks (`.ipynb`)**

Open as a rendered document (markdown cells, highlighted code with `In [n]` frames, outputs including inline images). `File: Reopen as Text` shows the raw JSON; `File: Reopen as Preview` returns to the render.

Click a code cell's `▷` to run it in the notebook's kernel. The first run starts the kernel with a Python that has `jupyter_client` and `ipykernel`: the workspace's `.venv` or `venv`, then `$VIRTUAL_ENV`, then `python3`.

The notebook's `metadata.kernelspec.name` picks the kernel. If this machine does not have that kernel, a picker of the installed kernels opens, and the choice is written to `metadata.kernelspec`.

A running or queued cell shows `In [*]`. Outputs are edits to the notebook: `Cmd+S` saves them and `Cmd+Z` takes them back.

Palette: `Notebook: Run All Cells`, `Notebook: Interrupt Kernel` (stops the running cell and keeps the kernel), `Notebook: Restart Kernel` and `Notebook: Select Kernel`. Closing croft shuts the kernels down.

* `File: Reopen as Text` sticks for the tab. A kernel this machine lacks (a `conda-env-…` or a venv kernel from someone else's machine) opens the picker; it starts on the kernel for the notebook's language, and Save keeps the choice.

* Outputs (text, errors, images) stream into the cell. `Notebook: Select Kernel` picks another installed kernel.

**SVG (`.svg`)**

Opens as a rendered preview. `File: Reopen as Text` switches the tab to the editable XML source; an SVG that fails to parse opens as its source.

* The SVG preview renders shapes, text via system fonts, and embedded images. The Reopen as Text choice sticks for the tab; external edits refresh whichever view it shows.

## Run & Debug

Breakpoint debugging. Adapters by language:

* **Python** — debugpy, set up on first use in a private `~/.croft/debug-venv` (CPython 3.14+). The program runs under the project's own interpreter (the nearest `.venv` / `venv` / `.env` in the workspace, else `python3`); a launch.json `python` overrides it.

* **JavaScript / TypeScript** — under Node via **vscode-js-debug**, whose pinned `js-debug` server is downloaded to `~/.croft/js-debug` on first use. `node` must be on `PATH`, and TypeScript binds through source maps.

* **Browser JavaScript / TypeScript** — launch.json `chrome` / `pwa-chrome` and `msedge` / `pwa-msedge` configs start or attach to Chrome or Edge. `url`, `file`, `webRoot` (default: the workspace folder), `runtimeExecutable`, `runtimeArgs`, `userDataDir` and `pathMapping` pass through; an attach with no `port` uses 9222. With no display it runs with `--headless=new`.

* **Rust / C / C++** — `lldb-dap`, from the Xcode Command Line Tools on macOS or from `PATH` elsewhere (`lldb-dap`, `lldb-dap-18`, legacy `lldb-vscode`). On Linux install the LLVM package (e.g. `apt install lldb`); on macOS breakpoint binding also needs a permitted `debugserver`.

* Python: the project interpreter needs no debugpy and no 3.14. When the program stops on an exception, the Run and Debug panel and the status line name its type and message.

* Browser: "no display" means over SSH, or Linux with no `DISPLAY` or `WAYLAND_DISPLAY`; the status says the browser is headless.

* Rust / C / C++: the binary is built on launch.

croft reads **debug configurations** from `.vscode/launch.json` (JSONC) and `.croft/launch.json` (same schema, for repos that don't want a `.vscode/` directory; it also accepts a bare configuration array).

Mapped fields: `name`, `type` (`python`/`debugpy`, `lldb`/`cppdbg`, `node`/`pwa-node`, `chrome`/`pwa-chrome`, `msedge`/`pwa-msedge`), `request` (`launch`/`attach`), `program`, `module`, `args`, `env`, `envFile`, `cwd`, `preLaunchTask`, `stopOnEntry`, `port` (node and python attach), and `processId` (lldb attach). Every other field passes through to the adapter.

* `preLaunchTask` matches a Tasks: Run Task label. The launch waits for exit 0, or, for an `"isBackground": true` watch task, for its first build to end with no errors.

* A `cppdbg` configuration's `environment` and `stopAtEntry` map to `env` and `stopOnEntry`; its gdb-only keys (`MIMode`, `miDebuggerPath`, `setupCommands`, …) have no effect.

* A Python session steps through your own code only unless the configuration sets `"justMyCode": false`.

* A background `preLaunchTask` (such as `tsc --watch` with `$tsc-watch`) keeps running after its first build.

* `cppdbg`: an explicit `env` wins per key, and the debug console names the gdb-only keys that have no effect.

* Adapter-specific keys such as `justMyCode` and `sourceMaps` pass through.

`${workspaceFolder}`, `${file}`, `${fileBasename}`, `${fileBasenameNoExtension}`, `${fileDirname}`, `${fileExtname}`, `${cwd}`, `${pathSeparator}`, and `${env:VAR}` substitute; an unresolvable variable fails the launch with a named error. `console` is forced to `internalConsole`.

**Compounds** (`"compounds": [{ "name": …, "configurations": [...] }]`) are read from the same file and listed in that picker. Selecting one launches every member as its own debug session; "Debug: Switch Session" moves between them.

* Members may be bare names or `{ "folder", "name" }` objects.

* A member that fails to start leaves the others running, and `"stopAll": true` stops the rest when one ends.

* `presentation.hidden` keeps a compound out of the picker; `presentation.group` and `presentation.order` sort it.

* A background member's output reaches the debug console, tagged with its name.

* A compound naming a configuration no launch.json declares reports that member by name. Focus lands on the first member.

* A member's own `preLaunchTask` is skipped, with a status saying so; the compound's task runs first.

* A compound's own `preLaunchTask` is honoured: it runs before the member starts and the launch waits for it to exit 0, chaining with the member's own task if it declares one (VS Code runs both).

* A compound asking only to be hidden launches normally. A `presentation` value of the wrong type (`"hidden": "true"`) is refused and named.

"Debug: Add Configuration…" writes a new entry to `.croft/launch.json`. A `.croft/launch.json` with comments or trailing commas is left alone and the entry is shown for you to paste in; a `.vscode/launch.json` is never touched.

Pick a configuration with "Debug: Select and Start Debugging" or the config row under the Run and Debug panel's button. `F5` and restart launch that pick, and an edited launch.json is re-read on the next `F5`.

Every launch (`F5`, restart, a picked configuration or compound, Debug Test, the Run button) first saves the unsaved editors (VS Code's `debug.saveBeforeStart`), without format on save. A file changed on disk since it was opened is not overwritten; the status line names it.

Set `"disable_save_before_debug": true` (or VS Code's `"debug.saveBeforeStart": "none"`) to launch without saving.

* The debuggee reads its files from disk, while breakpoints follow the buffer. With saving off, the status line names the unsaved files the launch runs without.

| Keys | Action |
|------|--------|
| `F5` | Start debugging (the selected `launch.json` configuration, or the active file when none is selected), or resume when paused |
| `Shift+F5` | Stop the debug session |
| `Shift+Cmd+F5` | Restart the debug session |
| `Ctrl+F5` | Attach to a running Python process |
| `F6` | Pause (interrupt) a running program |
| `Cmd+Opt+Shift+G` | Switch to the next debug session of a compound |
| `F9` | Toggle a breakpoint on the cursor's line (a red dot in the gutter) |
| `Shift+F9` | Add or edit a conditional breakpoint on the cursor's line |
| `Shift+Alt+F9` | Add or edit a logpoint on the cursor's line (amber diamond; prints instead of pausing) |
| `Alt+F9` | Toggle break on raised (not just uncaught) exceptions |
| `F10` | Step over (`Shift+F10` is Show Context Menu, as in VS Code) |
| `F11` / `Shift+F11` | Step into / out (palette: "Debug: Step Into" / "Debug: Step Out"). Command ids: `step_into` / `step_out` |

With the **terminal pane** focused and no debug session live, a bare `F5` / `F9` / `F10` / `F11` goes to the app running in the shell. Modified chords keep their croft meaning everywhere.

The updater's chord is `Ctrl+Shift+F9` (`Cmd+Shift+F9` too); you can also click the status bar's "Update ready" or rebuild pill, or run "Help: Update croft (Relaunch or Rebuild)" from the Command Palette.

It relaunches into a landed update, or rebuilds and reinstalls when croft is older than its source checkout. Otherwise it stages the version the update popup offers, or checks for a newer release.

* `F9` stays Toggle Breakpoint whatever the updater is doing; a breakpoint is pushed live to a running session.

* Updater: "Update ready" relaunches; "⟳ croft abc1234 < repo def5678" rebuilds in the background. With no version on offer it checks now and says what it found, including when there is no internet connection.

* Bare-key forwarding lets process-compose's `F10` Quit and htop's `F9` kill work.

Right-click the editor **gutter** for a breakpoint menu on that line: **Add Breakpoint** / **Remove Breakpoint** and **Add Conditional Breakpoint** / **Edit Condition**.

When paused, the Run and Debug panel shows the **call stack** (click a frame to inspect it), an expandable **variables** tree, and a **debug console** with a `❯` **REPL prompt** that evaluates expressions in the selected frame.

The console is mirrored to the OUTPUT panel's **Debug Console** channel, where it can be searched and copied. Hovering a variable in the editor shows its current value.

The panel's rows take the keys too, as VS Code's VARIABLES, WATCH and CALL STACK lists do:

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` (at the `❯` prompt) | Move the keys to the first / last row. `Tab`, `Shift+Tab`, `Esc` or a click on the prompt hand them back, and typing on a row goes to the prompt |
| `↑` / `↓`, `Home` / `End`, `PageUp` / `PageDown` | Move between rows; section headers are skipped |
| `→` / `←` | Expand / collapse a variable or scope; `←` elsewhere steps to the row it sits under |
| `Enter` / `Space` | What a click on the row does: inspect a frame, expand a variable, jump to a breakpoint, add a watch |
| `Delete` (`Cmd+Backspace` on a Mac) | Stop watching a WATCH row, or remove a BREAKPOINTS row. Plain `Backspace` edits the prompt, as typing does |
| `Shift+F10` / Menu key | The row's right-click menu: Copy Value, Copy as Expression, Add to Watch, Remove Expression, Copy Call Stack |

* Program output keeps its colours (pytest `--color=yes`, chalk, coloured logging) and long lines wrap to the panel's width. The OUTPUT channel shows it at full width, without colour codes.

* The gutter breakpoint menu does not move the cursor.

When the session ends the console output stays on screen. If the program exited without hitting a breakpoint you set, the panel says so.

Gutter marks: the paused line is a yellow `▶`, a breakpoint a red `●`, a conditional breakpoint a red `◆`, and a breakpoint the adapter could not bind a hollow `○`.

Also in the Command Palette: Start / Stop / Pause / Restart Debugging, Toggle Breakpoint, Add Conditional Breakpoint, Continue, Step Over, Step Into, Step Out, Step Into Target (pick which call on the paused line to enter), Toggle Break on Raised Exceptions, and Attach to Python Process.

* Step Into Target: an adapter that cannot list the calls steps into the first. Uncaught exceptions always break.

## Extensions sidebar

Jump here with `Cmd+Shift+X` (or the activity-bar icon, or the Command Palette's "View: Show Extensions"). The panel has three sections:

- **BUILT-IN** — the PDF and CSV viewers, Vim mode, the Color Themes, the language servers (Python, TypeScript, Rust, Go, YAML, JSON, HTML, CSS, Bash, TOML, C, C++) and the debuggers (Python, LLDB, JavaScript), each with a **toggle switch**. Disabling a debugger removes it from F5 for its file types.

- **INSTALLED** — extensions you added (from the catalog below or by dropping a manifest into `~/.config/croft/extensions/<id>/`), also with enable/disable toggles.

- **AVAILABLE** — the curated **MCP server catalog**; **+Add** installs and enables one. Seeded with **Web Fetch**, **Time**, **MarkItDown** (convert a PDF/DOCX/URL to Markdown), and **csvlens** (a terminal spreadsheet viewer for CSV/TSV).

To uninstall an INSTALLED catalog extension, click its **trash button** (🗑) or select the row and press `Delete`; `Enter` on the confirmation popup removes it, `Esc` keeps it. A manifest you added by hand has no trash button and is never deleted; disable it instead.

**MCP sidecar extensions** contribute commands to the Command Palette (`Cmd+Shift+P`). Adding **Web Fetch** gives *"Web: Fetch URL as Markdown"*: it prompts for a URL, fetches it via the `mcp-server-fetch` sidecar and opens the result in a Markdown scratch buffer.

The first time you run a sidecar command, a one-time consent popup shows the exact command croft will run. A tool whose definition has changed since is refused; turning the extension off and on, or uninstalling and re-adding it, approves the new definition.

Disabling the extension removes its commands from the palette.

**Viewer extensions** run a terminal program on a file. Adding **csvlens** gives CSV and TSV files an *Open in csvlens* entry in the Explorer's right-click menu and a *csvlens: Open in csvlens* palette row; either opens the file in a new terminal pane.

Its first run shows the same one-time consent popup. The tool is used from PATH when present; otherwise its pinned release is downloaded.

A filter box at the top narrows all three sections by name, blurb, or id.

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

Disabling takes effect immediately: a disabled PDF/CSV viewer opens that file type as plain text, a disabled Vim makes `Cmd+E` inert, and a disabled language server stops. The choice persists in `~/.config/croft/config.json`.

* Lifecycle: Available → Add → Installed → Enable → provisioned on first use; each step is visible and reversible.

* Toggles are blue when on, grey when off. The trash button sits just left of the toggle; an uninstalled extension drops back to AVAILABLE.

* Hover any row for a tooltip explaining the toggle, `+Add` or the trash button.

* Sidecar servers are installed pinned (via `uv`/`npm`) into croft's managed directory and started on first use. croft queries no remote marketplace.

* A language server turned back on or added starts at once for the files already open.

## Terminal

| Keys | Action |
|------|--------|
| Any key croft does not bind | Forwarded to the shell (arrows, most `Ctrl+letter`, `Alt+x`, function keys) |
| A chord croft binds | **Claimed by croft, whatever pane has focus**; the shell never sees it. Everything else is forwarded. The claimed chords are listed below the table |
| Command Palette: `Terminal: Clear` / `Rename` / `Kill the Active Terminal Instance` / `Split Terminal` / `Create New Terminal (With Profile)` / `Quick Select` / `Enter Copy Mode` / `Show Command History` / `Open Scrollback in Editor` | The terminal's actions as commands, acting on the active pane. A chord bound to one in `keybindings.json` is taken before the shell sees it. Command ids are below the table |
| Command Palette: `Terminal: Run Selected Text in Active Terminal` | Run the editor's selection in the active terminal pane; with no selection, the caret's line, then move to the next non-blank line. Focus stays in the editor. Command id: `run_selected_text` |
| Bare `F5` / `F9` / `F10` / `F11` | Forwarded while the terminal is focused and no debug session is live. Modified, they keep their debug meaning everywhere |
| Mouse drag | Select text; drag past an edge to auto-scroll through history. In a full-screen app that scrolls by repainting, the highlight follows its text |
| `Shift`+click | Extend the existing selection to the clicked cell instead of starting a new one |
| Click / drag inside a full-screen app that tracks the mouse | The app gets the press and release; a drag goes to the app only when it asked for motion. `Shift`+click / `Shift`+drag always selects with croft |
| Mouse wheel | At a shell prompt, scroll the scrollback (any keystroke snaps to the bottom). When a full-screen app tracks the mouse, the wheel scrolls that app; `Shift`+wheel scrolls croft's scrollback |
| `Ctrl`+`B` | Forwarded to the app in the pane while the terminal is focused; the side bar keeps `Cmd`+`B`, the palette and the menu |
| `Ctrl`+`Q` / `Ctrl`+`V` | Forwarded to the app in the pane while the terminal is focused. Quit keeps `Ctrl`+`Q` from every other pane; paste keeps `Ctrl`+`Shift`+`V`, `Cmd`+`V` and the pane menu |
| `Cmd+F` / `Ctrl+F` | Find in terminal: search the active pane's screen and scrollback; `Enter`/`F3` next, `Shift+Enter`/`Shift+F3` previous, `Esc` closes. In a full-screen app it searches the visible screen only |
| `Cmd+Opt+Up` / `Cmd+Opt+Down` (Linux: `Ctrl+Alt`) | Jump to the previous / next command prompt. Needs shell integration, auto-installed for zsh, bash and fish; opt out with `CROFT_SHELL_INTEGRATION=0` |
| Click a gutter dot on the pane's left border | Open that finished command's menu (dot: blue for success, red for a non-zero exit), headed by its exit code and runtime: **Copy Output**, **Select Output**, **Re-run Command** |
| `Cmd+K` `Shift+C` / `Shift+S` / `Shift+R` | For the last finished command in the active pane: copy its output, select its output, re-run it |
| `Ctrl+Shift+Y` | Copy mode: move a cursor with vi keys or arrows; `v` / `V` / `Ctrl+V` select characters, lines or a block; `y` or `Enter` copies and exits, `Esc`/`q` exits |
| `Ctrl+Shift+H` | In the terminal pane: search command history across sessions. Type to filter, `Ctrl+R` cycles the scope (all / this directory / failed only), `Enter` types the pick at your prompt |
| `Cmd+K` `Shift+T` | Reopen the closed terminal: for 10s after closing a pane its process and scrollback stay alive, and this brings it back |
| `Cmd+K` `Shift+W` | Reopen the closed editor tab with its pin state, cursor and scroll, back through the last 20 closes; deleted files are skipped |
| `Ctrl+Shift+Space` | Quick select: every URL, path, git SHA, UUID, IP, hex colour/address and long number gets a label. Type it to copy the match, in UPPERCASE to also paste it |
| `Cmd+Enter` (Markdown; `Ctrl+Enter` on Linux) / click `▷` (preview) | Runnable docs: run the `sh`/`bash`/`zsh`/`fish` fence (and `python`/`node` when on PATH) after a confirm popup showing the block, its pane and directory; `Y` runs it. Details below the table |
+| `Cmd`/`Ctrl`+`K` `Shift`+`M` | User Menu: your own short list of commands from `~/.config/croft/menu.json` (palette: "Preferences: Open User Menu (JSON)" seeds a starter file), opened as a drop-down over the editor; the same commands head the Command Palette, marked with a star, while its query is empty. A row is `{ "command": "<id>" }` (the ids `keybindings.json` uses; `"label"` renames it, `"pinned": false` keeps it out of the palette), `{ "label": "…", "items": [...] }` for a submenu, or `{ "separator": true }`. Only built-in commands are accepted; the file is read each time the menu or palette opens |
| Triggers (no chord) | Output triggers from `~/.config/croft/triggers.json` (palette: "Preferences: Open Terminal Triggers (JSON)", reloaded on save): each rule is a regex plus an action (`highlight`, `notify`, `bell`, `capture`, `redact`). Details below the table |
| Agents: Show Changed Files (palette) | The review queue: one row per agent, with its changed files grouped and counted by workspace root (a worktree lane's group is named by its branch) |
| Agents: Open Agent Lane (palette) | Open the Explorer's AGENT LANE (also in the EXPLORER ⋯ menu): each agent and the files it changed, unreviewed first. Click a file to diff it against its reviewed snapshot |
| Agents: Review a Changed File… (palette) | Pick from every agent lane's files (`●` unreviewed, `+n −m` since review); Enter opens the file's diff against its reviewed snapshot |
| Agent lanes (no chord) | Agents are defined in `~/.config/croft/agents.json` (palette: "Preferences: Open Agent Lanes (JSON)", reloaded on save). Built in: claude, codex, aider, gemini. A row's `launch` is the command a new lane starts |
| Problem matchers (no chord) | Problem matchers from `~/.config/croft/matchers.json` and workspace `.croft/matchers.json` (palette: "Preferences: Open Problem Matchers (JSON)", reloaded on save) turn tool output into PROBLEMS entries. See [MATCHERS.md](MATCHERS.md) |
| `Cmd+K` `I` | Toggle broadcast input: every keystroke and paste goes to every terminal pane. Enabling asks first; receiving panes show a red `⇶` pill; closing down to one pane switches it off |
| `Cmd+K` `Shift+I` | Exclude the active pane from broadcast input (or include it again); it still receives its own typing |
| `Shift+PageUp` / `Shift+PageDown` | Page through the pane's scrollback; `Shift+Home` jumps to the oldest line, `Shift+End` to the live bottom. Depth: 5000 lines by default, `terminal_scrollback` in `config.json` (new panes) |
| Mouse selection (with Copy on Selection) | "Terminal: Copy on Selection" in Settings (VS Code's `terminal.integrated.copyOnSelection`, off by default) copies a finished drag-selection to the clipboard |
| ANSI colors (no chord) | Pane colors use the theme's own 16-color ANSI palette; a theme extension overrides it with a 16-entry `ansi` array in its `[[themes]]` block |
| `imgcat`-style images (no chord) | A program printing an iTerm2 inline image (`imgcat photo.png`) shows the picture at its output row, scrolling with the text. The newest image per pane shows |
| `Cmd+K` `D` | Open the pane's entire scrollback, colours included, in a pinned rendered log tab; the dump is written under `~/.cache/croft/scrollback/`. Output with no colour opens a plain scratch tab instead |
| `Cmd+K` `N` | Annotate the selection: pin a note to the selected output; click the underlined span to read it. `Cmd+K` `N` on a note edits it (blank deletes); `Cmd+K` `Shift+N` deletes. Session-scoped |
| Click the prompt line | Move the shell cursor to the clicked column (zsh, bash and fish). Only at a prompt, only on the cursor's own row |
| Scrolled into long output (no chord) | The command that produced the output pins to the pane's top row with the scroll depth |
| Timestamps (palette toggle) | "Terminal: Toggle Timestamps" shows each row's arrival time (HH:MM:SS) down the right edge; a row that arrived 60s+ after its predecessor is tinted amber |
| Host accents (config) | `host_accents` rules in `config.json` ({"pattern": "prod-*", "accent": "#f14c4c", "badge": "PROD"}) give a pane whose hostname matches an accent border, warning pill and badge; reloaded on saving config.json |
| Progress reports (no chord) | A program reporting progress (OSC `9;4;state;percent`) fills the pane's bottom border as a gauge: blue normal, red on error, yellow paused, a sweeping segment while indeterminate |
| `Cmd+C` / `Ctrl+Shift+c` | Copy the terminal's selection (on a remote session, to the clipboard of the machine you are typing at). In a program that tracks the mouse, **Shift+drag** selects |
| `Cmd+T` / `Ctrl+Shift+t` | Open another terminal beside the current one (each has its own PTY, scrollback, selection) |
| `Cmd+W` / `Ctrl+Shift+w` | Close the active terminal (no-op when one is left; `Ctrl+J` hides the pane) |
| `Cmd+]` / `Cmd+[` | Cycle to the next / previous terminal (or click one). Palette: "Terminal: Focus Next Terminal" / "Terminal: Focus Previous Terminal", from any pane |
| Click the `⌄` caret (beside `+`) | Open the terminal profile menu: pick a shell (from `/etc/shells` + `$SHELL`) to launch a new pane |
| `Cmd+K` `R` | Rename the active terminal pane (a blank name clears it, restoring the auto label) |
| `Cmd+K` `K` | Clear the active terminal's screen and scrollback |
| `Cmd+K` `M` | Maximize the active terminal across the panel; press again to restore the even split |
| Click the up chevron (right end of the panel's tab row) | Grow the whole panel over the editor, as `Ctrl+Shift+j` does; the down chevron restores it. Shown on every panel tab |
| Click the `⛶` button (right end of the panel's tab row, left of the chevron) | Maximize the active pane to the panel's full width; the other terminals move to a rail down the right edge. Shown with two or more terminals |
| Click the `‹` button (left of `-`) | Collapse that pane to a one-column strip. The pane keeps running, and `-` still closes |
| Click a collapsed strip | Give that pane its width back |
| Right-click a collapsed strip | Open the pane menu on the folded pane, which has **Expand Terminal** |
| `Cmd+K` `[` | Collapse the active terminal pane. Collapsing the last expanded pane is refused; `Ctrl+J` hides the whole panel |
| `Cmd+K` `]` | Give every collapsed pane its width back |
| Click a rail row | While maximized: make that terminal the maximized pane |
| Wheel over the rail | Scroll the rail when there are more terminals than it has rows |
| Right-click a terminal pane | Pane menu: **Copy**, **Paste**, **Rename Terminal**, **Clear**, **Split Terminal**, **Kill Terminal**, **Quick Select**, **Copy Mode**, **Command History**, **Open Scrollback in Editor**, **Reopen Closed Terminal**, **Maximize Terminal**, **Collapse Terminal**, **Broadcast Input** |
| `Cmd`/`Ctrl` + click a printed URL | Open it. On a remote session a loopback dev-server URL (`http://localhost:3000`) is forwarded over SSH first, then opened in your local browser. OSC 8 hyperlinks work too |
| `Cmd`/`Ctrl` + click a printed `path:line[:col]` | Open that file in the editor at that line and column (compiler errors, test failures, grep hits, Python tracebacks); relative paths resolve against the pane's current directory |

* Chords croft claims from every pane: `Ctrl` + **F J P S**, and `Ctrl+Shift` + **B C D E F H J L M O P R S T U V W X Y**.

* Also claimed: `Cmd` + **B C E F K P S T V W**, and `Cmd+Shift` + **B C D E F H L M O P R S T U V W X**.

* Forwarded to the app: `Ctrl+A` `C` `D` `E` `K` `L` `R` `U` `W` `Z`, and `Ctrl+B`, `Ctrl+Q` and `Ctrl+V` while the terminal pane is focused. `Cmd+B` reaches croft from anywhere.

* Terminal command ids: `terminal_clear`, `terminal_rename`, `terminal_kill`, `terminal_split`, `terminal_new_with_profile`, `terminal_quick_select`, `terminal_copy_mode`, `terminal_command_history`, `terminal_open_scrollback`. `croft import-vscode` maps VS Code's `workbench.action.terminal.clear` / `rename` / `kill` / `split` / `newWithProfile` chords to them.

* A chord bound to a `Terminal:` command is taken before the shell sees it while the terminal has focus, and shows in the pane's right-click menu. On Linux this is how to clear or rename from the terminal, where `Ctrl+K` goes to the shell.

* A drag goes to apps that asked for motion (vim with `mouse=a`, lazygit); an app that asked for clicks only (Claude Code) keeps croft's drag-to-select. The wheel scrolls apps that track the mouse (Claude Code, htop, vim with `mouse=a`).

* A double-click over an app that tracks the mouse goes to the app, so word-select is not available there. Full-screen apps that don't track the mouse still get arrow keys.

* A command over 10s that finishes in a pane you are not focused on is announced in the status bar.

* Copy mode motions: `h`/`j`/`k`/`l` or arrows, `w`/`b`/`e` words, `0`/`$` line start/end, `g`/`G` oldest/newest line, `Ctrl+U`/`Ctrl+D` half page, `PageUp`/`PageDown` full page.

* Command history records each command's directory, exit code, duration and time; `↑`/`↓` select, `Esc` closes. Plain `Ctrl+R` still reaches your shell's own reverse search.

* Reopen Closed Editor: the file's undo history comes back while it still holds the text it had when it closed (kept for the last 30 closed files).

* Quick select: paths include `path:line:col`; `Backspace` erases a typed char, `Esc` cancels.

* Runnable docs: a `console` / `shell-session` fence, or a shell fence that opens with a `$ ` prompt, runs only its commands; its other lines are sample output.

* Runnable docs: the block runs in a pane named by the document's path (`<file>:<n>` in the popup), in the document's directory (the workspace root with `{cwd=root}`). A shell block of several lines runs as one script, on past a failing line.

* Runnable docs: blocks that look destructive (`rm -rf`, `sudo`, `curl … | sh`, redirects, `nc`, `chmod`, `--force`, `git reset --hard`) or say `{confirm}` turn the popup red. `{run=false}` removes the `▷`.

* Runnable docs: after a block finishes, its last twelve rows of output and a non-zero exit code appear under the fence in the preview. `{persist}` writes that output back into the document; `{timeout=N}` stops the capture after N seconds without stopping the block.

* Triggers: `highlight` recolours every visible occurrence (per-rule `#rrggbb` fg/bg), `notify` posts a status-bar notice (`\0` whole match, `\1`-`\9` capture groups), `bell` posts a bell notice, and `capture` collects the matching line into the CAPTURES tab.

* Triggers: `redact` masks the match (capture group 1 when the regex has one) as `••••`. A click on the mask reveals it; copies carry the real value unless the rule says `"copy": "masked"`, and the `Cmd+K D` scrollback dump is always masked.

* A built-in redact set is on by default (Settings: "Terminal: Redact Secrets"): AWS keys, `sk-`, `ghp_`/`github_pat_`, GitLab `glpat-`, Slack `xox`, Stripe `sk_live_`/`rk_live_` and Google `AIza` keys, JWTs, bearer tokens and the password in a `scheme://user:password@host` URL. "Terminal: Reveal Redacted Secrets for 10s" lifts the masks briefly.

* Triggers: `notify`, `bell` and `capture` fire once per completed output line, and never inside full-screen apps.

* `agents.json`: a row named like a built-in replaces it.

* Problem matchers are regexes with named groups; `applies_to` globs scope them to matching commands. tasks.json `problemMatcher` (`$gcc`, `$tsc-watch`, inline patterns) is honoured when a task runs.

* Pane menu: **Copy** shows while the pane has a selection, **Kill Terminal** with more than one pane, **Reopen Closed Terminal** while one is in its undo window. When active, the last three rows read **Restore Terminal Split**, **Expand Terminal** and **Exclude from Broadcast**.

* Run Selected Text: a block goes as one paste, so a `python3`, `psql` or `node` REPL gets it whole. Repeated runs with no selection step through a script.

* Selection in a full-screen app: rows covered by the app's chrome (an input box, a floating pill) drop out; a row with an overlay in its middle keeps both ends.

* That selection hides only when no rows remain, reappears as the text scrolls back, and copy still yields all of it. A drag held past an edge forwards wheel ticks, so the app scrolls under the drag.

* Scrollback at a shell prompt is 5000 rows. Find highlights the active match in orange and the rest in yellow; a full-screen app never sees `Cmd+F`.

* Prompt navigation parks the prompt at the top of the pane. Shell integration for zsh is a `ZDOTDIR` shim that sources your own dotfiles unchanged.

* Shell integration for bash is the `$ENV` + `--posix` bootstrap on 4.4+; macOS's bash 3.2 reads the same hooks through `--rcfile`.

* Shell integration for fish is a `vendor_conf.d` script that defers to fish 4's native marks and adds hooks only on old fish.

* Copy mode's cursor is a green block. Command history filtering is case-insensitive, duplicates collapse to the newest run, and `Enter` does not run the pick.

* Reopen Closed Terminal: after the 10s window the pane is disposed.

* Quick select labels are short home-row keys; the bottom-most match gets the cheapest.

* Runnable docs: in a transcript the `$ ` prompt (or a `console` fence's root `# `) is stripped. A command continued with `\`, a trailing `|` / `&&`, an open quote or a heredoc keeps its continuation lines; a transcript with no prompt line has no `▷`.

* Runnable docs: every block confirms. A pane sitting idle at a prompt is reused. A block with a control or bidi/zero-width character is never runnable.

* Runnable docs: a block holding a `#` runs as one command, so a comment stays a comment even in zsh without `interactive_comments`.

* Runnable docs: the output note may instead say croft stopped waiting while the block kept running. `{persist}` is the only attribute that changes the file on disk.

* Triggers: `highlight` covers scrollback; click a CAPTURES entry to jump its pane back to that line. The status bar counts masks on screen.

* Built-in AWS redaction covers access key ids, plus the secret access key and session token wherever a variable, config key, JSON field or `aws configure` prompt names them.

* Agents: Show Changed Files: a worktree lane's group is named by its branch (`agent/fix-login 4`), any other root by its folder name (with the parent, or the full path, when names clash).

* A grouped row shows labels and counts (`claude: 6 to review — croft 2; agent/fix-login 4`). A row under one root reads `agent: n to review (a.rs, b.rs)`, with at most three names then `…`.

* A changed file outside every root is counted under "outside the workspace".

* AGENT LANE: clicking an agent's row folds its files. The queue and reviewed marks are kept per workspace across restarts; the section takes no room until an agent has changed something.

* `agents.json` also says which foreground processes are agents and what their prompts look like. Built-ins launch as their own name; a replacing row without `launch` keeps it.

* Problem matchers publish when a command finishes; `background` begins/ends patterns make watch tasks (`tsc --watch`) republish on every recompile.

* Broadcast input (iTerm2's `Cmd+Opt+I`) switches off instantly. In the editor `Cmd+K` `Cmd+I` is Show Hover instead.

* ANSI palette: output looks the same in every host terminal.

* Inline images hide once scrolled off screen; text printed after an image may sit underneath it.

* Scrollback dump (`Cmd+K` `D`): one file per pane, overwritten on each dump; dumping again rebuilds the tab. Find, selection and copy work there.

* A scrollback dump with no colour is a plain scratch tab: vim mode and block selection apply, and File: Save As… keeps a copy.

* Annotations show a dotted-amber underline that scrolls with the content and survives in scrollback.

* The sticky command header unpins when the view crosses into another command's output or returns to the bottom. Late timestamps also get a warning mark.

* Host accents follow the hostname, so SSH inside the pane changes them.

* Progress gauge: with two or more panes the name pill shows the percent. It clears on the program's state-0 report or when the command finishes.

* A program that has not asked for mouse tracking selects on a plain drag.

* Maximize: the `⛶` button becomes a restore glyph, the rail highlights the active terminal, and switching panes scrolls it into view.

* Collapse: nothing is suspended and no output is lost; clicking anywhere on the strip, not only the `›` chevron at the top, expands it. `Cmd+K` `[` is distinct from `Cmd+[`.

* Printed links: non-loopback links open directly, and OSC 8 hyperlinks (`ls --hyperlink`, modern CLIs) work even when the visible text is not the URL. Paths match `src/x.rs:12:5` and `File "x.py", line 3`.

* Forwarded keys let vim enter visual block, nano search backwards and page down, the shell use XON, and Claude Code background a running command.

Collapse and maximize combine: leaving maximize puts the collapsed strips back. Collapse is not saved with the pane layout.

With two or more panes open, each pane's header shows its live foreground process (`zsh`, `vim`, `node`…); a manual rename overrides that label.

The panel's layout survives restarts: the pane arrangement, each pane's directory and name, and the focused pane are saved per workspace and restored as fresh shells.

* The layout is saved on splits, closes, renames, reorders and at quit. A plain single-shell workspace stores nothing.

* Entering maximize ignores the collapse flags rather than clearing them.

## Ports

The PORTS tab in the bottom panel (palette: "View: Show Ports") lists the loopback ports croft has noticed this session, from terminal output and the shell's processes. A newly announced port also raises a click-only toast in the bottom-right corner.

A port drops off the list when it stops listening. On a remote session, forwarding uses the existing SSH connection; on a local session the only action is to open it.

| Keys | Action |
|------|--------|
| `↑` / `↓` | Move the selection |
| `⏎` | Open the selected port in your browser (forwarding it home first on a remote session) |
| `f` | Forward the selected remote port without opening |
| `c` | Copy the selected port's address |
| `x` | On a forwarded port, stop forwarding it; the row stays. Otherwise dismiss the row, which also stops croft re-detecting that port this session |
| Click a port row | Select it; **double-click** opens it in your browser, same as `⏎` |
| Click the toast buttons | `Forward & Open` / `Forward` / `Open` / dismiss, depending on whether the session is remote |

* Ports are found from `http://localhost:PORT` banners and `listening on :PORT` lines, and from a periodic poll of each pane's own processes. A port outside them (one a container publishes) is found from terminal output only.

* Remote forwarding opens no second connection. After `x` on a forwarded port you can forward it again.


## Captures

The CAPTURES tab (palette: "View: Show Captures") collects output lines matched by `capture` triggers in `triggers.json`.

With a row selected, `n` (or right-click → "Ask Navigator About This Line", or the palette's "Captures: Ask Navigator About This Line") sends the line, its trigger and the redacted pane rows around it to the Navigator.

When the line carries a `path:line` that resolves to an existing file, that file opens first and the reply lands as a comment box there; otherwise the ask anchors on the active file.

| Keys | Action |
|------|--------|
| `↑` / `↓` | Move the selection |
| `Enter` / click a row | Jump the capturing pane back to that line: the TERMINAL tab activates and the pane scrolls to and selects the line |
| `n` | Ask the Navigator about the selected line (the redacted pane rows around it are included) |
| `x` | Remove the selected entry |
| `c` | Clear the list |
| `Esc` | Focus the editor |

* A `path:line` resolves relative to the pane's directory, then the workspace. With no file open, the ask says so.

## iTerm2 key mappings

`croft setup-iterm2` forwards these `Cmd` chords to croft and moves the conflicting iTerm2 / macOS menu shortcuts to unused alternates. Fully quit iTerm2 (`⌘Q`) and reopen after running it.

| iTerm2 keystroke | What croft does |
|------------------|-----------------|
| `⌘P` | Quick Open |
| `⌘⇧P` | Command Palette |
| `⌘F` | In-editor Find |
| `⌥⌘F` | Replace in File (find bar with the replace row) |
| `⌥⌘↩` | Replace All while the replace row is open |
| `⌘/` | Toggle line comment |
| `⌘.` | Quick Fix (inside a merge conflict, the Accept Current / Incoming / Both picker) |
| `⌥⌘↑` / `⌥⌘↓` | Add a cursor above / below (multi-cursor) |
| `⌘⇧E` / `⌘⇧F` / `⌘⇧S` / `⌘⇧D` / `⌘⇧R` / `⌘⇧X` | Jump to Explorer / Search / Source Control / Run and Debug / Remote / Extensions |
| `⌘⇧L` | Disconnect a remote session |
| `⌘⇧B` | Run the project's build task (auto-detected); iTerm2's View ▸ Show Toolbelt is relocated to `⌃⌘B` |
| `⌘⇧N` | Explorer "New folder" prompt |
| `⌃⇧J` | Maximize the terminal pane |
| `⌥⌘R` | Reveal in Finder: the selected Explorer entry, or the active editor file when the editor is focused (local macOS only) |
| `⌘B` | Toggle the primary side bar |
| `⌘K` | Leader for the `Cmd+K` chords; iTerm2's Edit ▸ Clear Buffer is relocated to `⌥⌘K` |
| `⌘\` | Split the editor |
| `⌘⇧\` | Go to Bracket (jump to the matching bracket) |
| `⌘⌥\` | Select to Bracket |
| `⌘⌥⇧S` / `⌘⌥⇧T` | Convert Indentation to Spaces / Tabs |
| `⌘⌥⇧N` | Trim Final Newlines |
| `⌘⌥⇧J` | Join Lines |
| `⌘⌥⇧U` / `⌘⌥⇧L` | Transform to Uppercase / Lowercase |
| `⌘⌥⇧C` | Copy Relative Path |
| `⌘⌥⇧A` / `⌘⌥⇧D` | Sort Lines Ascending / Descending |
| `⌘⌥⇧W` | Trim Trailing Whitespace |
| `⌘⌥⇧F` | Format Document |
| `⌘K` `F` | Toggle Format on Save |
| `⌘D` | Add Selection to Next Find Match (multi-cursor) |
| `⌘⇧K` | Delete Line |
| `⌥`+click / `⇧⌥`+drag | Add a caret / column (box) selection |
| `⌥⌘←` / `⌥⌘→` | Focus the left / right editor group |
| `⇧⌥⌘` + arrows | Column (box) selection |
| `⌘F12` | Go to Implementations |
| `⌃⇧F12` | Go to Declaration |
| `⌘S` / `⌘C` / `⌘X` / `⌘Z` / `⌘A` | Save / Copy / Cut / Undo / Select All |
| `⌘⇧Z` | Redo |
| `⌘T` / `⌘W` / `⌘]` / `⌘[` | New terminal / close terminal / cycle terminals |

`⌘V` stays on iTerm2's native Paste, and paste works the same over SSH. Without the setup command the `Ctrl`-based chords still work, and you can map `Cmd` chords by hand in iTerm2 → Settings → Profiles → Keys → Key Mappings (e.g. `⌘S` → Send Hex Code `0x13`).

* `⌘K` leads the `Cmd+K` chords (Color Theme, Close to the Right, compare, Close All, Close Saved, Reveal in Explorer View, Move/Copy into New Window).

## Ghostty key mappings

By default Ghostty's own keybinds (`new_tab`, `goto_tab`, ...) take `⌘T` and `⌘1`..`⌘9`. `croft setup-ghostty` adds a managed `keybind` block to your Ghostty config that forwards every croft chord; reload the config (`⌘⇧,`) or restart Ghostty afterwards.

The config is the first of `config.ghostty` or `config` found in `$XDG_CONFIG_HOME/ghostty` (default `~/.config/ghostty`), then `~/Library/Application Support/com.mitchellh.ghostty`. The command is macOS-only.

The chord set is identical to the [iTerm2 key mappings](#iterm2-key-mappings) above, and `⌘V` stays on Ghostty's native paste. Only the block between croft's marker comments is rewritten on each run.

* Chords include `⌘T` / `⌘W` / `⌘[` / `⌘]`, `⌘1`..`⌘9` / `⌘0`, and the `⌘F12` family.

* With no Ghostty config, a new `config` is created in `~/Library/Application Support/com.mitchellh.ghostty`. Off macOS the command refuses.

## Mouse bindings in `keybindings.json`

Mouse gestures bind in the same array as keys, using a gesture name in `key`
and an optional `when` region:

```jsonc
[
  { "key": "ctrl+click",       "command": "mouse_go_to_definition_at_click" },
  { "key": "alt+click",        "command": "mouse_add_cursor_at_click" },
  { "key": "middle_click",     "command": "search_from_terminal", "when": "terminal" },
  { "key": "alt+wheel_up",     "command": "toggle_word_wrap" }
]
```

**Gestures (six bindable):** `click`, `double_click`, `middle_click`, `right_click`, `wheel_up`, `wheel_down`, each combinable with `ctrl`, `alt`, `shift`. `triple_click` is refused with a warning (see the table below).

**`when` regions:** `editor` (the default when omitted), `terminal`,
`file_tree`, `tab_strip`. The same gesture can mean different things in
different regions.

User bindings resolve *before* the built-in behaviour, so rebinding one takes
it over; a gesture with no binding falls through untouched.

Three commands read the click position and need a mouse binding: `mouse_add_cursor_at_click`, `mouse_go_to_definition_at_click`, and `mouse_open_link_at_click`.

* Run from the palette, they report that they need a mouse binding.

In `tab_strip`, `close_editor`, `close_other_editors`, `close_editors_to_the_right` and `keep_editor` act on the tab under the click. With no binding, a `middle_click` on a tab closes that tab, asking first if it has unsaved edits.

### What croft refuses, and why

A refused row is reported in **OUTPUT · Keybindings** and, on a live reload, summarised in the status bar.

| Refused | Why |
|---|---|
| `cmd+click` / `super+click` | Terminals do not report Cmd/Super with mouse events, so the binding could never fire. Use `ctrl` or `alt`. |
| `triple_click` | Bindings see single and double clicks only (the editor's own triple-click selects a line), so a triple binding would fire on the second click. |
| bare `click` in `editor` or `terminal` | It is how the caret is placed. Bare `click` in `file_tree` or `tab_strip` is allowed. |
| bare `double_click` in `editor` or `terminal` | It is the only way to select a word. A modified double-click (`ctrl+double_click`) is bindable, and bare `double_click` in `file_tree` or `tab_strip` is allowed. |

`mod` means Ctrl for mouse on every platform, unlike keys, where it is Cmd on macOS.

**In terminal panes**, a TUI that has asked for mouse tracking owns the pointer, so user bindings do not fire there; hold `shift` to bypass.

The pane **border** is the exception: a binding there fires however the TUI is tracking.

* On the border, a wheel notch scrolls croft.

`shift` is also part of a gesture's identity: over a tracking TUI a `ctrl+click` binding stays silent, and the row that fires when you hold shift is `ctrl+shift+click`. Bind that spelling for a gesture that works over a full-screen TUI.

