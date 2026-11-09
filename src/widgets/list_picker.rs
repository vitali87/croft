//! A generic single-choice list picker for Source Control operations that
//! act on one item out of a list git already owns: apply/pop/drop a stash,
//! delete a tag, remove a remote, push to a remote. The user filters by
//! typing, navigates with the arrows, and Enter selects.
//!
//! Distinct from [`crate::widgets::branch_picker`], which adds a
//! "create new branch" affordance. This picker only ever selects an
//! existing row, so it stays a thin list. The App tags it with a
//! [`ListPurpose`] and reads back the chosen row's `id`.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

/// Why the picker is open, so the App dispatches the right git op on Enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListPurpose {
    /// Source Control: Review Pull Request (#365): the open PRs, `id` the
    /// PR's URL, or [`PR_BY_NUMBER`] for the typed-number prompt.
    ReviewPullRequest,
    /// Agents: Review a Changed File (#345): the agent lanes' files, `id`
    /// an index into the App's pending lane picks.
    AgentLaneFile,
    StashApply,
    StashPop,
    StashDrop,
    /// A stash whose changes to open (#1581).
    StashView,
    /// A branch switch local changes blocked (#925): `id` is `stash`
    /// (Stash & Checkout) or `migrate` (Migrate Changes).
    BlockedSwitch,
    RemoveRemote,
    DeleteTag,
    /// A tag on the push remote to delete there (#1582); each row's id is
    /// `<remote>\t<tag>`, a tab being no part of a ref name.
    DeleteRemoteTag,
    PushToRemote,
    /// LSP Quick Fix: the rows are the code actions at the cursor; the App
    /// reads back the chosen row's `id` as an index into its pending actions.
    CodeAction,
    /// Merge-conflict resolution: the rows are Accept Current / Incoming /
    /// Both for the conflict block at the cursor; `id` names the resolution.
    MergeConflict,
    /// Change Color Presentation (#254): the rows are the server's
    /// alternative spellings of the color at the cursor; `id` is an index
    /// into the App's pending presentation edits.
    ColorPresentation,
    /// Auto-detected project tasks (Tasks: Run Task): the rows are the
    /// commands discovered from the workspace's manifests; `id` is an
    /// index into the App's discovered-task list.
    RunTask,
    /// Debug: Select and Start Debugging — the launch.json configurations and
    /// compounds, plus the synthesized zero-config entry. `id` is `active` for
    /// "Debug active file", `compound:<index>` into the App's discovered
    /// compounds, or a bare index into its discovered-config list. Compounds
    /// carry their own id prefix so the two index spaces cannot collide.
    DebugConfig,
    /// Debug: Add Configuration… (#250), first step: the adapter, `id` a
    /// type from `dap::configs::DRAFT_TYPES`.
    DebugConfigType,
    /// Second step: `id` is `launch` or `attach`.
    DebugConfigRequest,
    /// Debug: Step Into Target (#1543): the calls on the paused line, `id`
    /// the adapter's `targetId`.
    StepInTarget,
    /// The searchable Settings hub (Preferences: Open Settings). Rows toggle a
    /// boolean setting (`id` = `toggle:<field>`) or run a follow-up command
    /// (`id` = `cmd:<command_id>`, e.g. open a JSON file or the theme picker).
    Settings,
    /// Preferences: Open Keyboard Shortcuts (#612): one row per command,
    /// `id` = `kb:<command id>`; choosing one records its new chord.
    KeyboardShortcuts,
    /// Profiles: Switch Profile (#618). `id` is `profile:<name>`,
    /// `profile:` for the default, or an action (`new`, `workspace`,
    /// `workspace-clear`).
    Profiles,
    /// Review: Submit Review (#366): the verdict, `id` = `COMMENT`,
    /// `APPROVE` or `REQUEST_CHANGES`.
    ReviewVerdict,
    /// Source Control: Export Comments to Pull Request (#368): a `post` row,
    /// then one row per comment to be posted.
    ExportComments,
    /// Multiplayer session roster (Session: Participants): the rows are the
    /// clients attached to this session's host; `id` is the participant id.
    /// Enter opens the per-participant action picker.
    SessionParticipant,
    /// Action on one participant: `id` is `grant:<id>` / `revoke:<id>` /
    /// `kick:<id>`, applied through the session host's control channel.
    SessionParticipantAction,
    /// `"processId": "${command:pickProcess}"` (#250): the rows are the
    /// user's processes; `id` is the pid the parked attach launches against.
    AttachProcess,
    /// GitHub code scanning (#577): the rows are the repository's recent
    /// analyses; `id` is the analysis id, opened as a SARIF log.
    CodeScanningAnalysis,
    /// Why a code scanning alert is dismissed: `id` is `<alert>:<reason>`,
    /// the reason an index into `DismissReason::ALL`.
    DismissAlert,
    /// A result set of a CodeQL query run (#578): `id` is its CSV.
    CodeqlResultSet,
    /// What to model a Model Editor endpoint as (#578): `id` is
    /// `<endpoint>:<choice>` or `<endpoint>:remove`.
    CodeqlModel,
    /// The databases checklist of "Run Query on Multiple Databases"
    /// (#578): `id` is a database's index, or `run`.
    CodeqlMultiDb,
    /// Output: Select Channel… (#852): the OUTPUT channels, `id` the
    /// channel's name.
    OutputChannel,
    /// Bookmarks: View Bookmarks: every bookmark in the open files; `id` is
    /// an index into the App's pending bookmark targets.
    Bookmark,
    /// Where to add a CodeQL database from (#578): `id` is an index into
    /// the App's database sources.
    CodeqlDbSource,
    /// The CodeQL database to make the current one (#578): `id` is its
    /// index in the database list.
    CodeqlDatabase,
    /// View: Set Panel Alignment… (#852): `id` is `left`, `center`,
    /// `right` or `justify`.
    PanelAlignment,
    /// View: Set Quick Input Position… (#852): `id` is `top` or `center`.
    QuickInputPosition,
    /// A `pickString` input a launch or task asks for (#1424); `id` the
    /// chosen value.
    LaunchInput,
    /// A language server's `window/showMessageRequest` (#1599): the rows
    /// are its actions, `id` an index into them; Esc answers no action.
    ServerQuestion,
    /// Notebook: Select Kernel (#1377): the installed kernelspecs, `id`
    /// the kernel's name.
    NotebookKernel,
    /// HTTP: Switch Environment (#924): `id` the named environment of the
    /// `.http` env files, or empty for none.
    HttpEnvironment,
    /// The tab strip's "all open tabs" button: every tab of the group, `id`
    /// its index.
    OpenTab,
}

