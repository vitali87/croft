//! Single-line text editing around a caret: the pure functions every text
//! input shares (#1729).
//!
//! An input row in croft is a `String` plus a caret, and until now each one
//! edited by pushing to or popping from the end. These functions give them all
//! the same caret model without a new widget type, so a field keeps the
//! `String` the rest of the code already reads and adds one `usize`:
//!
//! - The caret is a **char index** (`0..=text.chars().count()`), the unit the
//!   commit box's `message_cursor` and the find bar already use. Every function
//!   clamps what it is given, so a caret left stale by a wholesale text
//!   replacement degrades to "at the end" instead of panicking.
//! - A single step (Left, Right, Backspace, Delete) moves by an extended
//!   grapheme cluster, so `é` written as two code points, an emoji with a
//!   skin-tone modifier or a Hangul syllable is one step. A word step walks
//!   word and non-word runs ([`crate::widgets::editor::is_word_char`]) and
//!   lands on a cluster boundary too.
//! - Editing functions return the new text and the new caret. They never
//!   touch a selection: a field with one deletes it first and edits at the
//!   point it left.
//! - [`window`] places the row's text on screen by display cells, scrolling a
//!   long text so the caret stays in view.

use unicode_segmentation::UnicodeSegmentation;

/// Number of chars in `text`: the unit of a caret.
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// `caret` limited to the text.
pub fn clamp(text: &str, caret: usize) -> usize {
    caret.min(char_len(text))
}

/// Byte offset of char index `at` (the text's length for an index past it).
pub fn byte_of(text: &str, at: usize) -> usize {
    text.char_indices().nth(at).map_or(text.len(), |(i, _)| i)
}

