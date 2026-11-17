//! What Enter does: auto-indent, continuing comments and lists, the per-language Enter
//! rules (Markdown, YAML and others), and re-indenting a paste.

use super::*;

impl Editor {
    pub(super) fn insert_newline_raw(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        let right = self.lines[row].split_off(byte);
        self.lines.insert(row + 1, right);
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.mark_buffer_changed();
    }

    pub fn insert_newline(&mut self) {
        // Enter again on a line Enter only indented leaves that line blank
        // (#956): the indentation moves down with the caret, and the clear
        // is part of this Enter's undo step.
        let blank = self
            .auto_indent
            .take()
            .filter(|&(row, len, seq)| {
                seq == self.edit_seq
                    && row == self.cursor_row
                    && self.selection.is_none()
                    && self.is_auto_indent_line(row, len)
            })
            .map(|(row, ..)| row);
        self.pin_on_edit();
        self.push_undo(EditKind::Newline);
        self.delete_selection_inner();
        self.smart_insert_newline_inner();
        if let Some(row) = blank
            && row < self.cursor_row
            && self.lines[row].chars().all(|c| c == ' ' || c == '\t')
            && !self.lines[row].is_empty()
        {
            self.lines[row].clear();
            self.remap_carets(|p| p);
            self.mark_buffer_changed();
        }
        self.note_auto_indent();
        self.recompute_highlights();
        // Newline is an on-type trigger for several servers (#254).
        self.last_typed = Some(('\n', self.edit_seq));
    }

    /// Remember the caret's line when Enter left it holding nothing but the
    /// indentation it inserted, caret at its end (#956).
    pub(super) fn note_auto_indent(&mut self) {
        let row = self.cursor_row;
        self.auto_indent = self.lines.get(row).and_then(|line| {
            let len = line.chars().count();
            (len > 0 && self.cursor_col == len && line.chars().all(|c| c == ' ' || c == '\t'))
                .then_some((row, len, self.edit_seq))
        });
    }

    /// Whether `row` still holds exactly the `len` characters of
    /// indentation Enter gave it and nothing else.
    pub(super) fn is_auto_indent_line(&self, row: usize, len: usize) -> bool {
        self.lines.get(row).is_some_and(|line| {
            line.chars().count() == len && line.chars().all(|c| c == ' ' || c == '\t')
        })
    }

    /// Clear the indentation Enter put on a line the caret has since left
    /// with nothing typed on it (#956), so a blank line between statements
    /// is saved blank rather than as `"    "` that every linter flags. Called
    /// after each key and click, with `tab_left` set for every tab but the
    /// one being typed in: switching away from a tab leaves its line too.
    /// It takes no undo step of its own: the step on top is still the one
    /// before the Enter, so one Ctrl+Z still undoes the Enter.
    pub fn trim_auto_indent(&mut self, tab_left: bool) {
        let Some((row, len, seq)) = self.auto_indent else {
            return;
        };
        if seq == self.edit_seq && row == self.cursor_row && !tab_left {
            return;
        }
        self.auto_indent = None;
        // Anything typed or edited since, here or elsewhere, keeps the line.
        if seq != self.edit_seq || !self.is_auto_indent_line(row, len) {
            return;
        }
        self.lines[row].clear();
        // A selection end or an extra caret on the line stays on it, at
        // its new end, not past it.
        self.remap_carets(|p| p);
        let pos = self.last_edit_pos;
        self.mark_buffer_changed();
        self.last_edit_pos = pos;
        self.recompute_highlights();
    }

