//! Debug: Add Configuration…: the fields it asks for, checking each answer, the
//! launch.json entry it makes, and writing it into `.croft/launch.json` safely.

use super::*;

/// A field of the **Debug: Add Configuration…** flow (#250), in the order
/// [`ConfigDraft::next_field`] asks for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftField {
    Name,
    Program,
    Args,
    Cwd,
    Env,
    PreLaunchTask,
    /// The port a python or node debuggee listens on (attach).
    Port,
    /// The process id to attach lldb or delve to.
    ProcessId,
}

/// The adapters the flow offers: `(type, label)`, the type as written into
/// launch.json. One per [`AdapterKind`], so every choice resolves.
pub const DRAFT_TYPES: [(&str, &str); 4] = [
    ("python", "Python — debugpy"),
    ("lldb", "Rust / C / C++ — lldb-dap"),
    ("go", "Go — delve"),
    ("node", "JavaScript / TypeScript — Node"),
];

/// A configuration being built by **Debug: Add Configuration…** (#250):
/// the native form for what a launch.json entry says, written to
/// `.croft/launch.json` when it is complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigDraft {
    pub type_name: String,
    pub request: RequestKind,
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub pre_launch_task: String,
    pub port: Option<u16>,
    pub process_id: Option<u32>,
}

impl ConfigDraft {
    pub fn new(type_name: &str, request: RequestKind) -> Self {
        Self {
            type_name: type_name.to_string(),
            request,
            name: String::new(),
            program: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            env: Vec::new(),
            pre_launch_task: String::new(),
            port: None,
            process_id: None,
        }
    }

    /// Attaching to python or node means a port the debuggee listens on;
    /// lldb and delve attach to a process.
    pub(super) fn attaches_by_port(&self) -> bool {
        matches!(
            adapter_for_type(&self.type_name),
            Some(AdapterKind::Debugpy | AdapterKind::JsDebug)
        )
    }

    /// The field to ask for after `after` (`None`: the first), or `None`
    /// when the draft is complete.
    pub fn next_field(&self, after: Option<DraftField>) -> Option<DraftField> {
        use DraftField::*;
        let order: &[DraftField] = match self.request {
            RequestKind::Launch => &[Name, Program, Args, Cwd, Env, PreLaunchTask],
            RequestKind::Attach if self.attaches_by_port() => &[Name, Port],
            RequestKind::Attach => &[Name, ProcessId],
        };
        match after {
            None => order.first().copied(),
            Some(f) => order
                .iter()
                .position(|&o| o == f)
                .and_then(|i| order.get(i + 1).copied()),
        }
    }

    /// Whether `field` may be left blank (then it is left out).
    pub fn optional(field: DraftField) -> bool {
        matches!(
            field,
            DraftField::Args | DraftField::Cwd | DraftField::Env | DraftField::PreLaunchTask
        )
    }

    /// The prompt's title, hint, and the value it starts with.
    pub fn prompt(&self, field: DraftField) -> (String, &'static str, String) {
        let seed = match field {
            DraftField::Name => {
                let label = DRAFT_TYPES
                    .iter()
                    .find(|(t, _)| *t == self.type_name)
                    .map_or(self.type_name.as_str(), |(_, l)| {
                        l.split(" — ").next().unwrap_or(l)
                    });
                match self.request {
                    RequestKind::Launch => format!("Launch {label}"),
                    RequestKind::Attach => format!("Attach to {label}"),
                }
            }
            DraftField::Program => match adapter_for_type(&self.type_name) {
                Some(AdapterKind::LldbDap) => String::from("${workspaceFolder}/target/debug/"),
                Some(AdapterKind::Delve) => String::from("${workspaceFolder}"),
                _ => String::from("${file}"),
            },
            DraftField::Cwd => String::from("${workspaceFolder}"),
            _ => String::new(),
        };
        let (title, hint) = match field {
            DraftField::Name => ("Name", "What the configuration is called"),
            DraftField::Program => ("Program", "The program to debug"),
            DraftField::Args => (
                "Arguments",
                "Space-separated, \"quoted\" for spaces; Enter on blank for none",
            ),
            DraftField::Cwd => ("Working directory", "Enter on blank for the default"),
            DraftField::Env => ("Environment", "NAME=value pairs; Enter on blank for none"),
            DraftField::PreLaunchTask => (
                "Pre-launch task",
                "A task label to run first; Enter on blank for none",
            ),
            DraftField::Port => ("Port", "The port the debuggee listens on"),
            DraftField::ProcessId => ("Process id", "The process to attach to"),
        };
        (format!("Add Configuration: {title}"), hint, seed)
    }

    /// Take the typed value for `field`, or say why it does not fit.
    pub fn set(&mut self, field: DraftField, value: &str) -> Result<(), String> {
        let value = value.trim();
        match field {
            DraftField::Name => self.name = value.to_string(),
            DraftField::Program => self.program = value.to_string(),
            DraftField::Args => self.args = split_args(value)?,
            DraftField::Cwd => self.cwd = value.to_string(),
            DraftField::Env => self.env = parse_env_pairs(value)?,
            DraftField::PreLaunchTask => self.pre_launch_task = value.to_string(),
            DraftField::Port => {
                self.port = Some(
                    value
                        .parse::<u16>()
                        .ok()
                        .filter(|p| *p > 0)
                        .ok_or_else(|| format!("Not a port: {value}"))?,
                );
            }
            DraftField::ProcessId => {
                self.process_id = Some(
                    value
                        .parse::<u32>()
                        .ok()
                        .filter(|p| *p > 0)
                        .ok_or_else(|| format!("Not a process id: {value}"))?,
                );
            }
        }
        Ok(())
    }

