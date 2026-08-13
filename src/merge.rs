//! Git merge-conflict blocks: detection and resolution.
//!
//! Mirrors VS Code's merge-conflict decorations: a conflict is the classic
//! marker triple `<<<<<<<` (current / ours), `=======`, `>>>>>>>` (incoming /
//! theirs), with an optional diff3 `|||||||` common-ancestor section between
//! ours and the separator. The marker regexes follow VS Code's
//! merge-conflict extension: header / footer / base markers are exactly seven
//! characters followed by whitespace or end-of-line, the separator is exactly
//! seven `=` (trailing whitespace tolerated). Malformed groups (a dangling
//! header with no separator / footer) are ignored rather than half-matched.

/// One conflict block, as inclusive line indices into the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConflictBlock {
    /// The `<<<<<<<` header line.
    pub ours_start: usize,
    /// The `|||||||` diff3 common-ancestor line, when present.
    pub base_start: Option<usize>,
    /// The `=======` separator line.
    pub sep: usize,
    /// The `>>>>>>>` footer line.
    pub theirs_end: usize,
}

impl ConflictBlock {
    /// True when `row` falls anywhere inside the block, markers included.
    pub fn contains(&self, row: usize) -> bool {
        row >= self.ours_start && row <= self.theirs_end
    }
}

/// How to resolve a conflict — VS Code's Accept Current / Incoming / Both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Current,
    Incoming,
    Both,
}

