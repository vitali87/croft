//! `croft pair --repl`: the debug REPL driver, joining the relay and running until stdin
//! closes.

use super::*;

/// `croft pair --repl`: join the workspace's collab relay as the pilot seat
/// on the configured provider and run the REPL until stdin closes.
pub fn run(cfg: PairConfig) -> Result<()> {
    let pilot = match &cfg.provider {
        Provider::Claude => seat_pilot(&cfg.socket, &cfg.name, claude_command(&cfg)?, None)?,
        Provider::Local { base_url } => {
            let model = cfg
                .model
                .as_deref()
                .context("a local provider needs --model (there is no CLI default)")?;
            seat_local(&cfg.socket, &cfg.name, base_url, model, None)?
        }
    };
    let stdin = std::io::stdin();
    run_pilot(pilot, &cfg.name, cfg.task, &mut stdin.lock())
}

/// The debug REPL driver (hidden `--repl`), transport-agnostic so the e2e
/// tests can drive it with a scripted fake. Blocks until `input` (the
/// pilot's own terminal) hits EOF or the model conversation hangs up
/// mid-turn.
pub(super) fn run_pilot(
    pilot: Pilot,
    name: &str,
    task: Option<String>,
    input: &mut dyn BufRead,
) -> Result<()> {
    // REPL: the initial --task then the pilot's stdin, one turn per line.
    println!("croft pair: '{name}' seated; type a task ('@<file> <task>' to focus a buffer)");
    let mut pending = task;
    let mut hung_up = false;
    loop {
        let line = match pending.take() {
            Some(t) => t,
            None => {
                let mut line = String::new();
                match input.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => line.trim().to_string(),
                }
            }
        };
        if line.is_empty() {
            continue;
        }
        send_turn(&pilot.state, &pilot.sink, &line)?;
        match pilot.turn_rx.recv() {
            Ok(end) if end.cancelled => println!("\n[turn cancelled]"),
            Ok(end) if end.is_error => println!("\n[turn failed: {}]", end.text),
            Ok(_) => println!("\n[turn done]"),
            Err(_) => {
                // Reader gone: claude hung up mid-turn.
                hung_up = true;
                break;
            }
        }
    }
    pilot.shutdown();
    anyhow::ensure!(!hung_up, "claude exited mid-conversation");
    Ok(())
}
