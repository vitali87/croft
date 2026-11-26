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
    UndoLastCommit,
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
    /// Pick a stash and open its changes (#1581).
    ViewStash,
    // Tags submenu.
    CreateTag,
    DeleteTag,
    /// Push every local tag to the push remote (#1582).
    PushTags,
    /// Pick a tag on the push remote and delete it there (#1582).
    DeleteRemoteTag,
    // The operation in progress (#1356), listed first while there is one.
    ContinueOperation,
    SkipOperation,
    AbortOperation,
    BisectGood,
    BisectBad,
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
const ICON_ARROW_RIGHT: char = '\u{ea9c}'; // skip

/// The codicon painted on the `⋯` header pill that opens this menu.
pub const ICON_ELLIPSIS: char = '\u{ea7c}';

const COMMIT_ITEMS: &[Leaf] = &[
    leaf(ScmAction::Commit, "Commit", ICON_CHECK),
    leaf(ScmAction::CommitStaged, "Commit Staged", ICON_CHECK),
    leaf(ScmAction::CommitAll, "Commit All", ICON_CHECK),
    leaf(ScmAction::CommitAmend, "Commit (Amend)", ICON_EDIT),
    leaf(ScmAction::UndoLastCommit, "Undo Last Commit", ICON_DISCARD),
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
    leaf(ScmAction::PushTo, "Push to", ICON_ARROW_UP),
    leaf(ScmAction::PushForce, "Push (Force)", ICON_ARROW_UP),
    leaf(
        ScmAction::PublishBranch,
        "Publish Branch",
        ICON_CLOUD_UPLOAD,
    ),
];

const BRANCH_ITEMS: &[Leaf] = &[
    leaf(ScmAction::CreateBranch, "Create Branch", ICON_GIT_BRANCH),
    leaf(
        ScmAction::CreateBranchFrom,
        "Create Branch from",
        ICON_NEW_FOLDER,
    ),
    leaf(ScmAction::RenameBranch, "Rename Branch", ICON_EDIT),
    leaf(ScmAction::DeleteBranch, "Delete Branch", ICON_TRASH),
    leaf(ScmAction::Merge, "Merge", ICON_GIT_MERGE),
    leaf(ScmAction::Rebase, "Rebase", ICON_GIT_PR),
];

const REMOTE_ITEMS: &[Leaf] = &[
    leaf(ScmAction::AddRemote, "Add Remote", ICON_ADD),
    leaf(ScmAction::RemoveRemote, "Remove Remote", ICON_TRASH),
];

const STASH_ITEMS: &[Leaf] = &[
    leaf(ScmAction::Stash, "Stash", ICON_ARCHIVE),
    leaf(
        ScmAction::StashIncludeUntracked,
        "Stash (Include Untracked)",
        ICON_ARCHIVE,
    ),
    leaf(ScmAction::StashStaged, "Stash Staged", ICON_ARCHIVE),
    leaf(ScmAction::ApplyStash, "Apply Stash", ICON_ARCHIVE),
    leaf(ScmAction::PopStashLatest, "Pop Latest Stash", ICON_ARCHIVE),
    leaf(ScmAction::PopStashPick, "Pop Stash", ICON_ARCHIVE),
    leaf(ScmAction::DropStash, "Drop Stash", ICON_TRASH),
    leaf(ScmAction::ViewStash, "View Stash", ICON_ARCHIVE),
];

const TAGS_ITEMS: &[Leaf] = &[
    leaf(ScmAction::CreateTag, "Create Tag", ICON_TAG),
    leaf(ScmAction::DeleteTag, "Delete Tag", ICON_TRASH),
    leaf(ScmAction::PushTags, "Push Tags", ICON_ARROW_UP),
    leaf(ScmAction::DeleteRemoteTag, "Delete Remote Tag", ICON_TRASH),
];

