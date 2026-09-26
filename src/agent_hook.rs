//! Agent approval hooks (#346): `croft hook install` registers croft as
//! Claude Code's `PreToolUse` hook for file edits, and `croft hook
//! claude-code` is the program that hook runs.
//!
//! Only the settings writer here is Claude-specific. The hook itself reads
//! the agent's payload, hands the proposed change to the croft serving that
//! directory, and turns croft's answer back into the agent's reply format, so
//! another agent with a hook system needs a writer and a reply shape, not a
//! new transport.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

/// What the installed entry runs. Uninstall removes exactly the hooks whose
/// command is this string, so it must never change shape between versions.
pub const HOOK_COMMAND: &str = "croft hook claude-code";

/// The tools whose calls wait on croft's approval.
pub const HOOK_MATCHER: &str = "Edit|Write|MultiEdit";

/// Claude Code's own limit on the hook, in seconds. It sits above croft's
/// answer window so the hook returns "ask" on its own timeout instead of
/// being killed by Claude Code's (60 s when unset).
pub const HOOK_TIMEOUT_SECS: u64 = 130;

/// Which settings file the entry goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `~/.claude/settings.json`: every project.
    Global,
    /// `<cwd>/.claude/settings.json`: this project only.
    Project,
}

pub fn settings_path(scope: Scope, home: &Path, cwd: &Path) -> PathBuf {
    match scope {
        Scope::Global => home.join(".claude").join("settings.json"),
        Scope::Project => cwd.join(".claude").join("settings.json"),
    }
}

/// `before` with croft's entry added, or `None` when it is already there.
/// A missing file is `before = None`. Everything else in the file is kept
/// as it was written: same keys, same order, same text.
pub fn with_hook(before: Option<&str>) -> Result<Option<String>, String> {
    let mut root = match before {
        // An empty file (a `touch`ed one) is no settings yet, not bad JSON.
        Some(text) if !text.trim().is_empty() => Node::object_from(text)?,
        _ => Vec::new(),
    };
    if count_croft_hooks(&root)? > 0 {
        return Ok(None);
    }
    let hooks = object_at(&mut root, "hooks")?;
    let events = array_at(hooks, "PreToolUse")?;
    events.push(Node::Obj(vec![
        ("matcher".into(), Node::scalar(HOOK_MATCHER)),
        (
            "hooks".into(),
            Node::Arr(vec![Node::Obj(vec![
                ("type".into(), Node::scalar("command")),
                ("command".into(), Node::scalar(HOOK_COMMAND)),
                ("timeout".into(), Node::scalar(HOOK_TIMEOUT_SECS)),
            ])]),
        ),
    ]));
    Ok(Some(render(root)))
}

/// `current` with croft's hooks removed, and any matcher group, event list
/// or `hooks` object left empty by that removal dropped too. `None` when
/// the file holds no croft hook.
pub fn without_hook(current: &str) -> Result<Option<String>, String> {
    if current.trim().is_empty() {
        return Ok(None);
    }
    let mut root = Node::object_from(current)?;
    if count_croft_hooks(&root)? == 0 {
        return Ok(None);
    }
    let hooks = object_at(&mut root, "hooks")?;
    let events = array_at(hooks, "PreToolUse")?;
    for group in events.iter_mut() {
        let group = group.expand_object()?;
        if let Some(i) = group.iter().position(|(k, _)| k == "hooks") {
            let list = group[i].1.expand_array()?;
            list.retain(|h| !h.is_croft_hook());
            if list.is_empty() {
                group.clear();
            }
        }
    }
    events.retain(|g| !matches!(g, Node::Obj(fields) if fields.is_empty()));
    if events.is_empty() {
        hooks.retain(|(k, _)| k != "PreToolUse");
    }
    if hooks.is_empty() {
        root.retain(|(k, _)| k != "hooks");
    }
    Ok(Some(render(root)))
}

/// A settings file as croft edits it: the objects and lists on the path to
/// croft's entry are opened up, and every other value stays the file's own
/// text, so nothing croft does not touch is reordered or reflowed.
enum Node {
    Raw(Box<RawValue>),
    Obj(Vec<(String, Node)>),
    Arr(Vec<Node>),
}

