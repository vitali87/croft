//! `croft hook`: Claude Code's `PreToolUse` hook itself, and installing or removing it in
//! Claude Code's settings.

use super::*;

pub(super) fn hook_command(action: HookAction) -> std::result::Result<(), String> {
    use crate::agent_hook::{self, Outcome, Scope};
    match action {
        HookAction::ClaudeCode => agent_hook::run_claude_code(
            &mut std::io::stdin(),
            &mut std::io::stdout(),
            &mut std::io::stderr(),
            agent_hook::ANSWER_WINDOW,
        ),
        HookAction::Install {
            global, uninstall, ..
        } => {
            let scope = if global {
                Scope::Global
            } else {
                Scope::Project
            };
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or("HOME is not set")?;
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let path = agent_hook::settings_path(scope, &home, &cwd);
            let records = agent_hook::default_record_dir();
            let show = |d: &str| print!("{d}");
            let outcome = if uninstall {
                agent_hook::uninstall(&path, &records, show)?
            } else {
                agent_hook::install(&path, &records, show)?
            };
            match (outcome, uninstall) {
                (Outcome::Unchanged, false) => {
                    println!("croft's hook is already in {}", path.display())
                }
                (Outcome::Unchanged, true) => println!("no croft hook in {}", path.display()),
                (Outcome::Changed { .. }, _) => println!("wrote {}", path.display()),
            }
            Ok(())
        }
    }
}