/// The rows a stopped operation adds (#1356): its way forward, then its
/// way out. Labels name the operation, as VS Code's do.
fn operation_items(op: crate::git::RepoOp) -> Vec<Leaf> {
    use crate::git::RepoOp;
    let (cont, skip, abort): (Option<&str>, Option<&str>, &str) = match op {
        RepoOp::Merge => (None, None, "Abort Merge"),
        RepoOp::Rebase { .. } => (Some("Continue Rebase"), Some("Skip Commit"), "Abort Rebase"),
        RepoOp::CherryPick => (
            Some("Continue Cherry-Pick"),
            Some("Skip Commit"),
            "Abort Cherry-Pick",
        ),
        RepoOp::Revert => (Some("Continue Revert"), Some("Skip Commit"), "Abort Revert"),
        RepoOp::Bisect => {
            return vec![
                leaf(ScmAction::BisectGood, "Bisect: Mark Good", ICON_CHECK),
                leaf(ScmAction::BisectBad, "Bisect: Mark Bad", ICON_DISCARD),
                leaf(ScmAction::AbortOperation, "Bisect: Reset", ICON_TRASH),
            ];
        }
    };
    let mut items = Vec::new();
    if let Some(label) = cont {
        items.push(leaf(ScmAction::ContinueOperation, label, ICON_CHECK));
    }
    if let Some(label) = skip {
        items.push(leaf(ScmAction::SkipOperation, label, ICON_ARROW_RIGHT));
    }
    items.push(leaf(ScmAction::AbortOperation, abort, ICON_DISCARD));
    items
}

/// [`menu`] for a repository in the middle of `op` (#1356): the
/// operation's Continue / Skip / Abort rows come first, above a separator.
pub fn menu_for(op: Option<crate::git::RepoOp>) -> Vec<Node> {
    let mut nodes: Vec<Node> = op
        .map(|op| operation_items(op).into_iter().map(Node::Item).collect())
        .unwrap_or_default();
    if !nodes.is_empty() {
        nodes.push(Node::Sep);
    }
    nodes.extend(base_menu());
    nodes
}

/// The full top-level menu, mirroring VS Code's SCM "⋯" menu order.
fn base_menu() -> Vec<Node> {
    vec![
        Node::Item(leaf(ScmAction::Pull, "Pull", ICON_CLOUD_DOWNLOAD)),
        Node::Item(leaf(ScmAction::Push, "Push", ICON_CLOUD_UPLOAD)),
        Node::Item(leaf(ScmAction::Clone, "Clone", ICON_REPO_CLONE)),
        Node::Item(leaf(ScmAction::CheckoutTo, "Checkout to", ICON_GIT_BRANCH)),
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

/// One row of the menu as it is drawn: an action, a fly-out's parent, or a
/// separator. `accent` is the glyph's colour before the theme maps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Leaf {
        icon: char,
        accent: Color,
        label: &'static str,
    },
    Sub {
        icon: char,
        accent: Color,
        label: &'static str,
    },
    Sep,
}

impl Row {
    fn leaf(l: &Leaf) -> Self {
        Row::Leaf {
            icon: l.icon,
            accent: action_color(l.action),
            label: l.label,
        }
    }

    fn label(&self) -> Option<&'static str> {
        match self {
            Row::Leaf { label, .. } | Row::Sub { label, .. } => Some(label),
            Row::Sep => None,
        }
    }
}

/// The top column's rows for `nodes` (a [`menu_for`] list).
pub fn rows(nodes: &[Node]) -> Vec<Row> {
    nodes
        .iter()
        .map(|node| match node {
            Node::Item(l) => Row::leaf(l),
            Node::Sub { label, icon, .. } => Row::Sub {
                icon: *icon,
                accent: submenu_color(label),
                label,
            },
            Node::Sep => Row::Sep,
        })
        .collect()
}

/// The rows of the fly-out under `nodes[parent]`, when that is a fly-out.
pub fn fly_out_rows(nodes: &[Node], parent: usize) -> Option<Vec<Row>> {
    match nodes.get(parent)? {
        Node::Sub { items, .. } => Some(items.iter().map(Row::leaf).collect()),
        _ => None,
    }
}

