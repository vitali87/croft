//! Comment toggling: line and block comments for each language, JSX comments, and the
//! comment tokens each language uses.

use super::*;

impl Editor {
    /// VS Code "Toggle Line Comment" (Cmd+/): comment or uncomment every line
    /// the cursor, the selection or any extra caret touches using the
    /// language's line-comment marker. Comments are inserted at each block's
    /// common (minimum) indentation so the markers align; the lines
    /// uncomment only when every non-blank one is already commented. Each
    /// caret moves with the text on its line, so a selection still covers
    /// what it did. Returns false (a no-op) for languages with no line
    /// comment.
    pub fn toggle_line_comment(&mut self) -> bool {
        let Some(token) = self.line_comment_marker() else {
            // VS Code's fallback for a language with only block comments
            // (HTML, CSS, Markdown, XML): wrap the lines in one (#1181).
            return match block_comment_tokens(self.lang) {
                Some((open, close)) => self.toggle_lines_in_block_comment(open, close),
                None => false,
            };
        };
        let blocks = self.selected_row_blocks();
        let non_blank: Vec<usize> = blocks
            .iter()
            .flat_map(|&(start, end)| start..=end)
            .filter(|&r| !self.lines[r].trim().is_empty())
            .collect();
        if non_blank.is_empty() {
            return false;
        }
        self.push_undo(EditKind::ToggleComment);
        // Rows in JSX children take `{/* … */}`: there `//` is text (#1623).
        let jsx: std::collections::HashSet<usize> = self.jsx_child_rows(&non_blank);
        let all_commented = non_blank.iter().all(|&r| {
            if jsx.contains(&r) {
                is_jsx_comment(self.lines[r].trim())
            } else {
                line_comment_prefix_len(self.lines[r].trim_start(), token).is_some()
            }
        });
        // Each changed row with the char column of its edit and the chars it
        // added (negative: removed), to carry the carets on it along.
        let mut edits: Vec<(usize, usize, isize)> = Vec::with_capacity(non_blank.len());
        if all_commented {
            for &r in &non_blank {
                let line = self.lines[r].clone();
                let off = line.len() - line.trim_start().len();
                if jsx.contains(&r) {
                    let core = line[off..].trim_end();
                    let inner = &core[JSX_COMMENT_OPEN.len()..core.len() - JSX_COMMENT_CLOSE.len()];
                    let lead_space = usize::from(inner.starts_with(' '));
                    let inner = &inner[lead_space..];
                    let inner = inner.strip_suffix(' ').unwrap_or(inner);
                    self.lines[r] = format!("{}{inner}", &line[..off]);
                    let removed = JSX_COMMENT_OPEN.chars().count() + lead_space;
                    edits.push((r, line[..off].chars().count(), -(removed as isize)));
                    continue;
                }
                let mut rest = line[off..].to_string();
                let len = line_comment_prefix_len(&rest, token).unwrap_or(0);
                let mut removed = rest[..len].chars().count();
                rest.drain(..len);
                if rest.starts_with(' ') {
                    rest.drain(..1);
                    removed += 1;
                }
                self.lines[r] = format!("{}{}", &line[..off], rest);
                edits.push((r, line[..off].chars().count(), -(removed as isize)));
            }
        } else {
            let marker = format!("{token} ");
            for &(start, end) in &blocks {
                let rows: Vec<usize> = non_blank
                    .iter()
                    .copied()
                    .filter(|r| (start..=end).contains(r))
                    .collect();
                let Some(min_indent) = rows
                    .iter()
                    .map(|&r| {
                        let l = &self.lines[r];
                        l.len() - l.trim_start().len()
                    })
                    .min()
                else {
                    continue;
                };
                for &r in &rows {
                    if jsx.contains(&r) {
                        // Already `{/* … */}`: wrapping it again would nest
                        // the comment, and its `*/}` would end the outer one.
                        if is_jsx_comment(self.lines[r].trim()) {
                            continue;
                        }
                        let end = self.lines[r].trim_end().len();
                        self.lines[r].truncate(end);
                        self.lines[r].push_str(&format!(" {JSX_COMMENT_CLOSE}"));
                        let open = format!("{JSX_COMMENT_OPEN} ");
                        self.lines[r].insert_str(min_indent, &open);
                        let at = self.lines[r][..min_indent].chars().count();
                        edits.push((r, at, open.chars().count() as isize));
                        continue;
                    }
                    self.lines[r].insert_str(min_indent, &marker);
                    let at = self.lines[r][..min_indent].chars().count();
                    edits.push((r, at, marker.chars().count() as isize));
                }
            }
        }
        self.remap_carets(|(r, c)| match edits.iter().find(|e| e.0 == r) {
            Some(&(_, at, delta)) => (r, shift_col(c, at, delta)),
            None => (r, c),
        });
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// This buffer's line-comment marker: its language's, or for a file
    /// croft has no grammar for, the one its name or extension uses
    /// (#1181).
    pub(super) fn line_comment_marker(&self) -> Option<&'static str> {
        match self.lang {
            // SCSS and Sass share the CSS grammar but, unlike CSS, have a
            // `//` line comment, so the file's extension decides.
            Some(LangKind::Css) | None => {
                self.path.as_deref().and_then(line_comment_token_for_path)
            }
            Some(_) => line_comment_token(self.lang),
        }
    }

