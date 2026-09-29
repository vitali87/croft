//! A small, dependency-free Markdown linter (VS Code has no bundled
//! Markdown linter of its own; `davidanson.vscode-markdownlint` is the
//! popular third-party extension — see `src/vscode_extensions.rs`). Runs
//! entirely on the buffer text, no LSP server required, so it works the
//! moment a `.md` file is opened.
//!
//! Deliberately covers a handful of the most common `markdownlint` rules
//! rather than the whole rule set, chosen for a very low false-positive
//! rate: multiple top-level headings, no space after a heading's `#`,
//! trailing whitespace, and runs of blank lines.

use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};

/// Lints Markdown `text` and returns one diagnostic per violation, in
/// document order. `start_line`/`end_line` are 0-based, matching the LSP
/// convention `crate::lsp::manager::Diagnostic` already uses everywhere
/// else, so these diagnostics splice into the same per-line decode path
/// (`Editor::apply_diagnostics`) as a real language server's.
pub fn lint(text: &str) -> Vec<Diagnostic> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut seen_h1 = false;
    let mut blank_run_start: Option<usize> = None;

    let mut in_fence: Option<(char, usize)> = None;

    for (i, line) in lines.iter().enumerate() {
        // A fenced block's contents are literal, so no heading rule applies
        // inside one: `#NoSpace` in a shell snippet is a comment and a
        // `# Title` in a Markdown example is not this document's heading.
        // The closing fence must use the same character AND be at least as
        // long as the opener, which is what lets a ```` ```` ```` block quote
        // a ``` one. Binding the opener's length and discarding it let a
        // 3-tick line close a 4-tick fence, so the real close re-opened a
        // fence that swallowed the rest of the document.
        let fence_indent = line.len() - line.trim_start().len();
        let fence = if fence_indent <= 3 {
            let t = line.trim_start();
            let c = t.chars().next().filter(|&c| c == '`' || c == '~');
            c.map(|c| (c, t.chars().take_while(|&x| x == c).count()))
                .filter(|&(_, n)| n >= 3)
                .map(|(c, n)| (c, n, &t[n..]))
        } else {
            None
        };
        let is_fence_line = match (in_fence, fence) {
            // A backtick fence's info string cannot hold a backtick, so
            // "```ls``` lists files" is inline code, not an opener.
            (None, Some((c, n, info))) if !(c == '`' && info.contains('`')) => {
                in_fence = Some((c, n));
                true
            }
            // A closer carries nothing after its run: "```js" inside a
            // ``` block is content, not the end of it.
            (Some((oc, on)), Some((c, n, rest)))
                if oc == c && n >= on && rest.trim().is_empty() =>
            {
                in_fence = None;
                true
            }
            _ => false,
        };

        // A fence line is Markdown STRUCTURE, not code content, so MD009
        // applies to it (markdownlint flags a trailing space on a ```sh info
        // string). Only the lines BETWEEN fences are exempt.
        if is_fence_line {
            check_md009(line, i, &mut out);
        }

        // MD012 bookkeeping has to happen for fence lines and in-fence lines
        // alike, before the `continue` below. Skipping it left the blank run
        // open across a whole code block, so ONE blank line before a fence was
        // reported as "multiple consecutive blank lines" - 7 false hits on
        // this repo's own README, 17 on CONTRIBUTING.md.
        //
        // A fence line is a non-blank line, so it closes any open run. Lines
        // INSIDE a fence are content: they neither open nor extend a run,
        // which keeps blank lines in a code block unreported.
        if is_fence_line || in_fence.is_some() {
            if let Some(start) = blank_run_start
                && i - start > 1
            {
                out.push(diag(
                    start + 1,
                    0,
                    0,
                    DiagnosticSeverity::Hint,
                    "MD012: multiple consecutive blank lines",
                ));
            }
            blank_run_start = None;
            continue;
        }

        let trimmed_start = line.trim_start();
        // CommonMark: an ATX heading takes 0-3 leading spaces. Four or more
        // makes the line an indented code block, so `trim_start()` alone
        // linted code as headings.
        let indent = line.len() - trimmed_start.len();
        let heading_hashes = if indent <= 3 {
            trimmed_start.chars().take_while(|&c| c == '#').count()
        } else {
            0
        };

        // MD025: more than one top-level (`# `) heading in a document.
        // CommonMark separates the marker with a space OR a tab.
        if heading_hashes == 1 && trimmed_start[1..].starts_with([' ', '\t']) {
            if seen_h1 {
                out.push(diag(
                    i,
                    0,
                    // `Diagnostic`'s char fields are LSP UTF-16 offsets, which
                    // `Editor::recompute_diagnostic_spans` decodes with
                    // `utf16_to_char_col`. A byte length overshoots the line on
                    // any non-ASCII text.
                    line.encode_utf16().count(),
                    DiagnosticSeverity::Warning,
                    "MD025: multiple top-level headings in the same document",
                ));
            }
            seen_h1 = true;
        }

        // MD018: an ATX heading's `#`s must be followed by a space (`#Heading`
        // is not a heading in CommonMark, a common authoring slip).
        if (1..=6).contains(&heading_hashes) {
            let rest = &trimmed_start[heading_hashes..];
            if !rest.is_empty() && !rest.starts_with([' ', '\t', '#']) {
                // UTF-16 units, as above: the indent is ASCII spaces here,
                // but keeping the same unit everywhere is what stops the next
                // edit reintroducing a byte offset.
                let col = line[..line.len() - trimmed_start.len()]
                    .encode_utf16()
                    .count();
                out.push(diag(
                    i,
                    col,
                    col + heading_hashes,
                    DiagnosticSeverity::Warning,
                    "MD018: no space after '#' in the heading marker",
                ));
            }
        }

        // MD009: trailing whitespace, excluding Markdown's two-space hard
        // line break (exactly two trailing spaces) and blank lines.
        check_md009(line, i, &mut out);

        // MD012: more than one consecutive blank line.
        if line.trim().is_empty() {
            blank_run_start.get_or_insert(i);
        } else {
            if let Some(start) = blank_run_start
                && i - start > 1
            {
                out.push(diag(
                    start + 1,
                    0,
                    0,
                    DiagnosticSeverity::Hint,
                    "MD012: multiple consecutive blank lines",
                ));
            }
            blank_run_start = None;
        }
    }
    if let Some(start) = blank_run_start
        && lines.len().saturating_sub(start) > 1
    {
        out.push(diag(
            start + 1,
            0,
            0,
            DiagnosticSeverity::Hint,
            "MD012: multiple consecutive blank lines",
        ));
    }

    out
}

