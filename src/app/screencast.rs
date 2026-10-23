//! Developer: Toggle Screencast Mode, VS Code's keystroke overlay. While it
//! is on, the keys pressed show in a box at the bottom of the screen, the
//! command a key ran is named beside it, and a click lights the cell it
//! landed on. A recording of the terminal (the GIF a pull request carries, a
//! talk, a screen share) then shows what was pressed and where, which the
//! screen alone never does: the palette closes before anyone reads it, and a
//! terminal recording has no mouse pointer.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// How long the keys stay after the last one. VS Code's
/// `screencastMode.keyboardOverlayTimeout` is 800 ms, which a GIF at ten
/// frames a second holds for eight frames: too few to read a chord by.
pub(super) const LINGER: Duration = Duration::from_millis(1500);

/// How long a click's cell stays lit.
pub(super) const CLICK_LIT_FOR: Duration = Duration::from_millis(500);

/// Chips kept; older ones scroll off the left as new ones come.
const MAX_CHIPS: usize = 12;

/// The characters a typed run keeps, its newest.
const MAX_TYPED: usize = 40;

/// One box on the overlay's row: a chord, or a run of typed text.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Chip {
    /// The chord as the palette spells it (`Ctrl+Shift+P`), a `Cmd+K`
    /// chord's steps joined by a space (`Ctrl+K Z`), or the typed text.
    text: String,
    typed: bool,
    /// The same chord pressed this many times in a row: a held arrow reads
    /// `↓ ×12`, not twelve boxes.
    count: u32,
    /// The palette title of the command the key ran.
    command: Option<&'static str>,
}

impl Chip {
    fn keys(text: String) -> Self {
        Self {
            text,
            typed: false,
            count: 1,
            command: None,
        }
    }
}

#[derive(Default)]
pub(super) struct Screencast {
    on: bool,
    chips: Vec<Chip>,
    last_input: Option<Instant>,
    /// The cell a click landed on, and when.
    click: Option<(u16, u16, Instant)>,
    /// The chip the key being dispatched made, so the command it runs is
    /// named on that chip and no other.
    current: Option<usize>,
    /// The key being dispatched completed a `Cmd+K` chord: croft took it
    /// as a command, so it reached no prompt or shell.
    chord_step: bool,
}

impl Screencast {
    pub(super) fn is_on(&self) -> bool {
        self.on
    }

    /// Turn the overlay on or off, starting empty either way; true when it
    /// is now on.
    pub(super) fn toggle(&mut self) -> bool {
        *self = Self {
            on: !self.on,
            ..Self::default()
        };
        self.on
    }

    /// Show a key press. `second_step`: the key completed a `Cmd+K` chord,
    /// so it joins the leader's chip. `hide_typing`: what is typed here may
    /// be secret (a password field, the shell, which may be reading one with
    /// echo off), so a key that would type a character there is left out,
    /// `Alt` held or not, as a masked field types it either way; chords and
    /// named keys still show.
    pub(super) fn press(
        &mut self,
        key: KeyEvent,
        now: Instant,
        second_step: bool,
        hide_typing: bool,
    ) {
        self.current = None;
        if !self.on || (!second_step && hide_typing && types_a_character(key)) {
            return;
        }
        if !second_step && let Some(c) = typed_char(key) {
            match self.chips.last_mut() {
                Some(run) if run.typed => {
                    run.text.push(c);
                    let excess = run.text.chars().count().saturating_sub(MAX_TYPED);
                    if let Some((at, _)) = run.text.char_indices().nth(excess) {
                        run.text.drain(..at);
                    }
                }
                _ => self.chips.push(Chip {
                    typed: true,
                    ..Chip::keys(c.to_string())
                }),
            }
        } else {
            let Some(label) = key_label(key, cfg!(target_os = "macos")) else {
                return;
            };
            match self.chips.last_mut() {
                // A counted leader (`Ctrl+K ×2`, the first one's step left
                // out) gives up only its last press to the chord.
                Some(leader) if second_step && !leader.typed => {
                    let chord = format!("{} {label}", leader.text);
                    if leader.count > 1 {
                        leader.count -= 1;
                        self.chips.push(Chip::keys(chord));
                    } else {
                        leader.text = chord;
                        leader.command = None;
                    }
                    self.merge_repeated_chord();
                }
                Some(last) if !second_step && !last.typed && last.text == label => {
                    last.count += 1;
                    last.command = None;
                }
                _ => self.chips.push(Chip::keys(label)),
            }
            self.current = Some(self.chips.len() - 1);
        }
        self.last_input = Some(now);
        self.trim();
    }

