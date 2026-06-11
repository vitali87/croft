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
//! like a phone's shift), and a symbols page. Caps Lock is a persistent
//! uppercase lock that - like a real Mac caps lock - uppercases letters
//! only, leaving digits and punctuation untouched. Ctrl and Alt are
//! one-shot latches that apply to the next key tap, so Ctrl+C / Ctrl+P
//! chords work with two taps.
//!
//! Geometry follows a physical keyboard rather than stretching every key
//! proportionally: structural keys carry a max width in cells, so on wide
//! frames (an unfolded foldable) they stay key-sized while the letters and
//! the space bar absorb the slack. The left column staggers like a MacBook:
//! esc < tab < caps < shift. A split mode (toggled by the `split` key,
//! persisted in prefs) breaks the board into two thumb clusters separated
//! by a center gap of about a sixth of the band width, mirroring Gboard's
//! foldable split with no duplicated letter keys.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Key rows on the keyboard (the band is `OSK_ROWS` x the per-key height).
pub const OSK_ROWS: u16 = 5;

/// Below this band width the split layout collapses back to merged: two
/// 20-cell half-boards are unusable, and the fold's outer screen is narrow.
const MIN_SPLIT_WIDTH: u16 = 60;

/// Split-mode center gap as a divisor of the band width (about 3cm on an
/// unfolded foldable's inner screen).
const SPLIT_GAP_DIV: u16 = 6;

/// Weight of an ordinary letter/digit key; structural weights are tuned
/// against this so narrow frames keep MacBook-like proportions.
const CHAR_WEIGHT: u16 = 4;

/// "No cap": letters and the space bar absorb whatever the structural keys
/// give up on wide frames.
const UNCAPPED: u16 = u16::MAX;

/// Band height for a given frame: keys grow to thumb size on tall (portrait
/// phone) frames and shrink back to one row each on short ones. Roughly 40%
/// of the frame, quantised to whole key-row heights between 1 and 4.
pub fn band_height(frame_height: u16) -> u16 {
    let row_h = (u32::from(frame_height) * 2 / 5 / u32::from(OSK_ROWS)).clamp(1, 4) as u16;
    OSK_ROWS * row_h
}

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
    /// Persistent uppercase lock (letters only, like a Mac caps lock).
    Caps,
    /// One-shot Ctrl latch for the next key.
    Ctrl,
    /// One-shot Alt latch, same lifecycle as `ctrl`.
    Alt,
    /// Toggle the Symbols layer.
    Symbols,
    /// Toggle the split (two thumb clusters) layout; persisted by the app.
    SplitToggle,
    /// The split-mode center gap: never tappable, never painted.
    Gap,
    /// Dismiss the keyboard (handled by the app, never synthesized).
    Hide,
}

pub struct Osk {
    pub layer: OskLayer,
    /// Persistent uppercase lock; orthogonal to `layer` so a Symbols trip
    /// and back lands on uppercase again.
    pub caps: bool,
    /// User's split-layout choice (persisted in prefs). Only takes effect
    /// on bands at least `MIN_SPLIT_WIDTH` wide; see `split_active`.
    pub split: bool,
    /// One-shot Ctrl latch: armed by tapping `ctrl`, consumed by the next
    /// character/named key.
    pub ctrl: bool,
    /// One-shot Alt latch, same lifecycle as `ctrl`.
    pub alt: bool,
    /// Band rect from the last layout; the mouse handler hit-tests this.
    pub last_area: Rect,
    /// Whether the last layout actually split (wants split AND wide enough).
    split_active: bool,
    /// Per-key hit rects from the last layout, in render order.
    keys: Vec<(Rect, OskKey)>,
}

/// One key slot in a row: display label (plus a short fallback for cramped
/// keys), the key it fires, its relative width, and the max width in cells
/// it may grow to on wide bands (`UNCAPPED` for letters and space).
struct KeySlot {
    label: String,
    short: &'static str,
    key: OskKey,
    weight: u16,
    max_w: u16,
}

fn slot(label: &str, short: &'static str, key: OskKey, weight: u16, max_w: u16) -> KeySlot {
    KeySlot {
        label: String::from(label),
        short,
        key,
        weight,
        max_w,
    }
}

fn char_slots(chars: &str) -> Vec<KeySlot> {
    chars
        .chars()
        .map(|c| KeySlot {
            label: c.to_string(),
            short: "",
            key: OskKey::Char(c),
            weight: CHAR_WEIGHT,
            max_w: UNCAPPED,
        })
        .collect()
}

