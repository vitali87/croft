//! The claude CLI: the tools and settings it may use, finding it on a stripped GUI PATH,
//! the command that launches it, reading its stream-json events, and the scripted fake
//! claude the tests drive.

use super::*;

/// The tools the pilot's claude may call without a prompt (approved product
/// decision: read-only plus the stream; the ONLY write path is the fence).
/// The `croft-collab` MCP server here is a second, read-only seat riding
/// along for live buffer queries; the pilot itself owns the writing seat.
/// `Read`, `Grep` and `Glob` are deliberately not listed: a bare rule
/// matches every path (`~/.ssh`, `~/.aws`), while with no rule `dontAsk`
/// runs them inside the workspace and denies them outside it (#1926).
pub(super) const ALLOWED_TOOLS: &str = "mcp__croft-collab__collab_open,mcp__croft-collab__collab_read,\
     mcp__croft-collab__collab_status";

/// The built-in tools the pilot's claude has at all: no settings file can
/// allow a tool outside this set, so there is no shell and no write tool
/// whatever the user's or the workspace's settings say (#1926).
pub(super) const BUILTIN_TOOLS: &str = "Read,Grep,Glob";

/// The settings the pilot's claude loads: the user's own (sign-in, model,
/// their own hooks). The workspace's `.claude/settings.json` belongs to
/// whoever committed it, and its hooks would run the moment the navigator
/// starts, before anyone asked it anything (#1926).
pub(super) const SETTING_SOURCES: &str = "user";

