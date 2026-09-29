//! A client for the CodeQL CLI's query server (`codeql execute
//! query-server2`), for quick evaluation (#578).
//!
//! The server speaks JSON-RPC 2.0 over its stdio with LSP-style
//! `Content-Length` framing, as VS Code's CodeQL extension drives it. Each
//! request's params are `{ "body": …, "progressId": n }`. Two requests are
//! used: `evaluation/registerDatabases`, then `evaluation/runQuery` with a
//! `quickEval` target naming the selected span. The answer carries a
//! `resultType` (0 is success) and the CLI's message; `ql/progressUpdated`
//! notifications arrive meanwhile and are skipped.

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// A span of a query file to evaluate on its own: 1-based line and
/// column of its start, and of its end (inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// What a quick evaluation is of, for the query history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickEval {
    pub span: Span,
    /// Count the tuples rather than list them.
    pub count: bool,
    /// The selected text, for the history label.
    pub label: String,
}

impl QuickEval {
    /// The history label: "Quick evaluation of isCall" / "Quick evaluation
    /// count of isCall".
    pub fn title(&self) -> String {
        let kind = if self.count {
            "Quick evaluation count"
        } else {
            "Quick evaluation"
        };
        format!("{kind} of {}", self.label)
    }
}

/// One framed message.
pub fn frame(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Read one framed message; `None` at the end of the stream.
pub fn read_message(r: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let mut body = vec![0u8; len.unwrap_or(0)];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// A request, wrapped the way the query server expects.
pub fn request(id: u64, method: &str, body: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": { "body": body, "progressId": id },
    })
}

/// The `evaluation/runQuery` body for a quick evaluation of `span` in
/// `query` on `db`, writing its tuples (or counts) to `out`.
pub fn run_query_body(
    db: &Path,
    query: &Path,
    out: &Path,
    packs: &[PathBuf],
    span: Span,
    count: bool,
) -> Value {
    let mut target = json!({
        "quickEvalPos": {
            "fileName": query.display().to_string(),
            "line": span.line,
            "column": span.column,
            "endLine": span.end_line,
            "endColumn": span.end_column,
        }
    });
    if count {
        target["countOnly"] = json!(true);
    }
    json!({
        "db": db.display().to_string(),
        "queryPath": query.display().to_string(),
        "outputPath": out.display().to_string(),
        "additionalPacks": packs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "externalInputs": {},
        "singletonExternalInputs": {},
        "target": { "quickEval": target },
    })
}

/// The JSON-RPC error an answer carries, if any.
pub fn answer_error(answer: &Value) -> Option<String> {
    let err = answer.get("error")?;
    Some(
        err.get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("the query server failed")
            .to_string(),
    )
}

/// Why a `runQuery` answer is not a success, or `None` when it is.
pub fn run_query_failure(answer: &Value) -> Option<String> {
    if let Some(err) = answer_error(answer) {
        return Some(err);
    }
    let result = answer.get("result")?;
    if result.get("resultType").and_then(|t| t.as_i64()) == Some(0) {
        return None;
    }
    let message = result
        .get("message")
        .and_then(|m| m.as_str())
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("the evaluation failed");
    Some(message.to_string())
}

