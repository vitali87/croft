//! WezTerm-style quick-select ("hint mode") for the terminal pane.
//!
//! `Ctrl+Shift+Space` scans the visible viewport against a fixed pattern set
//! (URLs, filesystem paths incl. `path:line:col`, git SHAs, UUIDs, IPs, hex
//! colours/addresses, long numbers), overlays a short home-row label on every
//! match, and lets a typed label copy the matched text to the clipboard —
//! an UPPERCASE label also pastes it into the shell. The pattern list, the
//! label alphabet and the overflow scheme follow WezTerm's quick-select
//! (which tmux-thumbs shares), the only mature references for the feature.
//!
//! This module is the pure core: pattern matching over row text and label
//! assignment. The interaction (key handling, overlay painting) lives in the
//! app and the terminal widget.

use regex::Regex;
use std::sync::LazyLock;

/// WezTerm's / tmux-thumbs' default label alphabet: easiest-to-reach QWERTY
/// keys first (home row, then the rows above/below), so the labels nearest
/// the prompt cost the least finger travel.
pub const ALPHABET: &str = "asdfqwerzxcvjklmiuopghtybn";

/// One pattern-matched span on a viewport row. `start`/`len` are char
/// indices into the spacer-skipped row text (the same coordinate space the
/// find highlight uses; the render colmap translates back to grid columns).
/// `text` is what a committed label copies — the capture group for patterns
/// that select a sub-span (markdown links, diff paths, sha256 digests),
/// otherwise the whole match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickMatch {
    pub row: usize,
    pub start: usize,
    pub len: usize,
    pub text: String,
    /// Continuation segments `(row, start, len)` when the match wraps the
    /// pane edge: the head span above anchors the label, these carry the
    /// rest of the highlight one display row at a time. Empty for the
    /// common single-row match.
    pub tail: Vec<(usize, usize, usize)>,
}

/// The pattern set, most-specific first: a row span claimed by an earlier
/// pattern is dead to later ones, so a URL never gets shredded into a path
/// plus a number. WezTerm's shipped defaults plus the gaps it lacks (email,
/// scp-style git remotes, `path:line:col` — high value here given compiler /
/// test / LSP output in the pane).
static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Markdown link: select the URL inside the parens.
        r"\[[^\]]*\]\(([^)]+)\)",
        // Scheme URL. Trailing punctuation that commonly follows a URL in
        // prose/logs is excluded so `https://x.dev/y.` copies cleanly.
        r#"(?:https?|ftp|file|ssh|git)://[^\s'"<>\])]+[^\s'"<>\]).,;:]"#,
        // scp-style git remote (git@host:owner/repo.git).
        r"git@[\w.-]+:[\w./~-]+",
        // Email.
        r"\b[\w.+-]+@[\w-]+\.[\w.-]*\w",
        // Unified-diff paths: select the path after the a/ b/ prefix.
        r"--- a/(\S+)",
        r"\+\+\+ b/(\S+)",
        // Docker/OCI digest: select the hex.
        r"sha256:([0-9a-f]{64})",
        // Filesystem path (absolute, ~, or relative with ./ ../), with an
        // optional :line(:col) suffix from compiler / grep / test output.
        r"(?:[.\w\-@~]+)?(?:/+[.\w\-@]+)+(?::\d+(?::\d+)?)?",
        // UUID.
        r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
        // IPFS CIDv0.
        r"\bQm[1-9A-HJ-NP-Za-km-z]{44}\b",
        // git SHA / long hex run.
        r"\b[0-9a-f]{7,40}\b",
        // IPv4.
        r"\b\d{1,3}(?:\.\d{1,3}){3}\b",
        // IPv6 (loose, like every terminal ships it).
        r"\b(?:[A-Fa-f0-9]{1,4}:){2,7}[A-Fa-f0-9]{1,4}\b",
        // Hex colour.
        r"#[0-9a-fA-F]{6}\b",
        // Hex address / literal.
        r"\b0x[0-9a-fA-F]+\b",
        // Bare number, 4+ digits — lowest priority so it never cannibalises
        // an id matched above.
        r"\b[0-9]{4,}\b",
    ]
    .iter()
    .map(|src| Regex::new(src).expect("quick-select pattern"))
    .collect()
});