    /// Whether Toggle Line Comment has any comment to use in this buffer: a
    /// line comment, or a block comment to wrap the lines in.
    pub fn has_line_comment_syntax(&self) -> bool {
        self.line_comment_marker().is_some() || block_comment_tokens(self.lang).is_some()
    }

    /// Toggle Line Comment where the language has only block comments: each
    /// block of lines is wrapped in one `open … close` pair, from its first
    /// non-blank character to its last, or unwrapped when every block
    /// already is one, as VS Code does (#1181).
    pub(super) fn toggle_lines_in_block_comment(&mut self, open: &str, close: &str) -> bool {
        // Each block's first and last non-blank rows.
        let spans: Vec<(usize, usize)> = self
            .selected_row_blocks()
            .into_iter()
            .filter_map(|(start, end)| {
                let blank = |r: &usize| self.lines[*r].trim().is_empty();
                let first = (start..=end).find(|r| !blank(r))?;
                let last = (start..=end).rev().find(|r| !blank(r))?;
                Some((first, last))
            })
            .collect();
        if spans.is_empty() {
            return false;
        }
        let commented = spans.iter().all(|&(first, last)| {
            let (head, tail) = (self.lines[first].trim_start(), self.lines[last].trim_end());
            head.starts_with(open)
                && tail.ends_with(close)
                && (first != last || head.len() >= open.len() + close.len())
        });
        self.push_undo(EditKind::ToggleComment);
        // Each edit as (row, char column, chars added or, negative, removed),
        // in the order made: a row's closing edit comes before its opening
        // one, so each column is still true when its edit is applied.
        let mut edits: Vec<(usize, usize, isize)> = Vec::with_capacity(spans.len() * 2);
        for &(first, last) in &spans {
            let line = &self.lines[last];
            let end = line.trim_end().len();
            if commented {
                let mut cut = end - close.len();
                if line[..cut].ends_with(' ') {
                    cut -= 1;
                }
                let at = line[..cut].chars().count();
                let removed = line[cut..end].chars().count();
                self.lines[last].replace_range(cut..end, "");
                edits.push((last, at, -(removed as isize)));
            } else {
                let at = line[..end].chars().count();
                let marker = format!(" {close}");
                self.lines[last].insert_str(end, &marker);
                edits.push((last, at, marker.chars().count() as isize));
            }
            let line = &self.lines[first];
            let indent = line.len() - line.trim_start().len();
            let at = line[..indent].chars().count();
            if commented {
                let mut cut = indent + open.len();
                if line[cut..].starts_with(' ') {
                    cut += 1;
                }
                let removed = line[indent..cut].chars().count();
                self.lines[first].replace_range(indent..cut, "");
                edits.push((first, at, -(removed as isize)));
            } else {
                let marker = format!("{open} ");
                self.lines[first].insert_str(indent, &marker);
                edits.push((first, at, marker.chars().count() as isize));
            }
        }
        self.remap_carets(|(r, c)| {
            let c = edits
                .iter()
                .filter(|e| e.0 == r)
                .fold(c, |c, &(_, at, delta)| shift_col(c, at, delta));
            (r, c)
        });
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// VS Code "Toggle Block Comment" (Shift+Alt+A): wrap the selection (or,
    /// with no selection, the current line) in the language's block-comment
    /// delimiters, or strip them when the selection already is a block
    /// comment. Every extra caret's selection or line is done the same way,
    /// each on its own. Returns false for languages with no block comment
    /// (e.g. YAML).
    pub fn toggle_block_comment(&mut self) -> bool {
        let Some(tokens) = block_comment_tokens(self.lang) else {
            return false;
        };
        let last = self.lines.len().saturating_sub(1);
        let mut ranges: Vec<(BPos, BPos, bool)> = self
            .selections_with_primary()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let (start, end) = if s.has_area() {
                    s.normalised()
                } else {
                    let row = s.head.0.min(last);
                    ((row, 0), (row, self.line_char_len(row)))
                };
                (start, end, i == 0)
            })
            .collect();
        // Two carets on one line comment it once.
        ranges.sort_unstable();
        let mut merged: Vec<(BPos, BPos, bool)> = Vec::new();
        for (start, end, primary) in ranges {
            match merged.last_mut() {
                Some(m) if start < m.1 || start == m.0 => {
                    m.1 = m.1.max(end);
                    m.2 |= primary;
                }
                _ => merged.push((start, end, primary)),
            }
        }
        if merged
            .iter()
            .all(|&(start, end, _)| self.char_range_text(start, end).trim().is_empty())
        {
            return false;
        }
        self.push_undo(EditKind::ToggleComment);
        // A range starting in JSX children is wrapped `{/* … */}`, since a
        // bare `/* */` there is text (#1623). Read before any edit.
        let jsx_starts: Vec<(usize, usize)> = merged
            .iter()
            .map(|&(start, end, _)| {
                let text = self.char_range_text(start, end);
                let lead = text
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect::<String>();
                text_end(start, &lead)
            })
            .collect();
        let standalone: Vec<bool> = merged
            .iter()
            .map(|&(start, end, _)| is_jsx_comment(self.char_range_text(start, end).trim()))
            .collect();
        let jsx = self.jsx_children_at(&jsx_starts, &standalone);
        // From the bottom up, so an edit never moves a range still to come.
        let mut placed: Vec<(bool, (usize, usize))> = Vec::with_capacity(merged.len());
        for (i, &(start, end, primary)) in merged.iter().enumerate().rev() {
            let (open, close) = if jsx[i] {
                (JSX_COMMENT_OPEN, JSX_COMMENT_CLOSE)
            } else {
                tokens
            };
            let text = self.char_range_text(start, end);
            let trimmed = text.trim();
            let caret = if trimmed.is_empty() {
                end
            } else {
                let wrapped = trimmed.starts_with(open)
                    && trimmed.ends_with(close)
                    && trimmed.len() >= open.len() + close.len();
                let new = if wrapped {
                    trimmed[open.len()..trimmed.len() - close.len()]
                        .trim()
                        .to_string()
                } else {
                    format!("{open} {trimmed} {close}")
                };
                // Only the trimmed core is replaced: the whitespace around it
                // (the line's indent, and the line break a whole-line
                // selection ends with) stays as it was. Replacing the whole
                // range joined the next line into this one and dropped the
                // indent.
                let lead = &text[..text.len() - text.trim_start().len()];
                let trail = &text[text.trim_end().len()..];
                let before = format!("{lead}{new}");
                self.replace_char_range(start, end, &format!("{before}{trail}"));
                // The caret lands just after the (un)commented text.
                text_end(start, &before)
            };
            placed.push((primary, self.pin_from_end(caret)));
        }
        self.place_pinned_carets(&placed);
        self.mark_buffer_changed();
        self.recompute_highlights();
        self.ensure_cursor_col_visible();
        true
    }

    /// The rows among `rows` whose first non-blank character sits in JSX
    /// children (#1623).
    pub(super) fn jsx_child_rows(&self, rows: &[usize]) -> std::collections::HashSet<usize> {
        let points: Vec<(usize, usize)> = rows
            .iter()
            .map(|&r| {
                let line = &self.lines[r];
                (r, line.chars().take_while(|c| c.is_whitespace()).count())
            })
            .collect();
        let standalone: Vec<bool> = rows
            .iter()
            .map(|&r| is_jsx_comment(self.lines[r].trim()))
            .collect();
        rows.iter()
            .zip(self.jsx_children_at(&points, &standalone))
            .filter_map(|(&r, jsx)| jsx.then_some(r))
            .collect()
    }

    /// Whether each of `points` (char positions) starts something in JSX
    /// children, an element, a closing tag, text or a `{…}`, where a
    /// comment must be written `{/* … */}` (#1623). Always false outside
    /// the JSX-capable grammars, and on a line of plain code or attributes.
    ///
    /// Where `standalone[i]` says the text at `points[i]` is one whole
    /// `{/* … */}`, that point also counts when it sits in JSX or in a parse
    /// error: commenting out a closing tag leaves the markup unparseable, and
    /// a second toggle must still take the comment off. In well-formed plain
    /// code such a line is a block holding a comment, and stays code.
    pub(super) fn jsx_children_at(
        &self,
        points: &[(usize, usize)],
        standalone: &[bool],
    ) -> Vec<bool> {
        let none = vec![false; points.len()];
        if !matches!(self.lang, Some(LangKind::JavaScript) | Some(LangKind::Tsx)) {
            return none;
        }
        let Some(lang) = self.lang.map(crate::highlight::language_for) else {
            return none;
        };
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&lang).is_err() {
            return none;
        }
        let Some(tree) = parser.parse(self.lines.join("\n"), None) else {
            return none;
        };
        points
            .iter()
            .zip(standalone)
            .map(|(&p, &standalone)| {
                let byte = self.char_pos_to_byte(p);
                jsx_child_starts_at(tree.root_node(), byte)
                    || standalone && in_jsx_or_error(tree.root_node(), byte)
            })
            .collect()
    }
}

