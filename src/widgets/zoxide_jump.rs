//! The Explorer "jump" popup (Cmd+Z): a zoxide-backed fuzzy directory
//! jumper. The user types keywords, sees zoxide's frecency-ranked
//! matches, and Enter re-roots the workspace there (and `cd`s the
//! terminal — see `App::change_workspace_root`).
//!
//! This widget is deliberately pure: it owns the query text, the result
//! list, and the selection, but never spawns zoxide itself. The app
//! layer runs `crate::zoxide::query` and feeds results in via
//! `set_results`, which keeps every behaviour here unit-testable without
//! a zoxide binary and concentrates the one subprocess call off the
//! render path.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};
use std::path::{Path, PathBuf};

use crate::zoxide::InstallState;

pub struct ZoxideJump {
    pub query: String,
    pub cursor: usize,
    pub results: Vec<PathBuf>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    pub last_inner_height: u16,
    /// False once a `set_results(None)` reports the zoxide binary could
    /// not be found on this host; drives the "unavailable" message.
    pub available: bool,
    /// The full frecency-ranked database, captured once when the popup
    /// opens (`zoxide query -l`). The app layer reads this to run the
    /// typo-tolerant fuzzy fallback without re-spawning zoxide on every
    /// keystroke that misses.
    pub all_dirs: Vec<PathBuf>,
    /// True when `results` came from the fuzzy fallback rather than a
    /// strict zoxide match; drives the "no exact match" hint so an
    /// approximate result is never mistaken for a real frecency hit.
    pub approximate: bool,
    /// Lifecycle of the background zoxide install, injected by the app
    /// layer (`crate::zoxide::install_state()`) whenever results are
    /// refreshed, so the "unavailable" message reports what actually
    /// happened instead of a perpetual "installing".
    pub install_state: InstallState,
    /// True on Termux, injected at popup open; selects the platform's
    /// install hint (`pkg install zoxide` vs the manual route).
    pub termux: bool,
}

impl Default for ZoxideJump {
    fn default() -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
            available: true,
            all_dirs: Vec::new(),
            approximate: false,
            install_state: InstallState::Idle,
            termux: false,
        }
    }
}

/// The line shown when the zoxide binary cannot be found, honest about the
/// background install: claim "installing" only while the installer is
/// actually running, and once it has failed, say so and name the
/// platform's install command instead.
pub fn unavailable_message(state: InstallState, termux: bool) -> &'static str {
    match (state, termux) {
        (InstallState::Running, _) => {
            "  zoxide is not installed on this host. Installing it in the background; try again shortly."
        }
        (InstallState::Failed, true) => {
            "  zoxide auto-install failed. Run `pkg install zoxide` in the terminal pane, then try again."
        }
        (InstallState::Failed, false) => {
            "  zoxide auto-install failed. Install it manually (e.g. `brew install zoxide`), then try again."
        }
        _ => "  zoxide is not installed on this host.",
    }
}

impl ZoxideJump {
    pub fn new() -> Self {
        Self::default()
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

    /// Replace the result list after the app re-queries zoxide. `None`
    /// means the binary is unavailable; `Some(paths)` is zoxide's ranked
    /// output (possibly empty). Selection/scroll reset because the list
    /// just changed shape.
    pub fn set_results(&mut self, results: Option<Vec<PathBuf>>) {
        match results {
            Some(paths) => {
                self.available = true;
                self.results = paths;
            }
            None => {
                self.available = false;
                self.results = Vec::new();
            }
        }
        self.approximate = false;
        self.selected = 0;
        self.scroll = 0;
    }

    /// Cache the full frecency database so the fuzzy fallback can rank
    /// against it without re-querying zoxide. Called once at popup open.
    pub fn set_all_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.all_dirs = dirs;
    }

