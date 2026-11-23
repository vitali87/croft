//! Running and stepping: pause, continue, step over, into and out, and Step Into
//! Target's choice among the calls on the line.

use super::*;

/// What [`DapSession::request_step_in_targets`] did (#1543).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepInTargetsAsk {
    /// The request is out; the answer arrives as `StepInTargets`.
    Sent,
    /// Not paused, or the adapter cannot list step-in targets.
    Unsupported,
    /// The adapter could answer, but the paused frame is not known yet.
    NoFrame,
}

/// Build a thread-scoped request (`continue` / `next` / `stepIn` /
/// `stepOut`, and `exceptionInfo`, which takes the same `threadId`).
pub fn thread_request(command: &str, thread_id: i64) -> Value {
    json!({
        "type": "request",
        "command": command,
        "arguments": { "threadId": thread_id }
    })
}

/// Build a `stepInTargets` request (#1543): the calls on `frame_id`'s line.
pub fn step_in_targets_request(frame_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "stepInTargets",
        "arguments": { "frameId": frame_id }
    })
}

/// Build a `stepIn` into one of the line's calls (`targetId` from a
/// `stepInTargets` response) rather than the first call to run.
pub fn step_in_target_request(thread_id: i64, target_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "stepIn",
        "arguments": { "threadId": thread_id, "targetId": target_id }
    })
}

/// The `(id, label)` pairs of a `stepInTargets` response.
pub fn parse_step_in_targets(msg: &Value) -> Vec<(i64, String)> {
    msg.pointer("/body/targets")
        .and_then(Value::as_array)
        .map(|ts| {
            ts.iter()
                .filter_map(|t| {
                    Some((
                        t.get("id")?.as_i64()?,
                        t.get("label")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

impl DapSession {
    /// Interrupt a running program (`pause`), targeting the most recently known
    /// thread. No-op if no thread is known yet.
    pub fn pause(&mut self) {
        if let Some(tid) = self.stopped_thread.or(self.known_thread) {
            let _ = self.active().send(thread_request("pause", tid));
        }
    }

    /// Resume execution of the stopped thread.
    pub fn continue_execution(&mut self) {
        if let Some(tid) = self.stopped_thread {
            let _ = self.active().send(thread_request("continue", tid));
            self.phase = SessionPhase::Running;
            self.exception_info_seq = None;
            self.step_in_targets_seq = None;
        }
    }

    /// Step over (`next`), into (`stepIn`), or out (`stepOut`).
    pub fn step(&mut self, command: &str) {
        if let Some(tid) = self.stopped_thread {
            let _ = self.active().send(thread_request(command, tid));
            // The stop is over; its exception reply no longer applies (#864).
            self.exception_info_seq = None;
            self.step_in_targets_seq = None;
        }
    }

    /// Ask for the calls on the selected frame's line (#1543), answered as
    /// [`DapEvent::StepInTargets`]. [`StepInTargetsAsk::Unsupported`] when
    /// the program is not paused or the adapter lacks
    /// `supportsStepInTargetsRequest`; [`StepInTargetsAsk::NoFrame`] when it
    /// could answer but the paused frame has not arrived yet (or the send
    /// failed), so the caller must not fall back to a plain step in.
    pub fn request_step_in_targets(&mut self) -> StepInTargetsAsk {
        if self.phase != SessionPhase::Stopped || !self.supports("supportsStepInTargetsRequest") {
            return StepInTargetsAsk::Unsupported;
        }
        let Some(frame) = self
            .selected_frame
            .or_else(|| self.stack_frames.first().map(|f| f.id))
        else {
            return StepInTargetsAsk::NoFrame;
        };
        self.step_in_targets_seq = self.active().send(step_in_targets_request(frame)).ok();
        if self.step_in_targets_seq.is_some() {
            StepInTargetsAsk::Sent
        } else {
            StepInTargetsAsk::NoFrame
        }
    }

    /// Step into the call `target_id` on the paused line (#1543).
    pub fn step_in_target(&mut self, target_id: i64) {
        if let Some(tid) = self.stopped_thread {
            let _ = self.active().send(step_in_target_request(tid, target_id));
            self.exception_info_seq = None;
            self.step_in_targets_seq = None;
        }
    }
}
