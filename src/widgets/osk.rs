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
//! The layout mirrors Gboard's portrait QWERTY as closely as a terminal cell
//! grid allows. Five rows:
//!   - a top strip of croft-only keys: `esc ⇥tab F1 F2 … F10 split ⌄`. The
//!     F1..F10 keys are rendered narrower than letters so the whole row fits.
//!   - the three Gboard letter rows, with two SMALL shift-flipping arrow keys
//!     parked at the home row's edges: `←/→ a s d f g h j k l ↑/↓`, then
//!     `q w e r t y u i o p` above and `⇧ z x c v b n m ⌫` below.
//!   - a bottom row that mirrors a real keyboard: `?123 ctrl alt [space] alt
//!     ctrl ⏎`, with ctrl/alt flanking a shortened space bar on both sides.
//!
//! There is no permanent number row: digits live behind the `?123` toggle,
//! exactly like Gboard. Comma and period live on the `?123` page beside the
//! space bar (`abc ctrl alt , space . ⏎`), not on the letter page.
//!
//! The two edge arrows flip with shift/caps: `←` becomes `→` and `↑` becomes
//! `↓` while uppercase is active, and the key emits Right/Down accordingly.
//! So Shift now does three jobs: capitalise letters, double-tap = caps lock,
//! and flip the edge arrows.
//!
//! Geometry stretches the letters and the space bar while structural keys
//! carry a max width in cells, so on wide frames (an unfolded foldable) the
//! specials stay key-sized rather than ballooning. A split mode (toggled by
//! the `split` key, persisted in prefs) breaks the board into two thumb
//! clusters separated by a center gap of about two-ninths of the band width.
//! The top strip splits after F5 and the top letter row 5|5 (no duplication).
//! The home and shift rows duplicate their seam letter on both halves, the way
//! a real Pixel Fold Gboard split does: home reads `←/→ asdf g | g hjkl ↑/↓`
//! and shift reads `⇧ zxcv | v bnm ⌫`, so the inner-edge columns line up
//! T/G/V on the left and Y/G/V on the right. Tapping either copy of `g`/`v`
//! types that letter. Both thumbs get a space bar in split mode.

use crate::gradient::{PRIMARY_BTN_BG, rgb_color};
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Key rows on the keyboard (the band is `OSK_ROWS` x the per-key height).
pub const OSK_ROWS: u16 = 5;

/// Below this band width the split layout collapses back to merged: two
/// 20-cell half-boards are unusable, and the fold's outer screen is narrow.
const MIN_SPLIT_WIDTH: u16 = 60;

/// Split-mode center gap as a fraction of the band width: `2/9`, about 4cm on
/// an unfolded foldable's inner screen. Widened one centimetre past the
/// original `1/6` (~3cm) so thumbs get more separation on mobile.
const SPLIT_GAP_NUM: u16 = 2;
const SPLIT_GAP_DEN: u16 = 9;

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
    /// One-shot uppercase; a second tap before typing engages caps lock,
    /// exactly like Gboard's double-tap shift.
    Shift,
    /// Home-row left-edge arrow: emits Left normally and Right while shift or
    /// caps is active. The label flips with the same state.
    ArrowLR,
    /// Home-row right-edge arrow: emits Up normally and Down while shift or
    /// caps is active. The label flips with the same state.
    ArrowUD,
    /// A function key (F1..F10) on the top strip; emits `KeyCode::F(n)`, which
    /// `key_to_bytes` turns into the standard terminal sequence.
    Fn(u8),
    /// One-shot Ctrl latch for the next key.
    Ctrl,
    /// One-shot Alt latch, same lifecycle as `ctrl`.
    Alt,
    /// Toggle the Symbols layer.
    Symbols,
    /// Toggle the split (two thumb clusters) layout; persisted by the app.
    SplitToggle,
    /// Microphone (Termux only, via `termux-speech-to-text`): a tap starts
    /// speech recognition; the transcript is injected when you pause (Android
    /// finalizes on silence), and a second tap cancels. A tap rather than
    /// push-to-talk because Termux turns any finger hold into its own text
    /// selection. Handled entirely by the app, never synthesized into a
    /// `KeyEvent` here.
    Voice,
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

/// Weight of a function key on the top strip: about half a letter's, so the
/// ten F-keys render visibly narrower than the letters below.
const FN_WEIGHT: u16 = 2;

/// Nerd Font FontAwesome microphone glyph for the push-to-talk key. Termux
/// ships Meslo Nerd Font (auto-installed on first launch), so the PUA glyph
/// renders there; `short` falls back to "mic" on cramped bands.
const MIC_LABEL: &str = "\u{f130}";

/// Build the ten F1..F10 slots for the top strip. They carry a small weight
/// so they stay narrow, and are uncapped so they soak up the strip's slack
/// (every other strip key is capped, so without an uncapped key the row could
/// not fill the band and would leave dead space on the right).
fn fn_key_slots() -> Vec<KeySlot> {
    (1u8..=10)
        .map(|n| slot(&format!("F{n}"), "", OskKey::Fn(n), FN_WEIGHT, UNCAPPED))
        .collect()
}