impl Node {
    fn scalar(v: impl Serialize) -> Node {
        Node::Raw(serde_json::value::to_raw_value(&v).expect("a scalar serializes"))
    }

    fn object_from(text: &str) -> Result<Vec<(String, Node)>, String> {
        let raw: Box<RawValue> = serde_json::from_str(text)
            .map_err(|e| format!("the settings file is not valid JSON: {e}"))?;
        match Node::Raw(raw).expand_object() {
            Ok(fields) => Ok(std::mem::take(fields)),
            Err(_) => Err("the settings file is not a JSON object".into()),
        }
    }

    /// Open this value up as an object, in place.
    fn expand_object(&mut self) -> Result<&mut Vec<(String, Node)>, String> {
        if let Node::Raw(raw) = self {
            let Ordered(fields) = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
            *self = Node::Obj(fields.into_iter().map(|(k, v)| (k, Node::Raw(v))).collect());
        }
        match self {
            Node::Obj(fields) => Ok(fields),
            _ => Err("expected an object".into()),
        }
    }

    /// Open this value up as a list, in place.
    fn expand_array(&mut self) -> Result<&mut Vec<Node>, String> {
        if let Node::Raw(raw) = self {
            let items: Vec<Box<RawValue>> =
                serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
            *self = Node::Arr(items.into_iter().map(Node::Raw).collect());
        }
        match self {
            Node::Arr(items) => Ok(items),
            _ => Err("expected a list".into()),
        }
    }

    fn is_croft_hook(&self) -> bool {
        match self {
            Node::Raw(raw) => {
                serde_json::from_str::<Value>(raw.get()).is_ok_and(|v| v["command"] == HOOK_COMMAND)
            }
            Node::Obj(fields) => fields.iter().any(|(k, v)| {
                k == "command"
                    && matches!(v, Node::Raw(raw) if serde_json::from_str::<Value>(raw.get())
                        .is_ok_and(|c| c == HOOK_COMMAND))
            }),
            Node::Arr(_) => false,
        }
    }
}

impl Serialize for Node {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            Node::Raw(raw) => raw.serialize(s),
            Node::Obj(fields) => {
                let mut map = s.serialize_map(Some(fields.len()))?;
                for (k, v) in fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
            Node::Arr(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
        }
    }
}

/// An object's fields in file order, each value still raw text.
struct Ordered(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Fields;
        impl<'de> serde::de::Visitor<'de> for Fields {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Ordered, A::Error> {
                let mut fields = Vec::new();
                while let Some(entry) = a.next_entry()? {
                    fields.push(entry);
                }
                Ok(Ordered(fields))
            }
        }
        d.deserialize_map(Fields)
    }
}

/// The object under `key`, created at the end when absent.
fn object_at<'a>(
    fields: &'a mut Vec<(String, Node)>,
    key: &str,
) -> Result<&'a mut Vec<(String, Node)>, String> {
    let i = match fields.iter().position(|(k, _)| k == key) {
        Some(i) => i,
        None => {
            fields.push((key.into(), Node::Obj(Vec::new())));
            fields.len() - 1
        }
    };
    fields[i]
        .1
        .expand_object()
        .map_err(|_| format!("`{key}` is not an object"))
}

/// The list under `key`, created at the end when absent.
fn array_at<'a>(
    fields: &'a mut Vec<(String, Node)>,
    key: &str,
) -> Result<&'a mut Vec<Node>, String> {
    let i = match fields.iter().position(|(k, _)| k == key) {
        Some(i) => i,
        None => {
            fields.push((key.into(), Node::Arr(Vec::new())));
            fields.len() - 1
        }
    };
    fields[i]
        .1
        .expand_array()
        .map_err(|_| format!("`hooks.{key}` is not a list"))
}

/// Croft hooks already in the file. Reads a copy, so a shape croft cannot
/// edit is reported here, before anything is changed.
fn count_croft_hooks(root: &[(String, Node)]) -> Result<usize, String> {
    let Some((_, hooks)) = root.iter().find(|(k, _)| k == "hooks") else {
        return Ok(0);
    };
    let hooks: Value = serde_json::to_value(hooks).map_err(|e| e.to_string())?;
    let Some(hooks) = hooks.as_object() else {
        return Err("`hooks` is not an object".into());
    };
    let Some(events) = hooks.get("PreToolUse") else {
        return Ok(0);
    };
    let Some(groups) = events.as_array() else {
        return Err("`hooks.PreToolUse` is not a list".into());
    };
    Ok(groups
        .iter()
        .filter_map(|g| g["hooks"].as_array())
        .flatten()
        .filter(|h| h["command"] == HOOK_COMMAND)
        .count())
}

