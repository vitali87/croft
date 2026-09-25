//! Search Editor (#615): workspace search results as a document, in VS
//! Code's `.code-search` format. A header names the search; the body lists
//! each file's matches with context lines:
//!
//! ```text
//! # Query: needle
//! # Flags: CaseSensitive WordMatch RegExp
//! # Including: *.rs
//! # Excluding: target
//! # ContextLines: 1
//!
//! 2 results - 1 file
//!
//! src/a.rs:
//!    9    before
//!   10:   let needle = 1;
//!   11    after
//! ```
//!
//! Editing the header and re-running rewrites the body; Enter on a result
//! line opens that line. Pure: the caller runs the search and reads files.

use crate::widgets::search::{SearchHit, SearchOpts};
use std::path::{Path, PathBuf};

/// A search as the header states it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub opts: SearchOpts,
    pub include: String,
    pub exclude: String,
    /// Lines of context around each match.
    pub context: usize,
}

/// Read the header of a search editor document. Lines that are not header
/// lines end it; unknown flags are ignored.
pub fn parse_header(doc: &str) -> Query {
    let mut q = Query::default();
    for line in doc.lines() {
        let Some(rest) = line.strip_prefix("# ") else {
            break;
        };
        if let Some(v) = rest.strip_prefix("Query: ") {
            q.text = v.to_string();
        } else if let Some(v) = rest.strip_prefix("Flags: ") {
            for flag in v.split_whitespace() {
                match flag {
                    "CaseSensitive" => q.opts.case_sensitive = true,
                    "WordMatch" => q.opts.whole_word = true,
                    "RegExp" => q.opts.use_regex = true,
                    _ => {}
                }
            }
        } else if let Some(v) = rest.strip_prefix("Including: ") {
            q.include = v.trim().to_string();
        } else if let Some(v) = rest.strip_prefix("Excluding: ") {
            q.exclude = v.trim().to_string();
        } else if let Some(v) = rest.strip_prefix("ContextLines: ") {
            q.context = v.trim().parse().unwrap_or(0);
        }
    }
    q
}

