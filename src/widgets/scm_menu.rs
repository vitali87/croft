//! The Source Control "⋯" title menu: a VS Code-style actions menu with
//! one level of fly-out submenus (Commit ›, Changes ›, Branch ›, Stash ›,
//! …). Opened from the `⋯` pill in the SOURCE CONTROL header.
//!
//! The widget is a thin renderer over a static menu tree. It owns no git
//! state: every leaf is an [`ScmAction`] the App dispatches in
//! `dispatch_scm_action`, and the open/expanded state plus hit-test rects
//! live in [`ScmMenuState`] (held by the App, mirroring how the commit
//! dropdown stores its rects on the panel).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::{Clear, Widget},
};

/// Every leaf action reachable from the SCM "⋯" menu. The App maps each to
/// a handler (some fire immediately, some open an input modal or picker).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScmAction {
    // Top-level leaves.
    Pull,
    Push,
    Clone,
    CheckoutTo,
    Fetch,
    ShowGitOutput,
    // Commit submenu.
    Commit,
    CommitStaged,
    CommitAll,
    CommitAmend,
    CommitAndPush,
    CommitAndSync,
    // Changes submenu.
    StageAll,
    UnstageAll,
    DiscardAll,
    // Pull, Push submenu.
    Sync,
    PullRebase,
    PushTo,
    PushForce,
    PublishBranch,
    // Branch submenu.
    CreateBranch,
    CreateBranchFrom,
    RenameBranch,
    DeleteBranch,
    Merge,
    Rebase,
    // Remote submenu.
    AddRemote,
    RemoveRemote,
    // Stash submenu.
    Stash,
    StashIncludeUntracked,
    StashStaged,
    ApplyStash,
    PopStashLatest,
    PopStashPick,
    DropStash,
    // Tags submenu.
    CreateTag,
    DeleteTag,
}

/// A leaf row: the action it fires, its label, and its codicon glyph.
#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub action: ScmAction,
    pub label: &'static str,
    pub icon: char,
}

const fn leaf(action: ScmAction, label: &'static str, icon: char) -> Leaf {
    Leaf {
        action,
        label,
        icon,
    }
}

/// A top-level row: either a leaf, a separator, or a submenu (label + icon
/// + its leaf children).
pub enum Node {
    Item(Leaf),
    Sep,
    Sub {
        label: &'static str,
        icon: char,
        items: &'static [Leaf],
    },
}

// Codicon glyphs (Nerd Fonts `cod-*` PUA, verified against
// ryanoasis/nerd-fonts glyphnames.json v3.4.0 on 2026-06-19).
const ICON_CLOUD_DOWNLOAD: char = '\u{eac2}'; // pull
const ICON_CLOUD_UPLOAD: char = '\u{eac3}'; // push
const ICON_REPO_CLONE: char = '\u{eb3e}'; // clone
const ICON_GIT_BRANCH: char = '\u{ea96}'; // checkout / branch
const ICON_GIT_FETCH: char = '\u{ec1d}'; // fetch
const ICON_OUTPUT: char = '\u{eb9d}'; // show git output
const ICON_CHECK: char = '\u{eab2}'; // commit
const ICON_SYNC: char = '\u{ea77}'; // sync
const ICON_ADD: char = '\u{ea60}'; // add / stage
const ICON_REMOVE: char = '\u{eb3b}'; // unstage
const ICON_DISCARD: char = '\u{eae2}'; // discard
const ICON_ARROW_UP: char = '\u{eaa1}'; // push variants
const ICON_EDIT: char = '\u{ea73}'; // rename
const ICON_TRASH: char = '\u{ea81}'; // delete
const ICON_GIT_MERGE: char = '\u{eafe}'; // merge
const ICON_GIT_PR: char = '\u{ea64}'; // rebase
const ICON_BROADCAST: char = '\u{eaad}'; // remote
const ICON_ARCHIVE: char = '\u{ea98}'; // stash
const ICON_TAG: char = '\u{ea66}'; // tags
const ICON_NEW_FOLDER: char = '\u{ea80}'; // create from