    /// A finished two-step chord the same as the one before it counts as a
    /// repeat of it: `Ctrl+K Z` twice reads `Ctrl+K Z ×2`.
    fn merge_repeated_chord(&mut self) {
        let n = self.chips.len();
        if n >= 2 && !self.chips[n - 2].typed && self.chips[n - 2].text == self.chips[n - 1].text {
            self.chips.pop();
            self.chips[n - 2].count += 1;
        }
    }

    fn trim(&mut self) {
        let excess = self.chips.len().saturating_sub(MAX_CHIPS);
        if excess > 0 {
            self.chips.drain(..excess);
            self.current = self.current.and_then(|i| i.checked_sub(excess));
        }
    }

    /// The key being dispatched completed a `Cmd+K` chord.
    pub(super) fn note_chord_step(&mut self) {
        self.chord_step = self.on;
    }

    /// Whether the key just dispatched completed a `Cmd+K` chord.
    pub(super) fn take_chord_step(&mut self) -> bool {
        std::mem::take(&mut self.chord_step)
    }

    /// A paste from the host terminal (its Cmd+V, or Ctrl+Shift+V), which
    /// reaches croft as text and never as a key. It shows as `Paste`, never
    /// the text: what is pasted is as likely as anything to be a secret.
    pub(super) fn pasted(&mut self, now: Instant) {
        self.current = None;
        if !self.on {
            return;
        }
        match self.chips.last_mut() {
            Some(last) if !last.typed && last.text == "Paste" => last.count += 1,
            _ => self.chips.push(Chip::keys(String::from("Paste"))),
        }
        self.last_input = Some(now);
        self.trim();
    }

    /// Name the command the key being dispatched ran on its chip. The
    /// first named wins: a command that runs others is what the key did.
    pub(super) fn name_command(&mut self, title: &'static str) {
        if let Some(chip) = self.current.and_then(|i| self.chips.get_mut(i)) {
            chip.command.get_or_insert(title);
        }
    }

    /// The key has been dispatched: a command run after this is not its.
    pub(super) fn end_key(&mut self) {
        self.current = None;
    }

    /// Light the cell a press landed on. A press with a modifier held
    /// means something a plain one doesn't (`Cmd+Click` goes to a
    /// definition), so it shows as a chip too.
    pub(super) fn click(
        &mut self,
        col: u16,
        row: u16,
        button: MouseButton,
        modifiers: KeyModifiers,
        now: Instant,
    ) {
        if !self.on {
            return;
        }
        self.click = Some((col, row, now));
        let mods = modifier_names(modifiers, false, cfg!(target_os = "macos"));
        if mods.is_empty() {
            return;
        }
        let what = match button {
            MouseButton::Left => "Click",
            MouseButton::Right => "Right Click",
            MouseButton::Middle => "Middle Click",
        };
        let label = format!("{}+{what}", mods.join("+"));
        match self.chips.last_mut() {
            Some(last) if !last.typed && last.text == label => last.count += 1,
            _ => self.chips.push(Chip::keys(label)),
        }
        self.last_input = Some(now);
        self.trim();
    }

