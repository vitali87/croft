//! Where a hit's line splits so its matches are highlighted, for a plain query and for a
//! regex.

use super::*;

/// Split `line` into `(segment, is_match)` runs against `needle` honouring
/// the supplied `opts`, so highlights in result rows / the editor stay
/// consistent with what `collect_matches_in_text` would actually match.
/// Concatenating the segments reproduces the original line byte-for-byte.
/// Empty needle, no match, or invalid regex all return the whole line as
/// a single non-match segment.
pub fn split_for_highlight(line: &str, needle: &str, opts: SearchOpts) -> Vec<(String, bool)> {
    if needle.is_empty() {
        return vec![(line.to_string(), false)];
    }
    if opts.use_regex {
        return split_for_highlight_regex(line, needle, opts);
    }
    let (haystack, search_for): (String, String) = if opts.case_sensitive {
        (line.to_string(), needle.to_string())
    } else {
        (line.to_lowercase(), needle.to_lowercase())
    };
    // Lowercasing that changes any character's byte length (`ẞ`, `İ`, the
    // Kelvin sign) shifts every offset after it, even when the total length
    // happens to come out equal: those lines go through the regex path,
    // whose case folding matches on the original bytes.
    let shifts = |t: &str| {
        t.chars()
            .any(|c| c.to_lowercase().map(char::len_utf8).sum::<usize>() != c.len_utf8())
    };
    if !opts.case_sensitive && (shifts(line) || shifts(needle)) {
        let literal = SearchOpts {
            use_regex: true,
            ..opts
        };
        return split_for_highlight_regex(line, &regex::escape(needle), literal);
    }
    if search_for.is_empty() {
        return vec![(line.to_string(), false)];
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last = 0usize;
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(&search_for) {
        let abs = start + rel;
        let end = abs + search_for.len();
        if opts.whole_word && !is_whole_word_match(&haystack, abs, end) {
            start = end;
            continue;
        }
        if abs > last {
            out.push((line[last..abs].to_string(), false));
        }
        out.push((line[abs..end].to_string(), true));
        last = end;
        start = end;
    }
    if last < line.len() {
        out.push((line[last..].to_string(), false));
    }
    if out.is_empty() {
        out.push((line.to_string(), false));
    }
    out
}

pub(super) fn split_for_highlight_regex(
    line: &str,
    needle: &str,
    opts: SearchOpts,
) -> Vec<(String, bool)> {
    let Some(re) = Pattern::for_find(needle, opts) else {
        return vec![(line.to_string(), false)];
    };
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last = 0usize;
    for (start, end) in re.spans(line) {
        if start > last {
            out.push((line[last..start].to_string(), false));
        }
        // Zero-width matches (`\b`, `^`) still count as match positions —
        // the replace preview inserts the expansion there, mirroring what
        // `replace_in_text` writes — and `last` must advance regardless, or
        // the text before the NEXT match re-emits and the row doubles
        // ("foofoo" for `\b` on "foo"; #124 review).
        if end == start {
            out.push((String::new(), true));
            last = end;
            continue;
        }
        out.push((line[start..end].to_string(), true));
        last = end;
    }
    if last < line.len() {
        out.push((line[last..].to_string(), false));
    }
    if out.is_empty() {
        out.push((line.to_string(), false));
    }
    out
}