/// Scan viewport rows for pattern matches. Rows are the spacer-skipped text
/// of each visible line, top to bottom; `wraps[i]` says row `i` soft-wraps
/// into row `i + 1` (alacritty's `WRAPLINE`), and each such run is stitched
/// into one LOGICAL line before scanning — a URL crossing the pane edge is
/// one match, not an invisible head plus a path-fragment tail (#64). Match
/// coordinates come back in display-row space: the head span (where the
/// label sits) plus `tail` segments for continuation rows. Matches are
/// sorted by (row, start); overlapping spans keep the most-specific
/// (earliest) pattern.
pub fn find_matches_wrapped(lines: &[String], wraps: &[bool]) -> Vec<QuickMatch> {
    let mut out = Vec::new();
    let mut row = 0usize;
    while row < lines.len() {
        // The logical line: this row plus every row soft-wrapped onto it. A
        // final row wrapping off the bottom of the viewport has no
        // continuation to stitch; its visible fragment scans as-is (labels
        // the user cannot see cannot be typed).
        let mut end_row = row;
        while wraps.get(end_row).copied().unwrap_or(false) && end_row + 1 < lines.len() {
            end_row += 1;
        }
        let mut text = String::new();
        // (row, char offset of the row within the logical text, row char len)
        let mut row_spans: Vec<(usize, usize, usize)> = Vec::new();
        for (r, line) in lines.iter().enumerate().take(end_row + 1).skip(row) {
            row_spans.push((r, text.chars().count(), line.chars().count()));
            text.push_str(line);
        }
        // Byte spans already claimed on this logical line, by pattern
        // priority — a URL spanning rows claims its whole span, so later
        // patterns cannot shred the continuation into fragments.
        let mut taken: Vec<(usize, usize)> = Vec::new();
        for re in PATTERNS.iter() {
            for caps in re.captures_iter(&text) {
                let whole = caps.get(0).expect("group 0 always present");
                if taken
                    .iter()
                    .any(|&(s, e)| whole.start() < e && s < whole.end())
                {
                    continue;
                }
                taken.push((whole.start(), whole.end()));
                // The selected text is capture group 1 when the pattern has
                // one (markdown URL, diff path, sha256 digest), else the
                // whole match; the label sits at the selected span's start.
                let sel = caps.get(1).unwrap_or(whole);
                let start = text[..sel.start()].chars().count();
                let len = sel.as_str().chars().count();
                let mut segs = row_spans.iter().filter_map(|&(r, rstart, rlen)| {
                    let s = start.max(rstart);
                    let e = (start + len).min(rstart + rlen);
                    // Lazy `then`, not `then_some`: `e - s` underflows for
                    // the rows the span does not overlap.
                    (s < e).then(|| (r, s - rstart, e - s))
                });
                let Some((hrow, hstart, hlen)) = segs.next() else {
                    continue;
                };
                out.push(QuickMatch {
                    row: hrow,
                    start: hstart,
                    len: hlen,
                    text: sel.as_str().to_string(),
                    tail: segs.collect(),
                });
            }
        }
        row = end_row + 1;
    }
    out.sort_by_key(|m| (m.row, m.start));
    out
}

/// Pair matches with labels, bottom-priority: when there are more matches
/// than labels, the TOP overflow is dropped (the least useful rows) and the
/// bottom-most match — nearest the prompt, the likeliest target — always
/// keeps the cheapest label. Returns (match, label) in match order.
pub fn label_matches(matches: Vec<QuickMatch>, labels: &[String]) -> Vec<(QuickMatch, String)> {
    let n = labels.len();
    let skip = matches.len().saturating_sub(n);
    matches
        .into_iter()
        .skip(skip)
        .enumerate()
        .map(|(i, m)| {
            let label = labels[n - 1 - i].clone();
            (m, label)
        })
        .collect()
}

