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
    KillToEndOfLine,
    TransformUpper,
    TransformLower,
    TransformTitle,
    SortLinesAscending,
    SortLinesDescending,
    IncrementNumber,
    DecrementNumber,
    TrimTrailingWhitespace,
    EmmetExpandAbbreviation,
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
    GoToNextChange,
    GoToPrevChange,
    MergeComplete,
    MergeOpenEditor,
    MergeToggleBase,
    MergeAcceptBothReverse,
    MergeIgnore,
    DebugAddWatch,
    DebugClearWatch,
    PeekDefinition,
    PeekReferences,
    /// Go to Implementations (#843): Cmd+F12, which has no `Ctrl` form
    /// (`Ctrl+F12` is Go to Type Definition).
    GoToImplementations,
    MouseAddCursorAtClick,
    MouseGoToDefinitionAtClick,
    MouseOpenLinkAtClick,
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
    ToggleOverviewRuler,
    ToggleBookmark,
    NextBookmark,
    PreviousBookmark,
    ClearBookmarks,
    FormatDocument,
    FormatSelection,
    ChangeColorPresentation,
    ToggleFormatOnType,
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
    SaveAs,
    /// File: Save All (#852): every dirty tab, in every split.
    SaveAll,
    /// File: New File… / New Folder… (#852): the Explorer's create prompt
    /// from any pane, beside the active file or in the Explorer selection.
    NewFile,
    NewFolder,
    Undo,
    Redo,
    SelectAll,
    ToggleAutoSave,
    ToggleAutoSaveOnFocusChange,
    ToggleInlineBlame,
    ToggleLiveRun,
    ToggleProvenance,
    DiffToggleGroupBySeat,
    ToggleIndentGuides,
    ToggleBracketColors,
    ToggleRenderWhitespace,
    ToggleInlineValues,
    ToggleInlayHints,
    ToggleMarkdownPreview,
    /// Markdown: Run Code Block at Cursor (#843): Cmd+Enter's run of the
    /// fence under the caret, from any terminal.
    RunCodeBlockAtCursor,
    RestoreSnapshot,
    CloseEditor,
    ReopenClosedEditor,
    /// Explorer: Jump to Directory (zoxide) (#843): the Explorer's Cmd+Z,
    /// which has no `Ctrl` form.
    ZoxideJump,
    /// The editor tab menu's close / pin / keep rows as commands (#852),
    /// VS Code's names, each on the active tab of the focused group.
    CloseOtherEditors,
    CloseEditorsToTheRight,
    CloseSavedEditors,
    CloseAllEditors,
    /// View: Pin Editor / Unpin Editor. The palette offers only the one
    /// that applies to the active tab, as VS Code does
    /// (`CommandPalette::set_hidden`); both stay bindable.
    PinEditor,
    UnpinEditor,
    KeepEditor,
    SplitEditor,
    /// View: Focus Left / Right Editor Group (#843): Cmd+Opt+Left / Right,
    /// which have no `Ctrl` form off macOS.
    FocusLeftEditorGroup,
    FocusRightEditorGroup,
    QuickOpen,
    TakeTheTour,
    SarifNextResult,
    SarifPreviousResult,
    SarifOpenCodeScanning,
    /// SARIF: Load Code Scanning Results (#577): the current branch's
    /// newest analyses per tool, at the nearest scanned commit.
    SarifLoadCodeScanning,
    GoToSymbol,
    GoToWorkspaceSymbol,
    /// VS Code "Go Back" (Ctrl+-): return to the location before the last
    /// navigation jump (Go to Definition, a reference pick, a symbol jump).
    NavigateBack,
    NavigateForward,
    GoToLastEditLocation,
    ToggleVimMode,
    MacroStartStopRecording,
    MacroReplayLast,
    MacroReplayTimes,
    // --- View / navigation ---
    ShowExplorer,
    ShowSearch,
    /// Search: Replace in Files (#860): the Search side bar with its Replace
    /// row expanded and focused.
    ReplaceInFiles,
    /// The Search side bar's `Aa` / `ab` / `.*` toggles and its `...`
    /// include / exclude row, from the keyboard (#860).
    SearchToggleMatchCase,
    SearchToggleWholeWord,
    SearchToggleRegex,
    SearchToggleDetails,
    ShowSourceControl,
    AddWorkspaceFolder,
    RemoveWorkspaceFolder,
    SaveWorkspaceAs,
    OpenWorkspaceFromFile,
    ReopenAsHex,
    ReopenAsPreview,
    ReopenAsText,
    NotebookRunAll,
    NotebookInterrupt,
    NotebookRestart,
    TestingRunWithCoverage,
    CoverageClear,
    TestingInstallCoverageTool,
    TestingShowCoverageReport,
    DeveloperShowMemoryUsage,
    RunTestAtCursorWithCoverage,
    TestingToggleWatchAll,
    TestingGoToFirstFailure,
    HexFindNext,
    SheetInsertRowBelow,
    SheetDeleteRow,
    SheetInsertColRight,
    SheetDeleteCol,
    MediaOpenExternal,
    ShowRunDebug,
    ShowRemote,
    ShowExtensions,
    CompareExtensionsWithVscode,
    ShowTesting,
    ShowCodeQL,
    CodeqlRunQuery,
    CodeqlRunQuerySuite,
    CodeqlRemoveDatabase,
    CodeqlRenameDatabase,
    CodeqlSortDatabases,
    CodeqlRevealDatabase,
    CodeqlUpgradeDatabase,
    CodeqlClearCache,
    CodeqlTrimCache,
    CodeqlTrimCacheToOverlay,
    CodeqlAddDatabaseSource,
    CodeqlDeleteUnusedDatabases,
    CodeqlRemoveHistory,
    CodeqlRenameHistory,
    CodeqlSortHistory,
    CodeqlViewQuery,
    CodeqlCreateQuery,
    CodeqlRunPack,
    CodeqlRunQueryOnDatabases,
    CodeqlCancelQueue,
    CodeqlCopyVersion,
    CodeqlDownloadCli,
    CodeqlCheckCliUpdates,
    CodeqlInstallPackDependencies,
    CodeqlDownloadPacks,
    CodeqlRunPublishedPack,
    CodeqlQuickQuery,
    CodeqlCompareResults,
    CodeqlShowEvaluatorLog,
    CodeqlShowEvaluatorLogSummary,
    CodeqlShowEvaluatorLogViewer,
    CodeqlShowQueryLog,
    CodeqlComparePerformance,
    CodeqlPreviewQueryHelp,
    CodeqlExportResults,
    CodeqlSetUpController,
    CodeqlAddVariantRepo,
    CodeqlAddVariantList,
    CodeqlAddVariantOwner,
    CodeqlVariantCodeSearch,
    CodeqlOpenVariantConfig,
    CodeqlOpenVariantOnGithub,
    CodeqlRunVariantAnalysis,
    CodeqlOpenVariantResults,
    CodeqlExportVariantResults,
    CodeqlCopyVariantRepoList,
    CodeqlViewVariantLogs,
    CodeqlRunTests,
    CodeqlDebugQuery,
    CodeqlDebugSelection,
    CodeqlResultsUp,
    CodeqlResultsDown,
    CodeqlResultsLeft,
    CodeqlResultsRight,
    SheetSortByColumn,
    CodeqlQuickEval,
    CodeqlQuickEvalCount,
    CodeqlViewAst,
    CodeqlOpenModelEditor,
    CodeqlModelEndpoint,
    CodeqlOpenReferencedFile,
    CodeqlViewAlertsCsv,
    CodeqlViewAlertsSarif,
    CodeqlViewResultsCsv,
    CodeqlShowResultSet,
    CodeqlViewCfg,
    CodeqlRunAllQueries,
    CodeqlRunSelectedQueries,
    CodeqlCancelRunningQuery,
    CodeqlAcceptTestOutput,
    CodeqlFocusSideBar,
    CodeqlOpenResultsDirectory,
    RunTestAtCursor,
    DebugTestAtCursor,
    ToggleSideBar,
    ToggleAutoHideSideBar,
    ToggleSecondarySideBar,
    ToggleZenMode,
    ToggleTerminal,
    /// The Customize Layout popup's rows as commands (#852), VS Code's
    /// names: the bars' visibility, the side bar's side, and pickers for
    /// the panel alignment and the quick input position. `LAYOUT_COMMANDS`
    /// lists them with the popup's other rows so "layout" finds them all.
    ToggleActivityBar,
    ToggleStatusBar,
    ToggleSideBarPosition,
    SetPanelAlignment,
    SetQuickInputPosition,
    /// View: Customize Layout… (#852): the popup itself.
    CustomizeLayout,
    /// Terminal: Focus Terminal (#852): the TERMINAL tab, shown and focused.
    /// With the `Show*` commands below, one command per bottom-panel tab
    /// (`BottomPanelTab::show_command` pairs them).
    FocusTerminal,
    ShowProblems,
    ShowOutput,
    /// Output: Select Channel… (#852): the OUTPUT dropdown as a picker.
    OutputSelectChannel,
    ShowPorts,
    ShowCaptures,
    ToggleMinimap,
    /// Run the whole-project checker and put its findings in PROBLEMS (#256).
    ProblemsCheckProject,
    /// Cycle whether that check may run WITHOUT being asked (#256).
    ProblemsToggleProjectAuto,
    ProblemsToggleScope,
    DiffToggleIgnoreWhitespace,
    NewTerminal,
    /// Terminal: Focus Next / Previous Terminal (#843): Cmd+] / Cmd+[, which
    /// have no `Ctrl` form (`Ctrl+[` is `Esc`).
    FocusNextTerminal,
    FocusPreviousTerminal,
    // --- Run / debug ---
    StartDebugging,
    SelectDebugConfig,
    AddDebugConfig,
    StopDebugging,
    PauseDebugging,
    SwitchDebugSession,
    RestartDebugging,
    ToggleBreakpoint,
    EditBreakpointCondition,
    EditLogpoint,
    ShowIncomingCalls,
    ShowOutgoingCalls,
    ShowSupertypes,
    ShowSubtypes,
    ToggleCodeLens,
    RunCodeLens,
    DebugAddHitCountBreakpoint,
    DebugAddFunctionBreakpoint,
    DebugRemoveFunctionBreakpoints,
    DebugRunToCursor,
    DebugBreakOnValueChange,
    DebugRemoveDataBreakpoints,
    OpenSearchEditor,
    RerunSearchEditor,
    RebaseAbort,
    ToggleTerminalSuggestions,
    ToggleScreenReader,
    OpenKeyboardShortcuts,
    SwitchProfile,
    ToggleInlineSuggestions,
    StepOver,
    ToggleRaisedExceptions,
    AttachPythonProcess,
    ColorTheme,
    KeyboardShortcuts,
    UpdateCroft,
    OpenSettings,
    OpenSettingsEditor,
    OpenSettingsJson,
    OpenWorkspaceSettingsJson,
    OpenWorkspaceSettingsLocalJson,
    OpenKeybindingsJson,
    ConfigureSnippets,
    OpenTriggersJson,
    OpenAgentsJson,
    OpenMatchersJson,
    ToggleTerminalTimestamps,
    ToggleLogHighlight,
    CollapseTerminalPane,
    RestoreTerminalPanes,
    ToggleSecretRedaction,
    RevealRedactedSecrets,
    RunTask,
    RunBuildTask,
    RerunLastTask,
    SearchFromTerminal,
    /// Multiplayer: list who is attached to this persistent session and
    /// grant/revoke write control or disconnect them (docs/MULTIPLAYER.md).
    SessionParticipants,
    /// Detach the client that invoked it from the persistent session; the
    /// session keeps running (#679).
    SessionDetach,
    /// Cancel the AI pilot's token stream into a shared file; the pilot
    /// reverts the streamed text (`croft pair`, docs/MULTIPLAYER.md).
    CollabCancelStream,
    /// Ask the resident navigator about the caret line or selection (opens
    /// the instruction box; the navigator may edit on the resulting turn).
    AskNavigator,
    AskNavigatorAboutCapture,
    /// Re-root the workspace onto the host the active pane is ssh'd into (#364).
    OpenWorkspaceOnSshHost,
    /// Step the editor back through the branch's commits (#371).
    ScrubHistory,
    DebugInstallDelve,
    /// Remote: Sync Config Now (#262): push the syncable config to a host.
    RemoteSyncConfigNow,
    StopAutoApprove,
    ScrubOpenHere,
    ScrubDiffToWorkingTree,
    /// Run one command across several ssh hosts and compare (#363).
    FleetRun,
    /// Open the symbol under the cursor as its own tab (#369).
    OpenAsSymbolTab,
    /// Load this PR's review threads as comment boxes (#366).
    LoadReviewThreads,
    ReviewAddComment,
    ReviewSubmit,
    ReviewToggleResolved,
    ReviewDiscardPending,
    ExportCommentsToPr,
    NoteAdd,
    NoteDelete,
    /// Start or stop recording the active terminal as an asciicast (#356).
    ToggleSessionRecording,
    /// Create a git worktree lane for an agent to work in (#348).
    NewWorktreeLane,
    ReviewPullRequest,
    /// Remove the current worktree lane, refusing when it is dirty (#348).
    CloseWorktreeLane,
    /// Point the COMMITS graph at what the current lane added (#348).
    DiffWorktreeLane,
    /// Mark every file every agent changed as reviewed (#345).
    MarkAgentLaneReviewed,
    /// Summarise each agent's review queue (#345).
    ShowAgentLane,
    /// Show the Explorer's AGENT LANE section, open (#345).
    OpenAgentLaneSection,
    /// Diff the active file against the snapshot it was last reviewed
    /// against (#345).
    DiffAgentFileSinceReview,
    /// Pick any file an agent changed and open its diff since review, from
    /// the keyboard (#345).
    PickAgentLaneFile,
    /// Mark the active file reviewed in every agent lane holding it (#345).
    MarkAgentFileReviewed,
    /// Have the navigator fix the diagnostic under the caret as a streamed,
    /// cancellable edit (#374).
    FixProblemWithNavigator,
    /// Send the `.http` request under the caret; the response opens as a
    /// tab (#370).
    SendHttpRequest,
    /// Copy the `.http` request under the caret as a curl command (#370).
    CopyHttpRequestAsCurl,
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
    Command::KillToEndOfLine,
    Command::TransformUpper,
    Command::TransformLower,
    Command::TransformTitle,
    Command::SortLinesAscending,
    Command::SortLinesDescending,
    Command::IncrementNumber,
    Command::DecrementNumber,
    Command::TrimTrailingWhitespace,
    Command::EmmetExpandAbbreviation,
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
    Command::GoToNextChange,
    Command::GoToPrevChange,
    Command::MergeComplete,
    Command::MergeOpenEditor,
    Command::MergeToggleBase,
    Command::MergeAcceptBothReverse,
    Command::MergeIgnore,
    Command::DebugAddWatch,
    Command::DebugClearWatch,
    Command::PeekDefinition,
    Command::PeekReferences,
    Command::GoToImplementations,
    Command::MouseAddCursorAtClick,
    Command::MouseGoToDefinitionAtClick,
    Command::MouseOpenLinkAtClick,
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
    Command::ToggleOverviewRuler,
    Command::ToggleBookmark,
    Command::NextBookmark,
    Command::PreviousBookmark,
    Command::ClearBookmarks,
    Command::FormatDocument,
    Command::FormatSelection,
    Command::ChangeColorPresentation,
    Command::ToggleFormatOnType,
    Command::ToggleFormatOnSave,
    Command::QuickFix,
    Command::ToggleFold,
    Command::FoldAll,
    Command::UnfoldAll,
    Command::FoldAllComments,
    Command::FoldAllRegions,
    Command::UnfoldAllRegions,
    Command::SaveFile,
    Command::SaveAs,
    Command::SaveAll,
    Command::NewFile,
    Command::NewFolder,
    Command::Undo,
    Command::Redo,
    Command::SelectAll,
    Command::ToggleAutoSave,
    Command::ToggleAutoSaveOnFocusChange,
    Command::ToggleInlineBlame,
    Command::ToggleLiveRun,
    Command::ToggleProvenance,
    Command::DiffToggleGroupBySeat,
    Command::ToggleIndentGuides,
    Command::ToggleBracketColors,
    Command::ToggleRenderWhitespace,
    Command::ToggleInlineValues,
    Command::ToggleInlayHints,
    Command::ToggleMarkdownPreview,
    Command::RunCodeBlockAtCursor,
    Command::RestoreSnapshot,
    Command::CloseEditor,
    Command::ReopenClosedEditor,
    Command::ZoxideJump,
    Command::CloseOtherEditors,
    Command::CloseEditorsToTheRight,
    Command::CloseSavedEditors,
    Command::CloseAllEditors,
    Command::PinEditor,
    Command::UnpinEditor,
    Command::KeepEditor,
    Command::SplitEditor,
    Command::FocusLeftEditorGroup,
    Command::FocusRightEditorGroup,
    Command::QuickOpen,
    Command::TakeTheTour,
    Command::SarifNextResult,
    Command::SarifPreviousResult,
    Command::SarifOpenCodeScanning,
    Command::SarifLoadCodeScanning,
    Command::GoToSymbol,
    Command::GoToWorkspaceSymbol,
    Command::NavigateBack,
    Command::NavigateForward,
    Command::GoToLastEditLocation,
    Command::ToggleVimMode,
    Command::MacroStartStopRecording,
    Command::MacroReplayLast,
    Command::MacroReplayTimes,
    Command::ShowExplorer,
    Command::ShowSearch,
    Command::ReplaceInFiles,
    Command::SearchToggleMatchCase,
    Command::SearchToggleWholeWord,
    Command::SearchToggleRegex,
    Command::SearchToggleDetails,
    Command::ShowSourceControl,
    Command::AddWorkspaceFolder,
    Command::RemoveWorkspaceFolder,
    Command::SaveWorkspaceAs,
    Command::OpenWorkspaceFromFile,
    Command::ReopenAsHex,
    Command::ReopenAsPreview,
    Command::ReopenAsText,
    Command::NotebookRunAll,
    Command::NotebookInterrupt,
    Command::NotebookRestart,
    Command::TestingRunWithCoverage,
    Command::CoverageClear,
    Command::TestingInstallCoverageTool,
    Command::TestingShowCoverageReport,
    Command::DeveloperShowMemoryUsage,
    Command::RunTestAtCursorWithCoverage,
    Command::TestingToggleWatchAll,
    Command::TestingGoToFirstFailure,
    Command::HexFindNext,
    Command::SheetInsertRowBelow,
    Command::SheetDeleteRow,
    Command::SheetInsertColRight,
    Command::SheetDeleteCol,
    Command::MediaOpenExternal,
    Command::ShowRunDebug,
    Command::ShowRemote,
    Command::ShowExtensions,
    Command::CompareExtensionsWithVscode,
    Command::ShowTesting,
    Command::ShowCodeQL,
    Command::CodeqlRunQuery,
    Command::CodeqlRunQuerySuite,
    Command::CodeqlRemoveDatabase,
    Command::CodeqlRenameDatabase,
    Command::CodeqlSortDatabases,
    Command::CodeqlRevealDatabase,
    Command::CodeqlUpgradeDatabase,
    Command::CodeqlClearCache,
    Command::CodeqlTrimCache,
    Command::CodeqlTrimCacheToOverlay,
    Command::CodeqlAddDatabaseSource,
    Command::CodeqlDeleteUnusedDatabases,
    Command::CodeqlRemoveHistory,
    Command::CodeqlRenameHistory,
    Command::CodeqlSortHistory,
    Command::CodeqlViewQuery,
    Command::CodeqlCreateQuery,
    Command::CodeqlRunPack,
    Command::CodeqlRunQueryOnDatabases,
    Command::CodeqlCancelQueue,
    Command::CodeqlCopyVersion,
    Command::CodeqlDownloadCli,
    Command::CodeqlCheckCliUpdates,
    Command::CodeqlInstallPackDependencies,
    Command::CodeqlDownloadPacks,
    Command::CodeqlRunPublishedPack,
    Command::CodeqlQuickQuery,
    Command::CodeqlCompareResults,
    Command::CodeqlShowEvaluatorLog,
    Command::CodeqlShowEvaluatorLogSummary,
    Command::CodeqlShowEvaluatorLogViewer,
    Command::CodeqlShowQueryLog,
    Command::CodeqlComparePerformance,
    Command::CodeqlPreviewQueryHelp,
    Command::CodeqlExportResults,
    Command::CodeqlSetUpController,
    Command::CodeqlAddVariantRepo,
    Command::CodeqlAddVariantList,
    Command::CodeqlAddVariantOwner,
    Command::CodeqlVariantCodeSearch,
    Command::CodeqlOpenVariantConfig,
    Command::CodeqlOpenVariantOnGithub,
    Command::CodeqlRunVariantAnalysis,
    Command::CodeqlOpenVariantResults,
    Command::CodeqlExportVariantResults,
    Command::CodeqlCopyVariantRepoList,
    Command::CodeqlViewVariantLogs,
    Command::CodeqlRunTests,
    Command::CodeqlDebugQuery,
    Command::CodeqlDebugSelection,
    Command::CodeqlResultsUp,
    Command::CodeqlResultsDown,
    Command::CodeqlResultsLeft,
    Command::CodeqlResultsRight,
    Command::SheetSortByColumn,
    Command::CodeqlQuickEval,
    Command::CodeqlQuickEvalCount,
    Command::CodeqlViewAst,
    Command::CodeqlOpenModelEditor,
    Command::CodeqlModelEndpoint,
    Command::CodeqlOpenReferencedFile,
    Command::CodeqlViewAlertsCsv,
    Command::CodeqlViewAlertsSarif,
    Command::CodeqlViewResultsCsv,
    Command::CodeqlShowResultSet,
    Command::CodeqlViewCfg,
    Command::CodeqlRunAllQueries,
    Command::CodeqlRunSelectedQueries,
    Command::CodeqlCancelRunningQuery,
    Command::CodeqlAcceptTestOutput,
    Command::CodeqlFocusSideBar,
    Command::CodeqlOpenResultsDirectory,
    Command::RunTestAtCursor,
    Command::DebugTestAtCursor,
    Command::ToggleSideBar,
    Command::ToggleAutoHideSideBar,
    Command::ToggleSecondarySideBar,
    Command::ToggleZenMode,
    Command::ToggleTerminal,
    Command::ToggleActivityBar,
    Command::ToggleStatusBar,
    Command::ToggleSideBarPosition,
    Command::SetPanelAlignment,
    Command::SetQuickInputPosition,
    Command::CustomizeLayout,
    Command::FocusTerminal,
    Command::ShowProblems,
    Command::ShowOutput,
    Command::OutputSelectChannel,
    Command::ShowPorts,
    Command::ShowCaptures,
    Command::ToggleMinimap,
    Command::ProblemsCheckProject,
    Command::ProblemsToggleProjectAuto,
    Command::ProblemsToggleScope,
    Command::DiffToggleIgnoreWhitespace,
    Command::NewTerminal,
    Command::FocusNextTerminal,
    Command::FocusPreviousTerminal,
    Command::StartDebugging,
    Command::SelectDebugConfig,
    Command::AddDebugConfig,
    Command::StopDebugging,
    Command::PauseDebugging,
    Command::SwitchDebugSession,
    Command::RestartDebugging,
    Command::ToggleBreakpoint,
    Command::EditBreakpointCondition,
    Command::EditLogpoint,
    Command::ShowIncomingCalls,
    Command::ShowOutgoingCalls,
    Command::ShowSupertypes,
    Command::ShowSubtypes,
    Command::ToggleCodeLens,
    Command::RunCodeLens,
    Command::DebugAddHitCountBreakpoint,
    Command::DebugAddFunctionBreakpoint,
    Command::DebugRemoveFunctionBreakpoints,
    Command::DebugRunToCursor,
    Command::DebugBreakOnValueChange,
    Command::DebugRemoveDataBreakpoints,
    Command::OpenSearchEditor,
    Command::RerunSearchEditor,
    Command::RebaseAbort,
    Command::ToggleTerminalSuggestions,
    Command::ToggleScreenReader,
    Command::OpenKeyboardShortcuts,
    Command::SwitchProfile,
    Command::ToggleInlineSuggestions,
    Command::StepOver,
    Command::ToggleRaisedExceptions,
    Command::AttachPythonProcess,
    Command::RunTask,
    Command::RunBuildTask,
    Command::RerunLastTask,
    Command::ColorTheme,
    Command::KeyboardShortcuts,
    Command::UpdateCroft,
    Command::OpenSettings,
    Command::OpenSettingsEditor,
    Command::OpenSettingsJson,
    Command::OpenWorkspaceSettingsJson,
    Command::OpenWorkspaceSettingsLocalJson,
    Command::OpenKeybindingsJson,
    Command::ConfigureSnippets,
    Command::OpenTriggersJson,
    Command::OpenAgentsJson,
    Command::OpenMatchersJson,
    Command::ToggleTerminalTimestamps,
    Command::ToggleLogHighlight,
    Command::CollapseTerminalPane,
    Command::RestoreTerminalPanes,
    Command::ToggleSecretRedaction,
    Command::RevealRedactedSecrets,
    Command::SearchFromTerminal,
    Command::SessionParticipants,
    Command::SessionDetach,
    Command::CollabCancelStream,
    Command::AskNavigator,
    Command::AskNavigatorAboutCapture,
    Command::OpenWorkspaceOnSshHost,
    Command::ScrubHistory,
    Command::DebugInstallDelve,
    Command::RemoteSyncConfigNow,
    Command::StopAutoApprove,
    Command::ScrubOpenHere,
    Command::ScrubDiffToWorkingTree,
    Command::FleetRun,
    Command::OpenAsSymbolTab,
    Command::LoadReviewThreads,
    Command::ReviewAddComment,
    Command::ReviewSubmit,
    Command::ReviewToggleResolved,
    Command::ReviewDiscardPending,
    Command::ExportCommentsToPr,
    Command::NoteAdd,
    Command::NoteDelete,
    Command::ToggleSessionRecording,
    Command::NewWorktreeLane,
    Command::ReviewPullRequest,
    Command::CloseWorktreeLane,
    Command::DiffWorktreeLane,
    Command::MarkAgentLaneReviewed,
    Command::ShowAgentLane,
    Command::OpenAgentLaneSection,
    Command::MarkAgentFileReviewed,
    Command::DiffAgentFileSinceReview,
    Command::PickAgentLaneFile,
    Command::FixProblemWithNavigator,
    Command::SendHttpRequest,
    Command::CopyHttpRequestAsCurl,
    Command::YieldToNavigator,
    Command::ToggleNavigator,
    Command::ClearNavigatorNotes,
    Command::ToggleProactiveNavigator,
    Command::NextComment,
    Command::IgnoreComment,
];

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
            Command::KillToEndOfLine => "Kill to End of Line",
            Command::TransformUpper => "Transform to Uppercase",
            Command::TransformLower => "Transform to Lowercase",
            Command::TransformTitle => "Transform to Title Case",
            Command::SortLinesAscending => "Sort Lines Ascending",
            Command::SortLinesDescending => "Sort Lines Descending",
            Command::IncrementNumber => "Increment Number Under Cursor",
            Command::DecrementNumber => "Decrement Number Under Cursor",
            Command::TrimTrailingWhitespace => "Trim Trailing Whitespace",
            Command::EmmetExpandAbbreviation => "Emmet: Expand Abbreviation",
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
            Command::GoToNextChange => "Go to Next Change",
            Command::GoToPrevChange => "Go to Previous Change",
            Command::MergeComplete => "Merge: Complete Merge (stage file)",
            Command::MergeOpenEditor => "Merge: Open Merge Editor",
            Command::MergeToggleBase => "Merge: Show Base",
            Command::MergeAcceptBothReverse => "Merge: Accept Combination (Incoming First)",
            Command::MergeIgnore => "Merge: Ignore (Keep Base)",
            Command::DebugAddWatch => "Debug: Add Watch Expression",
            Command::DebugClearWatch => "Debug: Remove All Watch Expressions",
            Command::PeekDefinition => "Peek Definition",
            Command::PeekReferences => "Peek References",
            Command::GoToImplementations => "Go to Implementations",
            Command::MouseAddCursorAtClick => "Mouse: Add Cursor at Click",
            Command::MouseGoToDefinitionAtClick => "Mouse: Go to Definition at Click",
            Command::MouseOpenLinkAtClick => "Mouse: Open Link at Click",
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
            Command::ToggleOverviewRuler => "View: Toggle Overview Ruler",
            Command::ToggleBookmark => "Bookmarks: Toggle Bookmark",
            Command::NextBookmark => "Bookmarks: Next Bookmark",
            Command::PreviousBookmark => "Bookmarks: Previous Bookmark",
            Command::ClearBookmarks => "Bookmarks: Clear All in File",
            Command::FormatDocument => "Format Document",
            Command::FormatSelection => "Format Selection",
            Command::ChangeColorPresentation => "Change Color Presentation",
            Command::ToggleFormatOnType => "Preferences: Toggle Format on Type",
            Command::ToggleFormatOnSave => "Preferences: Toggle Format on Save",
            Command::QuickFix => "Quick Fix",
            Command::ToggleFold => "Toggle Fold",
            Command::FoldAll => "Fold All",
            Command::UnfoldAll => "Unfold All",
            Command::FoldAllComments => "Fold All Block Comments",
            Command::FoldAllRegions => "Fold All Regions",
            Command::UnfoldAllRegions => "Unfold All Regions",
            Command::SaveFile => "File: Save",
            Command::SaveAs => "File: Save As…",
            Command::SaveAll => "File: Save All",
            Command::NewFile => "File: New File…",
            Command::NewFolder => "File: New Folder…",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::SelectAll => "Select All",
            Command::ToggleAutoSave => "File: Toggle Auto Save",
            Command::ToggleAutoSaveOnFocusChange => "File: Toggle Auto Save on Focus Change",
            Command::ToggleInlineBlame => "Git: Toggle Inline Blame",
            Command::ToggleLiveRun => "Python: Toggle Live Run",
            Command::ToggleProvenance => "Editor: Toggle Provenance",
            Command::DiffToggleGroupBySeat => "Diff: Toggle Group by Seat",
            Command::ToggleIndentGuides => "View: Toggle Indent Guides",
            Command::ToggleBracketColors => "Editor: Toggle Bracket Pair Colorization",
            Command::ToggleRenderWhitespace => "View: Toggle Render Whitespace",
            Command::ToggleInlineValues => "Debug: Toggle Inline Values",
            Command::ToggleInlayHints => "Editor: Toggle Inlay Hints",
            Command::ToggleMarkdownPreview => "Markdown: Toggle Preview",
            Command::RunCodeBlockAtCursor => "Markdown: Run Code Block at Cursor",
            Command::RestoreSnapshot => "Local History: Restore Snapshot",
            Command::CloseEditor => "View: Close Editor",
            Command::ReopenClosedEditor => "View: Reopen Closed Editor",
            Command::ZoxideJump => "Explorer: Jump to Directory (zoxide)",
            Command::CloseOtherEditors => "View: Close Other Editors in Group",
            Command::CloseEditorsToTheRight => "View: Close Editors to the Right in Group",
            Command::CloseSavedEditors => "View: Close Saved Editors in Group",
            Command::CloseAllEditors => "View: Close All Editors",
            Command::PinEditor => "View: Pin Editor",
            Command::UnpinEditor => "View: Unpin Editor",
            Command::KeepEditor => "View: Keep Editor",
            Command::SplitEditor => "View: Split Editor",
            Command::FocusLeftEditorGroup => "View: Focus Left Editor Group",
            Command::FocusRightEditorGroup => "View: Focus Right Editor Group",
            Command::QuickOpen => "Go to File",
            Command::TakeTheTour => "Help: Take the Tour",
            Command::SarifNextResult => "SARIF: Next Result",
            Command::SarifPreviousResult => "SARIF: Previous Result",
            Command::SarifOpenCodeScanning => "SARIF: Open GitHub Code Scanning Analysis",
            Command::SarifLoadCodeScanning => "SARIF: Load Code Scanning Results for This Branch",
            Command::GoToSymbol => "Go to Symbol in Editor",
            Command::GoToWorkspaceSymbol => "Go to Symbol in Workspace",
            Command::NavigateBack => "Go Back",
            Command::NavigateForward => "Go Forward",
            Command::GoToLastEditLocation => "Go to Last Edit Location",
            Command::ToggleVimMode => "Toggle Vim Mode",
            Command::MacroStartStopRecording => "Macro: Start/Stop Recording",
            Command::MacroReplayLast => "Macro: Replay Last",
            Command::MacroReplayTimes => "Macro: Replay N Times…",
            Command::ShowExplorer => "View: Show Explorer",
            Command::ShowSearch => "View: Show Search",
            Command::ReplaceInFiles => "Search: Replace in Files",
            Command::SearchToggleMatchCase => "Search: Toggle Match Case",
            Command::SearchToggleWholeWord => "Search: Toggle Match Whole Word",
            Command::SearchToggleRegex => "Search: Toggle Use Regular Expression",
            Command::SearchToggleDetails => "Search: Toggle Search Details",
            Command::ShowSourceControl => "View: Show Source Control",
            Command::AddWorkspaceFolder => "Workspaces: Add Folder to Workspace",
            Command::SaveWorkspaceAs => "Workspaces: Save Workspace As",
            Command::OpenWorkspaceFromFile => "Workspaces: Open Workspace from File",
            Command::ReopenAsHex => "File: Reopen as Hex",
            Command::ReopenAsPreview => "File: Reopen as Preview",
            Command::ReopenAsText => "File: Reopen as Text",
            Command::NotebookRunAll => "Notebook: Run All Cells",
            Command::NotebookInterrupt => "Notebook: Interrupt Kernel",
            Command::NotebookRestart => "Notebook: Restart Kernel",
            Command::TestingRunWithCoverage => "Testing: Run All Tests with Coverage",
            Command::CoverageClear => "Coverage: Clear",
            Command::TestingInstallCoverageTool => "Testing: Install Coverage Tool",
            Command::TestingShowCoverageReport => "Testing: Show Coverage Report",
            Command::DeveloperShowMemoryUsage => "Developer: Show Memory Usage",
            Command::RunTestAtCursorWithCoverage => "Testing: Run Test at Cursor with Coverage",
            Command::TestingToggleWatchAll => "Testing: Toggle Watch All Tests",
            Command::TestingGoToFirstFailure => "Testing: Go to First Failure",
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
            Command::CompareExtensionsWithVscode => "Extensions: Compare with VS Code",
            Command::ShowTesting => "View: Show Testing",
            Command::ShowCodeQL => "View: Show CodeQL",
            Command::CodeqlRunQuery => "CodeQL: Run Query on Selected Database",
            Command::CodeqlRunQuerySuite => "CodeQL: Run Query Suite",
            Command::CodeqlRemoveDatabase => "CodeQL: Remove Database",
            Command::CodeqlRenameDatabase => "CodeQL: Rename Database",
            Command::CodeqlSortDatabases => "CodeQL: Sort Databases",
            Command::CodeqlRevealDatabase => "CodeQL: Reveal Database in Explorer",
            Command::CodeqlUpgradeDatabase => "CodeQL: Upgrade Database",
            Command::CodeqlClearCache => "CodeQL: Clear Cache",
            Command::CodeqlTrimCache => "CodeQL: Trim Cache",
            Command::CodeqlTrimCacheToOverlay => "CodeQL: Trim Cache to Overlay Base",
            Command::CodeqlAddDatabaseSource => "CodeQL: Add Database Source to Workspace",
            Command::CodeqlDeleteUnusedDatabases => "CodeQL: Delete Unused Databases",
            Command::CodeqlRemoveHistory => "CodeQL: Remove Query History Entry",
            Command::CodeqlRenameHistory => "CodeQL: Rename Query History Entry",
            Command::CodeqlSortHistory => "CodeQL: Sort Query History",
            Command::CodeqlViewQuery => "CodeQL: View Query",
            Command::CodeqlCreateQuery => "CodeQL: Create Query",
            Command::CodeqlRunPack => "CodeQL: Run Queries in Pack",
            Command::CodeqlRunQueryOnDatabases => "CodeQL: Run Query on Multiple Databases",
            Command::CodeqlCancelQueue => "CodeQL: Cancel Queued Queries",
            Command::CodeqlCopyVersion => "CodeQL: Copy Version Information",
            Command::CodeqlDownloadCli => "CodeQL: Download CLI",
            Command::CodeqlCheckCliUpdates => "CodeQL: Check for CLI Updates",
            Command::CodeqlInstallPackDependencies => "CodeQL: Install Pack Dependencies",
            Command::CodeqlDownloadPacks => "CodeQL: Download Packs",
            Command::CodeqlRunPublishedPack => "CodeQL: Run Queries in Published Pack",
            Command::CodeqlQuickQuery => "CodeQL: Quick Query",
            Command::CodeqlCompareResults => "CodeQL: Compare Query Results",
            Command::CodeqlShowEvaluatorLog => "CodeQL: Show Evaluator Log (Raw JSON)",
            Command::CodeqlShowEvaluatorLogSummary => "CodeQL: Show Evaluator Log (Summary Text)",
            Command::CodeqlShowEvaluatorLogViewer => "CodeQL: Show Evaluator Log (Viewer)",
            Command::CodeqlShowQueryLog => "CodeQL: Show Query Log",
            Command::CodeqlComparePerformance => "CodeQL: Compare Performance",
            Command::CodeqlPreviewQueryHelp => "CodeQL: Preview Query Help",
            Command::CodeqlExportResults => "CodeQL: Export Results",
            Command::CodeqlSetUpController => "CodeQL: Set Up Controller Repository",
            Command::CodeqlAddVariantRepo => "CodeQL: Add Variant Analysis Repository",
            Command::CodeqlAddVariantList => "CodeQL: Add Variant Analysis Repository List",
            Command::CodeqlAddVariantOwner => "CodeQL: Add Variant Analysis Owner",
            Command::CodeqlVariantCodeSearch => "CodeQL: Add Repositories with GitHub Code Search",
            Command::CodeqlOpenVariantConfig => "CodeQL: Open Variant Analysis Config File",
            Command::CodeqlOpenVariantOnGithub => {
                "CodeQL: Open Variant Analysis Repository on GitHub"
            }
            Command::CodeqlRunVariantAnalysis => "CodeQL: Run Variant Analysis",
            Command::CodeqlOpenVariantResults => "CodeQL: Open Variant Analysis Results",
            Command::CodeqlExportVariantResults => "CodeQL: Export Variant Analysis Results",
            Command::CodeqlCopyVariantRepoList => "CodeQL: Copy Variant Analysis Repository List",
            Command::CodeqlViewVariantLogs => "CodeQL: View Variant Analysis Logs",
            Command::CodeqlRunTests => "CodeQL: Run Tests",
            Command::CodeqlDebugQuery => "CodeQL: Debug Query",
            Command::CodeqlDebugSelection => "CodeQL: Debug Selection",
            Command::CodeqlResultsUp => "CodeQL: Previous Result",
            Command::CodeqlResultsDown => "CodeQL: Next Result",
            Command::CodeqlResultsLeft => "CodeQL: Previous Result Column",
            Command::CodeqlResultsRight => "CodeQL: Next Result Column",
            Command::SheetSortByColumn => "Sheet: Sort by Column",
            Command::CodeqlQuickEval => "CodeQL: Quick Evaluation",
            Command::CodeqlQuickEvalCount => "CodeQL: Quick Evaluation Count",
            Command::CodeqlViewAst => "CodeQL: View AST",
            Command::CodeqlOpenModelEditor => "CodeQL: Open Model Editor",
            Command::CodeqlModelEndpoint => "CodeQL: Model Endpoint",
            Command::CodeqlOpenReferencedFile => "CodeQL: Open Referenced File",
            Command::CodeqlViewAlertsCsv => "CodeQL: View Alerts (CSV)",
            Command::CodeqlViewAlertsSarif => "CodeQL: View Alerts (SARIF)",
            Command::CodeqlViewResultsCsv => "CodeQL: View Results (CSV)",
            Command::CodeqlShowResultSet => "CodeQL: Show Result Set",
            Command::CodeqlViewCfg => "CodeQL: View CFG",
            Command::CodeqlRunAllQueries => "CodeQL: Run All Queries in Workspace",
            Command::CodeqlRunSelectedQueries => "CodeQL: Run Queries in Selected Files",
            Command::CodeqlCancelRunningQuery => "CodeQL: Cancel Running Query",
            Command::CodeqlAcceptTestOutput => "CodeQL: Accept Test Output",
            Command::CodeqlFocusSideBar => "CodeQL: Focus Side Bar",
            Command::CodeqlOpenResultsDirectory => "CodeQL: Open Query Results Directory",
            Command::RunTestAtCursor => "Testing: Run Test at Cursor",
            Command::DebugTestAtCursor => "Testing: Debug Test at Cursor",
            Command::ToggleSideBar => "View: Toggle Primary Side Bar",
            Command::ToggleAutoHideSideBar => "View: Toggle Auto-Hide Side Bar",
            Command::ToggleSecondarySideBar => "View: Toggle Secondary Side Bar",
            Command::ToggleZenMode => "View: Toggle Zen Mode",
            Command::ToggleTerminal => "View: Toggle Terminal",
            Command::ToggleActivityBar => "View: Toggle Activity Bar Visibility",
            Command::ToggleStatusBar => "View: Toggle Status Bar Visibility",
            Command::ToggleSideBarPosition => "View: Toggle Primary Side Bar Position",
            Command::SetPanelAlignment => "View: Set Panel Alignment…",
            Command::SetQuickInputPosition => "View: Set Quick Input Position…",
            Command::CustomizeLayout => "View: Customize Layout…",
            Command::FocusTerminal => "Terminal: Focus Terminal",
            Command::ShowProblems => "View: Show Problems",
            Command::ShowOutput => "View: Show Output",
            Command::OutputSelectChannel => "Output: Select Channel…",
            Command::ShowPorts => "View: Show Ports",
            Command::ShowCaptures => "View: Show Captures",
            Command::ToggleMinimap => "View: Toggle Minimap",
            Command::ProblemsCheckProject => "Problems: Check Whole Project",
            Command::ProblemsToggleProjectAuto => {
                "Problems: Whole-Project Check Auto-Run (Auto / On / Off)"
            }
            Command::ProblemsToggleScope => "Problems: Toggle Scope (Open Files / Whole Project)",
            Command::DiffToggleIgnoreWhitespace => "Diff: Toggle Ignore Whitespace",
            Command::NewTerminal => "Terminal: Create New Terminal",
            Command::FocusNextTerminal => "Terminal: Focus Next Terminal",
            Command::FocusPreviousTerminal => "Terminal: Focus Previous Terminal",
            Command::StartDebugging => "Debug: Start Debugging",
            Command::SelectDebugConfig => "Debug: Select and Start Debugging",
            Command::AddDebugConfig => "Debug: Add Configuration…",
            Command::StopDebugging => "Debug: Stop Debugging",
            Command::PauseDebugging => "Debug: Pause",
            Command::SwitchDebugSession => "Debug: Switch Session",
            Command::RestartDebugging => "Debug: Restart",
            Command::ToggleBreakpoint => "Debug: Toggle Breakpoint",
            Command::EditBreakpointCondition => "Debug: Add Conditional Breakpoint",
            Command::EditLogpoint => "Debug: Add Logpoint",
            Command::ShowIncomingCalls => "Calls: Show Incoming Calls",
            Command::ShowOutgoingCalls => "Calls: Show Outgoing Calls",
            Command::ShowSupertypes => "Types: Show Supertypes",
            Command::ShowSubtypes => "Types: Show Subtypes",
            Command::ToggleCodeLens => "Editor: Toggle CodeLens",
            Command::RunCodeLens => "Editor: Run CodeLens on This Line",
            Command::DebugAddHitCountBreakpoint => "Debug: Add Hit Count Breakpoint",
            Command::DebugAddFunctionBreakpoint => "Debug: Add Function Breakpoint",
            Command::DebugRemoveFunctionBreakpoints => "Debug: Remove All Function Breakpoints",
            Command::DebugRunToCursor => "Debug: Run to Cursor",
            Command::DebugBreakOnValueChange => "Debug: Break When Value Changes",
            Command::DebugRemoveDataBreakpoints => "Debug: Remove All Data Breakpoints",
            Command::OpenSearchEditor => "Search: Open Results in Editor",
            Command::RerunSearchEditor => "Search Editor: Rerun",
            Command::RebaseAbort => "Rebase: Abort",
            Command::ToggleTerminalSuggestions => "Terminal: Toggle Command Suggestions",
            Command::ToggleScreenReader => "Accessibility: Toggle Screen Reader Mode",
            Command::OpenKeyboardShortcuts => "Preferences: Open Keyboard Shortcuts",
            Command::SwitchProfile => "Profiles: Switch Profile",
            Command::ToggleInlineSuggestions => "Editor: Toggle Inline Suggestions",
            Command::StepOver => "Debug: Step Over",
            Command::ToggleRaisedExceptions => "Debug: Toggle Break on Raised Exceptions",
            Command::AttachPythonProcess => "Debug: Attach to Python Process",
            Command::RunTask => "Tasks: Run Task",
            Command::RunBuildTask => "Tasks: Run Build Task",
            Command::RerunLastTask => "Tasks: Rerun Last Task",
            Command::ColorTheme => "Preferences: Color Theme",
            Command::KeyboardShortcuts => "Help: Keyboard Shortcuts Reference",
            Command::UpdateCroft => "Help: Update croft (Relaunch or Rebuild)",
            Command::OpenSettings => "Preferences: Open Settings",
            Command::OpenSettingsEditor => "Preferences: Open Settings (UI)",
            Command::OpenSettingsJson => "Preferences: Open Settings (JSON)",
            Command::OpenWorkspaceSettingsJson => "Preferences: Open Workspace Settings (JSON)",
            Command::OpenWorkspaceSettingsLocalJson => {
                "Preferences: Open Workspace Settings — Local (JSON)"
            }
            Command::OpenKeybindingsJson => "Preferences: Open Keyboard Shortcuts (JSON)",
            Command::ConfigureSnippets => "Preferences: Configure User Snippets",
            Command::OpenTriggersJson => "Preferences: Open Terminal Triggers (JSON)",
            Command::OpenAgentsJson => "Preferences: Open Agent Lanes (JSON)",
            Command::OpenMatchersJson => "Preferences: Open Problem Matchers (JSON)",
            Command::ToggleTerminalTimestamps => "Terminal: Toggle Timestamps",
            Command::ToggleLogHighlight => "Log: Toggle Highlighting (tailspin)",
            Command::CollapseTerminalPane => "Terminal: Collapse Pane",
            Command::RestoreTerminalPanes => "Terminal: Restore All Collapsed Panes",
            Command::ToggleSecretRedaction => "Terminal: Toggle Secret Redaction",
            Command::RevealRedactedSecrets => "Terminal: Reveal Redacted Secrets for 10s",
            Command::SearchFromTerminal => "Terminal: Search & Replace from Last grep/rg",
            Command::SessionParticipants => "Session: Participants",
            Command::SessionDetach => "Session: Detach",
            Command::CollabCancelStream => "Collab: Cancel AI Stream",
            Command::AskNavigator => "Navigator: Ask About Line or Selection",
            Command::AskNavigatorAboutCapture => "Captures: Ask Navigator About This Line",
            Command::OpenWorkspaceOnSshHost => "Remote: Open Workspace on This Pane's Host",
            Command::ScrubHistory => "Source Control: Scrub History",
            Command::DebugInstallDelve => "Debug: Install Go Debugger (delve)",
            Command::RemoteSyncConfigNow => "Remote: Sync Config Now",
            Command::StopAutoApprove => "Agents: Stop Auto-Approving",
            Command::ScrubOpenHere => "Source Control: Open Scrubbed File Here",
            Command::ScrubDiffToWorkingTree => "Source Control: Diff Scrubbed File to Working Tree",
            Command::FleetRun => "Terminal: Fleet Run",
            Command::OpenAsSymbolTab => "Editor: Open Symbol as Its Own Tab",
            Command::LoadReviewThreads => "Review: Load PR Comments for This File",
            Command::ReviewAddComment => "Review: Add Comment on This Line",
            Command::ReviewSubmit => "Review: Submit Review",
            Command::ReviewToggleResolved => "Review: Resolve or Unresolve Thread",
            Command::ReviewDiscardPending => "Review: Discard Pending Comments",
            Command::ExportCommentsToPr => "Source Control: Export Comments to Pull Request",
            Command::NoteAdd => "Notes: Add Note on This Line",
            Command::NoteDelete => "Notes: Delete Note",
            Command::ToggleSessionRecording => "Session: Record Terminal as Asciicast",
            Command::NewWorktreeLane => "Agent: New Worktree Lane",
            Command::ReviewPullRequest => "Source Control: Review Pull Request",
            Command::CloseWorktreeLane => "Agent: Close Worktree Lane",
            Command::DiffWorktreeLane => "Agent: Diff Lane Against Its Base",
            Command::MarkAgentLaneReviewed => "Agents: Mark Changed Files Reviewed",
            Command::ShowAgentLane => "Agents: Show Changed Files",
            Command::OpenAgentLaneSection => "Agents: Open Agent Lane",
            Command::MarkAgentFileReviewed => "Agents: Mark This File Reviewed",
            Command::DiffAgentFileSinceReview => "Agents: Diff This File Since Review",
            Command::PickAgentLaneFile => "Agents: Review a Changed File…",
            Command::FixProblemWithNavigator => "Problems: Fix With Navigator",
            Command::SendHttpRequest => "HTTP: Send Request Under Caret",
            Command::CopyHttpRequestAsCurl => "HTTP: Copy Request as curl",
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
    /// names; on Linux/Android the command modifier is `Ctrl`, and
    /// [`platform_hint`] spells it that way where it is shown.
    pub fn keybinding_hint(self) -> &'static str {
        match self {
            Command::MoveLineUp => "Alt+↑",
            Command::MoveLineDown => "Alt+↓",
            Command::JoinLines => "Cmd+Opt+Shift+J",
            Command::DeleteLine => "Cmd+Shift+K",
            // macOS keeps `Ctrl+K`; off it `Ctrl+K` is the `Cmd+K` leader
            // except in vim mode (#843), so the palette is the way there.
            Command::KillToEndOfLine => {
                if cfg!(target_os = "macos") {
                    "Ctrl+K"
                } else {
                    ""
                }
            }
            Command::TransformUpper => "Cmd+Opt+Shift+U",
            Command::TransformLower => "Cmd+Opt+Shift+L",
            Command::TransformTitle => "Cmd+Opt+Shift+C",
            Command::SortLinesAscending => "Cmd+Opt+Shift+A",
            Command::SortLinesDescending => "Cmd+Opt+Shift+D",
            Command::IncrementNumber => "Cmd+Opt+=",
            Command::DecrementNumber => "Cmd+Opt+-",
            Command::TrimTrailingWhitespace => "Cmd+Opt+Shift+W",
            Command::EmmetExpandAbbreviation => "Cmd+Opt+Shift+E",
            Command::ToggleLineComment => "Cmd+/",
            Command::ToggleBlockComment => "Shift+Alt+A",
            Command::ToggleWordWrap => "Alt+Z",
            Command::ExpandSelection => "Shift+Alt+\u{2192}",
            Command::ShrinkSelection => "Shift+Alt+\u{2190}",
            Command::ReplaceInFile => "Cmd+Opt+F",
            Command::ToggleAutoSave => "",
            Command::ToggleAutoSaveOnFocusChange => "",
            Command::ToggleInlineBlame => "",
            Command::ToggleLiveRun => "Cmd+K V",
            Command::ToggleProvenance => "",
            Command::DiffToggleGroupBySeat => "",
            Command::ToggleIndentGuides => "",
            Command::ToggleBracketColors => "",
            Command::ToggleRenderWhitespace => "",
            Command::ToggleInlineValues => "",
            Command::ToggleInlayHints => "",
            Command::ToggleMarkdownPreview => "Cmd+Shift+V",
            Command::RunCodeBlockAtCursor => "Cmd+Enter",
            Command::RestoreSnapshot => "",
            Command::MergeAcceptCurrent => "Cmd+.",
            Command::MergeAcceptIncoming => "Cmd+.",
            Command::MergeAcceptBoth => "Cmd+.",
            Command::MergeAcceptAllCurrent => "",
            Command::MergeAcceptAllIncoming => "",
            Command::MergeNextConflict => "F7",
            Command::MergePrevConflict => "Shift+F7",
            Command::GoToNextChange => "F7",
            Command::GoToPrevChange => "Shift+F7",
            Command::MergeComplete => "",
            Command::MergeOpenEditor => "",
            Command::MergeToggleBase => "",
            Command::MergeAcceptBothReverse => "",
            Command::MergeIgnore => "",
            Command::DebugAddWatch => "",
            Command::DebugClearWatch => "",
            Command::PeekDefinition => "Alt+F12",
            Command::PeekReferences => "Alt+Shift+F12",
            Command::GoToImplementations => "Cmd+F12",
            Command::MouseAddCursorAtClick => "",
            Command::MouseGoToDefinitionAtClick => "",
            Command::MouseOpenLinkAtClick => "",
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
            Command::ToggleOverviewRuler => "",
            Command::ToggleBookmark => "Cmd+Opt+Shift+K",
            Command::NextBookmark => "Cmd+Opt+.",
            Command::PreviousBookmark => "Cmd+Opt+,",
            Command::ClearBookmarks => "Cmd+Opt+Shift+B",
            Command::FormatDocument => "Cmd+Opt+Shift+F",
            Command::FormatSelection => "Cmd+K Cmd+F",
            Command::ChangeColorPresentation => "",
            Command::ToggleFormatOnType => "",
            Command::ToggleFormatOnSave => "Cmd+K F",
            Command::QuickFix => "Cmd+.",
            Command::ToggleFold => "Cmd+K Cmd+L",
            Command::FoldAll => "Cmd+K Cmd+0",
            Command::UnfoldAll => "Cmd+K Cmd+J",
            Command::FoldAllComments => "Cmd+K Cmd+/",
            Command::FoldAllRegions => "Cmd+K Cmd+8",
            Command::UnfoldAllRegions => "Cmd+K Cmd+9",
            Command::SaveFile => "Cmd+S",
            Command::SaveAs => "",
            // VS Code's macOS chord; its Linux `Ctrl+K S` is Select for
            // Compare here, so Linux takes `Ctrl+Alt+S` instead.
            Command::SaveAll => "Cmd+Opt+S",
            // Palette-only: the Explorer's `Cmd+F` / `Cmd+Shift+N` are Find
            // and unbound outside it, so neither is the command's chord.
            Command::NewFile => "",
            Command::NewFolder => "",
            Command::Undo => "Cmd+Z",
            Command::Redo => "Shift+Cmd+Z",
            Command::SelectAll => "Cmd+A",
            Command::CloseEditor => "Cmd+W",
            Command::ReopenClosedEditor => "Cmd+K Shift+W",
            Command::ZoxideJump => "Cmd+Z in Explorer",
            // No chord: the tab menu's ⌥⌘T hint is iTerm2's own New Tab
            // chord, which croft leaves alone.
            Command::CloseOtherEditors => "",
            Command::CloseEditorsToTheRight => "Cmd+K →",
            Command::CloseSavedEditors => "Cmd+K U",
            Command::CloseAllEditors => "Cmd+K W",
            // One chord toggles, as VS Code's `Ctrl+K Shift+Enter` serves
            // both through their `when` clauses.
            Command::PinEditor => "Cmd+K P",
            Command::UnpinEditor => "Cmd+K P",
            Command::KeepEditor => "Cmd+K Shift+P",
            Command::SplitEditor => "Cmd+\\",
            Command::FocusLeftEditorGroup => "Cmd+Opt+←",
            Command::FocusRightEditorGroup => "Cmd+Opt+→",
            Command::QuickOpen => "Cmd+P",
            Command::TakeTheTour => "",
            Command::SarifNextResult => "",
            Command::SarifPreviousResult => "",
            Command::SarifOpenCodeScanning => "",
            Command::SarifLoadCodeScanning => "",
            Command::GoToSymbol => "Cmd+Shift+O",
            Command::GoToWorkspaceSymbol => "Cmd+P #",
            Command::NavigateBack => "Ctrl+-",
            Command::NavigateForward => "Ctrl+Shift+-",
            Command::GoToLastEditLocation => "Cmd+K Cmd+Q",
            Command::ToggleVimMode => "Cmd+E",
            Command::MacroStartStopRecording => "",
            Command::MacroReplayLast => "",
            Command::MacroReplayTimes => "",
            Command::ShowExplorer => "Cmd+Shift+E",
            Command::ShowSearch => "Cmd+Shift+F",
            Command::ReplaceInFiles => "Cmd+Shift+H",
            // VS Code's search-input keys, live while a Search input has
            // focus; run from the palette they reveal the side bar first.
            Command::SearchToggleMatchCase => "Alt+C",
            Command::SearchToggleWholeWord => "Alt+W",
            Command::SearchToggleRegex => "Alt+R",
            // VS Code binds none that is free here (its Ctrl+Shift+J is
            // croft's maximize-terminal); D for details.
            Command::SearchToggleDetails => "Alt+D",
            Command::ShowSourceControl => "Cmd+Shift+S",
            Command::AddWorkspaceFolder => "",
            Command::SaveWorkspaceAs => "",
            Command::OpenWorkspaceFromFile => "",
            Command::ReopenAsHex => "",
            Command::ReopenAsPreview => "",
            Command::ReopenAsText => "",
            Command::NotebookRunAll => "",
            Command::NotebookInterrupt => "",
            Command::NotebookRestart => "",
            Command::TestingRunWithCoverage => "",
            Command::CoverageClear => "",
            Command::TestingInstallCoverageTool => "",
            Command::TestingShowCoverageReport => "",
            Command::DeveloperShowMemoryUsage => "",
            Command::RunTestAtCursorWithCoverage => "",
            Command::TestingToggleWatchAll => "",
            Command::TestingGoToFirstFailure => "",
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
            Command::CompareExtensionsWithVscode => "",
            Command::ShowTesting => "Cmd+K B",
            Command::ShowCodeQL => "",
            Command::CodeqlRunQuery => "",
            Command::CodeqlRunQuerySuite => "",
            Command::CodeqlRemoveDatabase => "",
            Command::CodeqlRenameDatabase => "",
            Command::CodeqlSortDatabases => "",
            Command::CodeqlRevealDatabase => "",
            Command::CodeqlUpgradeDatabase => "",
            Command::CodeqlClearCache => "",
            Command::CodeqlTrimCache => "",
            Command::CodeqlTrimCacheToOverlay => "",
            Command::CodeqlAddDatabaseSource => "",
            Command::CodeqlDeleteUnusedDatabases => "",
            Command::CodeqlRemoveHistory => "",
            Command::CodeqlRenameHistory => "",
            Command::CodeqlSortHistory => "",
            Command::CodeqlViewQuery => "",
            Command::CodeqlCreateQuery => "",
            Command::CodeqlRunPack => "",
            Command::CodeqlRunQueryOnDatabases => "",
            Command::CodeqlCancelQueue => "",
            Command::CodeqlCopyVersion => "",
            Command::CodeqlDownloadCli => "",
            Command::CodeqlCheckCliUpdates => "",
            Command::CodeqlInstallPackDependencies => "",
            Command::CodeqlDownloadPacks => "",
            Command::CodeqlRunPublishedPack => "",
            Command::CodeqlQuickQuery => "",
            Command::CodeqlCompareResults => "",
            Command::CodeqlShowEvaluatorLog => "",
            Command::CodeqlShowEvaluatorLogSummary => "",
            Command::CodeqlShowEvaluatorLogViewer => "",
            Command::CodeqlShowQueryLog => "",
            Command::CodeqlComparePerformance => "",
            Command::CodeqlPreviewQueryHelp => "",
            Command::CodeqlExportResults => "",
            Command::CodeqlSetUpController => "",
            Command::CodeqlAddVariantRepo => "",
            Command::CodeqlAddVariantList => "",
            Command::CodeqlAddVariantOwner => "",
            Command::CodeqlVariantCodeSearch => "",
            Command::CodeqlOpenVariantConfig => "",
            Command::CodeqlOpenVariantOnGithub => "",
            Command::CodeqlRunVariantAnalysis => "",
            Command::CodeqlOpenVariantResults => "",
            Command::CodeqlExportVariantResults => "",
            Command::CodeqlCopyVariantRepoList => "",
            Command::CodeqlViewVariantLogs => "",
            Command::CodeqlRunTests => "",
            Command::CodeqlDebugQuery => "",
            Command::CodeqlDebugSelection => "",
            Command::CodeqlResultsUp => "",
            Command::CodeqlResultsDown => "",
            Command::CodeqlResultsLeft => "",
            Command::CodeqlResultsRight => "",
            Command::SheetSortByColumn => "",
            Command::CodeqlQuickEval => "",
            Command::CodeqlQuickEvalCount => "",
            Command::CodeqlViewAst => "",
            Command::CodeqlOpenModelEditor => "",
            Command::CodeqlModelEndpoint => "",
            Command::CodeqlOpenReferencedFile => "",
            Command::CodeqlViewAlertsCsv => "",
            Command::CodeqlViewAlertsSarif => "",
            Command::CodeqlViewResultsCsv => "",
            Command::CodeqlShowResultSet => "",
            Command::CodeqlViewCfg => "",
            Command::CodeqlRunAllQueries => "",
            Command::CodeqlRunSelectedQueries => "",
            Command::CodeqlCancelRunningQuery => "",
            Command::CodeqlAcceptTestOutput => "",
            Command::CodeqlFocusSideBar => "",
            Command::CodeqlOpenResultsDirectory => "",
            Command::RunTestAtCursor => "Cmd+K Enter",
            Command::DebugTestAtCursor => "Cmd+K Shift+Enter",
            Command::ToggleSideBar => "Cmd+B",
            Command::ToggleAutoHideSideBar => "",
            Command::ToggleSecondarySideBar => "Cmd+Opt+B",
            Command::ToggleZenMode => "Cmd+K Z",
            Command::ToggleTerminal => "Ctrl+J",
            Command::ToggleActivityBar => "",
            Command::ToggleStatusBar => "",
            Command::ToggleSideBarPosition => "",
            Command::SetPanelAlignment => "",
            Command::SetQuickInputPosition => "",
            Command::CustomizeLayout => "",
            Command::FocusTerminal => "Cmd+Shift+T",
            Command::ShowProblems => "Cmd+Shift+M",
            Command::ShowOutput => "Cmd+Shift+U",
            Command::OutputSelectChannel => "",
            Command::ShowPorts => "",
            Command::ShowCaptures => "",
            Command::ToggleMinimap => "Cmd+Opt+M",
            Command::ProblemsCheckProject => "",
            Command::ProblemsToggleProjectAuto => "",
            Command::ProblemsToggleScope => "",
            Command::DiffToggleIgnoreWhitespace => "",
            Command::NewTerminal => "Cmd+T",
            Command::FocusNextTerminal => "Cmd+]",
            Command::FocusPreviousTerminal => "Cmd+[",
            Command::KeyboardShortcuts => "F1",
            Command::UpdateCroft => "Cmd+Shift+F9",
            Command::StartDebugging => "F5",
            Command::SelectDebugConfig => "",
            Command::AddDebugConfig => "",
            Command::StopDebugging => "Shift+F5",
            Command::PauseDebugging => "F6",
            Command::SwitchDebugSession => "Cmd+Opt+Shift+G",
            Command::ToggleBreakpoint => "F9",
            Command::StepOver => "F10",
            Command::RestartDebugging => "Shift+Cmd+F5",
            Command::EditBreakpointCondition => "Shift+F9",
            Command::EditLogpoint => "Shift+Alt+F9",
            Command::ShowIncomingCalls => "Cmd+K H",
            Command::ShowOutgoingCalls => "Cmd+K Shift+H",
            Command::ShowSupertypes => "Cmd+K Shift+U",
            Command::ShowSubtypes => "Cmd+K Shift+D",
            Command::ToggleCodeLens => "",
            Command::RunCodeLens => "Cmd+K Shift+E",
            Command::DebugAddHitCountBreakpoint => "",
            Command::DebugAddFunctionBreakpoint => "",
            Command::DebugRemoveFunctionBreakpoints => "",
            Command::DebugRunToCursor => "Ctrl+F10",
            Command::DebugBreakOnValueChange => "",
            Command::DebugRemoveDataBreakpoints => "",
            Command::OpenSearchEditor => "Cmd+K Shift+F",
            Command::RerunSearchEditor => "Cmd+K Shift+R",
            Command::RebaseAbort => "",
            Command::ToggleTerminalSuggestions => "",
            Command::ToggleScreenReader => "",
            Command::OpenKeyboardShortcuts => "Cmd+K Cmd+S",
            Command::SwitchProfile => "",
            Command::ToggleInlineSuggestions => "Cmd+K Shift+G",
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
            Command::OpenSettingsEditor => "",
            Command::OpenSettingsJson => "",
            Command::OpenWorkspaceSettingsJson => "",
            Command::OpenWorkspaceSettingsLocalJson => "",
            Command::OpenKeybindingsJson => "",
            Command::ConfigureSnippets => "",
            Command::OpenTriggersJson => "",
            Command::OpenAgentsJson => "",
            Command::OpenMatchersJson => "",
            Command::ToggleTerminalTimestamps => "",
            Command::ToggleLogHighlight => "",
            Command::CollapseTerminalPane => "Cmd+K [",
            Command::RestoreTerminalPanes => "Cmd+K ]",
            Command::ToggleSecretRedaction => "",
            Command::RevealRedactedSecrets => "",
            Command::SearchFromTerminal => "",
            Command::SessionParticipants => "Cmd+K A",
            Command::SessionDetach => "Cmd+K Shift+Q",
            Command::CollabCancelStream => "Cmd+K X",
            Command::AskNavigator => "Cmd+K Q",
            Command::AskNavigatorAboutCapture => "",
            Command::OpenWorkspaceOnSshHost => "",
            Command::ScrubHistory => "",
            Command::DebugInstallDelve => "",
            Command::RemoteSyncConfigNow => "",
            Command::StopAutoApprove => "",
            Command::ScrubOpenHere => "",
            Command::ScrubDiffToWorkingTree => "",
            Command::FleetRun => "",
            Command::OpenAsSymbolTab => "Cmd+K Shift+V",
            Command::LoadReviewThreads => "",
            Command::ReviewAddComment => "",
            Command::ReviewSubmit => "",
            Command::ReviewToggleResolved => "",
            Command::ReviewDiscardPending => "",
            Command::ExportCommentsToPr => "",
            Command::NoteAdd => "Cmd+K N",
            Command::NoteDelete => "",
            Command::ToggleSessionRecording => "",
            Command::NewWorktreeLane => "Cmd+K Shift+L",
            Command::ReviewPullRequest => "",
            Command::CloseWorktreeLane => "",
            // No chord: the lane commands that have one are the ones you
            // reach mid-flow; a diff is deliberate and the palette is where
            // you go for it.
            Command::DiffWorktreeLane => "",
            Command::MarkAgentLaneReviewed => "",
            Command::ShowAgentLane => "",
            Command::OpenAgentLaneSection => "",
            Command::MarkAgentFileReviewed => "",
            Command::DiffAgentFileSinceReview => "",
            Command::PickAgentLaneFile => "",
            Command::FixProblemWithNavigator => "",
            Command::SendHttpRequest => "Cmd+Enter",
            Command::CopyHttpRequestAsCurl => "",
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
            Command::KillToEndOfLine => "kill_to_end_of_line",
            Command::TransformUpper => "transform_upper",
            Command::TransformLower => "transform_lower",
            Command::TransformTitle => "transform_title",
            Command::SortLinesAscending => "sort_lines_ascending",
            Command::SortLinesDescending => "sort_lines_descending",
            Command::IncrementNumber => "increment_number",
            Command::DecrementNumber => "decrement_number",
            Command::TrimTrailingWhitespace => "trim_trailing_whitespace",
            Command::EmmetExpandAbbreviation => "emmet_expand_abbreviation",
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
            Command::GoToNextChange => "next_change",
            Command::GoToPrevChange => "previous_change",
            Command::MergeComplete => "merge_complete",
            Command::MergeOpenEditor => "merge_open_editor",
            Command::MergeToggleBase => "merge_toggle_base",
            Command::MergeAcceptBothReverse => "merge_accept_both_reverse",
            Command::MergeIgnore => "merge_ignore",
            Command::DebugAddWatch => "debug_add_watch",
            Command::DebugClearWatch => "debug_clear_watch",
            Command::PeekDefinition => "peek_definition",
            Command::PeekReferences => "peek_references",
            Command::GoToImplementations => "go_to_implementations",
            Command::MouseAddCursorAtClick => "mouse_add_cursor_at_click",
            Command::MouseGoToDefinitionAtClick => "mouse_go_to_definition_at_click",
            Command::MouseOpenLinkAtClick => "mouse_open_link_at_click",
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
            Command::ToggleOverviewRuler => "toggle_overview_ruler",
            Command::ToggleBookmark => "toggle_bookmark",
            Command::NextBookmark => "next_bookmark",
            Command::PreviousBookmark => "previous_bookmark",
            Command::ClearBookmarks => "clear_bookmarks",
            Command::FormatDocument => "format_document",
            Command::FormatSelection => "format_selection",
            Command::ChangeColorPresentation => "change_color_presentation",
            Command::ToggleFormatOnType => "toggle_format_on_type",
            Command::ToggleFormatOnSave => "toggle_format_on_save",
            Command::QuickFix => "quick_fix",
            Command::ToggleFold => "toggle_fold",
            Command::FoldAll => "fold_all",
            Command::UnfoldAll => "unfold_all",
            Command::FoldAllComments => "fold_all_comments",
            Command::FoldAllRegions => "fold_all_regions",
            Command::UnfoldAllRegions => "unfold_all_regions",
            Command::SaveFile => "save_file",
            Command::SaveAs => "save_as",
            Command::SaveAll => "save_all",
            Command::NewFile => "new_file",
            Command::NewFolder => "new_folder",
            Command::Undo => "undo",
            Command::Redo => "redo",
            Command::SelectAll => "select_all",
            Command::ToggleAutoSave => "toggle_auto_save",
            Command::ToggleAutoSaveOnFocusChange => "toggle_auto_save_on_focus_change",
            Command::ToggleInlineBlame => "toggle_inline_blame",
            Command::ToggleLiveRun => "toggle_live_run",
            Command::ToggleProvenance => "toggle_provenance",
            Command::DiffToggleGroupBySeat => "diff_toggle_group_by_seat",
            Command::ToggleIndentGuides => "toggle_indent_guides",
            Command::ToggleBracketColors => "toggle_bracket_colors",
            Command::ToggleRenderWhitespace => "toggle_render_whitespace",
            Command::ToggleInlineValues => "toggle_inline_values",
            Command::ToggleInlayHints => "toggle_inlay_hints",
            Command::ToggleMarkdownPreview => "toggle_markdown_preview",
            Command::RunCodeBlockAtCursor => "run_code_block_at_cursor",
            Command::RestoreSnapshot => "restore_snapshot",
            Command::CloseEditor => "close_editor",
            Command::ReopenClosedEditor => "reopen_closed_editor",
            Command::ZoxideJump => "zoxide_jump",
            Command::CloseOtherEditors => "close_other_editors",
            Command::CloseEditorsToTheRight => "close_editors_to_the_right",
            Command::CloseSavedEditors => "close_saved_editors",
            Command::CloseAllEditors => "close_all_editors",
            Command::PinEditor => "pin_editor",
            Command::UnpinEditor => "unpin_editor",
            Command::KeepEditor => "keep_editor",
            Command::SplitEditor => "split_editor",
            Command::FocusLeftEditorGroup => "focus_left_editor_group",
            Command::FocusRightEditorGroup => "focus_right_editor_group",
            Command::QuickOpen => "quick_open",
            Command::TakeTheTour => "help_take_the_tour",
            Command::SarifNextResult => "sarif_next_result",
            Command::SarifPreviousResult => "sarif_previous_result",
            Command::SarifOpenCodeScanning => "sarif_open_code_scanning",
            Command::SarifLoadCodeScanning => "sarif_load_code_scanning",
            Command::GoToSymbol => "go_to_symbol",
            Command::GoToWorkspaceSymbol => "go_to_workspace_symbol",
            Command::NavigateBack => "navigate_back",
            Command::NavigateForward => "navigate_forward",
            Command::GoToLastEditLocation => "go_to_last_edit_location",
            Command::ToggleVimMode => "toggle_vim_mode",
            Command::MacroStartStopRecording => "macro_record",
            Command::MacroReplayLast => "macro_replay_last",
            Command::MacroReplayTimes => "macro_replay_times",
            Command::ShowExplorer => "show_explorer",
            Command::ShowSearch => "show_search",
            Command::ReplaceInFiles => "replace_in_files",
            Command::SearchToggleMatchCase => "search_toggle_match_case",
            Command::SearchToggleWholeWord => "search_toggle_whole_word",
            Command::SearchToggleRegex => "search_toggle_regex",
            Command::SearchToggleDetails => "search_toggle_details",
            Command::ShowSourceControl => "show_source_control",
            Command::AddWorkspaceFolder => "add_workspace_folder",
            Command::SaveWorkspaceAs => "save_workspace_as",
            Command::OpenWorkspaceFromFile => "open_workspace_from_file",
            Command::ReopenAsHex => "reopen_as_hex",
            Command::ReopenAsPreview => "reopen_as_preview",
            Command::ReopenAsText => "reopen_as_text",
            Command::NotebookRunAll => "notebook_run_all",
            Command::NotebookInterrupt => "notebook_interrupt",
            Command::NotebookRestart => "notebook_restart",
            Command::TestingRunWithCoverage => "testing_run_with_coverage",
            Command::CoverageClear => "coverage_clear",
            Command::TestingInstallCoverageTool => "testing_install_coverage_tool",
            Command::TestingShowCoverageReport => "testing_show_coverage_report",
            Command::DeveloperShowMemoryUsage => "developer_show_memory_usage",
            Command::RunTestAtCursorWithCoverage => "run_test_at_cursor_with_coverage",
            Command::TestingToggleWatchAll => "testing_toggle_watch_all",
            Command::TestingGoToFirstFailure => "testing_go_to_first_failure",
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
            Command::CompareExtensionsWithVscode => "compare_extensions_with_vscode",
            Command::ShowTesting => "show_testing",
            Command::ShowCodeQL => "show_codeql",
            Command::CodeqlRunQuery => "codeql_run_query",
            Command::CodeqlRunQuerySuite => "codeql_run_query_suite",
            Command::CodeqlRemoveDatabase => "codeql_remove_database",
            Command::CodeqlRenameDatabase => "codeql_rename_database",
            Command::CodeqlSortDatabases => "codeql_sort_databases",
            Command::CodeqlRevealDatabase => "codeql_reveal_database",
            Command::CodeqlUpgradeDatabase => "codeql_upgrade_database",
            Command::CodeqlClearCache => "codeql_clear_cache",
            Command::CodeqlTrimCache => "codeql_trim_cache",
            Command::CodeqlTrimCacheToOverlay => "codeql_trim_cache_to_overlay_base",
            Command::CodeqlAddDatabaseSource => "codeql_add_database_source",
            Command::CodeqlDeleteUnusedDatabases => "codeql_delete_unused_databases",
            Command::CodeqlRemoveHistory => "codeql_remove_history",
            Command::CodeqlRenameHistory => "codeql_rename_history",
            Command::CodeqlSortHistory => "codeql_sort_history",
            Command::CodeqlViewQuery => "codeql_view_query",
            Command::CodeqlCreateQuery => "codeql_create_query",
            Command::CodeqlRunPack => "codeql_run_pack",
            Command::CodeqlRunQueryOnDatabases => "codeql_run_query_on_databases",
            Command::CodeqlCancelQueue => "codeql_cancel_queue",
            Command::CodeqlCopyVersion => "codeql_copy_version_information",
            Command::CodeqlDownloadCli => "codeql_download_cli",
            Command::CodeqlCheckCliUpdates => "codeql_check_for_cli_updates",
            Command::CodeqlInstallPackDependencies => "codeql_install_pack_dependencies",
            Command::CodeqlDownloadPacks => "codeql_download_packs",
            Command::CodeqlRunPublishedPack => "codeql_run_published_pack",
            Command::CodeqlQuickQuery => "codeql_quick_query",
            Command::CodeqlCompareResults => "codeql_compare_query_results",
            Command::CodeqlShowEvaluatorLog => "codeql_show_evaluator_log",
            Command::CodeqlShowEvaluatorLogSummary => "codeql_show_evaluator_log_summary",
            Command::CodeqlShowEvaluatorLogViewer => "codeql_show_evaluator_log_viewer",
            Command::CodeqlShowQueryLog => "codeql_show_query_log",
            Command::CodeqlComparePerformance => "codeql_compare_performance",
            Command::CodeqlPreviewQueryHelp => "codeql_preview_query_help",
            Command::CodeqlExportResults => "codeql_export_results",
            Command::CodeqlSetUpController => "codeql_set_up_controller_repository",
            Command::CodeqlAddVariantRepo => "codeql_add_variant_analysis_repository",
            Command::CodeqlAddVariantList => "codeql_add_variant_analysis_list",
            Command::CodeqlAddVariantOwner => "codeql_add_variant_analysis_owner",
            Command::CodeqlVariantCodeSearch => "codeql_add_repositories_with_code_search",
            Command::CodeqlOpenVariantConfig => "codeql_open_variant_analysis_config",
            Command::CodeqlOpenVariantOnGithub => "codeql_open_variant_analysis_on_github",
            Command::CodeqlRunVariantAnalysis => "codeql_run_variant_analysis",
            Command::CodeqlOpenVariantResults => "codeql_open_variant_analysis_results",
            Command::CodeqlExportVariantResults => "codeql_export_variant_analysis_results",
            Command::CodeqlCopyVariantRepoList => "codeql_copy_variant_repo_list",
            Command::CodeqlViewVariantLogs => "codeql_view_variant_logs",
            Command::CodeqlRunTests => "codeql_run_tests",
            Command::CodeqlDebugQuery => "codeql_debug_query",
            Command::CodeqlDebugSelection => "codeql_debug_selection",
            Command::CodeqlResultsUp => "codeql_results_up",
            Command::CodeqlResultsDown => "codeql_results_down",
            Command::CodeqlResultsLeft => "codeql_results_left",
            Command::CodeqlResultsRight => "codeql_results_right",
            Command::SheetSortByColumn => "sheet_sort_by_column",
            Command::CodeqlQuickEval => "codeql_quick_eval",
            Command::CodeqlQuickEvalCount => "codeql_quick_eval_count",
            Command::CodeqlViewAst => "codeql_view_ast",
            Command::CodeqlOpenModelEditor => "codeql_open_model_editor",
            Command::CodeqlModelEndpoint => "codeql_model_endpoint",
            Command::CodeqlOpenReferencedFile => "codeql_open_referenced_file",
            Command::CodeqlViewAlertsCsv => "codeql_view_alerts_csv",
            Command::CodeqlViewAlertsSarif => "codeql_view_alerts_sarif",
            Command::CodeqlViewResultsCsv => "codeql_view_results_csv",
            Command::CodeqlShowResultSet => "codeql_show_result_set",
            Command::CodeqlViewCfg => "codeql_view_cfg",
            Command::CodeqlRunAllQueries => "codeql_run_all_queries",
            Command::CodeqlRunSelectedQueries => "codeql_run_selected_queries",
            Command::CodeqlCancelRunningQuery => "codeql_cancel_running_query",
            Command::CodeqlAcceptTestOutput => "codeql_accept_test_output",
            Command::CodeqlFocusSideBar => "codeql_focus_side_bar",
            Command::CodeqlOpenResultsDirectory => "codeql_open_results_directory",
            Command::RunTestAtCursor => "run_test_at_cursor",
            Command::DebugTestAtCursor => "debug_test_at_cursor",
            Command::ToggleSideBar => "toggle_side_bar",
            Command::ToggleAutoHideSideBar => "toggle_auto_hide_side_bar",
            Command::ToggleSecondarySideBar => "toggle_secondary_side_bar",
            Command::ToggleZenMode => "toggle_zen_mode",
            Command::ToggleTerminal => "toggle_terminal",
            Command::ToggleActivityBar => "toggle_activity_bar",
            Command::ToggleStatusBar => "toggle_status_bar",
            Command::ToggleSideBarPosition => "toggle_side_bar_position",
            Command::SetPanelAlignment => "set_panel_alignment",
            Command::SetQuickInputPosition => "set_quick_input_position",
            Command::CustomizeLayout => "customize_layout",
            Command::FocusTerminal => "focus_terminal",
            Command::ShowProblems => "show_problems",
            Command::ShowOutput => "show_output",
            Command::OutputSelectChannel => "output_select_channel",
            Command::ShowPorts => "show_ports",
            Command::ShowCaptures => "show_captures",
            Command::ToggleMinimap => "toggle_minimap",
            Command::ProblemsCheckProject => "problems_check_project",
            Command::ProblemsToggleProjectAuto => "problems_toggle_project_auto",
            Command::ProblemsToggleScope => "problems_toggle_scope",
            Command::DiffToggleIgnoreWhitespace => "diff_toggle_ignore_whitespace",
            Command::NewTerminal => "new_terminal",
            Command::FocusNextTerminal => "focus_next_terminal",
            Command::FocusPreviousTerminal => "focus_previous_terminal",
            Command::StartDebugging => "start_debugging",
            Command::SelectDebugConfig => "select_debug_config",
            Command::AddDebugConfig => "add_debug_config",
            Command::StopDebugging => "stop_debugging",
            Command::PauseDebugging => "pause_debugging",
            Command::SwitchDebugSession => "switch_debug_session",
            Command::RestartDebugging => "restart_debugging",
            Command::ToggleBreakpoint => "toggle_breakpoint",
            Command::EditBreakpointCondition => "edit_breakpoint_condition",
            Command::EditLogpoint => "edit_logpoint",
            Command::ShowIncomingCalls => "show_incoming_calls",
            Command::ShowOutgoingCalls => "show_outgoing_calls",
            Command::ShowSupertypes => "show_supertypes",
            Command::ShowSubtypes => "show_subtypes",
            Command::ToggleCodeLens => "toggle_code_lens",
            Command::RunCodeLens => "run_code_lens",
            Command::DebugAddHitCountBreakpoint => "debug_add_hit_count_breakpoint",
            Command::DebugAddFunctionBreakpoint => "debug_add_function_breakpoint",
            Command::DebugRemoveFunctionBreakpoints => "debug_remove_function_breakpoints",
            Command::DebugRunToCursor => "debug_run_to_cursor",
            Command::DebugBreakOnValueChange => "debug_break_on_value_change",
            Command::DebugRemoveDataBreakpoints => "debug_remove_data_breakpoints",
            Command::OpenSearchEditor => "open_search_editor",
            Command::RerunSearchEditor => "rerun_search_editor",
            Command::RebaseAbort => "rebase_abort",
            Command::ToggleTerminalSuggestions => "toggle_terminal_suggestions",
            Command::ToggleScreenReader => "toggle_screen_reader",
            Command::OpenKeyboardShortcuts => "open_keyboard_shortcuts",
            Command::SwitchProfile => "switch_profile",
            Command::ToggleInlineSuggestions => "toggle_inline_suggestions",
            Command::StepOver => "step_over",
            Command::ToggleRaisedExceptions => "toggle_raised_exceptions",
            Command::AttachPythonProcess => "attach_python_process",
            Command::ColorTheme => "color_theme",
            Command::KeyboardShortcuts => "keyboard_shortcuts",
            Command::UpdateCroft => "update_croft",
            Command::OpenSettings => "open_settings",
            Command::OpenSettingsEditor => "open_settings_editor",
            Command::OpenSettingsJson => "open_settings_json",
            Command::OpenWorkspaceSettingsJson => "open_workspace_settings_json",
            Command::OpenWorkspaceSettingsLocalJson => "open_workspace_settings_local_json",
            Command::OpenKeybindingsJson => "open_keybindings_json",
            Command::ConfigureSnippets => "configure_snippets",
            Command::OpenTriggersJson => "open_triggers_json",
            Command::OpenAgentsJson => "open_agents_json",
            Command::OpenMatchersJson => "open_matchers_json",
            Command::ToggleTerminalTimestamps => "toggle_terminal_timestamps",
            Command::ToggleLogHighlight => "toggle_log_highlight",
            Command::CollapseTerminalPane => "collapse_terminal_pane",
            Command::RestoreTerminalPanes => "restore_terminal_panes",
            Command::ToggleSecretRedaction => "toggle_secret_redaction",
            Command::RevealRedactedSecrets => "reveal_redacted_secrets",
            Command::SearchFromTerminal => "search_from_terminal",
            Command::SessionParticipants => "session_participants",
            Command::SessionDetach => "session_detach",
            Command::CollabCancelStream => "collab_cancel_stream",
            Command::AskNavigator => "navigator_ask",
            Command::AskNavigatorAboutCapture => "captures_ask_navigator",
            Command::OpenWorkspaceOnSshHost => "remote_open_workspace_on_ssh_host",
            Command::ScrubHistory => "scm_scrub_history",
            Command::DebugInstallDelve => "debug_install_delve",
            Command::RemoteSyncConfigNow => "remote_sync_config_now",
            Command::StopAutoApprove => "agents_stop_auto_approve",
            Command::ScrubOpenHere => "scm_scrub_open_here",
            Command::ScrubDiffToWorkingTree => "scm_scrub_diff_working_tree",
            Command::FleetRun => "terminal_fleet_run",
            Command::OpenAsSymbolTab => "editor_open_symbol_tab",
            Command::LoadReviewThreads => "review_load_threads",
            Command::ReviewAddComment => "review_add_comment",
            Command::ReviewSubmit => "review_submit",
            Command::ReviewToggleResolved => "review_toggle_resolved",
            Command::ReviewDiscardPending => "review_discard_pending",
            Command::ExportCommentsToPr => "scm_export_comments_to_pr",
            Command::NoteAdd => "note_add",
            Command::NoteDelete => "note_delete",
            Command::ToggleSessionRecording => "session_toggle_recording",
            Command::NewWorktreeLane => "agent_new_worktree_lane",
            Command::ReviewPullRequest => "review_pull_request",
            Command::CloseWorktreeLane => "agent_close_worktree_lane",
            Command::DiffWorktreeLane => "agent_diff_worktree_lane",
            Command::MarkAgentLaneReviewed => "agents_mark_reviewed",
            Command::ShowAgentLane => "agents_show_lane",
            Command::OpenAgentLaneSection => "agents_open_lane",
            Command::MarkAgentFileReviewed => "agents_mark_file_reviewed",
            Command::DiffAgentFileSinceReview => "agents_diff_since_review",
            Command::PickAgentLaneFile => "agents_pick_changed_file",
            Command::FixProblemWithNavigator => "problems_fix_navigator",
            Command::SendHttpRequest => "http_send_request",
            Command::CopyHttpRequestAsCurl => "http_copy_curl",
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

    /// An extra word the palette matches this command on, beyond its
    /// title: "layout" for the Customize Layout commands (#852), whose
    /// VS Code names mostly don't say it, and the tab menu's "Keep Open"
    /// for View: Keep Editor. Empty for the rest.
    pub fn keyword(self) -> &'static str {
        if LAYOUT_COMMANDS.contains(&self) {
            "layout"
        } else if self == Command::KeepEditor {
            "keep open"
        } else {
            ""
        }
    }

    /// Resolve a `keybindings.json` command id back to its [`Command`]. Returns
    /// `None` for an unknown id so a typo in the user's config is ignored rather
    /// than fatal.
    pub fn from_id(id: &str) -> Option<Command> {
        ALL_COMMANDS.iter().copied().find(|c| c.id() == id)
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
    ("Cmd+Z in Explorer", "Super+Z in Explorer"),
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
    pub fn keybinding_hint(&self) -> &'static str {
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
    /// Built-ins left out of the list because they do not apply right now,
    /// VS Code's `when` clause for its palette (#852: Pin Editor on a pinned
    /// tab). They stay commands: keybindings.json and the Keyboard
    /// Shortcuts editor still reach them.
    pub hidden: Vec<Command>,
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
            hidden: Vec::new(),
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

    /// Leave `hidden` out of the list (see [`CommandPalette::hidden`]),
    /// then re-rank. Called by the app when opening the palette.
    pub fn set_hidden(&mut self, hidden: Vec<Command>) {
        self.hidden = hidden;
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
    /// injected extension commands, less the `hidden` built-ins. An empty
    /// query shows every command in
    /// declaration order (built-ins first, then extensions); otherwise rows are
    /// kept only when their lower-cased title fuzzy-matches the needle, ranked
    /// by score (best first), ties broken by declaration order for stability.
    /// Built-ins occupy the low index range so they win ties against extensions.
    fn refresh_results(&mut self) {
        let all: Vec<PaletteItem> = ALL_COMMANDS
            .iter()
            .filter(|c| !self.hidden.contains(c))
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
                // Match the English title and the translated one (#621), so
                // a query in either language finds the command.
                let title_lower = item.title().to_lowercase();
                let shown = crate::i18n::tr(item.title()).to_lowercase();
                let english = fuzzy_score(&needle, &title_lower, 0);
                let local = (shown != title_lower)
                    .then(|| fuzzy_score(&needle, &shown, 0))
                    .flatten();
                // A search word the title lacks, e.g. "layout" (#852).
                let keyword = match &item {
                    PaletteItem::Builtin(c) if !c.keyword().is_empty() => {
                        fuzzy_score(&needle, c.keyword(), 0)
                    }
                    _ => None,
                };
                english
                    .max(local)
                    .max(keyword)
                    .map(|score| (score, idx, item))
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
    let width = width.clamp(40, 100).min(area.width);
    let height = area.height.saturating_mul(6) / 10;
    let height = height.max(10).min(area.height);
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
        let title = crate::i18n::tr(cmd.title());
        let hint = platform_hint(cmd.keybinding_hint());
        // Right-align the keybinding hint: pad between the title and the hint
        // so the chord sits at the row's right edge, like VS Code.
        let used = 2 + title.chars().count() + hint.chars().count();
        let pad = (list_rect.width as usize).saturating_sub(used).max(1);
        let spans: Vec<Span<'static>> = vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(title.to_string(), row_style),
            Span::styled(" ".repeat(pad), row_style),
            // Borrowed as written unless the platform respells it.
            Span::styled(hint, hint_style),
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

    /// #865: the palette names chords in its macOS spelling, `Cmd` where
    /// macOS has Cmd, as the other Cmd hints do, so the updater reads
    /// `Cmd+Shift+F9` here; the status bar picks each platform's own.
    #[test]
    fn update_croft_is_reachable_and_shows_its_chord_in_the_palettes_spelling() {
        let mut palette = CommandPalette::new();
        palette.set_query("update croft");
        assert!(
            palette.results.contains(&builtin(Command::UpdateCroft)),
            "{:?}",
            palette.results
        );
        assert_eq!(Command::UpdateCroft.keybinding_hint(), "Cmd+Shift+F9");
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

    /// #852 negative: the "layout" search word reaches the Customize Layout
    /// commands only; an unrelated command whose title lacks the word is not
    /// pulled in, and a title query still ranks by the title alone.
    #[test]
    fn the_layout_keyword_finds_only_the_layout_commands() {
        let mut palette = CommandPalette::new();
        palette.set_query("layout");
        for c in LAYOUT_COMMANDS {
            assert!(palette.results.contains(&builtin(*c)), "{c:?}");
        }
        for c in [Command::SaveAll, Command::ShowProblems, Command::SaveFile] {
            assert!(!palette.results.contains(&builtin(c)), "{c:?}");
        }
        palette.set_query("status bar");
        assert_eq!(
            palette.results.first(),
            Some(&builtin(Command::ToggleStatusBar))
        );
        assert!(!palette.results.contains(&builtin(Command::ToggleZenMode)));
    }

    /// #852 (comment 3): each editor-tab action is in the palette under
    /// VS Code's title, as the top row for that title, showing the chord
    /// croft has for it (Close Others has none: the tab menu's ⌥⌘T is not a
    /// chord croft binds).
    #[test]
    fn the_editor_tab_commands_are_found_under_their_vscode_titles() {
        let mut palette = CommandPalette::new();
        for (title, chord) in [
            ("View: Close Other Editors in Group", ""),
            ("View: Close Editors to the Right in Group", "Cmd+K →"),
            ("View: Close Saved Editors in Group", "Cmd+K U"),
            ("View: Close All Editors", "Cmd+K W"),
            ("View: Pin Editor", "Cmd+K P"),
            ("View: Unpin Editor", "Cmd+K P"),
            ("View: Keep Editor", "Cmd+K Shift+P"),
        ] {
            palette.set_query(title);
            let top = palette.results.first();
            assert_eq!(
                top.map(PaletteItem::title),
                Some(title),
                "the palette's top row for {title:?}"
            );
            assert_eq!(
                top.map(PaletteItem::keybinding_hint),
                Some(chord),
                "{title:?}"
            );
        }
    }

    /// #852 (comment 3): the tab menu's own labels find the same commands,
    /// so a query in the menu's words ("keep open") is not a dead end.
    #[test]
    fn the_tab_menu_labels_find_the_editor_tab_commands() {
        let mut palette = CommandPalette::new();
        for (label, title) in [
            ("close others", "View: Close Other Editors in Group"),
            (
                "close to the right",
                "View: Close Editors to the Right in Group",
            ),
            ("close saved", "View: Close Saved Editors in Group"),
            ("close all", "View: Close All Editors"),
            ("pin", "View: Pin Editor"),
            ("unpin", "View: Unpin Editor"),
            ("keep open", "View: Keep Editor"),
        ] {
            palette.set_query(label);
            let titles: Vec<&str> = palette.results.iter().map(PaletteItem::title).collect();
            assert!(
                titles.contains(&title),
                "{label:?} must find {title:?}; it found {titles:?}"
            );
        }
    }

    #[test]
    fn every_command_has_a_nonempty_title() {
        for cmd in ALL_COMMANDS {
            assert!(!cmd.title().is_empty(), "{cmd:?} has empty title");
        }
    }

    /// #843: off macOS the command modifier is `Ctrl` (LINUX.md), so the row
    /// for Toggle Line Comment shows `Ctrl+/`, not the macOS `Cmd+/` its hint
    /// is written with.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_palette_row_spells_cmd_as_ctrl_off_macos() {
        let mut palette = CommandPalette::new();
        palette.set_query("toggle line comment");
        assert_eq!(
            palette.results.first(),
            Some(&builtin(Command::ToggleLineComment))
        );
        let area = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(area);
        render_command_palette(
            &mut palette,
            area,
            &mut buf,
            crate::theme::Theme::default(),
            false,
        );
        let row = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .find(|line| line.contains("Toggle Line Comment"))
            .expect("the row is painted");
        assert!(row.contains("Ctrl+/"), "{row:?}");
        assert!(!row.contains("Cmd"), "{row:?}");
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
            ("Cmd+Z in Explorer", "Super+Z in Explorer"),
        ] {
            assert_eq!(hint_for_platform(mac, false), linux);
        }
    }
}