fn gap_slot() -> KeySlot {
    slot("", "", OskKey::Gap, 0, 0)
}

/// Insert the split gap after `at` slots (the left thumb cluster).
fn split_at(mut row: Vec<KeySlot>, at: usize) -> Vec<KeySlot> {
    row.insert(at.min(row.len()), gap_slot());
    row
}

fn rows_for(layer: OskLayer, caps: bool, split: bool) -> Vec<Vec<KeySlot>> {
    // MacBook left-column stagger: esc < tab < caps < shift (1 : 1.5 :
    // 1.75 : 2.25 key units, quantised to cells).
    let esc = || slot("esc", "", OskKey::Code(KeyCode::Esc), 5, 5);
    let bksp = || slot("⌫", "", OskKey::Code(KeyCode::Backspace), 6, 6);
    let tab = || slot("⇥ tab", "⇥", OskKey::Code(KeyCode::Tab), 6, 6);
    let caps_k = || slot("⇪ caps", "⇪", OskKey::Caps, 7, 7);
    let enter = || slot("⏎", "", OskKey::Code(KeyCode::Enter), 7, 8);
    let shift = || slot("⇧ shift", "⇧", OskKey::Shift, 9, 9);
    let row = |left: KeySlot, mid: &str, right: Option<KeySlot>| {
        let mut r = vec![left];
        r.extend(char_slots(mid));
        r.extend(right);
        r
    };
    // Bottom row is shared by every layer; the layer key relabels itself.
    // Ctrl lives here, next to Alt, like a physical keyboard's bottom row.
    let bottom = |symbols_label: &str| {
        let mut left = vec![
            slot(symbols_label, "", OskKey::Symbols, 6, 6),
            slot("ctrl", "^", OskKey::Ctrl, 6, 6),
            slot("alt", "", OskKey::Alt, 5, 5),
            slot(" ", "", OskKey::Char(' '), 14, UNCAPPED),
        ];
        let right = vec![
            slot("←", "", OskKey::Code(KeyCode::Left), 4, 6),
            slot("↓", "", OskKey::Code(KeyCode::Down), 4, 6),
            slot("↑", "", OskKey::Code(KeyCode::Up), 4, 6),
            slot("→", "", OskKey::Code(KeyCode::Right), 4, 6),
            slot("split", "][", OskKey::SplitToggle, 4, 5),
            slot("⌄", "", OskKey::Hide, 8, 10),
        ];
        if split {
            // Both thumbs get a space bar, Gboard-style.
            left.push(gap_slot());
            left.push(slot(" ", "", OskKey::Char(' '), 14, UNCAPPED));
        }
        left.extend(right);
        left
    };
    // Caps Lock uppercases letters only: digits and punctuation rows stay
    // as on the Lower layer, unlike the one-shot Shift layer.
    let (digits, top, home, low) = match (layer, caps) {
        (OskLayer::Lower, false) => ("1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm,./"),
        (OskLayer::Lower, true) => ("1234567890", "QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM,./"),
        (OskLayer::Upper, _) => ("!@#$%^&*()", "QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM<>?"),
        (OskLayer::Symbols, _) => ("1234567890", "`~[]{}()<>", "-_=+;:'\"", "!@#$%\\|,.?"),
    };
    let layer_label = if layer == OskLayer::Symbols {
        "abc"
    } else {
        "&123"
    };
    let mut rows = vec![
        row(esc(), digits, Some(bksp())),
        row(tab(), top, None),
        row(caps_k(), home, Some(enter())),
        row(shift(), low, None),
    ];
    if split {
        // Left thumb cluster = structural key + first five characters
        // (esc 12345 | tab qwert | caps asdfg | shift zxcvb).
        rows = rows.into_iter().map(|r| split_at(r, 6)).collect();
    }
    rows.push(bottom(layer_label));
    rows
}

