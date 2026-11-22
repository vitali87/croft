//! The tree filter: VS Code's fuzzy match of what was typed, and a row's name with the
//! matched letters bold.

use super::*;

/// Case-insensitive subsequence (fuzzy) match of `query` against `haystack`,
/// mirroring the VS Code tree filter, which accepts a hit when the query's
/// characters appear in order (not necessarily contiguously). Returns the byte
/// offsets in `haystack` of the matched characters, in order, so the renderer
/// can bold the matched run; `None` when the query is not a subsequence.
///
/// An empty query matches everything with no highlighted offsets.
pub fn subsequence_match(haystack: &str, query: &str) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut needles = query.chars().flat_map(char::to_lowercase).peekable();
    let mut offsets = Vec::new();
    for (byte_idx, hc) in haystack.char_indices() {
        let Some(&want) = needles.peek() else { break };
        if hc.to_lowercase().eq([want]) {
            offsets.push(byte_idx);
            needles.next();
        }
    }
    needles.peek().is_none().then_some(offsets)
}

/// Foreground for the matched characters of a filtered row name: VS Code's
/// `list.filterMatchForeground` amber, which reads as a clear "this is why this
/// row survived the filter" cue on both the dark and black themes.
pub(super) const FILTER_MATCH_FG: Color = Color::Rgb(0xe5, 0xa3, 0x4b);

/// Push the file name onto `spans`, bolding the characters that the active
/// filter `query` matched (per [`subsequence_match`]). With an empty query the
/// whole name is pushed as a single span in `base` style, so the unfiltered
/// path is unchanged.
pub(super) fn push_name_spans(
    spans: &mut Vec<Span<'static>>,
    name: &str,
    query: &str,
    base: Style,
    theme: crate::theme::Theme,
) {
    let Some(offsets) = (!query.is_empty())
        .then(|| subsequence_match(name, query))
        .flatten()
    else {
        spans.push(Span::styled(name.to_string(), base));
        return;
    };
    let match_style = base
        .fg(theme.ui(FILTER_MATCH_FG))
        .add_modifier(Modifier::BOLD);
    let mut cursor = 0usize;
    for &off in &offsets {
        if off > cursor {
            spans.push(Span::styled(name[cursor..off].to_string(), base));
        }
        // The matched character runs from its byte offset to the next char
        // boundary; index by the char's own UTF-8 length.
        let ch_len = name[off..].chars().next().map_or(0, char::len_utf8);
        let next = off + ch_len;
        spans.push(Span::styled(name[off..next].to_string(), match_style));
        cursor = next;
    }
    if cursor < name.len() {
        spans.push(Span::styled(name[cursor..].to_string(), base));
    }
}