    pub(super) fn smart_insert_newline_inner(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        if let Some(rule) = on_enter_rule(self.lang, &self.lines, row, self.byte_index(row, col)) {
            self.apply_on_enter(rule);
            return;
        }
        let line = self.lines[row].clone();

        // Inside a doc comment the new line continues it (#1443).
        let below = self.lines.get(row + 1..).unwrap_or(&[]);
        if let Some(enter) = comment_on_enter(self.lang, &line, col, below) {
            self.continue_comment(enter);
            return;
        }

        let leading: String = line
            .chars()
            .take(col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();

        let prefix_chars: Vec<char> = line.chars().take(col).collect();
        let last_non_ws = prefix_chars
            .iter()
            .rev()
            .find(|c| !c.is_whitespace())
            .copied();
        let next_char = line.chars().nth(col);

        let unit = self.indent_unit();
        let extra = if extra_indent_triggered(self.lang, last_non_ws) {
            unit.as_str()
        } else {
            ""
        };
        let pair_split = is_bracket_pair_split(self.lang, last_non_ws, next_char);

        self.insert_newline_raw();

        let new_indent = if self.lang == Some(LangKind::Yaml) {
            let prefix: String = prefix_chars.iter().collect();
            yaml_newline_indent(&prefix, &unit)
        } else {
            format!("{leading}{extra}")
        };
        for c in new_indent.chars() {
            self.insert_char_raw(c);
        }

        if pair_split {
            self.insert_newline_raw();
            for c in leading.chars() {
                self.insert_char_raw(c);
            }
            self.cursor_row -= 1;
            self.cursor_col = new_indent.chars().count();
        }
    }

    /// Carry out a Markdown Enter rule (#1495). The text after
    /// the caret moves to the new line behind the continuation, without the
    /// blanks that separated it from the caret.
    pub(super) fn apply_on_enter(&mut self, rule: OnEnter) {
        let row = self.cursor_row;
        match rule {
            OnEnter::EndList => {
                self.lines[row].clear();
                self.cursor_col = 0;
                self.mark_buffer_changed();
            }
            OnEnter::Continue(text) => {
                self.insert_newline_raw();
                let rest = self.lines[row + 1]
                    .trim_start_matches([' ', '\t'])
                    .to_string();
                self.lines[row + 1] = format!("{text}{rest}");
                self.cursor_col = text.chars().count();
                self.mark_buffer_changed();
            }
        }
    }

    /// Break the line at the caret into a comment's next line (#1443): the
    /// new line starts with `enter.start`, and a `/**` just opened is closed
    /// on a line below, the caret staying on the line between.
    pub(super) fn continue_comment(&mut self, enter: CommentEnter) {
        let start_len = enter.start.chars().count();
        if let CommentBelow::MoveCloser(_) = &enter.below {
            // The `*/` right after the caret goes to the closing line, so
            // the space before it goes with the break.
            while self.lines[self.cursor_row]
                .chars()
                .nth(self.cursor_col)
                .is_some_and(|c| c == ' ' || c == '\t')
            {
                self.delete_forward_raw();
            }
        }
        self.insert_newline_raw();
        for c in enter.start.chars() {
            self.insert_char_raw(c);
        }
        match enter.below {
            CommentBelow::Nothing => {}
            CommentBelow::MoveCloser(indent) => {
                self.insert_newline_raw();
                for c in indent.chars() {
                    self.insert_char_raw(c);
                }
                self.cursor_row -= 1;
                self.cursor_col = start_len;
            }
            CommentBelow::AddCloser(closer) => {
                let (row, col) = (self.cursor_row, self.cursor_col);
                self.cursor_col = self.line_char_len(row);
                self.insert_newline_raw();
                for c in closer.chars() {
                    self.insert_char_raw(c);
                }
                self.cursor_row = row;
                self.cursor_col = col;
            }
        }
    }
}

/// Number of leading whitespace bytes to strip for one outdent step, matching
/// VS Code's tab-stop-aligned `unshiftIndent`. A leading tab counts as one
/// level (strip it). For spaces, strip back to the previous tab stop: with a
/// width-4 unit, 4 -> remove 4, 6 -> remove 2, 3 -> remove 3, 1 -> remove 1.
/// All leading whitespace here is ASCII (`' '` / `'\t'`), so byte count equals
/// char count.
pub(super) fn leading_outdent_chars(line: &str, unit_w: usize) -> usize {
    if unit_w == 0 {
        return 0;
    }
    if line.starts_with('\t') {
        return 1;
    }
    let spaces = line.chars().take_while(|c| *c == ' ').count();
    if spaces == 0 {
        0
    } else {
        ((spaces - 1) % unit_w) + 1
    }
}

/// What Enter types inside a doc comment (#1443), after VS Code's
/// `onEnterRules`.
#[derive(Debug, PartialEq)]
pub(super) struct CommentEnter {
    /// The new line's text, ahead of whatever followed the caret.
    start: String,
    /// What happens on the line below the new one.
    below: CommentBelow,
}

#[derive(Debug, PartialEq)]
pub(super) enum CommentBelow {
    Nothing,
    /// A `/**` was just opened with nothing closing it: this line closes it.
    AddCloser(String),
    /// The caret sat right before the `*/` of `/** */`: that `*/` moves to
    /// a line of its own, indented by this.
    MoveCloser(String),
}

/// The languages whose doc comments are `/** … */` blocks.
pub(super) fn has_block_doc_comments(lang: Option<LangKind>) -> bool {
    matches!(
        lang,
        Some(
            LangKind::JavaScript
                | LangKind::TypeScript
                | LangKind::Tsx
                | LangKind::C
                | LangKind::Cpp
                | LangKind::Java
                | LangKind::CSharp
                | LangKind::Rust
        )
    )
}

/// How Enter at char `col` of `line` continues a doc comment (#1443), or
/// None where it is a plain line break:
/// - `/**` opening a block gives ` * `, closing the block below when
///   nothing does yet;
/// - a ` * …` line inside the block gives the next ` * ` at the same
///   column;
/// - a ` */` line steps back to the column of its `/**`;
/// - in Rust, a `///` or `//!` line gives the next `/// ` or `//! `.
///
/// A plain `//` comment, as in VS Code, is not continued.
pub(super) fn comment_on_enter(
    lang: Option<LangKind>,
    line: &str,
    col: usize,
    below: &[String],
) -> Option<CommentEnter> {
    let before: String = line.chars().take(col).collect();
    let after: String = line.chars().skip(col).collect();
    let indent: String = before
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let body = &before[indent.len()..];
    let nothing = |start: String| {
        Some(CommentEnter {
            start,
            below: CommentBelow::Nothing,
        })
    };
    if lang == Some(LangKind::Rust) && !body.starts_with("////") {
        for intro in ["///", "//!"] {
            if body.starts_with(intro) {
                return nothing(format!("{indent}{intro} "));
            }
        }
    }
    if !has_block_doc_comments(lang) {
        return None;
    }
    let closer_next = after.trim_start().starts_with("*/");
    if let Some(rest) = body.strip_prefix("/**")
        && !rest.starts_with('/')
        && !rest.contains("*/")
    {
        let below = if closer_next {
            CommentBelow::MoveCloser(format!("{indent} "))
        } else if after.contains("*/") || block_closed_below(below) {
            // Already closed, on this line or further down.
            CommentBelow::Nothing
        } else {
            CommentBelow::AddCloser(format!("{indent} */"))
        };
        return Some(CommentEnter {
            start: format!("{indent} * "),
            below,
        });
    }
    // Only a `*` set in from its line's start is a block's continuation, as
    // VS Code's rule has it.
    if !indent.ends_with(' ') {
        return None;
    }
    if let Some(rest) = body.strip_prefix('*')
        && (rest.is_empty() || rest.starts_with(' '))
        && !rest.contains("*/")
        && !closer_next
    {
        return nothing(format!("{indent}* "));
    }
    if body.trim_end() == "*/" && after.trim().is_empty() {
        return nothing(indent[..indent.len() - 1].to_string());
    }
    None
}

/// Whether the lines below an opened block comment close it: a `*/`
/// comes before any further `/*` would open another block.
pub(super) fn block_closed_below(below: &[String]) -> bool {
    for line in below {
        match (line.find("/*"), line.find("*/")) {
            (open, Some(close)) if open.is_none_or(|open| close < open) => return true,
            (Some(_), _) => return false,
            _ => {}
        }
    }
    false
}

pub(super) fn extra_indent_triggered(lang: Option<LangKind>, last_non_ws: Option<char>) -> bool {
    let last = match last_non_ws {
        Some(c) => c,
        None => return false,
    };
    match lang {
        Some(LangKind::Python) => matches!(last, ':' | '(' | '[' | '{'),
        Some(LangKind::Rust)
        | Some(LangKind::JavaScript)
        | Some(LangKind::TypeScript)
        | Some(LangKind::Tsx)
        | Some(LangKind::Json)
        | Some(LangKind::Go)
        | Some(LangKind::Css)
        | Some(LangKind::Lua)
        | Some(LangKind::Ql)
        | Some(LangKind::Java)
        | Some(LangKind::CSharp) => matches!(last, '(' | '[' | '{'),
        _ => false,
    }
}

/// What Enter does on a line a per-language rule recognises, in place of
/// copying the indent (#1495), as the Markdown editors people come from do.
/// Doc comments (#1443) go through `comment_on_enter`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OnEnter {
    /// Open a new line starting with this text: the indent plus the list
    /// marker or quote that continues the line.
    Continue(String),
    /// Enter on a list item or quote that holds only its marker: clear the
    /// marker, which ends the list.
    EndList,
}