fn rows_for(layer: OskLayer, caps: bool, split: bool) -> Vec<Vec<KeySlot>> {
    // Gboard portrait QWERTY: three letter rows plus a bottom row, with one
    // croft-only top strip. The letter rows mirror Gboard exactly so muscle
    // memory carries over. The home row gains two small shift-flipping arrow
    // keys at its edges; the bottom row flanks a shortened space bar with
    // ctrl/alt on both sides like a real keyboard; the top strip carries Esc,
    // Tab, the F1..F10 row, and croft's split/dismiss chrome.
    let bksp = || slot("⌫", "", OskKey::Code(KeyCode::Backspace), 9, 9);
    let enter = || slot("⏎", "", OskKey::Code(KeyCode::Enter), 6, 9);
    // Gboard's row-3 shift sits to the LEFT of `z`; backspace flanks `m` on
    // the right. The shift cap is wide like Gboard's, then water-fills.
    let shift = || slot("⇧", "", OskKey::Shift, 9, 9);
    // Shift/caps flips the edge arrows: ← becomes →, ↑ becomes ↓. The label
    // tracks the keystroke `tap` will synthesize. These are SMALL keys
    // (narrower than letters) parked at the home-row edges.
    let shifted = layer == OskLayer::Upper || caps;
    let arrow_lr = || slot(if shifted { "→" } else { "←" }, "", OskKey::ArrowLR, 3, 4);
    let arrow_ud = || slot(if shifted { "↓" } else { "↑" }, "", OskKey::ArrowUD, 3, 4);
    // The top strip: Esc, Tab, the F1..F10 row, then croft chrome (split,
    // dismiss). Ctrl/Alt and the arrows have moved off this strip.
    let top_strip = || {
        let mut r = vec![
            slot("esc", "", OskKey::Code(KeyCode::Esc), 5, 6),
            slot("⇥ tab", "⇥", OskKey::Code(KeyCode::Tab), 5, 7),
        ];
        r.extend(fn_key_slots());
        r.push(slot("split", "][", OskKey::SplitToggle, 4, 6));
        r.push(slot("⌄", "", OskKey::Hide, 5, 7));
        r
    };
    // The bottom row mirrors a real keyboard: ctrl/alt flank a shortened
    // space bar on both sides. Comma and period are permanent on every
    // layer (not hidden behind `?123`) and live at opposite ends so each
    // thumb has punctuation without crossing the keyboard: comma sits
    // immediately right of the layer toggle, period immediately left of
    // Enter. The bottom row is therefore `?123 , ctrl alt space alt ctrl . ⏎`
    // (with `abc` replacing `?123` on the symbols page).
    let symbols = layer == OskLayer::Symbols;
    let bottom = |layer_label: &str| {
        let mut r = vec![
            slot(layer_label, "", OskKey::Symbols, 6, 8),
            slot(",", "", OskKey::Char(','), 3, 5),
            slot("ctrl", "^", OskKey::Ctrl, 4, 6),
            slot("alt", "", OskKey::Alt, 4, 6),
        ];
        // Push-to-talk mic, immediately right of the left alt. It carries a
        // fixed weight like ctrl/alt, so the uncapped space bar water-fills
        // around it: the mic takes its cells from the space bar (the left
        // space half in split, where it sits in the left thumb cluster).
        r.push(slot(MIC_LABEL, "mic", OskKey::Voice, 4, 6));
        r.push(slot(" ", "", OskKey::Char(' '), 12, UNCAPPED));
        if split {
            // Both thumbs get a space bar, Gboard-style.
            r.push(gap_slot());
            r.push(slot(" ", "", OskKey::Char(' '), 12, UNCAPPED));
        }
        r.push(slot("alt", "", OskKey::Alt, 4, 6));
        r.push(slot("ctrl", "^", OskKey::Ctrl, 4, 6));
        r.push(slot(".", "", OskKey::Char('.'), 3, 5));
        r.push(enter());
        r
    };
    // The three Gboard letter rows. Caps Lock uppercases letters only, so
    // under caps the rows go upper while the symbols layer carries punctuation
    // as Gboard's symbol page does. There is no permanent number row: digits
    // live behind the `?123` toggle, exactly like Gboard.
    let (top, home, low) = match (layer, caps) {
        (OskLayer::Lower, false) => ("qwertyuiop", "asdfghjkl", "zxcvbnm"),
        (OskLayer::Lower, true) => ("QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"),
        (OskLayer::Upper, _) => ("QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"),
        (OskLayer::Symbols, _) => ("1234567890", "@#$_&-+()/", "*\"':;!?\\"),
    };
    let layer_label = if symbols { "abc" } else { "?123" };
    // The five-letter seam: the home row's left half ends with `home[..5]`'s
    // last letter (`g` on the letter page) and its right half starts with that
    // same letter again; likewise the shift row duplicates `low[..4]`'s last
    // letter (`v`). Verified against a real Pixel Fold Gboard split, where the
    // inner-edge columns line up T/G/V on the left and Y/G/V on the right.
    if split {
        // home letters split 5|5 with the 5th letter duplicated:
        //   left:  ←/→ home[0..5]            (… ends in `g`)
        //   right: home[4..] ↑/↓             (starts in `g` again)
        let home_left = {
            let mut r = vec![arrow_lr()];
            r.extend(char_slots(&char_prefix(home, 5)));
            r.push(gap_slot());
            r
        };
        let home_full = {
            let mut r = home_left;
            r.extend(char_slots(&char_suffix(home, 4)));
            r.push(arrow_ud());
            r
        };
        // shift letters split 4|3 with the 4th letter duplicated:
        //   left:  ⇧ low[0..4]               (… ends in `v`)
        //   right: low[3..] ⌫                (starts in `v` again)
        let shift_full = {
            let mut r = vec![shift()];
            r.extend(char_slots(&char_prefix(low, 4)));
            r.push(gap_slot());
            r.extend(char_slots(&char_suffix(low, 3)));
            r.push(bksp());
            r
        };
        // The top strip (esc tab F1-F5 | F6-F10 split hide) and the top letter
        // row (qwert | yuiop) split cleanly with no duplication.
        let rows = vec![
            split_at(top_strip(), 7),
            split_at(char_slots(top), 5),
            home_full,
            shift_full,
            bottom(layer_label),
        ];
        return rows;
    }
    // Merged: the home row carries the two small edge arrows flanking all its
    // letters; the shift row is shift + letters + backspace.
    let home_row = {
        let mut r = vec![arrow_lr()];
        r.extend(char_slots(home));
        r.push(arrow_ud());
        r
    };
    vec![
        top_strip(),
        char_slots(top),
        home_row,
        {
            let mut r = vec![shift()];
            r.extend(char_slots(low));
            r.push(bksp());
            r
        },
        bottom(layer_label),
    ]
}

