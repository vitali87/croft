//! Running notebook cells (#355): one Jupyter kernel per open notebook,
//! driven through `assets/jupyter/bridge.py`, and the edits that write a
//! run's outputs back into the notebook's JSON.
//!
//! The bridge speaks the Jupyter wire protocol through `jupyter_client`,
//! which every `ipykernel` install carries, so croft needs no ZMQ client of
//! its own and any installed kernelspec (not only Python) works. croft and
//! the bridge exchange JSON lines; outputs arrive already in nbformat v4
//! shape. Outputs are written into the buffer as ordinary edits, so Save
//! persists them the way Jupyter does and Undo takes a run back.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const BRIDGE: &str = include_str!("../assets/jupyter/bridge.py");

/// What croft asks the kernel to do.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    Execute { id: String, code: String },
    Interrupt,
    Restart,
    Shutdown,
}

/// What the kernel reports back.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum Event {
    Ready {
        kernel: String,
        display: String,
    },
    Output {
        id: String,
        output: Value,
    },
    Clear {
        id: String,
    },
    Done {
        id: String,
        execution_count: Option<u64>,
        status: String,
    },
    Error {
        message: String,
    },
}

/// A notebook's kernel. Starting one never blocks: finding a Python with
/// `jupyter_client` and `ipykernel`, and the kernel's own start-up, happen
/// on a worker thread, and requests sent before it is ready wait there.
pub struct KernelSession {
    requests: Sender<Request>,
    events: Receiver<Event>,
}

impl KernelSession {
    pub fn start(workspace: &Path, cwd: &Path, kernel: &str) -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (ev_tx, ev_rx) = channel::<Event>();
        let workspace = workspace.to_path_buf();
        let cwd = cwd.to_path_buf();
        let kernel = kernel.to_string();
        std::thread::spawn(move || run_bridge(&workspace, &cwd, &kernel, req_rx, ev_tx));
        Self {
            requests: req_tx,
            events: ev_rx,
        }
    }

    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    pub fn try_events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for KernelSession {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Shutdown);
    }
}

/// Pythons worth trying, most specific first: the workspace's own venv
/// (where a project's kernel and its dependencies live), the active venv,
/// then whatever `python3` is on PATH.
pub fn python_candidates(workspace: &Path, virtual_env: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = [".venv", "venv"]
        .iter()
        .map(|d| workspace.join(d).join("bin").join("python"))
        .filter(|p| p.exists())
        .collect();
    if let Some(v) = virtual_env {
        let p = v.join("bin").join("python");
        if p.exists() && !out.contains(&p) {
            out.push(p);
        }
    }
    out.push(PathBuf::from("python3"));
    out
}

fn has_jupyter(python: &Path) -> bool {
    std::process::Command::new(python)
        .args(["-c", "import jupyter_client, ipykernel"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_bridge(
    workspace: &Path,
    cwd: &Path,
    kernel: &str,
    requests: Receiver<Request>,
    events: Sender<Event>,
) {
    use std::io::{BufRead, Write};
    let venv = std::env::var_os("VIRTUAL_ENV").map(PathBuf::from);
    let Some(python) = python_candidates(workspace, venv.as_deref())
        .into_iter()
        .find(|p| has_jupyter(p))
    else {
        let _ = events.send(Event::Error {
            message: "no Python with jupyter_client and ipykernel found (install ipykernel in the project's .venv)".into(),
        });
        return;
    };
    let child = std::process::Command::new(&python)
        .arg("-c")
        .arg(BRIDGE)
        .arg(kernel)
        .arg(cwd)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = events.send(Event::Error {
                message: format!("cannot start {}: {e}", python.display()),
            });
            return;
        }
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return;
    };
    let reader_events = events.clone();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(ev) = serde_json::from_str::<Event>(&line)
                && reader_events.send(ev).is_err()
            {
                break;
            }
        }
    });
    // The session hanging up (its notebook closed) is a shutdown too.
    for request in requests.iter().chain(std::iter::once(Request::Shutdown)) {
        let stop = matches!(request, Request::Shutdown);
        let Ok(mut line) = serde_json::to_string(&request) else {
            continue;
        };
        line.push('\n');
        if stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .is_err()
            || stop
        {
            break;
        }
    }
    drop(stdin);
    let _ = child.wait();
    let _ = reader.join();
}