    /// Drop the keys once they have lingered and the click light once it
    /// has shown; true when either went, so the frame without it is drawn.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self
            .last_input
            .is_some_and(|at| now.saturating_duration_since(at) >= LINGER)
        {
            self.chips.clear();
            self.current = None;
            self.last_input = None;
            changed = true;
        }
        if self
            .click
            .is_some_and(|(_, _, at)| now.saturating_duration_since(at) >= CLICK_LIT_FOR)
        {
            self.click = None;
            changed = true;
        }
        changed
    }

    /// The overlay's row as text, for tests: the chips that fit `width`.
    #[cfg(test)]
    pub(super) fn row_text(&self, width: u16) -> String {
        self.row(width, Theme::default())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// The chips that fit `width`, newest kept: the oldest scroll off the
    /// left, and a typed run too long for the row on its own keeps its end.
    fn row(&self, width: u16, theme: Theme) -> Line<'static> {
        let width = width as usize;
        let mut kept: Vec<Vec<Span<'static>>> = Vec::new();
        let mut used = 0;
        for (i, chip) in self.chips.iter().enumerate().rev() {
            let gap = usize::from(!kept.is_empty());
            let spans = chip_spans(chip, chip.command, theme);
            let w = spans_width(&spans);
            if used + gap + w <= width {
                used += gap + w;
                kept.push(spans);
                continue;
            }
            // Only the newest chip is cut to fit; an older one that doesn't
            // fit ends the row.
            if i + 1 == self.chips.len() {
                let bare = chip_spans(chip, None, theme);
                if spans_width(&bare) <= width {
                    kept.push(bare);
                } else if chip.typed {
                    kept.push(chip_spans(&tail_to_fit(chip, width), None, theme));
                }
            }
            break;
        }
        kept.reverse();
        let mut spans = Vec::new();
        for (i, chip) in kept.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            spans.extend(chip);
        }
        Line::from(spans)
    }

    /// Paint the overlay: the click's cell lit, and the keys in a box
    /// centred at the bottom of `area`, or at its top while the bottom box
    /// would cover `caret`, the cell being typed at.
    pub(super) fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        theme: Theme,
        caret: Option<(u16, u16)>,
    ) {
        if !self.on {
            return;
        }
        if let Some((x, y, _)) = self.click
            && let Some(cell) = buf.cell_mut((x, y))
        {
            cell.set_style(
                Style::default()
                    .bg(theme.accent())
                    .fg(theme.accent_contrast_fg()),
            );
        }
        const HEIGHT: u16 = 3;
        if self.chips.is_empty() || area.width < 8 || area.height < HEIGHT {
            return;
        }
        let row = self.row(area.width - 4, theme);
        let width = row.width() as u16 + 4;
        if width <= 4 {
            return;
        }
        let x = area.x + (area.width - width) / 2;
        let bottom = Rect::new(x, area.y + area.height - HEIGHT, width, HEIGHT);
        let covers_caret = caret.is_some_and(|(cx, cy)| {
            (bottom.x..bottom.x + bottom.width).contains(&cx)
                && (bottom.y..bottom.y + bottom.height).contains(&cy)
        });
        let rect = if covers_caret && area.height > 2 * HEIGHT {
            Rect {
                y: area.y + 1,
                ..bottom
            }
        } else {
            bottom
        };
        let accent = theme.accent();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(accent))
            .style(Style::default().bg(theme.editor_bg()));
        Clear.render(rect, buf);
        Paragraph::new(row)
            .alignment(Alignment::Center)
            .block(block)
            .render(rect, buf);
    }
}

/// The character a key types, when it types one: no modifier but Shift,
/// which is the character's own case.
fn typed_char(key: KeyEvent) -> Option<char> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if key.modifiers.intersects(
        KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER
            | KeyModifiers::META,
    ) {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        return c.to_uppercase().next();
    }
    Some(c)
}

/// Whether a key types a character into a text field or the shell: a
/// character with neither `Ctrl` nor `Cmd` held, the test a masked input
/// prompt applies before it takes one, `Alt` included.
fn types_a_character(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_))
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
}

/// The modifiers held, in the order croft's shortcut hints name them and
/// with their names there: `Cmd` and `Opt` on a Mac, `Super` and `Alt`
/// elsewhere, where `Ctrl` is the command key.
fn modifier_names(mods: KeyModifiers, shift: bool, macos: bool) -> Vec<&'static str> {
    let mut names = Vec::new();
    if mods.contains(KeyModifiers::CONTROL) {
        names.push("Ctrl");
    }
    if mods.contains(KeyModifiers::SUPER) {
        names.push(if macos { "Cmd" } else { "Super" });
    }
    if mods.contains(KeyModifiers::ALT) {
        names.push(if macos { "Opt" } else { "Alt" });
    }
    if shift || mods.contains(KeyModifiers::SHIFT) {
        names.push("Shift");
    }
    names
}

