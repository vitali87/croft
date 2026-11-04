//! JSONC, the JSON-with-comments dialect of VS Code's settings, tasks and
//! launch files: one stripper that turns it into strict JSON for serde_json.
//! A leaf module, so every reader can share it without depending on `tasks`.

/// Strip `//` and `/* */` comments (outside strings) plus trailing commas
/// so VS Code style JSONC parses with strict serde_json. Two
/// passes: comments first, then trailing commas, so a comma followed by
/// a comment followed by `}` still counts as trailing. Every JSONC
/// reader in croft (tasks.json, launch.json, settings layers, keybindings,
/// themes, ...) goes through it.
pub(crate) fn strip_jsonc(text: &str) -> String {
    let mut no_comments = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            no_comments.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        no_comments.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                no_comments.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        no_comments.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => no_comments.push(c),
        }
    }
    let mut out = String::with_capacity(no_comments.len());
    let mut chars = no_comments.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' => {
                let closes = chars
                    .clone()
                    .find(|n| !n.is_whitespace())
                    .is_some_and(|n| matches!(n, '}' | ']'));
                if !closes {
                    out.push(',');
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_jsonc_leaves_strings_intact() {
        let cleaned = strip_jsonc(
            r#"{"a": "http://x // not a comment", // real
"b": 1, /* gone */ }"#,
        );
        let v: serde_json::Value = serde_json::from_str(&cleaned).expect("valid strict JSON");
        assert_eq!(v["a"], "http://x // not a comment");
        assert_eq!(v["b"], 1);
    }

    /// The non-ASCII guarantee belongs on the function that carries it.
    ///
    /// Seven modules route their JSONC through here, and #396 was a second
    /// copy in `workspace.rs` that walked BYTES: it rebuilt every multi-byte
    /// character one byte at a time, so `caf\u{e9}` came back as `caf\u{c3}\u{a9}`
    /// and the JSON still parsed. That copy is gone, but the only tests that
    /// covered this were in the CALLERS, and each of those reaches this
    /// function through file IO and a JSON parse. A future rewrite of this
    /// stripper could reintroduce the defect for every module whose own
    /// tests happen to be ASCII, which was all of them.
    ///
    /// Non-ASCII in a key as well as a value: a mangled key does not merely
    /// arrive wrong, it makes the document fail to parse outright.
    #[test]
    fn strip_jsonc_preserves_non_ascii() {
        let cleaned = strip_jsonc(
            "{\n  // a comment, so the comment path actually runs\n  \"caf\u{e9}\": \"\u{4e2d}\u{6587} \u{444}\u{430}\u{439}\u{43b}\",\n}",
        );
        let v: serde_json::Value = serde_json::from_str(&cleaned).expect("valid strict JSON");
        assert_eq!(
            v["caf\u{e9}"],
            "\u{4e2d}\u{6587} \u{444}\u{430}\u{439}\u{43b}"
        );
    }
}