/// A notebook's kernel, the cells it still owes answers for, and events
/// that arrived while the notebook was not the active tab.
pub struct NotebookRun {
    session: KernelSession,
    pending: Vec<(String, CellRef)>,
    backlog: Vec<Event>,
    /// The kernel's display name, once it is up.
    pub kernel: Option<String>,
    sent: u64,
}

impl NotebookRun {
    pub fn new(session: KernelSession) -> Self {
        Self {
            session,
            pending: Vec::new(),
            backlog: Vec::new(),
            kernel: None,
            sent: 0,
        }
    }

    /// A run whose kernel is the test: it reads what croft sends and
    /// speaks for the kernel.
    #[cfg(test)]
    pub fn for_test() -> (Self, Sender<Event>, Receiver<Request>) {
        let (req_tx, req_rx) = channel();
        let (ev_tx, ev_rx) = channel();
        let session = KernelSession {
            requests: req_tx,
            events: ev_rx,
        };
        (Self::new(session), ev_tx, req_rx)
    }

    /// Send `cell` to the kernel. The kernel runs cells in the order they
    /// were sent, so Run All is simply every cell sent at once.
    pub fn execute(&mut self, cell: CellRef) {
        self.sent += 1;
        let id = format!("{}-{}", cell.index, self.sent);
        self.session.send(Request::Execute {
            id: id.clone(),
            code: cell.source.clone(),
        });
        self.pending.push((id, cell));
    }

    pub fn send(&self, request: Request) {
        self.session.send(request);
    }

    /// Cells running or queued, for the `In [*]` marks.
    pub fn running_indices(&self) -> Vec<usize> {
        self.pending.iter().map(|(_, c)| c.index).collect()
    }

    /// Take what the kernel has said since the last call.
    pub fn collect(&mut self) -> bool {
        let fresh = self.session.try_events();
        let any = !fresh.is_empty() || !self.backlog.is_empty();
        self.backlog.extend(fresh);
        any
    }

    /// Fold the collected events into the notebook's `text`. Returns the
    /// new text when a cell changed, and lines worth showing the user.
    pub fn fold(&mut self, text: &str) -> (Option<String>, Vec<String>) {
        let mut doc = text.to_string();
        let mut changed = false;
        let mut notes = Vec::new();
        for event in std::mem::take(&mut self.backlog) {
            let (id, edit) = match event {
                Event::Ready { display, .. } => {
                    // A second Ready is a restart: the old kernel will never
                    // finish what it had, so nothing still pending can
                    // complete. The bridge settles those cells first; this
                    // is the backstop if it could not.
                    if self.kernel.is_some() {
                        self.pending.clear();
                    }
                    notes.push(format!("Kernel ready: {display}"));
                    self.kernel = Some(display);
                    continue;
                }
                Event::Error { message } => {
                    // A kernel that never came up will answer nothing.
                    if self.kernel.is_none() {
                        self.pending.clear();
                    }
                    notes.push(format!("Kernel: {message}"));
                    continue;
                }
                Event::Output { id, output } => (id, CellEdit::Output(output)),
                Event::Clear { id } => (id, CellEdit::Clear),
                Event::Done {
                    id,
                    execution_count,
                    ..
                } => (id, CellEdit::Done(execution_count)),
            };
            let Some(pos) = self.pending.iter().position(|(p, _)| *p == id) else {
                continue;
            };
            let cell = if matches!(edit, CellEdit::Done(_)) {
                self.pending.remove(pos).1
            } else {
                self.pending[pos].1.clone()
            };
            if let Some(next) = apply(&doc, &cell, edit) {
                doc = next;
                changed = true;
            }
        }
        (changed.then_some(doc), notes)
    }
}