/// Where one column of the menu sits: its rect, the rim rows a card keeps
/// above and below its rows (none on a plain panel), how many rows it holds,
/// and the first one it shows. A row is drawn only while it fits inside the
/// rim, so on a short terminal the column shows a window of its rows, moved
/// to keep the keyboard's row on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    pub rect: Rect,
    pub pad: u16,
    rows: usize,
    first: usize,
}

impl Column {
    /// Row `i`'s rect, when the column draws it.
    pub fn row(&self, i: usize) -> Option<Rect> {
        if i >= self.rows {
            return None;
        }
        let k = u16::try_from(i.checked_sub(self.first)?).ok()?;
        let y = self.rect.y + self.pad + k;
        (y + self.pad < self.rect.bottom()).then(|| Rect::new(self.rect.x, y, self.rect.width, 1))
    }

    /// The row drawn under `(x, y)`, if any.
    pub fn row_at(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.rect.x || x >= self.rect.right() {
            return None;
        }
        let i = self.first + usize::from(y.checked_sub(self.rect.y + self.pad)?);
        self.row(i).map(|_| i)
    }

    /// How many rows the column has room to draw at once.
    pub fn shown(&self) -> usize {
        usize::from(self.rect.height.saturating_sub(2 * self.pad)).min(self.rows)
    }

    /// The column showing its rows from `first` on.
    pub fn shown_from(self, first: usize) -> Column {
        Column {
            first: first.min(self.rows.saturating_sub(self.shown())),
            ..self
        }
    }
}

/// The rim rows a card of `rows` items gets above and below them: one while
/// the padded card fits the screen's height, else none, so on a short
/// terminal every action matters more than the padding.
fn edge_pad(rows: u16, height: u16) -> u16 {
    u16::from(rows.saturating_add(2) <= height)
}

/// The top column for `rows`, under `anchor` (the header's `⋯` pill) within
/// `screen`. A `card` (a `surfaces` theme) pads its rows with a rim row
/// above and below.
pub fn top_column(rows: &[Row], anchor: Rect, screen: Rect, card: bool) -> Column {
    let width = column_width(
        rows.iter()
            .filter_map(|r| r.label())
            .map(|l| l.chars().count()),
        true,
    );
    let n = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    let x = anchor.x.min(screen.x + screen.width.saturating_sub(width));
    let (pad, height) = if card {
        let pad = edge_pad(n, screen.height);
        (pad, (n + 2 * pad).min(screen.height))
    } else {
        (0, n)
    };
    let rect = Rect {
        x,
        y: anchor
            .bottom()
            .min(screen.y + screen.height.saturating_sub(height)),
        width: if card { width.min(screen.width) } else { width },
        height: height.min(screen.height),
    };
    Column {
        rect,
        pad,
        rows: rows.len(),
        first: 0,
    }
}

/// The fly-out column for `rows`, beside row `parent` of `top`: to its right,
/// or to its left when there is no room, its first row level with the
/// parent's.
pub fn fly_out_column(
    top: Column,
    parent: usize,
    rows: &[Row],
    screen: Rect,
    card: bool,
) -> Column {
    let width = column_width(
        rows.iter()
            .filter_map(|r| r.label())
            .map(|l| l.chars().count()),
        false,
    );
    let right_x = top.rect.x + top.rect.width;
    let x = if right_x + width <= screen.x + screen.width {
        right_x
    } else {
        top.rect.x.saturating_sub(width)
    };
    let n = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    // Level with the parent row where the top column draws it.
    let parent_y = top.row(parent).map_or(top.rect.y + top.pad, |r| r.y);
    let (pad, height) = if card {
        let pad = edge_pad(n, screen.height);
        (pad, (n + 2 * pad).min(screen.height))
    } else {
        (0, n)
    };
    let rect = Rect {
        x,
        y: parent_y
            .saturating_sub(pad)
            .min(screen.y + screen.height.saturating_sub(height)),
        width,
        height: height.min(screen.height),
    };
    Column {
        rect,
        pad,
        rows: rows.len(),
        first: 0,
    }
}