/// The first `n` chars of `s` (by char, not byte). Used to carve the left
/// half of a split row whose seam letter is then duplicated on the right.
fn char_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The chars of `s` from index `start` onward (by char). The right half of a
/// split row starts here, re-including the seam letter the left half ended on.
fn char_suffix(s: &str, start: usize) -> String {
    s.chars().skip(start).collect()
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
        let gap_w = area.width * SPLIT_GAP_NUM / SPLIT_GAP_DEN;
        let half_w = (area.width - gap_w) / 2;
        let rows = self.rows();
        // Gboard keys are all one row tall: Enter lives on the final bottom
        // row with no row beneath to grow into, so there is no two-row "L".
        // Each row is laid straight; split rows carry a Gap slot that opens
        // the center channel between the two thumb clusters.
        for (i, row) in rows.iter().enumerate() {
            let y = area.y + i as u16 * row_h;
            if y >= area.y.saturating_add(area.height) {
                break;
            }
            match row.iter().position(|s| s.key == OskKey::Gap) {
                None => self.place_half(row, area.x, area.width, y, row_h),
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
                // Gboard shift: a tap arms one-shot uppercase; a second tap
                // before typing escalates to caps lock; a tap while caps is
                // engaged releases it. So the cycle is Lower -> Upper (one
                // shot) -> caps lock -> Lower.
                if self.caps {
                    self.caps = false;
                    self.layer = OskLayer::Lower;
                } else if self.layer == OskLayer::Upper {
                    // Second consecutive shift tap: engage caps lock. The base
                    // layer drops back to Lower; `caps` drives letter case.
                    self.caps = true;
                    self.layer = OskLayer::Lower;
                } else {
                    self.layer = OskLayer::Upper;
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
            // Voice toggles recognition at the app level (tap on, tap off), so
            // the tap itself emits no keystroke.
            OskKey::Gap | OskKey::Hide | OskKey::Voice => None,
            // The edge arrows flip with shift/caps: Left<->Right, Up<->Down.
            // A one-shot Upper layer is consumed by the tap (like a character),
            // so the next key returns to lowercase; caps lock holds.
            OskKey::ArrowLR => {
                let code = if self.shifted() {
                    KeyCode::Right
                } else {
                    KeyCode::Left
                };
                let ev = KeyEvent::new(code, self.take_latches());
                if self.layer == OskLayer::Upper {
                    self.layer = OskLayer::Lower;
                }
                Some(ev)
            }
            OskKey::ArrowUD => {
                let code = if self.shifted() {
                    KeyCode::Down
                } else {
                    KeyCode::Up
                };
                let ev = KeyEvent::new(code, self.take_latches());
                if self.layer == OskLayer::Upper {
                    self.layer = OskLayer::Lower;
                }
                Some(ev)
            }
            OskKey::Fn(n) => Some(KeyEvent::new(KeyCode::F(n), self.take_latches())),
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

    /// Whether uppercase is active (one-shot Upper or caps lock). Drives both
    /// letter case and the edge-arrow flip (←→ / ↑↓).
    fn shifted(&self) -> bool {
        self.layer == OskLayer::Upper || self.caps
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
            // Shift lights up for one-shot uppercase and stays lit while caps
            // lock is engaged (it is also the caps indicator, Gboard-style).
            OskKey::Shift => self.shifted(),
            OskKey::Symbols => self.layer == OskLayer::Symbols,
            OskKey::SplitToggle => self.split,
            // The mic glows while a recognition session is live (tap on, tap
            // off). Listening state is a process-global flag (like the
            // install-state atomics), so the band reads it directly without app
            // plumbing.
            OskKey::Voice => crate::voice::is_listening(),
            _ => false,
        }
    }
}

impl Default for Osk {
    fn default() -> Self {
        Self::new()
    }
}

/// Keyboard palette for one IDE theme: char-key caps raised a step above the
/// editor background, structural keys a step below the caps, and an armed
/// modifier on the theme's action accent.
struct OskPalette {
    fg: Color,
    armed_fg: Color,
    key_bg: Color,
    special_bg: Color,
    armed_bg: Color,
}

/// Resolve the keyboard palette so the band rhymes with the active theme:
/// neutral dark caps over a true-black band with the brand-teal armed accent
/// on Croft Black, the historical navy caps with the VS Code focus blue on
/// Croft Dark. Caps sit a step proud of the editor background either way, so
/// the band reads as raised chrome rather than a flat overlay.
fn palette(theme: Theme) -> OskPalette {
    match theme {
        Theme::Black => OskPalette {
            fg: Color::Rgb(0xd4, 0xd8, 0xe0),
            armed_fg: Color::White,
            key_bg: Color::Rgb(0x20, 0x24, 0x2b),
            special_bg: Color::Rgb(0x14, 0x16, 0x1b),
            // Brand teal — the same primary-action fill the Black theme uses
            // for buttons and active chrome elsewhere.
            armed_bg: rgb_color(PRIMARY_BTN_BG),
        },
        Theme::DarkBlue => OskPalette {
            fg: Color::Rgb(0xd4, 0xd8, 0xe0),
            armed_fg: Color::White,
            key_bg: Color::Rgb(0x3a, 0x40, 0x52),
            special_bg: Color::Rgb(0x2c, 0x31, 0x40),
            armed_bg: Color::Rgb(0x00, 0x7a, 0xcc),
        },
    }
}

/// Paint the keyboard band and refresh its hit rects. The band gap and key
/// caps are drawn from `theme`'s palette so the keyboard reads as part of the
/// active IDE theme's chrome.
pub fn render_osk(osk: &mut Osk, area: Rect, buf: &mut Buffer, theme: Theme) {
    osk.layout(area);
    let pal = palette(theme);
    let panel_bg = theme.editor_bg();
    let gap = Style::default().bg(panel_bg);
    let blank = " ".repeat(area.width as usize);
    for y in area.y..area.y.saturating_add(area.height) {
        buf.set_stringn(area.x, y, &blank, area.width as usize, gap);
    }
    let rows = osk.rows();
    let slots: Vec<&KeySlot> = rows.iter().flatten().collect();
    // Every key is one key-row tall, so the smallest placed rect is exactly
    // `row_h`. On tall bands (`row_h >= 2`) every cap gives up its last row
    // as a separating gap so adjacent rows read as distinct caps; on a
    // one-row band there is no slack to spare, so caps paint full height.
    let row_h = osk
        .keys
        .iter()
        .map(|(r, _)| r.height)
        .filter(|h| *h > 0)
        .min()
        .unwrap_or(1);
    for ((rect, key), s) in osk.keys.iter().zip(slots) {
        if *key == OskKey::Gap {
            continue;
        }
        let armed = osk.is_armed(*key);
        let special = !matches!(key, OskKey::Char(_));
        let style = if armed {
            Style::default()
                .fg(pal.armed_fg)
                .bg(pal.armed_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(pal.fg)
                .bg(if special { pal.special_bg } else { pal.key_bg })
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
        // Tall caps keep their last row as a panel-bg gap so adjacent key
        // rows read as separate caps (the gap still hit-tests as the key);
        // the label sits on the cap's vertical middle row.
        let visual_h = if row_h >= 2 {
            rect.height.saturating_sub(1)
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
        // The lower layer exposes the three Gboard letter rows. Digits are
        // NOT on the lower layer (they live behind `?123`, like Gboard).
        for c in ['a', 'q', 'z', 'p', 'l', 'm'] {
            assert!(
                osk.rect_for(OskKey::Char(c)).is_some(),
                "missing letter {c:?} on lower layer"
            );
        }
        assert!(
            osk.rect_for(OskKey::Char('1')).is_none(),
            "digits live behind ?123, not on the lower layer"
        );
        // Comma and period are permanent on the letter page (no longer
        // hidden behind ?123), parked at opposite ends of the bottom row.
        for c in [',', '.'] {
            assert!(
                osk.rect_for(OskKey::Char(c)).is_some(),
                "{c:?} is a permanent bottom-row key on the letter page"
            );
        }
        // The space bar is always present.
        assert!(osk.rect_for(OskKey::Char(' ')).is_some());
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
            OskKey::SplitToggle,
            OskKey::Hide,
            OskKey::ArrowLR,
            OskKey::ArrowUD,
            OskKey::Voice,
        ] {
            assert!(osk.rect_for(k).is_some(), "missing key {k:?}");
        }
        // All ten function keys are present on the top strip.
        for n in 1u8..=10 {
            assert!(
                osk.rect_for(OskKey::Fn(n)).is_some(),
                "missing function key F{n}"
            );
        }
        // rect_for and key_at agree: the center of the `a` key hits `a`.
        let r = osk.rect_for(OskKey::Char('a')).unwrap();
        assert_eq!(osk.key_at(r.x + r.width / 2, r.y), Some(OskKey::Char('a')));
    }

    #[test]
    fn rows_follow_gboard_order() {
        let mut osk = Osk::new();
        osk.layout(band());
        // Row 0 is the croft top strip (esc on the far left); rows 1-3 are
        // the three Gboard letter rows; row 4 is the bottom row.
        let y_of = |k: OskKey| osk.rect_for(k).unwrap().y;
        let esc_y = y_of(OskKey::Code(KeyCode::Esc));
        let q_y = y_of(OskKey::Char('q'));
        let a_y = y_of(OskKey::Char('a'));
        let z_y = y_of(OskKey::Char('z'));
        let space_y = y_of(OskKey::Char(' '));
        assert!(
            esc_y < q_y && q_y < a_y && a_y < z_y && z_y < space_y,
            "rows must descend: strip < qwerty < home < zxcvbnm < bottom"
        );
        // Gboard row 3: shift on the far left, backspace on the far right,
        // flanking z..m.
        let shift = osk.rect_for(OskKey::Shift).unwrap();
        let z = osk.rect_for(OskKey::Char('z')).unwrap();
        let m = osk.rect_for(OskKey::Char('m')).unwrap();
        let bksp = osk.rect_for(OskKey::Code(KeyCode::Backspace)).unwrap();
        assert_eq!(shift.y, z.y, "shift shares the zxcvbnm row");
        assert_eq!(bksp.y, z.y, "backspace shares the zxcvbnm row");
        assert!(
            shift.x < z.x && m.x < bksp.x,
            "shift left of z, backspace right of m"
        );
    }

    #[test]
    fn mic_sits_immediately_right_of_the_left_alt_in_both_layouts() {
        // Merged and split bottom rows both flow through `bottom()`, so the
        // mic lands right after the left alt in each. (The left alt is the
        // first `Alt`; the right alt flanks the space on the other side.)
        for split in [false, true] {
            let rows = rows_for(OskLayer::Lower, false, split);
            let bottom = rows.last().expect("bottom row");
            let alt_idx = bottom
                .iter()
                .position(|s| s.key == OskKey::Alt)
                .expect("left alt");
            assert_eq!(
                bottom[alt_idx + 1].key,
                OskKey::Voice,
                "mic must sit immediately right of the left alt (split={split})"
            );
        }
    }

    #[test]
    fn mic_is_in_the_left_thumb_cluster_when_split() {
        // The split gap divides the two thumb clusters; the mic must fall in
        // the left one (before the gap) so it eats into the left space half.
        let rows = rows_for(OskLayer::Lower, false, true);
        let bottom = rows.last().expect("bottom row");
        let mic_idx = bottom
            .iter()
            .position(|s| s.key == OskKey::Voice)
            .expect("mic");
        let gap_idx = bottom
            .iter()
            .position(|s| s.key == OskKey::Gap)
            .expect("split gap");
        assert!(
            mic_idx < gap_idx,
            "mic must be in the left thumb cluster, before the split gap"
        );
    }

    #[test]
    fn mic_sits_between_the_left_alt_and_the_space_bar_on_screen() {
        let mut osk = Osk::new();
        osk.layout(band());
        let alt = osk.rect_for(OskKey::Alt).expect("left alt"); // first match
        let mic = osk.rect_for(OskKey::Voice).expect("mic");
        let space = osk.rect_for(OskKey::Char(' ')).expect("space");
        assert_eq!(
            mic.y, space.y,
            "mic rides the bottom row with the space bar"
        );
        assert!(
            alt.x < mic.x && mic.x < space.x,
            "mic renders between the left alt and the space bar"
        );
    }

    #[test]
    fn tapping_the_mic_emits_no_key_event() {
        // Voice is push-to-talk, handled by the app on press/release; the tap
        // itself must not synthesize a keystroke.
        let mut osk = Osk::new();
        assert_eq!(osk.tap(OskKey::Voice), None);
    }

    #[test]
    fn home_row_has_small_edge_arrows_flanking_the_letters() {
        let mut osk = Osk::new();
        osk.layout(band());
        let lr = osk.rect_for(OskKey::ArrowLR).unwrap();
        let ud = osk.rect_for(OskKey::ArrowUD).unwrap();
        let a = osk.rect_for(OskKey::Char('a')).unwrap();
        let l = osk.rect_for(OskKey::Char('l')).unwrap();
        // Both arrows share the home row, ←/→ left of `a`, ↑/↓ right of `l`.
        assert_eq!(lr.y, a.y, "the left arrow rides the home row");
        assert_eq!(ud.y, a.y, "the right arrow rides the home row");
        assert!(lr.x < a.x, "←/→ sits left of `a`");
        assert!(ud.x > l.x, "↑/↓ sits right of `l`");
        // They are SMALL keys: narrower than an ordinary letter key.
        assert!(
            lr.width < a.width && ud.width < a.width,
            "edge arrows ({}, {}) must be narrower than a letter ({})",
            lr.width,
            ud.width,
            a.width
        );
    }

    #[test]
    fn edge_arrows_emit_left_up_and_flip_to_right_down_under_shift() {
        let mut osk = Osk::new();
        // Default (no shift): ← emits Left, ↑ emits Up.
        let ev = osk.tap(OskKey::ArrowLR).unwrap();
        assert_eq!(ev.code, KeyCode::Left);
        let ev = osk.tap(OskKey::ArrowUD).unwrap();
        assert_eq!(ev.code, KeyCode::Up);

        // With shift held the left/up keys flip to Right/Down. The one-shot
        // shift is consumed by the tap, so re-arm shift before each.
        osk.tap(OskKey::Shift);
        let ev = osk.tap(OskKey::ArrowLR).unwrap();
        assert_eq!(ev.code, KeyCode::Right, "shift flips ←/→ to Right");
        assert_eq!(osk.layer, OskLayer::Lower, "the one-shot shift is consumed");
        osk.tap(OskKey::Shift);
        let ev = osk.tap(OskKey::ArrowUD).unwrap();
        assert_eq!(ev.code, KeyCode::Down, "shift flips ↑/↓ to Down");

        // Under caps lock the flip holds across taps without re-arming.
        osk.tap(OskKey::Shift);
        osk.tap(OskKey::Shift); // double-tap -> caps lock
        assert!(osk.caps);
        assert_eq!(osk.tap(OskKey::ArrowLR).unwrap().code, KeyCode::Right);
        assert_eq!(osk.tap(OskKey::ArrowUD).unwrap().code, KeyCode::Down);
        assert!(osk.caps, "caps lock persists across arrow taps");
    }

    #[test]
    fn edge_arrow_labels_flip_with_shift_state() {
        // Lower layer: the labels read ← and ↑.
        let rows = rows_for(OskLayer::Lower, false, false);
        let label_of = |key: OskKey| {
            rows.iter()
                .flatten()
                .find(|s| s.key == key)
                .map(|s| s.label.clone())
                .unwrap()
        };
        assert_eq!(label_of(OskKey::ArrowLR), "←");
        assert_eq!(label_of(OskKey::ArrowUD), "↑");
        // Upper / caps: the labels flip to → and ↓.
        for rows in [
            rows_for(OskLayer::Upper, false, false),
            rows_for(OskLayer::Lower, true, false),
        ] {
            let label_of = |key: OskKey| {
                rows.iter()
                    .flatten()
                    .find(|s| s.key == key)
                    .map(|s| s.label.clone())
                    .unwrap()
            };
            assert_eq!(label_of(OskKey::ArrowLR), "→");
            assert_eq!(label_of(OskKey::ArrowUD), "↓");
        }
    }

    #[test]
    fn function_row_spans_f1_to_f10_on_the_top_strip() {
        let mut osk = Osk::new();
        osk.layout(band());
        let esc = osk.rect_for(OskKey::Code(KeyCode::Esc)).unwrap();
        let tab = osk.rect_for(OskKey::Code(KeyCode::Tab)).unwrap();
        // F1..F10 all sit on the top strip, in ascending left-to-right order,
        // to the right of Tab.
        let mut prev_x = tab.x;
        for n in 1u8..=10 {
            let f = osk.rect_for(OskKey::Fn(n)).unwrap();
            assert_eq!(f.y, esc.y, "F{n} rides the top strip");
            assert!(f.x > prev_x, "F{n} must sit right of the previous key");
            prev_x = f.x;
        }
        // No F11/F12: the cutoff is F10.
        assert!(osk.rect_for(OskKey::Fn(11)).is_none(), "cutoff is F10");
        // Function keys are SMALL: narrower than a letter key.
        let q = osk.rect_for(OskKey::Char('q')).unwrap();
        let f1 = osk.rect_for(OskKey::Fn(1)).unwrap();
        assert!(
            f1.width < q.width,
            "F1 ({}) must be narrower than a letter ({})",
            f1.width,
            q.width
        );
    }

    #[test]
    fn function_keys_emit_their_function_keycode() {
        let mut osk = Osk::new();
        for n in 1u8..=10 {
            let ev = osk.tap(OskKey::Fn(n)).unwrap();
            assert_eq!(ev.code, KeyCode::F(n), "F{n} must emit KeyCode::F({n})");
            assert_eq!(ev.modifiers, KeyModifiers::NONE);
        }
    }

    #[test]
    fn bottom_row_flanks_a_shortened_space_with_ctrl_and_alt() {
        let mut osk = Osk::new();
        osk.layout(band());
        // Letter-page bottom row: ?123 ctrl alt [space] alt ctrl ⏎.
        let sym = osk.rect_for(OskKey::Symbols).unwrap();
        let space = osk.rect_for(OskKey::Char(' ')).unwrap();
        let enter = osk.rect_for(OskKey::Code(KeyCode::Enter)).unwrap();
        // Two ctrl and two alt keys flank the space bar.
        assert_eq!(osk.count_of(OskKey::Ctrl), 2, "ctrl on both sides of space");
        assert_eq!(osk.count_of(OskKey::Alt), 2, "alt on both sides of space");
        // They all share the bottom row.
        for (_, k) in [
            (sym, OskKey::Symbols),
            (space, OskKey::Char(' ')),
            (enter, OskKey::Code(KeyCode::Enter)),
        ] {
            assert_eq!(osk.rect_for(k).unwrap().y, sym.y);
        }
        // Order across the row: ?123 < (left ctrl/alt) < space < (right
        // alt/ctrl) < enter.
        assert!(
            sym.x < space.x && space.x < enter.x,
            "?123 .. space .. enter"
        );
        // The two ctrls straddle the space bar.
        let ctrl_xs: Vec<u16> = osk
            .keys
            .iter()
            .filter(|(_, k)| *k == OskKey::Ctrl)
            .map(|(r, _)| r.x)
            .collect();
        assert!(
            ctrl_xs.iter().any(|&x| x < space.x) && ctrl_xs.iter().any(|&x| x > space.x),
            "a ctrl sits on each side of the space bar"
        );
        // The space bar is shortened: still the widest key but it shares the
        // row with six other keys, so it is far from spanning the band.
        assert!(
            (space.width as u32) < (band().width as u32) / 2,
            "the space bar is shortened to make room for the flanking modifiers"
        );
    }

    #[test]
    fn comma_sits_right_of_the_layer_toggle_and_period_left_of_enter() {
        // The bottom row reads `?123 , ctrl alt space alt ctrl . ⏎` on every
        // layer, so each thumb has punctuation without crossing the board:
        // comma immediately right of the layer toggle, period immediately
        // left of Enter. Verified on both the letter page and the ?123 page.
        for layer_tap in [false, true] {
            let mut osk = Osk::new();
            if layer_tap {
                osk.tap(OskKey::Symbols);
                assert_eq!(osk.layer, OskLayer::Symbols);
            }
            osk.layout(band());
            let toggle = osk.rect_for(OskKey::Symbols).unwrap();
            let comma = osk.rect_for(OskKey::Char(',')).unwrap();
            let period = osk.rect_for(OskKey::Char('.')).unwrap();
            let enter = osk.rect_for(OskKey::Code(KeyCode::Enter)).unwrap();
            let space = osk.rect_for(OskKey::Char(' ')).unwrap();
            // All on the bottom row.
            assert_eq!(comma.y, space.y, "comma shares the bottom row");
            assert_eq!(period.y, space.y, "period shares the bottom row");
            // Comma is wedged between the layer toggle and the space bar,
            // right of the toggle; period between the space bar and Enter,
            // left of Enter. No other key sits between toggle and comma.
            assert!(
                comma.x > toggle.x,
                "comma sits right of the ?123/abc toggle"
            );
            assert!(comma.x < space.x, "comma stays left of the space bar");
            assert!(period.x > space.x, "period stays right of the space bar");
            assert!(period.x < enter.x, "period sits left of Enter");
            let between_toggle_and_comma = osk
                .keys
                .iter()
                .any(|(r, _)| r.y == comma.y && r.x > toggle.x && r.x < comma.x);
            assert!(
                !between_toggle_and_comma,
                "comma is immediately right of the layer toggle"
            );
            let between_period_and_enter = osk
                .keys
                .iter()
                .any(|(r, _)| r.y == period.y && r.x > period.x && r.x < enter.x);
            assert!(
                !between_period_and_enter,
                "period is immediately left of Enter"
            );
        }
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
        assert!(!osk.caps, "a single shift tap never engages caps lock");
    }

    #[test]
    fn double_shift_tap_engages_caps_lock_gboard_style() {
        let mut osk = Osk::new();
        // First tap: one-shot uppercase. Second tap before typing: caps lock.
        assert!(osk.tap(OskKey::Shift).is_none());
        assert_eq!(osk.layer, OskLayer::Upper);
        assert!(osk.tap(OskKey::Shift).is_none());
        assert!(osk.caps, "a second shift tap engages caps lock");
        // The base layer is Lower; caps drives letter case so digits/symbols
        // are unaffected.
        assert_eq!(osk.layer, OskLayer::Lower);
        // A third shift tap releases the lock.
        assert!(osk.tap(OskKey::Shift).is_none());
        assert!(!osk.caps, "a third shift tap releases caps lock");
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

        // Caps lock (double-tap shift, letters-only) leaves Tab a forward-tab.
        osk.tap(OskKey::Shift);
        osk.tap(OskKey::Shift);
        assert!(osk.caps);
        let ev = osk.tap(OskKey::Code(KeyCode::Tab)).unwrap();
        assert_eq!(ev.code, KeyCode::Tab);
    }

    #[test]
    fn caps_lock_uppercases_letters_only_until_released() {
        let mut osk = Osk::new();
        // Double-tap shift engages caps lock, Gboard-style.
        osk.tap(OskKey::Shift);
        osk.tap(OskKey::Shift);
        assert!(osk.caps);
        osk.layout(band());
        // Letters are uppercase under caps lock.
        for c in ['A', 'Q', 'Z', 'M'] {
            assert!(
                osk.rect_for(OskKey::Char(c)).is_some(),
                "missing key {c:?} under caps lock"
            );
        }
        assert!(
            osk.rect_for(OskKey::Char('!')).is_none(),
            "caps lock must not expose the symbols layer"
        );
        // Typing does NOT release the lock (unlike one-shot Shift).
        let ev = osk.tap(OskKey::Char('A')).unwrap();
        assert_eq!(ev.code, KeyCode::Char('A'));
        assert!(osk.caps, "caps lock persists across keystrokes");
        // A Symbols round-trip lands back on uppercase.
        osk.tap(OskKey::Symbols);
        osk.tap(OskKey::Symbols);
        assert!(osk.caps);
        // A single shift tap releases the lock.
        assert!(osk.tap(OskKey::Shift).is_none());
        assert!(!osk.caps);
        assert_eq!(osk.layer, OskLayer::Lower);
    }

    #[test]
    fn structural_keys_stop_stretching_on_wide_bands() {
        let mut osk = Osk::new();
        osk.layout(wide());
        // Structural keys pin at their caps; letters absorb the slack and end
        // up wider than esc, a capped top-strip key.
        assert_eq!(osk.rect_for(OskKey::Code(KeyCode::Esc)).unwrap().width, 6);
        assert_eq!(osk.rect_for(OskKey::Code(KeyCode::Tab)).unwrap().width, 7);
        assert_eq!(osk.rect_for(OskKey::Shift).unwrap().width, 9);
        assert_eq!(
            osk.rect_for(OskKey::Code(KeyCode::Backspace))
                .unwrap()
                .width,
            9
        );
        let q = osk.rect_for(OskKey::Char('q')).unwrap();
        let esc = osk.rect_for(OskKey::Code(KeyCode::Esc)).unwrap();
        assert!(
            q.width > esc.width,
            "letters ({}) must out-grow structural keys ({}) on wide bands",
            q.width,
            esc.width
        );
    }

    #[test]
    fn enter_is_a_single_height_key_on_the_bottom_row() {
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
        let a = osk.rect_for(OskKey::Char('a')).unwrap();
        // Gboard's Enter is a plain single-row key, same height as a letter.
        assert_eq!(enter.height, row_h, "Enter is one key-row tall");
        assert_eq!(enter.height, a.height, "Enter matches an ordinary key");
        // It owns the right edge of the bottom row.
        let space = osk.rect_for(OskKey::Char(' ')).unwrap();
        assert!(
            enter.x > space.x,
            "Enter sits at the bottom row's right end"
        );
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
        // Every x where the char `c` is placed (it may appear twice when the
        // seam letter is duplicated across the two halves).
        let xs_of = |c: char| -> Vec<u16> {
            osk.keys
                .iter()
                .filter(|(_, k)| *k == OskKey::Char(c))
                .map(|(r, _)| r.x)
                .collect()
        };
        // The top row splits cleanly with no duplication: t ends the left
        // half, y starts the right.
        let t = osk.rect_for(OskKey::Char('t')).unwrap();
        let y = osk.rect_for(OskKey::Char('y')).unwrap();
        assert_eq!(xs_of('t').len(), 1, "`t` is not duplicated");
        assert_eq!(xs_of('y').len(), 1, "`y` is not duplicated");
        assert!(t.x + t.width <= center, "`t` ends the left top cluster");
        assert!(y.x >= center, "`y` starts the right top cluster");
        assert!(
            y.x - (t.x + t.width) >= area.width * SPLIT_GAP_NUM / SPLIT_GAP_DEN,
            "the center channel is at least two-ninths of the band"
        );
        // Both halves carry a space bar.
        assert_eq!(osk.count_of(OskKey::Char(' ')), 2);
        // The edge arrows land in opposite clusters: ←/→ left, ↑/↓ right.
        let lr = osk.rect_for(OskKey::ArrowLR).unwrap();
        let ud = osk.rect_for(OskKey::ArrowUD).unwrap();
        assert!(lr.x + lr.width <= center, "←/→ stays in the left cluster");
        assert!(ud.x >= center, "↑/↓ stays in the right cluster");
    }

    #[test]
    fn split_duplicates_the_seam_letters_g_and_v_on_both_halves() {
        let mut osk = Osk::new();
        osk.split = true;
        osk.layout(wide());
        let area = wide();
        let center = area.x + area.width / 2;
        // `g` and `v` each appear exactly twice: once at the inner edge of the
        // left half, once at the inner edge of the right half.
        let xs_of = |c: char| -> Vec<u16> {
            let mut xs: Vec<u16> = osk
                .keys
                .iter()
                .filter(|(_, k)| *k == OskKey::Char(c))
                .map(|(r, _)| r.x)
                .collect();
            xs.sort_unstable();
            xs
        };
        for c in ['g', 'v'] {
            let xs = xs_of(c);
            assert_eq!(xs.len(), 2, "`{c}` must be duplicated on both halves");
            assert!(xs[0] < center, "the left copy of `{c}` is in the left half");
            assert!(
                xs[1] >= center,
                "the right copy of `{c}` is in the right half"
            );
        }
        // The seam columns line up T/G/V on the left and Y/G/V on the right:
        // the rightmost key of each left row and the leftmost of each right
        // row, top→home→shift.
        let right_edge = |c: char| osk.rect_for(OskKey::Char(c)).map(|r| r.x + r.width);
        // Left half inner edges (the larger x where the duplicated letter sits
        // is the left half's rightmost copy for g/v).
        let left_inner = |c: char| xs_of(c)[0]; // single or left copy
        // Top: `t` (left) and `y` (right) frame the seam.
        let t_end = right_edge('t').unwrap();
        let y_start = osk.rect_for(OskKey::Char('y')).unwrap().x;
        // Home: left `g` ends the left cluster, right `g` starts the right.
        let g_left_x = left_inner('g');
        let g_right_x = xs_of('g')[1];
        // Shift: left `v` ends the left cluster, right `v` starts the right.
        let v_left_x = left_inner('v');
        let v_right_x = xs_of('v')[1];
        // All three left-edge keys sit left of center; all three right-edge
        // keys sit at/after center — i.e. T/G/V | Y/G/V across the seam.
        assert!(
            t_end <= center && g_left_x < center && v_left_x < center,
            "left inner edge reads T/G/V"
        );
        assert!(
            y_start >= center && g_right_x >= center && v_right_x >= center,
            "right inner edge reads Y/G/V"
        );
        // Tapping either copy of g/v types that letter (they are plain Chars).
        assert_eq!(osk.tap(OskKey::Char('g')).unwrap().code, KeyCode::Char('g'));
        assert_eq!(osk.tap(OskKey::Char('v')).unwrap().code, KeyCode::Char('v'));
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
        // Alt + the edge arrow emits Left with the Alt modifier (readline
        // backward-word), proving the latch reaches named-key paths.
        let ev = osk.tap(OskKey::ArrowLR).unwrap();
        assert_eq!(ev.code, KeyCode::Left);
        assert!(ev.modifiers.contains(KeyModifiers::ALT));
        assert!(!osk.alt);
    }

    #[test]
    fn symbols_layer_toggles_and_carries_punctuation() {
        let mut osk = Osk::new();
        assert!(osk.tap(OskKey::Symbols).is_none());
        assert_eq!(osk.layer, OskLayer::Symbols);
        osk.layout(band());
        // The symbols page carries digits (the number row lives here, like
        // Gboard's `?123` page) plus the common programming punctuation, and
        // the comma/period by the space bar.
        for c in [
            '1', '0', '@', '#', '$', '_', '&', '-', '+', '(', ')', '/', '\\', ',', '.',
        ] {
            assert!(
                osk.rect_for(OskKey::Char(c)).is_some(),
                "missing key {c:?} on symbols layer"
            );
        }
        // Typing a symbol keeps the layer (unlike one-shot Shift).
        osk.tap(OskKey::Char('@'));
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
