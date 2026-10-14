//! Edit a JSONC settings file in place (#1129): the values that changed are
//! rewritten where they stand, new keys are added after the last member of
//! their object, and everything else, comments, layout and key order
//! included, is left as the user wrote it. VS Code's settings editor works
//! the same way.

use serde_json::{Map, Value};

/// `text` rewritten so it parses (as JSONC) to `new`, keeping its comments
/// and layout. `None` when `text` is not a JSONC object or an edit cannot
/// be placed; the caller then writes `new` as plain JSON.
pub fn update(text: &str, new: &Value) -> Option<String> {
    let old: Value = serde_json::from_str(&crate::tasks::strip_jsonc(text)).ok()?;
    let (Value::Object(old), Value::Object(new_obj)) = (&old, new) else {
        return None;
    };
    let mut edits = Vec::new();
    diff(&mut Vec::new(), old, new_obj, &mut edits);
    let mut out = text.to_string();
    for (path, value) in &edits {
        out = apply(&out, path, value.as_ref())?;
    }
    // Whatever the edits did, the file must say exactly `new`.
    let check: Value = serde_json::from_str(&crate::tasks::strip_jsonc(&out)).ok()?;
    (check == *new).then_some(out)
}

/// The edits that turn `old` into `new`: a path and the value to set there,
/// or `None` to remove the key. Objects present on both sides are compared
/// key by key, so one changed setting touches only its own value.
fn diff(
    path: &mut Vec<String>,
    old: &Map<String, Value>,
    new: &Map<String, Value>,
    out: &mut Vec<(Vec<String>, Option<Value>)>,
) {
    for (key, value) in new {
        path.push(key.clone());
        match (old.get(key), value) {
            (Some(Value::Object(o)), Value::Object(n)) => diff(path, o, n, out),
            (Some(o), n) if o == n => {}
            _ => out.push((path.clone(), Some(value.clone()))),
        }
        path.pop();
    }
    for key in old.keys().filter(|k| !new.contains_key(*k)) {
        path.push(key.clone());
        out.push((path.clone(), None));
        path.pop();
    }
}

/// One member of a JSONC object, or element of an array, as byte offsets
/// into the text.
struct Member {
    /// `None` for an array element.
    key: Option<String>,
    /// Where the member starts: its key, or an element's value.
    key_start: usize,
    value_start: usize,
    value_end: usize,
    /// The comma after the value, if there is one.
    comma: Option<usize>,
}

/// An object's braces and members, or an array's brackets and elements.
struct Object {
    open: usize,
    close: usize,
    members: Vec<Member>,
}

struct Scanner<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scanner<'_> {
    /// Past whitespace and comments. `None` on an unclosed block comment.
    fn skip_blank(&mut self) -> Option<()> {
        loop {
            match self.s.get(self.i..self.i + 2) {
                Some(b"//") => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                Some(b"/*") => {
                    let end = self.s[self.i + 2..].windows(2).position(|w| w == b"*/")?;
                    self.i += 2 + end + 2;
                }
                _ => match self.s.get(self.i) {
                    Some(c) if c.is_ascii_whitespace() => self.i += 1,
                    _ => return Some(()),
                },
            }
        }
    }

    /// A string starting at the cursor, decoded.
    fn string(&mut self) -> Option<String> {
        let start = self.i;
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        loop {
            match self.s.get(self.i)? {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    let raw = std::str::from_utf8(&self.s[start..self.i]).ok()?;
                    return serde_json::from_str(raw).ok();
                }
                _ => self.i += 1,
            }
        }
    }

    /// Past one value of any kind.
    fn value(&mut self) -> Option<()> {
        match *self.s.get(self.i)? {
            b'"' => self.string().map(|_| ()),
            b'{' | b'[' => self.container().map(|_| ()),
            _ => {
                let start = self.i;
                while self
                    .s
                    .get(self.i)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || b"+-.".contains(c))
                {
                    self.i += 1;
                }
                (self.i > start).then_some(())
            }
        }
    }

    /// The object starting at the cursor.
    fn object(&mut self) -> Option<Object> {
        if self.s.get(self.i) != Some(&b'{') {
            return None;
        }
        self.container()
    }

    /// The object or array starting at the cursor.
    fn container(&mut self) -> Option<Object> {
        let open = self.i;
        let (keyed, end) = match *self.s.get(self.i)? {
            b'{' => (true, b'}'),
            b'[' => (false, b']'),
            _ => return None,
        };
        self.i += 1;
        let mut members = Vec::new();
        loop {
            self.skip_blank()?;
            if self.s.get(self.i) == Some(&end) {
                let close = self.i;
                self.i += 1;
                return Some(Object {
                    open,
                    close,
                    members,
                });
            }
            let key_start = self.i;
            let key = if keyed {
                let key = self.string()?;
                self.skip_blank()?;
                if self.s.get(self.i) != Some(&b':') {
                    return None;
                }
                self.i += 1;
                self.skip_blank()?;
                Some(key)
            } else {
                None
            };
            let value_start = self.i;
            self.value()?;
            let value_end = self.i;
            self.skip_blank()?;
            let comma = (self.s.get(self.i) == Some(&b',')).then(|| {
                self.i += 1;
                self.i - 1
            });
            members.push(Member {
                key,
                key_start,
                value_start,
                value_end,
                comma,
            });
            if comma.is_none() && self.s.get(self.i) != Some(&end) {
                return None;
            }
        }
    }
}

