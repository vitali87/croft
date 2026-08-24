//! VS Code "Command Palette" (Cmd/Ctrl+Shift+P): a fuzzy-filtered list of
//! every named action croft can run, invokable from the keyboard. It is the
//! discoverability backbone for actions that have no dedicated chord, and a
//! second way to reach the ones that do. The widget owns only the query and
//! selection; `App::run_command` performs the side effects.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::widgets::file_finder::fuzzy_score;

/// One invokable command. The order of variants is the order commands appear
/// in the palette before the user types anything (grouped by category, most
/// useful first), so keep related commands adjacent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    // --- Editor: line / selection ops (the new VS Code parity features) ---
    MoveLineUp,
    MoveLineDown,
    ToggleLineComment,
    ToggleBlockComment,
    JoinLines,
    DeleteLine,
    TransformUpper,
    TransformLower,
    TransformTitle,
    SortLinesAscending,
    SortLinesDescending,
    TrimTrailingWhitespace,
    ToggleWordWrap,
    ExpandSelection,
    ShrinkSelection,
    ReplaceInFile,
    MergeAcceptCurrent,
    MergeAcceptIncoming,
    MergeAcceptBoth,
    MergeAcceptAllCurrent,
    MergeAcceptAllIncoming,
    MergeNextConflict,
    MergePrevConflict,
    MergeComplete,
    DebugAddWatch,
    DebugClearWatch,
    PeekDefinition,
    ClearBuildDiagnostics,
    StageHunk,
    UnstageHunk,
    RevertHunk,
    AddCursorAbove,
    AddCursorBelow,
    AddSelectionToNextMatch,
    // --- Editor: bracket / character / indentation ops ---
    JumpToBracket,
    SelectToBracket,
    TransposeCharacters,
    IndentationToSpaces,
    IndentationToTabs,
    TrimFinalNewlines,
    FormatDocument,
    ChangeColorPresentation,
    ToggleFormatOnSave,
    QuickFix,
    // --- Editor: folding ---
    ToggleFold,
    FoldAll,
    UnfoldAll,
    FoldAllComments,
    FoldAllRegions,
    UnfoldAllRegions,
    // --- File / editor management ---
    SaveFile,
    Undo,
    Redo,
    ToggleAutoSave,
    ToggleAutoSaveOnFocusChange,
    ToggleInlineBlame,
    ToggleIndentGuides,
    ToggleBracketColors,
    ToggleRenderWhitespace,
    ToggleInlineValues,
    ToggleInlayHints,
    ToggleMarkdownPreview,
    RestoreSnapshot,
    CloseEditor,
    ReopenClosedEditor,
    SplitEditor,
    QuickOpen,
    GoToSymbol,
    GoToWorkspaceSymbol,
    /// VS Code "Go Back" (Ctrl+-): return to the location before the last
    /// navigation jump (Go to Definition, a reference pick, a symbol jump).
    NavigateBack,
    NavigateForward,
    GoToLastEditLocation,
    ToggleVimMode,
    // --- View / navigation ---
    ShowExplorer,
    ShowSearch,
    ShowSourceControl,
    AddWorkspaceFolder,
    RemoveWorkspaceFolder,
    SaveWorkspaceAs,
    OpenWorkspaceFromFile,
    ReopenAsHex,
    ReopenAsPreview,
    ReopenAsText,
    HexFindNext,
    SheetInsertRowBelow,
    SheetDeleteRow,
    SheetInsertColRight,
    SheetDeleteCol,
    MediaOpenExternal,
    ShowRunDebug,
    ShowRemote,
    ShowExtensions,
    ShowTesting,
    RunTestAtCursor,
    DebugTestAtCursor,
    ToggleSideBar,
    ToggleSecondarySideBar,
    ToggleZenMode,
    ToggleTerminal,
    ToggleMinimap,
    NewTerminal,
    // --- Run / debug ---
    StartDebugging,
    SelectDebugConfig,
    StopDebugging,
    PauseDebugging,
    RestartDebugging,
    ToggleBreakpoint,
    EditBreakpointCondition,
    EditLogpoint,
    ShowIncomingCalls,
    ShowOutgoingCalls,
    StepOver,
    ToggleRaisedExceptions,
    AttachPythonProcess,
    ColorTheme,
    KeyboardShortcuts,
    OpenSettings,
    OpenSettingsJson,
    OpenWorkspaceSettingsJson,
    OpenWorkspaceSettingsLocalJson,
    OpenKeybindingsJson,
    ConfigureSnippets,
    OpenTriggersJson,
    OpenMatchersJson,
    ToggleTerminalTimestamps,
    RunTask,
    RunBuildTask,
    RerunLastTask,
    SearchFromTerminal,
    /// Multiplayer: list who is attached to this persistent session and
    /// grant/revoke write control or disconnect them (docs/MULTIPLAYER.md).
    SessionParticipants,
    /// Cancel the AI pilot's token stream into a shared file; the pilot
    /// reverts the streamed text (`croft pair`, docs/MULTIPLAYER.md).
    CollabCancelStream,
    /// Ask the resident navigator about the caret line or selection (opens
    /// the instruction box; the navigator may edit on the resulting turn).
    AskNavigator,
    /// Hand the navigator the floor on the active file: a comment-only
    /// review turn, its say anchored as comment boxes.
    YieldToNavigator,
    /// Activate or deactivate the workspace's resident navigator (writes
    /// the pair record `croft pair` uses; the tick loop seats or unseats).
    ToggleNavigator,
    /// Drop every navigator comment box.
    ClearNavigatorNotes,
    /// Toggle the navigator's proactive comment-only looks (a completed
    /// construct plus a typing pause hands it the floor on its own).
    ToggleProactiveNavigator,
    /// Focus the active file's next navigator comment box (F4).
    NextComment,
    /// Ignore the focused navigator comment box, or the next one from the
    /// caret (Shift+F4).
    IgnoreComment,
}