/// The Enter rule for the caret at byte `col` of `lines[row]`, if any.
pub(super) fn on_enter_rule(
    lang: Option<LangKind>,
    lines: &[String],
    row: usize,
    col: usize,
) -> Option<OnEnter> {
    let line = lines.get(row)?;
    match lang? {
        LangKind::Markdown if !in_markdown_fence(lines, row) => markdown_on_enter(line, col),
        _ => None,
    }
}

/// True when `row` sits inside a fenced code block opened above it. A fence
/// opens with three or more `` ` `` or `~` and closes only on a line of the
/// same character, at least as long, with nothing after it.
pub(super) fn in_markdown_fence(lines: &[String], row: usize) -> bool {
    let mut open: Option<(char, usize)> = None;
    for l in &lines[..row] {
        let t = l.trim_start();
        let Some(ch) = t.chars().next().filter(|c| matches!(c, '`' | '~')) else {
            continue;
        };
        let width = t.len() - t.trim_start_matches(ch).len();
        if width < 3 {
            continue;
        }
        match open {
            None => open = Some((ch, width)),
            Some((c, w)) if c == ch && width >= w && t[width..].trim().is_empty() => open = None,
            Some(_) => {}
        }
    }
    open.is_some()
}

/// Enter on a Markdown list item, task item or blockquote (#1495): the next
/// line repeats the indent, the `>` quotes and the marker, with an ordered
/// number counted on and a task box left unticked. An item that is only its
/// marker ends the list instead. A caret inside the marker, a thematic break
/// (`* * *`) or emphasis (`**bold**`) is no list.
pub(super) fn markdown_on_enter(line: &str, col: usize) -> Option<OnEnter> {
    let trimmed = line.trim();
    if trimmed.len() >= 3
        && let Some(first) = trimmed.chars().next()
        && matches!(first, '-' | '*' | '_')
        && trimmed.chars().all(|c| c == first || c == ' ' || c == '\t')
    {
        return None;
    }
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let mut i = indent;
    let mut quote = String::new();
    while line[i..].starts_with('>') {
        quote.push('>');
        i += 1;
        if line[i..].starts_with(' ') {
            quote.push(' ');
            i += 1;
        }
    }
    let spaced = |at: usize| line[at..].starts_with([' ', '\t']);
    let rest = &line[i..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let mut marker = None;
    if rest.starts_with(['-', '*', '+']) && spaced(i + 1) {
        marker = Some(rest[..1].to_string());
        i += 1;
    } else if (1..=9).contains(&digits)
        && rest[digits..].starts_with(['.', ')'])
        && spaced(i + digits + 1)
    {
        let n: u64 = rest[..digits].parse().ok()?;
        marker = Some(format!("{}{}", n + 1, &rest[digits..=digits]));
        i += digits + 1;
    }
    let mut next = format!("{}{quote}", &line[..indent]);
    if let Some(marker) = marker {
        let gap = line[i..].len() - line[i..].trim_start_matches([' ', '\t']).len();
        next.push_str(&marker);
        next.push_str(&line[i..i + gap]);
        i += gap;
        let after = &line[i..];
        if matches!(after.get(..3), Some("[ ]" | "[x]" | "[X]"))
            && (after.len() == 3 || spaced(i + 3))
        {
            next.push_str("[ ] ");
            i += 3;
            i += line[i..].len() - line[i..].trim_start_matches([' ', '\t']).len();
        }
    } else if quote.is_empty() {
        return None;
    }
    if col < i.min(line.trim_end().len()) {
        return None;
    }
    if line[i.min(line.len())..].trim().is_empty() {
        return Some(OnEnter::EndList);
    }
    Some(OnEnter::Continue(next))
}

/// The indent for the line Enter opens after `prefix` in YAML (#1309).
/// A key with no value (`services:`), a block scalar (`run: |`) or an open
/// flow collection (`args: [`) starts a nested block one `unit` deeper. In a
/// list item (`- name: x`) the base column is the item's content, so the
/// next key lines up with `name`; anything else keeps the line's indent.
pub(super) fn yaml_newline_indent(prefix: &str, unit: &str) -> String {
    let leading: String = prefix
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let mut content = &prefix[leading.len()..];
    let mut base = leading.clone();
    while let Some(rest) = content.strip_prefix('-')
        && (rest.is_empty() || rest.starts_with(' '))
    {
        let after = rest.trim_start_matches(' ');
        base.push_str(&" ".repeat(content.len() - after.len()));
        content = after;
    }
    let content = yaml_strip_comment(content).trim_end();
    let Some(colon) = yaml_key_colon(content) else {
        return leading;
    };
    let mut value = content[colon + 1..].trim();
    // An anchor or tag (`&defaults`, `!!map`) comes before the node it names,
    // so `.defaults: &defaults` still opens the mapping below it, as shared
    // blocks in GitLab CI, compose and Helm values are written.
    while let Some(prop) = value.split_whitespace().next()
        && prop.starts_with(['&', '!'])
    {
        value = value[prop.len()..].trim_start();
    }
    let opens_block = value.is_empty()
        || value.ends_with('[')
        || value.ends_with('{')
        || (value.starts_with(['|', '>'])
            && value[1..]
                .chars()
                .all(|c| matches!(c, '-' | '+') || c.is_ascii_digit()));
    if opens_block {
        format!("{base}{unit}")
    } else {
        base
    }
}

/// `line` up to a `#` comment: a `#` at the start or after whitespace,
/// outside quotes.
pub(super) fn yaml_strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut prev_ws = true;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && prev_ws => return &line[..i],
            None => {}
        }
        prev_ws = c.is_whitespace();
    }
    line
}