/// The Review Pull Request row that asks for a number or URL instead.
pub const PR_BY_NUMBER: &str = "#by-number";

/// One selectable row: a stable `id` the App acts on (a stash index, a
/// remote/tag name) and the `label` shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRow {
    pub id: String,
    pub label: String,
}

pub struct ListPicker {
    pub purpose: ListPurpose,
    pub title: String,
    pub rows: Vec<ListRow>,
    pub query: String,
    pub cursor: usize,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    /// The rows the last render drew the list in, for the click test: two
    /// below the top edge on a boxed theme, three (under the rule) on a
    /// `surfaces` one.
    pub last_list: Rect,
    /// Where the terminal's caret goes in the query, set by a `surfaces`
    /// theme's render (which draws no caret of its own); `None` on a boxed
    /// theme, whose caret is a painted block.
    pub last_caret: Option<(u16, u16)>,
}

impl ListPicker {
    pub fn new(purpose: ListPurpose, title: impl Into<String>, rows: Vec<ListRow>) -> Self {
        Self {
            purpose,
            title: title.into(),
            rows,
            query: String::new(),
            cursor: 0,
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_list: Rect::default(),
            last_caret: None,
        }
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

    /// Indices of rows whose label matches the query (case-insensitive
    /// substring); an empty query matches everything.
    fn matches(&self) -> Vec<usize> {
        let needle = self.query.trim().to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| needle.is_empty() || r.label.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn visible_count(&self) -> usize {
        self.matches().len()
    }

    /// The row Enter would act on, if any.
    pub fn selected_row(&self) -> Option<&ListRow> {
        self.matches().get(self.selected).map(|i| &self.rows[*i])
    }

    pub fn push_char(&mut self, c: char) {
        let at = self.byte_offset(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
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
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn select_next(&mut self) {
        let count = self.visible_count();
        if count > 0 && self.selected + 1 < count {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Home: the first matched row.
    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    /// End: the last matched row.
    pub fn select_last(&mut self) {
        self.selected = self.visible_count().saturating_sub(1);
    }

    /// The matched-row index at screen row `y`, if `y` lands on a visible row.
    /// The renderer records the rows it drew the list in (`last_list`), so
    /// this and it stay in lock-step whatever the theme's anatomy. Used to
    /// map a mouse click to a selectable row.
    pub fn row_index_at(&self, y: u16) -> Option<usize> {
        let list = self.last_list;
        if y < list.y || y - list.y >= list.height {
            return None;
        }
        let idx = self.scroll + (y - list.y) as usize;
        (idx < self.visible_count()).then_some(idx)
    }
}

/// Draw the picker over `screen`. `backdrop` lets a `surfaces` theme dim
/// what is behind the card (the App withholds it where inline images would
/// not dim with the text).
pub fn render_list_picker(
    picker: &mut ListPicker,
    screen: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
    backdrop: bool,
) {
    if let Some(t) = crate::widgets::surface::tones(theme) {
        render_picker_card(picker, screen, buf, theme, t, backdrop);
        return;
    }
    picker.last_caret = None;
    let width = (screen.width.saturating_mul(6) / 10)
        .clamp(36, 96)
        .min(screen.width);
    let height = (screen.height.saturating_mul(6) / 10)
        .max(8)
        .min(screen.height);
    let rect = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 4,
        width,
        height,
    };
    picker.last_rect = rect;
    // The top border, then the prompt row; the list runs to the bottom
    // border.
    picker.last_list = Rect {
        x: rect.x + 1,
        y: rect.y.saturating_add(2),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(3),
    };
    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        format!(
            " {} — Esc to close, ↑/↓ navigate, Enter select ",
            picker.title
        ),
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
    if theme.framed_overlays() {
        crate::widgets::surface::paint_overlay_frame(buf, rect, theme);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    let sel_bg = if theme.gradient() {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e))
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Prompt row.
    let cursor = picker.cursor.min(picker.query.chars().count());
    let before: String = picker.query.chars().take(cursor).collect();
    let at: String = picker.query.chars().skip(cursor).take(1).collect();
    let after: String = picker.query.chars().skip(cursor + 1).collect();
    let caret = if at.is_empty() { " ".to_string() } else { at };
    let qstyle = Style::default().fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)));
    let caret_style = Style::default()
        .fg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f)))
        .bg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        .add_modifier(Modifier::SLOW_BLINK);
    Widget::render(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "> ",
                Style::default().fg(theme.ui(Color::Rgb(0x88, 0xc0, 0xd0))),
            ),
            Span::styled(before, qstyle),
            Span::styled(caret, caret_style),
            Span::styled(after, qstyle),
        ])),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
        buf,
    );

    let list = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    if list.height == 0 {
        return;
    }
    let matches = picker.matches();
    if matches.is_empty() {
        Widget::render(
            Paragraph::new(Line::from(Span::styled(
                "  (nothing to choose)",
                Style::default().fg(theme.ui(Color::Rgb(0x7a, 0x82, 0x90))),
            ))),
            list,
            buf,
        );
        return;
    }
    let visible = list.height as usize;
    if picker.selected >= picker.scroll + visible {
        picker.scroll = picker.selected + 1 - visible;
    }
    if picker.selected < picker.scroll {
        picker.scroll = picker.selected;
    }
    let end = (picker.scroll + visible).min(matches.len());
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - picker.scroll);
    for (offset, m) in matches[picker.scroll..end].iter().enumerate() {
        let row_idx = picker.scroll + offset;
        let is_sel = row_idx == picker.selected;
        let style = if is_sel {
            Style::default().bg(sel_bg).fg(theme.ui(Color::White))
        } else {
            Style::default().fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        };
        let prefix = if is_sel { "> " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), style),
            Span::styled(picker.rows[*m].label.clone(), style),
        ]));
    }
    Widget::render(Paragraph::new(lines), list, buf);
}

