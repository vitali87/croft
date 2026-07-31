//! Local-model transport for the pilot: one minimal Anthropic-compatible
//! `/v1/messages` streaming call per turn (Ollama, LM Studio, llama.cpp,
//! vLLM). Deliberately NOT the claude CLI: driving the full agent at a local
//! endpoint ships ~213 KB of tool schemas per turn and 500s on every Ollama
//! tested (docs/MULTIPLAYER.md, "local models"). Everything downstream of a
//! text delta — the fence machine, the apply path, notes, cancel — is the
//! shared pair machinery; only the transport differs. The endpoint is
//! stateless, so the caller owns the conversation as a message list.

use std::io::BufRead;
use std::sync::Mutex;
use std::sync::mpsc::Sender;
use std::time::Duration;

use serde_json::{Value, json};

use super::{FenceMachine, PairState, TurnEnd, apply_fence_event};

/// Ceiling for one turn's reply; fenced edits are short, and local servers
/// reject nothing under their context limit.
const MAX_TOKENS: u64 = 8192;

/// The auth headers for `base_url`: `(x-api-key value, Authorization
/// value)`. Keyed Anthropic-compatible gateways read the environment token
/// as `Authorization: Bearer` (Anthropic's own `ANTHROPIC_AUTH_TOKEN`
/// convention); it is also mirrored into `x-api-key` for gateways of the
/// other style. The credential only ever travels over https or to a
/// loopback host - a cleartext remote hop gets the harmless placeholder
/// instead, so the token cannot be sniffed off the wire. Environment-only
/// on purpose: a token must never land in `pair.json`.
pub(crate) fn auth_for(base_url: &str, token: Option<&str>) -> (String, Option<String>) {
    let allowed = base_url.starts_with("https://")
        || base_url
            .strip_prefix("http://")
            .is_some_and(|rest| loopback_host(rest.split('/').next().unwrap_or("")));
    match token {
        Some(t) if allowed => (t.to_string(), Some(format!("Bearer {t}"))),
        _ => (String::from("croft"), None),
    }
}

/// Whether `host_port` (authority without scheme or path) is loopback.
fn loopback_host(host_port: &str) -> bool {
    if let Some(rest) = host_port.strip_prefix('[') {
        return rest.split(']').next() == Some("::1");
    }
    matches!(
        host_port.split(':').next().unwrap_or(""),
        "localhost" | "127.0.0.1"
    )
}