    /// Install fuzzy-fallback results, marking the list approximate so the
    /// UI flags that no strict match existed. Selection/scroll reset.
    pub fn set_approximate_results(&mut self, paths: Vec<PathBuf>) {
        self.available = true;
        self.results = paths;
        self.approximate = true;
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn push_char(&mut self, c: char) {
        let at = self.byte_offset(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.query.remove(at);
        self.cursor -= 1;
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.query.remove(at);
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

    pub fn selected_path(&self) -> Option<&Path> {
        self.results.get(self.selected).map(|p| p.as_path())
    }

    /// The result index at screen row `y`, if `y` lands on a visible row.
    /// The list body starts three rows below `last_rect.y` (top border, the
    /// query prompt, then the separator or approximate-match hint) and runs
    /// `last_inner_height` rows, so this stays in lock-step with
    /// [`render_zoxide_jump`]. Used to map a mouse click to a result row.
    pub fn row_index_at(&self, y: u16) -> Option<usize> {
        let list_top = self.last_rect.y.saturating_add(3);
        if y < list_top || y - list_top >= self.last_inner_height {
            return None;
        }
        let idx = self.scroll + (y - list_top) as usize;
        (idx < self.results.len()).then_some(idx)
    }

    #[cfg(test)]
    pub fn selected_index(&self) -> usize {
        self.selected
    }
}

/// Collapse a leading `$HOME` to `~` for display, so the result list
/// reads like the shell (`~/Documents/croft`) instead of a long absolute
/// path. Purely cosmetic; the jump still uses the absolute `PathBuf`.
fn display_path(path: &Path) -> String {
    match std::env::var_os("HOME") {
        Some(home) => collapse_home(path, &home.to_string_lossy()),
        None => path.to_string_lossy().into_owned(),
    }
}

/// The collapsing itself, with `home` passed in. Split out from
/// [`display_path`] so its test needs no `$HOME`: the env var is
/// process-global, and repointing it at a directory that cannot be created
/// broke unrelated tests running in parallel that resolve `$HOME` to reach
/// the cache dir.
fn collapse_home(path: &Path, home: &str) -> String {
    let full = path.to_string_lossy();
    if !home.is_empty() {
        if full == home {
            return String::from("~");
        }
        if let Some(rest) = full.strip_prefix(&format!("{home}/")) {
            return format!("~/{rest}");
        }
    }
    full.into_owned()
}

/// Draw the popup into `rect` exactly as given. Placement (anchoring over
/// the Explorer column vs. a centered fallback) is decided by the caller
/// in `App::render_zoxide_jump`, which knows the pane layout; this widget
/// just fills the box it is handed.
pub fn render_zoxide_jump(
    jump: &mut ZoxideJump,
    rect: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
) {
    jump.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        " Jump (zoxide) — Esc to close, ↑/↓ to navigate, Enter to jump ",
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
    // Black theme: gradient border over the solid one, then re-stamp the title.
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
    let cursor = jump.cursor.min(jump.query.chars().count());
    let before: String = jump.query.chars().take(cursor).collect();
    let at: String = jump.query.chars().skip(cursor).take(1).collect();
    let after: String = jump.query.chars().skip(cursor + 1).collect();
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
    let sep_line = if jump.approximate {
        Line::from(Span::styled(
            "≈ no exact match — closest by spelling",
            Style::default()
                .fg(theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)))
                .add_modifier(Modifier::ITALIC),
        ))
    } else {
        Line::from(Span::styled(
            "─".repeat(separator_rect.width as usize),
            Style::default().fg(theme.ui(Color::Rgb(0x3b, 0x42, 0x52))),
        ))
    };
    Widget::render(Paragraph::new(sep_line), separator_rect, buf);

    let list_rect = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    jump.last_inner_height = list_rect.height;
    if list_rect.height == 0 {
        return;
    }

    if !jump.available {
        let msg = Line::from(Span::styled(
            unavailable_message(jump.install_state, jump.termux),
            Style::default().fg(theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b))),
        ));
        // Wrapped, not clipped: the popup is as narrow as the Explorer
        // column (phone-width on Termux), and the install hint's tail is
        // the actionable part.
        Widget::render(
            Paragraph::new(msg).wrap(Wrap { trim: false }),
            list_rect,
            buf,
        );
        return;
    }

    let visible = list_rect.height as usize;
    let total = jump.results.len();
    if jump.selected >= jump.scroll + visible {
        jump.scroll = jump.selected + 1 - visible;
    }
    if jump.selected < jump.scroll {
        jump.scroll = jump.selected;
    }
    let end = (jump.scroll + visible).min(total);

    if total == 0 {
        let empty = if jump.query.trim().is_empty() {
            Line::from(Span::styled(
                "  (no directories in the zoxide database yet)",
                Style::default().fg(theme.ui(Color::Rgb(0x7a, 0x82, 0x90))),
            ))
        } else {
            Line::from(Span::styled(
                format!("  No matches for '{}'", jump.query),
                Style::default().fg(theme.ui(Color::Rgb(0x7a, 0x82, 0x90))),
            ))
        };
        Widget::render(Paragraph::new(empty), list_rect, buf);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - jump.scroll);
    for (offset, path) in jump.results[jump.scroll..end].iter().enumerate() {
        let row_idx = jump.scroll + offset;
        let is_selected = row_idx == jump.selected;
        let row_style = if is_selected {
            Style::default().bg(sel_bg).fg(theme.ui(Color::White))
        } else {
            Style::default().fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        };
        let prefix = if is_selected { "> " } else { "  " };
        let spans: Vec<Span<'static>> = vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(display_path(path), row_style),
        ];
        lines.push(Line::from(spans));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(rels: &[&str]) -> Vec<PathBuf> {
        rels.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn set_results_some_marks_available_and_resets_selection() {
        let mut j = ZoxideJump::new();
        j.set_results(Some(paths(&["/a", "/b", "/c"])));
        j.select_next();
        j.select_next();
        assert_eq!(j.selected_index(), 2);
        j.set_results(Some(paths(&["/x"])));
        assert!(j.available);
        assert_eq!(
            j.selected_index(),
            0,
            "new result list must reset selection"
        );
        assert_eq!(j.results.len(), 1);
    }

    #[test]
    fn approximate_flag_tracks_fallback_vs_strict_results() {
        let mut j = ZoxideJump::new();
        j.set_approximate_results(paths(&["/a/pr-split"]));
        assert!(
            j.approximate,
            "fallback results must mark the list approximate"
        );
        assert!(j.available);
        assert_eq!(j.selected_index(), 0);
        // A subsequent strict result must clear the approximate flag.
        j.set_results(Some(paths(&["/a/exact"])));
        assert!(
            !j.approximate,
            "a strict result must reset the approximate flag"
        );
    }

    #[test]
    fn unavailable_message_reflects_install_state_and_platform() {
        assert!(
            unavailable_message(InstallState::Running, true)
                .to_lowercase()
                .contains("installing"),
            "while the installer runs, saying so is the truth"
        );
        assert!(
            unavailable_message(InstallState::Failed, true).contains("pkg install zoxide"),
            "a failed install on Termux must hand the user the pkg command"
        );
        let failed_elsewhere = unavailable_message(InstallState::Failed, false);
        assert!(
            !failed_elsewhere.contains("pkg install"),
            "pkg is Termux-only; never suggest it on macOS/Linux"
        );
        assert!(
            failed_elsewhere.contains("brew install zoxide"),
            "the non-Termux failed message must name a manual install route"
        );
        for termux in [false, true] {
            assert!(
                !unavailable_message(InstallState::Idle, termux)
                    .to_lowercase()
                    .contains("installing"),
                "must not claim an install is happening when none was started"
            );
            assert!(
                !unavailable_message(InstallState::Done, termux)
                    .to_lowercase()
                    .contains("installing"),
                "must not claim an install is happening after it finished"
            );
        }
    }

    #[test]
    fn unavailable_message_tail_stays_visible_in_a_narrow_popup() {
        let mut j = ZoxideJump::new();
        j.set_results(None);
        j.install_state = InstallState::Failed;
        j.termux = true;
        let rect = Rect::new(0, 0, 30, 12);
        let mut buf = Buffer::empty(rect);
        render_zoxide_jump(&mut j, rect, &mut buf, crate::theme::Theme::default());
        let screen: String = (0..rect.height)
            .map(|y| {
                (0..rect.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains("zoxide"),
            "message must render at all: {screen}"
        );
        assert!(
            screen.contains("again"),
            "the end of the message must wrap into view instead of being \
             truncated by the popup's right edge (phone-width Explorer): {screen}"
        );
    }

    #[test]
    fn set_results_none_marks_unavailable() {
        let mut j = ZoxideJump::new();
        j.set_results(None);
        assert!(
            !j.available,
            "None from query means the zoxide binary is missing"
        );
        assert!(j.results.is_empty());
        assert!(j.selected_path().is_none());
    }

    #[test]
    fn selected_path_tracks_navigation() {
        let mut j = ZoxideJump::new();
        j.set_results(Some(paths(&["/one", "/two", "/three"])));
        assert_eq!(j.selected_path(), Some(Path::new("/one")));
        j.select_next();
        assert_eq!(j.selected_path(), Some(Path::new("/two")));
        j.select_prev();
        assert_eq!(j.selected_path(), Some(Path::new("/one")));
    }

    #[test]
    fn select_next_caps_at_last_and_prev_caps_at_zero() {
        let mut j = ZoxideJump::new();
        j.set_results(Some(paths(&["/a", "/b"])));
        j.select_next();
        j.select_next();
        j.select_next();
        assert_eq!(j.selected_index(), 1, "select_next must not overshoot");
        j.select_prev();
        j.select_prev();
        j.select_prev();
        assert_eq!(j.selected_index(), 0, "select_prev must not underflow");
    }

    #[test]
    fn cursor_supports_mid_string_insert_and_delete() {
        let mut j = ZoxideJump::new();
        for c in "abc".chars() {
            j.push_char(c);
        }
        assert_eq!((j.query.as_str(), j.cursor), ("abc", 3));
        j.move_cursor_left();
        j.move_cursor_left();
        assert_eq!(j.cursor, 1);
        j.push_char('X');
        assert_eq!((j.query.as_str(), j.cursor), ("aXbc", 2));
        j.pop_char();
        assert_eq!((j.query.as_str(), j.cursor), ("abc", 1));
        j.delete_char();
        assert_eq!((j.query.as_str(), j.cursor), ("ac", 1));
        j.move_cursor_home();
        assert_eq!(j.cursor, 0);
        j.move_cursor_end();
        assert_eq!(j.cursor, 2);
        j.move_cursor_right();
        assert_eq!(
            j.cursor, 2,
            "cursor must not move past the end of the query"
        );
    }

    #[test]
    fn display_path_collapses_home_to_tilde() {
        // `collapse_home`, not `display_path`: taking the home directory as an
        // argument keeps this test off the process-global $HOME that every
        // other test in the binary reads concurrently.
        assert_eq!(
            collapse_home(Path::new("/Users/v/Documents/croft"), "/Users/v"),
            "~/Documents/croft"
        );
        assert_eq!(collapse_home(Path::new("/Users/v"), "/Users/v"), "~");
        assert_eq!(
            collapse_home(Path::new("/etc/hosts"), "/Users/v"),
            "/etc/hosts"
        );
        // A prefix that only looks like home must not be collapsed.
        assert_eq!(
            collapse_home(Path::new("/Users/vitali/x"), "/Users/v"),
            "/Users/vitali/x"
        );
        // No home at all leaves the path untouched.
        assert_eq!(collapse_home(Path::new("/Users/v"), ""), "/Users/v");
    }
}
