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
use crate::widgets::picker_colours;
use crate::widgets::picker_query;

pub use crate::command_registry::{ALL_COMMANDS, Command, glyph_hint_for_platform, platform_hint};

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
    /// Shortcuts that differ from a command's built-in hint (#1482): the
    /// user's own chord, or empty where a user binding took the chord.
    pub hint_overrides: Vec<(Command, String)>,
    /// The user's menu commands (`menu.json`), listed first while the query
    /// is empty and marked with a star.
    pub pinned: Vec<Command>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    pub last_inner_height: u16,
    /// Where the terminal's caret goes in the query, set by a `surfaces`
    /// theme's render (which draws no caret of its own); `None` on a boxed
    /// theme, whose caret is a painted block.
    pub last_caret: Option<(u16, u16)>,
}

impl CommandPalette {
    pub fn new() -> Self {
        let mut me = Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            extensions: Vec::new(),
            hidden: Vec::new(),
            hint_overrides: Vec::new(),
            pinned: Vec::new(),
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
            last_caret: None,
        };
        me.refresh_results();
        me
    }

    /// The shortcut shown beside `item`: an override from the user's
    /// keybindings (#1482), else the built-in hint as this platform spells it.
    pub fn hint_for(&self, item: &PaletteItem) -> std::borrow::Cow<'static, str> {
        if let PaletteItem::Builtin(cmd) = item
            && let Some((_, hint)) = self.hint_overrides.iter().find(|(c, _)| c == cmd)
        {
            return std::borrow::Cow::Owned(hint.clone());
        }
        platform_hint(item.keybinding_hint())
    }

    /// Inject the extension-contributed commands to interleave into the list,
    /// then re-rank. Called by the app when opening the palette.
    pub fn set_extension_commands(&mut self, extensions: Vec<ExtensionCommand>) {
        self.extensions = extensions;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    /// Put the user's menu commands first while the query is empty, then
    /// re-rank. Called by the app when opening the palette.
    pub fn set_pinned(&mut self, pinned: Vec<Command>) {
        self.pinned = pinned;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    /// Whether `item` is shown in the pinned block at the top of the list.
    pub fn is_pinned(&self, item: &PaletteItem) -> bool {
        self.query.trim().is_empty()
            && matches!(item, PaletteItem::Builtin(c) if self.pinned.contains(c) && !self.hidden.contains(c))
    }

    /// Leave `hidden` out of the list (see [`CommandPalette::hidden`]),
    /// then re-rank. Called by the app when opening the palette.
    pub fn set_hidden(&mut self, hidden: Vec<Command>) {
        self.hidden = hidden;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.to_string();
        self.cursor = self.query.chars().count();
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn push_char(&mut self, c: char) {
        picker_query::insert(&mut self.query, &mut self.cursor, c);
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn pop_char(&mut self) {
        if !picker_query::backspace(&mut self.query, &mut self.cursor) {
            return;
        }
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn delete_char(&mut self) {
        if !picker_query::delete(&mut self.query, self.cursor) {
            return;
        }
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_cursor_left(&mut self) {
        picker_query::left(&mut self.cursor);
    }

    pub fn move_cursor_right(&mut self) {
        picker_query::right(&self.query, &mut self.cursor);
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        picker_query::end(&self.query, &mut self.cursor);
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
            let (mut first, rest): (Vec<PaletteItem>, Vec<PaletteItem>) = all
                .into_iter()
                .partition(|i| matches!(i, PaletteItem::Builtin(c) if self.pinned.contains(c)));
            first.sort_by_key(|i| match i {
                PaletteItem::Builtin(c) => self.pinned.iter().position(|p| p == c),
                PaletteItem::Extension(_) => None,
            });
            first.extend(rest);
            self.results = first;
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

/// Draw the Command Palette over `area`. `center` is Quick Input Position:
/// Center; `backdrop` lets a `surfaces` theme dim what is behind the card
/// (the App withholds it where inline images would not dim with the text).
pub fn render_command_palette(
    palette: &mut CommandPalette,
    area: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
    center: bool,
    backdrop: bool,
) {
    if let Some(t) = crate::widgets::surface::tones(theme) {
        render_palette_card(palette, area, buf, theme, t, center, backdrop);
        return;
    }
    palette.last_caret = None;
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
        format!(
            " {} ",
            crate::i18n::tr("Command Palette — Esc to close, ↑/↓ to navigate, Enter to run")
        ),
        Style::default()
            .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.ui(picker_colours::BORDER)))
        .title(title.clone())
        .style(Style::default().bg(theme.ui(picker_colours::BODY)));
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
        theme.ui(picker_colours::SELECTION)
    };

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let query_style = Style::default()
        .fg(theme.ui(picker_colours::TEXT))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .fg(theme.ui(picker_colours::BODY))
        .bg(theme.ui(picker_colours::TEXT))
        .add_modifier(Modifier::SLOW_BLINK);
    let cursor = palette.cursor.min(palette.query.chars().count());
    let before: String = palette.query.chars().take(cursor).collect();
    let at: String = palette.query.chars().skip(cursor).take(1).collect();
    let after: String = palette.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let prompt_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.ui(picker_colours::PROMPT))),
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
            Style::default().fg(theme.ui(picker_colours::DIM)),
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
            Style::default().fg(theme.ui(picker_colours::TEXT))
        };
        let hint_style = if is_selected {
            Style::default()
                .bg(sel_bg)
                .fg(theme.ui(picker_colours::DETAIL_BRIGHT))
        } else {
            Style::default().fg(theme.ui(picker_colours::DETAIL))
        };
        let prefix = if is_selected {
            "> "
        } else if palette.is_pinned(cmd) {
            "\u{2605} "
        } else {
            "  "
        };
        let title = crate::i18n::tr(cmd.title());
        // Borrowed as written unless the platform respells it.
        let hint_spans: Vec<Span<'static>> = vec![Span::styled(palette.hint_for(cmd), hint_style)];
        // Right-align the keybinding hint: pad between the title and the hint
        // so the chord sits at the row's right edge, like VS Code.
        let hint_w: usize = hint_spans.iter().map(Span::width).sum();
        let used = 2 + title.chars().count() + hint_w;
        let pad = (list_rect.width as usize).saturating_sub(used).max(1);
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(title.to_string(), row_style),
            Span::styled(" ".repeat(pad), row_style),
        ];
        spans.extend(hint_spans);
        lines.push(Line::from(spans));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

/// The Command Palette as a `surfaces` theme draws it: the shared
/// quick-input card ([`crate::widgets::quick_input`]) with no title on its
/// edge, a `>` prompt, and rows that split a title at its first `: ` into a
/// muted category and the command's name, the query's matched characters
/// in bold strong ink ([`crate::widgets::quick_input::hit_style`]), and the
/// binding as keycaps at the right.
fn render_palette_card(
    palette: &mut CommandPalette,
    area: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
    t: crate::widgets::surface::Tones,
    center: bool,
    backdrop: bool,
) {
    use crate::widgets::quick_input::{self as qi, Card, Input, Row};
    let width = (area.width.saturating_mul(7) / 10).clamp(40, 100);
    let max_h = (area.height.saturating_mul(6) / 10).max(10);
    let total = palette.results.len();
    let card = Card::layout(area, width, max_h, total, center);
    palette.last_rect = card.rect;
    palette.last_inner_height = card.list.height;
    if backdrop {
        crate::widgets::surface::dim_backdrop(buf, card.rect, theme);
    }
    let filtering = !palette.query.trim().is_empty();
    let count = qi::count_label(total, filtering, false, "command", "commands");
    palette.last_caret = qi::paint_card(
        buf,
        &card,
        theme,
        t,
        &Input {
            glyph: ">",
            query: &palette.query,
            cursor: palette.cursor,
            placeholder: "Type a command",
            count: &count,
            verb: "run",
            keep: None,
        },
    );
    let visible = card.list.height as usize;
    if visible == 0 {
        return;
    }
    if total == 0 {
        qi::paint_empty(buf, &card, t, "No matching commands");
        return;
    }
    if palette.selected >= palette.scroll + visible {
        palette.scroll = palette.selected + 1 - visible;
    }
    if palette.selected < palette.scroll {
        palette.scroll = palette.selected;
    }
    let end = (palette.scroll + visible).min(total);
    let hit = qi::hit_style(t);
    for (offset, cmd) in palette.results[palette.scroll..end].iter().enumerate() {
        let selected = palette.scroll + offset == palette.selected;
        let title = crate::i18n::tr(cmd.title());
        let offsets = qi::match_offsets(&title, &palette.query);
        // `View: Toggle Terminal`: the category recedes, the name reads.
        let split = title.find(": ").map_or(0, |i| i + 2);
        let mut label = qi::highlighted(
            &title[..split],
            0,
            &offsets,
            Style::default().fg(t.muted()),
            hit,
        );
        label.extend(qi::highlighted(
            &title[split..],
            split,
            &offsets,
            Style::default().fg(t.text()),
            hit,
        ));
        label.retain(|s| !s.content.is_empty());
        let hint = palette.hint_for(cmd);
        let row_bg = if selected { t.band() } else { t.raised() };
        let trail = crate::widgets::surface::keycap_spans(&hint, t, row_bg, selected);
        qi::paint_row(
            buf,
            &card,
            offset as u16,
            t,
            Row {
                selected,
                glyph: palette
                    .is_pinned(cmd)
                    .then(|| Span::styled("\u{2605}", Style::default().fg(t.muted()))),
                label,
                trail,
            },
        );
    }
    qi::paint_scroll_cue(buf, &card, t, total, palette.scroll);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_registry::LAYOUT_COMMANDS;

    fn builtin(c: Command) -> PaletteItem {
        PaletteItem::Builtin(c)
    }

    #[test]
    fn a_chord_splits_into_its_keys() {
        use crate::widgets::surface::chord_keys;
        assert_eq!(chord_keys("Ctrl+Shift+P"), ["Ctrl", "Shift", "P"]);
        assert_eq!(chord_keys("F12"), ["F12"]);
        assert_eq!(chord_keys("Ctrl++"), ["Ctrl", "+"]);
        assert_eq!(chord_keys("\u{2318}+K"), ["\u{2318}", "K"]);
    }

    /// Croft Loch frames the palette as a card and shows chords
    /// as keycaps; every other theme keeps the plain hint text.
    #[test]
    fn croft_loch_draws_the_palette_hint_as_keycaps() {
        let render = |theme: crate::theme::Theme| {
            let mut palette = CommandPalette::new();
            palette.set_query("toggle terminal");
            let area = Rect::new(0, 0, 100, 30);
            let mut buf = Buffer::empty(area);
            render_command_palette(&mut palette, area, &mut buf, theme, false, false);
            (palette.last_rect, buf)
        };
        let (rect, buf) = render(crate::theme::Theme::from_id("loch"));
        let row: String = (rect.x..rect.right())
            .map(|x| buf[(x, rect.y + 3)].symbol().to_string())
            .collect();
        assert!(
            row.contains(" Ctrl + J "),
            "keycaps on the first row: {row:?}"
        );
        assert_eq!(buf[(rect.x, rect.y)].symbol(), "\u{2594}", "card top edge");
        let (rect, buf) = render(crate::theme::Theme::BLACK);
        let row: String = (rect.x..rect.right())
            .map(|x| buf[(x, rect.y + 3)].symbol().to_string())
            .collect();
        assert!(row.contains("Ctrl+J"), "plain hint on Croft Black: {row:?}");
    }

    fn loch_palette(query: &str, area: Rect) -> (CommandPalette, Buffer) {
        let mut palette = CommandPalette::new();
        palette.set_query(query);
        let mut buf = Buffer::empty(area);
        render_command_palette(
            &mut palette,
            area,
            &mut buf,
            crate::theme::Theme::from_id("loch"),
            false,
            false,
        );
        (palette, buf)
    }

    fn row_text(buf: &Buffer, rect: Rect, y: u16) -> String {
        (rect.x..rect.right())
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    /// Croft Loch: no title on the card's edge, and the card is as tall as
    /// its results with its top where the tallest card's would be.
    #[test]
    fn croft_loch_palette_is_a_titleless_card_that_hugs_its_results() {
        let area = Rect::new(0, 0, 120, 40);
        let (all, buf) = loch_palette("", area);
        let top = row_text(&buf, all.last_rect, all.last_rect.y);
        assert!(
            top.chars().all(|c| c == '\u{2594}'),
            "an unbroken top edge: {top:?}"
        );
        assert!(!top.contains("Command Palette"));
        let (one, buf) = loch_palette("toggle terminal", area);
        assert_eq!(one.last_rect.y, all.last_rect.y, "the top stays put");
        assert!(one.last_rect.height < all.last_rect.height);
        assert_eq!(
            one.last_inner_height,
            crate::widgets::quick_input::MIN_LIST_ROWS
        );
        let input = row_text(&buf, one.last_rect, one.last_rect.y + 1);
        assert!(input.contains("> toggle terminal"), "{input:?}");
        assert!(input.contains("match"), "a count: {input:?}");
        let footer = row_text(&buf, one.last_rect, one.last_rect.bottom() - 2);
        assert!(footer.contains("esc close"), "{footer:?}");
    }

    /// Croft Loch: the selected row is the band with the accent bar on the
    /// card's left edge, no `> ` marker; the title's category is muted, its
    /// name in body ink, and the matched characters in bold strong ink (not
    /// the accent, which stays for the selection).
    #[test]
    fn croft_loch_palette_rows_split_the_category_and_mark_the_match() {
        let t = crate::widgets::surface::tones(crate::theme::Theme::from_id("loch")).unwrap();
        let (palette, buf) = loch_palette("toggle terminal", Rect::new(0, 0, 120, 40));
        let rect = palette.last_rect;
        let y = rect.y + 3;
        let row = row_text(&buf, rect, y);
        assert!(
            row.starts_with("\u{258e}   View: Toggle Terminal"),
            "{row:?}"
        );
        assert!(!row.contains("> "), "{row:?}");
        assert_eq!(buf[(rect.x, y)].fg, t.accent(), "the bar is the accent");
        assert_eq!(buf[(rect.x + 1, y)].bg, t.band());
        let at = |s: &str| {
            let b = row.find(s).expect("on the row");
            rect.x + u16::try_from(row[..b].chars().count()).unwrap()
        };
        let view = at("View");
        assert_eq!(buf[(view, y)].fg, t.muted(), "the category recedes");
        let toggle = at("Toggle");
        assert_eq!(buf[(toggle, y)].fg, t.strong(), "a matched character");
        assert!(buf[(toggle, y)].modifier.contains(Modifier::BOLD));
        let teal = (rect.x + 1..rect.right())
            .flat_map(|x| (y..y + 3).map(move |y| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].fg == t.accent())
            .count();
        assert_eq!(teal, 0, "no teal in a row's text, only the bar");
        let second = (rect.x..rect.right())
            .map(|x| buf[(x, y + 1)].symbol().to_string())
            .collect::<String>();
        assert!(!second.starts_with('\u{258e}'), "one bar: {second:?}");
    }

    /// Croft Loch hands the caret to the terminal (no painted block); a
    /// boxed theme keeps its painted caret and asks for no terminal caret.
    #[test]
    fn croft_loch_palette_hands_the_caret_to_the_terminal() {
        let area = Rect::new(0, 0, 100, 30);
        let (palette, buf) = loch_palette("ab", area);
        let rect = palette.last_rect;
        assert_eq!(palette.last_caret, Some((rect.x + 6, rect.y + 1)));
        let blinking = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .any(|(x, y)| buf[(x, y)].modifier.contains(Modifier::SLOW_BLINK));
        assert!(!blinking, "no painted caret block");
        let mut black = CommandPalette::new();
        black.set_query("ab");
        let mut buf = Buffer::empty(area);
        render_command_palette(
            &mut black,
            area,
            &mut buf,
            crate::theme::Theme::BLACK,
            false,
            false,
        );
        assert_eq!(black.last_caret, None);
    }

    /// Croft Loch: no results is one centred, muted, italic line.
    #[test]
    fn croft_loch_palette_says_when_nothing_matches() {
        let t = crate::widgets::surface::tones(crate::theme::Theme::from_id("loch")).unwrap();
        let (palette, buf) = loch_palette("zzqqxxj", Rect::new(0, 0, 100, 30));
        assert!(palette.results.is_empty());
        let rect = palette.last_rect;
        let (y, x) = (0..rect.height)
            .map(|dy| rect.y + dy)
            .find_map(|y| {
                let row = row_text(&buf, rect, y);
                row.find("No matching commands")
                    .map(|b| (y, rect.x + u16::try_from(row[..b].chars().count()).unwrap()))
            })
            .expect("the empty state is drawn");
        assert_eq!(buf[(x, y)].fg, t.muted());
        assert!(buf[(x, y)].modifier.contains(Modifier::ITALIC));
    }

    /// Croft Loch dims the screen behind the palette when asked, and leaves
    /// it alone when the App withholds the scrim (inline images).
    #[test]
    fn croft_loch_palette_dims_its_backdrop_on_request() {
        let theme = crate::theme::Theme::from_id("loch");
        let area = Rect::new(0, 0, 100, 30);
        let draw = |backdrop: bool| {
            let mut palette = CommandPalette::new();
            let mut buf = Buffer::empty(area);
            buf.set_string(
                0,
                29,
                "behind",
                Style::default().fg(Color::Rgb(0xd0, 0xd0, 0xd0)),
            );
            render_command_palette(&mut palette, area, &mut buf, theme, false, backdrop);
            buf[(0, 29)].fg
        };
        assert_eq!(draw(false), Color::Rgb(0xd0, 0xd0, 0xd0));
        assert_ne!(draw(true), Color::Rgb(0xd0, 0xd0, 0xd0));
    }

    #[test]
    fn empty_query_lists_every_command() {
        let palette = CommandPalette::new();
        assert_eq!(palette.results.len(), ALL_COMMANDS.len());
        assert_eq!(palette.results.first(), Some(&builtin(Command::MoveLineUp)));
    }

    /// #1212: the caret-driven LSP actions were F-key checks only, so the
    /// palette could not find them and keybindings.json could not name them.
    #[test]
    fn lsp_navigation_and_rename_are_palette_commands() {
        for (query, title, id, hint) in [
            (
                "go to definition",
                "Go to Definition",
                "go_to_definition",
                "F12",
            ),
            (
                "go to references",
                "Go to References",
                "go_to_references",
                "Shift+F12",
            ),
            (
                "go to declaration",
                "Go to Declaration",
                "go_to_declaration",
                "Ctrl+Shift+F12",
            ),
            (
                "type definition",
                "Go to Type Definition",
                "go_to_type_definition",
                "Ctrl+F12",
            ),
            (
                "implementation",
                "Go to Implementations",
                "go_to_implementations",
                "Cmd+F12",
            ),
            ("rename symbol", "Rename Symbol", "rename_symbol", "F2"),
            (
                "suggest",
                "Trigger Suggest",
                "trigger_suggest",
                "Ctrl+Space",
            ),
            // #1518: VS Code's title and chord, so "hover" finds it.
            ("hover", "Show or Focus Hover", "show_hover", "Cmd+K Cmd+I"),
        ] {
            let mut palette = CommandPalette::new();
            palette.set_query(query);
            let found = palette.results.iter().find(|item| item.title() == title);
            assert!(found.is_some(), "{query:?} should list {title:?}");
            assert_eq!(found.unwrap().keybinding_hint(), hint, "{title}");
            let cmd = Command::from_id(id);
            assert!(cmd.is_some(), "{id:?} should be a command id");
            assert_eq!(cmd.unwrap().title(), title);
        }
    }

    /// The peek and pointer-driven forms keep their own ids and titles; the
    /// new caret commands sit beside them rather than replacing them.
    #[test]
    fn peek_and_mouse_definition_commands_are_unchanged() {
        for (id, title) in [
            ("peek_definition", "Peek Definition"),
            ("peek_references", "Peek References"),
            (
                "mouse_go_to_definition_at_click",
                "Mouse: Go to Definition at Click",
            ),
        ] {
            assert_eq!(Command::from_id(id).map(Command::title), Some(title));
        }
        let mut palette = CommandPalette::new();
        palette.set_query("go to definition");
        assert!(
            palette
                .results
                .contains(&builtin(Command::MouseGoToDefinitionAtClick))
        );
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
    /// croft has for it (Close Others is croft's own Cmd+K Shift+X, #2072:
    /// VS Code's ⌥⌘T never reaches croft).
    #[test]
    fn the_editor_tab_commands_are_found_under_their_vscode_titles() {
        let mut palette = CommandPalette::new();
        for (title, chord) in [
            ("View: Close Other Editors in Group", "Cmd+K Shift+X"),
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

    /// #1621: "restart" and "language server" find both restart commands,
    /// and so do VS Code's own names for them.
    #[test]
    fn restart_queries_find_the_language_server_restarts() {
        let mut palette = CommandPalette::new();
        for query in [
            "restart",
            "language server",
            "Restart Language Server",
            "restart ts server",
        ] {
            palette.set_query(query);
            let titles: Vec<&str> = palette.results.iter().map(PaletteItem::title).collect();
            for title in [
                "Language Servers: Restart All",
                "Language Servers: Restart for This File",
            ] {
                assert!(
                    titles.contains(&title),
                    "{query:?} must find {title:?}; it found {titles:?}"
                );
            }
        }
        assert_eq!(
            Command::from_id("restart_language_servers"),
            Some(Command::RestartLanguageServers)
        );
    }

    /// Negative (#1621): an unrelated query does not surface them.
    #[test]
    fn an_unrelated_query_leaves_the_restarts_out() {
        let mut palette = CommandPalette::new();
        palette.set_query("toggle word wrap");
        let titles: Vec<&str> = palette.results.iter().map(PaletteItem::title).collect();
        assert!(
            !titles.iter().any(|t| t.starts_with("Language Servers:")),
            "{titles:?}"
        );
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
        // The plain hint; Croft Loch spells it as keycaps (`Ctrl + /`).
        render_command_palette(
            &mut palette,
            area,
            &mut buf,
            crate::theme::Theme::BLACK,
            false,
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
}
