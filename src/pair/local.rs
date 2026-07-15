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

/// Bearer for keyed Anthropic-compatible gateways; local servers ignore it.
/// Environment-only on purpose: a token must never land in `pair.json`.
fn api_key() -> String {
    std::env::var("ANTHROPIC_AUTH_TOKEN").unwrap_or_else(|_| String::from("croft"))
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
    let resp = agent
        .post(&format!("{base_url}/v1/messages"))
        .set("content-type", "application/json")
        .set("x-api-key", &api_key())
        .send_string(&req.to_string());
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            end_turn(
                state,
                turn_tx,
                true,
                format!("local endpoint {base_url}: {e}"),
            );
            return;
        }
    };

    let mut fence = FenceMachine::new();
    let mut assistant = String::new();
    for line in std::io::BufReader::new(resp.into_reader()).lines() {
        let Ok(line) = line else {
            break; // endpoint hung up mid-stream; finish() reverts open fences
        };
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if v["type"] == "content_block_delta"
            && v["delta"]["type"] == "text_delta"
            && let Some(text) = v["delta"]["text"].as_str()
        {
            assistant.push_str(text);
            for event in fence.push(text) {
                apply_fence_event(state, event);
            }
        }
    }
    for event in fence.finish() {
        apply_fence_event(state, event);
    }
    messages.push(json!({ "role": "assistant", "content": assistant }));
    end_turn(state, turn_tx, false, assistant);
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
