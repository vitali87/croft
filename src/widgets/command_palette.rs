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
    TransformUpper,
    TransformLower,
    TransformTitle,
    SortLinesAscending,
    SortLinesDescending,
    TrimTrailingWhitespace,
    ToggleWordWrap,
    AddCursorAbove,
    AddCursorBelow,
    // --- File / editor management ---
    SaveFile,
    CloseEditor,
    SplitEditor,
    QuickOpen,
    ToggleVimMode,
    // --- View / navigation ---
    ShowExplorer,
    ShowSearch,
    ShowSourceControl,
    ShowRunDebug,
    ShowRemote,
    ToggleSideBar,
    ToggleTerminal,
    NewTerminal,
    // --- Run / debug ---
    StartDebugging,
    StopDebugging,
    ToggleBreakpoint,
    StepOver,
    AttachPythonProcess,
    ColorTheme,
    KeyboardShortcuts,
}

/// Every command, in palette display order. Single source of truth for both
/// the empty-query list and the test that guards the count.
pub const ALL_COMMANDS: &[Command] = &[
    Command::MoveLineUp,
    Command::MoveLineDown,
    Command::ToggleLineComment,
    Command::ToggleBlockComment,
    Command::JoinLines,
    Command::TransformUpper,
    Command::TransformLower,
    Command::TransformTitle,
    Command::SortLinesAscending,
    Command::SortLinesDescending,
    Command::TrimTrailingWhitespace,
    Command::ToggleWordWrap,
    Command::AddCursorAbove,
    Command::AddCursorBelow,
    Command::SaveFile,
    Command::CloseEditor,
    Command::SplitEditor,
    Command::QuickOpen,
    Command::ToggleVimMode,
    Command::ShowExplorer,
    Command::ShowSearch,
    Command::ShowSourceControl,
    Command::ShowRunDebug,
    Command::ShowRemote,
    Command::ToggleSideBar,
    Command::ToggleTerminal,
    Command::NewTerminal,
    Command::StartDebugging,
    Command::StopDebugging,
    Command::ToggleBreakpoint,
    Command::StepOver,
    Command::AttachPythonProcess,
    Command::ColorTheme,
    Command::KeyboardShortcuts,
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
            Command::TransformUpper => "Transform to Uppercase",
            Command::TransformLower => "Transform to Lowercase",
            Command::TransformTitle => "Transform to Title Case",
            Command::SortLinesAscending => "Sort Lines Ascending",
            Command::SortLinesDescending => "Sort Lines Descending",
            Command::TrimTrailingWhitespace => "Trim Trailing Whitespace",
            Command::ToggleWordWrap => "View: Toggle Word Wrap",
            Command::AddCursorAbove => "Add Cursor Above",
            Command::AddCursorBelow => "Add Cursor Below",
            Command::SaveFile => "File: Save",
            Command::CloseEditor => "View: Close Editor",
            Command::SplitEditor => "View: Split Editor",
            Command::QuickOpen => "Go to File...",
            Command::ToggleVimMode => "Toggle Vim Mode",
            Command::ShowExplorer => "View: Show Explorer",
            Command::ShowSearch => "View: Show Search",
            Command::ShowSourceControl => "View: Show Source Control",
            Command::ShowRunDebug => "View: Show Run and Debug",
            Command::ShowRemote => "View: Show Remote",
            Command::ToggleSideBar => "View: Toggle Primary Side Bar",
            Command::ToggleTerminal => "View: Toggle Terminal",
            Command::NewTerminal => "Terminal: Create New Terminal",
            Command::StartDebugging => "Debug: Start Debugging",
            Command::StopDebugging => "Debug: Stop Debugging",
            Command::ToggleBreakpoint => "Debug: Toggle Breakpoint",
            Command::StepOver => "Debug: Step Over",
            Command::AttachPythonProcess => "Debug: Attach to Python Process",
            Command::ColorTheme => "Preferences: Color Theme",
            Command::KeyboardShortcuts => "Help: Keyboard Shortcuts Reference",
        }
    }

    /// The default key-binding hint shown right-aligned on the row, or an
    /// empty string for palette-only commands. The label uses the macOS chord
    /// names; on Linux/Android the command modifier is `Ctrl`.
    pub fn keybinding_hint(self) -> &'static str {
        match self {
            Command::MoveLineUp => "Alt+↑",
            Command::MoveLineDown => "Alt+↓",
            Command::ToggleLineComment => "Cmd+/",
            Command::ToggleBlockComment => "Shift+Alt+A",
            Command::ToggleWordWrap => "Alt+Z",
            Command::AddCursorAbove => "Cmd+Opt+↑",
            Command::AddCursorBelow => "Cmd+Opt+↓",
            Command::SaveFile => "Cmd+S",
            Command::CloseEditor => "Cmd+W",
            Command::SplitEditor => "Cmd+\\",
            Command::QuickOpen => "Cmd+P",
            Command::ToggleVimMode => "Cmd+E",
            Command::ShowExplorer => "Cmd+Shift+E",
            Command::ShowSearch => "Cmd+Shift+F",
            Command::ShowSourceControl => "Cmd+Shift+S",
            Command::ShowRunDebug => "Cmd+Shift+D",
            Command::ShowRemote => "Cmd+Shift+R",
            Command::ToggleSideBar => "Cmd+B",
            Command::ToggleTerminal => "Ctrl+J",
            Command::NewTerminal => "Cmd+T",
            Command::KeyboardShortcuts => "F1",
            Command::StartDebugging => "F5",
            Command::StopDebugging => "Shift+F5",
            Command::ToggleBreakpoint => "F9",
            Command::StepOver => "F10",
            _ => "",
        }
    }
}