/// Per-row key widths: proportional by weight, but any key whose share
/// exceeds its `max_w` is pinned there and the freed cells flow back to the
/// uncapped keys (water-filling). The final flex pass uses cumulative
/// rounding so the row stays pinned to its region's full width with no
/// drift.
fn solve_widths(row: &[KeySlot], avail: u32) -> Vec<u16> {
    let mut widths = vec![0u16; row.len()];
    let mut fixed = vec![false; row.len()];
    loop {
        let used: u32 = widths
            .iter()
            .zip(&fixed)
            .filter(|(_, f)| **f)
            .map(|(w, _)| u32::from(*w))
            .sum();
        let rem = avail.saturating_sub(used);
        let wsum: u32 = row
            .iter()
            .zip(&fixed)
            .filter(|(_, f)| !**f)
            .map(|(s, _)| u32::from(s.weight))
            .sum();
        if wsum == 0 {
            return widths;
        }
        let mut changed = false;
        for (i, s) in row.iter().enumerate() {
            if !fixed[i] && rem * u32::from(s.weight) / wsum > u32::from(s.max_w) {
                widths[i] = s.max_w;
                fixed[i] = true;
                changed = true;
            }
        }
        if changed {
            continue;
        }
        let (mut cum, mut consumed) = (0u32, 0u32);
        for (i, s) in row.iter().enumerate() {
            if fixed[i] {
                continue;
            }
            cum += u32::from(s.weight);
            let end = rem * cum / wsum;
            widths[i] = ((end - consumed) as u16).max(1);
            consumed = end;
        }
        return widths;
    }
}

impl Osk {
    pub fn new() -> Self {
        Self {
            layer: OskLayer::Lower,
            caps: false,
            split: false,
            ctrl: false,
            alt: false,
            last_area: Rect::default(),
            split_active: false,
            keys: Vec::new(),
        }
    }

    /// Slot rows for the current layer/caps/split state, as last laid out.
    fn rows(&self) -> Vec<Vec<KeySlot>> {
        rows_for(self.layer, self.caps, self.split_active)
    }

    /// Recompute per-key hit rects for the current layer inside `area`.
    /// Each key row gets an equal share of the band's height, so taller
    /// bands directly mean taller (thumb-sized) keys. In split mode each
    /// half is solved independently inside its own region, so the center
    /// channel stays a straight column across every row regardless of how
    /// unevenly the clusters are loaded.
    pub fn layout(&mut self, area: Rect) {
        self.last_area = area;
        self.split_active = self.split && area.width >= MIN_SPLIT_WIDTH;
        self.keys.clear();
        let row_h = (area.height / OSK_ROWS).max(1);
        let gap_w = area.width / SPLIT_GAP_DIV;
        let half_w = (area.width - gap_w) / 2;
        let rows = self.rows();
        // In merged mode the Enter key grows into a two-row L on the right,
        // like a physical keyboard. The home row (the one carrying Enter) is
        // solved at full width; the row directly below it (shift) reserves
        // Enter's column so its letters stop just left of the cap - which is
        // what nudges the trailing `/` in off the screen edge.
        let tall_enter_row = if self.split_active {
            None
        } else {
            rows.iter()
                .position(|r| r.iter().any(|s| s.key == OskKey::Code(KeyCode::Enter)))
        };
        let mut enter_x: Option<u16> = None;
        for (i, row) in rows.iter().enumerate() {
            let y = area.y + i as u16 * row_h;
            if y >= area.y.saturating_add(area.height) {
                break;
            }
            // The shift row keeps clear of the Enter column carried down from
            // the home row above it.
            let region_w = match (tall_enter_row, enter_x) {
                (Some(hr), Some(ex)) if i == hr + 1 => ex.saturating_sub(area.x).saturating_sub(1),
                _ => area.width,
            };
            match row.iter().position(|s| s.key == OskKey::Gap) {
                None => {
                    self.place_half(row, area.x, region_w, y, row_h);
                    // Promote the just-placed Enter to a two-row-tall cap and
                    // record its column for the row below to clear.
                    if tall_enter_row == Some(i)
                        && let Some((rect, _)) = self
                            .keys
                            .iter_mut()
                            .rev()
                            .find(|(_, k)| *k == OskKey::Code(KeyCode::Enter))
                    {
                        let max_h = area.height.saturating_sub(rect.y - area.y);
                        rect.height = (row_h * 2).min(max_h);
                        enter_x = Some(rect.x);
                    }
                }
                Some(at) => {
                    self.place_half(&row[..at], area.x, half_w, y, row_h);
                    self.keys.push((
                        Rect {
                            x: area.x + half_w,
                            y,
                            width: gap_w,
                            height: row_h,
                        },
                        OskKey::Gap,
                    ));
                    let right_x = area.x + half_w + gap_w;
                    let right_w = area.width - half_w - gap_w;
                    self.place_half(&row[at + 1..], right_x, right_w, y, row_h);
                }
            }
        }
    }