/// The codicon painted on the `⋯` header pill that opens this menu.
pub const ICON_ELLIPSIS: char = '\u{ea7c}';

const COMMIT_ITEMS: &[Leaf] = &[
    leaf(ScmAction::Commit, "Commit", ICON_CHECK),
    leaf(ScmAction::CommitStaged, "Commit Staged", ICON_CHECK),
    leaf(ScmAction::CommitAll, "Commit All", ICON_CHECK),
    leaf(ScmAction::CommitAmend, "Commit (Amend)", ICON_EDIT),
    leaf(ScmAction::CommitAndPush, "Commit & Push", ICON_CLOUD_UPLOAD),
    leaf(ScmAction::CommitAndSync, "Commit & Sync", ICON_SYNC),
];

const CHANGES_ITEMS: &[Leaf] = &[
    leaf(ScmAction::StageAll, "Stage All Changes", ICON_ADD),
    leaf(ScmAction::UnstageAll, "Unstage All Changes", ICON_REMOVE),
    leaf(ScmAction::DiscardAll, "Discard All Changes", ICON_DISCARD),
];

const PULL_PUSH_ITEMS: &[Leaf] = &[
    leaf(ScmAction::Sync, "Sync", ICON_SYNC),
    leaf(ScmAction::PullRebase, "Pull (Rebase)", ICON_CLOUD_DOWNLOAD),
    leaf(ScmAction::PushTo, "Push to…", ICON_ARROW_UP),
    leaf(ScmAction::PushForce, "Push (Force)", ICON_ARROW_UP),
    leaf(
        ScmAction::PublishBranch,
        "Publish Branch",
        ICON_CLOUD_UPLOAD,
    ),
];

const BRANCH_ITEMS: &[Leaf] = &[
    leaf(ScmAction::CreateBranch, "Create Branch…", ICON_GIT_BRANCH),
    leaf(
        ScmAction::CreateBranchFrom,
        "Create Branch from…",
        ICON_NEW_FOLDER,
    ),
    leaf(ScmAction::RenameBranch, "Rename Branch…", ICON_EDIT),
    leaf(ScmAction::DeleteBranch, "Delete Branch…", ICON_TRASH),
    leaf(ScmAction::Merge, "Merge…", ICON_GIT_MERGE),
    leaf(ScmAction::Rebase, "Rebase…", ICON_GIT_PR),
];

const REMOTE_ITEMS: &[Leaf] = &[
    leaf(ScmAction::AddRemote, "Add Remote…", ICON_ADD),
    leaf(ScmAction::RemoveRemote, "Remove Remote…", ICON_TRASH),
];

const STASH_ITEMS: &[Leaf] = &[
    leaf(ScmAction::Stash, "Stash", ICON_ARCHIVE),
    leaf(
        ScmAction::StashIncludeUntracked,
        "Stash (Include Untracked)",
        ICON_ARCHIVE,
    ),
    leaf(ScmAction::StashStaged, "Stash Staged", ICON_ARCHIVE),
    leaf(ScmAction::ApplyStash, "Apply Stash…", ICON_ARCHIVE),
    leaf(ScmAction::PopStashLatest, "Pop Latest Stash", ICON_ARCHIVE),
    leaf(ScmAction::PopStashPick, "Pop Stash…", ICON_ARCHIVE),
    leaf(ScmAction::DropStash, "Drop Stash…", ICON_TRASH),
];

const TAGS_ITEMS: &[Leaf] = &[
    leaf(ScmAction::CreateTag, "Create Tag…", ICON_TAG),
    leaf(ScmAction::DeleteTag, "Delete Tag…", ICON_TRASH),
];