/// Two-space indentation and a trailing newline: the shape Claude Code
/// itself writes, so a round trip through croft leaves a file it wrote
/// byte-identical.
fn render(root: Vec<(String, Node)>) -> String {
    let mut out = serde_json::to_string_pretty(&Node::Obj(root)).unwrap_or_else(|_| "{}".into());
    out.push('\n');
    out
}

/// What install replaced, kept so uninstall can put the exact bytes back
/// when nothing has touched the file since.
#[derive(Serialize, Deserialize)]
struct InstallRecord {
    before: Option<String>,
    after: String,
}

fn record_path(record_dir: &Path, settings: &Path) -> PathBuf {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(settings.to_string_lossy().as_bytes());
    let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    record_dir.join(format!("{name}.json"))
}

/// Where install records live.
pub fn default_record_dir() -> PathBuf {
    crate::prefs::config_dir().join("hook-install")
}

/// A settings change: the unified diff shown before it is written, or
/// nothing to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed { diff: String },
    Unchanged,
}

fn diff(path: &Path, before: Option<&str>, after: Option<&str>) -> String {
    let name = path.display().to_string();
    similar::TextDiff::from_lines(before.unwrap_or(""), after.unwrap_or(""))
        .unified_diff()
        .header(&name, &name)
        .to_string()
}