/// Char index of byte offset `byte`, the inverse of [`byte_of`].
pub fn char_of(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// Start (as a char index) of the grapheme cluster that ends at `at`: one
/// step left. `0` stays `0`.
fn cluster_before(text: &str, at: usize) -> usize {
    if text.is_ascii() {
        return at.saturating_sub(1);
    }
    let mut start = 0;
    let mut prev = 0;
    for g in text.graphemes(true) {
        if start >= at {
            break;
        }
        prev = start;
        start += g.chars().count();
    }
    prev
}

/// End (as a char index) of the grapheme cluster that starts at `at`: one
/// step right, saturating at the end of the text.
fn cluster_after(text: &str, at: usize) -> usize {
    if text.is_ascii() {
        return (at + 1).min(text.len());
    }
    let mut end = 0;
    for g in text.graphemes(true) {
        end += g.chars().count();
        if end > at {
            return end;
        }
    }
    end
}

/// `at` moved back to the start of the cluster it falls inside.
fn cluster_floor(text: &str, at: usize) -> usize {
    if text.is_ascii() {
        return at.min(text.len());
    }
    let mut start = 0;
    for g in text.graphemes(true) {
        let len = g.chars().count();
        if start + len > at {
            return start;
        }
        start += len;
    }
    start
}

fn is_word(c: char) -> bool {
    crate::widgets::editor::is_word_char(c)
}

/// Where a word walk ends when moving left from `at`: back over a run of
/// non-word characters first, then over the word, as a text field walks.
fn word_start_before(text: &str, at: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut i = at.min(chars.len());
    while i > 0 && !is_word(chars[i - 1]) {
        i -= 1;
    }
    while i > 0 && is_word(chars[i - 1]) {
        i -= 1;
    }
    cluster_floor(text, i)
}

/// Where a word walk ends when moving right from `at`.
fn word_end_after(text: &str, at: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut i = at.min(chars.len());
    while i < chars.len() && !is_word(chars[i]) {
        i += 1;
    }
    while i < chars.len() && is_word(chars[i]) {
        i += 1;
    }
    cluster_floor(text, i).max(at.min(chars.len()))
}

/// `caret` one step left in `text`, by a word when `word`.
pub fn caret_left(text: &str, caret: usize, word: bool) -> usize {
    let at = clamp(text, caret);
    if word {
        word_start_before(text, at)
    } else {
        cluster_before(text, at)
    }
}

/// `caret` one step right in `text`, by a word when `word`.
pub fn caret_right(text: &str, caret: usize, word: bool) -> usize {
    let at = clamp(text, caret);
    if word {
        word_end_after(text, at)
    } else {
        cluster_after(text, at)
    }
}

/// `text` with `add` inserted at `caret`, and the caret after it. Control
/// characters are dropped: an input row is one line, so a pasted newline or
/// tab must not enter it.
pub fn insert_at(text: &str, caret: usize, add: &str) -> (String, usize) {
    let at = clamp(text, caret);
    let added: String = add.chars().filter(|c| !c.is_control()).collect();
    if added.is_empty() {
        return (text.to_string(), at);
    }
    let b = byte_of(text, at);
    let mut out = String::with_capacity(text.len() + added.len());
    out.push_str(&text[..b]);
    out.push_str(&added);
    out.push_str(&text[b..]);
    (out, at + char_len(&added))
}

/// `text` without the cluster (or, with `word`, the run) before `caret`, and
/// the caret where the cut began.
pub fn delete_before(text: &str, caret: usize, word: bool) -> (String, usize) {
    let at = clamp(text, caret);
    let from = caret_left(text, at, word);
    if from == at {
        return (text.to_string(), at);
    }
    (cut(text, from, at), from)
}

/// `text` without the cluster (or, with `word`, the run) at `caret` (Delete),
/// the caret unmoved.
pub fn delete_at(text: &str, caret: usize, word: bool) -> (String, usize) {
    let at = clamp(text, caret);
    let to = caret_right(text, at, word);
    if to == at {
        return (text.to_string(), at);
    }
    (cut(text, at, to), at)
}

/// `text` without the chars `from..to`.
fn cut(text: &str, from: usize, to: usize) -> String {
    let (a, b) = (byte_of(text, from), byte_of(text, to));
    let mut out = String::with_capacity(text.len() - (b - a));
    out.push_str(&text[..a]);
    out.push_str(&text[b..]);
    out
}

/// Display cells of one char, 0 for a control character or a combining mark.
pub fn cells(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Which part of a row's text is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// First visible char (an index into the text).
    pub start: usize,
    /// One past the last visible char.
    pub end: usize,
    /// Cell the caret is drawn on, counted from the row's left edge. Always
    /// less than the row's width, so a caret after the last char still has a
    /// cell of its own.
    pub caret_cell: usize,
}

/// The slice of `text` to draw in `avail` cells so that `caret` is in view:
/// leading chars scroll off just far enough to give the caret a cell, and the
/// rest fills what is left. A wide character that would straddle the right
/// edge is left out, as it is in the editor.
pub fn window(text: &str, caret: usize, avail: usize) -> Window {
    let chars: Vec<char> = text.chars().collect();
    let caret = caret.min(chars.len());
    let avail = avail.max(1);
    let before_w: usize = chars[..caret].iter().map(|&c| cells(c)).sum();
    let mut start = 0;
    let mut dropped = 0;
    while start < caret && before_w - dropped >= avail {
        dropped += cells(chars[start]);
        start += 1;
    }
    let caret_cell = before_w - dropped;
    // The char under the caret takes its own cell (an end-of-text caret gets
    // one too), then whatever of the rest still fits.
    let mut used = caret_cell;
    let mut end = caret;
    if end < chars.len() {
        let w = cells(chars[end]).max(1);
        if used + w <= avail {
            used += w;
            end += 1;
            while end < chars.len() {
                let w = cells(chars[end]);
                if used + w > avail {
                    break;
                }
                used += w;
                end += 1;
            }
        }
    }
    Window {
        start,
        end,
        caret_cell,
    }
}

/// The char of `text` under screen cell `cell`, counting from the row's left
/// edge when the row shows the text from char `start`: a click's caret. A
/// cell past the visible text is the end of the text; one on the second half
/// of a wide character is that character.
pub fn char_at_cell(text: &str, start: usize, cell: usize) -> usize {
    let mut at = 0;
    for (i, c) in text.chars().enumerate().skip(start) {
        let w = cells(c);
        if cell < at + w {
            return i;
        }
        at += w;
        if cell < at {
            return i;
        }
    }
    char_len(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_cell_maps_to_the_char_under_it() {
        assert_eq!(char_at_cell("abc", 0, 0), 0);
        assert_eq!(char_at_cell("abc", 0, 2), 2);
        assert_eq!(char_at_cell("abc", 0, 9), 3, "past the end");
        assert_eq!(char_at_cell("abcdef", 2, 1), 3, "a scrolled row");
        // 日 is cells 0-1, 本 is 2-3.
        assert_eq!(char_at_cell("日本x", 0, 1), 0);
        assert_eq!(char_at_cell("日本x", 0, 2), 1);
        assert_eq!(char_at_cell("日本x", 0, 4), 2);
    }

    #[test]
    fn steps_move_by_cluster_not_by_code_point() {
        // `e` + combining acute is two chars, one cluster.
        let t = "ae\u{301}b";
        assert_eq!(caret_right(t, 1, false), 3, "over e + accent in one step");
        assert_eq!(caret_left(t, 3, false), 1);
        // A flag is two regional indicators, one cluster.
        let f = "x🇪🇸y";
        assert_eq!(caret_right(f, 1, false), 3);
        assert_eq!(caret_left(f, 3, false), 1);
        // Ends saturate.
        assert_eq!(caret_left("ab", 0, false), 0);
        assert_eq!(caret_right("ab", 2, false), 2);
        assert_eq!(caret_right("ab", 99, false), 2, "a stale caret clamps");
    }

    #[test]
    fn word_steps_skip_a_run_then_a_word() {
        let t = "foo  bar_baz, qux";
        assert_eq!(caret_left(t, 17, true), 14, "back over qux");
        assert_eq!(caret_left(t, 14, true), 5, "back over `, ` and bar_baz");
        assert_eq!(caret_left(t, 5, true), 0);
        assert_eq!(caret_right(t, 0, true), 3);
        assert_eq!(caret_right(t, 3, true), 12, "over the gap and bar_baz");
        assert_eq!(caret_right(t, 17, true), 17);
        // Cyrillic is word text.
        assert_eq!(caret_left("привет мир", 10, true), 7);
    }

    #[test]
    fn insert_lands_at_the_caret_and_drops_control_chars() {
        assert_eq!(insert_at("ac", 1, "b"), ("abc".to_string(), 2));
        assert_eq!(insert_at("ac", 9, "b"), ("acb".to_string(), 3));
        assert_eq!(insert_at("ac", 0, "x\ny\tz"), ("xyzac".to_string(), 3));
        assert_eq!(insert_at("ac", 1, "\n"), ("ac".to_string(), 1));
        assert_eq!(insert_at("日本", 1, "語"), ("日語本".to_string(), 2));
    }

    #[test]
    fn deletes_cut_a_cluster_or_a_word_and_keep_the_caret_honest() {
        assert_eq!(delete_before("abc", 2, false), ("ac".to_string(), 1));
        assert_eq!(delete_before("abc", 0, false), ("abc".to_string(), 0));
        assert_eq!(delete_at("abc", 1, false), ("ac".to_string(), 1));
        assert_eq!(delete_at("abc", 3, false), ("abc".to_string(), 3));
        // The whole `é` goes with one Backspace.
        assert_eq!(delete_before("ae\u{301}", 3, false), ("a".to_string(), 1));
        assert_eq!(delete_before("foo bar", 7, true), ("foo ".to_string(), 4));
        assert_eq!(delete_at("foo bar", 0, true), (" bar".to_string(), 0));
    }

    #[test]
    fn window_keeps_the_caret_in_view_and_inside_the_row() {
        // Fits: nothing scrolls, the caret sits after the text.
        let w = window("abc", 3, 10);
        assert_eq!((w.start, w.end, w.caret_cell), (0, 3, 3));
        // Caret mid-text of a fitting text.
        let w = window("abcdef", 2, 10);
        assert_eq!((w.start, w.end, w.caret_cell), (0, 6, 2));
        // Too long, caret at the end: leading chars scroll away, and the
        // caret's cell is the last one.
        let w = window("abcdefghij", 10, 5);
        assert_eq!((w.start, w.end, w.caret_cell), (6, 10, 4));
        // Caret at the start of a long text: the head shows.
        let w = window("abcdefghij", 0, 5);
        assert_eq!((w.start, w.end, w.caret_cell), (0, 5, 0));
        // Never a caret outside the row.
        for caret in 0..=10 {
            let w = window("abcdefghij", caret, 5);
            assert!(w.caret_cell < 5, "caret {caret}: {w:?}");
            assert!(w.start <= caret && caret <= w.end.max(caret));
        }
    }

    #[test]
    fn window_counts_wide_characters_in_cells() {
        // Each CJK char is two cells: five cells hold two and a half.
        let w = window("日本語", 0, 5);
        assert_eq!((w.start, w.end), (0, 2), "the third does not straddle");
        let w = window("日本語", 3, 5);
        assert!(w.caret_cell < 5);
        assert_eq!(w.start, 1, "one char scrolled off to fit the caret");
    }
}