    /// The launch.json entry, in the fields [`resolve`] maps: python and
    /// node attach by `port`, lldb and delve by `processId` (delve in its
    /// `local` mode).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("name".into(), json!(self.name));
        o.insert("type".into(), json!(self.type_name));
        let request = match self.request {
            RequestKind::Launch => "launch",
            RequestKind::Attach => "attach",
        };
        o.insert("request".into(), json!(request));
        let adapter = adapter_for_type(&self.type_name);
        match self.request {
            RequestKind::Launch => {
                o.insert("program".into(), json!(self.program));
                if !self.args.is_empty() {
                    o.insert("args".into(), json!(self.args));
                }
                if !self.cwd.is_empty() {
                    o.insert("cwd".into(), json!(self.cwd));
                }
                if !self.env.is_empty() {
                    let env: Map<String, Value> = self
                        .env
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect();
                    o.insert("env".into(), Value::Object(env));
                }
                if !self.pre_launch_task.is_empty() {
                    o.insert("preLaunchTask".into(), json!(self.pre_launch_task));
                }
            }
            RequestKind::Attach => {
                if self.attaches_by_port() {
                    o.insert("port".into(), json!(self.port));
                } else {
                    if adapter == Some(AdapterKind::Delve) {
                        o.insert("mode".into(), json!("local"));
                    }
                    o.insert("processId".into(), json!(self.process_id));
                }
            }
        }
        Value::Object(o)
    }
}

/// Split an argument line the way a shell would for the simple cases:
/// whitespace separates, and "double" or 'single' quotes keep spaces.
pub(super) fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_arg = true;
            }
            None if c.is_whitespace() => {
                if in_arg {
                    out.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            }
            None => {
                cur.push(c);
                in_arg = true;
            }
        }
    }
    if quote.is_some() {
        return Err(String::from("Unclosed quote in the arguments"));
    }
    if in_arg {
        out.push(cur);
    }
    Ok(out)
}

/// `NAME=value` pairs, separated by whitespace or `;`. A value keeps
/// everything after the first `=`.
pub(super) fn parse_env_pairs(line: &str) -> Result<Vec<(String, String)>, String> {
    split_args(&line.replace(';', " "))?
        .into_iter()
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
            _ => Err(format!("Not NAME=value: {pair}")),
        })
        .collect()
}

/// Add `draft` to `.croft/launch.json` under `root`, creating the file when
/// there is none; returns the file written.
///
/// croft owns that file, so it is rewritten whole, but only when it parses
/// as plain JSON: a file with comments or trailing commas would lose them,
/// so it is left alone and the entry is handed back to add by hand. A name
/// the file already uses is refused, since the picker lists by name.
///
/// The new file is written aside and renamed into place, so a kill or a full
/// disk mid-write leaves the old one whole. The workspace controls `.croft`,
/// so a symlink there (or at the file) is refused rather than followed: the
/// rewrite must not land somewhere else.
pub fn add_to_croft_launch_json(root: &Path, draft: &ConfigDraft) -> Result<PathBuf, String> {
    let dir = root.join(".croft");
    let path = dir.join("launch.json");
    for p in [&dir, &path] {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!(
                "{} is a symlink; not rewriting it",
                p.strip_prefix(root).unwrap_or(p).display()
            ));
        }
    }
    let entry = draft.to_json();
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Value>(&text).map_err(|_| {
            format!(
                ".croft/launch.json has comments or trailing commas, which rewriting it would lose; add this by hand: {entry}"
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({ "version": "0.2.0", "configurations": [] })
        }
        Err(e) => return Err(format!("Could not read .croft/launch.json: {e}")),
    };
    let list = match &mut doc {
        Value::Array(list) => list,
        Value::Object(o) => match o
            .entry("configurations")
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            Value::Array(list) => list,
            _ => {
                return Err(String::from(
                    ".croft/launch.json: \"configurations\" is not a list",
                ));
            }
        },
        _ => {
            return Err(String::from(
                ".croft/launch.json is neither a list nor an object",
            ));
        }
    };
    if list
        .iter()
        .any(|c| c.get("name").and_then(Value::as_str) == Some(draft.name.as_str()))
    {
        return Err(format!(
            ".croft/launch.json already has a configuration named \"{}\"",
            draft.name
        ));
    }
    list.push(entry);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create .croft: {e}"))?;
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    let (tmp, mut file) =
        create_fresh_temp(&dir).map_err(|e| format!("Could not write .croft/launch.json: {e}"))?;
    use std::io::Write as _;
    file.write_all((text + "\n").as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("Could not write .croft/launch.json: {e}")
        })?;
    Ok(path)
}

/// The name [`create_fresh_temp`] tries `n`th for this process.
pub(super) fn temp_candidate(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("launch.json.{}.{n}.tmp", std::process::id()))
}

/// A new, empty temp file beside `launch.json` that nothing else could have
/// put there: created exclusively, so an existing path (a symlink the
/// workspace planted at a guessable name included) is never opened or
/// followed; the next name is tried instead.
pub(super) fn create_fresh_temp(dir: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    for n in 0..64 {
        let tmp = temp_candidate(dir, n);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free temporary file name beside it",
    ))
}