/// How a comment is written in JSX children (#1623), where `//` and a bare
/// `/* */` are text that React renders.
pub(super) const JSX_COMMENT_OPEN: &str = "{/*";

/// The close of a JSX comment, matching [`JSX_COMMENT_OPEN`].
pub(super) const JSX_COMMENT_CLOSE: &str = "*/}";

/// Whether the trimmed line is one `{/* … */}` comment.
pub(super) fn is_jsx_comment(trimmed: &str) -> bool {
    trimmed.len() >= JSX_COMMENT_OPEN.len() + JSX_COMMENT_CLOSE.len()
        && trimmed.starts_with(JSX_COMMENT_OPEN)
        && trimmed.ends_with(JSX_COMMENT_CLOSE)
}

/// Whether the text at `byte` is JSX children: inside a JSX element's text,
/// or at the start of a child element, closing tag or `{…}` expression.
/// A position in an opening tag's attributes, or in the code inside a
/// `{…}`, is not.
pub(super) fn jsx_child_starts_at(root: tree_sitter::Node, byte: usize) -> bool {
    let Some(mut node) = root.descendant_for_byte_range(byte, byte) else {
        return false;
    };
    loop {
        let Some(parent) = node.parent() else {
            return false;
        };
        if matches!(parent.kind(), "jsx_element" | "jsx_fragment") {
            match node.kind() {
                "jsx_text" => return true,
                "jsx_element"
                | "jsx_self_closing_element"
                | "jsx_expression"
                | "jsx_closing_element" => return node.start_byte() == byte,
                // The element's own opening tag: whether the element is
                // a child is a question for the element's parent.
                "jsx_opening_element" => {}
                _ => return false,
            }
        }
        node = parent;
    }
}

