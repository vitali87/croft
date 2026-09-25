//! Inline completions (#607): after a pause in typing, ask the navigator's
//! model to fill in the middle at the caret, and show its answer as ghost
//! text that `Tab` accepts and any other key discards.
//!
//! Pure: the prompt the model gets, what of its answer is kept, and the
//! request generation that makes a late answer for an old caret harmless.
//! The caller owns the transport and the painting.

/// How many lines around the caret the model sees.
pub const BEFORE_LINES: usize = 60;
pub const AFTER_LINES: usize = 20;
/// The longest completion kept, in lines.
pub const MAX_LINES: usize = 12;

/// The text around the caret: everything the model sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub file: String,
    /// Up to the caret, the caret's line included up to its column.
    pub prefix: String,
    /// From the caret on.
    pub suffix: String,
}

/// The window of `lines` around the caret at (`row`, `col`), `col` in
/// characters.
pub fn context(file: &str, lines: &[String], row: usize, col: usize) -> Context {
    let here = lines.get(row).map(String::as_str).unwrap_or("");
    let split = here
        .char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(here.len());
    let mut prefix = String::new();
    for l in &lines[row.saturating_sub(BEFORE_LINES)..row.min(lines.len())] {
        prefix.push_str(l);
        prefix.push('\n');
    }
    prefix.push_str(&here[..split]);
    let mut suffix = here[split..].to_string();
    for l in lines.iter().skip(row + 1).take(AFTER_LINES) {
        suffix.push('\n');
        suffix.push_str(l);
    }
    Context {
        file: file.to_string(),
        prefix,
        suffix,
    }
}

/// The system prompt: fill in the middle, code only.
pub const SYSTEM: &str = "You complete code at a cursor. Reply with ONLY the text to insert at <CURSOR>: no explanation, no code fences, nothing already before or after the cursor. Reply with nothing when no completion is useful.";

/// The user message for `ctx`.
pub fn prompt(ctx: &Context) -> String {
    format!("File: {}\n\n{}<CURSOR>{}", ctx.file, ctx.prefix, ctx.suffix)
}

/// What of the model's `answer` to insert: code fences stripped, a repeat
/// of the line's text before the caret dropped, cut where it starts
/// repeating the text after the caret, and at most [`MAX_LINES`] lines.
/// `None` when nothing is left.
pub fn clean(answer: &str, ctx: &Context) -> Option<String> {
    let mut text = answer.trim_matches('\n').to_string();
    if text.trim_start().starts_with("```") {
        text = text
            .split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default();
    }
    if let Some(stripped) = text.trim_end().strip_suffix("```") {
        text = stripped.to_string();
    }
    let line_so_far = ctx.prefix.rsplit('\n').next().unwrap_or("").trim_start();
    if !line_so_far.is_empty()
        && let Some(rest) = text.strip_prefix(line_so_far)
    {
        text = rest.to_string();
    }
    // Cut at the first point from which the rest is already after the caret.
    if !ctx.suffix.is_empty()
        && let Some(cut) = text
            .char_indices()
            .map(|(i, _)| i)
            .find(|&i| ctx.suffix.starts_with(&text[i..]))
    {
        text.truncate(cut);
    }
    let kept: Vec<&str> = text.trim_end().lines().take(MAX_LINES).collect();
    let text = kept.join("\n");
    (!text.trim().is_empty()).then_some(text)
}

/// Request generations: every edit or caret move bumps it, and an answer
/// is shown only if it was asked for the generation still current.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Generation(pub u64);

impl Generation {
    /// Start a new generation, making every earlier request stale.
    pub fn bump(&mut self) -> Generation {
        self.0 += 1;
        *self
    }

    /// Whether an answer asked at `asked` still applies.
    pub fn is_current(self, asked: Generation) -> bool {
        self == asked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn the_context_splits_at_the_caret_and_is_windowed() {
        let l = lines("fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}");
        let c = context("src/main.rs", &l, 1, 13);
        assert_eq!(c.prefix, "fn main() {\n    let x = 1");
        assert_eq!(c.suffix, ";\n    println!(\"{x}\");\n}");
        assert_eq!(c.file, "src/main.rs");
        let many: Vec<String> = (0..200).map(|i| format!("l{i}")).collect();
        let c = context("a", &many, 100, 2);
        assert_eq!(
            c.prefix.lines().count(),
            BEFORE_LINES + 1,
            "the window plus the caret's line"
        );
        assert!(c.prefix.starts_with(&format!("l{}", 100 - BEFORE_LINES)));
        assert_eq!(c.suffix.lines().count(), AFTER_LINES + 1);
        // A column past a multibyte character counts characters.
        let c = context("a", &lines("é = 1"), 0, 1);
        assert_eq!((c.prefix.as_str(), c.suffix.as_str()), ("é", " = 1"));
    }

    #[test]
    fn the_prompt_marks_the_cursor_between_prefix_and_suffix() {
        let c = Context {
            file: "a.rs".into(),
            prefix: "let x = ".into(),
            suffix: ";".into(),
        };
        let p = prompt(&c);
        assert!(p.contains("a.rs"), "{p}");
        assert!(p.contains("let x = <CURSOR>;"), "{p}");
    }

    fn ctx(prefix: &str, suffix: &str) -> Context {
        Context {
            file: "a.rs".into(),
            prefix: prefix.into(),
            suffix: suffix.into(),
        }
    }

    #[test]
    fn answers_are_cleaned_to_the_insert() {
        let c = ctx("    let total = ", ";\n}");
        assert_eq!(clean("items.len()", &c).as_deref(), Some("items.len()"));
        assert_eq!(
            clean("```rust\nitems.len()\n```", &c).as_deref(),
            Some("items.len()")
        );
        assert_eq!(
            clean("let total = items.len()", &c).as_deref(),
            Some("items.len()"),
            "a repeat of the line so far is dropped"
        );
        assert_eq!(
            clean("items.len();\n}", &c).as_deref(),
            Some("items.len()"),
            "text that is already after the caret is cut"
        );
        assert_eq!(clean("", &c), None);
        assert_eq!(clean("```\n```", &c), None);
        let long: String = (0..40).map(|i| format!("a{i}\n")).collect();
        assert_eq!(
            clean(&long, &ctx("x", "")).unwrap().lines().count(),
            MAX_LINES
        );
    }

    #[test]
    fn only_the_newest_request_is_shown() {
        let mut g = Generation::default();
        let first = g.bump();
        assert!(g.is_current(first));
        let second = g.bump();
        assert!(!g.is_current(first), "an edit made the first stale");
        assert!(g.is_current(second));
    }
}