/// The full top-level menu, mirroring VS Code's SCM "⋯" menu order.
pub fn menu() -> Vec<Node> {
    vec![
        Node::Item(leaf(ScmAction::Pull, "Pull", ICON_CLOUD_DOWNLOAD)),
        Node::Item(leaf(ScmAction::Push, "Push", ICON_CLOUD_UPLOAD)),
        Node::Item(leaf(ScmAction::Clone, "Clone…", ICON_REPO_CLONE)),
        Node::Item(leaf(ScmAction::CheckoutTo, "Checkout to…", ICON_GIT_BRANCH)),
        Node::Item(leaf(ScmAction::Fetch, "Fetch", ICON_GIT_FETCH)),
        Node::Sep,
        Node::Sub {
            label: "Commit",
            icon: ICON_CHECK,
            items: COMMIT_ITEMS,
        },
        Node::Sub {
            label: "Changes",
            icon: ICON_ADD,
            items: CHANGES_ITEMS,
        },
        Node::Sub {
            label: "Pull, Push",
            icon: ICON_SYNC,
            items: PULL_PUSH_ITEMS,
        },
        Node::Sub {
            label: "Branch",
            icon: ICON_GIT_BRANCH,
            items: BRANCH_ITEMS,
        },
        Node::Sub {
            label: "Remote",
            icon: ICON_BROADCAST,
            items: REMOTE_ITEMS,
        },
        Node::Sub {
            label: "Stash",
            icon: ICON_ARCHIVE,
            items: STASH_ITEMS,
        },
        Node::Sub {
            label: "Tags",
            icon: ICON_TAG,
            items: TAGS_ITEMS,
        },
        Node::Sep,
        Node::Item(leaf(
            ScmAction::ShowGitOutput,
            "Show Git Output",
            ICON_OUTPUT,
        )),
    ]
}

/// What a click on a top-level row resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopHit {
    /// A leaf row: fire this action and close.
    Action(ScmAction),
    /// A submenu row: toggle its fly-out (this is its top-level index).
    Expand(usize),
}

/// Open/closed + expansion state and the hit-test rects, owned by the App.
#[derive(Default)]
pub struct ScmMenuState {
    pub open: bool,
    /// Index (into `menu()`) of the submenu whose fly-out is showing.
    pub expanded: Option<usize>,
    pub top_hits: Vec<(Rect, TopHit)>,
    pub sub_hits: Vec<(Rect, ScmAction)>,
}

impl ScmMenuState {
    pub fn close(&mut self) {
        self.open = false;
        self.expanded = None;
        self.top_hits.clear();
        self.sub_hits.clear();
    }