/// Whether the node at `byte`, or any node around it, is JSX or a parse
/// error.
pub(super) fn in_jsx_or_error(root: tree_sitter::Node, byte: usize) -> bool {
    let mut node = root.descendant_for_byte_range(byte, byte);
    while let Some(n) = node {
        if n.is_error() || n.kind().starts_with("jsx") {
            return true;
        }
        node = n.parent();
    }
    false
}

/// The line-comment marker for a language (VS Code `lineComment`), or `None`
/// for languages with no line comment, in which case Toggle Line Comment is a
/// no-op.
pub(super) fn line_comment_token(lang: Option<LangKind>) -> Option<&'static str> {
    match lang {
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Go)
        | Some(LangKind::C)
        | Some(LangKind::Cpp)
        | Some(LangKind::Ql)
        | Some(LangKind::Dbscheme) => Some("//"),
        Some(LangKind::Python)
        | Some(LangKind::Yaml)
        | Some(LangKind::Toml)
        | Some(LangKind::Bash)
        | Some(LangKind::Ruby)
        | Some(LangKind::Make)
        | Some(LangKind::Dockerfile) => Some("#"),
        Some(LangKind::Java) | Some(LangKind::CSharp) => Some("//"),
        Some(LangKind::Lua) | Some(LangKind::Sql) => Some("--"),
        // JSONC, which croft's and VS Code's own settings files are: VS
        // Code's JSON language comments with `//` too.
        Some(LangKind::Json) => Some("//"),
        _ => None,
    }
}