/// The header lines for `q`.
pub fn header(q: &Query) -> String {
    let mut out = format!("# Query: {}\n", q.text);
    let flags: Vec<&str> = [
        (q.opts.case_sensitive, "CaseSensitive"),
        (q.opts.whole_word, "WordMatch"),
        (q.opts.use_regex, "RegExp"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if !flags.is_empty() {
        out.push_str(&format!("# Flags: {}\n", flags.join(" ")));
    }
    if !q.include.is_empty() {
        out.push_str(&format!("# Including: {}\n", q.include));
    }
    if !q.exclude.is_empty() {
        out.push_str(&format!("# Excluding: {}\n", q.exclude));
    }
    if q.context > 0 {
        out.push_str(&format!("# ContextLines: {}\n", q.context));
    }
    out
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// The whole document: header, summary, then each file's matches (paths
/// relative to `root`, files in path order, lines in order) with
/// `q.context` lines around them, taken from `read(path)`. Overlapping
/// context is printed once; a gap between runs prints `  ...`.
pub fn render(
    q: &Query,
    root: &Path,
    hits: &[SearchHit],
    read: &mut dyn FnMut(&Path) -> Option<String>,
) -> String {
    let mut out = header(q);
    if hits.is_empty() {
        out.push_str("\nNo results\n");
        return out;
    }
    let mut by_file: std::collections::BTreeMap<PathBuf, Vec<&SearchHit>> =
        std::collections::BTreeMap::new();
    for h in hits {
        let rel = h.path.strip_prefix(root).unwrap_or(&h.path).to_path_buf();
        by_file.entry(rel).or_default().push(h);
    }
    out.push_str(&format!(
        "\n{} - {}\n",
        plural(hits.len(), "result"),
        plural(by_file.len(), "file")
    ));
    for (rel, mut file_hits) in by_file {
        file_hits.sort_by_key(|h| h.line_no);
        file_hits.dedup_by_key(|h| h.line_no);
        let text = read(&file_hits[0].path);
        let lines: Vec<&str> = text
            .as_deref()
            .map(|t| t.lines().collect())
            .unwrap_or_default();
        out.push_str(&format!("\n{}:\n", rel.display()));
        let matched: std::collections::BTreeSet<usize> =
            file_hits.iter().map(|h| h.line_no).collect();
        let mut last: Option<usize> = None;
        for h in &file_hits {
            let from = h.line_no.saturating_sub(q.context).max(1);
            let to = if lines.is_empty() {
                h.line_no
            } else {
                (h.line_no + q.context).min(lines.len())
            };
            for n in from..=to {
                if last.is_some_and(|l| n <= l) {
                    continue;
                }
                if last.is_some_and(|l| n > l + 1) {
                    out.push_str("  ...\n");
                }
                let body = lines
                    .get(n - 1)
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| h.line_text.clone());
                if matched.contains(&n) {
                    out.push_str(&format!("  {n}:  {body}\n"));
                } else {
                    out.push_str(&format!("  {n}   {body}\n"));
                }
                last = Some(n);
            }
        }
    }
    out
}

/// A result or context line's number: "  12:  text" or "  12   text".
fn line_number(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("  ")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let after = &rest[digits.len()..];
    (after.starts_with(":  ") || after.starts_with("   ") || after == ":" || after.is_empty())
        .then(|| digits.parse().ok())
        .flatten()
}

/// The file and 1-based line a document row points at: a match or context
/// line under a `path:` heading. `None` on the header, the summary, a
/// heading or a gap.
pub fn locate(doc: &str, row: usize) -> Option<(PathBuf, usize)> {
    let lines: Vec<&str> = doc.lines().collect();
    let n = line_number(lines.get(row)?)?;
    let heading = lines[..row].iter().rev().find(|l| !l.starts_with("  "))?;
    let path = heading.strip_suffix(':')?;
    if path.is_empty() || path.starts_with('#') {
        return None;
    }
    Some((PathBuf::from(path), n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> Query {
        Query {
            text: String::from("needle"),
            opts: SearchOpts {
                case_sensitive: true,
                whole_word: false,
                use_regex: true,
            },
            include: String::from("*.rs"),
            exclude: String::from("target"),
            context: 1,
        }
    }

    #[test]
    fn the_header_round_trips_and_names_only_the_flags_that_are_on() {
        let h = header(&q());
        assert_eq!(
            h,
            "# Query: needle\n# Flags: CaseSensitive RegExp\n# Including: *.rs\n# Excluding: target\n# ContextLines: 1\n"
        );
        assert_eq!(parse_header(&h), q());
        let plain = Query {
            text: String::from("x"),
            ..Query::default()
        };
        assert_eq!(header(&plain), "# Query: x\n");
        assert_eq!(parse_header(&header(&plain)), plain);
    }

    #[test]
    fn the_header_ends_at_the_first_other_line_and_a_query_keeps_its_spaces() {
        let doc = "# Query:   two  words \n# Flags: WordMatch Bogus\n\n# Query: not me\n";
        let parsed = parse_header(doc);
        assert_eq!(parsed.text, "  two  words ");
        assert!(parsed.opts.whole_word);
        assert!(!parsed.opts.case_sensitive);
    }

    fn hit(path: &str, line_no: usize) -> SearchHit {
        SearchHit {
            path: PathBuf::from(format!("/ws/{path}")),
            line_no,
            line_text: String::new(),
        }
    }

    const A: &str = "one\ntwo needle\nthree\nfour\nfive\nsix needle\nseven\n";
    const B: &str = "needle first\nsecond\n";

    fn read(p: &Path) -> Option<String> {
        match p.to_str()? {
            "/ws/src/a.rs" => Some(A.to_string()),
            "/ws/b.rs" => Some(B.to_string()),
            _ => None,
        }
    }

    #[test]
    fn the_body_groups_by_file_with_context_and_gaps() {
        let hits = [hit("src/a.rs", 6), hit("b.rs", 1), hit("src/a.rs", 2)];
        let doc = render(&q(), Path::new("/ws"), &hits, &mut read);
        let body = doc
            .strip_prefix(&header(&q()))
            .expect("starts with the header");
        assert_eq!(
            body,
            "\n3 results - 2 files\n\nb.rs:\n  1:  needle first\n  2   second\n\nsrc/a.rs:\n  1   one\n  2:  two needle\n  3   three\n  ...\n  5   five\n  6:  six needle\n  7   seven\n"
        );
    }

    #[test]
    fn touching_context_runs_merge_without_a_gap() {
        let hits = [hit("src/a.rs", 2), hit("src/a.rs", 4)];
        let doc = render(&q(), Path::new("/ws"), &hits, &mut read);
        assert!(
            doc.contains("  2:  two needle\n  3   three\n  4:  four\n  5   five\n"),
            "{doc}"
        );
        assert!(!doc.contains("..."), "{doc}");
    }

    #[test]
    fn no_results_says_so() {
        let doc = render(&q(), Path::new("/ws"), &[], &mut read);
        assert_eq!(doc, format!("{}\nNo results\n", header(&q())));
    }

    #[test]
    fn a_row_locates_its_file_and_line() {
        let hits = [hit("src/a.rs", 6), hit("src/a.rs", 2), hit("b.rs", 1)];
        let doc = render(&q(), Path::new("/ws"), &hits, &mut read);
        let row = |needle: &str| doc.lines().position(|l| l.contains(needle)).unwrap();
        assert_eq!(
            locate(&doc, row("six needle")),
            Some((PathBuf::from("src/a.rs"), 6))
        );
        assert_eq!(
            locate(&doc, row("seven")),
            Some((PathBuf::from("src/a.rs"), 7))
        );
        assert_eq!(
            locate(&doc, row("needle first")),
            Some((PathBuf::from("b.rs"), 1))
        );
        assert_eq!(locate(&doc, 0), None, "the header");
        assert_eq!(locate(&doc, row("results - ")), None, "the summary");
        assert_eq!(locate(&doc, row("src/a.rs:")), None, "a heading");
        assert_eq!(locate(&doc, row("...")), None, "a gap");
    }
}