/// What a column marks on its rows besides their own look.
#[derive(Clone, Copy, Debug, Default)]
pub struct Marks {
    /// The row whose fly-out is open.
    pub open: Option<usize>,
    /// The keyboard's row.
    pub cursor: Option<usize>,
    /// The pointer's cell: a card lifts the row under it.
    pub pointer: Option<(u16, u16)>,
    /// The keyboard row's background and ink on a plain panel.
    pub cursor_colours: (Color, Color),
}

const BG: Color = Color::Rgb(0x25, 0x2b, 0x37);
const SUB_BG: Color = Color::Rgb(0x2b, 0x32, 0x3f);
const FG: Color = Color::Rgb(0xe6, 0xe9, 0xef);
const SEP_FG: Color = Color::Rgb(0x3b, 0x42, 0x52);
const SUB_ARROW_FG: Color = Color::Rgb(0x9a, 0xa4, 0xb2);

// Per-action icon accents, matching the Commit split-button dropdown so
// the two menus read as one design: network/sync ops blue, branch/remote
// cyan, commit/stage green, stash/tags amber, merge/rebase purple,
// destructive ops red, neutral grey.
const ACCENT_BLUE: Color = Color::Rgb(0x6c, 0xb6, 0xff);
const ACCENT_CYAN: Color = Color::Rgb(0x88, 0xc0, 0xd0);
const ACCENT_GREEN: Color = Color::Rgb(0xa3, 0xbe, 0x8c);
const ACCENT_AMBER: Color = Color::Rgb(0xeb, 0xcb, 0x8b);
const ACCENT_PURPLE: Color = Color::Rgb(0xc0, 0xa4, 0xf5);
const ACCENT_RED: Color = Color::Rgb(0xe7, 0x70, 0x70);
const ACCENT_GREY: Color = Color::Rgb(0x9a, 0xa4, 0xb2);

/// Accent colour for a leaf's icon.
fn action_color(action: ScmAction) -> Color {
    use ScmAction::*;
    match action {
        Pull | Push | Fetch | Clone | Sync | PullRebase | PushTo | PushForce | PublishBranch
        | CommitAndPush | CommitAndSync | PushTags => ACCENT_BLUE,
        CheckoutTo | CreateBranch | CreateBranchFrom => ACCENT_CYAN,
        Commit | CommitStaged | CommitAll | StageAll | AddRemote => ACCENT_GREEN,
        CommitAmend | UndoLastCommit | RenameBranch => ACCENT_AMBER,
        Merge | Rebase => ACCENT_PURPLE,
        DiscardAll | DeleteBranch | RemoveRemote | DropStash | DeleteTag | DeleteRemoteTag => {
            ACCENT_RED
        }
        Stash
        | StashIncludeUntracked
        | StashStaged
        | ApplyStash
        | PopStashLatest
        | PopStashPick
        | CreateTag => ACCENT_AMBER,
        UnstageAll | ShowGitOutput | ViewStash => ACCENT_GREY,
        ContinueOperation | BisectGood => ACCENT_GREEN,
        SkipOperation => ACCENT_AMBER,
        AbortOperation | BisectBad => ACCENT_RED,
    }
}

/// Accent colour for a submenu parent row, by category.
fn submenu_color(label: &str) -> Color {
    match label {
        "Commit" | "Changes" => ACCENT_GREEN,
        "Pull, Push" => ACCENT_BLUE,
        "Branch" | "Remote" => ACCENT_CYAN,
        "Stash" | "Tags" => ACCENT_AMBER,
        _ => ACCENT_GREY,
    }
}

/// Width a column needs to show the longest of `labels` as
/// `[pad][icon][gap][label][gap][›][pad]`.
fn column_width(labels: impl Iterator<Item = usize>, with_arrow: bool) -> u16 {
    let arrow = if with_arrow { 2 } else { 0 };
    let max = labels.max().unwrap_or(0) as u16;
    // 1 pad + icon + 1 gap + label + arrow + 1 pad
    (1 + 1 + 1 + max + arrow + 1).max(12)
}