/// The line comment of a file croft has no grammar for, by its file name or
/// extension, as VS Code's language configurations give them (#1181).
pub(super) fn line_comment_token_for_path(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if name.starts_with(".env")
        || matches!(
            name,
            ".gitignore"
                | ".dockerignore"
                | ".npmignore"
                | ".prettierignore"
                | ".gitattributes"
                | ".editorconfig"
                | "CODEOWNERS"
        )
    {
        return Some("#");
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "kt" | "kts" | "swift" | "scala" | "sc" | "dart" | "php" | "groovy" | "gradle"
        | "jsonc" | "json5" | "proto" | "zig" | "v" | "sv" | "fs" | "fsx" | "scss" | "sass"
        | "less" => Some("//"),
        "pl" | "pm" | "r" | "ps1" | "psm1" | "mk" | "conf" | "cfg" | "properties" | "env"
        | "tf" | "hcl" | "nix" | "jl" | "ex" | "exs" | "cmake" | "gitignore" | "zsh" | "fish"
        | "awk" | "tcl" | "nim" | "cr" | "graphql" | "gql" => Some("#"),
        "hs" | "elm" | "ada" | "adb" | "ads" | "vhd" | "vhdl" => Some("--"),
        "ini" | "clj" | "cljs" | "edn" | "lisp" | "el" | "scm" | "rkt" | "asm" => Some(";"),
        "tex" | "sty" | "erl" | "hrl" => Some("%"),
        "vim" => Some("\""),
        "bat" | "cmd" => Some("@REM"),
        _ => None,
    }
}

/// The byte length of the line comment `token` that `text` (a line with its
/// indentation trimmed) starts with, or `None` when it is not commented. A
/// word token (batch's `@REM`) must end at whitespace or the line's end, so
/// `REMOTE.EXE` is not a comment, and batch's plain `REM` counts too.
pub(super) fn line_comment_prefix_len(text: &str, token: &str) -> Option<usize> {
    let word = |t: &str| {
        text.starts_with(t)
            && text[t.len()..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
    };
    if !token.ends_with(|c: char| c.is_ascii_alphabetic()) {
        return text.starts_with(token).then_some(token.len());
    }
    if word(token) {
        return Some(token.len());
    }
    let plain = token.trim_start_matches('@');
    (plain != token && word(plain)).then_some(plain.len())
}

/// The block-comment delimiters for a language (VS Code `blockComment`), or
/// `None` for languages without one (e.g. YAML), in which case Toggle Block
/// Comment is a no-op. Python has no real block comment, but VS Code maps the
/// command onto a triple-quoted string, so it returns `""" """`.
pub(super) fn block_comment_tokens(lang: Option<LangKind>) -> Option<(&'static str, &'static str)> {
    match lang {
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Go)
        | Some(LangKind::C)
        | Some(LangKind::Cpp)
        | Some(LangKind::Css)
        | Some(LangKind::Ql)
        | Some(LangKind::Dbscheme) => Some(("/*", "*/")),
        Some(LangKind::Html)
        | Some(LangKind::Markdown)
        | Some(LangKind::Xml)
        | Some(LangKind::Vue)
        | Some(LangKind::Svelte) => Some(("<!--", "-->")),
        Some(LangKind::Lua) => Some(("--[[", "]]")),
        Some(LangKind::Java) | Some(LangKind::CSharp) | Some(LangKind::Sql) => Some(("/*", "*/")),
        // Python has no true block comment; VS Code's language config maps
        // Toggle Block Comment onto a triple-quoted string (`""" """`).
        Some(LangKind::Python) => Some(("\"\"\"", "\"\"\"")),
        _ => None,
    }
}
