//! Local-model transport for the pilot: one minimal Anthropic-compatible
//! `/v1/messages` streaming call per turn (Ollama, LM Studio, llama.cpp,
//! vLLM). Deliberately NOT the claude CLI: driving the full agent at a local
//! endpoint ships ~213 KB of tool schemas per turn and 500s on every Ollama
//! tested (docs/MULTIPLAYER.md, "local models"). Everything downstream of a
//! text delta — the fence machine, the apply path, notes, cancel — is the
//! shared pair machinery; only the transport differs. The endpoint is
//! stateless, so the caller owns the conversation as a message list.

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
    match token {
        Some(t) if credential_allowed(base_url) => (t.to_string(), Some(format!("Bearer {t}"))),
        _ => (String::from("croft"), None),
    }
}

/// Whether the credential may travel to `base_url`: https anywhere, http
/// only to a loopback host. A real URL parse, not a string scan - the
/// authority's userinfo made `http://localhost:pass@evil.example` read as
/// loopback under a colon-split check while the actual destination was the
/// remote host.
fn credential_allowed(base_url: &str) -> bool {
    let Ok(u) = url::Url::parse(base_url) else {
        return false;
    };
    match u.scheme() {
        "https" => true,
        "http" => match u.host() {
            Some(url::Host::Domain(d)) => d == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
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
    stream_turn_until(
        base_url,
        model,
        system,
        messages,
        state,
        turn_tx,
        std::time::Instant::now() + TURN_DEADLINE,
    );
}

/// [`stream_turn`] with an explicit wall-clock deadline, so tests can prove
/// the ceiling interrupts a trickling stream without waiting ten minutes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_turn_until(
    base_url: &str,
    model: &str,
    system: &str,
    messages: &mut Vec<Value>,
    state: &Mutex<PairState>,
    turn_tx: &Sender<TurnEnd>,
    deadline: std::time::Instant,
) {
    let req = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": system,
        "messages": messages,
    });
    // ureq 3 here, not the tree-wide ureq 2: version 3's phased timeouts
    // check the remaining budget before EVERY socket operation, so the
    // per-request global timeout below enforces the turn deadline at the
    // socket layer. A read can neither outlive the deadline nor keep the
    // socket alive after it: the cut drops the response, which closes the
    // connection. Version 2's only knob was a fixed per-read timeout, which
    // parked a blocked read (plus a helper thread and the socket) for up to
    // that timeout past the deadline on a peer that just went silent.
    let agent = ureq3::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        // Never follow redirects: the x-api-key header would be replayed to
        // the redirect target, so a loopback endpoint answering 302 could
        // bounce the credential to an arbitrary host (and a 301 would
        // downgrade the POST to GET anyway). With 0 the 3xx comes back as a
        // plain response and is refused below.
        .max_redirects(0)
        // Non-2xx arrives as a response, not an error: the error BODY
        // carries the fix ("model 'x' not found, try pulling it first").
        .http_status_as_error(false)
        .build()
        .new_agent();
    let token = std::env::var("ANTHROPIC_AUTH_TOKEN").ok();
    let (api_key, bearer) = auth_for(base_url, token.as_deref());
    let mut post = agent
        .post(format!("{base_url}/v1/messages"))
        .config()
        // The whole exchange (headers, cold-model wait, every body read)
        // shares the turn's remaining wall-clock budget.
        .timeout_global(Some(
            deadline.saturating_duration_since(std::time::Instant::now()),
        ))
        .build()
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", &api_key);
    if let Some(bearer) = &bearer {
        post = post.header("authorization", bearer);
    }
    let resp = match post.send(req.to_string()) {
        Ok(r) => r,
        Err(e) => {
            // The endpoint never answered the turn: pop the user message so
            // the conversation stays balanced. Leaving it made the NEXT turn
            // send [user, user], a hard 400 on strict gateways for the rest
            // of the seat's life, or a silent re-submit of the unanswered
            // ask on permissive local servers.
            messages.pop();
            end_turn(
                state,
                turn_tx,
                true,
                format!("local endpoint {base_url}: {}", error_detail(&e)),
            );
            return;
        }
    };

    let status = resp.status().as_u16();
    // With redirects disabled a 3xx arrives as a plain response; name it
    // instead of reading an empty stream (following it would have replayed
    // the credential header to wherever Location pointed).
    if (300..400).contains(&status) {
        messages.pop();
        end_turn(
            state,
            turn_tx,
            true,
            format!("local endpoint {base_url}: redirect refused (status {status})"),
        );
        return;
    }
    if status >= 400 {
        let body = resp.into_body().read_to_string().unwrap_or_default();
        messages.pop();
        end_turn(
            state,
            turn_tx,
            true,
            format!(
                "local endpoint {base_url}: {}",
                status_detail(status, &body)
            ),
        );
        return;
    }

    let mut fence = FenceMachine::new();
    let mut assistant = String::new();
    // Outcome honesty: the turn only succeeded if the stream ENDED cleanly
    // (message_stop / [DONE]) with untruncated text. A mid-stream drop, the
    // read timeout, an SSE error frame, or a token-limit cut all used to be
    // reported as a finished turn while the partial edit had been reverted.
    let mut clean_end = false;
    let mut truncated = false;
    let mut stream_error: Option<String> = None;
    // Total wall-clock ceiling: the transport's global timeout above bounds
    // every socket read by the remaining budget, so a silent or trickling
    // peer is cut mid-read at the deadline (and dropping the reader then
    // closes the socket). The deadline is enforced per CHUNK, not per line;
    // `lines()` blocks until a newline arrives, so a stream of non-newline
    // bytes would have dodged a per-line check indefinitely.
    let mut reader = resp.into_body().into_reader();
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    'stream: loop {
        // Checked even while data flows: a peer flooding bytes faster than
        // they drain must not starve the deadline.
        if std::time::Instant::now() > deadline {
            stream_error = Some(String::from("turn exceeded the 10 minute ceiling"));
            break;
        }
        // A single SSE line has no business being megabytes long; a cap
        // keeps a hostile or broken endpoint from ballooning memory.
        if pending.len() > 1 << 20 {
            stream_error = Some(String::from("endpoint sent an over-long SSE line"));
            break;
        }
        let n = match std::io::Read::read(&mut reader, &mut chunk) {
            Ok(0) => break, // hung up; finish() reverts open fences
            Ok(n) => n,
            Err(e) if is_timeout(&e) => {
                stream_error = Some(String::from("turn exceeded the 10 minute ceiling"));
                break;
            }
            Err(_) => break, // hung up; finish() reverts open fences
        };
        pending.extend_from_slice(&chunk[..n]);
        while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = pending.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw[..raw.len() - 1]);
            let line = line.trim_end_matches('\r');
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            if data.trim() == "[DONE]" {
                clean_end = true;
                break 'stream;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                // Skipping silently would drop a chunk of the reply and
                // still report success; the endpoint is broken, say so.
                stream_error = Some(String::from("endpoint sent malformed SSE data"));
                break 'stream;
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
                    break 'stream;
                }
                "error" => {
                    let msg = v["error"]["message"].as_str().unwrap_or("stream error");
                    stream_error = Some(msg.to_string());
                    break 'stream;
                }
                _ => {}
            }
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

/// The failure text for a transport error. HTTP statuses never land here
/// (`http_status_as_error(false)` returns them as responses, handled by
/// [`status_detail`]); a deadline hit before the headers arrived reads
/// better as the ceiling than as ureq's "timeout: global".
fn error_detail(e: &ureq3::Error) -> String {
    match e {
        ureq3::Error::Timeout(_) => String::from("turn exceeded the 10 minute ceiling"),
        other => other.to_string(),
    }
}

/// The failure text for an HTTP error status, with the error body included:
/// the body carries the fix ("model 'x' not found, try pulling it first").
fn status_detail(status: u16, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        format!("status {status}")
    } else {
        let short: String = body.chars().take(200).collect();
        format!("status {status}: {short}")
    }
}

/// Whether a body-read error is the transport enforcing the turn deadline
/// (the global timeout travels inside `io::Error::other`).
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) || e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ureq3::Error>())
        .is_some_and(|u| matches!(u, ureq3::Error::Timeout(_)))
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