    /// Lay one row (or one split half) into `[x, x + region_w)`.
    fn place_half(&mut self, slots: &[KeySlot], x: u16, region_w: u16, y: u16, row_h: u16) {
        let gaps = slots.len().saturating_sub(1) as u16;
        let avail = u32::from(region_w.saturating_sub(gaps));
        if avail == 0 {
            return;
        }
        let widths = solve_widths(slots, avail);
        let mut x = x;
        for (s, kw) in slots.iter().zip(widths) {
            self.keys.push((
                Rect {
                    x,
                    y,
                    width: kw,
                    height: row_h,
                },
                s.key,
            ));
            x = x.saturating_add(kw + 1);
        }
    }

    /// The key under a screen cell, if any. The full key rect is the hit
    /// target - including the visual gap row under tall keys - so thumb
    /// taps that graze a key's edge still land. The split-mode center gap
    /// is dead space and never matches.
    pub fn key_at(&self, col: u16, row: u16) -> Option<OskKey> {
        self.keys
            .iter()
            .filter(|(_, k)| *k != OskKey::Gap)
            .find(|(r, _)| {
                row >= r.y
                    && row < r.y.saturating_add(r.height)
                    && col >= r.x
                    && col < r.x.saturating_add(r.width)
            })
            .map(|(_, k)| *k)
    }

    /// The hit rect of `key` on the current layer. Test-only: production
    /// code goes the other way (cell -> key) through `key_at`.
    #[cfg(test)]
    pub fn rect_for(&self, key: OskKey) -> Option<Rect> {
        self.keys.iter().find(|(_, k)| *k == key).map(|(r, _)| *r)
    }

    /// How many slots fire `key` in the current layout. Test-only.
    #[cfg(test)]
    pub fn count_of(&self, key: OskKey) -> usize {
        self.keys.iter().filter(|(_, k)| *k == key).count()
    }