/// A key as the palette spells shortcuts (`Ctrl+Shift+P`, `Cmd+↓`, `F5`),
/// or `None` for one that is only a modifier going down (the kitty
/// protocol reports those) or has no name worth a chip.
fn key_label(key: KeyEvent, macos: bool) -> Option<String> {
    let name = match key.code {
        KeyCode::Char(' ') => String::from("Space"),
        KeyCode::Char(c) => c.to_uppercase().collect(),
        KeyCode::Enter => String::from("Enter"),
        KeyCode::Tab | KeyCode::BackTab => String::from("Tab"),
        KeyCode::Backspace => String::from("Backspace"),
        KeyCode::Delete => String::from("Delete"),
        KeyCode::Esc => String::from("Esc"),
        KeyCode::Up => String::from("\u{2191}"),
        KeyCode::Down => String::from("\u{2193}"),
        KeyCode::Left => String::from("\u{2190}"),
        KeyCode::Right => String::from("\u{2192}"),
        KeyCode::Home => String::from("Home"),
        KeyCode::End => String::from("End"),
        KeyCode::PageUp => String::from("PageUp"),
        KeyCode::PageDown => String::from("PageDown"),
        KeyCode::Insert => String::from("Insert"),
        KeyCode::F(n) => format!("F{n}"),
        _ => return None,
    };
    // An upper-case letter is Shift held, whether or not the terminal said
    // so; BackTab is Shift+Tab.
    let shift =
        matches!(key.code, KeyCode::Char(c) if c.is_uppercase()) || key.code == KeyCode::BackTab;
    let mut parts = modifier_names(key.modifiers, shift, macos);
    parts.push(&name);
    Some(parts.join("+"))
}

