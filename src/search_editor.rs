//! Search Editor (#615): workspace search results as a document.
//!
//! The Search sidebar holds one query at a time and forgets it when the next
//! one starts. A Search Editor is a tab holding a query and its results as
//! plain text, in VS Code's `.code-search` layout, so several searches can
//! stay open side by side, the header can be edited and re-run, and the
//! results can be read, scrolled and searched like any other buffer.
//!
//! The text is the whole state: re-running parses the query back out of the
//! header, and opening a result parses the file and line out of the body.
//! Nothing lives beside the buffer that an edit could get out of step with.

use std::path::{Path, PathBuf};

use crate::widgets::search::{SearchHit, SearchOpts};

/// The query a Search Editor runs, as its header spells it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub query: String,
    pub opts: SearchOpts,
    pub include: String,
    pub exclude: String,
}

const QUERY: &str = "# Query: ";
const FLAGS: &str = "# Flags: ";
const INCLUDING: &str = "# Including: ";
const EXCLUDING: &str = "# Excluding: ";

/// Whether `lines` are a Search Editor's (its first line is the query).
pub fn is_search_editor(lines: &[String]) -> bool {
    lines.first().is_some_and(|l| l.starts_with(QUERY))
}

/// Render a header and its hits. Files are listed in path order and hits
/// in line order, so the same search always reads the same way even though
/// the walker that found them is parallel.
pub fn render(header: &Header, hits: &[SearchHit], root: &Path) -> String {
    let mut out = format!("{QUERY}{}\n", header.query);
    let mut flags = Vec::new();
    if header.opts.use_regex {
        flags.push("RegExp");
    }
    if header.opts.case_sensitive {
        flags.push("CaseSensitive");
    }
    if header.opts.whole_word {
        flags.push("WordMatch");
    }
    if !flags.is_empty() {
        out.push_str(&format!("{FLAGS}{}\n", flags.join(" ")));
    }
    if !header.include.trim().is_empty() {
        out.push_str(&format!("{INCLUDING}{}\n", header.include.trim()));
    }
    if !header.exclude.trim().is_empty() {
        out.push_str(&format!("{EXCLUDING}{}\n", header.exclude.trim()));
    }
    let mut sorted: Vec<&SearchHit> = hits.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path).then(a.line_no.cmp(&b.line_no)));
    let files = {
        let mut paths: Vec<&PathBuf> = sorted.iter().map(|h| &h.path).collect();
        paths.dedup();
        paths.len()
    };
    out.push('\n');
    out.push_str(&match (sorted.len(), files) {
        (0, _) => String::from("No results"),
        (1, _) => String::from("1 result - 1 file"),
        (n, 1) => format!("{n} results - 1 file"),
        (n, f) => format!("{n} results - {f} files"),
    });
    out.push('\n');
    let width = sorted
        .iter()
        .map(|h| h.line_no.to_string().len())
        .max()
        .unwrap_or(1);
    let mut current: Option<&Path> = None;
    for hit in sorted {
        if current != Some(hit.path.as_path()) {
            let shown = hit.path.strip_prefix(root).unwrap_or(&hit.path);
            out.push_str(&format!("\n{}:\n", shown.display()));
            current = Some(hit.path.as_path());
        }
        out.push_str(&format!("  {:>width$}: {}\n", hit.line_no, hit.line_text));
    }
    out
}