    /// Apply a tapped key. Layer/modifier keys mutate state and return
    /// `None`; character/named keys return the synthesized `KeyEvent` with
    /// any one-shot latches consumed. `Hide` always returns `None` - the
    /// app dismisses the keyboard before calling this. `SplitToggle`
    /// returns `None` too; the app persists the new choice.
    pub fn tap(&mut self, key: OskKey) -> Option<KeyEvent> {
        match key {
            OskKey::Shift => {
                // Shift releases an armed caps lock; otherwise it is the
                // usual one-shot uppercase toggle.
                if self.caps {
                    self.caps = false;
                    self.layer = OskLayer::Lower;
                } else {
                    self.layer = match self.layer {
                        OskLayer::Upper => OskLayer::Lower,
                        _ => OskLayer::Upper,
                    };
                }
                None
            }
            OskKey::Caps => {
                self.caps = !self.caps;
                if self.layer == OskLayer::Upper {
                    self.layer = OskLayer::Lower;
                }
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
            OskKey::SplitToggle => {
                self.split = !self.split;
                None
            }
            OskKey::Gap | OskKey::Hide => None,
            OskKey::Char(c) => {
                let ev = KeyEvent::new(KeyCode::Char(c), self.take_latches());
                // One-shot shift, phone-style: typing a character drops the
                // Upper layer back to Lower. Symbols stays until toggled,
                // and caps lock holds until released.
                if self.layer == OskLayer::Upper {
                    self.layer = OskLayer::Lower;
                }
                Some(ev)
            }
            // Shift+Tab on a one-shot Upper layer synthesizes BackTab, the
            // only path by which the soft keyboard can emit the `ESC [ Z`
            // backtab sequence (key_to_bytes). Consoles like Claude Code
            // cycle modes on backtab, unreachable otherwise since the OSK's
            // Shift is a layer toggle, not a modifier carried by take_latches.
            // Caps lock (letters-only) deliberately does not trigger this.
            OskKey::Code(KeyCode::Tab) if self.layer == OskLayer::Upper => {
                self.layer = OskLayer::Lower;
                Some(KeyEvent::new(KeyCode::BackTab, self.take_latches()))
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
    /// or the key that owns the active layer/lock.
    fn is_armed(&self, key: OskKey) -> bool {
        match key {
            OskKey::Ctrl => self.ctrl,
            OskKey::Alt => self.alt,
            OskKey::Shift => self.layer == OskLayer::Upper,
            OskKey::Caps => self.caps,
            OskKey::Symbols => self.layer == OskLayer::Symbols,
            OskKey::SplitToggle => self.split,
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
    let rows = osk.rows();
    let slots: Vec<&KeySlot> = rows.iter().flatten().collect();
    for ((rect, key), s) in osk.keys.iter().zip(slots) {
        if *key == OskKey::Gap {
            continue;
        }
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
        // Cramped keys fall back to their short glyph label instead of
        // truncating the word ("shift" -> "⇧").
        let label = if s.label.chars().count() > w && !s.short.is_empty() {
            s.short
        } else {
            s.label.as_str()
        };
        let chars = label.chars().count();
        let pad = w.saturating_sub(chars) / 2;
        let mut cell = " ".repeat(pad);
        cell.push_str(label);
        cell.push_str(&" ".repeat(w.saturating_sub(cell.chars().count())));
        // Tall keys keep their last row as a panel-bg gap so adjacent key
        // rows read as separate caps (the gap still hit-tests as the key);
        // the label sits on the cap's vertical middle row.
        let visual_h = if rect.height >= 2 {
            rect.height - 1
        } else {
            rect.height
        };
        let label_y = rect.y + (visual_h.saturating_sub(1)) / 2;
        let fill = " ".repeat(w);
        for y in rect.y..rect.y + visual_h {
            let text = if y == label_y { &cell } else { &fill };
            buf.set_stringn(rect.x, y, text, w, style);
        }
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
            height: OSK_ROWS,
        }
    }

    fn wide() -> Rect {
        Rect {
            x: 0,
            y: 34,
            width: 120,
            height: OSK_ROWS,
        }
    }

    #[test]
    fn layout_fills_band_and_every_row_hit_tests() {
        let mut osk = Osk::new();
        osk.layout(band());
        assert_eq!(osk.last_area, band());
        // Every row of the band carries at least one tappable key.
        for row in band().y..band().y + OSK_ROWS {
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
            OskKey::Caps,
            OskKey::Ctrl,
            OskKey::Alt,
            OskKey::Symbols,
            OskKey::SplitToggle,
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
    fn shift_tab_synthesizes_backtab() {
        let mut osk = Osk::new();
        // Plain Tab is forward-tab, no modifier.
        let ev = osk.tap(OskKey::Code(KeyCode::Tab)).unwrap();
        assert_eq!(ev.code, KeyCode::Tab);
        assert_eq!(ev.modifiers, KeyModifiers::NONE);

        // Shift then Tab emits BackTab (the `ESC [ Z` backtab sequence) so
        // consoles like Claude Code can cycle modes from the soft keyboard.
        assert!(osk.tap(OskKey::Shift).is_none());
        assert_eq!(osk.layer, OskLayer::Upper);
        let ev = osk.tap(OskKey::Code(KeyCode::Tab)).unwrap();
        assert_eq!(ev.code, KeyCode::BackTab);
        // The one-shot shift is consumed, mirroring character taps.
        assert_eq!(osk.layer, OskLayer::Lower);

        // Caps lock (letters-only) leaves Tab as a forward-tab.
        assert!(osk.tap(OskKey::Caps).is_none());
        let ev = osk.tap(OskKey::Code(KeyCode::Tab)).unwrap();
        assert_eq!(ev.code, KeyCode::Tab);
    }

    #[test]
    fn caps_lock_uppercases_letters_only_until_released() {
        let mut osk = Osk::new();
        assert!(osk.tap(OskKey::Caps).is_none());
        assert!(osk.caps);
        osk.layout(band());
        // Letters are uppercase; digits and punctuation stay as on Lower
        // (a real caps lock, not a locked Shift layer).
        for c in ['A', 'Q', 'Z', '1', '0', ',', '.', '/'] {
            assert!(
                osk.rect_for(OskKey::Char(c)).is_some(),
                "missing key {c:?} under caps lock"
            );
        }
        assert!(
            osk.rect_for(OskKey::Char('!')).is_none(),
            "caps lock must not expose the Shift layer's symbol row"
        );
        // Typing does NOT release the lock (unlike one-shot Shift).
        let ev = osk.tap(OskKey::Char('A')).unwrap();
        assert_eq!(ev.code, KeyCode::Char('A'));
        assert!(osk.caps, "caps lock persists across keystrokes");
        // A Symbols round-trip lands back on uppercase.
        osk.tap(OskKey::Symbols);
        osk.tap(OskKey::Symbols);
        assert!(osk.caps);
        // Shift releases the lock.
        assert!(osk.tap(OskKey::Shift).is_none());
        assert!(!osk.caps);
        assert_eq!(osk.layer, OskLayer::Lower);
        // Tapping caps again toggles it off too.
        osk.tap(OskKey::Caps);
        osk.tap(OskKey::Caps);
        assert!(!osk.caps);
    }

    #[test]
    fn left_column_staggers_like_a_macbook() {
        let mut osk = Osk::new();
        osk.layout(wide());
        let esc = osk.rect_for(OskKey::Code(KeyCode::Esc)).unwrap();
        let tab = osk.rect_for(OskKey::Code(KeyCode::Tab)).unwrap();
        let caps = osk.rect_for(OskKey::Caps).unwrap();
        let shift = osk.rect_for(OskKey::Shift).unwrap();
        assert!(
            esc.width < tab.width && tab.width < caps.width && caps.width < shift.width,
            "MacBook stagger esc<tab<caps<shift, got {} {} {} {}",
            esc.width,
            tab.width,
            caps.width,
            shift.width
        );
    }

    #[test]
    fn structural_keys_stop_stretching_on_wide_bands() {
        let mut osk = Osk::new();
        osk.layout(wide());
        // Structural keys pin at their caps; letters absorb the slack and
        // end up wider than tab, the widest top-left structural key.
        assert_eq!(osk.rect_for(OskKey::Code(KeyCode::Esc)).unwrap().width, 5);
        assert_eq!(osk.rect_for(OskKey::Code(KeyCode::Tab)).unwrap().width, 6);
        assert_eq!(osk.rect_for(OskKey::Caps).unwrap().width, 7);
        assert_eq!(osk.rect_for(OskKey::Shift).unwrap().width, 9);
        assert_eq!(
            osk.rect_for(OskKey::Code(KeyCode::Backspace))
                .unwrap()
                .width,
            6
        );
        let q = osk.rect_for(OskKey::Char('q')).unwrap();
        let tab = osk.rect_for(OskKey::Code(KeyCode::Tab)).unwrap();
        assert!(
            q.width > tab.width,
            "letters ({}) must out-grow structural keys ({}) on wide bands",
            q.width,
            tab.width
        );
    }

    #[test]
    fn ctrl_sits_on_the_bottom_row_next_to_alt() {
        let mut osk = Osk::new();
        osk.layout(band());
        let ctrl = osk.rect_for(OskKey::Ctrl).unwrap();
        let alt = osk.rect_for(OskKey::Alt).unwrap();
        let shift = osk.rect_for(OskKey::Shift).unwrap();
        let space = osk.rect_for(OskKey::Char(' ')).unwrap();
        assert_eq!(ctrl.y, alt.y, "ctrl and alt share the bottom row");
        assert!(ctrl.y > shift.y, "ctrl lives below the shift row");
        assert!(ctrl.x < alt.x && alt.x < space.x, "order: ctrl, alt, space");
    }

    #[test]
    fn collapse_button_outgrows_split() {
        let mut osk = Osk::new();
        osk.layout(wide());
        let hide = osk.rect_for(OskKey::Hide).unwrap();
        let split = osk.rect_for(OskKey::SplitToggle).unwrap();
        assert!(
            hide.width >= split.width * 2,
            "collapse ({}) must be about twice the split key ({})",
            hide.width,
            split.width
        );
    }

    #[test]
    fn merged_enter_is_a_two_row_l_and_pushes_slash_off_the_edge() {
        let mut osk = Osk::new();
        let tall = Rect {
            x: 0,
            y: 20,
            width: 80,
            height: 15,
        };
        osk.layout(tall);
        let row_h = 15 / OSK_ROWS;
        let enter = osk.rect_for(OskKey::Code(KeyCode::Enter)).unwrap();
        assert_eq!(enter.height, row_h * 2, "Enter spans two key rows");
        // It hit-tests across both rows it covers.
        let cx = enter.x + enter.width / 2;
        assert_eq!(osk.key_at(cx, enter.y), Some(OskKey::Code(KeyCode::Enter)));
        assert_eq!(
            osk.key_at(cx, enter.y + row_h),
            Some(OskKey::Code(KeyCode::Enter)),
            "the lower arm of the L hit-tests as Enter too"
        );
        // `/` is the last shift-row key and now sits just left of the Enter
        // column rather than at the band's right edge.
        let slash = osk.rect_for(OskKey::Char('/')).unwrap();
        assert_eq!(slash.y, enter.y + row_h, "`/` is on the shift row");
        assert!(
            slash.x + slash.width <= enter.x,
            "`/` ({}) must clear the Enter column ({})",
            slash.x + slash.width,
            enter.x
        );
        assert!(
            enter.x + enter.width > slash.x + slash.width,
            "Enter, not `/`, owns the right edge"
        );
    }

    #[test]
    fn split_keeps_enter_a_single_row() {
        let mut osk = Osk::new();
        osk.split = true;
        osk.layout(wide());
        let row_h = wide().height / OSK_ROWS;
        let enter = osk.rect_for(OskKey::Code(KeyCode::Enter)).unwrap();
        assert_eq!(enter.height, row_h, "split mode keeps a one-row Enter");
    }

    #[test]
    fn split_layout_breaks_into_two_thumb_clusters_with_a_center_gap() {
        let mut osk = Osk::new();
        osk.split = true;
        osk.layout(wide());
        let area = wide();
        let center = area.x + area.width / 2;
        let row_y = |i: u16| area.y + i;
        // The center column is dead space on every row.
        for i in 0..OSK_ROWS {
            assert_eq!(
                osk.key_at(center, row_y(i)),
                None,
                "row {i} must have a center gap"
            );
        }
        // Gboard's 5|5 split with no duplicated keys: t/g/v/5 end the left
        // half, y/h/b/6 start the right half.
        for (l, r) in [('5', '6'), ('t', 'y'), ('g', 'h'), ('b', 'n')] {
            let lr = osk.rect_for(OskKey::Char(l)).unwrap();
            let rr = osk.rect_for(OskKey::Char(r)).unwrap();
            assert!(
                lr.x + lr.width <= center && rr.x >= center,
                "{l} must sit left of the gap and {r} right of it"
            );
            assert!(
                rr.x - (lr.x + lr.width) >= area.width / SPLIT_GAP_DIV,
                "gap between {l} and {r} must be at least a sixth of the band"
            );
        }
        // Both halves carry a space bar.
        assert_eq!(osk.count_of(OskKey::Char(' ')), 2);
    }

    #[test]
    fn split_toggle_taps_toggle_and_narrow_bands_stay_merged() {
        let mut osk = Osk::new();
        assert!(osk.tap(OskKey::SplitToggle).is_none());
        assert!(osk.split);
        // A narrow band (folded front screen) ignores the split choice.
        let narrow = Rect {
            x: 0,
            y: 34,
            width: 40,
            height: OSK_ROWS,
        };
        osk.layout(narrow);
        assert_eq!(osk.count_of(OskKey::Gap), 0, "narrow bands never split");
        assert_eq!(osk.count_of(OskKey::Char(' ')), 1);
        osk.tap(OskKey::SplitToggle);
        assert!(!osk.split);
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
    fn band_height_scales_with_frame_for_thumb_typing() {
        assert_eq!(band_height(40), 15); // portrait phone: 3-row keys
        assert_eq!(band_height(50), 20); // tall portrait: 4-row keys
        assert_eq!(band_height(30), 10); // landscape: 2-row keys
        assert_eq!(band_height(24), 5); // short frame: 1-row keys
    }

    #[test]
    fn tall_band_lays_out_multi_row_keys_with_full_height_hit_targets() {
        let mut osk = Osk::new();
        let tall = Rect {
            x: 0,
            y: 20,
            width: 80,
            height: 15,
        };
        osk.layout(tall);
        let r = osk.rect_for(OskKey::Char('a')).unwrap();
        assert_eq!(r.height, 3, "15-row band gives every key 3 rows");
        for dy in 0..3 {
            assert_eq!(
                osk.key_at(r.x + r.width / 2, r.y + dy),
                Some(OskKey::Char('a')),
                "row {dy} of a tall key must hit-test as the key"
            );
        }
        // Key rows step by the full key height: q sits directly above a.
        let q = osk.rect_for(OskKey::Char('q')).unwrap();
        assert_eq!(q.y + 3, r.y);
    }

    #[test]
    fn hide_never_synthesizes_an_event() {
        let mut osk = Osk::new();
        assert!(osk.tap(OskKey::Hide).is_none());
    }
}
