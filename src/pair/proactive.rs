//! Proactive navigator trigger (docs/MULTIPLAYER.md): decide that the
//! driver just COMPLETED a new construct, so the seated navigator may take
//! a comment-only look on its own once typing pauses.
//!
//! "New construct" is judged by tree-sitter, not text heuristics: for code
//! languages the outline query ([`crate::outline_syntax::symbols_for`])
//! must yield a definition that the navigator's last look did not have — a
//! half-typed `fn foo(` parses as an ERROR node, matches no query, and
//! therefore never fires. Markdown has no outline query, so its own block
//! grammar is walked directly: a new heading, or an ADDED paragraph.
//! In every case the construct population must GROW: renames, rewordings,
//! moves, and edits inside existing constructs swap or reshape keys
//! without adding one, and must never fire an unsolicited turn.
//!
//! Everything here is pure text-in/row-out; the App owns the pause clock,
//! the busy/seated gates, and the actual yield turn.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use tree_sitter::Parser;

use crate::highlight::LangKind;
use crate::outline_syntax::symbols_for;

/// The 0-based row of a construct present in `new` but not in `old` (the
/// navigator's last look of the file), or None when nothing new completed.
/// Instances are counted, not just named: a second `fn helper` counts as
/// new even though the name already existed.
pub fn new_construct_row(kind: LangKind, old: &str, new: &str) -> Option<usize> {
    match kind {
        LangKind::Markdown => markdown_new_row(old, new),
        _ => code_new_row(kind, old, new),
    }
}

/// Code languages: multiset-compare outline symbols (kind + name); the
/// anchor is the last instance whose running count exceeds the old count —
/// with several candidates, the bottom-most is where the driver most
/// likely just typed. The population must GROW: a rename swaps one key
/// for another without adding anything, and is an edit, not a construct.
fn code_new_row(kind: LangKind, old: &str, new: &str) -> Option<usize> {
    let fresh = symbols_for(kind, new.as_bytes());
    if fresh.is_empty() {
        return None; // no outline query for this language, or nothing parsed
    }
    let old_syms = symbols_for(kind, old.as_bytes());
    if fresh.len() <= old_syms.len() {
        return None; // nothing added — at most edits, moves, or renames
    }
    let mut before: HashMap<(u8, &str), usize> = HashMap::new();
    for s in &old_syms {
        *before.entry((s.kind as u8, s.name.as_str())).or_default() += 1;
    }
    let mut running: HashMap<(u8, &str), usize> = HashMap::new();
    let mut row = None;
    for s in &fresh {
        let key = (s.kind as u8, s.name.as_str());
        let seen = running.entry(key).or_default();
        *seen += 1;
        if *seen > before.get(&key).copied().unwrap_or(0) {
            row = Some(s.range_start_line as usize);
        }
    }
    row
}

/// Markdown: a new heading (multiset by text), else an ADDED paragraph.
fn markdown_new_row(old: &str, new: &str) -> Option<usize> {
    let (old_heads, old_paras) = markdown_scan(old);
    let (new_heads, new_paras) = markdown_scan(new);

    // Headings must GROW too: rewording one swaps its text key without
    // adding a heading, and is an edit, not a new section.
    if new_heads.len() > old_heads.len() {
        let mut before: HashMap<&str, usize> = HashMap::new();
        for (text, _) in &old_heads {
            *before.entry(text.as_str()).or_default() += 1;
        }
        let mut running: HashMap<&str, usize> = HashMap::new();
        let mut row = None;
        for (text, r) in &new_heads {
            let seen = running.entry(text.as_str()).or_default();
            *seen += 1;
            if *seen > before.get(text.as_str()).copied().unwrap_or(0) {
                row = Some(*r);
            }
        }
        if row.is_some() {
            return row;
        }
    }

    // Paragraphs carry no name, so only GROWTH counts as new (an edit to
    // existing prose changes a hash but not the count). The anchor is the
    // first paragraph whose text the old document did not contain, falling
    // back to the last paragraph.
    if new_paras.len() <= old_paras.len() {
        return None;
    }
    let old_hashes: std::collections::HashSet<u64> = old_paras.iter().map(|(h, _)| *h).collect();
    new_paras
        .iter()
        .find(|(h, _)| !old_hashes.contains(h))
        .or(new_paras.last())
        .map(|(_, r)| *r)
}

