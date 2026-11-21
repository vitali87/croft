//! The Commit button's dropdown: its items in VS Code's order, their labels and icons, and
//! the clicks that open it and pick an item.

use super::*;

/// Items in the chevron-opened dropdown next to the Commit button.
/// Ordered top-to-bottom in render order; the click handler in `App`
/// dispatches on the variant returned by `click_commit_menu_item`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitMenuItem {
    CommitAndPush,
    Push,
    Pull,
    Sync,
    CheckoutBranch,
    Stash,
    StashPop,
    ViewStagedDiff,
    ViewPreviousCommitDiff,
    ViewDefaultBranchDiff,
}

impl CommitMenuItem {
    /// Ordered to mirror VS Code's Source Control "…" menu: the sync trio
    /// (push / pull / sync), then branch, then the stash pair, then the
    /// diff views.
    pub const ALL: [CommitMenuItem; 10] = [
        CommitMenuItem::CommitAndPush,
        CommitMenuItem::Push,
        CommitMenuItem::Pull,
        CommitMenuItem::Sync,
        CommitMenuItem::CheckoutBranch,
        CommitMenuItem::Stash,
        CommitMenuItem::StashPop,
        CommitMenuItem::ViewStagedDiff,
        CommitMenuItem::ViewPreviousCommitDiff,
        CommitMenuItem::ViewDefaultBranchDiff,
    ];

    /// Display label rendered in the dropdown. `default_branch` is the
    /// repo's default branch, inlined into the "View Changes vs <name>"
    /// label so the user sees exactly which branch is being compared.
    pub fn label(&self, default_branch: &str) -> String {
        match self {
            CommitMenuItem::CommitAndPush => "Commit & Push".to_string(),
            CommitMenuItem::Push => "Push".to_string(),
            CommitMenuItem::Pull => "Pull".to_string(),
            CommitMenuItem::Sync => "Sync (Pull, Push)".to_string(),
            CommitMenuItem::CheckoutBranch => "Checkout / Create Branch".to_string(),
            CommitMenuItem::Stash => "Stash".to_string(),
            CommitMenuItem::StashPop => "Pop Stash".to_string(),
            CommitMenuItem::ViewStagedDiff => "View Staged Changes".to_string(),
            CommitMenuItem::ViewPreviousCommitDiff => "View Changes vs previous".to_string(),
            CommitMenuItem::ViewDefaultBranchDiff => {
                format!("View Changes vs {default_branch}")
            }
        }
    }

    /// Leading codicon glyph painted before the label, mirroring VS Code's
    /// Source Control menu. Codepoints are Nerd Fonts `cod-*` PUA values
    /// (verified against ryanoasis/nerd-fonts glyphnames.json v3.4.0 on
    /// 2026-06-19), matching the cod-source-control (U+EA68) glyph already
    /// used by the branch row:
    ///   - cloud-upload (U+EAC3): commit then push to the remote
    ///   - arrow-up (U+EAA1): push to the remote
    ///   - cloud-download (U+EAC2): pull from the remote
    ///   - sync (U+EA77): pull then push in one step
    ///   - git-branch (U+EA96): checkout / create a branch
    ///   - archive (U+EA98): stash / pop the working tree
    ///   - diff (U+EAE1): the staged diff against HEAD
    ///   - git-compare (U+EAFD): a branch/commit comparison
    pub fn icon(&self) -> char {
        match self {
            CommitMenuItem::CommitAndPush => '\u{eac3}',
            CommitMenuItem::Push => '\u{eaa1}',
            CommitMenuItem::Pull => '\u{eac2}',
            CommitMenuItem::Sync => '\u{ea77}',
            CommitMenuItem::CheckoutBranch => '\u{ea96}',
            CommitMenuItem::Stash => '\u{ea98}',
            CommitMenuItem::StashPop => '\u{ea98}',
            CommitMenuItem::ViewStagedDiff => '\u{eae1}',
            CommitMenuItem::ViewPreviousCommitDiff => '\u{eafd}',
            CommitMenuItem::ViewDefaultBranchDiff => '\u{eafd}',
        }
    }
}

impl SourceControlPanel {
    pub fn click_commit_caret(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_commit_caret_area, x, y)
    }

    pub fn click_commit_menu_item(&self, x: u16, y: u16) -> Option<CommitMenuItem> {
        self.commit_menu_item_areas
            .iter()
            .find(|(rect, _)| rect_hit(*rect, x, y))
            .map(|(_, item)| *item)
    }
}