/// Read the query back out of a Search Editor's header, or `None` when the
/// first line is not one.
pub fn parse_header(lines: &[String]) -> Option<Header> {
    let query = lines.first()?.strip_prefix(QUERY)?.to_string();
    let mut header = Header {
        query,
        ..Header::default()
    };
    for line in lines.iter().skip(1).take_while(|l| l.starts_with("# ")) {
        if let Some(flags) = line.strip_prefix(FLAGS) {
            for flag in flags.split_whitespace() {
                match flag {
                    "RegExp" => header.opts.use_regex = true,
                    "CaseSensitive" => header.opts.case_sensitive = true,
                    "WordMatch" => header.opts.whole_word = true,
                    _ => {}
                }
            }
        } else if let Some(v) = line.strip_prefix(INCLUDING) {
            header.include = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix(EXCLUDING) {
            header.exclude = v.trim().to_string();
        }
    }
    Some(header)
}

/// The file and 1-based line a result row points at: its `  NN: text` line
/// number, under the nearest `path:` heading above it.
pub fn location_at(lines: &[String], row: usize, root: &Path) -> Option<(PathBuf, usize)> {
    let line_no = parse_result_line(lines.get(row)?)?;
    // Only the header is skipped, not every `#` line: `#notes.md:` is a
    // file heading like any other.
    let header_end = 1 + lines
        .iter()
        .skip(1)
        .take_while(|l| l.starts_with("# "))
        .count();
    let heading = lines
        .get(header_end..row)?
        .iter()
        .rev()
        .find(|l| !l.starts_with(' ') && l.ends_with(':'))?;
    let rel = heading.strip_suffix(':')?;
    let path = Path::new(rel);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    Some((path, line_no))
}

/// `  12: text` → 12.
fn parse_result_line(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("  ")?.trim_start();
    let (num, _) = rest.split_once(':')?;
    num.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, line_no: usize, text: &str) -> SearchHit {
        SearchHit {
            path: PathBuf::from(path),
            line_no,
            line_text: text.to_string(),
        }
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn render_groups_hits_by_file_in_path_then_line_order() {
        let header = Header {
            query: String::from("foo"),
            opts: SearchOpts {
                case_sensitive: true,
                whole_word: false,
                use_regex: true,
            },
            include: String::from("src"),
            exclude: String::new(),
        };
        let hits = [
            hit("/p/src/b.rs", 3, "foo()"),
            hit("/p/src/a.rs", 12, "let foo = 1;"),
            hit("/p/src/a.rs", 2, "use foo;"),
        ];
        let text = render(&header, &hits, Path::new("/p"));
        assert_eq!(
            text,
            "# Query: foo\n# Flags: RegExp CaseSensitive\n# Including: src\n\n\
             3 results - 2 files\n\nsrc/a.rs:\n   2: use foo;\n  12: let foo = 1;\n\n\
             src/b.rs:\n   3: foo()\n"
        );
    }

    #[test]
    fn the_header_round_trips() {
        let header = Header {
            query: String::from("a b"),
            opts: SearchOpts {
                case_sensitive: false,
                whole_word: true,
                use_regex: false,
            },
            include: String::from("*.rs"),
            exclude: String::from("target"),
        };
        let text = render(&header, &[], Path::new("/p"));
        assert!(text.contains("No results"));
        assert_eq!(parse_header(&lines(&text)), Some(header));
    }

    #[test]
    fn a_result_row_points_at_its_file_and_line() {
        let text = "# Query: foo\n\n2 results - 2 files\n\nsrc/a.rs:\n   2: use foo;\n\nsrc/b.rs:\n  30: foo()\n";
        let ls = lines(text);
        let root = Path::new("/p");
        assert_eq!(
            location_at(&ls, 5, root),
            Some((PathBuf::from("/p/src/a.rs"), 2))
        );
        assert_eq!(
            location_at(&ls, 8, root),
            Some((PathBuf::from("/p/src/b.rs"), 30))
        );
        assert_eq!(location_at(&ls, 4, root), None, "a heading is not a result");
        assert_eq!(location_at(&ls, 0, root), None, "nor is the header");
    }

    #[test]
    fn a_file_named_with_a_leading_hash_is_a_heading_too() {
        let hits = vec![hit("/p/a.rs", 3, "x"), hit("/p/#b.rs", 7, "x")];
        let text = render(&Header::default(), &hits, Path::new("/p"));
        let ls = lines(&text);
        let row = ls.iter().rposition(|l| l.starts_with("  7:")).unwrap();
        assert_eq!(
            location_at(&ls, row, Path::new("/p")),
            Some((PathBuf::from("/p/#b.rs"), 7))
        );
    }

    #[test]
    fn only_a_query_first_line_makes_a_search_editor() {
        assert!(is_search_editor(&lines("# Query: x\n")));
        assert!(!is_search_editor(&lines("# A heading\n")));
    }
}