/// The claude CLI invocation for a pair session: a persistent stream-json
/// conversation over stdio, sandboxed to the read-only toolbox, with the
/// Install dirs to probe for the claude CLI when it is not on PATH (npm
/// global bin, the native installer's `~/.claude/local`, homebrew).
pub(super) fn claude_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".claude").join("local"));
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".npm-global").join("bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// Resolve the claude CLI to a spawnable program string. A terminal launch
/// has claude on PATH and spawns it by bare name; a macOS GUI launch
/// (Croft.app) inherits the stripped launchd PATH, so `Command::new("claude")`
/// fails with ENOENT and the navigator can never seat. When claude is absent
/// from PATH we probe the usual install dirs and pin the absolute path,
/// mirroring the LSP path-only resolution. `None` means fall back to the bare
/// name so the spawn error still surfaces to the user.
pub(super) fn resolve_claude_in(on_path: bool, dirs: &[PathBuf]) -> Option<String> {
    if on_path {
        return Some(String::from("claude"));
    }
    crate::proc::find::first_in("claude", dirs).map(|p| p.to_string_lossy().into_owned())
}

/// The claude program to spawn, resolved to an absolute path on a stripped
/// GUI PATH (see [`resolve_claude_in`]).
pub(super) fn claude_program() -> String {
    resolve_claude_in(
        crate::lsp::manager::is_on_path("claude"),
        &claude_search_dirs(),
    )
    .unwrap_or_else(|| String::from("claude"))
}

/// fence protocol appended to its system prompt and a read-only collab-agent
/// seat as its MCP server.
pub(crate) fn claude_command(cfg: &PairConfig) -> Result<Command> {
    let exe = std::env::current_exe().context("resolving croft binary path")?;
    let mcp_config = json!({
        "mcpServers": {
            "croft-collab": {
                "command": exe.to_string_lossy(),
                "args": [
                    "collab-agent",
                    "--workspace", cfg.workspace.to_string_lossy(),
                    "--name", format!("{}-reader", cfg.name),
                ],
            }
        }
    });
    let mut cmd = Command::new(claude_program());
    cmd.arg("-p")
        .args(["--input-format", "stream-json"])
        .args(["--output-format", "stream-json"])
        .arg("--verbose")
        .arg("--include-partial-messages")
        .args(["--permission-mode", "dontAsk"])
        .args(["--setting-sources", SETTING_SOURCES])
        .args(["--tools", BUILTIN_TOOLS])
        .args(["--allowedTools", ALLOWED_TOOLS])
        .arg("--strict-mcp-config")
        .args(["--mcp-config", &mcp_config.to_string()])
        .args(["--append-system-prompt", &system_prompt(&cfg.provider)])
        .current_dir(&cfg.workspace);
    if let Some(model) = &cfg.model {
        cmd.args(["--model", model]);
    }
    Ok(cmd)
}

/// One inbound claude event: init capabilities, token deltas through the
/// fence machine, and each turn's result.
pub(super) fn handle_claude_event(
    state: &Mutex<PairState>,
    fence: &mut FenceMachine,
    msg: &Value,
    turn_tx: &Sender<TurnEnd>,
) {
    match msg.get("type").and_then(Value::as_str) {
        Some("system") if msg.get("subtype").and_then(Value::as_str) == Some("init") => {
            let can = msg
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|caps| {
                    caps.iter()
                        .any(|c| c.as_str() == Some("interrupt_receipt_v1"))
                });
            state.lock().unwrap().can_interrupt = can;
        }
        Some("stream_event") => {
            let delta = msg.pointer("/event/delta");
            if delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta")
                && let Some(text) = delta.and_then(|d| d.get("text")).and_then(Value::as_str)
            {
                for event in fence.push(text) {
                    apply_fence_event(state, event);
                }
            }
        }
        Some("result") => {
            // claude's stdout is an external input: a result with no
            // dispatched turn behind it (every dispatch sets turn_active,
            // and the arm below clears it exactly once) is a duplicate or
            // spontaneous emission. Ending a turn it does not own would
            // queue a phantom TurnEnd — releasing the host's busy gate
            // under a newer turn — and the finish()/flag resets would
            // revert that turn's open fence and staging.
            if !state.lock().unwrap().turn_active {
                return;
            }
            for event in fence.finish() {
                apply_fence_event(state, event);
            }
            let mut st = state.lock().unwrap();
            let cancelled = st.cancelled;
            st.turn_active = false;
            st.cancelled = false;
            st.discarding = false;
            st.comment_only = false;
            drop(st);
            let _ = turn_tx.send(TurnEnd {
                is_error: msg
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                cancelled,
                text: msg
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        _ => {} // hook noise, control_response acks, message_start/stop, ...
    }
}

/// A scripted claude: speaks just enough stream-json for the pilot.
/// argv: <log file> <mode>. Logs every stdin line (the e2es assert the
/// interrupt landed there). "stream" mode streams one fenced edit split
/// across deltas (header split mid-marker on purpose); "cancel" mode stops
/// mid-body and BLOCKS on stdin — only the pilot's interrupt line unblocks
/// it — then streams more body the pilot must drop; "notes" pins one NOTE
/// fence; "repeat" stays seated and inserts the turn's number (1, 2, ...) at
/// 0:0 on EVERY turn;
/// "linger" refuses to exit on stdin EOF. Crate-visible so the App's
/// navigator tests drive the same fake.
#[cfg(test)]
pub(crate) const FAKE_CLAUDE: &str = r#"
import json, sys

log = open(sys.argv[1], "a")
mode = sys.argv[2]

# The real claude CLI is chatty on stderr while its MCP servers start; the
# tee must route this to OUTPUT, never into the model's comment boxes.
print("mcp server starting", file=sys.stderr)
sys.stderr.flush()

def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def delta(text):
    emit({"type": "stream_event", "event": {
        "type": "content_block_delta",
        "delta": {"type": "text_delta", "text": text}}})

emit({"type": "system", "subtype": "init",
      "capabilities": ["interrupt_receipt_v1"]})

line = sys.stdin.readline()
if not line:
    sys.exit(0)
log.write(line)
log.flush()

if mode == "hang":
    # Read one turn, then never respond: turn_active stays true so the host
    # reports busy and rejects a second turn.
    import time
    time.sleep(60)
    sys.exit(0)

if mode == "notes":
    delta("Reviewing.\n")
    delta("<<<NOTE demo.txt:1>>>\n")
    delta("second line could be tighter\n")
    delta("<<<END>>>\n")
    emit({"type": "result", "subtype": "success",
          "is_error": False, "result": "ok"})
    # Stay seated like the real CLI: wait for the next turn (or EOF).
    sys.stdin.readline()
    sys.exit(0)

if mode == "repeat":
    # An edit on every turn, numbered so a test can tell which turn's edit
    # reached the owner and watch the host gate decide turn by turn.
    turn = 0
    while line:
        turn += 1
        delta("<<<EDIT demo.txt:0:0-0:0>>>\n%d\n<<<END>>>\n" % turn)
        emit({"type": "result", "subtype": "success",
              "is_error": False, "result": "ok"})
        line = sys.stdin.readline()
        log.write(line)
        log.flush()
    sys.exit(0)

delta("Let me fix that.\n")
delta("<<<EDIT demo.txt:0:6-0:11>")
delta(">>\n")
delta("streamed")
if mode == "cancel":
    line2 = sys.stdin.readline()
    log.write(line2)
    log.flush()
    delta(" MORE-AFTER-CANCEL")
    delta("\n<<<END>>>\n")
    emit({"type": "result", "subtype": "success",
          "is_error": False, "result": "cancelled turn"})
else:
    delta(" edit")
    delta("\n<<<END>>>\nDone.\n")
    emit({"type": "result", "subtype": "success",
          "is_error": False, "result": "ok"})
if mode == "linger":
    # The real claude CLI does NOT reliably exit on stdin EOF (MCP
    # teardown lingers); the pilot must not gamble its own exit on it.
    import time
    time.sleep(60)
"#;