/// The palette's owned state: the query being typed and which filtered row is
/// selected. Mirrors `FileFinder` so the App can drive both the same way.
#[derive(Default)]
pub struct CommandPalette {
    pub query: String,
    pub cursor: usize,
    pub results: Vec<Command>,
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
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
        };
        me.refresh_results();
        me
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

    pub fn selected_command(&self) -> Option<Command> {
        self.results.get(self.selected).copied()
    }

    /// Re-rank the command list against the current query. An empty query
    /// shows every command in declaration order; otherwise commands are kept
    /// only when their lower-cased title fuzzy-matches the needle, ranked by
    /// score (best first), ties broken by declaration order for stability.
    fn refresh_results(&mut self) {
        let needle = self.query.trim().to_lowercase();
        if needle.is_empty() {
            self.results = ALL_COMMANDS.to_vec();
            return;
        }
        let mut scored: Vec<(i32, usize, Command)> = ALL_COMMANDS
            .iter()
            .enumerate()
            .filter_map(|(idx, &cmd)| {
                let title_lower = cmd.title().to_lowercase();
                fuzzy_score(&needle, &title_lower, 0).map(|score| (score, idx, cmd))
            })
            .collect();
        // Higher score first; equal scores keep declaration order.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.results = scored.into_iter().map(|(_, _, cmd)| cmd).collect();
    }
}

pub fn render_command_palette(
    palette: &mut CommandPalette,
    area: Rect,
    buf: &mut Buffer,
    gradient: bool,
) {
    let width = area.width.saturating_mul(7) / 10;
    let width = width.clamp(40, 100.min(area.width));
    let height = area.height.saturating_mul(6) / 10;
    let height = height.clamp(10, area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 4;
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
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(title.clone())
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    if gradient {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    let sel_bg = if gradient {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        Color::Rgb(0x1e, 0x3a, 0x6e)
    };

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let query_style = Style::default()
        .fg(Color::Rgb(0xec, 0xef, 0xf4))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .fg(Color::Rgb(0x16, 0x18, 0x1f))
        .bg(Color::Rgb(0xec, 0xef, 0xf4))
        .add_modifier(Modifier::SLOW_BLINK);
    let cursor = palette.cursor.min(palette.query.chars().count());
    let before: String = palette.query.chars().take(cursor).collect();
    let at: String = palette.query.chars().skip(cursor).take(1).collect();
    let after: String = palette.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let prompt_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0))),
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
        Style::default().fg(Color::Rgb(0x3b, 0x42, 0x52)),
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
            Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
        ));
        Widget::render(Paragraph::new(empty), list_rect, buf);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - palette.scroll);
    for (offset, cmd) in palette.results[palette.scroll..end].iter().enumerate() {
        let row_idx = palette.scroll + offset;
        let is_selected = row_idx == palette.selected;
        let row_style = if is_selected {
            Style::default().bg(sel_bg).fg(Color::White)
        } else {
            Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4))
        };
        let hint_style = if is_selected {
            Style::default().bg(sel_bg).fg(Color::Rgb(0xa0, 0xb4, 0xd8))
        } else {
            Style::default().fg(Color::Rgb(0x8e, 0x95, 0xa4))
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

    #[test]
    fn empty_query_lists_every_command() {
        let palette = CommandPalette::new();
        assert_eq!(palette.results.len(), ALL_COMMANDS.len());
        assert_eq!(palette.results.first(), Some(&Command::MoveLineUp));
    }

    #[test]
    fn query_filters_by_fuzzy_title() {
        let mut palette = CommandPalette::new();
        palette.set_query("comment");
        assert!(palette.results.contains(&Command::ToggleLineComment));
        assert!(palette.results.contains(&Command::ToggleBlockComment));
        assert!(!palette.results.contains(&Command::SaveFile));
    }

    #[test]
    fn query_matches_subsequence() {
        let mut palette = CommandPalette::new();
        palette.set_query("sortasc");
        assert_eq!(palette.results.first(), Some(&Command::SortLinesAscending));
    }

    #[test]
    fn no_match_yields_empty_results() {
        let mut palette = CommandPalette::new();
        palette.set_query("zzzznotacommand");
        assert!(palette.results.is_empty());
        assert_eq!(palette.selected_command(), None);
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
        assert_eq!(palette.selected_command(), Some(Command::MoveLineDown));
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
