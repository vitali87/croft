//! On-screen keyboard for touch-only environments (Termux/Android).
//!
//! Termux's `TerminalView` only raises the Android soft keyboard from the
//! single-tap path, and that path is skipped entirely while the foreground
//! app has mouse tracking active (`onUp()` converts the tap into terminal
//! mouse press/release events and returns early). Croft needs mouse tracking
//! for all of its click routing, so on Termux a tap can never summon the
//! native keyboard - and Termux ships no escape sequence to request it
//! (termux-app#3733 is still open). The fix is to render our own keyboard:
//! a bottom-docked band of tappable keys whose taps synthesize ordinary
//! `crossterm` `KeyEvent`s and feed them through `App::handle_key`, so they
//! reach the editor buffer, the terminal PTY, and every modal exactly like
//! hardware keystrokes.
//!
//! Layers mirror a phone keyboard: lowercase, Shift (one-shot uppercase,
//! like a phone's shift), and a symbols page. Ctrl and Alt are one-shot
//! latches that apply to the next key tap, so Ctrl+C / Ctrl+P chords work
//! with two taps.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Rows the keyboard band occupies (one terminal row per key row).
pub const OSK_HEIGHT: u16 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OskLayer {
    Lower,
    Upper,
    Symbols,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OskKey {
    /// A printable character on the current layer.
    Char(char),
    /// A named key (Esc, Tab, Enter, Backspace, arrows, ...).
    Code(KeyCode),
    /// One-shot uppercase / layer flip back from Symbols.
    Shift,
    /// One-shot Ctrl latch for the next key.
    Ctrl,
    /// One-shot Alt latch for the next key.
    Alt,
    /// Toggle the Symbols layer.
    Symbols,
    /// Dismiss the keyboard (handled by the app, never synthesized).
    Hide,
}

pub struct Osk {
    pub layer: OskLayer,
    /// One-shot Ctrl latch: armed by tapping `ctrl`, consumed by the next
    /// character/named key.
    pub ctrl: bool,
    /// One-shot Alt latch, same lifecycle as `ctrl`.
    pub alt: bool,
    /// Band rect from the last layout; the mouse handler hit-tests this.
    pub last_area: Rect,
    /// Per-key hit rects from the last layout, in render order.
    keys: Vec<(Rect, OskKey)>,
}

/// One key slot in a row: display label, the key it fires, and its relative
/// width. Every layer keeps identical key counts per row so the geometry is
/// stable across layer flips that happen between two frames.
type KeySlot = (String, OskKey, u16);

fn slot(label: &str, key: OskKey, weight: u16) -> KeySlot {
    (String::from(label), key, weight)
}

fn char_slots(chars: &str) -> Vec<KeySlot> {
    chars
        .chars()
        .map(|c| (c.to_string(), OskKey::Char(c), 1))
        .collect()
}

fn rows_for(layer: OskLayer) -> Vec<Vec<KeySlot>> {
    let esc = || slot("esc", OskKey::Code(KeyCode::Esc), 2);
    let bksp = || slot("⌫", OskKey::Code(KeyCode::Backspace), 2);
    let tab = || slot("⇥", OskKey::Code(KeyCode::Tab), 2);
    let ctrl = || slot("ctrl", OskKey::Ctrl, 2);
    let enter = || slot("⏎", OskKey::Code(KeyCode::Enter), 2);
    let shift = || slot("⇧", OskKey::Shift, 2);
    let row = |left: KeySlot, mid: &str, right: Option<KeySlot>| {
        let mut r = vec![left];
        r.extend(char_slots(mid));
        r.extend(right);
        r
    };
    // Bottom row is shared by every layer; the layer key relabels itself.
    let bottom = |symbols_label: &str| {
        vec![
            slot(symbols_label, OskKey::Symbols, 3),
            slot("alt", OskKey::Alt, 2),
            slot(" ", OskKey::Char(' '), 8),
            slot("←", OskKey::Code(KeyCode::Left), 1),
            slot("↓", OskKey::Code(KeyCode::Down), 1),
            slot("↑", OskKey::Code(KeyCode::Up), 1),
            slot("→", OskKey::Code(KeyCode::Right), 1),
            slot("⌄", OskKey::Hide, 2),
        ]
    };
    match layer {
        OskLayer::Lower => vec![
            row(esc(), "1234567890", Some(bksp())),
            row(tab(), "qwertyuiop", None),
            row(ctrl(), "asdfghjkl", Some(enter())),
            row(shift(), "zxcvbnm,./", None),
            bottom("&123"),
        ],
        OskLayer::Upper => vec![
            row(esc(), "!@#$%^&*()", Some(bksp())),
            row(tab(), "QWERTYUIOP", None),
            row(ctrl(), "ASDFGHJKL", Some(enter())),
            row(shift(), "ZXCVBNM<>?", None),
            bottom("&123"),
        ],
        OskLayer::Symbols => vec![
            row(esc(), "1234567890", Some(bksp())),
            row(tab(), "`~[]{}()<>", None),
            row(ctrl(), "-_=+;:'\"", Some(enter())),
            row(shift(), "!@#$%\\|,.?", None),
            bottom("abc"),
        ],
    }
}

impl Osk {
    pub fn new() -> Self {
        Self {
            layer: OskLayer::Lower,
            ctrl: false,
            alt: false,
            last_area: Rect::default(),
            keys: Vec::new(),
        }
    }

    /// Recompute per-key hit rects for the current layer inside `area`.
    pub fn layout(&mut self, area: Rect) {
        self.last_area = area;
        self.keys.clear();
        for (i, row) in rows_for(self.layer).iter().enumerate() {
            let y = area.y + i as u16;
            if y >= area.y.saturating_add(area.height) {
                break;
            }
            let gaps = row.len().saturating_sub(1) as u16;
            let avail = u32::from(area.width.saturating_sub(gaps));
            let total: u32 = row.iter().map(|(_, _, w)| u32::from(*w)).sum();
            if avail == 0 || total == 0 {
                continue;
            }
            // Proportional rounding over cumulative weights keeps the row
            // pinned to the band's full width with no drift.
            let (mut cum, mut consumed, mut x) = (0u32, 0u32, area.x);
            for (_, key, w) in row {
                cum += u32::from(*w);
                let end = avail * cum / total;
                let kw = ((end - consumed) as u16).max(1);
                self.keys.push((
                    Rect {
                        x,
                        y,
                        width: kw,
                        height: 1,
                    },
                    *key,
                ));
                x = x.saturating_add(kw + 1);
                consumed = end;
            }
        }
    }

    /// The key under a screen cell, if any.
    pub fn key_at(&self, col: u16, row: u16) -> Option<OskKey> {
        self.keys
            .iter()
            .find(|(r, _)| row == r.y && col >= r.x && col < r.x.saturating_add(r.width))
            .map(|(_, k)| *k)
    }

    /// The hit rect of `key` on the current layer. Test-only: production
    /// code goes the other way (cell -> key) through `key_at`.
    #[cfg(test)]
    pub fn rect_for(&self, key: OskKey) -> Option<Rect> {
        self.keys.iter().find(|(_, k)| *k == key).map(|(r, _)| *r)
    }

    /// Apply a tapped key. Layer/modifier keys mutate state and return
    /// `None`; character/named keys return the synthesized `KeyEvent` with
    /// any one-shot latches consumed. `Hide` always returns `None` - the
    /// app dismisses the keyboard before calling this.
    pub fn tap(&mut self, key: OskKey) -> Option<KeyEvent> {
        match key {
            OskKey::Shift => {
                self.layer = match self.layer {
                    OskLayer::Upper => OskLayer::Lower,
                    _ => OskLayer::Upper,
                };
                None
            }
            OskKey::Symbols => {
                self.layer = match self.layer {
                    OskLayer::Symbols => OskLayer::Lower,
                    _ => OskLayer::Symbols,
                };
                None
            }
            OskKey::Ctrl => {
                self.ctrl = !self.ctrl;
                None
            }
            OskKey::Alt => {
                self.alt = !self.alt;
                None
            }
            OskKey::Hide => None,
            OskKey::Char(c) => {
                let ev = KeyEvent::new(KeyCode::Char(c), self.take_latches());
                // One-shot shift, phone-style: typing a character drops the
                // Upper layer back to Lower. Symbols stays until toggled.
                if self.layer == OskLayer::Upper {
                    self.layer = OskLayer::Lower;
                }
                Some(ev)
            }
            OskKey::Code(code) => Some(KeyEvent::new(code, self.take_latches())),
        }
    }

    fn take_latches(&mut self) -> KeyModifiers {
        let mut mods = KeyModifiers::NONE;
        if std::mem::take(&mut self.ctrl) {
            mods |= KeyModifiers::CONTROL;
        }
        if std::mem::take(&mut self.alt) {
            mods |= KeyModifiers::ALT;
        }
        mods
    }

    /// True when `key` should render highlighted: an armed one-shot latch
    /// or the key that owns the active layer.
    fn is_armed(&self, key: OskKey) -> bool {
        match key {
            OskKey::Ctrl => self.ctrl,
            OskKey::Alt => self.alt,
            OskKey::Shift => self.layer == OskLayer::Upper,
            OskKey::Symbols => self.layer == OskLayer::Symbols,
            _ => false,
        }
    }
}

impl Default for Osk {
    fn default() -> Self {
        Self::new()
    }
}

/// VS Code dark-keyboard palette: char keys on a raised grey, structural
/// keys a step darker, armed modifiers on the focus-accent blue.
const KEY_FG: Color = Color::Rgb(0xd4, 0xd8, 0xe0);
const KEY_BG: Color = Color::Rgb(0x3a, 0x40, 0x52);
const KEY_SPECIAL_BG: Color = Color::Rgb(0x2c, 0x31, 0x40);
const KEY_ARMED_BG: Color = Color::Rgb(0x00, 0x7a, 0xcc);

/// Paint the keyboard band and refresh its hit rects. `panel_bg` is the
/// theme's editor background so the band reads as part of the chrome.
pub fn render_osk(osk: &mut Osk, area: Rect, buf: &mut Buffer, panel_bg: Color) {
    osk.layout(area);
    let gap = Style::default().bg(panel_bg);
    let blank = " ".repeat(area.width as usize);
    for y in area.y..area.y.saturating_add(area.height) {
        buf.set_stringn(area.x, y, &blank, area.width as usize, gap);
    }
    let rows = rows_for(osk.layer);
    let labels: Vec<&KeySlot> = rows.iter().flatten().collect();
    for ((rect, key), (label, _, _)) in osk.keys.iter().zip(labels) {
        let armed = osk.is_armed(*key);
        let special = !matches!(key, OskKey::Char(_));
        let style = if armed {
            Style::default()
                .fg(Color::White)
                .bg(KEY_ARMED_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(KEY_FG)
                .bg(if special { KEY_SPECIAL_BG } else { KEY_BG })
        };
        let w = rect.width as usize;
        let chars = label.chars().count();
        let pad = w.saturating_sub(chars) / 2;
        let mut cell = " ".repeat(pad);
        cell.push_str(label);
        cell.push_str(&" ".repeat(w.saturating_sub(cell.chars().count())));
        buf.set_stringn(rect.x, rect.y, &cell, w, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band() -> Rect {
        Rect {
            x: 0,
            y: 34,
            width: 80,
            height: OSK_HEIGHT,
        }
    }

    #[test]
    fn layout_fills_band_and_every_row_hit_tests() {
        let mut osk = Osk::new();
        osk.layout(band());
        assert_eq!(osk.last_area, band());
        // Every row of the band carries at least one tappable key.
        for row in band().y..band().y + OSK_HEIGHT {
            assert!(
                (0..80).any(|col| osk.key_at(col, row).is_some()),
                "row {row} has no keys"
            );
        }
        // The lower layer exposes the letters and the digits.
        for c in ['a', 'q', 'z', '1', '0'] {
            assert!(
                osk.rect_for(OskKey::Char(c)).is_some(),
                "missing key {c:?} on lower layer"
            );
        }
        // Structural keys are always present.
        for k in [
            OskKey::Code(KeyCode::Esc),
            OskKey::Code(KeyCode::Enter),
            OskKey::Code(KeyCode::Backspace),
            OskKey::Code(KeyCode::Tab),
            OskKey::Char(' '),
            OskKey::Shift,
            OskKey::Ctrl,
            OskKey::Alt,
            OskKey::Symbols,
            OskKey::Hide,
            OskKey::Code(KeyCode::Left),
            OskKey::Code(KeyCode::Right),
            OskKey::Code(KeyCode::Up),
            OskKey::Code(KeyCode::Down),
        ] {
            assert!(osk.rect_for(k).is_some(), "missing key {k:?}");
        }
        // rect_for and key_at agree: the center of the `a` key hits `a`.
        let r = osk.rect_for(OskKey::Char('a')).unwrap();
        assert_eq!(osk.key_at(r.x + r.width / 2, r.y), Some(OskKey::Char('a')));
    }

    #[test]
    fn plain_char_tap_synthesizes_unmodified_key_event() {
        let mut osk = Osk::new();
        let ev = osk
            .tap(OskKey::Char('a'))
            .expect("char tap yields an event");
        assert_eq!(ev.code, KeyCode::Char('a'));
        assert_eq!(ev.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn shift_is_one_shot_uppercase() {
        let mut osk = Osk::new();
        assert!(osk.tap(OskKey::Shift).is_none());
        assert_eq!(osk.layer, OskLayer::Upper);
        let ev = osk.tap(OskKey::Char('A')).unwrap();
        assert_eq!(ev.code, KeyCode::Char('A'));
        // One-shot: typing a character drops back to lowercase.
        assert_eq!(osk.layer, OskLayer::Lower);
        // Tapping Shift twice toggles back off without typing.
        osk.tap(OskKey::Shift);
        osk.tap(OskKey::Shift);
        assert_eq!(osk.layer, OskLayer::Lower);
    }

    #[test]
    fn ctrl_latch_applies_to_exactly_one_key() {
        let mut osk = Osk::new();
        assert!(osk.tap(OskKey::Ctrl).is_none());
        assert!(osk.ctrl);
        let ev = osk.tap(OskKey::Char('c')).unwrap();
        assert!(ev.modifiers.contains(KeyModifiers::CONTROL));
        assert!(!osk.ctrl, "ctrl latch must be consumed by the chord");
        let ev = osk.tap(OskKey::Char('c')).unwrap();
        assert_eq!(ev.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn alt_latch_applies_to_named_keys_too() {
        let mut osk = Osk::new();
        osk.tap(OskKey::Alt);
        let ev = osk.tap(OskKey::Code(KeyCode::Right)).unwrap();
        assert_eq!(ev.code, KeyCode::Right);
        assert!(ev.modifiers.contains(KeyModifiers::ALT));
        assert!(!osk.alt);
    }

    #[test]
    fn symbols_layer_toggles_and_carries_punctuation() {
        let mut osk = Osk::new();
        assert!(osk.tap(OskKey::Symbols).is_none());
        assert_eq!(osk.layer, OskLayer::Symbols);
        osk.layout(band());
        for c in ['[', ']', '{', '}', '=', '+', '_', '\'', '"', '|', '\\'] {
            assert!(
                osk.rect_for(OskKey::Char(c)).is_some(),
                "missing key {c:?} on symbols layer"
            );
        }
        // Typing a symbol keeps the layer (unlike one-shot Shift).
        osk.tap(OskKey::Char('['));
        assert_eq!(osk.layer, OskLayer::Symbols);
        assert!(osk.tap(OskKey::Symbols).is_none());
        assert_eq!(osk.layer, OskLayer::Lower);
    }

    #[test]
    fn hide_never_synthesizes_an_event() {
        let mut osk = Osk::new();
        assert!(osk.tap(OskKey::Hide).is_none());
    }
}