/// MD009: trailing WHITESPACE, so a tab counts; trimming only `' '` let
/// `foo\t` through. The hard-break exemption is exactly two ASCII spaces -
/// `" \t"` is not a hard break. Shared by the fence path and the ordinary
/// one, because a fence line is structure and markdownlint flags it too.
fn check_md009(line: &str, i: usize, out: &mut Vec<Diagnostic>) {
    let trimmed_end = line.trim_end();
    let trailing = &line[trimmed_end.len()..];
    let hard_break = trailing == "  ";
    if !trailing.is_empty() && !hard_break && !trimmed_end.is_empty() {
        out.push(diag(
            i,
            trimmed_end.encode_utf16().count(),
            line.encode_utf16().count(),
            DiagnosticSeverity::Hint,
            "MD009: trailing whitespace",
        ));
    }
}

fn diag(
    line: usize,
    start_char: usize,
    end_char: usize,
    severity: DiagnosticSeverity,
    message: &str,
) -> Diagnostic {
    Diagnostic {
        start_line: line as u32,
        start_char: start_char as u32,
        end_line: line as u32,
        end_char: end_char as u32,
        severity,
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_code_at_a_line_start_does_not_open_a_fence() {
        let d = lint("```ls``` lists files.\n\n#Bad\n");
        assert!(
            !d.is_empty(),
            "the heading after inline code is still linted"
        );
        let d = lint("```\n```js\n#NotLinted\n```\n\n#Bad\n");
        assert_eq!(
            d.len(),
            1,
            "an info line inside a block does not close it: {d:?}"
        );
    }

    #[test]
    fn flags_a_second_top_level_heading() {
        let text = "# Title\n\nSome text.\n\n# Another Title\n";
        let diags = lint(text);
        assert!(
            diags.iter().any(|d| d.message.contains("MD025")),
            "found: {diags:?}"
        );
    }

    #[test]
    fn single_h1_is_not_flagged() {
        let text = "# Title\n\nSome text.\n\n## Subheading\n";
        let diags = lint(text);
        assert!(!diags.iter().any(|d| d.message.contains("MD025")));
    }

    #[test]
    fn flags_missing_space_after_hash() {
        let text = "#Title with no space\n";
        let diags = lint(text);
        assert!(diags.iter().any(|d| d.message.contains("MD018")));
    }

    #[test]
    fn flags_trailing_whitespace_but_not_hard_break() {
        let text = "trailing spaces here   \nhard break here  \nclean line\n";
        let diags = lint(text);
        let md009: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("MD009"))
            .collect();
        assert_eq!(md009.len(), 1, "found: {diags:?}");
        assert_eq!(md009[0].start_line, 0);
    }

    #[test]
    fn flags_multiple_consecutive_blank_lines() {
        let text = "one\n\n\n\ntwo\n";
        let diags = lint(text);
        assert!(
            diags.iter().any(|d| d.message.contains("MD012")),
            "found: {diags:?}"
        );
    }

    #[test]
    fn single_blank_line_is_not_flagged() {
        let text = "one\n\ntwo\n";
        let diags = lint(text);
        assert!(!diags.iter().any(|d| d.message.contains("MD012")));
    }

    /// A fenced code block is not Markdown structure: its contents are
    /// literal. A `#NoSpace` line inside one is shell/Python syntax, not a
    /// malformed heading, and a second `# Title` inside one is not a second
    /// document heading. Linting them produces false positives on the most
    /// common thing a Markdown file contains.
    #[test]
    fn fenced_code_is_not_linted_as_headings() {
        let text = "# Title\n\n```sh\n#!/bin/sh\n#NoSpace\n# Title\n```\n";
        let diags = lint(text);
        assert!(
            !diags.iter().any(|d| d.message.contains("MD018")),
            "a comment inside a fence is not a heading: {diags:?}"
        );
        assert!(
            !diags.iter().any(|d| d.message.contains("MD025")),
            "a heading inside a fence is not a document heading: {diags:?}"
        );
    }

    /// CommonMark allows an ATX heading 0-3 leading spaces; four or more
    /// makes it an indented code block. `trim_start()` accepts any amount,
    /// so indented code was linted as headings.
    #[test]
    fn four_space_indented_code_is_not_a_heading() {
        let text = "# Title\n\nExample:\n\n    # Title\n    #NoSpace\n";
        let diags = lint(text);
        assert!(
            !diags.iter().any(|d| d.message.contains("MD025")),
            "4-space indented text is code, not a second H1: {diags:?}"
        );
        assert!(
            !diags.iter().any(|d| d.message.contains("MD018")),
            "4-space indented text is code, not a heading: {diags:?}"
        );
    }

    /// A heading indented 0-3 spaces IS a heading - the control proving the
    /// bound above rejects only what it should.
    #[test]
    fn three_space_indented_heading_still_counts() {
        let text = "# Title\n\n   # Second\n";
        let diags = lint(text);
        assert!(
            diags.iter().any(|d| d.message.contains("MD025")),
            "3 spaces is still a heading: {diags:?}"
        );
    }

    /// `Diagnostic`'s char fields are LSP UTF-16 offsets - the editor decodes
    /// them with `utf16_to_char_col`. Handing it `line.len()` (UTF-8 bytes)
    /// overshoots on any non-ASCII line, so the squiggle runs past the text.
    #[test]
    fn md025_end_is_a_utf16_offset_not_a_byte_count() {
        let text = "# Title\n\n# Héllo wörld\n";
        let diags = lint(text);
        let md025 = diags
            .iter()
            .find(|d| d.message.contains("MD025"))
            .expect("second heading flagged");
        let line = "# Héllo wörld";
        assert_eq!(
            md025.end_char as usize,
            line.encode_utf16().count(),
            "end_char must be UTF-16 units ({}), not bytes ({})",
            line.encode_utf16().count(),
            line.len()
        );
    }

    /// MD009 is trailing WHITESPACE. Trimming only `' '` let a line ending in
    /// a tab through.
    #[test]
    fn md009_catches_a_trailing_tab() {
        let text = "a line ending in a tab\t\n";
        let diags = lint(text);
        assert!(
            diags.iter().any(|d| d.message.contains("MD009")),
            "a trailing tab is trailing whitespace: {diags:?}"
        );
    }

    /// The hard-break exemption is exactly two ASCII spaces; a tab plus a
    /// space is not a hard break and must still be flagged.
    #[test]
    fn md009_hard_break_exemption_is_two_spaces_only() {
        let text = "hard break  \nnot a break \t\n";
        let diags = lint(text);
        let md009: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("MD009"))
            .collect();
        assert_eq!(md009.len(), 1, "only the tab line: {diags:?}");
        assert_eq!(md009[0].start_line, 1);
    }

    #[test]
    fn repro_readme_has_no_false_md012() {
        let text = include_str!("../README.md");
        let md012: Vec<_> = lint(text)
            .into_iter()
            .filter(|d| d.message.contains("MD012"))
            .collect();
        assert!(
            md012.is_empty(),
            "false MD012 on the repo README: {md012:?}"
        );
    }

    #[test]
    fn repro_four_tick_fence_not_closed_by_three() {
        let diags = lint("````\n```\n````\n#RealTypo\n");
        assert!(
            diags.iter().any(|d| d.message.contains("MD018")),
            "a 3-tick line must not close a 4-tick fence: {diags:?}"
        );
    }

    /// The realistic-document control. It carries a FENCED BLOCK with the
    /// blank lines a real document puts around one, because without that this
    /// test passed while every code block in the repo drew a false MD012.
    #[test]
    fn clean_document_has_no_diagnostics() {
        let text = "# Title\n\nA paragraph with *emphasis* and a [link](https://example.com).\n\n## Section\n\n```sh\ncargo install croft\n```\n\nMore text.\n";
        assert!(lint(text).is_empty(), "found: {:?}", lint(text));
    }

    /// MD012 still fires on a genuine double blank - the control proving the
    /// fence fix above did not simply disable the rule.
    #[test]
    fn md012_still_fires_outside_a_fence() {
        let diags = lint("# T\n\n```sh\nx\n```\n\n\ntext\n");
        assert!(
            diags.iter().any(|d| d.message.contains("MD012")),
            "two blanks after a fence is still a violation: {diags:?}"
        );
    }

    /// Blank lines INSIDE a fence are code content, never MD012.
    #[test]
    fn blank_lines_inside_a_fence_are_not_md012() {
        let diags = lint("# T\n\n```sh\none\n\n\n\ntwo\n```\n");
        assert!(
            !diags.iter().any(|d| d.message.contains("MD012")),
            "blank lines in a code block are content: {diags:?}"
        );
    }

    /// A fence line is Markdown structure, so trailing whitespace on it is
    /// still MD009 - on the info string and on the closing fence alike.
    #[test]
    fn md009_applies_to_the_fence_lines_themselves() {
        let opening = lint("```sh   \nx\n```\n");
        assert!(
            opening.iter().any(|d| d.message.contains("MD009")),
            "trailing space on an info string: {opening:?}"
        );
        let closing = lint("```\nx\n```   \n");
        assert!(
            closing.iter().any(|d| d.message.contains("MD009")),
            "trailing space on a closing fence: {closing:?}"
        );
    }

    /// ...but NOT to the code inside it, which is literal text.
    #[test]
    fn md009_does_not_apply_inside_a_fence() {
        let diags = lint("```sh\ntrailing spaces are code   \n```\n");
        assert!(
            !diags.iter().any(|d| d.message.contains("MD009")),
            "code content is literal: {diags:?}"
        );
    }

    /// A tab after the marker is a valid separator in CommonMark: `#\tTitle`
    /// is a heading, so it is not MD018 and it does count toward MD025.
    #[test]
    fn a_tab_separates_the_heading_marker() {
        let diags = lint("#\tTitle\n\n#\tSecond\n");
        assert!(
            !diags.iter().any(|d| d.message.contains("MD018")),
            "a tab is a separator: {diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.message.contains("MD025")),
            "two tab-separated H1s are still two H1s: {diags:?}"
        );
    }

    /// A tilde fence is not closed by a backtick line.
    #[test]
    fn a_backtick_line_does_not_close_a_tilde_fence() {
        let diags = lint("~~~\n```\n~~~\n#RealTypo\n");
        assert!(
            diags.iter().any(|d| d.message.contains("MD018")),
            "the ~~~ close ends the block, so line 4 is linted: {diags:?}"
        );
    }
}