/// Marker classification for one line, VS Code's merge-conflict regexes:
/// `^<{7}(\s|$)`, `^\|{7}(\s|$)`, `^={7}\s*$`, `^>{7}(\s|$)`.
fn marker(line: &str) -> Option<Marker> {
    let bytes = line.as_bytes();
    let seven = |b: u8| bytes.len() >= 7 && bytes[..7].iter().all(|&c| c == b);
    let word_boundary = || bytes.len() == 7 || bytes[7].is_ascii_whitespace();
    if seven(b'<') && bytes.get(7) != Some(&b'<') && word_boundary() {
        return Some(Marker::Ours);
    }
    if seven(b'|') && bytes.get(7) != Some(&b'|') && word_boundary() {
        return Some(Marker::Base);
    }
    if seven(b'=') && line[7..].chars().all(char::is_whitespace) && bytes.get(7) != Some(&b'=') {
        return Some(Marker::Sep);
    }
    if seven(b'>') && bytes.get(7) != Some(&b'>') && word_boundary() {
        return Some(Marker::Theirs);
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Ours,
    Base,
    Sep,
    Theirs,
}

/// Scan the buffer for complete conflict blocks, in order.
pub fn find_conflicts(lines: &[String]) -> Vec<ConflictBlock> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if marker(&lines[i]) != Some(Marker::Ours) {
            i += 1;
            continue;
        }
        // Walk forward for base? / sep / theirs in order; any out-of-order
        // marker (or a new header) abandons this group as malformed.
        let ours_start = i;
        let mut base_start = None;
        let mut sep = None;
        let mut theirs_end = None;
        let mut j = i + 1;
        while j < lines.len() {
            match marker(&lines[j]) {
                Some(Marker::Ours) => break,
                Some(Marker::Base) if sep.is_none() && base_start.is_none() => {
                    base_start = Some(j);
                }
                Some(Marker::Sep) if sep.is_none() => sep = Some(j),
                Some(Marker::Theirs) if sep.is_some() => {
                    theirs_end = Some(j);
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        match (sep, theirs_end) {
            (Some(sep), Some(theirs_end)) => {
                out.push(ConflictBlock {
                    ours_start,
                    base_start,
                    sep,
                    theirs_end,
                });
                i = theirs_end + 1;
            }
            // Malformed: resume after the dangling header (or at the next
            // header when one interrupted the walk).
            _ => i = j.max(i + 1),
        }
    }
    out
}

/// The block containing `row`, if any.
pub fn conflict_containing(blocks: &[ConflictBlock], row: usize) -> Option<ConflictBlock> {
    blocks.iter().copied().find(|b| b.contains(row))
}

/// The lines that replace the whole block (markers included) for `res`.
/// The diff3 base section is never kept — like VS Code, resolving discards
/// the common-ancestor text.
pub fn resolution_lines(lines: &[String], block: &ConflictBlock, res: Resolution) -> Vec<String> {
    let ours_end = block.base_start.unwrap_or(block.sep);
    let ours = &lines[block.ours_start + 1..ours_end];
    let theirs = &lines[block.sep + 1..block.theirs_end];
    match res {
        Resolution::Current => ours.to_vec(),
        Resolution::Incoming => theirs.to_vec(),
        Resolution::Both => {
            let mut out = ours.to_vec();
            out.extend_from_slice(theirs);
            out
        }
    }
}

/// The next conflict strictly after `from_row`, wrapping to the first —
/// `(index into blocks, header row)`. F7's jump order, VS Code's "Go to
/// Next Conflict".
pub fn next_conflict(blocks: &[ConflictBlock], from_row: usize) -> Option<(usize, usize)> {
    if blocks.is_empty() {
        return None;
    }
    blocks
        .iter()
        .enumerate()
        .find(|(_, b)| b.ours_start > from_row)
        .or(blocks.iter().enumerate().next())
        .map(|(i, b)| (i, b.ours_start))
}

/// The previous conflict strictly before `from_row`, wrapping to the last.
pub fn prev_conflict(blocks: &[ConflictBlock], from_row: usize) -> Option<(usize, usize)> {
    if blocks.is_empty() {
        return None;
    }
    blocks
        .iter()
        .enumerate()
        .rev()
        .find(|(_, b)| b.ours_start < from_row)
        .or(blocks.iter().enumerate().next_back())
        .map(|(i, b)| (i, b.ours_start))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    fn conflicted() -> Vec<String> {
        lines(&[
            "fn main() {",
            "<<<<<<< HEAD",
            "    ours();",
            "=======",
            "    theirs();",
            ">>>>>>> feature",
            "}",
        ])
    }

    #[test]
    fn find_conflicts_locates_the_marker_triple() {
        let blocks = find_conflicts(&conflicted());
        assert_eq!(
            blocks,
            vec![ConflictBlock {
                ours_start: 1,
                base_start: None,
                sep: 3,
                theirs_end: 5,
            }]
        );
    }

    #[test]
    fn find_conflicts_handles_the_diff3_base_section() {
        let buf = lines(&[
            "<<<<<<< HEAD",
            "ours",
            "||||||| merged common ancestors",
            "base",
            "=======",
            "theirs",
            ">>>>>>> feature",
        ]);
        let blocks = find_conflicts(&buf);
        assert_eq!(
            blocks,
            vec![ConflictBlock {
                ours_start: 0,
                base_start: Some(2),
                sep: 4,
                theirs_end: 6,
            }]
        );
    }

    #[test]
    fn find_conflicts_finds_every_block_in_order() {
        let mut buf = conflicted();
        buf.extend(conflicted());
        let blocks = find_conflicts(&buf);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].ours_start, 1);
        assert_eq!(blocks[1].ours_start, 8);
    }

    #[test]
    fn find_conflicts_ignores_a_dangling_header() {
        let buf = lines(&["<<<<<<< HEAD", "no separator or footer here"]);
        assert!(find_conflicts(&buf).is_empty());
    }

    #[test]
    fn find_conflicts_ignores_lookalike_content_lines() {
        // A separator-looking line of the wrong length, and markers not at
        // column zero, are content, not conflict machinery.
        let buf = lines(&[
            "<<<<<<< HEAD",
            "====",
            " =======",
            "======= x",
            ">>>>>>> br",
        ]);
        assert!(
            find_conflicts(&buf).is_empty(),
            "an eight-char or indented or suffixed separator must not split a block"
        );
    }

    #[test]
    fn resolution_current_keeps_only_the_ours_section() {
        let buf = conflicted();
        let block = find_conflicts(&buf)[0];
        assert_eq!(
            resolution_lines(&buf, &block, Resolution::Current),
            lines(&["    ours();"])
        );
    }

    #[test]
    fn resolution_incoming_keeps_only_the_theirs_section() {
        let buf = conflicted();
        let block = find_conflicts(&buf)[0];
        assert_eq!(
            resolution_lines(&buf, &block, Resolution::Incoming),
            lines(&["    theirs();"])
        );
    }

    #[test]
    fn resolution_both_keeps_ours_then_theirs() {
        let buf = conflicted();
        let block = find_conflicts(&buf)[0];
        assert_eq!(
            resolution_lines(&buf, &block, Resolution::Both),
            lines(&["    ours();", "    theirs();"])
        );
    }

    #[test]
    fn resolution_discards_the_diff3_base_section() {
        let buf = lines(&[
            "<<<<<<< HEAD",
            "ours",
            "||||||| base",
            "base text",
            "=======",
            "theirs",
            ">>>>>>> feature",
        ]);
        let block = find_conflicts(&buf)[0];
        assert_eq!(
            resolution_lines(&buf, &block, Resolution::Current),
            lines(&["ours"]),
            "the common-ancestor section is never kept"
        );
        assert_eq!(
            resolution_lines(&buf, &block, Resolution::Both),
            lines(&["ours", "theirs"])
        );
    }
}

#[cfg(test)]
mod nav_tests {
    use super::*;

    fn two_block_doc() -> Vec<String> {
        [
            "a",
            "<<<<<<< HEAD",
            "one",
            "=======",
            "uno",
            ">>>>>>> feature",
            "b",
            "c",
            "<<<<<<< HEAD",
            "two",
            "=======",
            "dos",
            ">>>>>>> feature",
            "d",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn next_and_prev_wrap_across_the_document() {
        let doc = two_block_doc();
        let blocks = find_conflicts(&doc);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            next_conflict(&blocks, 0),
            Some((0, 1)),
            "from the top, F7 reaches the first conflict"
        );
        assert_eq!(
            next_conflict(&blocks, 1),
            Some((1, 8)),
            "from block 0's header the next jump is block 1"
        );
        assert_eq!(
            next_conflict(&blocks, 8),
            Some((0, 1)),
            "past the last block it wraps to the first"
        );
        assert_eq!(prev_conflict(&blocks, 8), Some((0, 1)));
        assert_eq!(
            prev_conflict(&blocks, 1),
            Some((1, 8)),
            "before the first block it wraps to the last"
        );
        assert_eq!(next_conflict(&[], 0), None);
    }
}