/// The kernel a notebook asks for (`metadata.kernelspec.name`).
pub fn kernel_name(doc_text: &str) -> String {
    serde_json::from_str::<Value>(doc_text)
        .ok()
        .and_then(|d| {
            d.pointer("/metadata/kernelspec/name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "python3".into())
}

/// A code cell as sent to the kernel: where it is and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRef {
    /// Position in `cells` when the run was asked for.
    pub index: usize,
    /// nbformat 4.5's cell id, when the file has one: it finds the cell
    /// again after cells above it were added or removed mid-run.
    pub cell_id: Option<String>,
    pub source: String,
}

/// Every code cell, in order: what Run All sends.
pub fn code_cells(doc_text: &str) -> Vec<CellRef> {
    let Ok(doc) = serde_json::from_str::<Value>(doc_text) else {
        return Vec::new();
    };
    doc["cells"]
        .as_array()
        .map(|cells| {
            cells
                .iter()
                .enumerate()
                .filter(|(_, c)| c["cell_type"] == "code")
                .map(|(i, c)| cell_ref(i, c))
                .collect()
        })
        .unwrap_or_default()
}

/// The code cell at `index`.
pub fn code_cell(doc_text: &str, index: usize) -> Option<CellRef> {
    let doc = serde_json::from_str::<Value>(doc_text).ok()?;
    let cell = doc["cells"].get(index)?;
    (cell["cell_type"] == "code").then(|| cell_ref(index, cell))
}

fn cell_ref(index: usize, cell: &Value) -> CellRef {
    CellRef {
        index,
        cell_id: cell["id"].as_str().map(str::to_string),
        source: joined(&cell["source"]),
    }
}

/// nbformat stores multi-line strings as a list of lines.
fn joined(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// Split text into nbformat's list-of-lines form, each line keeping its
/// newline, as Jupyter writes it.
fn lines_of(text: &str) -> Value {
    Value::Array(
        text.split_inclusive('\n')
            .map(|l| Value::String(l.to_string()))
            .collect(),
    )
}

/// An output as the kernel sent it, with its text fields in the form
/// Jupyter saves.
fn stored(mut output: Value) -> Value {
    if let Some(text) = output
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        output["text"] = lines_of(&text);
    }
    if let Some(data) = output.get_mut("data").and_then(Value::as_object_mut) {
        for (mime, value) in data.iter_mut() {
            let textual = mime.starts_with("text/") || mime == "application/javascript";
            if textual && let Some(text) = value.as_str().map(str::to_string) {
                *value = lines_of(&text);
            }
        }
    }
    output
}

/// A change a kernel event makes to one cell.
pub enum CellEdit {
    /// The run is starting: old outputs and count go.
    Begin,
    Output(Value),
    Clear,
    Done(Option<u64>),
}

/// `doc_text` with `edit` applied to `cell`, or `None` when the cell can no
/// longer be found (deleted mid-run) or the buffer is not valid JSON.
pub fn apply(doc_text: &str, cell: &CellRef, edit: CellEdit) -> Option<String> {
    let mut doc: Value = serde_json::from_str(doc_text).ok()?;
    let cells = doc.get_mut("cells")?.as_array_mut()?;
    let at = match &cell.cell_id {
        Some(id) => cells.iter().position(|c| c["id"] == id.as_str())?,
        None => cell.index,
    };
    let target = cells.get_mut(at)?.as_object_mut()?;
    if target.get("cell_type").and_then(Value::as_str) != Some("code") {
        return None;
    }
    let outputs = target
        .entry("outputs")
        .or_insert_with(|| Value::Array(Vec::new()));
    match edit {
        CellEdit::Begin => {
            *outputs = Value::Array(Vec::new());
            target.insert("execution_count".into(), Value::Null);
        }
        CellEdit::Output(output) => {
            if let Some(list) = outputs.as_array_mut() {
                list.push(stored(output));
            }
        }
        CellEdit::Clear => *outputs = Value::Array(Vec::new()),
        CellEdit::Done(count) => {
            target.insert(
                "execution_count".into(),
                count.map_or(Value::Null, Value::from),
            );
        }
    }
    Some(render(&doc))
}

/// One-space indentation, sorted keys and a trailing newline: nbformat's
/// own layout, so a notebook Jupyter wrote keeps its shape.
fn render(doc: &Value) -> String {
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    if doc.serialize(&mut ser).is_err() {
        return String::new();
    }
    let mut text = String::from_utf8(out).unwrap_or_default();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const NB: &str = "{\n \"cells\": [\n  {\n   \"cell_type\": \"markdown\",\n   \"metadata\": {},\n   \"source\": [\n    \"# t\"\n   ]\n  },\n  {\n   \"cell_type\": \"code\",\n   \"execution_count\": 7,\n   \"id\": \"c-a\",\n   \"metadata\": {},\n   \"outputs\": [\n    {\n     \"name\": \"stdout\",\n     \"output_type\": \"stream\",\n     \"text\": [\n      \"old\\n\"\n     ]\n    }\n   ],\n   \"source\": [\n    \"print(1+1)\\n\",\n    \"40+2\"\n   ]\n  }\n ],\n \"metadata\": {\n  \"kernelspec\": {\n   \"display_name\": \"Python 3\",\n   \"language\": \"python\",\n   \"name\": \"python3\"\n  }\n },\n \"nbformat\": 4,\n \"nbformat_minor\": 5\n}\n";

    #[test]
    fn code_cells_and_the_kernel_come_from_the_notebook() {
        assert_eq!(kernel_name(NB), "python3");
        assert_eq!(kernel_name("{\"cells\": []}"), "python3");
        let cells = code_cells(NB);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].index, 1);
        assert_eq!(cells[0].cell_id.as_deref(), Some("c-a"));
        assert_eq!(cells[0].source, "print(1+1)\n40+2");
        assert_eq!(code_cell(NB, 1), Some(cells[0].clone()));
        assert_eq!(code_cell(NB, 0), None, "a markdown cell does not run");
    }

    #[test]
    fn a_run_replaces_outputs_and_count_in_jupyter_layout() {
        let cell = code_cell(NB, 1).unwrap();
        let t = apply(NB, &cell, CellEdit::Begin).unwrap();
        assert!(!t.contains("old"));
        let t = apply(
            &t,
            &cell,
            CellEdit::Output(
                serde_json::json!({"output_type": "stream", "name": "stdout", "text": "2\n"}),
            ),
        )
        .unwrap();
        let t = apply(
            &t,
            &cell,
            CellEdit::Output(serde_json::json!({"output_type": "execute_result", "execution_count": 1, "data": {"text/plain": "42"}, "metadata": {}})),
        )
        .unwrap();
        let t = apply(&t, &cell, CellEdit::Done(Some(1))).unwrap();
        let expected = NB
            .replace("\"execution_count\": 7", "\"execution_count\": 1")
            .replace(
                "   \"outputs\": [\n    {\n     \"name\": \"stdout\",\n     \"output_type\": \"stream\",\n     \"text\": [\n      \"old\\n\"\n     ]\n    }\n   ],",
                "   \"outputs\": [\n    {\n     \"name\": \"stdout\",\n     \"output_type\": \"stream\",\n     \"text\": [\n      \"2\\n\"\n     ]\n    },\n    {\n     \"data\": {\n      \"text/plain\": [\n       \"42\"\n      ]\n     },\n     \"execution_count\": 1,\n     \"metadata\": {},\n     \"output_type\": \"execute_result\"\n    }\n   ],",
            );
        assert_eq!(t, expected);
    }

    #[test]
    fn a_cell_is_found_by_id_after_cells_move_and_lost_when_deleted() {
        let cell = code_cell(NB, 1).unwrap();
        // A cell inserted above: index 1 is now the new cell, id still finds ours.
        let moved = NB.replacen(
            "  {\n   \"cell_type\": \"markdown\"",
            "  {\n   \"cell_type\": \"code\", \"id\": \"new\", \"metadata\": {}, \"outputs\": [], \"source\": [], \"execution_count\": null\n  },\n  {\n   \"cell_type\": \"markdown\"",
            1,
        );
        let t = apply(&moved, &cell, CellEdit::Done(Some(9))).unwrap();
        let doc: Value = serde_json::from_str(&t).unwrap();
        assert_eq!(doc["cells"][2]["execution_count"], 9);
        assert_eq!(doc["cells"][0]["execution_count"], Value::Null);
        let gone = r#"{"cells": [{"cell_type": "markdown", "source": []}]}"#;
        assert!(apply(gone, &cell, CellEdit::Done(Some(1))).is_none());
    }

    #[test]
    fn candidates_prefer_the_workspace_venv() {
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join(".venv/bin/python");
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(&py, "").unwrap();
        let c = python_candidates(dir.path(), None);
        assert_eq!(c, vec![py, PathBuf::from("python3")]);
    }

    /// Runs a real kernel. Set `CROFT_TEST_JUPYTER_PYTHON` to the `python`
    /// of a venv with ipykernel; the test links that whole venv in as the
    /// workspace's `.venv` (a venv's python moved out of its venv is the
    /// bare base interpreter).
    #[test]
    #[ignore = "needs a Python with ipykernel; set CROFT_TEST_JUPYTER_PYTHON"]
    fn a_real_kernel_runs_prints_and_survives_an_interrupt() {
        let python = PathBuf::from(std::env::var("CROFT_TEST_JUPYTER_PYTHON").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let venv = python.parent().and_then(Path::parent).unwrap();
        std::os::unix::fs::symlink(venv, dir.path().join(".venv")).unwrap();
        let k = KernelSession::start(dir.path(), dir.path(), "python3");
        let wait = |pred: &dyn Fn(&Event) -> bool| -> Vec<Event> {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let mut seen = Vec::new();
            while std::time::Instant::now() < end {
                for ev in k.try_events() {
                    assert!(!matches!(ev, Event::Error { .. }), "{ev:?}");
                    let hit = pred(&ev);
                    seen.push(ev);
                    if hit {
                        return seen;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("timed out; saw {seen:?}");
        };
        k.send(Request::Execute {
            id: "a".into(),
            code: "print(1+1)".into(),
        });
        let seen = wait(&|e| matches!(e, Event::Done { .. }));
        assert!(matches!(seen[0], Event::Ready { .. }), "{seen:?}");
        assert!(
            seen.iter()
                .any(|e| matches!(e, Event::Output { output, .. } if output["text"] == "2\n"))
        );
        k.send(Request::Execute {
            id: "b".into(),
            code: "import time\nwhile True: time.sleep(0.05)".into(),
        });
        std::thread::sleep(std::time::Duration::from_millis(800));
        k.send(Request::Interrupt);
        let seen = wait(&|e| matches!(e, Event::Done { .. }));
        assert!(seen.iter().any(
            |e| matches!(e, Event::Done { id, status, .. } if id == "b" && status == "error")
        ));
        k.send(Request::Execute {
            id: "c".into(),
            code: "6*7".into(),
        });
        let seen = wait(&|e| matches!(e, Event::Done { .. }));
        assert!(seen.iter().any(
            |e| matches!(e, Event::Output { output, .. } if output["data"]["text/plain"] == "42")
        ));
    }

    fn run_without_kernel() -> NotebookRun {
        NotebookRun::for_test().0
    }

    #[test]
    fn folding_events_writes_outputs_and_clears_the_running_mark() {
        let mut run = run_without_kernel();
        let cell = code_cell(NB, 1).unwrap();
        run.execute(cell);
        assert_eq!(run.running_indices(), vec![1]);
        let id = run.pending[0].0.clone();
        run.backlog = vec![
            Event::Ready {
                kernel: "python3".into(),
                display: "Python 3".into(),
            },
            Event::Output {
                id: id.clone(),
                output: serde_json::json!({"output_type": "stream", "name": "stdout", "text": "2\n"}),
            },
            Event::Output {
                id: "someone-else".into(),
                output: serde_json::json!({"output_type": "stream", "name": "stdout", "text": "stray\n"}),
            },
            Event::Done {
                id,
                execution_count: Some(3),
                status: "ok".into(),
            },
        ];
        let (text, notes) = run.fold(NB);
        let doc: Value = serde_json::from_str(&text.unwrap()).unwrap();
        assert_eq!(doc["cells"][1]["execution_count"], 3);
        let outputs = doc["cells"][1]["outputs"].as_array().unwrap();
        assert_eq!(
            outputs.len(),
            2,
            "the old output and the new one; no stray: {outputs:?}"
        );
        assert_eq!(outputs[1]["text"][0], "2\n");
        assert_eq!(notes, vec!["Kernel ready: Python 3".to_string()]);
        assert!(run.running_indices().is_empty());
        assert_eq!(run.kernel.as_deref(), Some("Python 3"));
    }

    #[test]
    fn a_kernel_that_never_starts_leaves_nothing_running() {
        let mut run = run_without_kernel();
        run.execute(code_cell(NB, 1).unwrap());
        run.backlog = vec![Event::Error {
            message: "no Python".into(),
        }];
        let (text, notes) = run.fold(NB);
        assert!(text.is_none());
        assert_eq!(notes, vec!["Kernel: no Python".to_string()]);
        assert!(run.running_indices().is_empty());
    }

    #[test]
    fn a_restart_settles_cells_the_old_kernel_was_running() {
        let mut run = run_without_kernel();
        let ready = || Event::Ready {
            kernel: "python3".into(),
            display: "Python 3".into(),
        };
        run.backlog = vec![ready()];
        run.fold(NB);
        run.execute(code_cell(NB, 1).unwrap());
        assert_eq!(run.running_indices(), vec![1]);
        // The restarted kernel announces itself; the old one never answers.
        run.backlog = vec![ready()];
        run.fold(NB);
        assert!(
            run.running_indices().is_empty(),
            "no cell stays at In [*] across a restart"
        );
        // The first Ready of a run is not a restart: it clears nothing.
        let mut fresh = run_without_kernel();
        fresh.execute(code_cell(NB, 1).unwrap());
        fresh.backlog = vec![ready()];
        fresh.fold(NB);
        assert_eq!(fresh.running_indices(), vec![1]);
    }

    /// A stand-in for `jupyter_client` (#1477): it records which threads
    /// touch the kernel client, and with `FAKE_RESTART_FAIL` set fails the
    /// restart's `wait_for_ready` the way a socket race does. A code cell
    /// containing `hang` never finishes, like a runaway loop.
    const FAKE_JUPYTER: &str = r#"# A stand-in for jupyter_client that records which threads touch the
# kernel's sockets, and can fail a restart the way a socket race does.
import json, os, queue, threading, time

touched = set()


class Spec:
    display_name = "Fake"


class Client:
    def __init__(self):
        self.n = 0
        self.idle = []
        self.up = False
        self.restarted = False

    def mark(self):
        if self.up:
            touched.add(threading.current_thread().name)

    def start_channels(self):
        pass

    def wait_for_ready(self, timeout=None):
        self.mark()
        if self.restarted and os.environ.get("FAKE_RESTART_FAIL"):
            raise RuntimeError("Invalid Signature")
        self.up = True

    def execute(self, code, store_history=True):
        self.mark()
        self.n += 1
        mid = "m%d" % self.n
        if "hang" not in code:
            self.idle.append(mid)
        return mid

    def get_iopub_msg(self, timeout=None):
        self.mark()
        if self.idle:
            mid = self.idle.pop(0)
            return {"parent_header": {"msg_id": mid}, "msg_type": "status",
                    "content": {"execution_state": "idle"}}
        time.sleep(min(timeout or 0, 0.02))
        raise queue.Empty

    def get_shell_msg(self, timeout=None):
        self.mark()
        time.sleep(min(timeout or 0, 0.02))
        raise queue.Empty

    def stop_channels(self):
        with open(os.environ["FAKE_TOUCHED"], "w") as f:
            json.dump(sorted(touched), f)


class KernelManager:
    def __init__(self, kernel_name=None):
        self.kernel_spec = Spec()
        self.c = Client()

    def start_kernel(self, cwd=None):
        pass

    def client(self):
        return self.c

    def restart_kernel(self, now=False):
        self.c.mark()
        self.c.restarted = True

    def interrupt_kernel(self):
        pass

    def shutdown_kernel(self, now=False):
        pass
"#;

    /// Runs the real bridge against the fake kernel with `requests` as its
    /// stdin; returns what it emitted and the threads that used the client.
    fn bridge_against_fake(requests: &[&str], restart_fails: bool) -> (Vec<Event>, Vec<String>) {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("jupyter_client");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("__init__.py"), "").unwrap();
        std::fs::write(pkg.join("manager.py"), FAKE_JUPYTER).unwrap();
        let touched = tmp.path().join("touched.json");
        let mut cmd = std::process::Command::new("python3");
        cmd.arg("-c")
            .arg(BRIDGE)
            .arg("python3")
            .arg(tmp.path())
            .env("PYTHONPATH", tmp.path())
            .env("FAKE_TOUCHED", &touched)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if restart_fails {
            cmd.env("FAKE_RESTART_FAIL", "1");
        }
        let mut child = cmd.spawn().unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            for r in requests {
                writeln!(stdin, "{r}").unwrap();
            }
            // Let the bridge work through them before it is told to stop.
            std::thread::sleep(std::time::Duration::from_millis(800));
            writeln!(stdin, r#"{{"op":"shutdown"}}"#).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let events = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let touched: Vec<String> = std::fs::read_to_string(&touched)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        (events, touched)
    }

    const RUN_HANG_RESTART_RUN: [&str; 4] = [
        r#"{"op":"execute","id":"a","code":"x = 1"}"#,
        r#"{"op":"execute","id":"h","code":"hang"}"#,
        r#"{"op":"restart"}"#,
        r#"{"op":"execute","id":"b","code":"print('after')"}"#,
    ];

    fn done_status(events: &[Event], cell: &str) -> Option<String> {
        events.iter().find_map(|e| match e {
            Event::Done { id, status, .. } if id == cell => Some(status.clone()),
            _ => None,
        })
    }

    #[test]
    fn one_thread_owns_the_kernel_sockets_across_a_restart() {
        if !crate::lsp::manager::is_on_path("python3") {
            eprintln!("SKIPPED: python3 not on PATH");
            return;
        }
        let (events, touched) = bridge_against_fake(&RUN_HANG_RESTART_RUN, false);
        // zmq sockets are not thread-safe: a second reader is what lost the
        // restart's reply and left cells at In [*].
        assert_eq!(
            touched.len(),
            1,
            "threads that used the client: {touched:?}"
        );
        let readies = events
            .iter()
            .filter(|e| matches!(e, Event::Ready { .. }))
            .count();
        assert_eq!(
            readies, 2,
            "the restart must announce the new kernel: {events:?}"
        );
        assert_eq!(
            done_status(&events, "b").as_deref(),
            Some("ok"),
            "{events:?}"
        );
    }

    #[test]
    fn a_failed_restart_still_settles_the_cells_it_lost() {
        if !crate::lsp::manager::is_on_path("python3") {
            eprintln!("SKIPPED: python3 not on PATH");
            return;
        }
        let (events, _) = bridge_against_fake(&RUN_HANG_RESTART_RUN, true);
        assert_eq!(
            done_status(&events, "h").as_deref(),
            Some("error"),
            "the runaway cell must not stay at In [*]: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Error { message } if message.starts_with("restart:")
            )),
            "the failure is reported: {events:?}"
        );
        // The bridge keeps serving after it.
        assert_eq!(
            done_status(&events, "b").as_deref(),
            Some("ok"),
            "{events:?}"
        );
    }

    /// Negative: without a restart, a finished cell is reported once, as ok,
    /// and nothing is settled as failed.
    #[test]
    fn cells_without_a_restart_finish_ok() {
        if !crate::lsp::manager::is_on_path("python3") {
            eprintln!("SKIPPED: python3 not on PATH");
            return;
        }
        let (events, _) = bridge_against_fake(
            &[
                r#"{"op":"execute","id":"a","code":"x = 1"}"#,
                r#"{"op":"execute","id":"b","code":"y = 2"}"#,
            ],
            false,
        );
        assert_eq!(done_status(&events, "a").as_deref(), Some("ok"));
        assert_eq!(done_status(&events, "b").as_deref(), Some("ok"));
        assert!(
            !events.iter().any(|e| matches!(e, Event::Error { .. })),
            "{events:?}"
        );
    }
}