fn read_settings(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Add croft's entry to `settings`, printing the diff through `show` before
/// the file is written.
pub fn install(
    settings: &Path,
    record_dir: &Path,
    show: impl FnOnce(&str),
) -> Result<Outcome, String> {
    let before = read_settings(settings)?;
    let Some(after) =
        with_hook(before.as_deref()).map_err(|e| format!("{}: {e}", settings.display()))?
    else {
        return Ok(Outcome::Unchanged);
    };
    let d = diff(settings, before.as_deref(), Some(&after));
    show(&d);
    if let Some(dir) = settings.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let record = InstallRecord {
        before,
        after: after.clone(),
    };
    std::fs::create_dir_all(record_dir)
        .map_err(|e| format!("cannot create {}: {e}", record_dir.display()))?;
    let json = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    std::fs::write(record_path(record_dir, settings), json)
        .map_err(|e| format!("cannot record the install: {e}"))?;
    write_replacing(settings, &after)
        .map_err(|e| format!("cannot write {}: {e}", settings.display()))?;
    Ok(Outcome::Changed { diff: d })
}

/// Replace `path`'s contents by writing aside and renaming: a kill or a full
/// disk mid-write must not truncate the user's Claude Code settings. A
/// symlinked settings file is written through, so the link survives.
fn write_replacing(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let tmp = target.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    if let Ok(meta) = std::fs::metadata(&target) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Remove croft's entry from `settings`. When the file is still exactly
/// what install wrote, the bytes it replaced come back, a file install
/// created is removed; otherwise only croft's hook is taken out.
pub fn uninstall(
    settings: &Path,
    record_dir: &Path,
    show: impl FnOnce(&str),
) -> Result<Outcome, String> {
    let record_file = record_path(record_dir, settings);
    let record: Option<InstallRecord> = std::fs::read_to_string(&record_file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let Some(current) = read_settings(settings)? else {
        let _ = std::fs::remove_file(&record_file);
        return Ok(Outcome::Unchanged);
    };
    let restored = match record {
        Some(r) if r.after == current => r.before,
        _ => match without_hook(&current).map_err(|e| format!("{}: {e}", settings.display()))? {
            Some(text) => Some(text),
            None => {
                let _ = std::fs::remove_file(&record_file);
                return Ok(Outcome::Unchanged);
            }
        },
    };
    let d = diff(settings, Some(&current), restored.as_deref());
    show(&d);
    match &restored {
        Some(text) => write_replacing(settings, text),
        None => std::fs::remove_file(settings),
    }
    .map_err(|e| format!("cannot write {}: {e}", settings.display()))?;
    let _ = std::fs::remove_file(&record_file);
    Ok(Outcome::Changed { diff: d })
}

/// How long croft has to answer before the hook hands the decision back to
/// Claude Code's own prompt.
pub const ANSWER_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

/// A running croft's answer to a proposed edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny { reason: String },
    Ask { reason: String },
}

/// The one line a hook sends to croft: which agent, which tool, and the
/// tool's own input, untouched. Agent-agnostic, so another agent's hook
/// sends the same request.
#[derive(Debug, Serialize, Deserialize)]
pub struct EditRequest {
    pub agent: String,
    pub tool: String,
    pub input: Value,
    pub cwd: PathBuf,
}

/// The croft listening for `cwd`: the socket of the nearest enclosing
/// workspace that accepts a connection.
fn connect_croft(cwd: &Path) -> Option<std::os::unix::net::UnixStream> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    cwd.ancestors().find_map(|dir| {
        std::os::unix::net::UnixStream::connect(crate::session::hook_socket_path(dir)).ok()
    })
}

/// Send `request` over `stream` and wait up to `window` for the answer.
/// Anything short of a well-formed answer in time is "ask": Claude Code's
/// own prompt takes over rather than the edit going through unseen.
fn exchange(
    mut stream: std::os::unix::net::UnixStream,
    request: &EditRequest,
    window: std::time::Duration,
) -> Decision {
    use std::io::Write;
    let ask = |why: &str| Decision::Ask {
        reason: format!("croft {why}; answer here instead"),
    };
    let mut line = match serde_json::to_string(request) {
        Ok(l) => l,
        Err(_) => return ask("could not be sent the edit"),
    };
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() {
        return ask("could not be sent the edit");
    }
    let deadline = std::time::Instant::now() + window;
    match crate::view_ipc::read_line_by_deadline(&stream, deadline, "croft") {
        Ok(reply) => serde_json::from_str(reply.trim())
            .unwrap_or_else(|_| ask("sent an answer it could not read")),
        Err(_) => ask(&format!("did not answer within {} s", window.as_secs())),
    }
}

/// Claude Code's `PreToolUse` reply for a decision.
pub fn claude_code_reply(decision: &Decision) -> String {
    let (verdict, reason) = match decision {
        Decision::Allow => ("allow", "approved in croft"),
        Decision::Deny { reason } => ("deny", reason.as_str()),
        Decision::Ask { reason } => ("ask", reason.as_str()),
    };
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": verdict,
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

/// `croft hook claude-code`: read Claude Code's payload from `input`, ask
/// the croft serving its cwd, write the reply to `out`. With no croft
/// running nothing is written, which leaves Claude Code's own permission
/// flow in charge: the hook never blocks an agent while the editor is
/// closed, and never approves an edit nobody saw.
pub fn run_claude_code(
    input: &mut dyn std::io::Read,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
    window: std::time::Duration,
) -> Result<(), String> {
    let mut text = String::new();
    input
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read the hook payload: {e}"))?;
    let payload: Value =
        serde_json::from_str(&text).map_err(|e| format!("the hook payload is not JSON: {e}"))?;
    let cwd = payload["cwd"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let Some(stream) = connect_croft(&cwd) else {
        let _ = writeln!(
            err,
            "croft: no croft is open on {}; Claude Code decides",
            cwd.display()
        );
        return Ok(());
    };
    let request = EditRequest {
        agent: "claude-code".into(),
        tool: payload["tool_name"].as_str().unwrap_or_default().into(),
        input: payload["tool_input"].clone(),
        cwd,
    };
    let decision = exchange(stream, &request, window);
    writeln!(out, "{}", claude_code_reply(&decision)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_settings_file_takes_the_hook_like_a_missing_one() {
        assert_eq!(with_hook(Some("  \n")), with_hook(None));
        assert_eq!(without_hook(""), Ok(None));
    }

    fn croft_hooks(text: &str) -> usize {
        let v: Value = serde_json::from_str(text).unwrap();
        v["hooks"]["PreToolUse"]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                    .filter(|h| h["command"] == HOOK_COMMAND)
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn a_missing_file_gets_just_the_hook_entry() {
        let after = with_hook(None).unwrap().unwrap();
        let v: Value = serde_json::from_str(&after).unwrap();
        let group = &v["hooks"]["PreToolUse"][0];
        assert_eq!(group["matcher"], HOOK_MATCHER);
        assert_eq!(group["hooks"][0]["type"], "command");
        assert_eq!(group["hooks"][0]["command"], HOOK_COMMAND);
        assert_eq!(group["hooks"][0]["timeout"], HOOK_TIMEOUT_SECS);
        assert!(after.ends_with("}\n"));
    }

    #[test]
    fn existing_settings_and_hooks_are_kept_in_order() {
        let before = "{\n  \"permissions\": {\n    \"allow\": [\n      \"Bash(ls)\"\n    ]\n  },\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\n        \"matcher\": \"Bash\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"guard.sh\"\n          }\n        ]\n      }\n    ]\n  }\n}\n";
        let after = with_hook(Some(before)).unwrap().unwrap();
        // Keys keep the file's order: permissions, model, hooks.
        let p = after.find("\"permissions\"").unwrap();
        let m = after.find("\"model\"").unwrap();
        let h = after.find("\"hooks\"").unwrap();
        assert!(p < m && m < h, "{after}");
        assert!(after.contains("guard.sh"));
        assert_eq!(croft_hooks(&after), 1);
    }

    #[test]
    fn installing_twice_changes_nothing() {
        let once = with_hook(None).unwrap().unwrap();
        assert_eq!(with_hook(Some(&once)).unwrap(), None);
        assert_eq!(croft_hooks(&once), 1);
    }

    #[test]
    fn a_file_that_is_not_an_object_is_refused() {
        assert!(with_hook(Some("[1, 2]")).is_err());
        assert!(with_hook(Some("{ not json")).is_err());
        assert!(with_hook(Some("{\"hooks\": 3}")).is_err());
    }

    #[test]
    fn removal_drops_only_croft_and_the_containers_it_emptied() {
        let before = "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\n        \"matcher\": \"Bash\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"guard.sh\"\n          }\n        ]\n      }\n    ]\n  }\n}\n";
        let installed = with_hook(Some(before)).unwrap().unwrap();
        let removed = without_hook(&installed).unwrap().unwrap();
        assert_eq!(removed, before);
        assert_eq!(croft_hooks(&removed), 0);

        let fresh = with_hook(None).unwrap().unwrap();
        assert_eq!(without_hook(&fresh).unwrap().unwrap(), "{}\n");
    }

    #[test]
    fn removal_keeps_a_user_hook_sharing_croft_matcher_group() {
        let shared = format!(
            "{{\"hooks\": {{\"PreToolUse\": [{{\"matcher\": \"{HOOK_MATCHER}\", \"hooks\": [{{\"type\": \"command\", \"command\": \"{HOOK_COMMAND}\"}}, {{\"type\": \"command\", \"command\": \"lint.sh\"}}]}}]}}}}"
        );
        let removed = without_hook(&shared).unwrap().unwrap();
        assert!(removed.contains("lint.sh"));
        assert_eq!(croft_hooks(&removed), 0);
        assert!(
            removed.contains(HOOK_MATCHER),
            "the group still holds lint.sh"
        );
    }

    #[test]
    fn removal_without_a_croft_hook_is_none() {
        assert_eq!(without_hook("{\"model\": \"opus\"}").unwrap(), None);
    }

    #[test]
    fn uninstall_restores_the_original_bytes_and_removes_a_created_file() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("records");
        let settings = dir.path().join("proj/.claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        // Not the shape croft renders: only the record can bring it back.
        let original = "{\"model\":\"opus\",  \"env\": {\"A\": \"1\"}}";
        std::fs::write(&settings, original).unwrap();
        let mut shown = String::new();
        let out = install(&settings, &records, |d| shown = d.to_string()).unwrap();
        assert!(matches!(out, Outcome::Changed { .. }));
        assert!(
            shown
                .lines()
                .any(|l| l.starts_with('+') && l.contains(HOOK_COMMAND)),
            "{shown}"
        );
        assert!(
            std::fs::read_to_string(&settings)
                .unwrap()
                .contains(HOOK_COMMAND)
        );
        assert_eq!(
            install(&settings, &records, |_| {}).unwrap(),
            Outcome::Unchanged
        );

        uninstall(&settings, &records, |_| {}).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), original);

        let fresh = dir.path().join("other/.claude/settings.json");
        install(&fresh, &records, |_| {}).unwrap();
        uninstall(&fresh, &records, |_| {}).unwrap();
        assert!(!fresh.exists());
        assert_eq!(
            uninstall(&fresh, &records, |_| {}).unwrap(),
            Outcome::Unchanged
        );
    }

    #[test]
    fn uninstall_after_a_later_edit_removes_only_croft() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("records");
        let settings = dir.path().join("settings.json");
        install(&settings, &records, |_| {}).unwrap();
        // The user (or Claude Code) edits the file after the install.
        let edited = std::fs::read_to_string(&settings).unwrap().replacen(
            '{',
            "{\n  \"model\": \"opus\",",
            1,
        );
        std::fs::write(&settings, &edited).unwrap();
        uninstall(&settings, &records, |_| {}).unwrap();
        let left = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(left, "{\n  \"model\": \"opus\"\n}\n");
    }

    fn fake_croft(
        reply: Option<&'static str>,
    ) -> (
        tempfile::TempDir,
        std::os::unix::net::UnixStream,
        std::thread::JoinHandle<String>,
    ) {
        use std::io::{BufRead, Write};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let client = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(conn.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            if let Some(r) = reply {
                conn.write_all(r.as_bytes()).unwrap();
            } else {
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
            line
        });
        (dir, client, server)
    }

    fn request() -> EditRequest {
        EditRequest {
            agent: "claude-code".into(),
            tool: "Edit".into(),
            input: serde_json::json!({"file_path": "/w/a.rs", "old_string": "a", "new_string": "b"}),
            cwd: "/w".into(),
        }
    }

    #[test]
    fn croft_answer_reaches_the_hook_and_the_request_carries_the_edit() {
        let (_d, client, server) = fake_croft(Some(
            "{\"decision\":\"deny\",\"reason\":\"not that file\"}\n",
        ));
        let got = exchange(client, &request(), std::time::Duration::from_secs(5));
        assert_eq!(
            got,
            Decision::Deny {
                reason: "not that file".into()
            }
        );
        let sent: EditRequest = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(sent.tool, "Edit");
        assert_eq!(sent.input["new_string"], "b");

        let (_d, client, _s) = fake_croft(Some("{\"decision\":\"allow\"}\n"));
        assert_eq!(
            exchange(client, &request(), std::time::Duration::from_secs(5)),
            Decision::Allow
        );
    }

    #[test]
    fn silence_or_garbage_from_croft_is_ask() {
        let (_d, client, _s) = fake_croft(None);
        let got = exchange(client, &request(), std::time::Duration::from_millis(100));
        assert!(
            matches!(got, Decision::Ask { ref reason } if reason.contains("did not answer")),
            "{got:?}"
        );

        let (_d, client, _s) = fake_croft(Some("yes please\n"));
        let got = exchange(client, &request(), std::time::Duration::from_secs(5));
        assert!(matches!(got, Decision::Ask { .. }), "{got:?}");
    }

    #[test]
    fn replies_use_claude_code_permission_decisions() {
        let v: Value = serde_json::from_str(&claude_code_reply(&Decision::Allow)).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
        let v: Value =
            serde_json::from_str(&claude_code_reply(&Decision::Ask { reason: "r".into() }))
                .unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "ask");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "r");
    }

    #[test]
    fn with_no_croft_open_the_hook_says_nothing_and_returns_fast() {
        let dir = tempfile::tempdir().unwrap();
        let payload = format!(
            "{{\"tool_name\":\"Edit\",\"tool_input\":{{}},\"cwd\":\"{}\"}}",
            dir.path().display()
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let t = std::time::Instant::now();
        run_claude_code(&mut payload.as_bytes(), &mut out, &mut err, ANSWER_WINDOW).unwrap();
        assert!(t.elapsed() < std::time::Duration::from_millis(100));
        assert!(
            out.is_empty(),
            "no decision: Claude Code's own prompt stays in charge"
        );
        assert!(String::from_utf8(err).unwrap().contains("no croft is open"));
    }
}
