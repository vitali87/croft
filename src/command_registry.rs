//! The command registry: every built-in command croft can run from the palette, a
//! menu or a keybinding (the `Command` enum, its palette order, titles, shortcut hints,
//! `keybindings.json` ids and search words), and the spelling of a hint on each
//! platform. The palette widget, the menus, the keyboard shortcuts screen and
//! `keybindings.json` all read it here.

/// Declares every built-in command once, as one row: the variant (under its
/// docs), its `keybindings.json` id, its title and its default shortcut hint,
/// with the reason beside any hint that is empty or unusual, and its
/// [`Group`] when it has one. From the rows come [`Command`], [`ALL_COMMANDS`]
/// in the rows' order, [`Command::id`], [`Command::title`],
/// [`Command::keybinding_hint`], [`Command::from_id`] and [`Command::group`]. A row
/// without all three fields does not compile, so no command can ship without
/// an id, a title or a decision about its shortcut (croft tenet: everything
/// has a shortcut, or says why not).
macro_rules! commands {
    ($(
        $(#[$attr:meta])*
        $variant:ident {
            id: $id:literal,
            title: $title:literal,
            hint: $hint:expr,
            $(group: $group:ident,)?
        }
    )*) => {
        /// One invokable command. The order of variants is the order commands
        /// appear in the palette before the user types anything (grouped by
        /// category, most useful first), so keep related commands adjacent.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Command {
            $($(#[$attr])* $variant,)*
        }

        /// Every command, in palette display order. Single source of truth for
        /// both the empty-query list and the test that guards the count.
        pub const ALL_COMMANDS: &[Command] = &[$(Command::$variant,)*];

        impl Command {
            /// The human-readable label shown in the palette and matched against the
            /// query. Mirrors VS Code's command titles.
            pub fn title(self) -> &'static str {
                match self {
                    $(Command::$variant => $title,)*
                }
            }

            /// The default key-binding hint shown right-aligned on the row, or an
            /// empty string for palette-only commands. The label uses the macOS chord
            /// names; on Linux/Android the command modifier is `Ctrl`, and
            /// [`platform_hint`] spells it that way where it is shown.
            pub fn keybinding_hint(self) -> &'static str {
                match self {
                    $(Command::$variant => $hint,)*
                }
            }

            /// The stable snake_case identifier used in `keybindings.json`. This is the
            /// contract a user's config binds against, so the strings must never drift
            /// once shipped (unlike `title`, which is display-only).
            pub fn id(self) -> &'static str {
                match self {
                    $(Command::$variant => $id,)*
                }
            }

            /// Resolve a `keybindings.json` command id back to its [`Command`].
            /// Returns `None` for an unknown id so a typo in the user's config is
            /// ignored rather than fatal. Two rows with one id would be an
            /// unreachable pattern here, which the build refuses.
            pub fn from_id(id: &str) -> Option<Command> {
                match id {
                    $($id => Some(Command::$variant),)*
                    _ => None,
                }
            }

            /// The group a row declares, if any: a command's behaviour that
            /// hangs on its group is read from the row, never from its title.
            pub fn group(self) -> Option<Group> {
                match self {
                    $(Command::$variant => group_of!($($group)?),)*
                }
            }
        }
    };
}

/// A row's optional `group:` as an `Option<Group>`.
macro_rules! group_of {
    () => {
        None
    };
    ($group:ident) => {
        Some(Group::$group)
    };
}

/// A set of commands that behave alike, declared on their rows in
/// [`commands!`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// The terminal's commands ("Terminal: …"): a `keybindings.json` chord
    /// for one runs it while the terminal has focus (#1562).
    Terminal,
    /// The CodeQL extension's side bar and commands ("CodeQL: …"), which
    /// leave the palette until CodeQL is added and enabled (#1842).
    CodeQl,
}