/// Paint `col` with `rows`: a plain panel (no border, each glyph in its
/// accent) unless the theme draws `surfaces`, where the column is a raised
/// card with the accent lip, its rows one cell in from the card's top and
/// bottom edges and its rim on the rows' pad columns, so each row's rect is
/// still exactly the row drawn. A card's icons rest muted and lift to the
/// body ink with their row; the row under the pointer, or the keyboard's,
/// gets the neutral hover lift, and the open fly-out's parent the selected
/// band with its accent bar. Separators run edge to edge on a card.
pub fn paint_column(
    buf: &mut Buffer,
    col: Column,
    rows: &[Row],
    fly_out: bool,
    marks: Marks,
    theme: crate::theme::Theme,
) {
    if let Some(t) = crate::widgets::surface::tones(theme) {
        paint_card_column(buf, col, rows, marks, theme, t);
        return;
    }
    let bg = theme.ui(if fly_out { SUB_BG } else { BG });
    Widget::render(Clear, col.rect, buf);
    fill(buf, col.rect, bg);
    let (cursor_bg, cursor_ink) = marks.cursor_colours;
    for (i, row) in rows.iter().enumerate().skip(col.first) {
        let Some(r) = col.row(i) else {
            break;
        };
        let cursor = marks.cursor == Some(i);
        if cursor {
            fill(buf, r, cursor_bg);
        }
        match *row {
            Row::Sep => {
                for x in r.x + 1..r.x + r.width.saturating_sub(1) {
                    buf.set_string(x, r.y, "─", Style::default().fg(theme.ui(SEP_FG)).bg(bg));
                }
            }
            Row::Leaf {
                icon,
                accent,
                label,
            } => {
                let (row_bg, ink) = if cursor {
                    (cursor_bg, Some(cursor_ink))
                } else {
                    (bg, None)
                };
                paint_row(
                    buf,
                    r,
                    icon,
                    theme.ui(accent),
                    label,
                    false,
                    row_bg,
                    ink,
                    theme,
                );
            }
            Row::Sub {
                icon,
                accent,
                label,
            } => {
                let (row_bg, ink) = if cursor {
                    (cursor_bg, Some(cursor_ink))
                } else if marks.open == Some(i) {
                    (theme.ui(SUB_BG), None)
                } else {
                    (bg, None)
                };
                paint_row(
                    buf,
                    r,
                    icon,
                    theme.ui(accent),
                    label,
                    true,
                    row_bg,
                    ink,
                    theme,
                );
            }
        }
    }
}

/// [`paint_column`] as a `surfaces` theme draws it.
fn paint_card_column(
    buf: &mut Buffer,
    col: Column,
    rows: &[Row],
    marks: Marks,
    theme: crate::theme::Theme,
    t: crate::widgets::surface::Tones,
) {
    Widget::render(Clear, col.rect, buf);
    fill(buf, col.rect, t.raised());
    crate::widgets::surface::paint_overlay_frame(buf, col.rect, theme);
    for (i, row) in rows.iter().enumerate().skip(col.first) {
        let Some(r) = col.row(i) else {
            break;
        };
        let lit = marks.cursor == Some(i) || crate::widgets::hover::contains(r, marks.pointer);
        match *row {
            Row::Sep => {
                let rule = "─".repeat(usize::from(r.width));
                buf.set_string(r.x, r.y, rule, Style::default().fg(t.rim()));
            }
            Row::Leaf { icon, label, .. } => {
                paint_card_row(buf, r, t, icon, label, false, lit, false);
            }
            Row::Sub { icon, label, .. } => {
                paint_card_row(buf, r, t, icon, label, true, lit, marks.open == Some(i));
            }
        }
    }
}