/// Byte offset of the `:` that ends a mapping key in `content`: the first
/// one outside quotes followed by a space or the end of the line.
pub(super) fn yaml_key_colon(content: &str) -> Option<usize> {
    let mut quote = None;
    let mut chars = content.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if (c == '\'' || c == '"') && i == 0 => quote = Some(c),
            None if c == ':' && chars.peek().is_none_or(|(_, n)| n.is_whitespace()) => {
                return Some(i);
            }
            None => {}
        }
    }
    None
}

pub(super) fn is_bracket_pair_split(
    lang: Option<LangKind>,
    prev: Option<char>,
    next: Option<char>,
) -> bool {
    let bracket_aware = matches!(
        lang,
        Some(LangKind::Rust)
            | Some(LangKind::Python)
            | Some(LangKind::JavaScript)
            | Some(LangKind::TypeScript)
            | Some(LangKind::Tsx)
            | Some(LangKind::Json)
            | Some(LangKind::Go)
            | Some(LangKind::Css)
            | Some(LangKind::Lua)
            | Some(LangKind::Ql)
            | Some(LangKind::Java)
            | Some(LangKind::CSharp)
    );
    if !bracket_aware {
        return false;
    }
    matches!(
        (prev, next),
        (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
    )
}

/// Shift highlight spans left by `byte_start`, dropping spans that fall
/// entirely before the cut and clamping spans straddling the cut.
/// Split file text into buffer lines the way the LSP spec / VS Code / Zed do:
/// `\r\n`, a lone `\r`, and `\n` are all line terminators. Rust's `str::lines`
/// only breaks on `\n` (stripping a trailing `\r`), so a stray lone `\r` would
/// leave croft's line count one short of the language server's. Every LSP
/// position past that `\r` (semantic tokens, diagnostics, hover, definition)
/// would then resolve one row off. Normalizing first keeps the two in lockstep
/// and is a no-op for clean `\n`-only files.
pub(crate) fn split_into_lines(text: &str) -> Vec<String> {
    // Most text has no `\r` at all; skip the two whole-text copies that
    // normalising would make (#371 splits a 35k-line file per scrub step).
    if !text.contains('\r') {
        return text.lines().map(str::to_string).collect();
    }
    normalize_newlines(text)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

/// The edits that re-indent a paste of `text` made at `(row, col)` (#1218):
/// every later line moves under `base` (the caret line's indentation) by its
/// indentation relative to the least-indented line, written in the buffer's
/// style. The first line counts toward that least indentation when the paste
/// starts in the caret line's leading whitespace, and then keeps only its
/// indentation beyond that least (the caret's column already indents it). Lines that are
/// only whitespace are emptied; a final segment that is only whitespace is
/// left alone, as the rest of the caret's line follows it.
pub(super) fn paste_reindent_edits(
    text: &str,
    row: usize,
    col: usize,
    base: &str,
    at_indent: bool,
    tab: u32,
    spaces: bool,
) -> Vec<TextSpanEdit> {
    let tab = tab.max(1) as usize;
    let segs: Vec<&str> = text.split('\n').collect();
    let n = segs.len() - 1;
    let ws_of = |l: &str| -> (usize, usize) {
        let mut chars = 0;
        let mut width = 0;
        for c in l.chars() {
            match c {
                ' ' => width += 1,
                '\t' => width += tab - width % tab,
                _ => break,
            }
            chars += 1;
        }
        (chars, width)
    };
    let blank = |l: &str| l.trim().is_empty();
    let counted = segs
        .iter()
        .enumerate()
        .filter(|(i, l)| (*i > 0 || at_indent) && !blank(l))
        .map(|(_, l)| ws_of(l).1);
    let Some(least) = counted.min() else {
        return Vec::new();
    };
    let render = |width: usize| match spaces {
        true => " ".repeat(width),
        false => format!("{}{}", "\t".repeat(width / tab), " ".repeat(width % tab)),
    };
    let edit = |r: usize, from: usize, to: usize, new_text: String| TextSpanEdit {
        start: (r, from),
        end: (r, to),
        new_text,
        utf16: false,
    };
    let mut edits = Vec::new();
    let (first_chars, first_width) = ws_of(segs[0]);
    if at_indent && first_chars > 0 {
        let keep = match blank(segs[0]) {
            true => String::new(),
            false => render(first_width - least),
        };
        if segs[0].chars().take(first_chars).collect::<String>() != keep {
            edits.push(edit(row, col, col + first_chars, keep));
        }
    }
    for (i, seg) in segs.iter().enumerate().skip(1) {
        let (chars, width) = ws_of(seg);
        let new_ws = match blank(seg) {
            true if i == n => continue,
            true => String::new(),
            false => format!("{base}{}", render(width - least)),
        };
        let whole = if blank(seg) {
            seg.chars().count()
        } else {
            chars
        };
        if seg.chars().take(whole).collect::<String>() != new_ws {
            edits.push(edit(row + i, 0, whole, new_ws));
        }
    }
    edits
}