/// Every command, in palette display order. Single source of truth for both
/// the empty-query list and the test that guards the count.
pub const ALL_COMMANDS: &[Command] = &[
    Command::MoveLineUp,
    Command::MoveLineDown,
    Command::ToggleLineComment,
    Command::ToggleBlockComment,
    Command::JoinLines,
    Command::DeleteLine,
    Command::TransformUpper,
    Command::TransformLower,
    Command::TransformTitle,
    Command::SortLinesAscending,
    Command::SortLinesDescending,
    Command::TrimTrailingWhitespace,
    Command::ToggleWordWrap,
    Command::ExpandSelection,
    Command::ShrinkSelection,
    Command::ReplaceInFile,
    Command::MergeAcceptCurrent,
    Command::MergeAcceptIncoming,
    Command::MergeAcceptBoth,
    Command::MergeAcceptAllCurrent,
    Command::MergeAcceptAllIncoming,
    Command::MergeNextConflict,
    Command::MergePrevConflict,
    Command::MergeComplete,
    Command::DebugAddWatch,
    Command::DebugClearWatch,
    Command::PeekDefinition,
    Command::ClearBuildDiagnostics,
    Command::StageHunk,
    Command::UnstageHunk,
    Command::RevertHunk,
    Command::AddCursorAbove,
    Command::AddCursorBelow,
    Command::AddSelectionToNextMatch,
    Command::JumpToBracket,
    Command::SelectToBracket,
    Command::TransposeCharacters,
    Command::IndentationToSpaces,
    Command::IndentationToTabs,
    Command::TrimFinalNewlines,
    Command::FormatDocument,
    Command::ChangeColorPresentation,
    Command::ToggleFormatOnSave,
    Command::QuickFix,
    Command::ToggleFold,
    Command::FoldAll,
    Command::UnfoldAll,
    Command::FoldAllComments,
    Command::FoldAllRegions,
    Command::UnfoldAllRegions,
    Command::SaveFile,
    Command::Undo,
    Command::Redo,
    Command::ToggleAutoSave,
    Command::ToggleAutoSaveOnFocusChange,
    Command::ToggleInlineBlame,
    Command::ToggleIndentGuides,
    Command::ToggleBracketColors,
    Command::ToggleRenderWhitespace,
    Command::ToggleInlineValues,
    Command::ToggleInlayHints,
    Command::ToggleMarkdownPreview,
    Command::RestoreSnapshot,
    Command::CloseEditor,
    Command::ReopenClosedEditor,
    Command::SplitEditor,
    Command::QuickOpen,
    Command::GoToSymbol,
    Command::GoToWorkspaceSymbol,
    Command::NavigateBack,
    Command::NavigateForward,
    Command::GoToLastEditLocation,
    Command::ToggleVimMode,
    Command::ShowExplorer,
    Command::ShowSearch,
    Command::ShowSourceControl,
    Command::AddWorkspaceFolder,
    Command::RemoveWorkspaceFolder,
    Command::SaveWorkspaceAs,
    Command::OpenWorkspaceFromFile,
    Command::ReopenAsHex,
    Command::ReopenAsPreview,
    Command::ReopenAsText,
    Command::HexFindNext,
    Command::SheetInsertRowBelow,
    Command::SheetDeleteRow,
    Command::SheetInsertColRight,
    Command::SheetDeleteCol,
    Command::MediaOpenExternal,
    Command::ShowRunDebug,
    Command::ShowRemote,
    Command::ShowExtensions,
    Command::ShowTesting,
    Command::RunTestAtCursor,
    Command::DebugTestAtCursor,
    Command::ToggleSideBar,
    Command::ToggleSecondarySideBar,
    Command::ToggleZenMode,
    Command::ToggleTerminal,
    Command::ToggleMinimap,
    Command::NewTerminal,
    Command::StartDebugging,
    Command::SelectDebugConfig,
    Command::StopDebugging,
    Command::PauseDebugging,
    Command::RestartDebugging,
    Command::ToggleBreakpoint,
    Command::EditBreakpointCondition,
    Command::EditLogpoint,
    Command::ShowIncomingCalls,
    Command::ShowOutgoingCalls,
    Command::StepOver,
    Command::ToggleRaisedExceptions,
    Command::AttachPythonProcess,
    Command::RunTask,
    Command::RunBuildTask,
    Command::RerunLastTask,
    Command::ColorTheme,
    Command::KeyboardShortcuts,
    Command::OpenSettings,
    Command::OpenSettingsJson,
    Command::OpenWorkspaceSettingsJson,
    Command::OpenWorkspaceSettingsLocalJson,
    Command::OpenKeybindingsJson,
    Command::ConfigureSnippets,
    Command::OpenTriggersJson,
    Command::OpenMatchersJson,
    Command::ToggleTerminalTimestamps,
    Command::SearchFromTerminal,
    Command::SessionParticipants,
    Command::CollabCancelStream,
    Command::AskNavigator,
    Command::YieldToNavigator,
    Command::ToggleNavigator,
    Command::ClearNavigatorNotes,
    Command::ToggleProactiveNavigator,
    Command::NextComment,
    Command::IgnoreComment,
];