/// Headings as (text, 0-based row), in document order.
type Headings = Vec<(String, usize)>;
/// Paragraphs as (text hash, 0-based row), in document order.
type Paragraphs = Vec<(u64, usize)>;

/// Walk the markdown block tree: headings plus paragraphs.
fn markdown_scan(src: &str) -> (Headings, Paragraphs) {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .is_err()
    {
        return (Vec::new(), Vec::new());
    }
    let Some(tree) = parser.parse(src, None) else {
        return (Vec::new(), Vec::new());
    };
    let mut heads = Vec::new();
    let mut paras = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "atx_heading" | "setext_heading" => {
                let text = node
                    .utf8_text(src.as_bytes())
                    .unwrap_or_default()
                    .trim_start_matches(['#', ' '])
                    .trim()
                    .to_string();
                heads.push((text, node.start_position().row));
            }
            "paragraph" => {
                let mut h = DefaultHasher::new();
                node.utf8_text(src.as_bytes())
                    .unwrap_or_default()
                    .hash(&mut h);
                paras.push((h.finish(), node.start_position().row));
            }
            _ => {}
        }
        for i in (0..node.named_child_count() as u32).rev() {
            if let Some(c) = node.named_child(i) {
                stack.push(c);
            }
        }
    }
    (heads, paras)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_completed_function_fires_and_a_body_edit_does_not() {
        let old = "fn alpha() {\n    1;\n}\n";
        // Body edit only: same constructs, no trigger.
        let edited = "fn alpha() {\n    2;\n}\n";
        assert_eq!(new_construct_row(LangKind::Rust, old, edited), None);
        // A new function below fires at its own row.
        let grown = "fn alpha() {\n    1;\n}\n\nfn beta() {}\n";
        assert_eq!(new_construct_row(LangKind::Rust, old, grown), Some(4));
        // Half-typed: parses as an error, never fires.
        let typing = "fn alpha() {\n    1;\n}\n\nfn bet\n";
        assert_eq!(new_construct_row(LangKind::Rust, old, typing), None);
    }

    #[test]
    fn a_second_instance_of_the_same_name_counts_as_new() {
        let old = "impl A {\n    fn go(&self) {}\n}\n";
        let grown = "impl A {\n    fn go(&self) {}\n}\nimpl B {\n    fn go(&self) {}\n}\n";
        // The bottom-most exceeding instance anchors the look.
        let row = new_construct_row(LangKind::Rust, old, grown);
        assert_eq!(
            row,
            Some(4),
            "the second `fn go` (inside impl B) is the new one"
        );
    }

    #[test]
    fn markdown_headings_and_added_paragraphs_fire_but_prose_edits_do_not() {
        let old = "# Title\n\nfirst paragraph here.\n";
        // Editing existing prose: hash changes, count does not — no fire.
        let edited = "# Title\n\nfirst paragraph, reworded.\n";
        assert_eq!(new_construct_row(LangKind::Markdown, old, edited), None);
        // A new heading fires at its row.
        let heading = "# Title\n\nfirst paragraph here.\n\n## Details\n";
        assert_eq!(new_construct_row(LangKind::Markdown, old, heading), Some(4));
        // A new paragraph fires at its row.
        let para = "# Title\n\nfirst paragraph here.\n\na second paragraph lands.\n";
        assert_eq!(new_construct_row(LangKind::Markdown, old, para), Some(4));
    }

    #[test]
    fn a_rename_is_an_edit_not_a_new_construct() {
        // Renaming a function swaps one key for another but adds nothing:
        // the construct population did not grow, so no proactive turn.
        let old = "fn alpha() {}\n\nfn beta() {}\n";
        let renamed = "fn omega() {}\n\nfn beta() {}\n";
        assert_eq!(new_construct_row(LangKind::Rust, old, renamed), None);
        // Same for rewording a markdown heading.
        let old_md = "# Setup\n\nprose.\n";
        let reworded = "# Set up\n\nprose.\n";
        assert_eq!(
            new_construct_row(LangKind::Markdown, old_md, reworded),
            None
        );
    }

    #[test]
    fn languages_without_an_outline_query_never_fire() {
        assert_eq!(
            new_construct_row(LangKind::Json, "{}", "{\"a\": [1, 2, 3]}"),
            None
        );
    }
}