/// One row of a `surfaces` card: `[rim][icon][gap][label]…[›][rim]`. A
/// `selected` row (an open submenu's parent) is the band with the accent
/// bar on the card's edge; a `lit` one (under the pointer) the hover lift.
#[allow(clippy::too_many_arguments)]
fn paint_card_row(
    buf: &mut Buffer,
    row: Rect,
    t: crate::widgets::surface::Tones,
    icon: char,
    label: &str,
    submenu: bool,
    lit: bool,
    selected: bool,
) {
    let body = Rect::new(row.x, row.y, row.width.saturating_sub(1), 1);
    if selected {
        crate::widgets::surface::paint_selected_row(buf, body, t, t.raised, true);
    } else if lit {
        let inner = Rect::new(row.x + 1, row.y, row.width.saturating_sub(2), 1);
        buf.set_style(inner, Style::default().bg(t.hover_on(t.raised)));
    }
    let active = selected || lit;
    let icon_fg = if active { t.text() } else { t.muted() };
    buf.set_string(
        row.x + 1,
        row.y,
        icon.to_string(),
        Style::default().fg(icon_fg),
    );
    let avail = row.width.saturating_sub(3 + if submenu { 2 } else { 1 }) as usize;
    let shown: String = label.chars().take(avail).collect();
    let ink = if active { t.strong() } else { t.text() };
    buf.set_string(row.x + 3, row.y, &shown, Style::default().fg(ink));
    if submenu && row.width >= 2 {
        let fg = if active { t.text() } else { t.faint() };
        buf.set_string(row.x + row.width - 2, row.y, "›", Style::default().fg(fg));
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

/// One row of a plain panel: `[pad][icon][gap][label]…[›][pad]`, the label
/// and `›` in `ink` when given, else the panel's own.
#[allow(clippy::too_many_arguments)]
fn paint_row(
    buf: &mut Buffer,
    row: Rect,
    icon: char,
    icon_color: Color,
    label: &str,
    submenu: bool,
    bg: Color,
    ink: Option<Color>,
    theme: crate::theme::Theme,
) {
    let fg = Style::default().fg(ink.unwrap_or(theme.ui(FG))).bg(bg);
    buf.set_string(
        row.x + 1,
        row.y,
        icon.to_string(),
        Style::default().fg(icon_color).bg(bg),
    );
    let label_x = row.x + 3;
    let avail = row.width.saturating_sub(3 + if submenu { 2 } else { 1 }) as usize;
    let shown: String = label.chars().take(avail).collect();
    buf.set_string(label_x, row.y, &shown, fg);
    if submenu && row.width >= 2 {
        buf.set_string(
            row.x + row.width - 2,
            row.y,
            "›",
            Style::default()
                .fg(ink.unwrap_or(theme.ui(SUB_ARROW_FG)))
                .bg(bg),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1356: a stopped rebase puts Continue, Skip and Abort first, above
    /// a separator; a merge gets only Abort (a commit concludes it); no
    /// operation, no extra rows.
    #[test]
    fn an_operation_in_progress_leads_the_menu() {
        use crate::git::RepoOp;
        let labels = |op| -> Vec<&'static str> {
            menu_for(op)
                .iter()
                .map_while(|n| match n {
                    Node::Item(l) => Some(l.label),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            labels(Some(RepoOp::Rebase { step: 2, total: 5 })),
            ["Continue Rebase", "Skip Commit", "Abort Rebase"]
        );
        assert_eq!(labels(Some(RepoOp::Merge)), ["Abort Merge"]);
        assert!(matches!(menu_for(Some(RepoOp::Merge))[1], Node::Sep));
        assert_eq!(menu_for(None).len(), base_menu().len());
    }

    #[test]
    fn menu_has_the_expected_top_level_shape() {
        let m = menu_for(None);
        // 5 leaves + sep + 7 submenus + sep + 1 leaf = 15 nodes.
        assert_eq!(m.len(), 15);
        let subs = m.iter().filter(|n| matches!(n, Node::Sub { .. })).count();
        assert_eq!(subs, 7, "Commit/Changes/Pull-Push/Branch/Remote/Stash/Tags");
    }

    /// The top column, drawn under a pill at (10, 0) on a 120×40 screen.
    fn top(theme: crate::theme::Theme, screen: Rect) -> (Vec<Row>, Column) {
        let rows = rows(&menu_for(None));
        let card = crate::widgets::surface::tones(theme).is_some();
        let col = top_column(&rows, Rect::new(10, 0, 2, 1), screen, card);
        (rows, col)
    }

    fn row_of(rows: &[Row], label: &str) -> usize {
        rows.iter().position(|r| r.label() == Some(label)).unwrap()
    }

    /// The boxed brand theme keeps its per-action icon accents.
    #[test]
    fn leaf_icons_carry_their_per_action_accent_colour() {
        let screen = Rect::new(0, 0, 120, 40);
        let theme = crate::theme::Theme::BLACK;
        let (rows, col) = top(theme, screen);
        let mut buf = Buffer::empty(screen);
        paint_column(&mut buf, col, &rows, false, Marks::default(), theme);
        let pull = col.row(row_of(&rows, "Pull")).unwrap();
        // The icon sits one cell in from the row's left edge.
        assert_eq!(
            buf[(pull.x + 1, pull.y)].fg,
            ACCENT_BLUE,
            "the Pull icon must be painted in its blue accent, not the neutral foreground"
        );
    }

    /// Croft Loch draws the menu as a raised card: its rows sit inside an
    /// edge one row above and below them (each row's rect is the row
    /// drawn), icons rest muted, and the row under the pointer lifts with
    /// its icon in the body ink.
    #[test]
    fn croft_loch_draws_the_menu_as_a_card_with_a_hover_lift() {
        let theme = crate::theme::Theme::from_id("loch");
        let t = crate::widgets::surface::tones(theme).unwrap();
        let screen = Rect::new(0, 0, 120, 40);
        let (rows, col) = top(theme, screen);
        let first = col.row(0).unwrap();
        assert_eq!(first.y, 2, "a card edge above the rows, under the pill");
        let mut buf = Buffer::empty(screen);
        paint_column(&mut buf, col, &rows, false, Marks::default(), theme);
        assert_eq!(buf[(first.x, first.y - 1)].symbol(), "\u{2594}");
        assert_eq!(buf[(first.x + 3, first.y)].bg, t.raised());
        let pull = col.row(row_of(&rows, "Pull")).unwrap();
        assert_eq!(buf[(pull.x + 1, pull.y)].fg, t.muted(), "icons rest muted");

        let marks = Marks {
            pointer: Some((pull.x + 4, pull.y)),
            ..Marks::default()
        };
        let mut buf = Buffer::empty(screen);
        paint_column(&mut buf, col, &rows, false, marks, theme);
        assert_eq!(buf[(pull.x + 3, pull.y)].bg, t.hover_on(t.raised));
        assert_eq!(buf[(pull.x + 1, pull.y)].fg, t.text(), "the icon lifts too");
        assert_eq!(col.row_at(pull.x + 4, pull.y), Some(row_of(&rows, "Pull")));
    }

    /// The keyboard's row shows on a plain panel as a band in the cursor
    /// colours, and on a card as the hover lift.
    #[test]
    fn the_keyboards_row_shows_in_both_looks() {
        let screen = Rect::new(0, 0, 120, 40);
        let band = Color::Rgb(1, 2, 3);
        let ink = Color::Rgb(4, 5, 6);
        let plain = crate::theme::Theme::BLACK;
        let (rows, col) = top(plain, screen);
        let push = row_of(&rows, "Push");
        let marks = Marks {
            cursor: Some(push),
            cursor_colours: (band, ink),
            ..Marks::default()
        };
        let mut buf = Buffer::empty(screen);
        paint_column(&mut buf, col, &rows, false, marks, plain);
        let r = col.row(push).unwrap();
        assert_eq!(buf[(r.x, r.y)].bg, band, "the band fills the whole row");
        assert_eq!(buf[(r.x + 3, r.y)].fg, ink);
        assert_eq!(
            buf[(r.x + 1, r.y)].fg,
            plain.ui(ACCENT_BLUE),
            "the icon keeps its accent"
        );

        let card = crate::theme::Theme::from_id("loch");
        let t = crate::widgets::surface::tones(card).unwrap();
        let (rows, col) = top(card, screen);
        let marks = Marks {
            cursor: Some(push),
            ..Marks::default()
        };
        let mut buf = Buffer::empty(screen);
        paint_column(&mut buf, col, &rows, false, marks, card);
        let r = col.row(push).unwrap();
        assert_eq!(buf[(r.x + 3, r.y)].bg, t.hover_on(t.raised));
    }

    /// On a terminal too short for the padded card, Croft Loch's menu gives
    /// its rim rows to the actions: every row the plain menu reaches stays
    /// reachable on the card too.
    #[test]
    fn croft_loch_keeps_every_action_reachable_on_a_short_terminal() {
        let rows_n = menu_for(None).len() as u16;
        let screen = Rect::new(0, 0, 120, rows_n + 1);
        let (_, card) = top(crate::theme::Theme::from_id("loch"), screen);
        let (_, plain) = top(crate::theme::Theme::BLACK, screen);
        assert_eq!(card.shown(), menu_for(None).len(), "no action clipped");
        assert!(card.shown() >= plain.shown());
    }

    /// A fly-out opens to the right of its parent row, level with it, and
    /// flips to the left when the screen has no room on the right.
    #[test]
    fn a_fly_out_sits_beside_its_parent_row() {
        let nodes = menu_for(None);
        let rows = rows(&nodes);
        let stash = row_of(&rows, "Stash");
        let items = fly_out_rows(&nodes, stash).unwrap();
        assert!(items.iter().any(|r| r.label() == Some("Drop Stash")));
        let screen = Rect::new(0, 0, 120, 40);
        let top = top_column(&rows, Rect::new(10, 0, 2, 1), screen, false);
        let fly = fly_out_column(top, stash, &items, screen, false);
        assert_eq!(fly.rect.x, top.rect.right());
        assert_eq!(fly.row(0).unwrap().y, top.row(stash).unwrap().y);
        // Far enough from the left edge for the fly-out to fit there.
        let pill = Rect::new(60, 0, 2, 1);
        let narrow = Rect::new(
            0,
            0,
            top_column(&rows, pill, screen, false).rect.right() + 3,
            40,
        );
        let top = top_column(&rows, pill, narrow, false);
        let fly = fly_out_column(top, stash, &items, narrow, false);
        assert!(fly.rect.width < pill.x, "room on the left");
        assert_eq!(fly.rect.right(), top.rect.x, "flipped to the left");
    }

    /// A column shown from a later row draws that row first, maps its cells
    /// back to it, and draws none of the rows above it.
    #[test]
    fn a_column_shown_from_a_later_row() {
        let rows = rows(&menu_for(None));
        let short = Rect::new(0, 0, 120, 8);
        let col = top_column(&rows, Rect::new(10, 0, 2, 1), short, false);
        assert!(col.shown() < rows.len());
        let col = col.shown_from(5);
        assert_eq!(col.row(4), None);
        let first = col.row(5).unwrap();
        assert_eq!(first.y, col.rect.y);
        assert_eq!(col.row_at(first.x + 1, first.y), Some(5));
        assert_eq!(
            col.shown_from(99).row(rows.len() - 1).map(|r| r.bottom()),
            Some(col.rect.bottom())
        );
    }

    /// A cell maps to the row drawn there, and nothing outside the column.
    #[test]
    fn a_cell_maps_to_the_row_drawn_there() {
        let screen = Rect::new(0, 0, 120, 40);
        let (rows, col) = top(crate::theme::Theme::BLACK, screen);
        for i in 0..rows.len() {
            let r = col.row(i).unwrap();
            assert_eq!(col.row_at(r.x, r.y), Some(i));
            assert_eq!(col.row_at(r.right() - 1, r.y), Some(i));
        }
        assert_eq!(col.row_at(col.rect.right(), col.rect.y), None);
        assert_eq!(col.row_at(col.rect.x, col.rect.bottom()), None);
    }
}