/// A chip's spans: the key cap, then the command it ran in the accent.
fn chip_spans(chip: &Chip, command: Option<&'static str>, theme: Theme) -> Vec<Span<'static>> {
    let fg = theme.ui(Color::Rgb(0xd4, 0xd8, 0xe0));
    let cap = if chip.typed {
        Style::default().fg(fg).bg(theme.osk_key_bg())
    } else {
        Style::default()
            .fg(fg)
            .bg(theme.osk_special_bg())
            .add_modifier(Modifier::BOLD)
    };
    let text = if chip.count > 1 {
        format!(" {} \u{d7}{} ", chip.text, chip.count)
    } else {
        format!(" {} ", chip.text)
    };
    let mut spans = vec![Span::styled(text, cap)];
    if let Some(title) = command {
        spans.push(Span::styled(
            format!(" {title}"),
            Style::default().fg(theme.accent()),
        ));
    }
    spans
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// A typed run cut to its end so its cap fits `width`, an ellipsis where
/// the rest was.
fn tail_to_fit(chip: &Chip, width: usize) -> Chip {
    let room = width.saturating_sub(3);
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for c in chip.text.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        tail.push(c);
    }
    tail.push('\u{2026}');
    tail.reverse();
    Chip {
        text: tail.into_iter().collect(),
        ..chip.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn on() -> Screencast {
        let mut s = Screencast::default();
        assert!(s.toggle());
        s
    }

    fn type_str(s: &mut Screencast, text: &str, now: Instant) {
        for c in text.chars() {
            s.press(k(KeyCode::Char(c), KeyModifiers::NONE), now, false, false);
        }
    }

    #[test]
    fn a_chord_reads_as_the_palette_spells_it_on_each_platform() {
        let p = k(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(key_label(p, false).as_deref(), Some("Ctrl+Shift+P"));
        let opt = k(KeyCode::Down, KeyModifiers::SUPER | KeyModifiers::ALT);
        assert_eq!(key_label(opt, true).as_deref(), Some("Cmd+Opt+\u{2193}"));
        assert_eq!(key_label(opt, false).as_deref(), Some("Super+Alt+\u{2193}"));
        // A terminal that reports Shift+P as a capital without the flag.
        let capital = k(KeyCode::Char('P'), KeyModifiers::CONTROL);
        assert_eq!(key_label(capital, false).as_deref(), Some("Ctrl+Shift+P"));
        let back_tab = k(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(key_label(back_tab, false).as_deref(), Some("Shift+Tab"));
        let space = k(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert_eq!(key_label(space, false).as_deref(), Some("Ctrl+Space"));
        assert_eq!(
            key_label(k(KeyCode::F(5), KeyModifiers::NONE), false).as_deref(),
            Some("F5")
        );
        let shift_only = k(
            KeyCode::Modifier(crossterm::event::ModifierKeyCode::LeftShift),
            KeyModifiers::SHIFT,
        );
        assert_eq!(
            key_label(shift_only, false),
            None,
            "a lone modifier is no chip"
        );
    }

    #[test]
    fn typing_gathers_into_one_run_and_a_chord_starts_the_next_chip() {
        let now = Instant::now();
        let mut s = on();
        type_str(&mut s, "fn ", now);
        s.press(
            k(KeyCode::Char('M'), KeyModifiers::SHIFT),
            now,
            false,
            false,
        );
        s.press(k(KeyCode::Enter, KeyModifiers::NONE), now, false, false);
        type_str(&mut s, "x", now);
        assert_eq!(s.row_text(80), " fn M   Enter   x ");
    }

    #[test]
    fn a_held_key_is_counted_not_repeated() {
        let now = Instant::now();
        let mut s = on();
        for _ in 0..12 {
            s.press(k(KeyCode::Down, KeyModifiers::NONE), now, false, false);
        }
        s.press(k(KeyCode::Up, KeyModifiers::NONE), now, false, false);
        assert_eq!(s.row_text(80), " \u{2193} \u{d7}12   \u{2191} ");
    }

    #[test]
    fn a_cmd_k_chord_is_one_chip_and_a_repeat_of_it_is_counted() {
        let now = Instant::now();
        let mut s = on();
        let leader = k(KeyCode::Char('k'), KeyModifiers::CONTROL);
        let z = k(KeyCode::Char('z'), KeyModifiers::NONE);
        s.press(leader, now, false, false);
        s.press(z, now, true, false);
        assert_eq!(s.row_text(80), " Ctrl+K Z ");
        s.press(leader, now, false, false);
        s.press(z, now, true, false);
        assert_eq!(s.row_text(80), " Ctrl+K Z \u{d7}2 ");
        // A chord step croft took as a command is never typing, even in
        // the shell.
        s.press(leader, now, false, true);
        s.press(k(KeyCode::Char('k'), KeyModifiers::NONE), now, true, true);
        assert_eq!(s.row_text(80), " Ctrl+K Z \u{d7}2   Ctrl+K K ");
        // A leader whose step went to the shell, then a whole chord: the
        // first press keeps its chip.
        let mut s = on();
        s.press(leader, now, false, true);
        s.press(k(KeyCode::Char('?'), KeyModifiers::NONE), now, false, true);
        s.press(leader, now, false, true);
        s.press(k(KeyCode::Char('k'), KeyModifiers::NONE), now, true, true);
        assert_eq!(s.row_text(80), " Ctrl+K   Ctrl+K K ");
    }

    #[test]
    fn secret_typing_is_left_out_but_its_enter_shows() {
        let now = Instant::now();
        let mut s = on();
        for c in "hunter2".chars() {
            s.press(k(KeyCode::Char(c), KeyModifiers::NONE), now, false, true);
        }
        // A masked field types an Alt-held character too.
        s.press(k(KeyCode::Char('q'), KeyModifiers::ALT), now, false, true);
        s.press(k(KeyCode::Enter, KeyModifiers::NONE), now, false, true);
        let row = s.row_text(80);
        assert_eq!(row, " Enter ");
        assert!(!row.contains("hunter"));
        // Where nothing is secret, Alt+Q is a chord like any other.
        s.press(k(KeyCode::Char('q'), KeyModifiers::ALT), now, false, false);
        assert_eq!(s.row_text(80), " Enter   Alt+Q ");
    }

    #[test]
    fn off_it_records_nothing() {
        let now = Instant::now();
        let mut s = Screencast::default();
        s.press(k(KeyCode::Tab, KeyModifiers::NONE), now, false, false);
        s.pasted(now);
        s.click(1, 1, MouseButton::Left, KeyModifiers::CONTROL, now);
        s.note_chord_step();
        assert!(!s.take_chord_step());
        assert!(s.toggle());
        assert_eq!(s.row_text(80), "");
        assert!(!s.expire(now + LINGER), "nothing was waiting to go");
    }

    #[test]
    fn the_command_a_key_ran_is_named_on_its_chip_only() {
        let now = Instant::now();
        let mut s = on();
        s.press(k(KeyCode::Enter, KeyModifiers::NONE), now, false, false);
        s.name_command("View: Toggle Zen Mode");
        s.name_command("Run by Toggle Zen Mode");
        s.end_key();
        s.name_command("Not this one");
        assert_eq!(s.row_text(80), " Enter  View: Toggle Zen Mode");
        // Typing names nothing, so a command run while it is dispatched
        // lands on no chip.
        type_str(&mut s, "a", now);
        s.name_command("Nor this");
        assert!(!s.row_text(80).contains("Nor this"));
    }

    #[test]
    fn the_oldest_chips_scroll_off_and_a_long_run_keeps_its_end() {
        let now = Instant::now();
        let mut s = on();
        for n in 1..=9 {
            s.press(k(KeyCode::F(n), KeyModifiers::NONE), now, false, false);
        }
        assert_eq!(s.row_text(15), " F7   F8   F9 ");
        type_str(&mut s, "the quick brown fox", now);
        assert_eq!(s.row_text(12), " \u{2026}brown fox ");
        let mut many = on();
        for _ in 0..(MAX_CHIPS + 5) {
            many.press(k(KeyCode::Up, KeyModifiers::NONE), now, false, false);
            many.press(k(KeyCode::Down, KeyModifiers::NONE), now, false, false);
        }
        assert_eq!(many.chips.len(), MAX_CHIPS);
        type_str(&mut many, &"x".repeat(MAX_TYPED + 9), now);
        assert_eq!(many.chips.last().unwrap().text.chars().count(), MAX_TYPED);
    }

    #[test]
    fn the_keys_linger_then_go_and_toggling_starts_empty() {
        let now = Instant::now();
        let mut s = on();
        s.press(k(KeyCode::Esc, KeyModifiers::NONE), now, false, false);
        s.click(3, 4, MouseButton::Left, KeyModifiers::NONE, now);
        assert!(!s.expire(now + CLICK_LIT_FOR / 2));
        assert!(s.expire(now + CLICK_LIT_FOR), "the click light goes first");
        assert_eq!(s.row_text(80), " Esc ");
        assert!(s.expire(now + LINGER));
        assert_eq!(s.row_text(80), "");
        assert!(!s.expire(now + LINGER * 2), "nothing left to drop");
        s.press(k(KeyCode::Esc, KeyModifiers::NONE), now, false, false);
        assert!(!s.toggle());
        assert!(s.toggle());
        assert_eq!(s.row_text(80), "", "a new session starts empty");
    }

    #[test]
    fn a_modified_click_is_a_chip_and_a_plain_one_only_lights_its_cell() {
        let now = Instant::now();
        let mut s = on();
        s.click(1, 1, MouseButton::Left, KeyModifiers::NONE, now);
        assert_eq!(s.row_text(80), "");
        s.click(1, 1, MouseButton::Left, KeyModifiers::CONTROL, now);
        s.click(1, 1, MouseButton::Left, KeyModifiers::CONTROL, now);
        assert_eq!(s.row_text(80), " Ctrl+Click \u{d7}2 ");
    }

    #[test]
    fn a_paste_shows_as_a_paste_and_never_its_text() {
        let now = Instant::now();
        let mut s = on();
        s.pasted(now);
        s.pasted(now);
        assert_eq!(s.row_text(80), " Paste \u{d7}2 ");
        assert!(s.expire(now + LINGER), "it lingers like a key");
    }

    #[test]
    fn the_box_moves_to_the_top_rather_than_cover_the_caret() {
        let now = Instant::now();
        let mut s = on();
        s.press(k(KeyCode::Tab, KeyModifiers::NONE), now, false, false);
        let area = Rect::new(0, 0, 40, 12);
        let row_of_tab = |buf: &Buffer| {
            (0..area.height).find(|&y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Tab")
            })
        };
        let mut buf = Buffer::empty(area);
        s.render(&mut buf, area, Theme::default(), Some((0, 10)));
        assert_eq!(row_of_tab(&buf), Some(10), "bottom, clear of the caret");
        let mut buf = Buffer::empty(area);
        s.render(&mut buf, area, Theme::default(), Some((20, 10)));
        assert_eq!(
            row_of_tab(&buf),
            Some(2),
            "top, the caret under the bottom box"
        );
    }

    #[test]
    fn off_it_paints_nothing() {
        let now = Instant::now();
        let mut s = on();
        s.press(k(KeyCode::Tab, KeyModifiers::NONE), now, false, false);
        s.click(0, 0, MouseButton::Left, KeyModifiers::NONE, now);
        s.toggle();
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        s.render(&mut buf, area, Theme::default(), None);
        assert_eq!(buf, Buffer::empty(area));
    }
}
