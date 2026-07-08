use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

#[derive(Clone, Copy)]
pub struct ShortcutEntry {
    pub keys: &'static str,
    pub description: &'static str,
    /// Names of the `is_*_key` predicate(s) in `src/app/mod.rs` that this entry
    /// documents, space-separated. Empty for bindings handled outside that
    /// predicate convention (vim modal keys, the F-key debug family, raw
    /// motion). The `every_app_keybinding_predicate_is_documented_in_f1` test
    /// fails the build if any predicate is missing here, so a new shortcut
    /// cannot ship without appearing in this F1 overlay.
    // Read only by the enforcement test, so it is dead code in non-test builds.
    #[cfg_attr(not(test), allow(dead_code))]
    pub handler: &'static str,
}

#[derive(Clone, Copy)]
pub struct ShortcutGroup {
    pub title: &'static str,
    pub entries: &'static [ShortcutEntry],
}

pub const SHORTCUT_GROUPS: &[ShortcutGroup] = &[
    ShortcutGroup {
        title: "Global",
        entries: &[
            ShortcutEntry {
                keys: "Cmd/Ctrl+S",
                description: "Save the open file (also stages selected source-control entry)",
                handler: "is_save_key",
            },
            ShortcutEntry {
                keys: "Ctrl+Q",
                description: "Quit",
                handler: "",
            },
            ShortcutEntry {
                keys: "F6",
                description: "Cycle focus across panes",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+B",
                description: "Toggle the sidebar",
                handler: "is_sidebar_toggle_key",
            },
            ShortcutEntry {
                keys: "Opt+Cmd/Ctrl+B",
                description: "Toggle the secondary side bar (Outline)",
                handler: "is_secondary_sidebar_toggle_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+K Z",
                description: "Toggle Zen Mode (hide all chrome; press again to restore)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Ctrl+J",
                description: "Toggle the terminal pane",
                handler: "is_terminal_toggle_key",
            },
            ShortcutEntry {
                keys: "Ctrl+Shift+J",
                description: "Maximize the terminal pane (collapses editor; press again to restore)",
                handler: "is_terminal_maximize_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+P",
                description: "Quick Open: fuzzy-search workspace files by name",
                handler: "is_file_finder_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+P",
                description: "Command Palette: fuzzy-search and run any named command",
                handler: "is_command_palette_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+O",
                description: "Go to Symbol in Editor: fuzzy-jump to a symbol (or `:` then a line number)",
                handler: "is_go_to_symbol_key",
            },
            ShortcutEntry {
                keys: "Opt+Cmd/Ctrl+M",
                description: "Toggle the editor minimap",
                handler: "is_minimap_toggle_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+K",
                description: "Chord leader: press, then a follow-up key for extra accelerators",
                handler: "is_cmd_k_leader_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+E",
                description: "Jump to the Explorer sidebar",
                handler: "is_explorer_jump_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+F",
                description: "Jump to the Search sidebar",
                handler: "is_search_jump_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+S",
                description: "Jump to Source Control (from any pane)",
                handler: "is_source_control_jump_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+D",
                description: "Jump to Run and Debug",
                handler: "is_run_debug_jump_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+X",
                description: "Jump to the Extensions sidebar",
                handler: "is_extensions_jump_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+R",
                description: "Jump to Remote (SSH)",
                handler: "is_remote_jump_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+L",
                description: "Disconnect a remote session and drop back into the local croft",
                handler: "is_drop_to_local_key",
            },
            ShortcutEntry {
                keys: "Cmd+Shift+T",
                description: "Focus the Terminal pane (un-hides it if collapsed)",
                handler: "is_terminal_focus_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+B",
                description: "Run the project's build task, auto-detected from its manifests (Makefile, package.json, Cargo.toml, …)",
                handler: "is_run_build_task_key",
            },
            ShortcutEntry {
                keys: "Cmd+\\",
                description: "Split the editor into two side-by-side columns (duplicates the active file)",
                handler: "is_editor_split_key",
            },
            ShortcutEntry {
                keys: "Cmd+Opt+Left / Right",
                description: "Move focus to the left / right editor group while split",
                handler: "is_focus_group_left_key is_focus_group_right_key",
            },
            ShortcutEntry {
                keys: "F9",
                description: "When a background update is ready: relaunch croft into the new binary",
                handler: "",
            },
            ShortcutEntry {
                keys: "F1",
                description: "Open this shortcuts panel",
                handler: "",
            },
            ShortcutEntry {
                keys: "Esc",
                description: "Close this panel (or clear selection / dismiss menus)",
                handler: "",
            },
        ],
    },
    ShortcutGroup {
        title: "Explorer",
        entries: &[
            ShortcutEntry {
                keys: "Up / Down",
                description: "Move selection",
                handler: "",
            },
            ShortcutEntry {
                keys: "Enter / Right",
                description: "Open file or expand folder",
                handler: "",
            },
            ShortcutEntry {
                keys: "Left",
                description: "Collapse folder",
                handler: "",
            },
            ShortcutEntry {
                keys: "Shift+Up/Down/PgUp/PgDn/Home/End",
                description: "Extend multi-selection",
                handler: "",
            },
            ShortcutEntry {
                keys: "Alt-click / Ctrl-click",
                description: "Toggle a row in or out of the multi-selection",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+A",
                description: "Select every visible row",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+C / X / V",
                description: "Copy / cut / paste paths in the explorer clipboard",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+F",
                description: "New file in selected folder",
                handler: "is_tree_new_file_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+N",
                description: "New folder in selected folder",
                handler: "is_tree_new_folder_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+R or F2",
                description: "Rename",
                handler: "is_tree_rename_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+/",
                description: "Re-root the workspace at the selected folder (also cd's the active terminal)",
                handler: "is_tree_make_root_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+/",
                description: "Re-root at parent of the selected node (also cd's the active terminal)",
                handler: "is_tree_make_parent_root_key",
            },
            ShortcutEntry {
                keys: "Cmd+Z",
                description: "Jump to a directory via zoxide; re-roots the workspace and cd's the active terminal",
                handler: "is_tree_zoxide_jump_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+D",
                description: "Compare-anchor / diff toggle on file",
                handler: "is_compare_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+R",
                description: "Reveal the selected entry or active file in Finder (macOS)",
                handler: "is_reveal_in_finder_key",
            },
            ShortcutEntry {
                keys: "Delete / Backspace",
                description: "Move every selected path to OS Trash",
                handler: "is_delete_node_key",
            },
            ShortcutEntry {
                keys: "Drag onto folder",
                description: "Move (Alt-drag copies)",
                handler: "",
            },
        ],
    },
    ShortcutGroup {
        title: "Editor: text",
        entries: &[
            ShortcutEntry {
                keys: "Cmd/Ctrl+F",
                description: "Find in current file (Enter next, Shift+Enter prev, Esc close)",
                handler: "is_editor_find_key",
            },
            ShortcutEntry {
                keys: "Cmd+Opt+F / Ctrl+Alt+F",
                description: "Replace in current file (Tab switches field, Enter replaces, Cmd+Opt+Enter all)",
                handler: "is_editor_replace_key",
            },
            ShortcutEntry {
                keys: "Arrows / Home / End",
                description: "Navigate; clears any active selection",
                handler: "",
            },
            ShortcutEntry {
                keys: "Shift+arrows / Home / End / PgUp / PgDn",
                description: "Extend the selection",
                handler: "",
            },
            ShortcutEntry {
                keys: "PgUp / PgDn",
                description: "Scroll exactly one viewport",
                handler: "",
            },
            ShortcutEntry {
                keys: "Alt+Left / Right",
                description: "Word-step left / right",
                handler: "",
            },
            ShortcutEntry {
                keys: "Shift+Alt+Up / Down",
                description: "Copy the current line or selection up / down",
                handler: "",
            },
            ShortcutEntry {
                keys: "Alt+Up / Down",
                description: "Move the current line or selection up / down",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Up / Down",
                description: "Add a cursor above / below (multi-cursor)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+D",
                description: "Add Selection to Next Find Match (multi-cursor on repeats)",
                handler: "is_select_next_occurrence_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+K",
                description: "Delete the current line or every selected line",
                handler: "is_delete_line_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+/",
                description: "Toggle line comment for the line or selection",
                handler: "is_toggle_line_comment_key",
            },
            ShortcutEntry {
                keys: "Shift+Alt+A",
                description: "Toggle block comment around the selection",
                handler: "is_toggle_block_comment_key",
            },
            ShortcutEntry {
                keys: "Alt+Z",
                description: "Toggle word wrap for this file",
                handler: "is_toggle_wrap_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+F",
                description: "Format Document (reformat the whole buffer via the language server)",
                handler: "is_format_document_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+.",
                description: "Quick Fix: code actions at the cursor (auto-import, fixes, refactors)",
                handler: "is_quick_fix_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+C",
                description: "Copy the selection (native system clipboard; OSC 52 fallback on remote)",
                handler: "is_editor_copy_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+C",
                description: "Copy Path: copy the active file's absolute path to the clipboard",
                handler: "is_copy_path_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+C",
                description: "Copy Relative Path: copy the active file's path relative to the workspace root",
                handler: "is_copy_relative_path_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+X",
                description: "Cut the selection",
                handler: "is_editor_cut_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+V",
                description: "Paste at the cursor",
                handler: "is_editor_paste_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Z",
                description: "Undo",
                handler: "is_editor_undo_key",
            },
            ShortcutEntry {
                keys: "Shift+Cmd/Ctrl+Z",
                description: "Redo",
                handler: "is_editor_redo_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+A",
                description: "Select the entire buffer",
                handler: "is_editor_select_all_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+W",
                description: "Close the active tab",
                handler: "is_close_tab_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+1..9",
                description: "Jump to that tab (when no vim chord is pending)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Ctrl+Space",
                description: "Trigger LSP completion",
                handler: "is_completion_trigger_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+E",
                description: "Toggle native modal (vim) editing",
                handler: "is_vim_toggle_key",
            },
            ShortcutEntry {
                keys: "Ctrl+T",
                description: "Transpose the characters around the cursor",
                handler: "is_transpose_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+\\",
                description: "Go to the matching bracket",
                handler: "is_goto_bracket_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+\\",
                description: "Select to the matching bracket",
                handler: "is_select_to_bracket_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+J",
                description: "Join the selected lines into one",
                handler: "is_join_lines_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+U",
                description: "Transform the selection to UPPERCASE",
                handler: "is_transform_upper_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+L",
                description: "Transform the selection to lowercase",
                handler: "is_transform_lower_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+A",
                description: "Sort the selected lines ascending",
                handler: "is_sort_lines_asc_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+D",
                description: "Sort the selected lines descending",
                handler: "is_sort_lines_desc_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+S",
                description: "Convert indentation to spaces",
                handler: "is_indentation_to_spaces_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+T",
                description: "Convert indentation to tabs",
                handler: "is_indentation_to_tabs_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+W",
                description: "Trim trailing whitespace",
                handler: "is_trim_trailing_whitespace_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Opt+Shift+N",
                description: "Trim final newlines",
                handler: "is_trim_final_newlines_key",
            },
        ],
    },
    ShortcutGroup {
        title: "Editor: code navigation",
        entries: &[
            ShortcutEntry {
                keys: "F2",
                description: "Rename the symbol across every file it touches",
                handler: "is_rename_symbol_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+F2",
                description: "Change all occurrences in this file",
                handler: "is_change_all_occurrences_key",
            },
            ShortcutEntry {
                keys: "F12",
                description: "Go to definition",
                handler: "is_go_to_definition_key",
            },
            ShortcutEntry {
                keys: "Shift+F12",
                description: "Go to references (project-wide)",
                handler: "is_go_to_references_key",
            },
            ShortcutEntry {
                keys: "Cmd+F12",
                description: "Go to implementations",
                handler: "is_go_to_implementation_key",
            },
            ShortcutEntry {
                keys: "Ctrl+F12",
                description: "Go to type definition",
                handler: "is_go_to_type_definition_key",
            },
            ShortcutEntry {
                keys: "Ctrl+Shift+F12",
                description: "Go to declaration",
                handler: "is_go_to_declaration_key",
            },
        ],
    },
    ShortcutGroup {
        title: "Editor: line motion",
        entries: &[
            ShortcutEntry {
                keys: "Ctrl+A",
                description: "Move to start of current line (readline-style)",
                handler: "is_editor_line_home_key",
            },
            ShortcutEntry {
                keys: "Ctrl+E",
                description: "Move to end of current line",
                handler: "is_editor_line_end_key",
            },
            ShortcutEntry {
                keys: "Ctrl+K",
                description: "Kill from cursor to end of line",
                handler: "is_editor_kill_to_eol_key",
            },
            ShortcutEntry {
                keys: "Ctrl+U",
                description: "Kill from cursor to start of line",
                handler: "is_editor_kill_to_bol_key",
            },
        ],
    },
    ShortcutGroup {
        title: "Editor: vim chords",
        entries: &[
            ShortcutEntry {
                keys: "Cmd+g g",
                description: "Go to the top of the file",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+g N g",
                description: "Go to line N (digits between the chord keys)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+N Cmd+g g",
                description: "Same as above with the count first",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+Shift+G",
                description: "Go to the bottom (or line N with a leading count)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+d d",
                description: "Delete the current line (yanks to system clipboard)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+N Cmd+d d",
                description: "Delete N lines",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+y y",
                description: "Yank (copy) the current line",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+N Cmd+y y",
                description: "Yank N lines",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+o",
                description: "Open a new line below, inheriting indent",
                handler: "is_editor_open_line_below_key",
            },
            ShortcutEntry {
                keys: "Cmd+Shift+O",
                description: "Open a new line above, inheriting indent",
                handler: "is_editor_open_line_above_key",
            },
        ],
    },
    ShortcutGroup {
        title: "Editor: vim mode (Cmd+E)",
        entries: &[
            ShortcutEntry {
                keys: "i / a / I / A",
                description: "Insert at cursor / after cursor / line start / line end",
                handler: "",
            },
            ShortcutEntry {
                keys: "o / O",
                description: "Open a line below / above and insert",
                handler: "",
            },
            ShortcutEntry {
                keys: "Esc",
                description: "Leave Insert/Visual; clear a pending operator or count",
                handler: "",
            },
            ShortcutEntry {
                keys: "h / j / k / l / arrows",
                description: "Move one cell",
                handler: "",
            },
            ShortcutEntry {
                keys: "w / b / e",
                description: "Word forward / back / end",
                handler: "",
            },
            ShortcutEntry {
                keys: "0 / ^ / $",
                description: "Line start / first non-blank / line end",
                handler: "",
            },
            ShortcutEntry {
                keys: "gg / G / N G",
                description: "File start / file end / go to line N",
                handler: "",
            },
            ShortcutEntry {
                keys: "f / t / F / T then char, ; / ,",
                description: "Find / till a character forward / back, then repeat",
                handler: "",
            },
            ShortcutEntry {
                keys: "N (digits)",
                description: "Count for the next motion or operator (3j, 2dw)",
                handler: "",
            },
            ShortcutEntry {
                keys: "x",
                description: "Delete N characters under the cursor",
                handler: "",
            },
            ShortcutEntry {
                keys: "d / y / c + motion or object",
                description: "Delete / yank / change (dw, diw, caw)",
                handler: "",
            },
            ShortcutEntry {
                keys: "dd / yy / cc",
                description: "Linewise delete / yank / change",
                handler: "",
            },
            ShortcutEntry {
                keys: "p / P",
                description: "Paste after / before the cursor",
                handler: "",
            },
            ShortcutEntry {
                keys: "u",
                description: "Undo",
                handler: "",
            },
            ShortcutEntry {
                keys: "v / V",
                description: "Charwise / linewise Visual; a motion extends, d/y/c operate",
                handler: "",
            },
            ShortcutEntry {
                keys: "/ or ? then Enter, n / N",
                description: "Search forward / back, then next / previous match",
                handler: "",
            },
            ShortcutEntry {
                keys: ":w / :q / :wq / :x / :q! / :qa / :N",
                description: "Write / quit / write-quit / quit-all / go to line N",
                handler: "",
            },
        ],
    },
    ShortcutGroup {
        title: "Editor: PDF preview",
        entries: &[
            ShortcutEntry {
                keys: "Right / PgDn / Space",
                description: "Next page",
                handler: "",
            },
            ShortcutEntry {
                keys: "Left / PgUp",
                description: "Previous page",
                handler: "",
            },
            ShortcutEntry {
                keys: "Home",
                description: "First page",
                handler: "",
            },
            ShortcutEntry {
                keys: "End",
                description: "Last page",
                handler: "",
            },
        ],
    },
    ShortcutGroup {
        title: "Editor: spreadsheet preview",
        entries: &[
            ShortcutEntry {
                keys: "Arrows",
                description: "Pan one row / column",
                handler: "",
            },
            ShortcutEntry {
                keys: "PgUp / PgDn",
                description: "Pan a full viewport vertically",
                handler: "",
            },
            ShortcutEntry {
                keys: "Home",
                description: "Jump to row 1, column 1",
                handler: "",
            },
            ShortcutEntry {
                keys: "End",
                description: "Jump to the last visible page",
                handler: "",
            },
            ShortcutEntry {
                keys: "Tab / Shift+Tab",
                description: "Switch worksheet (multi-sheet workbooks)",
                handler: "",
            },
        ],
    },
    ShortcutGroup {
        title: "Run & Debug",
        entries: &[
            ShortcutEntry {
                keys: "F5",
                description: "Start debugging, or resume from a breakpoint",
                handler: "",
            },
            ShortcutEntry {
                keys: "Shift+F5",
                description: "Stop the debug session",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+Shift+F5",
                description: "Restart the debug session",
                handler: "",
            },
            ShortcutEntry {
                keys: "Ctrl+F5",
                description: "Attach to a running Python process",
                handler: "",
            },
            ShortcutEntry {
                keys: "F6",
                description: "Pause the running program (while a debug session is active)",
                handler: "",
            },
            ShortcutEntry {
                keys: "F9",
                description: "Toggle a breakpoint on the cursor's line",
                handler: "",
            },
            ShortcutEntry {
                keys: "Shift+F9",
                description: "Add or edit a conditional breakpoint",
                handler: "",
            },
            ShortcutEntry {
                keys: "Alt+F9",
                description: "Toggle break on raised (not just uncaught) exceptions",
                handler: "",
            },
            ShortcutEntry {
                keys: "F10",
                description: "Step over",
                handler: "",
            },
            ShortcutEntry {
                keys: "F11",
                description: "Step into",
                handler: "",
            },
            ShortcutEntry {
                keys: "Shift+F11",
                description: "Step out",
                handler: "",
            },
        ],
    },
    ShortcutGroup {
        title: "Terminal",
        entries: &[
            ShortcutEntry {
                keys: "Any key",
                description: "Forwarded to the shell PTY",
                handler: "",
            },
            ShortcutEntry {
                keys: "Ctrl+Shift+C / Cmd+C",
                description: "Copy current selection",
                handler: "is_terminal_copy_key",
            },
            ShortcutEntry {
                keys: "Cmd+T / Ctrl+Shift+T",
                description: "Open another terminal next to the active one",
                handler: "is_terminal_split_key",
            },
            ShortcutEntry {
                keys: "Cmd+W / Ctrl+Shift+W",
                description: "Close the active terminal",
                handler: "is_terminal_close_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+K Shift+T",
                description: "Reopen the closed terminal: within 10s of closing, the pane comes back with its process and scrollback intact",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+[ / Cmd+]",
                description: "Cycle to the previous / next terminal",
                handler: "is_terminal_cycle_back_key is_terminal_cycle_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+K M",
                description: "Maximize the active terminal across the panel (again to restore the split)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Ctrl+Shift+Space",
                description: "Quick select: label every URL / path / hash / IP on screen; type a label to copy it (UPPERCASE pastes), Esc cancels",
                handler: "is_quick_select_key",
            },
            ShortcutEntry {
                keys: "Ctrl+Shift+Y",
                description: "Copy mode: vi keys (h/j/k/l, w/b/e, 0/$, g/G, ^U/^D) walk the scrollback; v/V/^V select char/line/block, y copies, Esc exits",
                handler: "is_copy_mode_key",
            },
            ShortcutEntry {
                keys: "Ctrl+Shift+H",
                description: "Command history: search every command this machine ran (cwd, exit, duration kept); ^R cycles all / this directory / failed, Enter types it",
                handler: "is_command_history_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+K I",
                description: "Toggle broadcast input: keystrokes and pastes go to every pane at once (confirm gate; red ⇶ pills mark receiving panes)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+K Shift+I",
                description: "Exclude / include the active pane from receiving broadcast input",
                handler: "",
            },
            ShortcutEntry {
                keys: "Shift+PageUp / Shift+PageDown",
                description: "Page through the scrollback (Shift+Home jumps to the oldest line, Shift+End back to the live bottom)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd+V / Ctrl+V",
                description: "Paste local clipboard into the shell",
                handler: "is_clipboard_paste_key",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+click a path:line",
                description: "Open that file in the editor at that line (compiler errors, test failures, grep hits, tracebacks)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+click a URL",
                description: "Open the link (a remote dev-server port forwards home first)",
                handler: "",
            },
        ],
    },
    ShortcutGroup {
        title: "Search sidebar",
        entries: &[
            ShortcutEntry {
                keys: "Type",
                description: "Live gitignore-aware workspace search",
                handler: "",
            },
            ShortcutEntry {
                keys: "Click Aa / ab / .*",
                description: "Case-sensitive / whole-word / regex toggles",
                handler: "",
            },
            ShortcutEntry {
                keys: "Up / Down + Enter",
                description: "Open the file at the matched line",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+V",
                description: "Paste into the search field",
                handler: "is_search_paste_key",
            },
        ],
    },
    ShortcutGroup {
        title: "Source Control sidebar",
        entries: &[
            ShortcutEntry {
                keys: "Type",
                description: "Edit the commit message",
                handler: "",
            },
            ShortcutEntry {
                keys: "Enter",
                description: "Commit the staged changes",
                handler: "",
            },
            ShortcutEntry {
                keys: "Cmd/Ctrl+A then Cmd/Ctrl+S",
                description: "Select all changes, then stage them",
                handler: "",
            },
            ShortcutEntry {
                keys: "Click + / -",
                description: "Stage / unstage a file",
                handler: "",
            },
            ShortcutEntry {
                keys: "Click branch name",
                description: "Open the checkout / create-branch picker",
                handler: "",
            },
            ShortcutEntry {
                keys: "S / U / R in a diff",
                description: "Stage / unstage / revert the change hunk under the cursor (revert confirms first)",
                handler: "",
            },
            ShortcutEntry {
                keys: "F7 / Shift+F7 in a diff",
                description: "Jump to the next / previous change hunk",
                handler: "",
            },
        ],
    },
    ShortcutGroup {
        title: "Extensions sidebar",
        entries: &[
            ShortcutEntry {
                keys: "Type",
                description: "Filter by name / description / id",
                handler: "",
            },
            ShortcutEntry {
                keys: "Up / Down",
                description: "Move selection",
                handler: "",
            },
            ShortcutEntry {
                keys: "Space / Enter",
                description: "Toggle on / off (or Add on an available row)",
                handler: "",
            },
            ShortcutEntry {
                keys: "Delete",
                description: "Uninstall the selected installed extension",
                handler: "",
            },
            ShortcutEntry {
                keys: "Esc",
                description: "Clear the filter, or leave the view when it is empty",
                handler: "",
            },
        ],
    },
];