impl Command {
    /// The human-readable label shown in the palette and matched against the
    /// query. Mirrors VS Code's command titles.
    pub fn title(self) -> &'static str {
        match self {
            Command::MoveLineUp => "Move Line Up",
            Command::MoveLineDown => "Move Line Down",
            Command::ToggleLineComment => "Toggle Line Comment",
            Command::ToggleBlockComment => "Toggle Block Comment",
            Command::JoinLines => "Join Lines",
            Command::DeleteLine => "Delete Line",
            Command::TransformUpper => "Transform to Uppercase",
            Command::TransformLower => "Transform to Lowercase",
            Command::TransformTitle => "Transform to Title Case",
            Command::SortLinesAscending => "Sort Lines Ascending",
            Command::SortLinesDescending => "Sort Lines Descending",
            Command::TrimTrailingWhitespace => "Trim Trailing Whitespace",
            Command::ToggleWordWrap => "View: Toggle Word Wrap",
            Command::ExpandSelection => "Expand Selection",
            Command::ShrinkSelection => "Shrink Selection",
            Command::ReplaceInFile => "Replace in File",
            Command::MergeAcceptCurrent => "Merge Conflict: Accept Current",
            Command::MergeAcceptIncoming => "Merge Conflict: Accept Incoming",
            Command::MergeAcceptBoth => "Merge Conflict: Accept Both",
            Command::MergeAcceptAllCurrent => "Merge Conflict: Accept All Current",
            Command::MergeAcceptAllIncoming => "Merge Conflict: Accept All Incoming",
            Command::MergeNextConflict => "Merge Conflict: Next Conflict",
            Command::MergePrevConflict => "Merge Conflict: Previous Conflict",
            Command::MergeComplete => "Merge: Complete Merge (stage file)",
            Command::DebugAddWatch => "Debug: Add Watch Expression",
            Command::DebugClearWatch => "Debug: Remove All Watch Expressions",
            Command::PeekDefinition => "Peek Definition",
            Command::ClearBuildDiagnostics => "Problems: Clear Build Diagnostics",
            Command::StageHunk => "Git: Stage Hunk",
            Command::UnstageHunk => "Git: Unstage Hunk",
            Command::RevertHunk => "Git: Revert Hunk",
            Command::AddCursorAbove => "Add Cursor Above",
            Command::AddCursorBelow => "Add Cursor Below",
            Command::AddSelectionToNextMatch => "Add Selection to Next Find Match",
            Command::JumpToBracket => "Go to Bracket",
            Command::SelectToBracket => "Select to Bracket",
            Command::TransposeCharacters => "Transpose Characters around the Cursor",
            Command::IndentationToSpaces => "Convert Indentation to Spaces",
            Command::IndentationToTabs => "Convert Indentation to Tabs",
            Command::TrimFinalNewlines => "Trim Final Newlines",
            Command::FormatDocument => "Format Document",
            Command::ChangeColorPresentation => "Change Color Presentation",
            Command::ToggleFormatOnSave => "Preferences: Toggle Format on Save",
            Command::QuickFix => "Quick Fix",
            Command::ToggleFold => "Toggle Fold",
            Command::FoldAll => "Fold All",
            Command::UnfoldAll => "Unfold All",
            Command::FoldAllComments => "Fold All Block Comments",
            Command::FoldAllRegions => "Fold All Regions",
            Command::UnfoldAllRegions => "Unfold All Regions",
            Command::SaveFile => "File: Save",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::ToggleAutoSave => "File: Toggle Auto Save",
            Command::ToggleAutoSaveOnFocusChange => "File: Toggle Auto Save on Focus Change",
            Command::ToggleInlineBlame => "Git: Toggle Inline Blame",
            Command::ToggleIndentGuides => "View: Toggle Indent Guides",
            Command::ToggleBracketColors => "Editor: Toggle Bracket Pair Colorization",
            Command::ToggleRenderWhitespace => "View: Toggle Render Whitespace",
            Command::ToggleInlineValues => "Debug: Toggle Inline Values",
            Command::ToggleInlayHints => "Editor: Toggle Inlay Hints",
            Command::ToggleMarkdownPreview => "Markdown: Toggle Preview",
            Command::RestoreSnapshot => "Local History: Restore Snapshot",
            Command::CloseEditor => "View: Close Editor",
            Command::ReopenClosedEditor => "View: Reopen Closed Editor",
            Command::SplitEditor => "View: Split Editor",
            Command::QuickOpen => "Go to File",
            Command::GoToSymbol => "Go to Symbol in Editor",
            Command::GoToWorkspaceSymbol => "Go to Symbol in Workspace",
            Command::NavigateBack => "Go Back",
            Command::NavigateForward => "Go Forward",
            Command::GoToLastEditLocation => "Go to Last Edit Location",
            Command::ToggleVimMode => "Toggle Vim Mode",
            Command::ShowExplorer => "View: Show Explorer",
            Command::ShowSearch => "View: Show Search",
            Command::ShowSourceControl => "View: Show Source Control",
            Command::AddWorkspaceFolder => "Workspaces: Add Folder to Workspace",
            Command::SaveWorkspaceAs => "Workspaces: Save Workspace As",
            Command::OpenWorkspaceFromFile => "Workspaces: Open Workspace from File",
            Command::ReopenAsHex => "File: Reopen as Hex",
            Command::ReopenAsPreview => "File: Reopen as Preview",
            Command::ReopenAsText => "File: Reopen as Text",
            Command::HexFindNext => "Hex: Find Next",
            Command::SheetInsertRowBelow => "Sheet: Insert Row Below",
            Command::SheetDeleteRow => "Sheet: Delete Row",
            Command::SheetInsertColRight => "Sheet: Insert Column Right",
            Command::SheetDeleteCol => "Sheet: Delete Column",
            Command::MediaOpenExternal => "Media: Open in System Player",
            Command::RemoveWorkspaceFolder => "Workspaces: Remove Folder from Workspace",
            Command::ShowRunDebug => "View: Show Run and Debug",
            Command::ShowRemote => "View: Show Remote",
            Command::ShowExtensions => "View: Show Extensions",
            Command::ShowTesting => "View: Show Testing",
            Command::RunTestAtCursor => "Testing: Run Test at Cursor",
            Command::DebugTestAtCursor => "Testing: Debug Test at Cursor",
            Command::ToggleSideBar => "View: Toggle Primary Side Bar",
            Command::ToggleSecondarySideBar => "View: Toggle Secondary Side Bar",
            Command::ToggleZenMode => "View: Toggle Zen Mode",
            Command::ToggleTerminal => "View: Toggle Terminal",
            Command::ToggleMinimap => "View: Toggle Minimap",
            Command::NewTerminal => "Terminal: Create New Terminal",
            Command::StartDebugging => "Debug: Start Debugging",
            Command::SelectDebugConfig => "Debug: Select and Start Debugging",
            Command::StopDebugging => "Debug: Stop Debugging",
            Command::PauseDebugging => "Debug: Pause",
            Command::RestartDebugging => "Debug: Restart",
            Command::ToggleBreakpoint => "Debug: Toggle Breakpoint",
            Command::EditBreakpointCondition => "Debug: Add Conditional Breakpoint",
            Command::EditLogpoint => "Debug: Add Logpoint",
            Command::ShowIncomingCalls => "Calls: Show Incoming Calls",
            Command::ShowOutgoingCalls => "Calls: Show Outgoing Calls",
            Command::StepOver => "Debug: Step Over",
            Command::ToggleRaisedExceptions => "Debug: Toggle Break on Raised Exceptions",
            Command::AttachPythonProcess => "Debug: Attach to Python Process",
            Command::RunTask => "Tasks: Run Task",
            Command::RunBuildTask => "Tasks: Run Build Task",
            Command::RerunLastTask => "Tasks: Rerun Last Task",
            Command::ColorTheme => "Preferences: Color Theme",
            Command::KeyboardShortcuts => "Help: Keyboard Shortcuts Reference",
            Command::OpenSettings => "Preferences: Open Settings",
            Command::OpenSettingsJson => "Preferences: Open Settings (JSON)",
            Command::OpenWorkspaceSettingsJson => "Preferences: Open Workspace Settings (JSON)",
            Command::OpenWorkspaceSettingsLocalJson => {
                "Preferences: Open Workspace Settings — Local (JSON)"
            }
            Command::OpenKeybindingsJson => "Preferences: Open Keyboard Shortcuts (JSON)",
            Command::ConfigureSnippets => "Preferences: Configure User Snippets",
            Command::OpenTriggersJson => "Preferences: Open Terminal Triggers (JSON)",
            Command::OpenMatchersJson => "Preferences: Open Problem Matchers (JSON)",
            Command::ToggleTerminalTimestamps => "Terminal: Toggle Timestamps",
            Command::SearchFromTerminal => "Terminal: Search & Replace from Last grep/rg",
            Command::SessionParticipants => "Session: Participants",
            Command::CollabCancelStream => "Collab: Cancel AI Stream",
            Command::AskNavigator => "Navigator: Ask About Line or Selection",
            Command::YieldToNavigator => "Navigator: Yield the Turn",
            Command::ToggleNavigator => "Navigator: Activate or Deactivate",
            Command::ClearNavigatorNotes => "Navigator: Clear Comments",
            Command::ToggleProactiveNavigator => "Navigator: Toggle Proactive Comments",
            Command::NextComment => "Navigator: Next Comment",
            Command::IgnoreComment => "Navigator: Ignore Comment",
        }
    }

    /// The default key-binding hint shown right-aligned on the row, or an
    /// empty string for palette-only commands. The label uses the macOS chord
    /// names; on Linux/Android the command modifier is `Ctrl`.
    pub fn keybinding_hint(self) -> &'static str {
        match self {
            Command::MoveLineUp => "Alt+↑",
            Command::MoveLineDown => "Alt+↓",
            Command::JoinLines => "Cmd+Opt+Shift+J",
            Command::DeleteLine => "Cmd+Shift+K",
            Command::TransformUpper => "Cmd+Opt+Shift+U",
            Command::TransformLower => "Cmd+Opt+Shift+L",
            Command::TransformTitle => "Cmd+Opt+Shift+C",
            Command::SortLinesAscending => "Cmd+Opt+Shift+A",
            Command::SortLinesDescending => "Cmd+Opt+Shift+D",
            Command::TrimTrailingWhitespace => "Cmd+Opt+Shift+W",
            Command::ToggleLineComment => "Cmd+/",
            Command::ToggleBlockComment => "Shift+Alt+A",
            Command::ToggleWordWrap => "Alt+Z",
            Command::ExpandSelection => "Shift+Alt+\u{2192}",
            Command::ShrinkSelection => "Shift+Alt+\u{2190}",
            Command::ReplaceInFile => "Cmd+Opt+F",
            Command::ToggleAutoSave => "",
            Command::ToggleAutoSaveOnFocusChange => "",
            Command::ToggleInlineBlame => "",
            Command::ToggleIndentGuides => "",
            Command::ToggleBracketColors => "",
            Command::ToggleRenderWhitespace => "",
            Command::ToggleInlineValues => "",
            Command::ToggleInlayHints => "",
            Command::ToggleMarkdownPreview => "Cmd+Shift+V",
            Command::RestoreSnapshot => "",
            Command::MergeAcceptCurrent => "Cmd+.",
            Command::MergeAcceptIncoming => "Cmd+.",
            Command::MergeAcceptBoth => "Cmd+.",
            Command::MergeAcceptAllCurrent => "",
            Command::MergeAcceptAllIncoming => "",
            Command::MergeNextConflict => "F7",
            Command::MergePrevConflict => "Shift+F7",
            Command::MergeComplete => "",
            Command::DebugAddWatch => "",
            Command::DebugClearWatch => "",
            Command::PeekDefinition => "Alt+F12",
            Command::ClearBuildDiagnostics => "",
            Command::StageHunk => "S in diff",
            Command::UnstageHunk => "U in diff",
            Command::RevertHunk => "R in diff",
            Command::AddCursorAbove => "Cmd+Opt+↑",
            Command::AddCursorBelow => "Cmd+Opt+↓",
            Command::AddSelectionToNextMatch => "Cmd+D",
            Command::JumpToBracket => "Cmd+Shift+\\",
            Command::SelectToBracket => "Cmd+Opt+\\",
            Command::TransposeCharacters => "Ctrl+T",
            Command::IndentationToSpaces => "Cmd+Opt+Shift+S",
            Command::IndentationToTabs => "Cmd+Opt+Shift+T",
            Command::TrimFinalNewlines => "Cmd+Opt+Shift+N",
            Command::FormatDocument => "Cmd+Opt+Shift+F",
            Command::ChangeColorPresentation => "",
            Command::ToggleFormatOnSave => "Cmd+K F",
            Command::QuickFix => "Cmd+.",
            Command::ToggleFold => "Cmd+K Cmd+L",
            Command::FoldAll => "Cmd+K Cmd+0",
            Command::UnfoldAll => "Cmd+K Cmd+J",
            Command::FoldAllComments => "Cmd+K Cmd+/",
            Command::FoldAllRegions => "Cmd+K Cmd+8",
            Command::UnfoldAllRegions => "Cmd+K Cmd+9",
            Command::SaveFile => "Cmd+S",
            Command::Undo => "Cmd+Z",
            Command::Redo => "Shift+Cmd+Z",
            Command::CloseEditor => "Cmd+W",
            Command::ReopenClosedEditor => "Cmd+K Shift+W",
            Command::SplitEditor => "Cmd+\\",
            Command::QuickOpen => "Cmd+P",
            Command::GoToSymbol => "Cmd+Shift+O",
            Command::GoToWorkspaceSymbol => "Cmd+P #",
            Command::NavigateBack => "Ctrl+-",
            Command::NavigateForward => "Ctrl+Shift+-",
            Command::GoToLastEditLocation => "Cmd+K Cmd+Q",
            Command::ToggleVimMode => "Cmd+E",
            Command::ShowExplorer => "Cmd+Shift+E",
            Command::ShowSearch => "Cmd+Shift+F",
            Command::ShowSourceControl => "Cmd+Shift+S",
            Command::AddWorkspaceFolder => "",
            Command::SaveWorkspaceAs => "",
            Command::OpenWorkspaceFromFile => "",
            Command::ReopenAsHex => "",
            Command::ReopenAsPreview => "",
            Command::ReopenAsText => "",
            Command::HexFindNext => "F3",
            Command::SheetInsertRowBelow => "",
            Command::SheetDeleteRow => "",
            Command::SheetInsertColRight => "",
            Command::SheetDeleteCol => "",
            Command::MediaOpenExternal => "",
            Command::RemoveWorkspaceFolder => "",
            Command::ShowRunDebug => "Cmd+Shift+D",
            Command::ShowRemote => "Cmd+Shift+R",
            Command::ShowExtensions => "Cmd+Shift+X",
            Command::ShowTesting => "Cmd+K B",
            Command::RunTestAtCursor => "Cmd+K Enter",
            Command::DebugTestAtCursor => "Cmd+K Shift+Enter",
            Command::ToggleSideBar => "Cmd+B",
            Command::ToggleSecondarySideBar => "Cmd+Opt+B",
            Command::ToggleZenMode => "Cmd+K Z",
            Command::ToggleTerminal => "Ctrl+J",
            Command::ToggleMinimap => "Cmd+Opt+M",
            Command::NewTerminal => "Cmd+T",
            Command::KeyboardShortcuts => "F1",
            Command::StartDebugging => "F5",
            Command::SelectDebugConfig => "",
            Command::StopDebugging => "Shift+F5",
            Command::PauseDebugging => "F6",
            Command::ToggleBreakpoint => "F9",
            Command::StepOver => "F10",
            Command::RestartDebugging => "Shift+Cmd+F5",
            Command::EditBreakpointCondition => "Shift+F9",
            Command::EditLogpoint => "Shift+Alt+F9",
            Command::ShowIncomingCalls => "Cmd+K H",
            Command::ShowOutgoingCalls => "Cmd+K Shift+H",
            Command::ToggleRaisedExceptions => "Alt+F9",
            Command::AttachPythonProcess => "Ctrl+F5",
            Command::RunTask => "",
            Command::RunBuildTask => "Cmd+Shift+B",
            Command::RerunLastTask => "",
            Command::ColorTheme => "Cmd+K Cmd+T",
            // Palette-only by default; the whole point of the keybindings.json
            // loader is that a user can bind these (the seeded template shows
            // Cmd+, -> open_settings as the example).
            Command::OpenSettings => "",
            Command::OpenSettingsJson => "",
            Command::OpenWorkspaceSettingsJson => "",
            Command::OpenWorkspaceSettingsLocalJson => "",
            Command::OpenKeybindingsJson => "",
            Command::ConfigureSnippets => "",
            Command::OpenTriggersJson => "",
            Command::OpenMatchersJson => "",
            Command::ToggleTerminalTimestamps => "",
            Command::SearchFromTerminal => "",
            Command::SessionParticipants => "Cmd+K A",
            Command::CollabCancelStream => "Cmd+K X",
            Command::AskNavigator => "Cmd+K Q",
            Command::YieldToNavigator => "Cmd+K Y",
            Command::ToggleNavigator => "",
            Command::ClearNavigatorNotes => "",
            Command::ToggleProactiveNavigator => "",
            Command::NextComment => "F4",
            Command::IgnoreComment => "Shift+F4",
        }
        // No catch-all: every Command must carry an accelerator (croft tenet),
        // so adding a variant fails to compile until its hint is supplied.
    }

    /// The stable snake_case identifier used in `keybindings.json`. This is the
    /// contract a user's config binds against, so the strings must never drift
    /// once shipped (unlike `title`, which is display-only). No catch-all: a new
    /// variant fails to compile until it declares an id.
    pub fn id(self) -> &'static str {
        match self {
            Command::MoveLineUp => "move_line_up",
            Command::MoveLineDown => "move_line_down",
            Command::ToggleLineComment => "toggle_line_comment",
            Command::ToggleBlockComment => "toggle_block_comment",
            Command::JoinLines => "join_lines",
            Command::DeleteLine => "delete_line",
            Command::TransformUpper => "transform_upper",
            Command::TransformLower => "transform_lower",
            Command::TransformTitle => "transform_title",
            Command::SortLinesAscending => "sort_lines_ascending",
            Command::SortLinesDescending => "sort_lines_descending",
            Command::TrimTrailingWhitespace => "trim_trailing_whitespace",
            Command::ToggleWordWrap => "toggle_word_wrap",
            Command::ExpandSelection => "expand_selection",
            Command::ShrinkSelection => "shrink_selection",
            Command::ReplaceInFile => "replace_in_file",
            Command::MergeAcceptCurrent => "merge_accept_current",
            Command::MergeAcceptIncoming => "merge_accept_incoming",
            Command::MergeAcceptBoth => "merge_accept_both",
            Command::MergeAcceptAllCurrent => "merge_accept_all_current",
            Command::MergeAcceptAllIncoming => "merge_accept_all_incoming",
            Command::MergeNextConflict => "merge_next_conflict",
            Command::MergePrevConflict => "merge_prev_conflict",
            Command::MergeComplete => "merge_complete",
            Command::DebugAddWatch => "debug_add_watch",
            Command::DebugClearWatch => "debug_clear_watch",
            Command::PeekDefinition => "peek_definition",
            Command::ClearBuildDiagnostics => "clear_build_diagnostics",
            Command::StageHunk => "stage_hunk",
            Command::UnstageHunk => "unstage_hunk",
            Command::RevertHunk => "revert_hunk",
            Command::AddCursorAbove => "add_cursor_above",
            Command::AddCursorBelow => "add_cursor_below",
            Command::AddSelectionToNextMatch => "add_selection_to_next_match",
            Command::JumpToBracket => "jump_to_bracket",
            Command::SelectToBracket => "select_to_bracket",
            Command::TransposeCharacters => "transpose_characters",
            Command::IndentationToSpaces => "indentation_to_spaces",
            Command::IndentationToTabs => "indentation_to_tabs",
            Command::TrimFinalNewlines => "trim_final_newlines",
            Command::FormatDocument => "format_document",
            Command::ChangeColorPresentation => "change_color_presentation",
            Command::ToggleFormatOnSave => "toggle_format_on_save",
            Command::QuickFix => "quick_fix",
            Command::ToggleFold => "toggle_fold",
            Command::FoldAll => "fold_all",
            Command::UnfoldAll => "unfold_all",
            Command::FoldAllComments => "fold_all_comments",
            Command::FoldAllRegions => "fold_all_regions",
            Command::UnfoldAllRegions => "unfold_all_regions",
            Command::SaveFile => "save_file",
            Command::Undo => "undo",
            Command::Redo => "redo",
            Command::ToggleAutoSave => "toggle_auto_save",
            Command::ToggleAutoSaveOnFocusChange => "toggle_auto_save_on_focus_change",
            Command::ToggleInlineBlame => "toggle_inline_blame",
            Command::ToggleIndentGuides => "toggle_indent_guides",
            Command::ToggleBracketColors => "toggle_bracket_colors",
            Command::ToggleRenderWhitespace => "toggle_render_whitespace",
            Command::ToggleInlineValues => "toggle_inline_values",
            Command::ToggleInlayHints => "toggle_inlay_hints",
            Command::ToggleMarkdownPreview => "toggle_markdown_preview",
            Command::RestoreSnapshot => "restore_snapshot",
            Command::CloseEditor => "close_editor",
            Command::ReopenClosedEditor => "reopen_closed_editor",
            Command::SplitEditor => "split_editor",
            Command::QuickOpen => "quick_open",
            Command::GoToSymbol => "go_to_symbol",
            Command::GoToWorkspaceSymbol => "go_to_workspace_symbol",
            Command::NavigateBack => "navigate_back",
            Command::NavigateForward => "navigate_forward",
            Command::GoToLastEditLocation => "go_to_last_edit_location",
            Command::ToggleVimMode => "toggle_vim_mode",
            Command::ShowExplorer => "show_explorer",
            Command::ShowSearch => "show_search",
            Command::ShowSourceControl => "show_source_control",
            Command::AddWorkspaceFolder => "add_workspace_folder",
            Command::SaveWorkspaceAs => "save_workspace_as",
            Command::OpenWorkspaceFromFile => "open_workspace_from_file",
            Command::ReopenAsHex => "reopen_as_hex",
            Command::ReopenAsPreview => "reopen_as_preview",
            Command::ReopenAsText => "reopen_as_text",
            Command::HexFindNext => "hex_find_next",
            Command::SheetInsertRowBelow => "sheet_insert_row_below",
            Command::SheetDeleteRow => "sheet_delete_row",
            Command::SheetInsertColRight => "sheet_insert_col_right",
            Command::SheetDeleteCol => "sheet_delete_col",
            Command::MediaOpenExternal => "media_open_external",
            Command::RemoveWorkspaceFolder => "remove_workspace_folder",
            Command::ShowRunDebug => "show_run_debug",
            Command::ShowRemote => "show_remote",
            Command::ShowExtensions => "show_extensions",
            Command::ShowTesting => "show_testing",
            Command::RunTestAtCursor => "run_test_at_cursor",
            Command::DebugTestAtCursor => "debug_test_at_cursor",
            Command::ToggleSideBar => "toggle_side_bar",
            Command::ToggleSecondarySideBar => "toggle_secondary_side_bar",
            Command::ToggleZenMode => "toggle_zen_mode",
            Command::ToggleTerminal => "toggle_terminal",
            Command::ToggleMinimap => "toggle_minimap",
            Command::NewTerminal => "new_terminal",
            Command::StartDebugging => "start_debugging",
            Command::SelectDebugConfig => "select_debug_config",
            Command::StopDebugging => "stop_debugging",
            Command::PauseDebugging => "pause_debugging",
            Command::RestartDebugging => "restart_debugging",
            Command::ToggleBreakpoint => "toggle_breakpoint",
            Command::EditBreakpointCondition => "edit_breakpoint_condition",
            Command::EditLogpoint => "edit_logpoint",
            Command::ShowIncomingCalls => "show_incoming_calls",
            Command::ShowOutgoingCalls => "show_outgoing_calls",
            Command::StepOver => "step_over",
            Command::ToggleRaisedExceptions => "toggle_raised_exceptions",
            Command::AttachPythonProcess => "attach_python_process",
            Command::ColorTheme => "color_theme",
            Command::KeyboardShortcuts => "keyboard_shortcuts",
            Command::OpenSettings => "open_settings",
            Command::OpenSettingsJson => "open_settings_json",
            Command::OpenWorkspaceSettingsJson => "open_workspace_settings_json",
            Command::OpenWorkspaceSettingsLocalJson => "open_workspace_settings_local_json",
            Command::OpenKeybindingsJson => "open_keybindings_json",
            Command::ConfigureSnippets => "configure_snippets",
            Command::OpenTriggersJson => "open_triggers_json",
            Command::OpenMatchersJson => "open_matchers_json",
            Command::ToggleTerminalTimestamps => "toggle_terminal_timestamps",
            Command::SearchFromTerminal => "search_from_terminal",
            Command::SessionParticipants => "session_participants",
            Command::CollabCancelStream => "collab_cancel_stream",
            Command::AskNavigator => "navigator_ask",
            Command::YieldToNavigator => "navigator_yield",
            Command::ToggleNavigator => "navigator_toggle",
            Command::ClearNavigatorNotes => "navigator_clear_notes",
            Command::ToggleProactiveNavigator => "navigator_toggle_proactive",
            Command::NextComment => "navigator_next_comment",
            Command::IgnoreComment => "navigator_ignore_comment",
            Command::RunTask => "run_task",
            Command::RunBuildTask => "run_build_task",
            Command::RerunLastTask => "rerun_last_task",
        }
    }

    /// Resolve a `keybindings.json` command id back to its [`Command`]. Returns
    /// `None` for an unknown id so a typo in the user's config is ignored rather
    /// than fatal.
    pub fn from_id(id: &str) -> Option<Command> {
        ALL_COMMANDS.iter().copied().find(|c| c.id() == id)
    }
}

