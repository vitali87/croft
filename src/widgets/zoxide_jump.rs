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
    /// The rc file an install would add the shell hook to (`~/.bashrc`),
    /// injected at popup open so the install offer names it up front;
    /// `None` when the shell is unsupported and no rc would be touched.
    pub rc_hint: Option<String>,
    /// What the last finished install came to (`crate::zoxide::install_outcome`),
    /// injected with `install_state`; a failure's reason is shown in the popup.
    pub install_note: Option<String>,
    /// The rc file to offer zoxide's `j` hook to (`~/.bashrc`) when zoxide
    /// is installed but no hook feeds its database (#926), injected at
    /// popup open; `None` when there is nothing to offer.
    pub hook_offer: Option<String>,
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
            rc_hint: None,
            install_note: None,
            hook_offer: None,
        }
    }
}

/// The empty list's text when zoxide is installed but `rc` has no hook
/// feeding it (#926), one `\n` per line: what is missing, and the offer.
pub fn hook_offer_message(rc: &str) -> String {
    format!(
        "No directories yet: {rc} has no zoxide hook, so no terminal `cd` adds one.\nAdd its `j` hook there? [y/N]\n\ny appends a block marked \"croft zoxide (managed)\"."
    )
}

/// The text shown when the zoxide binary cannot be found, one `\n` per line
/// (#926). Nothing is installed until the user says so: before any attempt
/// it offers the install, naming the exact command `y` runs and the rc file
/// it edits; while that install runs it says so; once it has failed it gives
/// the reason and the platform's own install route, and `y` tries again.
pub fn unavailable_message(jump: &ZoxideJump) -> String {
    match jump.install_state {
        InstallState::Running => String::from(
            "Installing zoxide in the background; the list appears here when it is done.",
        ),
        InstallState::Failed => {
            let manual = if jump.termux {
                "Run `pkg install zoxide` in the terminal pane"
            } else if cfg!(target_os = "macos") {
                "Install it yourself (e.g. `brew install zoxide`)"
            } else {
                "Install it with your package manager (e.g. `apt install zoxide`)"
            };
            let reason = jump.install_note.as_deref().unwrap_or("no reason given");
            format!("zoxide install failed: {reason}\n\n{manual}, or press y to try again.")
        }
        InstallState::Idle | InstallState::Done => {
            let hook = match &jump.rc_hint {
                Some(rc) => format!(
                    "then adds its `j` shell hook to {rc}, in a block marked \"croft zoxide (managed)\"."
                ),
                None => String::from("No shell rc file is changed."),
            };
            format!(
                "zoxide is not installed on this host. Install it? [y/N]\n\ny runs: {}\n{hook}",
                crate::zoxide::install_script(jump.termux)
            )
        }
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

    /// Whether the popup is asking `[y/N]` rather than taking a query: the
    /// install offer when zoxide is missing, or the hook offer (#926).
    pub fn asks_yes_no(&self) -> bool {
        !self.available || self.hook_offer_shown()
    }

    /// Whether the list shows the hook offer: zoxide is installed, nothing
    /// is typed, and its database is empty with no hook to feed it.
    pub fn hook_offer_shown(&self) -> bool {
        self.available
            && self.hook_offer.is_some()
            && self.query.is_empty()
            && self.results.is_empty()
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
        crate::widgets::surface::paint_overlay_frame(buf, rect, theme);
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
        let style = Style::default().fg(theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)));
        let msg: Vec<Line<'static>> = unavailable_message(jump)
            .lines()
            .map(|line| Line::from(Span::styled(line.to_string(), style)))
            .collect();
        // Wrapped, not clipped: the popup is as narrow as the Explorer
        // column (phone-width on Termux), and the install hint's tail is
        // the actionable part. Inset two cells so wrapped lines align.
        let text_rect = Rect {
            x: list_rect.x + 2,
            width: list_rect.width.saturating_sub(2),
            ..list_rect
        };
        Widget::render(
            Paragraph::new(msg).wrap(Wrap { trim: false }),
            text_rect,
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

    if let Some(rc) = jump
        .hook_offer
        .as_deref()
        .filter(|_| jump.hook_offer_shown())
    {
        let style = Style::default().fg(theme.ui(Color::Rgb(0xe5, 0xc0, 0x7b)));
        let msg: Vec<Line<'static>> = hook_offer_message(rc)
            .lines()
            .map(|line| Line::from(Span::styled(line.to_string(), style)))
            .collect();
        let text_rect = Rect {
            x: list_rect.x + 2,
            width: list_rect.width.saturating_sub(2),
            ..list_rect
        };
        Widget::render(
            Paragraph::new(msg).wrap(Wrap { trim: false }),
            text_rect,
            buf,
        );
        return;
    }
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

    fn missing(state: InstallState, termux: bool) -> ZoxideJump {
        let mut j = ZoxideJump::new();
        j.set_results(None);
        j.install_state = state;
        j.termux = termux;
        j
    }

    #[test]
    fn unavailable_message_reflects_install_state_and_platform() {
        assert!(
            unavailable_message(&missing(InstallState::Running, true))
                .to_lowercase()
                .contains("installing"),
            "while the installer runs, saying so is the truth"
        );
        assert!(
            unavailable_message(&missing(InstallState::Failed, true))
                .contains("pkg install zoxide"),
            "a failed install on Termux must hand the user the pkg command"
        );
        let failed_elsewhere = unavailable_message(&missing(InstallState::Failed, false));
        assert!(
            !failed_elsewhere.contains("pkg install"),
            "pkg is Termux-only; never suggest it on macOS/Linux"
        );
        assert!(
            failed_elsewhere.contains(if cfg!(target_os = "macos") {
                "brew install zoxide"
            } else {
                "apt install zoxide"
            }),
            "the non-Termux failed message must name this platform's install route"
        );
        for termux in [false, true] {
            for state in [InstallState::Idle, InstallState::Done] {
                assert!(
                    !unavailable_message(&missing(state, termux))
                        .to_lowercase()
                        .contains("installing"),
                    "must not claim an install is happening when none is running"
                );
            }
        }
    }

    /// #926: with zoxide missing and nothing asked yet, the popup asks before
    /// installing, and shows the exact command and the rc file it would edit.
    #[test]
    fn a_missing_zoxide_is_offered_with_the_exact_command_and_rc_file() {
        let mut j = missing(InstallState::Idle, false);
        j.rc_hint = Some(String::from("~/.bashrc"));
        let offer = unavailable_message(&j);
        assert!(offer.contains("Install it? [y/N]"), "asks first: {offer}");
        assert!(
            offer.contains(crate::zoxide::install_script(false)),
            "names the exact command y runs: {offer}"
        );
        assert!(
            offer.contains("~/.bashrc") && offer.contains("croft zoxide (managed)"),
            "says which rc file gets the hook and how the block is marked: {offer}"
        );
        j.rc_hint = None;
        assert!(
            unavailable_message(&j).contains("No shell rc file is changed"),
            "an unsupported shell's rc is left alone, and the offer says so"
        );
        j.termux = true;
        assert!(unavailable_message(&j).contains("y runs: pkg install -y zoxide"));
    }

    /// #926: an installed zoxide with no hook is offered the hook only over
    /// an empty list with nothing typed, naming the rc and the block.
    #[test]
    fn an_unhooked_zoxide_is_offered_the_hook_over_an_empty_list() {
        let mut j = ZoxideJump::new();
        j.set_results(Some(Vec::new()));
        j.hook_offer = Some(String::from("~/.zshrc"));
        assert!(j.hook_offer_shown() && j.asks_yes_no());
        let offer = hook_offer_message("~/.zshrc");
        assert!(
            offer.contains("~/.zshrc") && offer.contains("[y/N]"),
            "{offer}"
        );
        assert!(offer.contains("croft zoxide (managed)"), "{offer}");
        j.push_char('p');
        assert!(!j.asks_yes_no(), "a typed query is a query");
        j.query.clear();
        j.set_results(Some(paths(&["/a"])));
        assert!(!j.hook_offer_shown(), "a list that has entries is shown");
    }

    #[test]
    fn a_failed_install_shows_its_reason_and_offers_a_retry() {
        let mut j = missing(InstallState::Failed, false);
        j.install_note = Some(String::from(
            "Error: zoxide has not yet been packaged for your architecture (x86_64-unknown-linux-musl)",
        ));
        let msg = unavailable_message(&j);
        assert!(
            msg.contains("zoxide install failed: Error: zoxide has not yet been packaged"),
            "the installer's own reason is shown, not just a log line: {msg}"
        );
        assert!(msg.contains("press y to try again"), "{msg}");
    }

    #[test]
    fn unavailable_message_tail_stays_visible_in_a_narrow_popup() {
        let mut j = missing(InstallState::Failed, true);
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