commands! {
    // --- Editor: line / selection ops (the new VS Code parity features) ---
    MoveLineUp {
        id: "move_line_up",
        title: "Move Line Up",
        hint: "Alt+↑",
    }
    MoveLineDown {
        id: "move_line_down",
        title: "Move Line Down",
        hint: "Alt+↓",
    }
    /// Copy Line Up/Down (#916): VS Code's `copyLinesUpAction` /
    /// `copyLinesDownAction`, by default Shift+Alt+Up/Down.
    CopyLineUp {
        id: "copy_line_up",
        title: "Copy Line Up",
        hint: "Shift+Alt+↑",
    }
    CopyLineDown {
        id: "copy_line_down",
        title: "Copy Line Down",
        hint: "Shift+Alt+↓",
    }
    ToggleLineComment {
        id: "toggle_line_comment",
        title: "Toggle Line Comment",
        hint: "Cmd+/",
    }
    ToggleBlockComment {
        id: "toggle_block_comment",
        title: "Toggle Block Comment",
        hint: "Shift+Alt+A",
    }
    JoinLines {
        id: "join_lines",
        title: "Join Lines",
        hint: "Cmd+Opt+Shift+J",
    }
    InsertLineBelow {
        id: "insert_line_below",
        title: "Insert Line Below",
        hint: "Cmd+Enter",
    }
    InsertLineAbove {
        id: "insert_line_above",
        title: "Insert Line Above",
        hint: "Cmd+Shift+Enter",
    }
    DeleteLine {
        id: "delete_line",
        title: "Delete Line",
        hint: "Cmd+Shift+K",
    }
    KillToEndOfLine {
        id: "kill_to_end_of_line",
        title: "Kill to End of Line",
        // macOS keeps `Ctrl+K`; off it `Ctrl+K` is the `Cmd+K` leader
        // except in vim mode (#843), so the palette is the way there.
        hint: if cfg!(target_os = "macos") {
            "Ctrl+K"
        } else {
            ""
        },
    }
    TransformUpper {
        id: "transform_upper",
        title: "Transform to Uppercase",
        hint: "Cmd+Opt+Shift+U",
    }
    TransformLower {
        id: "transform_lower",
        title: "Transform to Lowercase",
        hint: "Cmd+Opt+Shift+L",
    }
    TransformTitle {
        id: "transform_title",
        title: "Transform to Title Case",
        // VS Code binds it no key. Cmd+Opt+Shift+C is Copy Relative Path,
        // there and here (#2648).
        hint: "",
    }
    TransformSnake {
        id: "transform_snake",
        title: "Transform to Snake Case",
        hint: "",
    }
    TransformCamel {
        id: "transform_camel",
        title: "Transform to Camel Case",
        hint: "",
    }
    TransformPascal {
        id: "transform_pascal",
        title: "Transform to Pascal Case",
        hint: "",
    }
    TransformKebab {
        id: "transform_kebab",
        title: "Transform to Kebab Case",
        hint: "",
    }
    SortLinesAscending {
        id: "sort_lines_ascending",
        title: "Sort Lines Ascending",
        hint: "Cmd+Opt+Shift+A",
    }
    SortLinesDescending {
        id: "sort_lines_descending",
        title: "Sort Lines Descending",
        hint: "Cmd+Opt+Shift+D",
    }
    IncrementNumber {
        id: "increment_number",
        title: "Increment Number Under Cursor",
        hint: "Cmd+Opt+=",
    }
    DecrementNumber {
        id: "decrement_number",
        title: "Decrement Number Under Cursor",
        hint: "Cmd+Opt+-",
    }
    TrimTrailingWhitespace {
        id: "trim_trailing_whitespace",
        title: "Trim Trailing Whitespace",
        hint: "Cmd+Opt+Shift+W",
    }
    EmmetExpandAbbreviation {
        id: "emmet_expand_abbreviation",
        title: "Emmet: Expand Abbreviation",
        hint: "Cmd+Opt+Shift+E",
    }
    EmmetWrapWithAbbreviation {
        id: "emmet_wrap_with_abbreviation",
        title: "Emmet: Wrap with Abbreviation",
        hint: "",
    }
    EmmetRemoveTag {
        id: "emmet_remove_tag",
        title: "Emmet: Remove Tag",
        hint: "",
    }
    EmmetUpdateTag {
        id: "emmet_update_tag",
        title: "Emmet: Update Tag",
        hint: "",
    }
    EmmetBalanceOutward {
        id: "emmet_balance_outward",
        title: "Emmet: Balance (outward)",
        hint: "",
    }
    EmmetBalanceInward {
        id: "emmet_balance_inward",
        title: "Emmet: Balance (inward)",
        hint: "",
    }
    EmmetMatchTag {
        id: "emmet_match_tag",
        title: "Emmet: Go to Matching Pair",
        hint: "",
    }
    ToggleWordWrap {
        id: "toggle_word_wrap",
        title: "View: Toggle Word Wrap",
        hint: "Alt+Z",
    }
    ExpandSelection {
        id: "expand_selection",
        title: "Expand Selection",
        hint: "Shift+Alt+\u{2192}",
    }
    ShrinkSelection {
        id: "shrink_selection",
        title: "Shrink Selection",
        hint: "Shift+Alt+\u{2190}",
    }
    ReplaceInFile {
        id: "replace_in_file",
        title: "Replace in File",
        hint: "Cmd+Opt+F",
    }
    ToggleFindInSelection {
        id: "toggle_find_in_selection",
        title: "Find: Toggle Find in Selection",
        hint: "Alt+L",
    }
    MergeAcceptCurrent {
        id: "merge_accept_current",
        title: "Merge Conflict: Accept Current",
        hint: "Cmd+.",
    }
    MergeAcceptIncoming {
        id: "merge_accept_incoming",
        title: "Merge Conflict: Accept Incoming",
        hint: "Cmd+.",
    }
    MergeAcceptBoth {
        id: "merge_accept_both",
        title: "Merge Conflict: Accept Both",
        hint: "Cmd+.",
    }
    MergeAcceptAllCurrent {
        id: "merge_accept_all_current",
        title: "Merge Conflict: Accept All Current",
        hint: "",
    }
    MergeAcceptAllIncoming {
        id: "merge_accept_all_incoming",
        title: "Merge Conflict: Accept All Incoming",
        hint: "",
    }
    MergeNextConflict {
        id: "merge_next_conflict",
        title: "Merge Conflict: Next Conflict",
        hint: "F7",
    }
    MergePrevConflict {
        id: "merge_prev_conflict",
        title: "Merge Conflict: Previous Conflict",
        hint: "Shift+F7",
    }
    GoToNextChange {
        id: "next_change",
        title: "Go to Next Change",
        hint: "F7",
    }
    GoToPrevChange {
        id: "previous_change",
        title: "Go to Previous Change",
        hint: "Shift+F7",
    }
    /// Walk the PROBLEMS list (build messages and diagnostics) from the
    /// caret, across files, wrapping at the ends.
    GoToNextProblem {
        id: "next_problem",
        title: "Go to Next Problem",
        // No default chord: F8 is the perf toggle here. Bind one in
        // keybindings.json, or import VS Code's F8 / Shift+F8.
        hint: "",
    }
    GoToPrevProblem {
        id: "previous_problem",
        title: "Go to Previous Problem",
        hint: "",
    }
    MergeComplete {
        id: "merge_complete",
        title: "Merge: Complete Merge (stage file)",
        hint: "",
    }
    MergeOpenEditor {
        id: "merge_open_editor",
        title: "Merge: Open Merge Editor",
        hint: "",
    }
    MergeToggleBase {
        id: "merge_toggle_base",
        title: "Merge: Show Base",
        hint: "",
    }
    MergeAcceptBothReverse {
        id: "merge_accept_both_reverse",
        title: "Merge: Accept Combination (Incoming First)",
        hint: "",
    }
    MergeIgnore {
        id: "merge_ignore",
        title: "Merge: Ignore (Keep Base)",
        hint: "",
    }
    DebugAddWatch {
        id: "debug_add_watch",
        title: "Debug: Add Watch Expression",
        hint: "",
    }
    DebugClearWatch {
        id: "debug_clear_watch",
        title: "Debug: Remove All Watch Expressions",
        hint: "",
    }
    PeekDefinition {
        id: "peek_definition",
        title: "Peek Definition",
        hint: "Alt+F12",
    }
    PeekReferences {
        id: "peek_references",
        title: "Peek References",
        hint: "Alt+Shift+F12",
    }
    GoToDefinition {
        id: "go_to_definition",
        title: "Go to Definition",
        hint: "F12",
    }
    GoToReferences {
        id: "go_to_references",
        title: "Go to References",
        hint: "Shift+F12",
    }
    GoToDeclaration {
        id: "go_to_declaration",
        title: "Go to Declaration",
        hint: "Ctrl+Shift+F12",
    }
    GoToTypeDefinition {
        id: "go_to_type_definition",
        title: "Go to Type Definition",
        hint: "Ctrl+F12",
    }
    /// Go to Implementations (#843): Cmd+F12, which has no `Ctrl` form
    /// (`Ctrl+F12` is Go to Type Definition).
    GoToImplementations {
        id: "go_to_implementations",
        title: "Go to Implementations",
        hint: "Cmd+F12",
    }
    SwitchHeaderSource {
        id: "switch_header_source",
        title: "C/C++: Switch Header/Source",
        hint: "Alt+O",
    }
    RenameSymbol {
        id: "rename_symbol",
        title: "Rename Symbol",
        hint: "F2",
    }
    /// Trigger Suggest / Trigger Parameter Hints (#916): the completion
    /// popup Ctrl+Space opens, and the parameter hints typing `(` asks for.
    TriggerSuggest {
        id: "trigger_suggest",
        title: "Trigger Suggest",
        hint: "Ctrl+Space",
    }
    TriggerParameterHints {
        id: "trigger_parameter_hints",
        title: "Trigger Parameter Hints",
        // No chord of its own: VS Code's Ctrl+Shift+Space is the
        // terminal's Quick Select here. Typing `(` or `,` asks too.
        hint: "",
    }
    /// Show or Focus Hover (#1518): the hover popup for the caret, the one
    /// the pointer's rest opens, from the keyboard.
    ShowHover {
        id: "show_hover",
        title: "Show or Focus Hover",
        hint: "Cmd+K Cmd+I",
    }
    MouseAddCursorAtClick {
        id: "mouse_add_cursor_at_click",
        title: "Mouse: Add Cursor at Click",
        hint: "",
    }
    MouseGoToDefinitionAtClick {
        id: "mouse_go_to_definition_at_click",
        title: "Mouse: Go to Definition at Click",
        hint: "",
    }
    MouseOpenLinkAtClick {
        id: "mouse_open_link_at_click",
        title: "Mouse: Open Link at Click",
        hint: "",
    }
    ClearBuildDiagnostics {
        id: "clear_build_diagnostics",
        title: "Problems: Clear Build Diagnostics",
        hint: "",
    }
    StageHunk {
        id: "stage_hunk",
        title: "Git: Stage Hunk",
        hint: "S in diff",
    }
    UnstageHunk {
        id: "unstage_hunk",
        title: "Git: Unstage Hunk",
        hint: "U in diff",
    }
    RevertHunk {
        id: "revert_hunk",
        title: "Git: Revert Hunk",
        hint: "R in diff",
    }
    /// Git: Undo Last Commit (#1350): `reset --soft HEAD~1`, its message
    /// back in the Source Control box.
    GitUndoLastCommit {
        id: "git_undo_last_commit",
        title: "Git: Undo Last Commit",
        hint: "",
    }
    /// rust-analyzer's own commands (#1626), listed only while it serves
    /// the active file.
    RustExpandMacro {
        id: "rust_expand_macro",
        title: "Rust: Expand Macro Recursively at Caret",
        hint: "",
    }
    RustParentModule {
        id: "rust_parent_module",
        title: "Rust: Locate Parent Module",
        hint: "",
    }
    RustOpenCargoToml {
        id: "rust_open_cargo_toml",
        title: "Rust: Open Cargo.toml",
        hint: "",
    }
    RustRebuildProcMacros {
        id: "rust_rebuild_proc_macros",
        title: "Rust: Rebuild Proc Macros and Build Scripts",
        hint: "",
    }
    RustReloadWorkspace {
        id: "rust_reload_workspace",
        title: "Rust: Reload Workspace",
        hint: "",
    }
    /// Git: Copy Permalink / Copy Head Link / Open on Remote (#1610): the
    /// selected lines on the remote's web page, at HEAD's commit or branch.
    GitCopyPermalink {
        id: "git_copy_permalink",
        title: "Git: Copy Permalink",
        hint: "",
    }
    GitCopyHeadLink {
        id: "git_copy_head_link",
        title: "Git: Copy Head Link",
        hint: "",
    }
    GitOpenOnRemote {
        id: "git_open_on_remote",
        title: "Git: Open on Remote",
        hint: "",
    }
    /// Pick a stash and open its changes (#1581).
    GitViewStash {
        id: "git_view_stash",
        title: "Git: View Stash\u{2026}",
        hint: "",
    }
    /// The caret line's commit, as the inline blame names it (#1556): open
    /// the changes it made to the file, or copy its hash.
    GitOpenLineCommit {
        id: "git_open_line_commit",
        title: "Git: Open Commit for Line",
        hint: "",
    }
    GitCopyLineCommitHash {
        id: "git_copy_line_commit_hash",
        title: "Git: Copy Commit Hash for Line",
        hint: "",
    }
    AddCursorAbove {
        id: "add_cursor_above",
        title: "Add Cursor Above",
        hint: "Cmd+Opt+↑",
    }
    AddCursorBelow {
        id: "add_cursor_below",
        title: "Add Cursor Below",
        hint: "Cmd+Opt+↓",
    }
    ColumnSelectUp {
        id: "column_select_up",
        title: "Column Select Up",
        hint: "Cmd+Shift+Opt+↑",
    }
    ColumnSelectDown {
        id: "column_select_down",
        title: "Column Select Down",
        hint: "Cmd+Shift+Opt+↓",
    }
    ColumnSelectLeft {
        id: "column_select_left",
        title: "Column Select Left",
        hint: "Cmd+Shift+Opt+←",
    }
    ColumnSelectRight {
        id: "column_select_right",
        title: "Column Select Right",
        hint: "Cmd+Shift+Opt+→",
    }
    ColumnSelectPageUp {
        id: "column_select_page_up",
        title: "Column Select Page Up",
        hint: "Cmd+Shift+Opt+PageUp",
    }
    ColumnSelectPageDown {
        id: "column_select_page_down",
        title: "Column Select Page Down",
        hint: "Cmd+Shift+Opt+PageDown",
    }
    ToggleColumnSelection {
        id: "toggle_column_selection",
        title: "View: Toggle Column Selection Mode",
        hint: "",
    }
    AddSelectionToNextMatch {
        id: "add_selection_to_next_match",
        title: "Add Selection to Next Find Match",
        hint: "Cmd+D",
    }
    SelectAllOccurrences {
        id: "select_all_occurrences",
        title: "Select All Occurrences of Find Match",
        hint: "Cmd+Shift+L",
    }
    AddCursorsToLineEnds {
        id: "add_cursors_to_line_ends",
        title: "Add Cursors to Line Ends",
        hint: "Shift+Alt+I",
    }
    ExpandLineSelection {
        id: "expand_line_selection",
        title: "Expand Line Selection",
        hint: "Cmd+L",
    }
    // --- Editor: bracket / character / indentation ops ---
    JumpToBracket {
        id: "jump_to_bracket",
        title: "Go to Bracket",
        hint: "Cmd+Shift+\\",
    }
    SelectToBracket {
        id: "select_to_bracket",
        title: "Select to Bracket",
        hint: "Cmd+Opt+\\",
    }
    TransposeCharacters {
        id: "transpose_characters",
        title: "Transpose Characters around the Cursor",
        hint: "Ctrl+T",
    }
    IndentationToSpaces {
        id: "indentation_to_spaces",
        title: "Convert Indentation to Spaces",
        hint: "Cmd+Opt+Shift+S",
    }
    IndentationToTabs {
        id: "indentation_to_tabs",
        title: "Convert Indentation to Tabs",
        hint: "Cmd+Opt+Shift+T",
    }
    TrimFinalNewlines {
        id: "trim_final_newlines",
        title: "Trim Final Newlines",
        hint: "Cmd+Opt+Shift+N",
    }
    ToggleOverviewRuler {
        id: "toggle_overview_ruler",
        title: "View: Toggle Overview Ruler",
        hint: "",
    }
    ToggleBookmark {
        id: "toggle_bookmark",
        title: "Bookmarks: Toggle Bookmark",
        hint: "Cmd+Opt+Shift+K",
    }
    NextBookmark {
        id: "next_bookmark",
        title: "Bookmarks: Next Bookmark",
        hint: "Cmd+Opt+.",
    }
    PreviousBookmark {
        id: "previous_bookmark",
        title: "Bookmarks: Previous Bookmark",
        hint: "Cmd+Opt+,",
    }
    ClearBookmarks {
        id: "clear_bookmarks",
        title: "Bookmarks: Clear All in File",
        hint: "Cmd+Opt+Shift+B",
    }
    ViewBookmarks {
        id: "view_bookmarks",
        title: "Bookmarks: View Bookmarks",
        hint: "Cmd+K Cmd+M",
    }
    FormatDocument {
        id: "format_document",
        title: "Format Document",
        hint: "Cmd+Opt+Shift+F",
    }
    FormatSelection {
        id: "format_selection",
        title: "Format Selection",
        hint: "Cmd+K Cmd+F",
    }
    ChangeColorPresentation {
        id: "change_color_presentation",
        title: "Change Color Presentation",
        hint: "",
    }
    ToggleFormatOnType {
        id: "toggle_format_on_type",
        title: "Preferences: Toggle Format on Type",
        hint: "",
    }
    ToggleFormatOnSave {
        id: "toggle_format_on_save",
        title: "Preferences: Toggle Format on Save",
        hint: "Cmd+K F",
    }
    QuickFix {
        id: "quick_fix",
        title: "Quick Fix",
        hint: "Cmd+.",
    }
    // --- Editor: folding ---
    ToggleFold {
        id: "toggle_fold",
        title: "Toggle Fold",
        hint: "Cmd+K Cmd+L",
    }
    FoldAll {
        id: "fold_all",
        title: "Fold All",
        hint: "Cmd+K Cmd+0",
    }
    UnfoldAll {
        id: "unfold_all",
        title: "Unfold All",
        hint: "Cmd+K Cmd+J",
    }
    FoldAllComments {
        id: "fold_all_comments",
        title: "Fold All Block Comments",
        hint: "Cmd+K Cmd+/",
    }
    FoldAllRegions {
        id: "fold_all_regions",
        title: "Fold All Regions",
        hint: "Cmd+K Cmd+8",
    }
    UnfoldAllRegions {
        id: "unfold_all_regions",
        title: "Unfold All Regions",
        hint: "Cmd+K Cmd+9",
    }
    /// Fold Level 1-7 (#1653): collapse every fold that deep or deeper.
    FoldLevel1 {
        id: "fold_level_1",
        title: "Fold Level 1",
        hint: "Cmd+K Cmd+1",
    }
    FoldLevel2 {
        id: "fold_level_2",
        title: "Fold Level 2",
        hint: "Cmd+K Cmd+2",
    }
    FoldLevel3 {
        id: "fold_level_3",
        title: "Fold Level 3",
        hint: "Cmd+K Cmd+3",
    }
    FoldLevel4 {
        id: "fold_level_4",
        title: "Fold Level 4",
        hint: "Cmd+K Cmd+4",
    }
    FoldLevel5 {
        id: "fold_level_5",
        title: "Fold Level 5",
        hint: "Cmd+K Cmd+5",
    }
    FoldLevel6 {
        id: "fold_level_6",
        title: "Fold Level 6",
        hint: "Cmd+K Cmd+6",
    }
    FoldLevel7 {
        id: "fold_level_7",
        title: "Fold Level 7",
        hint: "Cmd+K Cmd+7",
    }
    /// The fold at the caret and every fold inside it (#1653).
    FoldRecursively {
        id: "fold_recursively",
        title: "Fold Recursively",
        // VS Code's Cmd+K Cmd+[ / ] collapse and restore terminal panes
        // here, so these two are palette commands, bindable by id.
        hint: "",
    }
    UnfoldRecursively {
        id: "unfold_recursively",
        title: "Unfold Recursively",
        hint: "",
    }
    // --- File / editor management ---
    SaveFile {
        id: "save_file",
        title: "File: Save",
        hint: "Cmd+S",
    }
    SaveAs {
        id: "save_as",
        title: "File: Save As…",
        hint: "",
    }
    /// File: Save with Sudo (#1379): write a file this user may not write
    /// through `sudo tee` in a terminal pane, where sudo asks for the
    /// password itself.
    SaveWithSudo {
        id: "save_with_sudo",
        title: "File: Save with Sudo",
        hint: "",
    }
    /// File: Revert File (#1285): drop the tab's unsaved edits for the
    /// text on disk, as one undo step.
    RevertFile {
        id: "revert_file",
        title: "File: Revert File",
        hint: "",
    }
    /// File: Save All (#852): every dirty tab, in every split.
    SaveAll {
        id: "save_all",
        title: "File: Save All",
        // VS Code's macOS chord; its Linux `Ctrl+K S` is Select for
        // Compare here, so Linux takes `Ctrl+Alt+S` instead.
        hint: "Cmd+Opt+S",
    }
    /// File: New File… / New Folder… (#852): the Explorer's create prompt
    /// from any pane, beside the active file or in the Explorer selection.
    NewFile {
        id: "new_file",
        title: "File: New File…",
        // Palette-only: the Explorer's `Cmd+F` / `Cmd+Shift+N` are Find
        // and unbound outside it, so neither is the command's chord.
        hint: "",
    }
    NewUntitledFile {
        id: "new_untitled_file",
        title: "File: New Untitled Text File",
        hint: "Cmd+N",
    }
    NewFolder {
        id: "new_folder",
        title: "File: New Folder…",
        hint: "",
    }
    Undo {
        id: "undo",
        title: "Undo",
        hint: "Cmd+Z",
    }
    Redo {
        id: "redo",
        title: "Redo",
        hint: "Shift+Cmd+Z",
    }
    SelectAll {
        id: "select_all",
        title: "Select All",
        hint: "Cmd+A",
    }
    ToggleAutoSave {
        id: "toggle_auto_save",
        title: "File: Toggle Auto Save",
        hint: "",
    }
    ToggleAutoSaveOnFocusChange {
        id: "toggle_auto_save_on_focus_change",
        title: "File: Toggle Auto Save on Focus Change",
        hint: "",
    }
    ToggleInlineBlame {
        id: "toggle_inline_blame",
        title: "Git: Toggle Inline Blame",
        hint: "",
    }
    ToggleLiveRun {
        id: "toggle_live_run",
        title: "Python: Toggle Live Run",
        hint: "Cmd+K V",
    }
    ToggleProvenance {
        id: "toggle_provenance",
        title: "Editor: Toggle Provenance",
        hint: "",
    }
    DiffToggleGroupBySeat {
        id: "diff_toggle_group_by_seat",
        title: "Diff: Toggle Group by Seat",
        hint: "",
    }
    ToggleIndentGuides {
        id: "toggle_indent_guides",
        title: "View: Toggle Indent Guides",
        hint: "",
    }
    ToggleBracketColors {
        id: "toggle_bracket_colors",
        title: "Editor: Toggle Bracket Pair Colorization",
        hint: "",
    }
    ToggleRenderWhitespace {
        id: "toggle_render_whitespace",
        title: "View: Toggle Render Whitespace",
        hint: "",
    }
    /// View: Change Line Numbers (#1576): on, relative, interval, off.
    CycleLineNumbers {
        id: "cycle_line_numbers",
        title: "View: Change Line Numbers (On, Relative, Interval, Off)",
        hint: "",
    }
    ToggleInlineValues {
        id: "toggle_inline_values",
        title: "Debug: Toggle Inline Values",
        hint: "",
    }
    ToggleInlayHints {
        id: "toggle_inlay_hints",
        title: "Editor: Toggle Inlay Hints",
        hint: "",
    }
    ToggleMarkdownPreview {
        id: "toggle_markdown_preview",
        title: "Markdown: Toggle Preview",
        hint: "Cmd+Shift+V",
    }
    /// Markdown: Open Preview to the Side (#1605), VS Code's
    /// `markdown.showPreviewToSide`: the rendered view in the next group,
    /// following the source as it is typed.
    OpenMarkdownPreviewToSide {
        id: "open_markdown_preview_to_side",
        title: "Markdown: Open Preview to the Side",
        hint: "Cmd+K V",
    }
    /// Markdown: Run Code Block at Cursor (#843): Cmd+Enter's run of the
    /// fence under the caret, from any terminal.
    RunCodeBlockAtCursor {
        id: "run_code_block_at_cursor",
        title: "Markdown: Run Code Block at Cursor",
        hint: "Cmd+Enter",
    }
    RestoreSnapshot {
        id: "restore_snapshot",
        title: "Local History: Restore Snapshot",
        hint: "",
    }
    CloseEditor {
        id: "close_editor",
        title: "View: Close Editor",
        hint: "Cmd+W",
    }
    ReopenClosedEditor {
        id: "reopen_closed_editor",
        title: "View: Reopen Closed Editor",
        hint: "Cmd+K Shift+W",
    }
    /// Explorer: Jump to Directory (zoxide) (#843): the Explorer's Cmd+Z,
    /// which has no `Ctrl` form.
    ZoxideJump {
        id: "zoxide_jump",
        title: "Explorer: Jump to Directory (zoxide)",
        hint: "Cmd+Z in Explorer",
    }
    /// The editor tab menu's close / pin / keep rows as commands (#852),
    /// VS Code's names, each on the active tab of the focused group.
    CloseOtherEditors {
        id: "close_other_editors",
        title: "View: Close Other Editors in Group",
        // Not VS Code's ⌥⌘T, which iTerm2 keeps for New Tab and a Linux
        // desktop for a new terminal (#2072).
        hint: "Cmd+K Shift+X",
    }
    CloseEditorsToTheRight {
        id: "close_editors_to_the_right",
        title: "View: Close Editors to the Right in Group",
        hint: "Cmd+K →",
    }
    CloseSavedEditors {
        id: "close_saved_editors",
        title: "View: Close Saved Editors in Group",
        hint: "Cmd+K U",
    }
    CloseAllEditors {
        id: "close_all_editors",
        title: "View: Close All Editors",
        hint: "Cmd+K W",
    }
    /// View: Pin Editor / Unpin Editor. The palette offers only the one
    /// that applies to the active tab, as VS Code does
    /// (`CommandPalette::set_hidden`); both stay bindable.
    PinEditor {
        id: "pin_editor",
        title: "View: Pin Editor",
        // One chord toggles, as VS Code's `Ctrl+K Shift+Enter` serves
        // both through their `when` clauses.
        hint: "Cmd+K P",
    }
    UnpinEditor {
        id: "unpin_editor",
        title: "View: Unpin Editor",
        hint: "Cmd+K P",
    }
    KeepEditor {
        id: "keep_editor",
        title: "View: Keep Editor",
        hint: "Cmd+K Shift+P",
    }
    /// View: Move Editor Left / Right (#1565): the active tab one place
    /// along its group's tab bar.
    MoveEditorLeftInGroup {
        id: "move_editor_left_in_group",
        title: "View: Move Editor Left",
        // VS Code's Linux and Windows chords; its macOS Cmd+K
        // Cmd+Shift+Left / Right are rebindable by id.
        hint: "Ctrl+Shift+PageUp",
    }
    MoveEditorRightInGroup {
        id: "move_editor_right_in_group",
        title: "View: Move Editor Right",
        hint: "Ctrl+Shift+PageDown",
    }
    /// View: Move Editor into Next / Previous Group (#1565): the active tab
    /// to the next or previous group, creating one beside it when there is
    /// none that way.
    MoveEditorToNextGroup {
        id: "move_editor_to_next_group",
        title: "View: Move Editor into Next Group",
        // VS Code's Ctrl+Alt+Left / Right already move croft's caret a
        // word (they carry Alt), so these two are palette commands,
        // bindable by id.
        hint: "",
    }
    MoveEditorToPreviousGroup {
        id: "move_editor_to_previous_group",
        title: "View: Move Editor into Previous Group",
        hint: "",
    }
    SplitEditor {
        id: "split_editor",
        title: "View: Split Editor",
        hint: "Cmd+\\",
    }
    /// View: Focus Left / Right Editor Group (#843): Cmd+Opt+Left / Right,
    /// which have no `Ctrl` form off macOS.
    FocusLeftEditorGroup {
        id: "focus_left_editor_group",
        title: "View: Focus Left Editor Group",
        hint: "Cmd+Opt+←",
    }
    FocusRightEditorGroup {
        id: "focus_right_editor_group",
        title: "View: Focus Right Editor Group",
        hint: "Cmd+Opt+→",
    }
    QuickOpen {
        id: "quick_open",
        title: "Go to File",
        hint: "Cmd+P",
    }
    TakeTheTour {
        id: "help_take_the_tour",
        title: "Help: Take the Tour",
        hint: "",
    }
    /// Help: Sponsor croft (#1940): the activity bar's heart from the
    /// keyboard, opening croft's GitHub Sponsors page.
    SponsorCroft {
        id: "help_sponsor_croft",
        title: "Help: Sponsor croft",
        hint: "",
    }
    SarifNextResult {
        id: "sarif_next_result",
        title: "SARIF: Next Result",
        hint: "",
    }
    SarifPreviousResult {
        id: "sarif_previous_result",
        title: "SARIF: Previous Result",
        hint: "",
    }
    SarifOpenCodeScanning {
        id: "sarif_open_code_scanning",
        title: "SARIF: Open GitHub Code Scanning Analysis",
        hint: "",
    }
    /// SARIF: Load Code Scanning Results (#577): the current branch's
    /// newest analyses per tool, at the nearest scanned commit.
    SarifLoadCodeScanning {
        id: "sarif_load_code_scanning",
        title: "SARIF: Load Code Scanning Results for This Branch",
        hint: "",
    }
    GoToSymbol {
        id: "go_to_symbol",
        title: "Go to Symbol in Editor",
        hint: "Cmd+Shift+O",
    }
    GoToWorkspaceSymbol {
        id: "go_to_workspace_symbol",
        title: "Go to Symbol in Workspace",
        hint: "Cmd+P #",
    }
    /// VS Code "Go Back" (Ctrl+-): return to the location before the last
    /// navigation jump (Go to Definition, a reference pick, a symbol jump).
    NavigateBack {
        id: "navigate_back",
        title: "Go Back",
        hint: "Ctrl+-",
    }
    NavigateForward {
        id: "navigate_forward",
        title: "Go Forward",
        hint: "Ctrl+Shift+-",
    }
    GoToLastEditLocation {
        id: "go_to_last_edit_location",
        title: "Go to Last Edit Location",
        hint: "Cmd+K Cmd+Q",
    }
    ToggleVimMode {
        id: "toggle_vim_mode",
        title: "Toggle Vim Mode",
        hint: "Cmd+E",
    }
    MacroStartStopRecording {
        id: "macro_record",
        title: "Macro: Start/Stop Recording",
        hint: "",
    }
    MacroReplayLast {
        id: "macro_replay_last",
        title: "Macro: Replay Last",
        hint: "",
    }
    MacroReplayTimes {
        id: "macro_replay_times",
        title: "Macro: Replay N Times…",
        hint: "",
    }
    // --- View / navigation ---
    ShowExplorer {
        id: "show_explorer",
        title: "View: Show Explorer",
        hint: "Cmd+Shift+E",
    }
    /// View: Focus and Select Breadcrumbs (#1608): the last crumb's picker.
    FocusBreadcrumbs {
        id: "breadcrumbs_focus_and_select",
        title: "View: Focus and Select Breadcrumbs",
        hint: "Cmd+Shift+.",
    }
    ShowSearch {
        id: "show_search",
        title: "View: Show Search",
        hint: "Cmd+Shift+F",
    }
    /// Search: Replace in Files (#860): the Search side bar with its Replace
    /// row expanded and focused.
    ReplaceInFiles {
        id: "replace_in_files",
        title: "Search: Replace in Files",
        hint: "Cmd+Shift+H",
    }
    /// The Search side bar's `Aa` / `ab` / `.*` toggles and its `...`
    /// include / exclude row, from the keyboard (#860).
    SearchToggleMatchCase {
        id: "search_toggle_match_case",
        title: "Search: Toggle Match Case",
        // VS Code's search-input keys, live while a Search input has
        // focus; run from the palette they reveal the side bar first.
        hint: "Alt+C",
    }
    SearchToggleWholeWord {
        id: "search_toggle_whole_word",
        title: "Search: Toggle Match Whole Word",
        hint: "Alt+W",
    }
    SearchToggleRegex {
        id: "search_toggle_regex",
        title: "Search: Toggle Use Regular Expression",
        hint: "Alt+R",
    }
    SearchTogglePreserveCase {
        id: "search_toggle_preserve_case",
        title: "Search: Toggle Preserve Case",
        hint: "Alt+P",
    }
    SearchToggleDetails {
        id: "search_toggle_details",
        title: "Search: Toggle Search Details",
        // VS Code binds none that is free here (its Ctrl+Shift+J is
        // croft's maximize-terminal); D for details.
        hint: "Alt+D",
    }
    ShowSourceControl {
        id: "show_source_control",
        title: "View: Show Source Control",
        hint: "Cmd+Shift+S",
    }
    AddWorkspaceFolder {
        id: "add_workspace_folder",
        title: "Workspaces: Add Folder to Workspace",
        hint: "",
    }
    RemoveWorkspaceFolder {
        id: "remove_workspace_folder",
        title: "Workspaces: Remove Folder from Workspace",
        hint: "",
    }
    SaveWorkspaceAs {
        id: "save_workspace_as",
        title: "Workspaces: Save Workspace As",
        hint: "",
    }
    OpenWorkspaceFromFile {
        id: "open_workspace_from_file",
        title: "Workspaces: Open Workspace from File",
        hint: "",
    }
    ReopenAsHex {
        id: "reopen_as_hex",
        title: "File: Reopen as Hex",
        hint: "",
    }
    ReopenAsPreview {
        id: "reopen_as_preview",
        title: "File: Reopen as Preview",
        hint: "",
    }
    ReopenAsText {
        id: "reopen_as_text",
        title: "File: Reopen as Text",
        hint: "",
    }
    NotebookRunAll {
        id: "notebook_run_all",
        title: "Notebook: Run All Cells",
        hint: "",
    }
    NotebookInterrupt {
        id: "notebook_interrupt",
        title: "Notebook: Interrupt Kernel",
        hint: "",
    }
    NotebookRestart {
        id: "notebook_restart",
        title: "Notebook: Restart Kernel",
        hint: "",
    }
    NotebookSelectKernel {
        id: "notebook_select_kernel",
        title: "Notebook: Select Kernel",
        hint: "",
    }
    RestartLanguageServers {
        id: "restart_language_servers",
        title: "Language Servers: Restart All",
        hint: "",
    }
    RestartLanguageServersForFile {
        id: "restart_language_servers_for_file",
        title: "Language Servers: Restart for This File",
        hint: "",
    }
    TestingRunWithCoverage {
        id: "testing_run_with_coverage",
        title: "Testing: Run All Tests with Coverage",
        hint: "",
    }
    CoverageClear {
        id: "coverage_clear",
        title: "Coverage: Clear",
        hint: "",
    }
    TestingInstallCoverageTool {
        id: "testing_install_coverage_tool",
        title: "Testing: Install Coverage Tool",
        hint: "",
    }
    TestingShowCoverageReport {
        id: "testing_show_coverage_report",
        title: "Testing: Show Coverage Report",
        hint: "",
    }
    DeveloperShowMemoryUsage {
        id: "developer_show_memory_usage",
        title: "Developer: Show Memory Usage",
        hint: "",
    }
    /// Developer: Toggle Screencast Mode: show the keys pressed, the command
    /// each ran and where a click landed, for a recording.
    ToggleScreencastMode {
        id: "toggle_screencast_mode",
        title: "Developer: Toggle Screencast Mode",
        hint: "Shift+F8",
    }
    RunTestAtCursorWithCoverage {
        id: "run_test_at_cursor_with_coverage",
        title: "Testing: Run Test at Cursor with Coverage",
        hint: "",
    }
    TestingToggleWatchAll {
        id: "testing_toggle_watch_all",
        title: "Testing: Toggle Watch All Tests",
        hint: "",
    }
    TestingGoToFirstFailure {
        id: "testing_go_to_first_failure",
        title: "Testing: Go to First Failure",
        hint: "",
    }
    TestingCancelRun {
        id: "testing_cancel_run",
        title: "Testing: Cancel Test Run",
        hint: "",
    }
    HexFindNext {
        id: "hex_find_next",
        title: "Hex: Find Next",
        hint: "F3",
    }
    SheetInsertRowBelow {
        id: "sheet_insert_row_below",
        title: "Sheet: Insert Row Below",
        hint: "",
    }
    SheetDeleteRow {
        id: "sheet_delete_row",
        title: "Sheet: Delete Row",
        hint: "",
    }
    SheetInsertColRight {
        id: "sheet_insert_col_right",
        title: "Sheet: Insert Column Right",
        hint: "",
    }
    SheetDeleteCol {
        id: "sheet_delete_col",
        title: "Sheet: Delete Column",
        hint: "",
    }
    MediaOpenExternal {
        id: "media_open_external",
        title: "Media: Open in System Player",
        hint: "",
    }
    ShowRunDebug {
        id: "show_run_debug",
        title: "View: Show Run and Debug",
        hint: "Cmd+Shift+D",
    }
    ShowRemote {
        id: "show_remote",
        title: "View: Show Remote",
        hint: "Cmd+Shift+R",
    }
    ShowExtensions {
        id: "show_extensions",
        title: "View: Show Extensions",
        hint: "Cmd+Shift+X",
    }
    CompareExtensionsWithVscode {
        id: "compare_extensions_with_vscode",
        title: "Extensions: Compare with VS Code",
        hint: "",
    }
    ShowTesting {
        id: "show_testing",
        title: "View: Show Testing",
        hint: "Cmd+K B",
    }
    ShowCodeQL {
        id: "show_codeql",
        title: "View: Show CodeQL",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunQuery {
        id: "codeql_run_query",
        title: "CodeQL: Run Query on Selected Database",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunQuerySuite {
        id: "codeql_run_query_suite",
        title: "CodeQL: Run Query Suite",
        hint: "",
        group: CodeQl,
    }
    CodeqlRemoveDatabase {
        id: "codeql_remove_database",
        title: "CodeQL: Remove Database",
        hint: "",
        group: CodeQl,
    }
    CodeqlRenameDatabase {
        id: "codeql_rename_database",
        title: "CodeQL: Rename Database",
        hint: "",
        group: CodeQl,
    }
    CodeqlSortDatabases {
        id: "codeql_sort_databases",
        title: "CodeQL: Sort Databases",
        hint: "",
        group: CodeQl,
    }
    CodeqlRevealDatabase {
        id: "codeql_reveal_database",
        title: "CodeQL: Reveal Database in Explorer",
        hint: "",
        group: CodeQl,
    }
    CodeqlUpgradeDatabase {
        id: "codeql_upgrade_database",
        title: "CodeQL: Upgrade Database",
        hint: "",
        group: CodeQl,
    }
    CodeqlClearCache {
        id: "codeql_clear_cache",
        title: "CodeQL: Clear Cache",
        hint: "",
        group: CodeQl,
    }
    CodeqlTrimCache {
        id: "codeql_trim_cache",
        title: "CodeQL: Trim Cache",
        hint: "",
        group: CodeQl,
    }
    CodeqlTrimCacheToOverlay {
        id: "codeql_trim_cache_to_overlay_base",
        title: "CodeQL: Trim Cache to Overlay Base",
        hint: "",
        group: CodeQl,
    }
    CodeqlAddDatabaseSource {
        id: "codeql_add_database_source",
        title: "CodeQL: Add Database Source to Workspace",
        hint: "",
        group: CodeQl,
    }
    CodeqlDeleteUnusedDatabases {
        id: "codeql_delete_unused_databases",
        title: "CodeQL: Delete Unused Databases",
        hint: "",
        group: CodeQl,
    }
    CodeqlRemoveHistory {
        id: "codeql_remove_history",
        title: "CodeQL: Remove Query History Entry",
        hint: "",
        group: CodeQl,
    }
    CodeqlRenameHistory {
        id: "codeql_rename_history",
        title: "CodeQL: Rename Query History Entry",
        hint: "",
        group: CodeQl,
    }
    CodeqlSortHistory {
        id: "codeql_sort_history",
        title: "CodeQL: Sort Query History",
        hint: "",
        group: CodeQl,
    }
    CodeqlViewQuery {
        id: "codeql_view_query",
        title: "CodeQL: View Query",
        hint: "",
        group: CodeQl,
    }
    CodeqlCreateQuery {
        id: "codeql_create_query",
        title: "CodeQL: Create Query",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunPack {
        id: "codeql_run_pack",
        title: "CodeQL: Run Queries in Pack",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunQueryOnDatabases {
        id: "codeql_run_query_on_databases",
        title: "CodeQL: Run Query on Multiple Databases",
        hint: "",
        group: CodeQl,
    }
    CodeqlCancelQueue {
        id: "codeql_cancel_queue",
        title: "CodeQL: Cancel Queued Queries",
        hint: "",
        group: CodeQl,
    }
    CodeqlCopyVersion {
        id: "codeql_copy_version_information",
        title: "CodeQL: Copy Version Information",
        hint: "",
        group: CodeQl,
    }
    CodeqlDownloadCli {
        id: "codeql_download_cli",
        title: "CodeQL: Download CLI",
        hint: "",
        group: CodeQl,
    }
    CodeqlCheckCliUpdates {
        id: "codeql_check_for_cli_updates",
        title: "CodeQL: Check for CLI Updates",
        hint: "",
        group: CodeQl,
    }
    CodeqlInstallPackDependencies {
        id: "codeql_install_pack_dependencies",
        title: "CodeQL: Install Pack Dependencies",
        hint: "",
        group: CodeQl,
    }
    CodeqlDownloadPacks {
        id: "codeql_download_packs",
        title: "CodeQL: Download Packs",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunPublishedPack {
        id: "codeql_run_published_pack",
        title: "CodeQL: Run Queries in Published Pack",
        hint: "",
        group: CodeQl,
    }
    CodeqlQuickQuery {
        id: "codeql_quick_query",
        title: "CodeQL: Quick Query",
        hint: "",
        group: CodeQl,
    }
    CodeqlCompareResults {
        id: "codeql_compare_query_results",
        title: "CodeQL: Compare Query Results",
        hint: "",
        group: CodeQl,
    }
    CodeqlShowEvaluatorLog {
        id: "codeql_show_evaluator_log",
        title: "CodeQL: Show Evaluator Log (Raw JSON)",
        hint: "",
        group: CodeQl,
    }
    CodeqlShowEvaluatorLogSummary {
        id: "codeql_show_evaluator_log_summary",
        title: "CodeQL: Show Evaluator Log (Summary Text)",
        hint: "",
        group: CodeQl,
    }
    CodeqlShowEvaluatorLogViewer {
        id: "codeql_show_evaluator_log_viewer",
        title: "CodeQL: Show Evaluator Log (Viewer)",
        hint: "",
        group: CodeQl,
    }
    CodeqlShowQueryLog {
        id: "codeql_show_query_log",
        title: "CodeQL: Show Query Log",
        hint: "",
        group: CodeQl,
    }
    CodeqlComparePerformance {
        id: "codeql_compare_performance",
        title: "CodeQL: Compare Performance",
        hint: "",
        group: CodeQl,
    }
    CodeqlPreviewQueryHelp {
        id: "codeql_preview_query_help",
        title: "CodeQL: Preview Query Help",
        hint: "",
        group: CodeQl,
    }
    CodeqlExportResults {
        id: "codeql_export_results",
        title: "CodeQL: Export Results",
        hint: "",
        group: CodeQl,
    }
    CodeqlSetUpController {
        id: "codeql_set_up_controller_repository",
        title: "CodeQL: Set Up Controller Repository",
        hint: "",
        group: CodeQl,
    }
    CodeqlAddVariantRepo {
        id: "codeql_add_variant_analysis_repository",
        title: "CodeQL: Add Variant Analysis Repository",
        hint: "",
        group: CodeQl,
    }
    CodeqlAddVariantList {
        id: "codeql_add_variant_analysis_list",
        title: "CodeQL: Add Variant Analysis Repository List",
        hint: "",
        group: CodeQl,
    }
    CodeqlAddVariantOwner {
        id: "codeql_add_variant_analysis_owner",
        title: "CodeQL: Add Variant Analysis Owner",
        hint: "",
        group: CodeQl,
    }
    CodeqlVariantCodeSearch {
        id: "codeql_add_repositories_with_code_search",
        title: "CodeQL: Add Repositories with GitHub Code Search",
        hint: "",
        group: CodeQl,
    }
    CodeqlOpenVariantConfig {
        id: "codeql_open_variant_analysis_config",
        title: "CodeQL: Open Variant Analysis Config File",
        hint: "",
        group: CodeQl,
    }
    CodeqlOpenVariantOnGithub {
        id: "codeql_open_variant_analysis_on_github",
        title: "CodeQL: Open Variant Analysis Repository on GitHub",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunVariantAnalysis {
        id: "codeql_run_variant_analysis",
        title: "CodeQL: Run Variant Analysis",
        hint: "",
        group: CodeQl,
    }
    CodeqlOpenVariantResults {
        id: "codeql_open_variant_analysis_results",
        title: "CodeQL: Open Variant Analysis Results",
        hint: "",
        group: CodeQl,
    }
    CodeqlExportVariantResults {
        id: "codeql_export_variant_analysis_results",
        title: "CodeQL: Export Variant Analysis Results",
        hint: "",
        group: CodeQl,
    }
    CodeqlCopyVariantRepoList {
        id: "codeql_copy_variant_repo_list",
        title: "CodeQL: Copy Variant Analysis Repository List",
        hint: "",
        group: CodeQl,
    }
    CodeqlViewVariantLogs {
        id: "codeql_view_variant_logs",
        title: "CodeQL: View Variant Analysis Logs",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunTests {
        id: "codeql_run_tests",
        title: "CodeQL: Run Tests",
        hint: "",
        group: CodeQl,
    }
    CodeqlDebugQuery {
        id: "codeql_debug_query",
        title: "CodeQL: Debug Query",
        hint: "",
        group: CodeQl,
    }
    CodeqlDebugSelection {
        id: "codeql_debug_selection",
        title: "CodeQL: Debug Selection",
        hint: "",
        group: CodeQl,
    }
    CodeqlResultsUp {
        id: "codeql_results_up",
        title: "CodeQL: Previous Result",
        hint: "",
        group: CodeQl,
    }
    CodeqlResultsDown {
        id: "codeql_results_down",
        title: "CodeQL: Next Result",
        hint: "",
        group: CodeQl,
    }
    CodeqlResultsLeft {
        id: "codeql_results_left",
        title: "CodeQL: Previous Result Column",
        hint: "",
        group: CodeQl,
    }
    CodeqlResultsRight {
        id: "codeql_results_right",
        title: "CodeQL: Next Result Column",
        hint: "",
        group: CodeQl,
    }
    SheetSortByColumn {
        id: "sheet_sort_by_column",
        title: "Sheet: Sort by Column",
        hint: "",
    }
    CodeqlQuickEval {
        id: "codeql_quick_eval",
        title: "CodeQL: Quick Evaluation",
        hint: "",
        group: CodeQl,
    }
    CodeqlQuickEvalCount {
        id: "codeql_quick_eval_count",
        title: "CodeQL: Quick Evaluation Count",
        hint: "",
        group: CodeQl,
    }
    CodeqlViewAst {
        id: "codeql_view_ast",
        title: "CodeQL: View AST",
        hint: "",
        group: CodeQl,
    }
    CodeqlOpenModelEditor {
        id: "codeql_open_model_editor",
        title: "CodeQL: Open Model Editor",
        hint: "",
        group: CodeQl,
    }
    CodeqlModelEndpoint {
        id: "codeql_model_endpoint",
        title: "CodeQL: Model Endpoint",
        hint: "",
        group: CodeQl,
    }
    CodeqlOpenReferencedFile {
        id: "codeql_open_referenced_file",
        title: "CodeQL: Open Referenced File",
        hint: "",
        group: CodeQl,
    }
    CodeqlViewAlertsCsv {
        id: "codeql_view_alerts_csv",
        title: "CodeQL: View Alerts (CSV)",
        hint: "",
        group: CodeQl,
    }
    CodeqlViewAlertsSarif {
        id: "codeql_view_alerts_sarif",
        title: "CodeQL: View Alerts (SARIF)",
        hint: "",
        group: CodeQl,
    }
    CodeqlViewResultsCsv {
        id: "codeql_view_results_csv",
        title: "CodeQL: View Results (CSV)",
        hint: "",
        group: CodeQl,
    }
    CodeqlShowResultSet {
        id: "codeql_show_result_set",
        title: "CodeQL: Show Result Set",
        hint: "",
        group: CodeQl,
    }
    CodeqlViewCfg {
        id: "codeql_view_cfg",
        title: "CodeQL: View CFG",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunAllQueries {
        id: "codeql_run_all_queries",
        title: "CodeQL: Run All Queries in Workspace",
        hint: "",
        group: CodeQl,
    }
    CodeqlRunSelectedQueries {
        id: "codeql_run_selected_queries",
        title: "CodeQL: Run Queries in Selected Files",
        hint: "",
        group: CodeQl,
    }
    CodeqlCancelRunningQuery {
        id: "codeql_cancel_running_query",
        title: "CodeQL: Cancel Running Query",
        hint: "",
        group: CodeQl,
    }
    CodeqlAcceptTestOutput {
        id: "codeql_accept_test_output",
        title: "CodeQL: Accept Test Output",
        hint: "",
        group: CodeQl,
    }
    CodeqlFocusSideBar {
        id: "codeql_focus_side_bar",
        title: "CodeQL: Focus Side Bar",
        hint: "",
        group: CodeQl,
    }
    CodeqlOpenResultsDirectory {
        id: "codeql_open_results_directory",
        title: "CodeQL: Open Query Results Directory",
        hint: "",
        group: CodeQl,
    }
    RunTestAtCursor {
        id: "run_test_at_cursor",
        title: "Testing: Run Test at Cursor",
        hint: "Cmd+K Enter",
    }
    /// Testing: Run All Tests / Run Tests in Current File / Rerun Failed
    /// Tests / Rerun Last Run (#1535), VS Code's everyday test commands.
    TestingRunAll {
        id: "testing_run_all",
        title: "Testing: Run All Tests",
        hint: "",
    }
    TestingRunFile {
        id: "testing_run_file",
        title: "Testing: Run Tests in Current File",
        hint: "",
    }
    TestingRerunFailed {
        id: "testing_rerun_failed",
        title: "Testing: Rerun Failed Tests",
        hint: "",
    }
    TestingRerunLast {
        id: "testing_rerun_last",
        title: "Testing: Rerun Last Run",
        hint: "",
    }
    DebugTestAtCursor {
        id: "debug_test_at_cursor",
        title: "Testing: Debug Test at Cursor",
        hint: "Cmd+K Shift+Enter",
    }
    ToggleSideBar {
        id: "toggle_side_bar",
        title: "View: Toggle Primary Side Bar",
        hint: "Cmd+B",
    }
    ToggleAutoHideSideBar {
        id: "toggle_auto_hide_side_bar",
        title: "View: Toggle Auto-Hide Side Bar",
        hint: "",
    }
    ToggleSecondarySideBar {
        id: "toggle_secondary_side_bar",
        title: "View: Toggle Secondary Side Bar",
        hint: "Cmd+Opt+B",
    }
    ToggleZenMode {
        id: "toggle_zen_mode",
        title: "View: Toggle Zen Mode",
        hint: "Cmd+K Z",
    }
    /// Centre the editor in its column (#1585), VS Code's View: Toggle
    /// Centered Layout.
    ToggleCenteredLayout {
        id: "toggle_centered_layout",
        title: "View: Toggle Centered Layout",
        hint: "",
    }
    ToggleTerminal {
        id: "toggle_terminal",
        title: "View: Toggle Terminal",
        hint: "Ctrl+J",
    }
    /// The Customize Layout popup's rows as commands (#852), VS Code's
    /// names: the bars' visibility, the side bar's side, and pickers for
    /// the panel alignment and the quick input position. `LAYOUT_COMMANDS`
    /// lists them with the popup's other rows so "layout" finds them all.
    ToggleActivityBar {
        id: "toggle_activity_bar",
        title: "View: Toggle Activity Bar Visibility",
        hint: "",
    }
    ToggleStatusBar {
        id: "toggle_status_bar",
        title: "View: Toggle Status Bar Visibility",
        hint: "",
    }
    ToggleSideBarPosition {
        id: "toggle_side_bar_position",
        title: "View: Toggle Primary Side Bar Position",
        hint: "",
    }
    SetPanelAlignment {
        id: "set_panel_alignment",
        title: "View: Set Panel Alignment…",
        hint: "",
    }
    SetQuickInputPosition {
        id: "set_quick_input_position",
        title: "View: Set Quick Input Position…",
        hint: "",
    }
    /// View: Customize Layout… (#852): the popup itself.
    CustomizeLayout {
        id: "customize_layout",
        title: "View: Customize Layout…",
        hint: "",
    }
    /// Terminal: Focus Terminal (#852): the TERMINAL tab, shown and focused.
    /// With the `Show*` commands below, one command per bottom-panel tab
    /// (`BottomPanelTab::show_command` pairs them).
    FocusTerminal {
        id: "focus_terminal",
        title: "Terminal: Focus Terminal",
        hint: "Cmd+Shift+T",
        group: Terminal,
    }
    ShowProblems {
        id: "show_problems",
        title: "View: Show Problems",
        hint: "Cmd+Shift+M",
    }
    ShowOutput {
        id: "show_output",
        title: "View: Show Output",
        hint: "Cmd+Shift+U",
    }
    /// Output: Select Channel… (#852): the OUTPUT dropdown as a picker.
    OutputSelectChannel {
        id: "output_select_channel",
        title: "Output: Select Channel…",
        hint: "",
    }
    ShowPorts {
        id: "show_ports",
        title: "View: Show Ports",
        hint: "",
    }
    ShowCaptures {
        id: "show_captures",
        title: "View: Show Captures",
        hint: "",
    }
    ToggleMinimap {
        id: "toggle_minimap",
        title: "View: Toggle Minimap",
        hint: "Cmd+Opt+M",
    }
    /// Run the whole-project checker and put its findings in PROBLEMS (#256).
    ProblemsCheckProject {
        id: "problems_check_project",
        title: "Problems: Check Whole Project",
        hint: "",
    }
    /// Cycle whether that check may run WITHOUT being asked (#256).
    ProblemsToggleProjectAuto {
        id: "problems_toggle_project_auto",
        title: "Problems: Whole-Project Check Auto-Run (Auto / On / Off)",
        hint: "",
    }
    ProblemsToggleScope {
        id: "problems_toggle_scope",
        title: "Problems: Toggle Scope (Open Files / Whole Project)",
        hint: "",
    }
    DiffToggleIgnoreWhitespace {
        id: "diff_toggle_ignore_whitespace",
        title: "Diff: Toggle Ignore Whitespace",
        hint: "",
    }
    NewTerminal {
        id: "new_terminal",
        title: "Terminal: Create New Terminal",
        hint: "Cmd+T",
        group: Terminal,
    }
    /// Terminal: Run Selected Text in Active Terminal (#1292): the
    /// editor's selection, or the caret's line, run in the active pane.
    RunSelectedText {
        id: "run_selected_text",
        title: "Terminal: Run Selected Text in Active Terminal",
        hint: "",
        group: Terminal,
    }
    /// Terminal: Focus Next / Previous Terminal (#843): Cmd+] / Cmd+[, which
    /// have no `Ctrl` form (`Ctrl+[` is `Esc`).
    FocusNextTerminal {
        id: "focus_next_terminal",
        title: "Terminal: Focus Next Terminal",
        hint: "Cmd+]",
        group: Terminal,
    }
    FocusPreviousTerminal {
        id: "focus_previous_terminal",
        title: "Terminal: Focus Previous Terminal",
        hint: "Cmd+[",
        group: Terminal,
    }
    /// The terminal's own actions as commands (#1562): before, each was a
    /// fixed chord or a right-click row only, so the palette could not find
    /// it, `keybindings.json` could not bind it, and `croft import-vscode`
    /// dropped a VS Code user's chord for it. Each acts on the active pane.
    TerminalClear {
        id: "terminal_clear",
        title: "Terminal: Clear",
        hint: "Cmd+K K",
        group: Terminal,
    }
    TerminalRename {
        id: "terminal_rename",
        title: "Terminal: Rename",
        hint: "Cmd+K R",
        group: Terminal,
    }
    TerminalKill {
        id: "terminal_kill",
        title: "Terminal: Kill the Active Terminal Instance",
        hint: "",
        group: Terminal,
    }
    TerminalSplit {
        id: "terminal_split",
        title: "Terminal: Split Terminal",
        hint: "",
        group: Terminal,
    }
    TerminalNewWithProfile {
        id: "terminal_new_with_profile",
        title: "Terminal: Create New Terminal (With Profile)",
        hint: "",
        group: Terminal,
    }
    TerminalQuickSelect {
        id: "terminal_quick_select",
        title: "Terminal: Quick Select",
        hint: "Ctrl+Shift+Space",
        group: Terminal,
    }
    TerminalCopyMode {
        id: "terminal_copy_mode",
        title: "Terminal: Enter Copy Mode",
        hint: "Ctrl+Shift+Y",
        group: Terminal,
    }
    TerminalCommandHistory {
        id: "terminal_command_history",
        title: "Terminal: Show Command History",
        hint: "Ctrl+Shift+H",
        group: Terminal,
    }
    TerminalOpenScrollback {
        id: "terminal_open_scrollback",
        title: "Terminal: Open Scrollback in Editor",
        hint: "Cmd+K D",
        group: Terminal,
    }
    // --- Run / debug ---
    StartDebugging {
        id: "start_debugging",
        title: "Debug: Start Debugging",
        hint: "F5",
    }
    SelectDebugConfig {
        id: "select_debug_config",
        title: "Debug: Select and Start Debugging",
        hint: "",
    }
    AddDebugConfig {
        id: "add_debug_config",
        title: "Debug: Add Configuration…",
        hint: "",
    }
    StopDebugging {
        id: "stop_debugging",
        title: "Debug: Stop Debugging",
        hint: "Shift+F5",
    }
    PauseDebugging {
        id: "pause_debugging",
        title: "Debug: Pause",
        hint: "F6",
    }
    SwitchDebugSession {
        id: "switch_debug_session",
        title: "Debug: Switch Session",
        hint: "Cmd+Opt+Shift+G",
    }
    RestartDebugging {
        id: "restart_debugging",
        title: "Debug: Restart",
        hint: "Shift+Cmd+F5",
    }
    ToggleBreakpoint {
        id: "toggle_breakpoint",
        title: "Debug: Toggle Breakpoint",
        hint: "F9",
    }
    EditBreakpointCondition {
        id: "edit_breakpoint_condition",
        title: "Debug: Add Conditional Breakpoint",
        hint: "Shift+F9",
    }
    EditLogpoint {
        id: "edit_logpoint",
        title: "Debug: Add Logpoint",
        hint: "Shift+Alt+F9",
    }
    ShowIncomingCalls {
        id: "show_incoming_calls",
        title: "Calls: Show Incoming Calls",
        hint: "Cmd+K H",
    }
    ShowOutgoingCalls {
        id: "show_outgoing_calls",
        title: "Calls: Show Outgoing Calls",
        hint: "Cmd+K Shift+H",
    }
    ShowSupertypes {
        id: "show_supertypes",
        title: "Types: Show Supertypes",
        hint: "Cmd+K Shift+U",
    }
    ShowSubtypes {
        id: "show_subtypes",
        title: "Types: Show Subtypes",
        hint: "Cmd+K Shift+D",
    }
    ToggleCodeLens {
        id: "toggle_code_lens",
        title: "Editor: Toggle CodeLens",
        hint: "",
    }
    RunCodeLens {
        id: "run_code_lens",
        title: "Editor: Run CodeLens on This Line",
        hint: "Cmd+K Shift+E",
    }
    DebugAddHitCountBreakpoint {
        id: "debug_add_hit_count_breakpoint",
        title: "Debug: Add Hit Count Breakpoint",
        hint: "",
    }
    DebugAddFunctionBreakpoint {
        id: "debug_add_function_breakpoint",
        title: "Debug: Add Function Breakpoint",
        hint: "",
    }
    DebugRemoveFunctionBreakpoints {
        id: "debug_remove_function_breakpoints",
        title: "Debug: Remove All Function Breakpoints",
        hint: "",
    }
    DebugRunToCursor {
        id: "debug_run_to_cursor",
        title: "Debug: Run to Cursor",
        hint: "Ctrl+F10",
    }
    DebugBreakOnValueChange {
        id: "debug_break_on_value_change",
        title: "Debug: Break When Value Changes",
        hint: "",
    }
    DebugRemoveDataBreakpoints {
        id: "debug_remove_data_breakpoints",
        title: "Debug: Remove All Data Breakpoints",
        hint: "",
    }
    OpenSearchEditor {
        id: "open_search_editor",
        title: "Search: Open Results in Editor",
        hint: "Cmd+K Shift+F",
    }
    RerunSearchEditor {
        id: "rerun_search_editor",
        title: "Search Editor: Rerun",
        hint: "Cmd+K Shift+R",
    }
    RebaseAbort {
        id: "rebase_abort",
        title: "Rebase: Abort",
        hint: "",
    }
    GitContinueOperation {
        id: "git_continue_operation",
        title: "Git: Continue Rebase, Cherry-Pick or Revert",
        hint: "",
    }
    GitSkipOperation {
        id: "git_skip_operation",
        title: "Git: Skip Commit (Rebase, Cherry-Pick or Revert)",
        hint: "",
    }
    GitAbortOperation {
        id: "git_abort_operation",
        title: "Git: Abort Merge, Rebase, Cherry-Pick, Revert or Bisect",
        hint: "",
    }
    /// Source Control's Tags menu on the palette (#1582), so each can be
    /// bound to a key.
    GitCreateTag {
        id: "git_create_tag",
        title: "Git: Create Tag",
        hint: "",
    }
    GitPushTags {
        id: "git_push_tags",
        title: "Git: Push Tags",
        hint: "",
    }
    GitDeleteRemoteTag {
        id: "git_delete_remote_tag",
        title: "Git: Delete Remote Tag",
        hint: "",
    }
    /// The Commit button's and menus' everyday actions on the palette
    /// (#871), so each is reachable, and bindable, from the keyboard.
    GitCommit {
        id: "git_commit",
        title: "Git: Commit",
        hint: "",
    }
    GitPush {
        id: "git_push",
        title: "Git: Push",
        hint: "",
    }
    GitPull {
        id: "git_pull",
        title: "Git: Pull",
        hint: "",
    }
    GitSync {
        id: "git_sync",
        title: "Git: Sync",
        hint: "",
    }
    GitCheckoutTo {
        id: "git_checkout_to",
        title: "Git: Checkout to\u{2026}",
        hint: "",
    }
    GitStash {
        id: "git_stash",
        title: "Git: Stash",
        hint: "",
    }
    /// Source Control's `⋯` actions menu from the keyboard (#871): every
    /// Git action it lists, reached with the arrows and run with Enter.
    GitShowSourceControlActions {
        id: "git_show_source_control_actions",
        title: "Git: Show Source Control Actions",
        hint: "Cmd+K Shift+A",
    }
    /// The Commit button's `▾` dropdown from the keyboard (#871).
    GitShowCommitActions {
        id: "git_show_commit_actions",
        title: "Git: Show Commit Actions",
        hint: "Cmd+K Shift+C",
    }
    /// The open file's changes (#871), or the selected Source Control row's
    /// while the change list has the keys; its keys are the shortcuts.
    GitOpenChanges {
        id: "git_open_changes",
        title: "Git: Open Changes",
        hint: "Enter in Source Control",
    }
    GitStageChanges {
        id: "git_stage_changes",
        title: "Git: Stage Changes",
        hint: "S in Source Control",
    }
    GitUnstageChanges {
        id: "git_unstage_changes",
        title: "Git: Unstage Changes",
        hint: "U in Source Control",
    }
    GitDiscardChanges {
        id: "git_discard_changes",
        title: "Git: Discard Changes",
        hint: "D in Source Control",
    }
    /// The header's refresh button (#871); `R` in Source Control's lists.
    GitRefresh {
        id: "git_refresh",
        title: "Git: Refresh",
        hint: "R in Source Control",
    }
    ToggleTerminalSuggestions {
        id: "toggle_terminal_suggestions",
        title: "Terminal: Toggle Command Suggestions",
        hint: "",
        group: Terminal,
    }
    ToggleScreenReader {
        id: "toggle_screen_reader",
        title: "Accessibility: Toggle Screen Reader Mode",
        hint: "",
    }
    OpenKeyboardShortcuts {
        id: "open_keyboard_shortcuts",
        title: "Preferences: Open Keyboard Shortcuts",
        hint: "Cmd+K Cmd+S",
    }
    SwitchProfile {
        id: "switch_profile",
        title: "Profiles: Switch Profile",
        hint: "",
    }
    ToggleInlineSuggestions {
        id: "toggle_inline_suggestions",
        title: "Editor: Toggle Inline Suggestions",
        hint: "Cmd+K Shift+G",
    }
    StepOver {
        id: "step_over",
        title: "Debug: Step Over",
        hint: "F10",
    }
    /// The right-click menu of what has the keys, at it (#2739): VS Code's
    /// Shift+F10. The Menu key opens it too.
    ShowContextMenu {
        id: "show_context_menu",
        title: "View: Show Context Menu",
        hint: "Shift+F10",
    }
    /// Debug: Continue / Step Into / Step Out (#1543): the F5 / F11 /
    /// Shift+F11 actions as commands, so the palette finds them and
    /// `keybindings.json` can rebind them.
    ContinueDebugging {
        id: "continue",
        title: "Debug: Continue",
        hint: "F5",
    }
    StepInto {
        id: "step_into",
        title: "Debug: Step Into",
        hint: "F11",
    }
    StepOut {
        id: "step_out",
        title: "Debug: Step Out",
        hint: "Shift+F11",
    }
    /// Debug: Step Into Target (#1543): pick which call on the paused line
    /// to step into (DAP `stepInTargets`).
    StepIntoTarget {
        id: "step_into_target",
        title: "Debug: Step Into Target",
        hint: "",
    }
    ToggleRaisedExceptions {
        id: "toggle_raised_exceptions",
        title: "Debug: Toggle Break on Raised Exceptions",
        hint: "Alt+F9",
    }
    AttachPythonProcess {
        id: "attach_python_process",
        title: "Debug: Attach to Python Process",
        hint: "Ctrl+F5",
    }
    RunTask {
        id: "run_task",
        title: "Tasks: Run Task",
        hint: "",
    }
    RunBuildTask {
        id: "run_build_task",
        title: "Tasks: Run Build Task",
        hint: "Cmd+Shift+B",
    }
    RerunLastTask {
        id: "rerun_last_task",
        title: "Tasks: Rerun Last Task",
        hint: "",
    }
    ColorTheme {
        id: "color_theme",
        title: "Preferences: Color Theme",
        hint: "Cmd+K Cmd+T",
    }
    KeyboardShortcuts {
        id: "keyboard_shortcuts",
        title: "Help: Keyboard Shortcuts Reference",
        hint: "F1",
    }
    UpdateCroft {
        id: "update_croft",
        title: "Help: Update croft (Relaunch or Rebuild)",
        hint: "Cmd+Shift+F9",
    }
    OpenSettings {
        id: "open_settings",
        title: "Preferences: Open Settings",
        // Palette-only by default; the whole point of the keybindings.json
        // loader is that a user can bind these (the seeded template shows
        // Cmd+, -> open_settings as the example).
        hint: "",
    }
    OpenSettingsEditor {
        id: "open_settings_editor",
        title: "Preferences: Open Settings (UI)",
        hint: "",
    }
    OpenSettingsJson {
        id: "open_settings_json",
        title: "Preferences: Open Settings (JSON)",
        hint: "",
    }
    OpenWorkspaceSettingsJson {
        id: "open_workspace_settings_json",
        title: "Preferences: Open Workspace Settings (JSON)",
        hint: "",
    }
    OpenWorkspaceSettingsLocalJson {
        id: "open_workspace_settings_local_json",
        title: "Preferences: Open Workspace Settings — Local (JSON)",
        hint: "",
    }
    OpenKeybindingsJson {
        id: "open_keybindings_json",
        title: "Preferences: Open Keyboard Shortcuts (JSON)",
        hint: "",
    }
    ConfigureSnippets {
        id: "configure_snippets",
        title: "Preferences: Configure User Snippets",
        hint: "",
    }
    OpenTriggersJson {
        id: "open_triggers_json",
        title: "Preferences: Open Terminal Triggers (JSON)",
        hint: "",
    }
    OpenAgentsJson {
        id: "open_agents_json",
        title: "Preferences: Open Agent Lanes (JSON)",
        hint: "",
    }
    OpenMatchersJson {
        id: "open_matchers_json",
        title: "Preferences: Open Problem Matchers (JSON)",
        hint: "",
    }
    ToggleTerminalTimestamps {
        id: "toggle_terminal_timestamps",
        title: "Terminal: Toggle Timestamps",
        hint: "",
        group: Terminal,
    }
    ToggleLogHighlight {
        id: "toggle_log_highlight",
        title: "Log: Toggle Highlighting (tailspin)",
        hint: "",
    }
    CollapseTerminalPane {
        id: "collapse_terminal_pane",
        title: "Terminal: Collapse Pane",
        hint: "Cmd+K [",
        group: Terminal,
    }
    RestoreTerminalPanes {
        id: "restore_terminal_panes",
        title: "Terminal: Restore All Collapsed Panes",
        hint: "Cmd+K ]",
        group: Terminal,
    }
    ToggleSecretRedaction {
        id: "toggle_secret_redaction",
        title: "Terminal: Toggle Secret Redaction",
        hint: "",
        group: Terminal,
    }
    RevealRedactedSecrets {
        id: "reveal_redacted_secrets",
        title: "Terminal: Reveal Redacted Secrets for 10s",
        hint: "",
        group: Terminal,
    }
    SearchFromTerminal {
        id: "search_from_terminal",
        title: "Terminal: Search & Replace from Last grep/rg",
        hint: "",
        group: Terminal,
    }
    /// Multiplayer: list who is attached to this persistent session and
    /// grant/revoke write control or disconnect them (docs/MULTIPLAYER.md).
    SessionParticipants {
        id: "session_participants",
        title: "Session: Participants",
        hint: "Cmd+K A",
    }
    /// Detach the client that invoked it from the persistent session; the
    /// session keeps running (#679).
    SessionDetach {
        id: "session_detach",
        title: "Session: Detach",
        hint: "Cmd+K Shift+Q",
    }
    /// Cancel the AI pilot's token stream into a shared file; the pilot
    /// reverts the streamed text (`croft pair`, docs/MULTIPLAYER.md).
    CollabCancelStream {
        id: "collab_cancel_stream",
        title: "Collab: Cancel AI Stream",
        hint: "Cmd+K X",
    }
    /// Ask the resident navigator about the caret line or selection (opens
    /// the instruction box; the navigator may edit on the resulting turn).
    AskNavigator {
        id: "navigator_ask",
        title: "Navigator: Ask About Line or Selection",
        hint: "Cmd+K Q",
    }
    AskNavigatorAboutCapture {
        id: "captures_ask_navigator",
        title: "Captures: Ask Navigator About This Line",
        hint: "",
    }
    /// Re-root the workspace onto the host the active pane is ssh'd into (#364).
    OpenWorkspaceOnSshHost {
        id: "remote_open_workspace_on_ssh_host",
        title: "Remote: Open Workspace on This Pane's Host",
        hint: "",
    }
    /// Step the editor back through the branch's commits (#371).
    ScrubHistory {
        id: "scm_scrub_history",
        title: "Source Control: Scrub History",
        hint: "",
    }
    DebugInstallDelve {
        id: "debug_install_delve",
        title: "Debug: Install Go Debugger (delve)",
        hint: "",
    }
    /// Remote: Sync Config Now (#262): push the syncable config to a host.
    RemoteSyncConfigNow {
        id: "remote_sync_config_now",
        title: "Remote: Sync Config Now",
        hint: "",
    }
    /// Remote: Drop to Local (#2071): end this remote croft and go back to
    /// the local one it was launched from.
    DropToLocal {
        id: "drop_to_local",
        title: "Remote: Drop to Local",
        // Its own chord (#2071): Cmd+Shift+L, which it shared, selects
        // all occurrences everywhere, a remote session included.
        hint: "Cmd+K L",
    }
    StopAutoApprove {
        id: "agents_stop_auto_approve",
        title: "Agents: Stop Auto-Approving",
        hint: "",
    }
    ScrubOpenHere {
        id: "scm_scrub_open_here",
        title: "Source Control: Open Scrubbed File Here",
        hint: "",
    }
    ScrubDiffToWorkingTree {
        id: "scm_scrub_diff_working_tree",
        title: "Source Control: Diff Scrubbed File to Working Tree",
        hint: "",
    }
    /// Run one command across several ssh hosts and compare (#363).
    FleetRun {
        id: "terminal_fleet_run",
        title: "Terminal: Fleet Run",
        hint: "",
        group: Terminal,
    }
    /// Open the symbol under the cursor as its own tab (#369).
    OpenAsSymbolTab {
        id: "editor_open_symbol_tab",
        title: "Editor: Open Symbol as Its Own Tab",
        hint: "Cmd+K Shift+V",
    }
    /// Load this PR's review threads as comment boxes (#366).
    LoadReviewThreads {
        id: "review_load_threads",
        title: "Review: Load PR Comments for This File",
        hint: "",
    }
    ReviewAddComment {
        id: "review_add_comment",
        title: "Review: Add Comment on This Line",
        hint: "",
    }
    ReviewSubmit {
        id: "review_submit",
        title: "Review: Submit Review",
        hint: "",
    }
    ReviewToggleResolved {
        id: "review_toggle_resolved",
        title: "Review: Resolve or Unresolve Thread",
        hint: "",
    }
    ReviewDiscardPending {
        id: "review_discard_pending",
        title: "Review: Discard Pending Comments",
        hint: "",
    }
    ExportCommentsToPr {
        id: "scm_export_comments_to_pr",
        title: "Source Control: Export Comments to Pull Request",
        hint: "",
    }
    NoteAdd {
        id: "note_add",
        title: "Notes: Add Note on This Line",
        hint: "Cmd+K N",
    }
    NoteDelete {
        id: "note_delete",
        title: "Notes: Delete Note",
        hint: "",
    }
    /// Start or stop recording the active terminal as an asciicast (#356).
    ToggleSessionRecording {
        id: "session_toggle_recording",
        title: "Session: Record Terminal as Asciicast",
        hint: "",
    }
    /// Create a git worktree lane for an agent to work in (#348).
    NewWorktreeLane {
        id: "agent_new_worktree_lane",
        title: "Agent: New Worktree Lane",
        hint: "Cmd+K Shift+L",
    }
    ReviewPullRequest {
        id: "review_pull_request",
        title: "Source Control: Review Pull Request",
        hint: "",
    }
    /// Remove the current worktree lane, refusing when it is dirty (#348).
    CloseWorktreeLane {
        id: "agent_close_worktree_lane",
        title: "Agent: Close Worktree Lane",
        hint: "",
    }
    /// Point the COMMITS graph at what the current lane added (#348).
    DiffWorktreeLane {
        id: "agent_diff_worktree_lane",
        title: "Agent: Diff Lane Against Its Base",
        // No chord: the lane commands that have one are the ones you
        // reach mid-flow; a diff is deliberate and the palette is where
        // you go for it.
        hint: "",
    }
    /// Mark every file every agent changed as reviewed (#345).
    MarkAgentLaneReviewed {
        id: "agents_mark_reviewed",
        title: "Agents: Mark Changed Files Reviewed",
        hint: "",
    }
    /// Summarise each agent's review queue (#345).
    ShowAgentLane {
        id: "agents_show_lane",
        title: "Agents: Show Changed Files",
        hint: "",
    }
    /// Show the Explorer's AGENT LANE section, open (#345).
    OpenAgentLaneSection {
        id: "agents_open_lane",
        title: "Agents: Open Agent Lane",
        hint: "",
    }
    /// Mark the active file reviewed in every agent lane holding it (#345).
    MarkAgentFileReviewed {
        id: "agents_mark_file_reviewed",
        title: "Agents: Mark This File Reviewed",
        hint: "",
    }
    /// Diff the active file against the snapshot it was last reviewed
    /// against (#345).
    DiffAgentFileSinceReview {
        id: "agents_diff_since_review",
        title: "Agents: Diff This File Since Review",
        hint: "",
    }
    /// Pick any file an agent changed and open its diff since review, from
    /// the keyboard (#345).
    PickAgentLaneFile {
        id: "agents_pick_changed_file",
        title: "Agents: Review a Changed File…",
        hint: "",
    }
    /// Have the navigator fix the diagnostic under the caret as a streamed,
    /// cancellable edit (#374).
    FixProblemWithNavigator {
        id: "problems_fix_navigator",
        title: "Problems: Fix With Navigator",
        hint: "",
    }
    /// Send the `.http` request under the caret; the response opens as a
    /// tab (#370).
    SendHttpRequest {
        id: "http_send_request",
        title: "HTTP: Send Request Under Caret",
        hint: "Cmd+Enter",
    }
    /// Copy the `.http` request under the caret as a curl command (#370).
    CopyHttpRequestAsCurl {
        id: "http_copy_curl",
        title: "HTTP: Copy Request as curl",
        hint: "",
    }
    /// Pick the named environment of the `.http` env files whose values
    /// fill requests (#924).
    SwitchHttpEnvironment {
        id: "http_switch_environment",
        title: "HTTP: Switch Environment",
        hint: "",
    }
    /// Hand the navigator the floor on the active file: a comment-only
    /// review turn, its say anchored as comment boxes.
    YieldToNavigator {
        id: "navigator_yield",
        title: "Navigator: Yield the Turn",
        hint: "Cmd+K Y",
    }
    /// Activate or deactivate the workspace's resident navigator (writes
    /// the pair record `croft pair` uses; the tick loop seats or unseats).
    ToggleNavigator {
        id: "navigator_toggle",
        title: "Navigator: Activate or Deactivate",
        hint: "",
    }
    /// Drop every navigator comment box.
    ClearNavigatorNotes {
        id: "navigator_clear_notes",
        title: "Navigator: Clear Comments",
        hint: "",
    }
    /// Toggle the navigator's proactive comment-only looks (a completed
    /// construct plus a typing pause hands it the floor on its own).
    ToggleProactiveNavigator {
        id: "navigator_toggle_proactive",
        title: "Navigator: Toggle Proactive Comments",
        hint: "",
    }
    /// Focus the active file's next navigator comment box (F4).
    NextComment {
        id: "navigator_next_comment",
        title: "Navigator: Next Comment",
        hint: "F4",
    }
    /// Ignore the focused navigator comment box, or the next one from the
    /// caret (Shift+F4).
    IgnoreComment {
        id: "navigator_ignore_comment",
        title: "Navigator: Ignore Comment",
        hint: "Shift+F4",
    }
}

/// The Customize Layout popup as palette commands (#852): the popup itself,
/// then one command per row group in the popup's order. VS Code's titles
/// mostly don't say "layout", so the palette also matches these on that
/// word, and the query "layout" lists everything the popup can change.
pub const LAYOUT_COMMANDS: &[Command] = &[
    Command::CustomizeLayout,
    Command::ToggleActivityBar,
    Command::ToggleSideBar,
    Command::ToggleSecondarySideBar,
    Command::ToggleTerminal,
    Command::ToggleStatusBar,
    Command::ToggleMinimap,
    Command::ToggleAutoHideSideBar,
    Command::ToggleSideBarPosition,
    Command::SetPanelAlignment,
    Command::SetQuickInputPosition,
    Command::ToggleZenMode,
    Command::ToggleCenteredLayout,
];

impl Command {
    /// An extra word the palette matches this command on, beyond its
    /// title: "layout" for the Customize Layout commands (#852), whose
    /// VS Code names mostly don't say it, the tab menu's "Keep Open" for
    /// View: Keep Editor, the hosts a shared link opens on (#1610), and VS
    /// Code's own names for restarting a language server (#1621), and
    /// "blame" for the commands on the caret line's commit (#1556). Empty
    /// for the rest.
    pub fn keyword(self) -> &'static str {
        if LAYOUT_COMMANDS.contains(&self) {
            "layout"
        } else if self == Command::KeepEditor {
            "keep open"
        } else if matches!(
            self,
            Command::RestartLanguageServers | Command::RestartLanguageServersForFile
        ) {
            "restart language server, restart ts server, restart lsp"
        } else if matches!(
            self,
            Command::GitCopyPermalink | Command::GitCopyHeadLink | Command::GitOpenOnRemote
        ) {
            "github gitlab bitbucket share link url"
        } else if matches!(
            self,
            Command::GitOpenLineCommit | Command::GitCopyLineCommitHash
        ) {
            "blame"
        } else {
            ""
        }
    }

    /// Whether a `keybindings.json` chord for this command runs it while the
    /// terminal has focus, instead of going to the shell (#1562). VS Code
    /// sends its terminal commands' chords past the shell the same way
    /// (`terminal.integrated.commandsToSkipShell`): a user who binds
    /// Terminal: Clear means it for the terminal.
    pub fn skips_the_shell(self) -> bool {
        self.group() == Some(Group::Terminal)
    }

    /// Whether this command belongs to the CodeQL extension: its side bar,
    /// or one of its "CodeQL:" commands. CodeQL is not built in (#1842), so
    /// these leave the palette until it is added and enabled.
    pub fn is_codeql(self) -> bool {
        self.group() == Some(Group::CodeQl)
    }
}

/// The `Cmd` chords with no `Ctrl` form off macOS (the table in
/// docs/LINUX.md), by their hint, with what the hint reads there instead:
/// `Super`, which is `Cmd` on Linux and reaches croft over the kitty keyboard
/// protocol, or the chord Linux uses. `Cmd+Shift+T` (focus the terminal) is
/// here for when a palette row carries it: `Ctrl+Shift+T` splits one.
const HINTS_WITHOUT_A_CTRL_FORM: &[(&str, &str)] = &[
    ("Cmd+\\", "Super+\\"),
    ("Cmd+Shift+\\", "Super+Shift+\\"),
    ("Cmd+Opt+\\", "Super+Alt+\\"),
    ("Cmd+Opt+←", "Super+Alt+←"),
    ("Cmd+Opt+→", "Super+Alt+→"),
    ("Cmd+]", "Super+]"),
    ("Cmd+[", "Super+["),
    ("Cmd+T", "Ctrl+Shift+T"),
    ("Cmd+Shift+T", "Super+Shift+T"),
    ("Cmd+A", "Super+A"),
    ("Cmd+E", "Super+E"),
    ("Cmd+F12", "Super+F12"),
    // A legacy terminal sends `Ctrl+\` as `Ctrl+4`; the leader's second key
    // works without a modifier.
    ("Cmd+K Cmd+\\", "Ctrl+K \\"),
    ("Cmd+K Cmd+Shift+\\", "Ctrl+K Shift+\\"),
];

/// `hint` spelled for the platform croft runs on: [`hint_for_platform`].
pub fn platform_hint(hint: &str) -> std::borrow::Cow<'_, str> {
    hint_for_platform(hint, cfg!(target_os = "macos"))
}

/// A keybinding hint as a Mac (`macos`) or anything else spells it (#843).
/// The hints are written with the macOS names; off macOS the command
/// modifier is `Ctrl` (docs/LINUX.md), so `Cmd+/` reads `Ctrl+/` there and
/// `Opt` reads `Alt`, except the chords with no `Ctrl` form, which read as
/// [`HINTS_WITHOUT_A_CTRL_FORM`] says. A hint without `Cmd` or `Opt` is the
/// same everywhere.
pub fn hint_for_platform(hint: &str, macos: bool) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if macos || !(hint.contains("Cmd") || hint.contains("Opt")) {
        return Cow::Borrowed(hint);
    }
    if let Some((_, there)) = HINTS_WITHOUT_A_CTRL_FORM
        .iter()
        .find(|(mac, _)| *mac == hint)
    {
        return Cow::Borrowed(there);
    }
    Cow::Owned(hint.replace("Cmd", "Ctrl").replace("Opt", "Alt"))
}

/// A shortcut written in the Mac's menu glyphs (`⌥⌘R`, `⌘K ⇧P`, as the
/// context menus show them) as a Mac (`macos`) or anything else spells it
/// (#885). Elsewhere each step is first written in the palette's names
/// (`Cmd+Opt+R`) and then spelled by [`hint_for_platform`], so a menu and
/// the palette agree on `Ctrl`, `Alt` and `Super` (`Ctrl+Alt+R`, `Super+\`).
/// `⌫` reads `Delete`, as the key is named there.
pub fn glyph_hint_for_platform(glyphs: &str, macos: bool) -> std::borrow::Cow<'_, str> {
    if macos {
        return std::borrow::Cow::Borrowed(glyphs);
    }
    let names = glyphs
        .split(' ')
        .map(|step| {
            let key = step.trim_start_matches(['⌃', '⌘', '⌥', '⇧']);
            let held = &step[..step.len() - key.len()];
            let mut words: Vec<&str> = [('⌃', "Ctrl"), ('⌘', "Cmd"), ('⌥', "Opt"), ('⇧', "Shift")]
                .into_iter()
                .filter(|(glyph, _)| held.contains(*glyph))
                .map(|(_, word)| word)
                .collect();
            words.push(if key == "⌫" { "Delete" } else { key });
            words.join("+")
        })
        .collect::<Vec<_>>()
        .join(" ");
    std::borrow::Cow::Owned(hint_for_platform(&names, false).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2062: the lookup made from the table finds every command by its id,
    /// and nothing for an id no row has, as the old scan of every command
    /// did.
    #[test]
    fn every_id_resolves_to_its_command() {
        for cmd in ALL_COMMANDS {
            assert_eq!(Command::from_id(cmd.id()), Some(*cmd), "{cmd:?}");
        }
        for unknown in ["", "no_such_command", "Move_Line_Up", "move_line_up "] {
            assert_eq!(Command::from_id(unknown), None, "{unknown:?}");
        }
    }

    /// How many palette commands show no chord on this platform: the ratchet
    /// below holds it there or lower. Kill to End of Line has one on macOS
    /// only, which is the difference between the two.
    const WITHOUT_A_CHORD: usize = if cfg!(target_os = "macos") { 271 } else { 272 };

    /// Everything has a shortcut (TENETS.md): every row of the table must
    /// state its hint, and the number with none may only fall. A new command
    /// needs a chord; one that gains a chord lowers the number here, so it
    /// cannot rise again unseen.
    #[test]
    fn commands_without_a_chord_never_grow_in_number() {
        let without: Vec<&Command> = ALL_COMMANDS
            .iter()
            .filter(|cmd| cmd.keybinding_hint().is_empty())
            .collect();
        assert!(
            without.len() <= WITHOUT_A_CHORD,
            "{} palette commands show no chord, more than the {WITHOUT_A_CHORD} allowed: \
             a new command needs a chord in its row of the table",
            without.len()
        );
        assert!(
            without.len() >= WITHOUT_A_CHORD,
            "{} palette commands show no chord, fewer than {WITHOUT_A_CHORD}: lower \
             WITHOUT_A_CHORD to {} here (and the other platform's number with it) so \
             the count cannot rise again",
            without.len(),
            without.len()
        );
    }

    #[test]
    fn every_command_has_a_nonempty_title() {
        for cmd in ALL_COMMANDS {
            assert!(!cmd.title().is_empty(), "{cmd:?} has empty title");
        }
    }

    /// #843: off macOS a hint's `Cmd` reads `Ctrl` and its `Opt` reads `Alt`,
    /// on every chord of a sequence.
    #[test]
    fn off_macos_a_hint_reads_ctrl_for_cmd_and_alt_for_opt() {
        for (mac, linux) in [
            ("Cmd+/", "Ctrl+/"),
            ("Shift+Cmd+Z", "Shift+Ctrl+Z"),
            ("Cmd+Opt+Shift+J", "Ctrl+Alt+Shift+J"),
            ("Cmd+K Cmd+T", "Ctrl+K Ctrl+T"),
            ("Cmd+K Z", "Ctrl+K Z"),
            ("Cmd+Enter", "Ctrl+Enter"),
        ] {
            assert_eq!(hint_for_platform(mac, false), linux);
        }
    }

    /// Guard (#843): on macOS every hint keeps its written `Cmd` / `Opt`
    /// names, and a hint without them is the same on every platform.
    #[test]
    fn a_hint_keeps_its_names_on_macos_and_without_cmd() {
        for hint in ["Cmd+/", "Cmd+Opt+Shift+J", "Cmd+E", "Cmd+\\", "Cmd+T"] {
            assert_eq!(hint_for_platform(hint, true), hint);
        }
        for hint in ["Alt+\u{2191}", "F5", "Shift+F9", "Ctrl+J", "Ctrl+-", ""] {
            assert_eq!(hint_for_platform(hint, true), hint);
            assert_eq!(hint_for_platform(hint, false), hint);
        }
    }

    /// Guard (#843): a chord LINUX.md lists without a `Ctrl` form never reads
    /// `Ctrl`+ the same key off macOS (that `Ctrl` chord does something else,
    /// or nothing): it reads `Super`, or the chord Linux uses instead.
    #[test]
    fn a_chord_without_a_ctrl_form_never_reads_as_one() {
        for (mac, linux) in [
            ("Cmd+E", "Super+E"),
            ("Cmd+A", "Super+A"),
            ("Cmd+\\", "Super+\\"),
            ("Cmd+Shift+\\", "Super+Shift+\\"),
            ("Cmd+Opt+\\", "Super+Alt+\\"),
            ("Cmd+Opt+\u{2190}", "Super+Alt+\u{2190}"),
            ("Cmd+Opt+\u{2192}", "Super+Alt+\u{2192}"),
            ("Cmd+]", "Super+]"),
            ("Cmd+[", "Super+["),
            ("Cmd+T", "Ctrl+Shift+T"),
            ("Cmd+F12", "Super+F12"),
        ] {
            assert_eq!(hint_for_platform(mac, false), linux);
        }
    }

    /// #885: a menu's glyph chord reads the palette's names off macOS, by
    /// the same `Ctrl` / `Super` rules, and stays as drawn on a Mac.
    #[test]
    fn a_glyph_chord_reads_as_the_palette_names_it_off_macos() {
        for (mac, linux) in [
            ("⌘B", "Ctrl+B"),
            ("⌥⌘M", "Ctrl+Alt+M"),
            ("⌥⇧⌘C", "Ctrl+Alt+Shift+C"),
            ("⌘⇧N", "Ctrl+Shift+N"),
            ("⇧⌥F9", "Alt+Shift+F9"),
            ("⌃⇧Space", "Ctrl+Shift+Space"),
            ("⌘K Z", "Ctrl+K Z"),
            ("⌘K ⇧P", "Ctrl+K Shift+P"),
            ("⌘K ⌘T", "Ctrl+K Ctrl+T"),
            ("⌘\\", "Super+\\"),
            ("⌥⌘\u{2190}", "Super+Alt+\u{2190}"),
            ("⌘T", "Ctrl+Shift+T"),
            ("⌘F12", "Super+F12"),
            ("⌘K ⌘\\", "Ctrl+K \\"),
            ("⌘K ⇧⌘\\", "Ctrl+K Shift+\\"),
            ("⌫", "Delete"),
            ("F2", "F2"),
            ("Enter", "Enter"),
        ] {
            assert_eq!(glyph_hint_for_platform(mac, false), linux);
            assert_eq!(glyph_hint_for_platform(mac, true), mac);
        }
    }

    /// #885: off macOS no palette or Keyboard Shortcuts row names a Mac key.
    #[test]
    fn off_macos_no_palette_hint_names_a_mac_key() {
        for cmd in ALL_COMMANDS {
            let hint = hint_for_platform(cmd.keybinding_hint(), false);
            for mac in ["Cmd", "Opt", "⌘", "⌥"] {
                assert!(!hint.contains(mac), "{cmd:?} reads {hint:?}");
            }
        }
    }
}
