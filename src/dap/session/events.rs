//! Reading what an adapter sends: a raw DAP message classified into the [`DapEvent`]
//! the app acts on, which program output is worth showing, and why an adapter refused
//! to start.

use super::*;

/// Why an adapter refused `launch`/`attach`: DAP puts the full text in
/// `body.error.format` (delve's "Failed to launch …: Go version … is too old
/// for this version of Delve"), the short form in `message`.
pub(super) fn launch_failure_reason(msg: &Value) -> String {
    msg.pointer("/body/error/format")
        .and_then(Value::as_str)
        .or_else(|| msg.get("message").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .unwrap_or("the debug adapter refused to start the program")
        .to_string()
}

/// Classify a raw incoming DAP message into a [`DapEvent`], or `None` for
/// messages the app doesn't surface (responses, bookkeeping events). Pure.
pub fn classify_event(msg: &Value) -> Option<DapEvent> {
    if msg.get("type")?.as_str()? != "event" {
        return None;
    }
    let body = msg.get("body");
    match msg.get("event")?.as_str()? {
        "initialized" => Some(DapEvent::Initialized),
        "stopped" => Some(DapEvent::Stopped {
            thread_id: body
                .and_then(|b| b.get("threadId"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
            reason: body
                .and_then(|b| b.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        "continued" => Some(DapEvent::Continued),
        "output" => Some(DapEvent::Output {
            category: body
                .and_then(|b| b.get("category"))
                .and_then(Value::as_str)
                .unwrap_or("console")
                .to_string(),
            text: body
                .and_then(|b| b.get("output"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        "terminated" | "exited" => Some(DapEvent::Terminated),
        _ => None,
    }
}

/// Whether a DAP `output` event of this `category` is user-facing program
/// output worth showing in the debug console. `telemetry` (debugpy's own
/// `ptvsd` / `debugpy` banner lines) is adapter noise and is filtered out;
/// `stdout` / `stderr` / `console` / `important` are kept.
pub fn output_is_user_visible(category: &str) -> bool {
    category != "telemetry"
}