/// The picker as a `surfaces` theme draws it: the shared quick-input card
/// ([`crate::widgets::quick_input`]) with no title on its edge. The title
/// ("Select a Remote", "Apply Stash") is what the empty field asks for, so
/// it is the placeholder, as in VS Code's quick picks; the rows carry the
/// query's matched characters in the accent.
fn render_picker_card(
    picker: &mut ListPicker,
    screen: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
    t: crate::widgets::surface::Tones,
    backdrop: bool,
) {
    use crate::widgets::quick_input::{self as qi, Card, Input, Row};
    let width = (screen.width.saturating_mul(6) / 10).clamp(36, 96);
    let max_h = (screen.height.saturating_mul(6) / 10).max(8);
    let matches = picker.matches();
    let total = matches.len();
    let card = Card::layout(screen, width, max_h, total, false);
    picker.last_rect = card.rect;
    picker.last_list = card.list;
    if backdrop {
        crate::widgets::surface::dim_backdrop(buf, card.rect, theme);
    }
    let filtering = !picker.query.trim().is_empty();
    let count = qi::count_label(total, filtering, false, "item", "items");
    // The title is the placeholder, which any typed character hides (a
    // space included); from then on it rides beside the count and keeps
    // its cells, so a picker whose title is its operation (Apply Stash vs
    // Pop Stash) never stops saying which it is.
    let titled = !picker.query.is_empty() && !picker.title.is_empty();
    let count = if titled {
        format!("{} \u{b7} {count}", picker.title)
    } else {
        count
    };
    picker.last_caret = qi::paint_card(
        buf,
        &card,
        theme,
        t,
        &Input {
            glyph: "\u{203a}",
            query: &picker.query,
            cursor: picker.cursor,
            placeholder: &picker.title,
            count: &count,
            verb: "select",
            keep_count: titled,
        },
    );
    let visible = card.list.height as usize;
    if visible == 0 {
        return;
    }
    if total == 0 {
        let empty = if picker.rows.is_empty() {
            "Nothing to choose"
        } else {
            "No matching items"
        };
        qi::paint_empty(buf, &card, t, empty);
        return;
    }
    if picker.selected >= picker.scroll + visible {
        picker.scroll = picker.selected + 1 - visible;
    }
    if picker.selected < picker.scroll {
        picker.scroll = picker.selected;
    }
    let end = (picker.scroll + visible).min(total);
    let hit = qi::hit_style(t);
    for (offset, &m) in matches[picker.scroll..end].iter().enumerate() {
        let label = &picker.rows[m].label;
        let offsets = qi::match_offsets(label, &picker.query);
        let label = qi::highlighted(label, 0, &offsets, Style::default().fg(t.text()), hit);
        qi::paint_row(
            buf,
            &card,
            offset as u16,
            t,
            Row {
                selected: picker.scroll + offset == picker.selected,
                glyph: None,
                label,
                trail: Vec::new(),
            },
        );
    }
    qi::paint_scroll_cue(buf, &card, t, total, picker.scroll);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<ListRow> {
        vec![
            ListRow {
                id: "0".into(),
                label: "stash@{0}: WIP on main".into(),
            },
            ListRow {
                id: "1".into(),
                label: "stash@{1}: WIP on feature".into(),
            },
        ]
    }

    #[test]
    fn selected_row_tracks_navigation() {
        let mut p = ListPicker::new(ListPurpose::StashApply, "Apply Stash", rows());
        assert_eq!(p.selected_row().map(|r| r.id.as_str()), Some("0"));
        p.select_next();
        assert_eq!(p.selected_row().map(|r| r.id.as_str()), Some("1"));
        p.select_prev();
        assert_eq!(p.selected_row().map(|r| r.id.as_str()), Some("0"));
    }

    #[test]
    fn typing_filters_rows_and_resets_selection() {
        let mut p = ListPicker::new(ListPurpose::StashDrop, "Drop Stash", rows());
        p.select_next();
        for c in "feature".chars() {
            p.push_char(c);
        }
        assert_eq!(p.visible_count(), 1);
        assert_eq!(p.selected, 0, "a filter edit resets the highlight");
        assert_eq!(p.selected_row().map(|r| r.id.as_str()), Some("1"));
    }

    fn rendered(theme: crate::theme::Theme) -> (ListPicker, Buffer) {
        let mut p = ListPicker::new(ListPurpose::CodeAction, "Quick Fix", rows());
        let screen = Rect::new(0, 0, 80, 30);
        let mut buf = Buffer::empty(screen);
        render_list_picker(&mut p, screen, &mut buf, theme, false);
        (p, buf)
    }

    /// The click test follows the rows the renderer drew: two below the
    /// top border on a boxed theme (border, prompt), three on Croft Loch
    /// (edge, prompt, rule).
    #[test]
    fn row_index_at_maps_click_y_to_the_row_under_the_pointer() {
        for (theme, first) in [
            (crate::theme::Theme::BLACK, 2),
            (crate::theme::Theme::from_id("loch"), 3),
        ] {
            let (p, buf) = rendered(theme);
            let top = p.last_rect.y + first;
            assert_eq!(p.row_index_at(top), Some(0), "first list row");
            assert_eq!(p.row_index_at(top + 1), Some(1), "second list row");
            assert_eq!(
                p.row_index_at(p.last_rect.y + 1),
                None,
                "the prompt row is not selectable"
            );
            assert_eq!(
                p.row_index_at(top + 2),
                None,
                "below the last row is nothing"
            );
            let row: String = (p.last_rect.x..p.last_rect.right())
                .map(|x| buf[(x, top)].symbol().to_string())
                .collect();
            assert!(row.contains("stash@{0}"), "{}: {row:?}", theme.id());
        }
    }

    /// Croft Loch: the title becomes the empty field's placeholder instead
    /// of a heading on the card's edge, and the selected row is the band
    /// with the accent bar on the edge.
    #[test]
    fn croft_loch_picker_asks_its_title_in_the_field() {
        let t = crate::widgets::surface::tones(crate::theme::Theme::from_id("loch")).unwrap();
        let (p, buf) = rendered(crate::theme::Theme::from_id("loch"));
        let r = p.last_rect;
        let line = |y: u16| -> String {
            (r.x..r.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        };
        assert!(!line(r.y).contains("Quick Fix"), "no title on the edge");
        assert!(
            line(r.y + 1).contains("\u{203a} Quick Fix"),
            "{:?}",
            line(r.y + 1)
        );
        assert_eq!(buf[(r.x + 4, r.y + 1)].fg, t.muted());
        assert_eq!(
            buf[(r.x, r.y + 3)].symbol(),
            crate::widgets::surface::ROW_BAR
        );
        assert_eq!(p.last_caret, Some((r.x + 4, r.y + 1)));
    }

    /// A Loch picker's title is its placeholder, so once the user types it
    /// moves beside the count: Pop Stash never reads like Apply Stash.
    #[test]
    fn croft_loch_picker_keeps_its_title_while_filtering() {
        let theme = crate::theme::Theme::from_id("loch");
        let mut p = ListPicker::new(ListPurpose::CodeAction, "Pop Stash", rows());
        for c in "stash".chars() {
            p.push_char(c);
        }
        let screen = Rect::new(0, 0, 80, 30);
        let mut buf = Buffer::empty(screen);
        render_list_picker(&mut p, screen, &mut buf, theme, false);
        let r = p.last_rect;
        let line: String = (r.x..r.right())
            .map(|x| buf[(x, r.y + 1)].symbol().to_string())
            .collect();
        assert!(line.contains("stash"), "{line:?}");
        assert!(line.contains("Pop Stash \u{b7} "), "{line:?}");
        // A query of spaces hides the placeholder too, and a query longer
        // than the field scrolls instead of pushing the title off the row.
        for query in ["  ", &"stash ".repeat(20)] {
            let mut p = ListPicker::new(ListPurpose::CodeAction, "Pop Stash", rows());
            for c in query.chars() {
                p.push_char(c);
            }
            let mut buf = Buffer::empty(screen);
            render_list_picker(&mut p, screen, &mut buf, theme, false);
            let r = p.last_rect;
            let line: String = (r.x..r.right())
                .map(|x| buf[(x, r.y + 1)].symbol().to_string())
                .collect();
            assert!(line.contains("Pop Stash \u{b7} "), "{query:?}: {line:?}");
            let (cx, cy) = p.last_caret.expect("the caret stays on the row");
            assert_eq!(cy, r.y + 1);
            let title_col = line[..line.find("Pop Stash").unwrap()].chars().count();
            assert!(
                usize::from(cx - r.x) < title_col,
                "the caret stays left of the title"
            );
        }
    }

    #[test]
    fn navigation_is_bounded() {
        let mut p = ListPicker::new(ListPurpose::DeleteTag, "Delete Tag", rows());
        for _ in 0..5 {
            p.select_next();
        }
        assert_eq!(p.selected, 1);
        for _ in 0..5 {
            p.select_prev();
        }
        assert_eq!(p.selected, 0);
    }
}