#[derive(Default)]
pub struct ShortcutsModal {
    pub scroll: u16,
    pub last_inner_height: u16,
    pub last_content_height: u16,
    pub last_rect: Rect,
}

impl ShortcutsModal {
    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        for (i, group) in SHORTCUT_GROUPS.iter().enumerate() {
            if i > 0 {
                out.push(Line::from(""));
            }
            out.push(Line::from(Span::styled(
                group.title,
                Style::default()
                    .fg(Color::Rgb(0x4e, 0x9a, 0xff))
                    .add_modifier(Modifier::BOLD),
            )));
            out.push(Line::from(Span::styled(
                "─".repeat(group.title.chars().count()),
                Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)),
            )));
            for entry in group.entries {
                out.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<32} ", entry.keys),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        entry.description,
                        Style::default().fg(Color::Rgb(0xd0, 0xd0, 0xd0)),
                    ),
                ]));
            }
        }
        out
    }

    pub fn scroll_down(&mut self, by: u16) {
        let max_scroll = self
            .last_content_height
            .saturating_sub(self.last_inner_height);
        self.scroll = self.scroll.saturating_add(by).min(max_scroll);
    }

    pub fn scroll_up(&mut self, by: u16) {
        self.scroll = self.scroll.saturating_sub(by);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = self
            .last_content_height
            .saturating_sub(self.last_inner_height);
    }
}