/// A palette command contributed by an MCP sidecar extension. Carries its own
/// runtime `title` (so the built-in [`Command`] enum stays a closed, `Copy`,
/// `&'static`-titled set) plus the ids needed to dispatch and gate it. The app
/// injects these via [`CommandPalette::set_extension_commands`]; the widget
/// never reads manifests itself (it stays a pure projection, like the Extensions
/// panel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommand {
    pub ext_id: String,
    pub id: String,
    pub title: String,
}

/// One row in the palette: a built-in command or an extension-contributed one.
/// Built-ins keep their compile-time identity; extension rows carry owned data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteItem {
    Builtin(Command),
    Extension(ExtensionCommand),
}

impl PaletteItem {
    /// The label shown in the palette and matched against the query.
    pub fn title(&self) -> &str {
        match self {
            PaletteItem::Builtin(c) => c.title(),
            PaletteItem::Extension(e) => &e.title,
        }
    }

    /// The right-aligned keybinding hint. Extension commands have none (they are
    /// palette-only), so they show a blank hint.
    pub fn keybinding_hint(&self) -> &str {
        match self {
            PaletteItem::Builtin(c) => c.keybinding_hint(),
            PaletteItem::Extension(_) => "",
        }
    }
}

/// The palette's owned state: the query being typed and which filtered row is
/// selected. Mirrors `FileFinder` so the App can drive both the same way.
#[derive(Default)]
pub struct CommandPalette {
    pub query: String,
    pub cursor: usize,
    pub results: Vec<PaletteItem>,
    /// Extension-contributed commands injected by the app (empty until set), kept
    /// separate from the built-in registry and merged into `results` on each
    /// re-rank.
    pub extensions: Vec<ExtensionCommand>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    pub last_inner_height: u16,
}

