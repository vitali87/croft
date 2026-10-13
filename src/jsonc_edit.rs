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

/// One member of a JSONC object, as byte offsets into the text.
struct Member {
    key: String,
    key_start: usize,
    value_start: usize,
    value_end: usize,
    /// The comma after the value, if there is one.
    comma: Option<usize>,
}

/// An object's braces and members.
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
            b'{' => self.object().map(|_| ()),
            b'[' => {
                self.i += 1;
                loop {
                    self.skip_blank()?;
                    match *self.s.get(self.i)? {
                        b']' => {
                            self.i += 1;
                            return Some(());
                        }
                        b',' => self.i += 1,
                        _ => self.value()?,
                    }
                }
            }
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
        let open = self.i;
        if self.s.get(self.i) != Some(&b'{') {
            return None;
        }
        self.i += 1;
        let mut members = Vec::new();
        loop {
            self.skip_blank()?;
            if self.s.get(self.i) == Some(&b'}') {
                let close = self.i;
                self.i += 1;
                return Some(Object {
                    open,
                    close,
                    members,
                });
            }
            let key_start = self.i;
            let key = self.string()?;
            self.skip_blank()?;
            if self.s.get(self.i) != Some(&b':') {
                return None;
            }
            self.i += 1;
            self.skip_blank()?;
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
            if comma.is_none() && self.s.get(self.i) != Some(&b'}') {
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
        let m = obj.members.iter().rev().find(|m| &m.key == key)?;
        sc.i = m.value_start;
        obj = sc.object()?;
    }
    Some(obj)
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
    let found = obj.members.iter().rposition(|m| &m.key == key);
    match (found, value) {
        (Some(i), Some(value)) => {
            let m = &obj.members[i];
            let indent = line_indent(text, m.key_start).unwrap_or("");
            Some(format!(
                "{}{}{}",
                &text[..m.value_start],
                render(value, indent)?,
                &text[m.value_end..]
            ))
        }
        (Some(i), None) => Some(remove(text, &obj, i)),
        (None, Some(value)) => insert(text, &obj, key, value),
        (None, None) => Some(text.to_string()),
    }
}

/// `text` with a new member `key: value` after the last one of `obj`.
fn insert(text: &str, obj: &Object, key: &str, value: &Value) -> Option<String> {
    let name = serde_json::to_string(key).ok()?;
    let Some(last) = obj.members.last() else {
        // An empty object: open it onto lines of its own.
        let outer = line_indent(text, obj.open).unwrap_or("");
        let indent = format!("{outer}  ");
        return Some(format!(
            "{}{{\n{indent}{name}: {}\n{outer}}}{}",
            &text[..obj.open],
            render(value, &indent)?,
            &text[obj.close + 1..]
        ));
    };
    // Members one per line: the new one gets its own line after the last,
    // past a comment that ends that line, so the comment keeps its member.
    if let Some(indent) = line_indent(text, last.key_start) {
        let after = last.comma.map_or(last.value_end, |c| c + 1);
        let eol = text[after..].find('\n').map(|n| after + n);
        let rest = &text[after..eol.unwrap_or(text.len())];
        let rest = rest.trim();
        if let Some(eol) = eol
            && (rest.is_empty() || rest.starts_with("//"))
            && after <= obj.close
            && eol < obj.close
        {
            let member = format!("{name}: {}", render(value, indent)?);
            return Some(match last.comma {
                // A trailing comma stays the file's style.
                Some(_) => format!("{}\n{indent}{member},{}", &text[..eol], &text[eol..]),
                None => format!(
                    "{},{}\n{indent}{member}{}",
                    &text[..last.value_end],
                    &text[last.value_end..eol],
                    &text[eol..]
                ),
            });
        }
    }
    // A one-line object: the member joins the line.
    Some(format!(
        "{}, {name}: {}{}",
        &text[..last.value_end],
        serde_json::to_string(value).ok()?,
        &text[last.value_end..]
    ))
}

/// `text` without member `i` of `obj` (and its line, when it had one).
fn remove(text: &str, obj: &Object, i: usize) -> String {
    let m = &obj.members[i];
    let mut start = m.key_start;
    let mut end = m.comma.map_or(m.value_end, |c| c + 1);
    if m.comma.is_none()
        && i > 0
        && let Some(prev) = obj.members[i - 1].comma
    {
        // The last member goes: the comma before it would now trail.
        start = prev;
    }
    if line_indent(text, m.key_start).is_some() {
        let line_start = text[..m.key_start].rfind('\n').map_or(0, |n| n + 1);
        let rest_end = text[end..].find('\n').map_or(text.len(), |n| end + n);
        if text[end..rest_end].trim().is_empty() && start == m.key_start {
            start = line_start;
            end = (rest_end + 1).min(text.len());
        }
    }
    format!("{}{}", &text[..start], &text[end..])
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
}