/// Labels for `count` matches from `alphabet`, WezTerm's overflow scheme:
/// single-char labels while they fit; otherwise the smallest tail of the
/// alphabet is reserved as two-char prefixes (`abcd` for 6 matches gives
/// `a b c da db dc`). The caller hands labels out in REVERSE match order so
/// the bottom-most (nearest the prompt) match gets the cheapest label.
pub fn assign_labels(count: usize, alphabet: &str) -> Vec<String> {
    let chars: Vec<char> = alphabet.chars().collect();
    let l = chars.len();
    if l == 0 || count == 0 {
        return Vec::new();
    }
    // Smallest tail of the alphabet reserved as two-char prefixes such that
    // the remaining singles plus the prefixed pairs cover `count`.
    let mut prefixes = 0usize;
    while prefixes < l && (l - prefixes) + prefixes * l < count {
        prefixes += 1;
    }
    let mut labels: Vec<String> = chars[..l - prefixes]
        .iter()
        .map(|c| c.to_string())
        .collect();
    for &p in &chars[l - prefixes..] {
        for &c in &chars {
            labels.push(format!("{p}{c}"));
        }
    }
    labels.truncate(count.min(labels.len()));
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|s| s.to_string()).collect()
    }

    /// The unwrapped case the pattern tests exercise: every row its own
    /// logical line.
    fn find_matches(lines: &[String]) -> Vec<QuickMatch> {
        find_matches_wrapped(lines, &vec![false; lines.len()])
    }

    /// More matches than labels: the BOTTOM of the screen keeps its labels
    /// (nearest the prompt, the likeliest target) and the top overflow is
    /// dropped — `take(n)` used to keep the top and leave everything near
    /// the prompt unlabelled and unreachable.
    #[test]
    fn label_overflow_drops_the_top_and_keeps_the_cheapest_label_at_the_bottom() {
        let matches: Vec<QuickMatch> = (0..20)
            .map(|r| QuickMatch {
                row: r,
                start: 0,
                len: 3,
                text: format!("m{r}"),
                tail: Vec::new(),
            })
            .collect();
        let labels = assign_labels(matches.len(), "abc");
        assert_eq!(labels.len(), 9, "3-char alphabet caps at 9 pair labels");
        let paired = label_matches(matches, &labels);
        assert_eq!(paired.len(), 9);
        assert_eq!(
            paired.first().unwrap().0.row,
            11,
            "the TOP overflow is dropped, not the bottom"
        );
        assert_eq!(
            (
                paired.last().unwrap().0.row,
                paired.last().unwrap().1.as_str()
            ),
            (19, labels[0].as_str()),
            "the bottom-most match keeps the cheapest label"
        );
    }

    /// The #64 defect: a URL wrapping the pane edge was invisible as a URL —
    /// scanning display rows independently, the head fragment matched
    /// nothing and the tail was labelled as a filesystem-path fragment, so
    /// typing the label copied `/drift.io/path` instead of what the user
    /// sees. Stitched scanning must see it whole, anchor the label on the
    /// head, and carry the continuation rows as tail segments.
    #[test]
    fn a_match_wrapping_the_pane_edge_is_seen_whole_and_anchored_on_its_head() {
        let m = find_matches_wrapped(
            &lines(&["fetching http:/", "/drift.io/path now"]),
            &[true, false],
        );
        assert_eq!(m.len(), 1, "one URL, not a path fragment: {m:?}");
        assert_eq!(m[0].text, "http://drift.io/path");
        assert_eq!(
            (m[0].row, m[0].start, m[0].len),
            (0, 9, 6),
            "the label anchors on the head fragment"
        );
        assert_eq!(
            m[0].tail,
            vec![(1, 0, 14)],
            "the continuation row rides along as a tail segment"
        );
    }

    /// A row that merely ENDS where the next one starts is not a wrap:
    /// without the flag the rows stay independent and the tail is a path of
    /// its own, exactly the pre-stitching behavior.
    #[test]
    fn an_unwrapped_row_boundary_still_scans_rows_independently() {
        let m = find_matches_wrapped(
            &lines(&["fetching http:/", "/drift.io/path now"]),
            &[false, false],
        );
        assert!(
            m.iter().any(|q| q.text == "/drift.io/path"),
            "no wrap flag, so the tail is its own path match: {m:?}"
        );
        assert!(m.iter().all(|q| q.tail.is_empty()));
    }

    /// A match spanning THREE rows (a long URL through a narrow pane)
    /// carries one tail segment per continuation row.
    #[test]
    fn a_match_spanning_three_wrapped_rows_carries_two_tail_segments() {
        let m = find_matches_wrapped(
            &lines(&["see https://cra", "tes.io/api/v1/c", "rates/regex ok"]),
            &[true, true, false],
        );
        let url = m
            .iter()
            .find(|q| q.text == "https://crates.io/api/v1/crates/regex")
            .expect("the stitched URL");
        assert_eq!((url.row, url.start, url.len), (0, 4, 11));
        assert_eq!(url.tail, vec![(1, 0, 15), (2, 0, 11)]);
    }

    #[test]
    fn urls_win_over_the_paths_and_numbers_inside_them() {
        let m = find_matches(&lines(&[
            "fetching https://crates.io/api/v1/crates/regex/1.12.0/download now",
        ]));
        assert_eq!(
            m.len(),
            1,
            "one URL, not a URL plus path plus number: {m:?}"
        );
        assert_eq!(
            m[0].text,
            "https://crates.io/api/v1/crates/regex/1.12.0/download"
        );
        assert_eq!(m[0].row, 0);
        assert_eq!(m[0].start, 9);
    }

    #[test]
    fn compiler_style_path_line_col_is_one_match() {
        let m = find_matches(&lines(&[
            "error[E0308]: src/app/mod.rs:16512:9 mismatched types",
        ]));
        let path = m
            .iter()
            .find(|q| q.text.contains("mod.rs"))
            .expect("path match");
        assert_eq!(path.text, "src/app/mod.rs:16512:9");
    }

    #[test]
    fn markdown_and_diff_captures_select_the_inner_group() {
        let m = find_matches(&lines(&[
            "see [the docs](https://wezterm.org/quickselect.html) for more",
            "--- a/src/main.rs",
        ]));
        assert!(
            m.iter()
                .any(|q| q.text == "https://wezterm.org/quickselect.html"),
            "{m:?}"
        );
        assert!(m.iter().any(|q| q.text == "src/main.rs"), "{m:?}");
        // The markdown label span starts at the captured URL, not at `[`.
        let url = m.iter().find(|q| q.text.starts_with("https")).unwrap();
        assert_eq!(url.start, "see [the docs](".chars().count());
    }

    #[test]
    fn shas_ips_and_hex_are_matched_and_sorted_by_position() {
        let m = find_matches(&lines(&[
            "commit de40bdf12ab34cd56ef7 on 192.168.1.10",
            "addr 0xdeadbeef color #ff8c2a port 8080",
        ]));
        let texts: Vec<&str> = m.iter().map(|q| q.text.as_str()).collect();
        assert!(texts.contains(&"de40bdf12ab34cd56ef7"), "{texts:?}");
        assert!(texts.contains(&"192.168.1.10"), "{texts:?}");
        assert!(texts.contains(&"0xdeadbeef"), "{texts:?}");
        assert!(texts.contains(&"#ff8c2a"), "{texts:?}");
        assert!(texts.contains(&"8080"), "{texts:?}");
        let sorted = m
            .windows(2)
            .all(|w| (w[0].row, w[0].start) <= (w[1].row, w[1].start));
        assert!(sorted, "matches must be sorted by (row, start): {m:?}");
    }

    #[test]
    fn labels_stay_single_char_until_the_alphabet_runs_out() {
        assert_eq!(assign_labels(3, "abcd"), vec!["a", "b", "c"]);
        assert_eq!(assign_labels(4, "abcd"), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn label_overflow_reserves_the_alphabet_tail_as_prefixes() {
        // WezTerm's documented example: alphabet `abcd`, 6 matches.
        assert_eq!(
            assign_labels(6, "abcd"),
            vec!["a", "b", "c", "da", "db", "dc"]
        );
    }

    #[test]
    fn empty_inputs_yield_no_labels() {
        assert!(assign_labels(0, ALPHABET).is_empty());
        assert!(assign_labels(5, "").is_empty());
        assert!(find_matches(&[]).is_empty());
    }
}
