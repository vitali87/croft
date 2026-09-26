//! Inline AI completions (#607): ghost text at the caret, accepted with Tab.
//!
//! After a pause in typing the editor asks a model for the text that belongs
//! at the caret, given the lines before and after it (fill-in-the-middle),
//! and paints the answer as dim ghost text. `Tab` inserts it, `Esc` drops it,
//! and any edit makes it stale, since it answered for text that no longer
//! reads the same.
//!
//! # Where the model comes from
//!
//! The navigator's configuration, when the workspace seats a local one
//! (`croft pair` with an Anthropic-compatible endpoint such as Ollama);
//! otherwise the Claude API with `ANTHROPIC_API_KEY`. Either can be
//! overridden with `CROFT_COMPLETE_URL` / `CROFT_COMPLETE_MODEL`. Nothing is
//! sent until the user turns suggestions on: this ships code off the machine.
//!
//! # Why one blocking request per pause, on its own thread
//!
//! A suggestion is a few dozen tokens, so a non-streaming request is simplest
//! and fastest to first paint. The worker only ever runs the newest request:
//! typing outpaces the network, and an answer for text the user has since
//! changed is thrown away on arrival anyway.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde_json::{Value, json};

/// The default model on the Claude API. `CROFT_COMPLETE_MODEL` overrides it.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// Quiet time after the last edit before a suggestion is asked for.
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// Lines of context sent before and after the caret.
const LINES_BEFORE: usize = 80;
const LINES_AFTER: usize = 30;

/// A suggestion is short; the model is told so, and this bounds it.
const MAX_TOKENS: u64 = 256;

/// The whole request's wall-clock budget: a suggestion later than this is
/// no longer wanted.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

const SYSTEM: &str = "You are a code completion engine inside a text editor. The user message is a file with the marker <CURSOR> where the caret is. Reply with ONLY the text to insert at <CURSOR>: no explanation, no code fences, no repetition of text already before or after the marker. Prefer completing the current line; continue onto following lines only when the next lines are obvious. If nothing should be inserted, reply with nothing.";

/// Which backend answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// The Claude API.
    Claude { base_url: String, api_key: String },
    /// A local Anthropic-compatible endpoint (Ollama, LM Studio, ...).
    Local { base_url: String },
}

/// A resolved backend and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub backend: Backend,
    pub model: String,
}

