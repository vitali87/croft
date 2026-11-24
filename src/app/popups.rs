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
//! yields to one of those only where it lands on the picture. Which pictures
//! that is, the frame tells: [`PictureCover`] notes the cells under every
//! picture just before the frame draws its pop-ups, and again after, and a
//! picture whose cells changed in between has something drawn over it
//! (#2663).

use super::App;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

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

    /// Whether the pop-up handles the mouse itself: a picker's rows and the
    /// click outside that closes it, a dialog's buttons, or the New File
    /// box's own rule further down the mouse handler. One that does not
    /// takes every mouse event while it is open, so a click, the wheel or
    /// the pointer never reaches what it covers (#862, #2674). The match
    /// has no catch-all: a new pop-up must say which it is.
    pub(super) fn handles_mouse(self) -> bool {
        match self {
            Popup::Prompt
            | Popup::FileFinder
            | Popup::CommandPalette
            | Popup::GoToSymbol
            | Popup::WorkspaceSymbols
            | Popup::ProcessPicker
            | Popup::ZoxideJump
            | Popup::CommandHistory
            | Popup::BranchPicker
            | Popup::InputPrompt
            | Popup::ListPicker
            | Popup::ShortcutsModal
            | Popup::ConnectDialog => true,
            Popup::LocalOpenConfirm
            | Popup::DiscardConfirm
            | Popup::RevertHunkConfirm
            | Popup::DiscardAllConfirm
            | Popup::StashDropConfirm
            | Popup::UnsavedScmConfirm
            | Popup::ReplaceAllConfirm
            | Popup::BroadcastConfirm
            | Popup::TerminalPasteConfirm
            | Popup::RunBlockConfirm
            | Popup::UnsavedConfirm
            | Popup::LossySaveConfirm
            | Popup::SettingsEditor
            | Popup::Approval
            | Popup::TerminalWarning => false,
        }
    }
}

impl App {
    /// Whether every picture is held back this frame: a pop-up is open.
    /// iTerm2 and Sixel have no layer for pictures, so a picture sent after
    /// the frame would wipe the part of a pop-up it overlaps (#2659); Kitty
    /// keeps pictures below the text and loses nothing by the wait.
    pub(super) fn pictures_held_back(&self) -> bool {
        self.popup_open()
    }

    /// Whether one of the pop-ups drawn over the middle of the screen is
    /// open. It has the keys, so no pane's caret shows through it (#2669).
    /// Whether an open pop-up takes every mouse event, as one that handles
    /// no mouse of its own does (#2674).
    pub(super) fn popup_takes_the_mouse(&self) -> bool {
        Popup::ALL
            .iter()
            .any(|p| !p.handles_mouse() && p.is_open(self))
    }

    pub(super) fn popup_open(&self) -> bool {
        Popup::ALL.iter().any(|p| p.is_open(self))
    }

    /// Whether the picture at `spot` waits this frame: a pop-up is open, or
    /// something the frame drew after the panes landed on the picture.
    pub(super) fn picture_waits(&self, spot: PictureSpot) -> bool {
        self.pictures_held_back() || self.picture_cover.covered.contains(&spot)
    }

    /// Where each picture sits this frame, from the places the panes
    /// recorded while they painted. A picture not on screen has no spot or
    /// an empty one, which nothing can cover.
    pub(super) fn picture_spots(&self) -> Vec<(PictureSpot, Rect)> {
        let o = &self.overlays;
        let mut spots = Vec::with_capacity(9);
        for side in 0..2 {
            if let Some(l) = o.editor[side].layout() {
                let r = Rect::new(l.cell_x, l.cell_y, l.cell_w, l.cell_h);
                spots.push((PictureSpot::Editor(side), r));
            }
        }
        if let Some(l) = o.terminal_image.layout() {
            let r = Rect::new(l.cell_x, l.cell_y, l.cell_w, l.cell_h);
            spots.push((PictureSpot::Terminal, r));
        }
        if let Some(l) = o.markdown_image.layout() {
            let r = Rect::new(l.cell_x, l.cell_y, l.cell_w, l.cell_h);
            spots.push((PictureSpot::Markdown, r));
        }
        spots.push((PictureSpot::Minimap, self.minimap_img_rect));
        if let Some(l) = o.welcome.layout() {
            let r = Rect::new(l.cell_x, l.cell_y, l.cell_w, l.cell_h);
            spots.push((PictureSpot::Welcome, r));
        }
        if let Some((x, y)) = self.run_debug.last_icon_cell {
            use crate::widgets::run_debug::{RUN_DEBUG_ICON_CELLS_H, RUN_DEBUG_ICON_CELLS_W};
            let r = Rect::new(x, y, RUN_DEBUG_ICON_CELLS_W, RUN_DEBUG_ICON_CELLS_H);
            spots.push((PictureSpot::RunDebugIcon, r));
        }
        if let Some((x, y)) = self.problems_badge_cell {
            let w = crate::iterm2_inline::count_badge_cells_w(self.problems_badge_count);
            spots.push((PictureSpot::ProblemsBadge, Rect::new(x, y, w, 1)));
        }
        spots.push((
            PictureSpot::SourceControlHero,
            self.source_control.last_hero_area,
        ));
        if let Some((x, y)) = self.remote.last_image_cell {
            use crate::widgets::remote::{SSH_EMPTY_STATE_CELLS_H, SSH_EMPTY_STATE_CELLS_W};
            let r = Rect::new(x, y, SSH_EMPTY_STATE_CELLS_W, SSH_EMPTY_STATE_CELLS_H);
            spots.push((PictureSpot::SshIllustration, r));
        }
        spots
    }

    /// Called just before the frame draws its pop-ups.
    pub(super) fn note_pictures_before_popups(&mut self, buf: &Buffer) {
        let spots = self.picture_spots();
        self.picture_cover.note_before(buf, spots);
    }

    /// Called once the frame has drawn its last pop-up.
    pub(super) fn note_pictures_after_popups(&mut self, buf: &Buffer) {
        self.picture_cover.note_after(buf);
    }
}

/// A place croft sends a picture to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PictureSpot {
    /// An image or PDF in the left (0) or right (1) editor group.
    Editor(usize),
    Terminal,
    Markdown,
    Minimap,
    Welcome,
    RunDebugIcon,
    ProblemsBadge,
    SourceControlHero,
    SshIllustration,
}

/// What lay under each picture before the frame drew its pop-ups, and which
/// pictures something was then drawn over (#2663).
#[derive(Default)]
pub(super) struct PictureCover {
    before: Vec<(PictureSpot, Rect, u64)>,
    covered: Vec<PictureSpot>,
}

impl PictureCover {
    /// Forget the last frame's answer; a frame that never reaches its
    /// pop-ups covers nothing.
    pub(super) fn start_frame(&mut self) {
        self.before.clear();
        self.covered.clear();
    }

    fn note_before(&mut self, buf: &Buffer, spots: Vec<(PictureSpot, Rect)>) {
        self.before.clear();
        self.covered.clear();
        for (spot, r) in spots {
            if r.width > 0 && r.height > 0 {
                let print =
                    super::inline_images::cells_fingerprint(buf, r.x, r.y, r.width, r.height);
                self.before.push((spot, r, print));
            }
        }
    }

    fn note_after(&mut self, buf: &Buffer) {
        self.covered = self
            .before
            .iter()
            .filter(|(_, r, print)| {
                super::inline_images::cells_fingerprint(buf, r.x, r.y, r.width, r.height) != *print
            })
            .map(|(spot, _, _)| *spot)
            .collect();
    }
}