    /// Resolve a click at (x, y) to a menu outcome, mutating expansion
    /// state for submenu toggles. Returns `Some(action)` only when a leaf
    /// fired (the caller then dispatches and closes). Returns `None` for a
    /// submenu toggle (menu stays open) or a miss.
    pub fn click(&mut self, x: u16, y: u16) -> ClickOutcome {
        for (rect, action) in &self.sub_hits {
            if hit(*rect, x, y) {
                return ClickOutcome::Fire(*action);
            }
        }
        for (rect, top) in &self.top_hits {
            if hit(*rect, x, y) {
                return match top {
                    TopHit::Action(a) => ClickOutcome::Fire(*a),
                    TopHit::Expand(i) => {
                        self.expanded = if self.expanded == Some(*i) {
                            None
                        } else {
                            Some(*i)
                        };
                        ClickOutcome::Toggled
                    }
                };
            }
        }
        ClickOutcome::Missed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickOutcome {
    Fire(ScmAction),
    Toggled,
    Missed,
}

fn hit(rect: Rect, x: u16, y: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.x + rect.width
        && y >= rect.y
        && y < rect.y + rect.height
}

const BG: Color = Color::Rgb(0x25, 0x2b, 0x37);
const SUB_BG: Color = Color::Rgb(0x2b, 0x32, 0x3f);
const FG: Color = Color::Rgb(0xe6, 0xe9, 0xef);
const SEP_FG: Color = Color::Rgb(0x3b, 0x42, 0x52);
const SUB_ARROW_FG: Color = Color::Rgb(0x9a, 0xa4, 0xb2);

/// Width a column needs to show the longest of `labels` as
/// `[pad][icon][gap][label][gap][›][pad]`.
fn column_width(labels: impl Iterator<Item = usize>, with_arrow: bool) -> u16 {
    let arrow = if with_arrow { 2 } else { 0 };
    let max = labels.max().unwrap_or(0) as u16;
    // 1 pad + icon + 1 gap + label + arrow + 1 pad
    (1 + 1 + 1 + max + arrow + 1).max(12)
}

/// Paint the menu anchored under `anchor` within `screen`, recording hit
/// rects into `state`. The submenu fly-out renders to the right of the top
/// column (or to its left when there is no room).
pub fn render(state: &mut ScmMenuState, anchor: Rect, screen: Rect, buf: &mut Buffer) {
    state.top_hits.clear();
    state.sub_hits.clear();
    let nodes = menu();

    let top_w = column_width(
        nodes.iter().filter_map(|n| match n {
            Node::Item(l) => Some(l.label.chars().count()),
            Node::Sub { label, .. } => Some(label.chars().count()),
            Node::Sep => None,
        }),
        true,
    );
    let top_h = nodes.len() as u16;
    let top_x = anchor.x.min(screen.x + screen.width.saturating_sub(top_w));
    let top_y = (anchor.y + anchor.height).min(screen.y + screen.height.saturating_sub(top_h));
    let top_rect = Rect {
        x: top_x,
        y: top_y,
        width: top_w,
        height: top_h.min(screen.height),
    };
    Widget::render(Clear, top_rect, buf);
    fill(buf, top_rect, BG);

    for (i, node) in nodes.iter().enumerate() {
        let row_y = top_rect.y + i as u16;
        if row_y >= screen.y + screen.height {
            break;
        }
        let row = Rect {
            x: top_rect.x,
            y: row_y,
            width: top_rect.width,
            height: 1,
        };
        match node {
            Node::Sep => {
                for x in row.x + 1..row.x + row.width.saturating_sub(1) {
                    buf.set_string(x, row_y, "─", Style::default().fg(SEP_FG).bg(BG));
                }
            }
            Node::Item(l) => {
                paint_row(buf, row, l.icon, l.label, false, BG);
                state.top_hits.push((row, TopHit::Action(l.action)));
            }
            Node::Sub { label, icon, .. } => {
                let is_open = state.expanded == Some(i);
                let row_bg = if is_open { SUB_BG } else { BG };
                paint_row(buf, row, *icon, label, true, row_bg);
                state.top_hits.push((row, TopHit::Expand(i)));
            }
        }
    }

    // Fly-out submenu column.
    if let Some(i) = state.expanded
        && let Some(Node::Sub { items, .. }) = nodes.get(i)
    {
        let sub_w = column_width(items.iter().map(|l| l.label.chars().count()), false);
        let parent_y = top_rect.y + i as u16;
        // Prefer the right of the top column; fall back to its left.
        let right_x = top_rect.x + top_rect.width;
        let sub_x = if right_x + sub_w <= screen.x + screen.width {
            right_x
        } else {
            top_rect.x.saturating_sub(sub_w)
        };
        let sub_h = items.len() as u16;
        let sub_y = parent_y.min(screen.y + screen.height.saturating_sub(sub_h));
        let sub_rect = Rect {
            x: sub_x,
            y: sub_y,
            width: sub_w,
            height: sub_h.min(screen.height),
        };
        Widget::render(Clear, sub_rect, buf);
        fill(buf, sub_rect, SUB_BG);
        for (j, l) in items.iter().enumerate() {
            let row_y = sub_rect.y + j as u16;
            if row_y >= screen.y + screen.height {
                break;
            }
            let row = Rect {
                x: sub_rect.x,
                y: row_y,
                width: sub_rect.width,
                height: 1,
            };
            paint_row(buf, row, l.icon, l.label, false, SUB_BG);
            state.sub_hits.push((row, l.action));
        }
    }
}

fn fill(buf: &mut Buffer, rect: Rect, bg: Color) {
    let style = Style::default().bg(bg);
    for ry in 0..rect.height {
        for rx in 0..rect.width {
            buf[(rect.x + rx, rect.y + ry)]
                .set_symbol(" ")
                .set_style(style);
        }
    }
}

fn paint_row(buf: &mut Buffer, row: Rect, icon: char, label: &str, submenu: bool, bg: Color) {
    let fg = Style::default().fg(FG).bg(bg);
    buf.set_string(row.x + 1, row.y, icon.to_string(), fg);
    let label_x = row.x + 3;
    let avail = row.width.saturating_sub(3 + if submenu { 2 } else { 1 }) as usize;
    let shown: String = label.chars().take(avail).collect();
    buf.set_string(label_x, row.y, &shown, fg);
    if submenu && row.width >= 2 {
        buf.set_string(
            row.x + row.width - 2,
            row.y,
            "›",
            Style::default().fg(SUB_ARROW_FG).bg(bg),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_has_the_expected_top_level_shape() {
        let m = menu();
        // 5 leaves + sep + 7 submenus + sep + 1 leaf = 15 nodes.
        assert_eq!(m.len(), 15);
        let subs = m.iter().filter(|n| matches!(n, Node::Sub { .. })).count();
        assert_eq!(subs, 7, "Commit/Changes/Pull-Push/Branch/Remote/Stash/Tags");
    }

    #[test]
    fn clicking_a_submenu_row_toggles_its_flyout_without_firing() {
        let mut st = ScmMenuState::default();
        let screen = Rect::new(0, 0, 120, 40);
        let anchor = Rect::new(10, 0, 2, 1);
        let mut buf = Buffer::empty(screen);
        st.open = true;
        render(&mut st, anchor, screen, &mut buf);
        // Find the "Branch" submenu's top-level index and its rect.
        let branch_idx = menu()
            .iter()
            .position(|n| matches!(n, Node::Sub { label, .. } if *label == "Branch"))
            .unwrap();
        let (rect, _) = st
            .top_hits
            .iter()
            .find(|(_, t)| *t == TopHit::Expand(branch_idx))
            .copied()
            .unwrap();
        let outcome = st.click(rect.x, rect.y);
        assert_eq!(outcome, ClickOutcome::Toggled);
        assert_eq!(st.expanded, Some(branch_idx));
    }

    #[test]
    fn clicking_a_leaf_fires_its_action() {
        let mut st = ScmMenuState::default();
        let screen = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(screen);
        st.open = true;
        render(&mut st, Rect::new(10, 0, 2, 1), screen, &mut buf);
        let (rect, _) = st
            .top_hits
            .iter()
            .find(|(_, t)| *t == TopHit::Action(ScmAction::Pull))
            .copied()
            .unwrap();
        assert_eq!(
            st.click(rect.x, rect.y),
            ClickOutcome::Fire(ScmAction::Pull)
        );
    }

    #[test]
    fn expanded_submenu_exposes_its_leaves_as_hits() {
        let mut st = ScmMenuState::default();
        let screen = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(screen);
        st.open = true;
        let stash_idx = menu()
            .iter()
            .position(|n| matches!(n, Node::Sub { label, .. } if *label == "Stash"))
            .unwrap();
        st.expanded = Some(stash_idx);
        render(&mut st, Rect::new(10, 0, 2, 1), screen, &mut buf);
        assert!(
            st.sub_hits.iter().any(|(_, a)| *a == ScmAction::DropStash),
            "the expanded Stash submenu must expose Drop Stash as a hit target"
        );
    }
}