impl CommandPalette {
    pub fn new() -> Self {
        let mut me = Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            extensions: Vec::new(),
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
        };
        me.refresh_results();
        me
    }

    /// Inject the extension-contributed commands to interleave into the list,
    /// then re-rank. Called by the app when opening the palette.
    pub fn set_extension_commands(&mut self, extensions: Vec<ExtensionCommand>) {
        self.extensions = extensions;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    fn char_count(&self) -> usize {
        self.query.chars().count()
    }

    fn byte_offset(&self, char_idx: usize) -> usize {
        self.query
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.query.len())
    }

    #[cfg(test)]
    pub fn set_query(&mut self, q: &str) {
        self.query = q.to_string();
        self.cursor = self.query.chars().count();
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn push_char(&mut self, c: char) {
        let at = self.byte_offset(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.query.remove(at);
        self.cursor -= 1;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.query.remove(at);
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor = self.char_count();
    }

    pub fn select_next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn selected_item(&self) -> Option<PaletteItem> {
        self.results.get(self.selected).cloned()
    }

    /// The result index at screen row `y`, if `y` lands on a visible row.
    /// The list body starts three rows below `last_rect.y` (top border, the
    /// query prompt, then the separator) and runs `last_inner_height` rows,
    /// so this stays in lock-step with [`render_command_palette`]. Used to
    /// map a mouse click to a result row.
    pub fn row_index_at(&self, y: u16) -> Option<usize> {
        let list_top = self.last_rect.y.saturating_add(3);
        if y < list_top || y - list_top >= self.last_inner_height {
            return None;
        }
        let idx = self.scroll + (y - list_top) as usize;
        (idx < self.results.len()).then_some(idx)
    }

    /// Re-rank the command list against the current query, over built-ins AND
    /// injected extension commands. An empty query shows every command in
    /// declaration order (built-ins first, then extensions); otherwise rows are
    /// kept only when their lower-cased title fuzzy-matches the needle, ranked
    /// by score (best first), ties broken by declaration order for stability.
    /// Built-ins occupy the low index range so they win ties against extensions.
    fn refresh_results(&mut self) {
        let all: Vec<PaletteItem> = ALL_COMMANDS
            .iter()
            .map(|&c| PaletteItem::Builtin(c))
            .chain(self.extensions.iter().cloned().map(PaletteItem::Extension))
            .collect();
        let needle = self.query.trim().to_lowercase();
        if needle.is_empty() {
            self.results = all;
            return;
        }
        let mut scored: Vec<(i32, usize, PaletteItem)> = all
            .into_iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let title_lower = item.title().to_lowercase();
                fuzzy_score(&needle, &title_lower, 0).map(|score| (score, idx, item))
            })
            .collect();
        // Higher score first; equal scores keep declaration order.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.results = scored.into_iter().map(|(_, _, item)| item).collect();
    }
}