/// The object at `path` (keys from the root object down).
fn object_at(text: &str, path: &[String]) -> Option<Object> {
    let mut sc = Scanner {
        s: text.as_bytes(),
        i: 0,
    };
    sc.skip_blank()?;
    let mut obj = sc.object()?;
    for key in path {
        let m = obj
            .members
            .iter()
            .rev()
            .find(|m| m.key.as_deref() == Some(key.as_str()))?;
        sc.i = m.value_start;
        obj = sc.object()?;
    }
    Some(obj)
}

/// The array whose `[` is at `at`.
fn array_at(text: &str, at: usize) -> Option<Object> {
    if text.as_bytes().get(at) != Some(&b'[') {
        return None;
    }
    Scanner {
        s: text.as_bytes(),
        i: at,
    }
    .container()
}

/// The whitespace before `at` on its line, when nothing else is.
fn line_indent(text: &str, at: usize) -> Option<&str> {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[start..at];
    indent
        .chars()
        .all(|c| c == ' ' || c == '\t')
        .then_some(indent)
}

/// The whitespace that starts the line `at` is on.
fn leading_indent(text: &str, at: usize) -> &str {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[start..at];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// `value` as JSON, its later lines indented to sit under `indent`.
fn render(value: &Value, indent: &str) -> Option<String> {
    let pretty = serde_json::to_string_pretty(value).ok()?;
    Some(pretty.replace('\n', &format!("\n{indent}")))
}

/// `text` with the key at `path` set to `value`, or removed for `None`.
fn apply(text: &str, path: &[String], value: Option<&Value>) -> Option<String> {
    let (key, parent) = path.split_last()?;
    let obj = object_at(text, parent)?;
    // serde_json keeps the last of a repeated key, so that is the one to edit.
    let found = obj
        .members
        .iter()
        .rposition(|m| m.key.as_deref() == Some(key.as_str()));
    match (found, value) {
        (Some(i), Some(value)) => {
            let m = &obj.members[i];
            // An array is edited element by element, so the comments
            // between its elements (why an extension is disabled) stay.
            if let Value::Array(new) = value
                && let Some(out) = edit_array(text, m.value_start, new)
            {
                return Some(out);
            }
            let indent = line_indent(text, m.key_start).unwrap_or("");
            Some(format!(
                "{}{}{}",
                &text[..m.value_start],
                render(value, indent)?,
                &text[m.value_end..]
            ))
        }
        (Some(i), None) => Some(remove(text, &obj, i)),
        (None, Some(value)) => insert(text, &obj, obj.members.len(), Some(key), value),
        (None, None) => Some(text.to_string()),
    }
}

/// `text` with the array whose `[` is at `at` turned into `new` one element
/// at a time: the elements it shares with `new` stay where they are, with
/// the comments around them, the others are removed, and each new element
/// is inserted after the one it follows. `None` when the array can't be read.
fn edit_array(text: &str, at: usize, new: &[Value]) -> Option<String> {
    let old = array_at(text, at)?
        .members
        .iter()
        .map(|m| {
            serde_json::from_str(&crate::tasks::strip_jsonc(
                &text[m.value_start..m.value_end],
            ))
            .ok()
        })
        .collect::<Option<Vec<Value>>>()?;
    let kept = common(&old, new);
    let mut out = text.to_string();
    // Last first, so the indices still name the elements they did.
    for i in (0..old.len()).rev() {
        if !kept.iter().any(|&(o, _)| o == i) {
            out = remove(&out, &array_at(&out, at)?, i);
        }
    }
    // The array now holds the kept elements in order, so each new element
    // goes in at its own index in `new`.
    for (j, value) in new.iter().enumerate() {
        if !kept.iter().any(|&(_, n)| n == j) {
            out = insert(&out, &array_at(&out, at)?, j, None, value)?;
        }
    }
    Some(out)
}

/// The longest run of elements `old` and `new` share, in order, as pairs of
/// their indices: what an array edit keeps.
fn common(old: &[Value], new: &[Value]) -> Vec<(usize, usize)> {
    let mut len = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            len[i][j] = if old[i] == new[j] {
                len[i + 1][j + 1] + 1
            } else {
                len[i + 1][j].max(len[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if len[i + 1][j] >= len[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// `text` with a new member `key: value`, or element `value` for no key,
/// placed in `obj` at `index`: after the member before it, or first.
fn insert(
    text: &str,
    obj: &Object,
    index: usize,
    key: Option<&str>,
    value: &Value,
) -> Option<String> {
    let name = key.map(serde_json::to_string).transpose().ok()?;
    // Pretty under an indent on a line of its own, else compact.
    let member = |indent: Option<&str>| -> Option<String> {
        let value = match indent {
            Some(indent) => render(value, indent)?,
            None => serde_json::to_string(value).ok()?,
        };
        Some(match &name {
            Some(name) => format!("{name}: {value}"),
            None => value,
        })
    };
    let Some(prev) = index.checked_sub(1) else {
        let Some(first) = obj.members.first() else {
            // Empty: opened onto lines of its own, below any comment inside
            // it, which documents the settings to come.
            let outer = leading_indent(text, obj.open);
            let indent = format!("{outer}  ");
            let inside = text[obj.open + 1..obj.close].trim_end();
            return Some(format!(
                "{}{inside}\n{indent}{}\n{outer}{}",
                &text[..=obj.open],
                member(Some(&indent))?,
                &text[obj.close..]
            ));
        };
        // Members one per line: the new one gets the line after the
        // bracket's, so a comment above the first member keeps it.
        if let Some(indent) = line_indent(text, first.key_start)
            && let Some(eol) = text[obj.open..first.key_start].find('\n')
        {
            let eol = obj.open + eol;
            return Some(format!(
                "{}\n{indent}{},{}",
                &text[..eol],
                member(Some(indent))?,
                &text[eol..]
            ));
        }
        return Some(format!(
            "{}{}, {}",
            &text[..first.key_start],
            member(None)?,
            &text[first.key_start..]
        ));
    };
    let before = obj.members.get(prev)?;
    // Members one per line: the new one gets its own line after the one it
    // follows, past a comment that ends that line, so the comment keeps its
    // member.
    if let Some(indent) = line_indent(text, before.key_start) {
        let after = before.comma.map_or(before.value_end, |c| c + 1);
        let eol = text[after..].find('\n').map(|n| after + n);
        let rest = &text[after..eol.unwrap_or(text.len())];
        let rest = rest.trim();
        if let Some(eol) = eol
            && (rest.is_empty() || rest.starts_with("//"))
            && after <= obj.close
            && eol < obj.close
        {
            let member = member(Some(indent))?;
            return Some(match before.comma {
                // A trailing comma stays the file's style.
                Some(_) => format!("{}\n{indent}{member},{}", &text[..eol], &text[eol..]),
                None => format!(
                    "{},{}\n{indent}{member}{}",
                    &text[..before.value_end],
                    &text[before.value_end..eol],
                    &text[eol..]
                ),
            });
        }
    }
    // All on one line: the member joins it.
    Some(format!(
        "{}, {}{}",
        &text[..before.value_end],
        member(None)?,
        &text[before.value_end..]
    ))
}

/// `text` without member `i` of `obj` (and its line, when it had one).
fn remove(text: &str, obj: &Object, i: usize) -> String {
    let m = &obj.members[i];
    let mut start = m.key_start;
    let mut end = m.comma.map_or(m.value_end, |c| c + 1);
    if line_indent(text, m.key_start).is_some() {
        let line_start = text[..m.key_start].rfind('\n').map_or(0, |n| n + 1);
        let rest_end = text[end..].find('\n').map_or(text.len(), |n| end + n);
        if text[end..rest_end].trim().is_empty() {
            start = line_start;
            end = (rest_end + 1).min(text.len());
        }
    }
    // The last member goes: the comma before it would now trail. Only that
    // comma goes with it, so a comment after the comma keeps its member.
    let prev = i.checked_sub(1).and_then(|p| obj.members[p].comma);
    match (m.comma, prev) {
        (None, Some(prev)) => format!(
            "{}{}{}",
            &text[..prev],
            &text[prev + 1..start],
            &text[end..]
        ),
        _ => format!("{}{}", &text[..start], &text[end..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The issue's config.json.
    const ISSUE: &str = r#"{
  // Team defaults live in dotfiles; keep this file small.
  "extends": "~/dotfiles/croft-base.json",
  /* Format on save is OFF on purpose: prettier fights ruff here.
     See INFRA-2231 before turning it back on. */
  "format_on_save": false,
  "linux": {
    "theme": "nord", // matches the tmux theme on the build boxes
  },
}
"#;

    fn parsed(text: &str) -> Value {
        serde_json::from_str(&crate::tasks::strip_jsonc(text)).unwrap()
    }

    fn with(text: &str, edit: impl FnOnce(&mut Map<String, Value>)) -> Value {
        let mut v = parsed(text);
        edit(v.as_object_mut().unwrap());
        v
    }

    /// #1129: a new setting is added after the last member and every
    /// comment, the key order and the trailing commas stay.
    #[test]
    fn a_new_setting_keeps_the_files_comments() {
        let new = with(ISSUE, |m| {
            m.insert("auto_save".into(), json!(true));
        });
        let out = update(ISSUE, &new).unwrap();
        assert_eq!(
            out,
            ISSUE.replace(
                "    \"theme\": \"nord\", // matches the tmux theme on the build boxes\n  },\n",
                "    \"theme\": \"nord\", // matches the tmux theme on the build boxes\n  },\n  \"auto_save\": true,\n"
            ),
            "{out}"
        );
        assert_eq!(parsed(&out), new);
    }

    /// #1129: a changed value is rewritten where it stands, nested ones too,
    /// with the comment on its line kept.
    #[test]
    fn a_changed_setting_is_rewritten_in_place() {
        let new = with(ISSUE, |m| {
            m.insert("format_on_save".into(), json!(true));
            m["linux"]["theme"] = json!("dark");
        });
        let out = update(ISSUE, &new).unwrap();
        assert_eq!(
            out,
            ISSUE
                .replace("\"format_on_save\": false", "\"format_on_save\": true")
                .replace("\"theme\": \"nord\"", "\"theme\": \"dark\"")
        );
    }

    /// #1129: a new key in a nested object, an object value and a removed
    /// key, in a file without trailing commas.
    #[test]
    fn nested_additions_objects_and_removals_keep_the_file_valid() {
        let text = "{\n  // mine\n  \"a\": 1,\n  \"b\": { \"x\": 1 },\n  \"c\": {\n    \"y\": 2 // why\n  }\n}\n";
        let new = json!({ "a": 1, "b": { "x": 1, "z": [1, 2] }, "c": { "y": 2, "w": { "k": "v" } }, "d": null });
        let out = update(text, &new).unwrap();
        assert!(out.contains("// mine") && out.contains("// why"), "{out}");
        assert!(out.contains("\"y\": 2, // why\n    \"w\": {"), "{out}");
        assert_eq!(parsed(&out), new);
        let fewer = json!({ "b": { "x": 1 }, "c": { "y": 2 } });
        let out = update(text, &fewer).unwrap();
        assert!(out.contains("// why"), "{out}");
        assert!(!out.contains("\"a\""), "{out}");
        assert_eq!(parsed(&out), fewer);
    }

    /// #1129: an empty file object and a value that did not change.
    #[test]
    fn an_empty_object_opens_and_an_unchanged_file_stays_byte_for_byte() {
        let out = update("{}\n", &json!({ "theme": "nord" })).unwrap();
        assert_eq!(out, "{\n  \"theme\": \"nord\"\n}\n");
        assert_eq!(update(ISSUE, &parsed(ISSUE)).unwrap(), ISSUE);
    }

    /// #1129 negative: text that is not a JSONC object is left to the plain
    /// writer, and a key whose text looks like a comment is still a key.
    #[test]
    fn what_cannot_be_edited_in_place_is_refused() {
        assert_eq!(update("[1, 2]", &json!({ "a": 1 })), None);
        assert_eq!(update("{ \"a\": 1 ", &json!({ "a": 2 })), None);
        assert_eq!(update("", &json!({ "a": 2 })), None);
        let text = "{ \"//a\": \"/* b */\" }";
        let out = update(text, &json!({ "//a": "c" })).unwrap();
        assert_eq!(out, "{ \"//a\": \"c\" }");
    }

    /// #1129: disabling an extension adds to `disabled_extensions` in the
    /// set's sorted order; the comments between its elements stay with the
    /// elements they explain, and a removed element takes only its own line.
    #[test]
    fn a_changed_array_keeps_the_comments_between_its_elements() {
        let text = "{\n  \"disabled_extensions\": [\n    // rust-analyzer is too slow on this box\n    \"rust\",\n    \"zig\" // broken build\n  ]\n}\n";
        let new = json!({ "disabled_extensions": ["go", "python", "rust", "zig"] });
        let out = update(text, &new).unwrap();
        assert_eq!(
            out,
            "{\n  \"disabled_extensions\": [\n    \"go\",\n    \"python\",\n    // rust-analyzer is too slow on this box\n    \"rust\",\n    \"zig\" // broken build\n  ]\n}\n"
        );
        let fewer = json!({ "disabled_extensions": ["python", "rust", "zig"] });
        let out = update(&out, &fewer).unwrap();
        assert_eq!(
            out,
            "{\n  \"disabled_extensions\": [\n    \"python\",\n    // rust-analyzer is too slow on this box\n    \"rust\",\n    \"zig\" // broken build\n  ]\n}\n"
        );
        let one_line = "{ \"disabled_extensions\": [ /* rationale */ \"rust\" ] }";
        let out = update(
            one_line,
            &json!({ "disabled_extensions": ["go", "rust", "zig"] }),
        )
        .unwrap();
        assert_eq!(
            out,
            "{ \"disabled_extensions\": [ /* rationale */ \"go\", \"rust\", \"zig\" ] }"
        );
        let out = update(one_line, &json!({ "disabled_extensions": [] })).unwrap();
        assert_eq!(out, "{ \"disabled_extensions\": [ /* rationale */  ] }");
    }

    /// #1129: an object or array holding only comments keeps them when its
    /// first member arrives, below them, indented under its own line.
    #[test]
    fn the_first_member_goes_below_the_comments_of_an_empty_object() {
        let text = "{ /* shared defaults are documented here */ }";
        let out = update(text, &json!({ "theme": "nord" })).unwrap();
        assert_eq!(
            out,
            "{ /* shared defaults are documented here */\n  \"theme\": \"nord\"\n}"
        );
        let text = "{\n  // settings go here\n}\n";
        let out = update(text, &json!({ "theme": "nord" })).unwrap();
        assert_eq!(out, "{\n  // settings go here\n  \"theme\": \"nord\"\n}\n");
        let text = "{\n  \"disabled_extensions\": [\n    // none yet\n  ],\n  \"linux\": {}\n}\n";
        let new = json!({ "disabled_extensions": ["rust"], "linux": { "theme": "nord" } });
        let out = update(text, &new).unwrap();
        assert_eq!(
            out,
            "{\n  \"disabled_extensions\": [\n    // none yet\n    \"rust\"\n  ],\n  \"linux\": {\n    \"theme\": \"nord\"\n  }\n}\n"
        );
    }

    /// #1129: removing the last member, with no comma after it, takes the
    /// comma before it and nothing else: the comment after that comma
    /// belongs to the member before.
    #[test]
    fn removing_the_last_member_keeps_the_comment_of_the_one_before() {
        let text = "{ \"a\": 1, // explanation for a\n \"b\": 2 }";
        let out = update(text, &json!({ "a": 1 })).unwrap();
        assert_eq!(out, "{ \"a\": 1 // explanation for a\n  }");
        let text = "{\n  \"a\": 1, // explanation for a\n  \"b\": 2\n}\n";
        let out = update(text, &json!({ "a": 1 })).unwrap();
        assert_eq!(out, "{\n  \"a\": 1 // explanation for a\n}\n");
    }
}