/// POST one streaming `/v1/messages` turn and drive its text deltas through
/// the fence machine into the collab seat. `messages` must already end with
/// the user message; on success the assistant's full text is appended so the
/// next turn carries the conversation. Always ends the turn: state flags
/// reset and a [`TurnEnd`] sent, error or not. A cancel that landed
/// mid-stream keeps draining the HTTP body (no mid-request abort) but
/// applies nothing — the shared `cancelled` flag gates the apply path.
pub(crate) fn stream_turn(
    base_url: &str,
    model: &str,
    system: &str,
    messages: &mut Vec<Value>,
    state: &Mutex<PairState>,
    turn_tx: &Sender<TurnEnd>,
) {
    let req = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": system,
        "messages": messages,
    });
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        // Per-read, not per-request: a cold model may take minutes to load
        // before its first token, but the whole stream may run far longer.
        .timeout_read(Duration::from_secs(300))
        .build();
    let token = std::env::var("ANTHROPIC_AUTH_TOKEN").ok();
    let (api_key, bearer) = auth_for(base_url, token.as_deref());
    let mut post = agent
        .post(&format!("{base_url}/v1/messages"))
        .set("content-type", "application/json")
        .set("anthropic-version", "2023-06-01")
        .set("x-api-key", &api_key);
    if let Some(bearer) = &bearer {
        post = post.set("authorization", bearer);
    }
    let resp = match post.send_string(&req.to_string()) {
        Ok(r) => r,
        Err(e) => {
            // The endpoint never answered the turn: pop the user message so
            // the conversation stays balanced. Leaving it made the NEXT turn
            // send [user, user] - a hard 400 on strict gateways for the rest
            // of the seat's life, or a silent re-submit of the unanswered
            // ask on permissive local servers.
            messages.pop();
            end_turn(
                state,
                turn_tx,
                true,
                format!("local endpoint {base_url}: {}", error_detail(e)),
            );
            return;
        }
    };

    let mut fence = FenceMachine::new();
    let mut assistant = String::new();
    // Outcome honesty: the turn only succeeded if the stream ENDED cleanly
    // (message_stop / [DONE]) with untruncated text. A mid-stream drop, the
    // read timeout, an SSE error frame, or a token-limit cut all used to be
    // reported as a finished turn while the partial edit had been reverted.
    let mut clean_end = false;
    let mut truncated = false;
    let mut stream_error: Option<String> = None;
    // Total wall-clock ceiling: the 300s socket timeout is per READ, so a
    // server trickling bytes could otherwise hold the seat busy forever.
    let deadline = std::time::Instant::now() + TURN_DEADLINE;
    for line in std::io::BufReader::new(resp.into_reader()).lines() {
        if std::time::Instant::now() > deadline {
            stream_error = Some(String::from("turn exceeded the 10 minute ceiling"));
            break;
        }
        let Ok(line) = line else {
            break; // endpoint hung up mid-stream; finish() reverts open fences
        };
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data.trim() == "[DONE]" {
            clean_end = true;
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        match v["type"].as_str().unwrap_or("") {
            "content_block_delta" => {
                if v["delta"]["type"] == "text_delta"
                    && let Some(text) = v["delta"]["text"].as_str()
                {
                    assistant.push_str(text);
                    for event in fence.push(text) {
                        apply_fence_event(state, event);
                    }
                }
            }
            "message_delta" if v["delta"]["stop_reason"] == "max_tokens" => {
                truncated = true;
            }
            "message_stop" => {
                clean_end = true;
                break;
            }
            "error" => {
                let msg = v["error"]["message"].as_str().unwrap_or("stream error");
                stream_error = Some(msg.to_string());
                break;
            }
            _ => {}
        }
    }
    for event in fence.finish() {
        apply_fence_event(state, event);
    }
    let failure = if let Some(err) = stream_error {
        Some(err)
    } else if truncated {
        Some(format!("reply truncated at the {MAX_TOKENS}-token limit"))
    } else if !clean_end {
        Some(String::from("stream ended before message_stop"))
    } else if assistant.is_empty() {
        // An empty assistant message is itself a 400 on the next turn.
        Some(String::from("endpoint streamed no text"))
    } else {
        None
    };
    match failure {
        Some(reason) => {
            messages.pop(); // balanced: the next ask starts a clean exchange
            end_turn(
                state,
                turn_tx,
                true,
                format!("local endpoint {base_url}: {reason}"),
            );
        }
        None => {
            messages.push(json!({ "role": "assistant", "content": assistant }));
            end_turn(state, turn_tx, false, assistant);
        }
    }
}

/// Total wall-clock ceiling for one turn (the socket timeout is per read).
const TURN_DEADLINE: Duration = Duration::from_secs(600);

/// The failure text for a request error, with any HTTP error body included:
/// the body carries the fix ("model 'x' not found, try pulling it first"),
/// and ureq's Display is just the status code.
fn error_detail(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let body = body.trim();
            if body.is_empty() {
                format!("status {code}")
            } else {
                let short: String = body.chars().take(200).collect();
                format!("status {code}: {short}")
            }
        }
        other => other.to_string(),
    }
}

/// The local twin of the claude reader's `result` handling: reset the
/// per-turn flags and report the turn's end.
fn end_turn(state: &Mutex<PairState>, turn_tx: &Sender<TurnEnd>, is_error: bool, text: String) {
    let mut st = state.lock().unwrap();
    let cancelled = st.cancelled;
    st.turn_active = false;
    st.cancelled = false;
    st.discarding = false;
    st.comment_only = false;
    drop(st);
    let _ = turn_tx.send(TurnEnd {
        is_error,
        cancelled,
        text,
    });
}