pub fn render_command_palette(
    palette: &mut CommandPalette,
    area: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
    center: bool,
) {
    let width = area.width.saturating_mul(7) / 10;
    let width = width.clamp(40, 100.min(area.width));
    let height = area.height.saturating_mul(6) / 10;
    let height = height.clamp(10, area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    // Quick Input Position: Top anchors in the upper third (VS Code's
    // default); Center pins it to the vertical middle.
    let y = if center {
        area.y + (area.height.saturating_sub(height)) / 2
    } else {
        area.y + (area.height.saturating_sub(height)) / 4
    };
    let rect = Rect {
        x,
        y,
        width,
        height,
    };
    palette.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        " Command Palette — Esc to close, ↑/↓ to navigate, Enter to run ",
        Style::default()
            .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
        .title(title.clone())
        .style(Style::default().bg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f))));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    if theme.gradient() {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    let sel_bg = if theme.gradient() {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e))
    };

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let query_style = Style::default()
        .fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .fg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f)))
        .bg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        .add_modifier(Modifier::SLOW_BLINK);
    let cursor = palette.cursor.min(palette.query.chars().count());
    let before: String = palette.query.chars().take(cursor).collect();
    let at: String = palette.query.chars().skip(cursor).take(1).collect();
    let after: String = palette.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let prompt_line = Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(theme.ui(Color::Rgb(0x88, 0xc0, 0xd0))),
        ),
        Span::styled(before, query_style),
        Span::styled(caret_glyph, caret_style),
        Span::styled(after, query_style),
    ]);
    let prompt_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    Widget::render(Paragraph::new(prompt_line), prompt_rect, buf);

    let separator_rect = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    let sep_line = Line::from(Span::styled(
        "─".repeat(separator_rect.width as usize),
        Style::default().fg(theme.ui(Color::Rgb(0x3b, 0x42, 0x52))),
    ));
    Widget::render(Paragraph::new(sep_line), separator_rect, buf);

    let list_rect = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    palette.last_inner_height = list_rect.height;
    if list_rect.height == 0 {
        return;
    }

    let visible = list_rect.height as usize;
    let total = palette.results.len();
    if palette.selected >= palette.scroll + visible {
        palette.scroll = palette.selected + 1 - visible;
    }
    if palette.selected < palette.scroll {
        palette.scroll = palette.selected;
    }
    let end = (palette.scroll + visible).min(total);

    if total == 0 {
        let empty = Line::from(Span::styled(
            format!("  No commands match '{}'", palette.query),
            Style::default().fg(theme.ui(Color::Rgb(0x7a, 0x82, 0x90))),
        ));
        Widget::render(Paragraph::new(empty), list_rect, buf);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - palette.scroll);
    for (offset, cmd) in palette.results[palette.scroll..end].iter().enumerate() {
        let row_idx = palette.scroll + offset;
        let is_selected = row_idx == palette.selected;
        let row_style = if is_selected {
            Style::default().bg(sel_bg).fg(theme.ui(Color::White))
        } else {
            Style::default().fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        };
        let hint_style = if is_selected {
            Style::default()
                .bg(sel_bg)
                .fg(theme.ui(Color::Rgb(0xa0, 0xb4, 0xd8)))
        } else {
            Style::default().fg(theme.ui(Color::Rgb(0x8e, 0x95, 0xa4)))
        };
        let prefix = if is_selected { "> " } else { "  " };
        let title = cmd.title();
        let hint = cmd.keybinding_hint();
        // Right-align the keybinding hint: pad between the title and the hint
        // so the chord sits at the row's right edge, like VS Code.
        let used = 2 + title.chars().count() + hint.chars().count();
        let pad = (list_rect.width as usize).saturating_sub(used).max(1);
        let spans: Vec<Span<'static>> = vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(title.to_string(), row_style),
            Span::styled(" ".repeat(pad), row_style),
            Span::styled(hint.to_string(), hint_style),
        ];
        lines.push(Line::from(spans));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin(c: Command) -> PaletteItem {
        PaletteItem::Builtin(c)
    }

    #[test]
    fn empty_query_lists_every_command() {
        let palette = CommandPalette::new();
        assert_eq!(palette.results.len(), ALL_COMMANDS.len());
        assert_eq!(palette.results.first(), Some(&builtin(Command::MoveLineUp)));
    }

    #[test]
    fn query_filters_by_fuzzy_title() {
        let mut palette = CommandPalette::new();
        palette.set_query("comment");
        assert!(
            palette
                .results
                .contains(&builtin(Command::ToggleLineComment))
        );
        assert!(
            palette
                .results
                .contains(&builtin(Command::ToggleBlockComment))
        );
        assert!(!palette.results.contains(&builtin(Command::SaveFile)));
    }

    #[test]
    fn query_matches_subsequence() {
        let mut palette = CommandPalette::new();
        palette.set_query("sortasc");
        assert_eq!(
            palette.results.first(),
            Some(&builtin(Command::SortLinesAscending))
        );
    }

    #[test]
    fn format_document_is_reachable_and_shows_its_chord() {
        let mut palette = CommandPalette::new();
        palette.set_query("format document");
        assert_eq!(
            palette.results.first(),
            Some(&builtin(Command::FormatDocument))
        );
        assert_eq!(Command::FormatDocument.keybinding_hint(), "Cmd+Opt+Shift+F");
    }

    #[test]
    fn quick_fix_is_reachable_and_shows_its_chord() {
        let mut palette = CommandPalette::new();
        palette.set_query("quick fix");
        assert_eq!(palette.results.first(), Some(&builtin(Command::QuickFix)));
        assert_eq!(Command::QuickFix.keybinding_hint(), "Cmd+.");
    }

    #[test]
    fn no_match_yields_empty_results() {
        let mut palette = CommandPalette::new();
        palette.set_query("zzzznotacommand");
        assert!(palette.results.is_empty());
        assert_eq!(palette.selected_item(), None);
    }

    #[test]
    fn injected_extension_commands_appear_and_fuzzy_match() {
        let mut palette = CommandPalette::new();
        palette.set_extension_commands(vec![ExtensionCommand {
            ext_id: "mcp-fetch".into(),
            id: "fetch.url".into(),
            title: "Fetch: URL to Markdown".into(),
        }]);
        // Listed after the built-ins on an empty query.
        assert_eq!(palette.results.len(), ALL_COMMANDS.len() + 1);
        // Fuzzy-matches its title and dispatches as an extension item.
        palette.set_query("fetch url");
        match palette.results.first() {
            Some(PaletteItem::Extension(e)) => assert_eq!(e.id, "fetch.url"),
            other => panic!("expected the extension command first, got {other:?}"),
        }
    }

    #[test]
    fn selection_walks_and_clamps() {
        let mut palette = CommandPalette::new();
        palette.set_query("");
        assert_eq!(palette.selected, 0);
        palette.select_prev();
        assert_eq!(palette.selected, 0, "clamps at top");
        palette.select_next();
        assert_eq!(palette.selected, 1);
        assert_eq!(
            palette.selected_item(),
            Some(builtin(Command::MoveLineDown))
        );
    }

    #[test]
    fn typing_resets_selection_to_top() {
        let mut palette = CommandPalette::new();
        palette.select_next();
        palette.select_next();
        palette.push_char('s');
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn every_command_has_a_nonempty_title() {
        for cmd in ALL_COMMANDS {
            assert!(!cmd.title().is_empty(), "{cmd:?} has empty title");
        }
    }
}