/// Resolve where suggestions come from, or say why none can: a local
/// navigator first, then the Claude API key, then nothing.
pub fn resolve(
    local: Option<(String, Option<String>)>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Config, String> {
    let model_override = env("CROFT_COMPLETE_MODEL").filter(|m| !m.is_empty());
    let url_override = env("CROFT_COMPLETE_URL").filter(|u| !u.is_empty());
    if let Some((base_url, model)) = local {
        let Some(model) = model_override.or(model) else {
            return Err(String::from(
                "the local navigator names no model; set CROFT_COMPLETE_MODEL",
            ));
        };
        return Ok(Config {
            backend: Backend::Local {
                base_url: url_override.unwrap_or(base_url),
            },
            model,
        });
    }
    match env("ANTHROPIC_API_KEY").filter(|k| !k.is_empty()) {
        Some(api_key) => Ok(Config {
            backend: Backend::Claude {
                base_url: url_override.unwrap_or_else(|| String::from("https://api.anthropic.com")),
                api_key,
            },
            model: model_override.unwrap_or_else(|| String::from(DEFAULT_MODEL)),
        }),
        None => Err(String::from(
            "set ANTHROPIC_API_KEY, or seat a local model with croft pair",
        )),
    }
}

/// The prompt: the window of lines around the caret with `<CURSOR>` at it.
/// `row` and `col` are 0-based, `col` in characters.
pub fn prompt(lines: &[String], row: usize, col: usize) -> String {
    let start = row.saturating_sub(LINES_BEFORE);
    let end = (row + LINES_AFTER + 1).min(lines.len());
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate().take(end).skip(start) {
        if i == row {
            let byte = line
                .char_indices()
                .nth(col)
                .map(|(b, _)| b)
                .unwrap_or(line.len());
            out.push_str(&line[..byte]);
            out.push_str("<CURSOR>");
            out.push_str(&line[byte..]);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// The request body for `config`.
pub fn request_body(config: &Config, prompt: &str) -> Value {
    let mut body = json!({
        "model": config.model,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM,
        "messages": [{ "role": "user", "content": prompt }],
    });
    if matches!(config.backend, Backend::Claude { .. }) {
        // Low effort: a completion wants speed, not deliberation. A refusal
        // re-runs server-side on Anthropic's recommended model instead of
        // coming back as no suggestion for no visible reason.
        body["output_config"] = json!({ "effort": "low" });
        body["fallbacks"] = json!("default");
    }
    body
}

/// The suggestion in a response, cleaned: the text blocks joined, a code
/// fence the model added anyway removed, and `None` for a refusal, an error
/// or an empty answer.
pub fn parse_response(body: &Value) -> Option<String> {
    if body.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return None;
    }
    let text: String = body
        .get("content")?
        .as_array()?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    let cleaned = strip_fence(&text);
    (!cleaned.trim().is_empty()).then(|| cleaned.to_string())
}

fn strip_fence(text: &str) -> &str {
    let t = text.trim_end();
    let Some(rest) = t.strip_prefix("```") else {
        return text.trim_end_matches('\n');
    };
    let body = rest.split_once('\n').map(|(_, b)| b).unwrap_or("");
    body.strip_suffix("```")
        .unwrap_or(body)
        .trim_end_matches('\n')
}

/// One suggestion request, tagged with what it answers for.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub config: Config,
    pub prompt: String,
}

/// The worker's answer to [`Job`] `id`.
#[derive(Debug)]
pub struct Done {
    pub id: u64,
    pub result: Result<Option<String>, String>,
}

/// The background requester: one thread, newest job wins.
pub struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
}

impl Worker {
    pub fn spawn() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        std::thread::Builder::new()
            .name("croft-inline-complete".into())
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    while let Ok(newer) = job_rx.try_recv() {
                        job = newer;
                    }
                    let result = send(&job.config, &job.prompt);
                    if done_tx.send(Done { id: job.id, result }).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn inline-complete thread");
        Worker {
            tx: job_tx,
            rx: done_rx,
        }
    }

    pub fn submit(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    pub fn try_recv(&self) -> Option<Done> {
        self.rx.try_recv().ok()
    }
}

/// POST one request and parse the suggestion out of the answer.
fn send(config: &Config, prompt: &str) -> Result<Option<String>, String> {
    let agent = ureq3::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        // A redirect would replay the API key to wherever it points.
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let (base_url, key) = match &config.backend {
        Backend::Claude { base_url, api_key } => (base_url.as_str(), Some(api_key.as_str())),
        Backend::Local { base_url } => (base_url.as_str(), None),
    };
    let mut req = agent
        .post(format!("{}/v1/messages", base_url.trim_end_matches('/')))
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01");
    if let Some(key) = key {
        req = req
            .header("x-api-key", key)
            .header("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let mut resp = req
        .send(request_body(config, prompt).to_string())
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading the answer: {e}"))?;
    if !status.is_success() {
        let why = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("{status}: {why}"));
    }
    let body: Value = serde_json::from_str(&text).map_err(|e| format!("unreadable answer: {e}"))?;
    Ok(parse_response(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            owned
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn a_local_navigator_wins_then_the_api_key_then_nothing() {
        let local = Some((
            String::from("http://localhost:11434"),
            Some(String::from("qwen")),
        ));
        assert_eq!(
            resolve(local, env_of(&[("ANTHROPIC_API_KEY", "k")])),
            Ok(Config {
                backend: Backend::Local {
                    base_url: String::from("http://localhost:11434")
                },
                model: String::from("qwen"),
            })
        );
        assert_eq!(
            resolve(None, env_of(&[("ANTHROPIC_API_KEY", "k")])).map(|c| c.model),
            Ok(String::from(DEFAULT_MODEL))
        );
        assert!(resolve(None, env_of(&[])).is_err());
        assert_eq!(
            resolve(
                None,
                env_of(&[("ANTHROPIC_API_KEY", "k"), ("CROFT_COMPLETE_MODEL", "m")])
            )
            .map(|c| c.model),
            Ok(String::from("m"))
        );
    }

    #[test]
    fn the_prompt_marks_the_caret_in_a_window_of_lines() {
        let lines: Vec<String> = ["fn main() {", "    let x = ", "}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            prompt(&lines, 1, 12),
            "fn main() {\n    let x = <CURSOR>\n}\n"
        );
    }

    #[test]
    fn the_claude_request_asks_for_low_effort_and_server_side_fallbacks() {
        let claude = Config {
            backend: Backend::Claude {
                base_url: String::from("https://api.anthropic.com"),
                api_key: String::from("k"),
            },
            model: String::from(DEFAULT_MODEL),
        };
        let body = request_body(&claude, "p");
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["messages"][0]["content"], "p");
        let local = Config {
            backend: Backend::Local {
                base_url: String::from("http://localhost:11434"),
            },
            model: String::from("qwen"),
        };
        let body = request_body(&local, "p");
        assert!(
            body.get("fallbacks").is_none(),
            "local servers get the minimal payload"
        );
    }

    #[test]
    fn responses_are_cleaned_and_refusals_give_nothing() {
        let ok = json!({"stop_reason": "end_turn", "content": [
            {"type": "thinking", "thinking": ""},
            {"type": "text", "text": "42;\n"}
        ]});
        assert_eq!(parse_response(&ok).as_deref(), Some("42;"));
        let fenced = json!({"content": [{"type": "text", "text": "```rust\nfoo()\n```"}]});
        assert_eq!(parse_response(&fenced).as_deref(), Some("foo()"));
        let refused = json!({"stop_reason": "refusal", "content": []});
        assert_eq!(parse_response(&refused), None);
        let empty = json!({"content": [{"type": "text", "text": "  "}]});
        assert_eq!(parse_response(&empty), None);
    }
}
