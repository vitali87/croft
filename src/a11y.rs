//! Screen reader mode (#621): a steady cursor on the focused text, and a
//! one-line description of what changed for terminal screen readers
//! (VoiceOver, Orca, NVDA over WSL) to speak.
//!
//! Pure: what is focused goes in, the sentence to announce comes out.

/// What the reader is on in the editor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Focus {
    /// The file's name, as shown in its tab.
    pub file: String,
    /// 1-based.
    pub line: usize,
    pub text: String,
    /// The most severe diagnostic on the line, as "error: message".
    pub diagnostic: Option<String>,
}

/// What to announce moving from `prev` to `now`, or nothing when nothing a
/// listener needs changed: a new file names the file, a new line reads the
/// line, and a diagnostic appearing on the same line is read on its own.
/// Blank lines are read as "blank".
pub fn announce(prev: Option<&Focus>, now: &Focus) -> Option<String> {
    let text = match now.text.trim() {
        "" => "blank",
        t => t,
    };
    let with_diag = |line: String| match &now.diagnostic {
        Some(d) => format!("{line}. {d}"),
        None => line,
    };
    match prev {
        Some(p) if p.file == now.file && p.line == now.line => {
            if now.diagnostic.is_some() && now.diagnostic != p.diagnostic {
                now.diagnostic.clone()
            } else {
                None
            }
        }
        Some(p) if p.file == now.file => Some(with_diag(format!("Line {}: {text}", now.line))),
        _ => Some(with_diag(format!(
            "{}, line {}: {text}",
            now.file, now.line
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(file: &str, line: usize, text: &str, diagnostic: Option<&str>) -> Focus {
        Focus {
            file: file.into(),
            line,
            text: text.into(),
            diagnostic: diagnostic.map(str::to_string),
        }
    }

    #[test]
    fn a_new_file_names_it_and_a_new_line_reads_it() {
        let a = at("main.rs", 3, "    let x = 1;", None);
        assert_eq!(
            announce(None, &a).as_deref(),
            Some("main.rs, line 3: let x = 1;")
        );
        let b = at("main.rs", 4, "", None);
        assert_eq!(announce(Some(&a), &b).as_deref(), Some("Line 4: blank"));
        let c = at("lib.rs", 4, "fn f() {}", None);
        assert_eq!(
            announce(Some(&b), &c).as_deref(),
            Some("lib.rs, line 4: fn f() {}")
        );
    }

    #[test]
    fn diagnostics_are_read_with_their_line_or_when_they_appear() {
        let a = at("main.rs", 3, "let x = y;", None);
        let b = at(
            "main.rs",
            4,
            "let z = w;",
            Some("error: cannot find value `w`"),
        );
        assert_eq!(
            announce(Some(&a), &b).as_deref(),
            Some("Line 4: let z = w;. error: cannot find value `w`")
        );
        let c = at("main.rs", 3, "let x = y;", Some("warning: unused variable"));
        assert_eq!(
            announce(Some(&a), &c).as_deref(),
            Some("warning: unused variable"),
            "a diagnostic arriving on the same line"
        );
    }

    #[test]
    fn nothing_is_announced_when_nothing_a_listener_needs_changed() {
        let a = at("main.rs", 3, "let x = 1;", None);
        assert_eq!(announce(Some(&a), &a), None);
        // Typing on the same line changes the text but not the line: no
        // announcement per keystroke.
        let typed = at("main.rs", 3, "let x = 12;", None);
        assert_eq!(announce(Some(&a), &typed), None);
    }
}
