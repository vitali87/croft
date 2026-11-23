//! Every pop-up croft draws over the middle of the screen, in one list
//! (#2062, card 9).
//!
//! Code that must act differently while a pop-up is open used to keep its
//! own list of `is_some()` checks, and the lists drifted apart. Six of them
//! decided whether pictures are held back, and between them they missed 16
//! pop-ups: on iTerm2 and Sixel a picture was drawn over the agent's
//! approval dialog, every confirm, the file prompt, the connect dialog and
//! Settings (#2659). A new pop-up is now added here, once, and every
//! picture waits for it.
//!
//! Menus, tooltips, notices, hover cards, the find bars and the tour caption
//! open next to what they belong to, so they are not listed: a picture
//! yields to one of those only where they overlap.

use super::App;

/// A pop-up drawn over the middle of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Popup {
    /// New File, New Folder, Rename and Save As.
    Prompt,
    LocalOpenConfirm,
    DiscardConfirm,
    RevertHunkConfirm,
    DiscardAllConfirm,
    StashDropConfirm,
    UnsavedScmConfirm,
    ReplaceAllConfirm,
    BroadcastConfirm,
    TerminalPasteConfirm,
    RunBlockConfirm,
    UnsavedConfirm,
    LossySaveConfirm,
    FileFinder,
    CommandPalette,
    GoToSymbol,
    WorkspaceSymbols,
    ProcessPicker,
    ZoxideJump,
    CommandHistory,
    BranchPicker,
    SettingsEditor,
    InputPrompt,
    ListPicker,
    ShortcutsModal,
    ConnectDialog,
    /// An agent's edit proposal.
    Approval,
    /// The unsupported-terminal warning at startup.
    TerminalWarning,
}

impl Popup {
    /// Every pop-up, in the order the frame paints them. Nothing reads the
    /// order yet.
    pub(super) const ALL: [Popup; 28] = [
        Popup::Prompt,
        Popup::LocalOpenConfirm,
        Popup::DiscardConfirm,
        Popup::RevertHunkConfirm,
        Popup::DiscardAllConfirm,
        Popup::StashDropConfirm,
        Popup::UnsavedScmConfirm,
        Popup::ReplaceAllConfirm,
        Popup::BroadcastConfirm,
        Popup::TerminalPasteConfirm,
        Popup::RunBlockConfirm,
        Popup::UnsavedConfirm,
        Popup::LossySaveConfirm,
        Popup::FileFinder,
        Popup::CommandPalette,
        Popup::GoToSymbol,
        Popup::WorkspaceSymbols,
        Popup::ProcessPicker,
        Popup::ZoxideJump,
        Popup::CommandHistory,
        Popup::BranchPicker,
        Popup::SettingsEditor,
        Popup::InputPrompt,
        Popup::ListPicker,
        Popup::ShortcutsModal,
        Popup::ConnectDialog,
        Popup::Approval,
        Popup::TerminalWarning,
    ];

    /// Whether this pop-up is open: the state its painter draws from.
    pub(super) fn is_open(self, app: &App) -> bool {
        match self {
            Popup::Prompt => app.prompt.is_some(),
            Popup::LocalOpenConfirm => app.pending_local_open.is_some(),
            Popup::DiscardConfirm => app.pending_discard.is_some(),
            Popup::RevertHunkConfirm => app.pending_revert_hunk.is_some(),
            Popup::DiscardAllConfirm => app.pending_discard_all,
            Popup::StashDropConfirm => app.pending_stash_drop.is_some(),
            Popup::UnsavedScmConfirm => app.pending_unsaved_scm.is_some(),
            Popup::ReplaceAllConfirm => app.pending_replace_all.is_some(),
            Popup::BroadcastConfirm => app.pending_broadcast_enable,
            Popup::TerminalPasteConfirm => app.pending_terminal_paste.is_some(),
            Popup::RunBlockConfirm => app.pending_run_block.is_some(),
            Popup::UnsavedConfirm => app.pending_unsaved.is_some(),
            Popup::LossySaveConfirm => app.pending_lossy_save.is_some(),
            Popup::FileFinder => app.file_finder.is_some(),
            Popup::CommandPalette => app.command_palette.is_some(),
            Popup::GoToSymbol => app.go_to_symbol.is_some(),
            Popup::WorkspaceSymbols => app.workspace_symbols.is_some(),
            Popup::ProcessPicker => app.process_picker.is_some(),
            Popup::ZoxideJump => app.zoxide_jump.is_some(),
            Popup::CommandHistory => app.command_history_popup.is_some(),
            Popup::BranchPicker => app.branch_picker.is_some(),
            Popup::SettingsEditor => app.settings_editor.is_some(),
            Popup::InputPrompt => app.input_prompt.is_some(),
            Popup::ListPicker => app.list_picker.is_some(),
            Popup::ShortcutsModal => app.shortcuts_modal.is_some(),
            Popup::ConnectDialog => app.connect_dialog.is_some(),
            Popup::Approval => app.approval_ui.is_some(),
            Popup::TerminalWarning => app.pending_terminal_warning,
        }
    }
}

impl App {
    /// Whether every picture is held back this frame: a pop-up is open.
    /// iTerm2 and Sixel have no layer for pictures, so a picture sent after
    /// the frame would wipe the part of a pop-up it overlaps (#2659); Kitty
    /// keeps pictures below the text and loses nothing by the wait.
    pub(super) fn pictures_held_back(&self) -> bool {
        Popup::ALL.iter().any(|p| p.is_open(self))
    }
}