/// Quick-evaluate `span` of `query` on `db` with `program`'s query server,
/// writing the BQRS to `out`. The server is killed as soon as `cancel` is
/// raised, and when the answer is in.
#[allow(clippy::too_many_arguments)]
pub fn run_quick_eval(
    program: &Path,
    db: &Path,
    query: &Path,
    out: &Path,
    packs: &[PathBuf],
    span: Span,
    count: bool,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let mut child = std::process::Command::new(program)
        .args(["execute", "query-server2"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the query server: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("the query server has no input")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("the query server has no output")?;
    // Read on a thread of its own, so a cancel is seen while it evaluates.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut r = std::io::BufReader::new(stdout);
        while let Ok(Some(msg)) = read_message(&mut r) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    let mut stop = |why: String| -> Result<(), String> {
        let _ = child.kill();
        let _ = child.wait();
        Err(why)
    };
    let requests = [
        request(
            1,
            "evaluation/registerDatabases",
            json!({ "databases": [db.display().to_string()] }),
        ),
        request(
            2,
            "evaluation/runQuery",
            run_query_body(db, query, out, packs, span, count),
        ),
    ];
    for req in requests {
        let id = req["id"].clone();
        if let Err(e) = stdin.write_all(&frame(&req)).and_then(|()| stdin.flush()) {
            return stop(format!("the query server stopped: {e}"));
        }
        let answer = loop {
            if cancel.load(Ordering::SeqCst) {
                return stop(String::from("cancelled"));
            }
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(msg) if msg.get("id") == Some(&id) => break msg,
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return stop(String::from("the query server stopped unexpectedly"));
                }
            }
        };
        let failure = if req["method"] == "evaluation/runQuery" {
            run_query_failure(&answer)
        } else {
            answer_error(&answer)
        };
        if let Some(why) = failure {
            return stop(why);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// The span to quick-evaluate in `lines` for a cursor at 0-based
/// (`row`, `col`) and an optional selection (0-based, end exclusive): the
/// selection when there is one on one line, else the identifier under the
/// cursor. Returned 1-based with an inclusive end, and the selected text.
pub fn span_at(
    lines: &[String],
    row: usize,
    col: usize,
    selection: Option<((usize, usize), (usize, usize))>,
) -> Option<(Span, String)> {
    if let Some(((r0, c0), (r1, c1))) = selection
        && (r0, c0) != (r1, c1)
    {
        let text: String = if r0 == r1 {
            lines.get(r0)?.chars().skip(c0).take(c1 - c0).collect()
        } else {
            lines.get(r0)?.chars().skip(c0).collect()
        };
        let span = Span {
            line: r0 as u32 + 1,
            column: c0 as u32 + 1,
            end_line: r1 as u32 + 1,
            end_column: c1.max(1) as u32,
        };
        return Some((span, text.trim().to_string()));
    }
    let chars: Vec<char> = lines.get(row)?.chars().collect();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let at = col.min(chars.len());
    let mut start = at;
    while start > 0 && word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < chars.len() && word(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let text: String = chars[start..end].iter().collect();
    Some((
        Span {
            line: row as u32 + 1,
            column: start as u32 + 1,
            end_line: row as u32 + 1,
            end_column: end as u32,
        },
        text,
    ))
}

/// QL words that open a formula or an aggregate: followed by `(` but
/// not predicates, so never what a breakpoint means.
const NOT_PREDICATES: [&str; 20] = [
    "exists",
    "forall",
    "forex",
    "not",
    "count",
    "strictcount",
    "sum",
    "strictsum",
    "min",
    "max",
    "avg",
    "concat",
    "strictconcat",
    "rank",
    "any",
    "none",
    "unique",
    "if",
    "and",
    "or",
];

/// What a breakpoint on 1-based `line` (whose text is `text`) evaluates,
/// the way VS Code's CodeQL debugger stops at one (#578): the predicate
/// the line defines or calls, which is the first name followed by `(`
/// that is not a formula word. The query server takes that name as a
/// quick-evaluation target; a whole line it refuses.
pub fn breakpoint_target(text: &str, line: u32) -> Option<(Span, String)> {
    let chars: Vec<char> = text.chars().collect();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = 0;
    while i < chars.len() {
        if !(chars[i].is_alphabetic() || chars[i] == '_') || (i > 0 && word(chars[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && word(chars[i]) {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        let mut j = i;
        while j < chars.len() && chars[j] == ' ' {
            j += 1;
        }
        if chars.get(j) == Some(&'(') && !NOT_PREDICATES.contains(&name.as_str()) {
            return Some((
                Span {
                    line,
                    column: start as u32 + 1,
                    end_line: line,
                    end_column: i as u32,
                },
                name,
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_breakpoint_evaluates_the_predicate_its_line_defines_or_calls() {
        // Both spans were checked against the real query server (2.27.1):
        // the name is a valid target, the whole line and the call are not.
        let (span, name) = breakpoint_target("predicate isCall(Call c) { exists(c) }", 3).unwrap();
        assert_eq!(name, "isCall");
        assert_eq!((span.line, span.column, span.end_column), (3, 11, 16));
        let (span, name) = breakpoint_target("where isCall(c)", 6).unwrap();
        assert_eq!(
            (name.as_str(), span.column, span.end_column),
            ("isCall", 7, 12)
        );
        assert_eq!(
            breakpoint_target("  exists(Call c | isCall (c))", 1)
                .map(|t| t.1)
                .as_deref(),
            Some("isCall"),
            "formula words are skipped"
        );
        assert_eq!(breakpoint_target("select c", 7), None);
        assert_eq!(breakpoint_target("from Call c", 5), None);
    }

    #[test]
    fn messages_are_framed_and_read_back() {
        let msg = request(2, "evaluation/runQuery", json!({"a": 1}));
        let bytes = frame(&msg);
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "), "{text}");
        let mut stream = bytes.clone();
        stream.extend_from_slice(&bytes);
        let mut r = std::io::BufReader::new(stream.as_slice());
        assert_eq!(read_message(&mut r).unwrap(), Some(msg.clone()));
        assert_eq!(read_message(&mut r).unwrap(), Some(msg.clone()));
        assert_eq!(read_message(&mut r).unwrap(), None);
        assert_eq!(msg["params"]["progressId"], 2);
        assert_eq!(msg["params"]["body"]["a"], 1);
    }

    #[test]
    fn the_run_query_body_names_the_span_and_the_count() {
        let span = Span {
            line: 3,
            column: 11,
            end_line: 3,
            end_column: 16,
        };
        let body = run_query_body(
            Path::new("/db"),
            Path::new("/w/Q.ql"),
            Path::new("/r/out.bqrs"),
            &[PathBuf::from("/w")],
            span,
            false,
        );
        assert_eq!(
            body["target"],
            json!({"quickEval": {"quickEvalPos": {"fileName": "/w/Q.ql", "line": 3, "column": 11, "endLine": 3, "endColumn": 16}}})
        );
        assert_eq!(body["additionalPacks"], json!(["/w"]));
        let count = run_query_body(
            Path::new("/db"),
            Path::new("/w/Q.ql"),
            Path::new("/r/out.bqrs"),
            &[],
            span,
            true,
        );
        assert_eq!(count["target"]["quickEval"]["countOnly"], true);
    }

    #[test]
    fn answers_read_as_success_or_the_clis_reason() {
        // The real query server's answers (2.27.1).
        let ok = json!({"jsonrpc": "2.0", "id": 2, "result": {"resultType": 0, "message": "", "evaluationTime": 384}});
        assert_eq!(run_query_failure(&ok), None);
        let bad = json!({"jsonrpc": "2.0", "id": 2, "result": {"resultType": 2, "message": "ERROR: The selection is not a valid quick-eval target. (/w/Q.ql:1,1-6)\n", "evaluationTime": -1}});
        assert_eq!(
            run_query_failure(&bad).as_deref(),
            Some("ERROR: The selection is not a valid quick-eval target. (/w/Q.ql:1,1-6)")
        );
        let err = json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32603, "message": "Internal error."}});
        assert_eq!(run_query_failure(&err).as_deref(), Some("Internal error."));
        // Other requests fail only by a JSON-RPC error.
        assert_eq!(answer_error(&json!({"id": 1, "result": {}})), None);
        assert_eq!(answer_error(&err).as_deref(), Some("Internal error."));
    }

    #[test]
    fn the_span_is_the_selection_or_the_word_at_the_cursor() {
        let lines = vec![
            String::from("import python"),
            String::new(),
            String::from("predicate isCall(Call c) { exists(c) }"),
        ];
        let (span, text) = span_at(&lines, 2, 13, None).unwrap();
        assert_eq!(text, "isCall");
        assert_eq!(
            span,
            Span {
                line: 3,
                column: 11,
                end_line: 3,
                end_column: 16
            },
            "1-based, end inclusive, as the server wants"
        );
        let (span, text) = span_at(&lines, 0, 0, Some(((2, 27), (2, 36)))).unwrap();
        assert_eq!(text, "exists(c)");
        assert_eq!((span.column, span.end_column), (28, 36));
        assert_eq!(
            span_at(&lines, 1, 0, None),
            None,
            "nothing under the cursor"
        );
    }
}