pub fn render_shortcuts_modal(
    modal: &mut ShortcutsModal,
    area: Rect,
    buf: &mut Buffer,
    gradient: bool,
) {
    let width = area.width.saturating_mul(8) / 10;
    let width = width.clamp(40, 110.min(area.width));
    let height = area.height.saturating_mul(8) / 10;
    let height = height.clamp(10, area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let rect = Rect {
        x,
        y,
        width,
        height,
    };
    modal.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        " Shortcuts — Esc/q to close, ↑/↓ PgUp/PgDn Home/End to scroll ",
        Style::default()
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(title.clone())
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    // Black theme: gradient border over the solid one, then re-stamp the title.
    if gradient {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }

    let paragraph = Paragraph::new(modal.lines()).wrap(Wrap { trim: false });
    // Count the rows AFTER wrapping, not the logical-line count: a description
    // that wraps onto a second visual row adds a row the scroll ceiling must
    // account for, otherwise the bottom of the list is unreachable.
    modal.last_content_height = paragraph.line_count(inner.width) as u16;
    modal.last_inner_height = inner.height;
    let max_scroll = modal.last_content_height.saturating_sub(inner.height);
    if modal.scroll > max_scroll {
        modal.scroll = max_scroll;
    }
    Widget::render(paragraph.scroll((modal.scroll, 0)), inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_groups_cover_every_pane_the_user_can_focus() {
        let titles: Vec<&str> = SHORTCUT_GROUPS.iter().map(|g| g.title).collect();
        for required in [
            "Global",
            "Explorer",
            "Editor: text",
            "Editor: line motion",
            "Editor: vim chords",
            "Terminal",
            "Search sidebar",
        ] {
            assert!(
                titles.contains(&required),
                "shortcuts panel is missing a section for {required}; new panes / chord layers must be discoverable here so the user does not have to grep the source for the binding"
            );
        }
    }

    #[test]
    fn no_group_is_empty_because_an_empty_section_is_just_chrome() {
        for group in SHORTCUT_GROUPS {
            assert!(
                !group.entries.is_empty(),
                "group '{}' has no entries; remove the group or fill it in",
                group.title
            );
        }
    }

    #[test]
    fn lines_contain_every_keys_label_so_the_user_can_find_any_binding_in_the_modal() {
        let modal = ShortcutsModal::default();
        let rendered = modal
            .lines()
            .into_iter()
            .flat_map(|l| l.spans.into_iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        for group in SHORTCUT_GROUPS {
            for entry in group.entries {
                assert!(
                    rendered.contains(entry.keys),
                    "rendered modal is missing the keys label '{}' from group '{}'; the lines() builder must surface every entry so scrolling reaches everything",
                    entry.keys,
                    group.title
                );
            }
        }
    }

    #[test]
    fn scroll_down_is_clamped_so_the_view_cannot_overshoot_the_last_visible_line() {
        let mut modal = ShortcutsModal {
            scroll: 0,
            last_inner_height: 10,
            last_content_height: 25,
            last_rect: Rect::default(),
        };
        modal.scroll_down(100);
        assert_eq!(
            modal.scroll, 15,
            "scroll must clamp at content_height - inner_height (25 - 10 = 15) so the last line stays visible at the bottom of the viewport"
        );
    }

    #[test]
    fn scroll_up_clamps_at_zero_so_the_view_cannot_undershoot_the_first_line() {
        let mut modal = ShortcutsModal {
            scroll: 5,
            last_inner_height: 10,
            last_content_height: 25,
            last_rect: Rect::default(),
        };
        modal.scroll_up(100);
        assert_eq!(modal.scroll, 0);
    }

    #[test]
    fn explorer_new_folder_entry_documents_cmd_shift_n_not_cmd_shift_f() {
        let explorer = SHORTCUT_GROUPS
            .iter()
            .find(|g| g.title == "Explorer")
            .expect("Explorer group must be present in the shortcuts modal");
        let new_folder = explorer
            .entries
            .iter()
            .find(|e| e.description == "New folder in selected folder")
            .expect("Explorer group must list a 'New folder in selected folder' entry");
        assert_eq!(
            new_folder.keys, "Cmd/Ctrl+Shift+N",
            "New Folder lives on Cmd+Shift+N — Cmd+Shift+F now always jumps to the Search sidebar, so the modal must document the new chord"
        );
    }

    #[test]
    fn scroll_to_bottom_lands_exactly_at_max_scroll() {
        let mut modal = ShortcutsModal {
            scroll: 0,
            last_inner_height: 8,
            last_content_height: 30,
            last_rect: Rect::default(),
        };
        modal.scroll_to_bottom();
        assert_eq!(modal.scroll, 22);
    }

    fn buffer_text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in buf.area.top()..buf.area.bottom() {
            for x in buf.area.left()..buf.area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn scrolling_to_the_bottom_reveals_the_last_entry_even_when_lines_wrap() {
        // A width where the long descriptions (zoxide jump, re-root, etc.) wrap to
        // a second visual row but the short final entry still fits on one line.
        let area = Rect::new(0, 0, 90, 24);
        let mut buf = Buffer::empty(area);
        let mut modal = ShortcutsModal::default();
        // First render populates last_content_height / last_inner_height.
        render_shortcuts_modal(&mut modal, area, &mut buf, false);
        modal.scroll_to_bottom();
        let mut buf = Buffer::empty(area);
        render_shortcuts_modal(&mut modal, area, &mut buf, false);
        let last_entry = SHORTCUT_GROUPS
            .last()
            .and_then(|g| g.entries.last())
            .expect("there must be a final shortcut entry");
        // The description may wrap across rows, so its full text is not a
        // contiguous substring of the buffer. Word wrap never splits a word, so
        // the final word is a reliable "did we reach the bottom" marker.
        let last_word = last_entry
            .description
            .split_whitespace()
            .next_back()
            .expect("final entry description must not be empty");
        let text = buffer_text(&buf);
        assert!(
            text.contains(last_word),
            "after scroll_to_bottom the final entry '{}' must be visible; wrapped rows are not counted in the scroll ceiling so the bottom of the list is unreachable\n--- rendered ---\n{text}",
            last_entry.description
        );
    }

    /// The set of `is_*_key` predicates in the app key router IS the list of
    /// shortcuts croft handles. Every one MUST be named in a ShortcutEntry's
    /// `handler` so it shows up in F1. This fails the build the moment a
    /// predicate is added without an entry (or an entry names a predicate that
    /// no longer exists), making "every shortcut appears in F1" a hard invariant
    /// rather than a hand-maintained hope.
    #[test]
    fn every_app_keybinding_predicate_is_documented_in_f1() {
        use std::collections::BTreeSet;
        const APP_SRC: &str = include_str!("../app/mod.rs");

        // Every `fn is_*_key` defined in the app key router.
        let mut handled: BTreeSet<&str> = BTreeSet::new();
        for (i, _) in APP_SRC.match_indices("fn is_") {
            let after_fn = &APP_SRC[i + 3..];
            let Some(paren) = after_fn.find('(') else {
                continue;
            };
            let name = &after_fn[..paren];
            if name.ends_with("_key")
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                handled.insert(name);
            }
        }
        assert!(
            !handled.is_empty(),
            "scanned src/app/mod.rs but found no is_*_key predicates; the scan is broken"
        );

        // Every predicate named by an F1 entry's `handler`.
        let mut documented: BTreeSet<&str> = BTreeSet::new();
        for group in SHORTCUT_GROUPS {
            for entry in group.entries {
                for token in entry.handler.split_whitespace() {
                    documented.insert(token);
                }
            }
        }

        let missing: Vec<&&str> = handled.difference(&documented).collect();
        let stale: Vec<&&str> = documented.difference(&handled).collect();
        assert!(
            missing.is_empty() && stale.is_empty(),
            "F1 shortcuts overlay is out of sync with src/app/mod.rs key handlers.\n  \
             UNDOCUMENTED (add a ShortcutEntry whose `handler` names each): {missing:?}\n  \
             STALE (handler names a predicate that no longer exists): {stale:?}"
        );
    }
}
