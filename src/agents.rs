//! Agent lanes (#344): which terminal panes are running a coding agent,
//! which one, and whether it is working, waiting for the user, or idle.
//!
//! Claude Code, Codex CLI, aider and gemini-cli all run in a pane, so croft
//! is already where the agents live; everything in the agent-cockpit series
//! starts from knowing that. The foreground process of every pane is
//! already sampled for the name pill, so an agent is recognised from that
//! name against a small table — built in, extended by `agents.json` in the
//! config directory without a rebuild — and its status is judged from two
//! things the pane already has: how long since it last produced output, and
//! what its last screen rows say. An agent that stays in the foreground
//! never emits OSC 133 marks, which is why "waiting" cannot come from the
//! shell integration and has to be read off the screen.

use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::Regex;

/// One recognised agent: the badge it wears, the process names that mean
/// it, and the prompt shapes that mean it is waiting on the user.
#[derive(Debug, Clone)]
pub struct AgentKind {
    /// The badge text (`claude`).
    pub name: String,
    /// Foreground process names, matched case-insensitively against the
    /// sampled name's basename.
    pub process: Vec<String>,
    /// Regexes a recent screen row matches when the agent is waiting for
    /// input: a permission question, a `>` prompt, a Y/n.
    pub prompt: Vec<Regex>,
}

/// What an agent in a pane is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Output is still flowing.
    Working,
    /// Quiet, and the last rows look like a prompt for the user.
    Waiting,
    /// Quiet, and nothing on screen asks for anything.
    Idle,
}

impl AgentStatus {
    /// The glyph the pane pill and the status chip show beside the name.
    pub fn glyph(self) -> char {
        match self {
            AgentStatus::Working => '\u{25cf}', // ● output flowing
            AgentStatus::Waiting => '\u{25d0}', // ◐ waiting on you
            AgentStatus::Idle => '\u{25cb}',    // ○ quiet
        }
    }
}

/// An agent seated in a pane, as the pane carries it between samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLane {
    pub name: String,
    pub status: AgentStatus,
}

/// The transitions the rest of the cockpit consumes (#345, #358, #347).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// An agent appeared in the pane.
    Seated { pane: String, agent: String },
    /// It went from quiet to producing output.
    Working { pane: String, agent: String },
    /// It stopped and its screen asks for the user: once per prompt.
    Waiting { pane: String, agent: String },
    /// It is no longer the pane's foreground process.
    Gone { pane: String, agent: String },
}

/// No output for this long is "quiet"; a sample inside it is Working.
pub const QUIET_AFTER: Duration = Duration::from_secs(3);

/// The user-editable table beside the other config files.
pub fn agents_path() -> PathBuf {
    crate::prefs::config_dir().join("agents.json")
}

/// Starter `agents.json`, seeded on first open. Rows here EXTEND the
/// built-in table; a row whose `name` matches a built-in replaces it.
pub const TEMPLATE: &str = r##"// croft agent lanes: which foreground processes are coding agents, and what
// their screen looks like when they are waiting for you (#344).
//
// Built in: claude, codex, aider, gemini. Add a row to teach croft another
// agent without a rebuild; a row named like a built-in replaces it.
//   name:    the badge shown on the pane pill
//   process: foreground process names that mean this agent
//   prompt:  regexes a recent screen row matches when it is waiting on you
[
  // { "name": "goose", "process": ["goose"], "prompt": ["^\\s*>\\s*$", "\\(y/n\\)"] }
]
"##;

/// One raw `agents.json` row.
#[derive(serde::Deserialize)]
struct AgentRow {
    name: String,
    #[serde(default)]
    process: Vec<String>,
    #[serde(default)]
    prompt: Vec<String>,
}

/// The table of agents croft recognises.
#[derive(Debug, Clone, Default)]
pub struct AgentTable {
    kinds: Vec<AgentKind>,
}

fn kind(name: &str, process: &[&str], prompt: &[&str]) -> AgentKind {
    AgentKind {
        name: name.to_string(),
        process: process.iter().map(|p| p.to_string()).collect(),
        prompt: prompt.iter().filter_map(|p| Regex::new(p).ok()).collect(),
    }
}

impl AgentTable {
    /// The agents croft knows without any configuration.
    pub fn builtin() -> Self {
        // The prompt shapes are what each agent prints when it stops for the
        // user: a permission question, a bare prompt line, a yes/no.
        let common = [
            "\\(y/n\\)",
            "\\[Y/n\\]",
            "\\[y/N\\]",
            "\\?\\s*$",
            "^\\s*[>❯›]\\s*$",
        ];
        Self {
            kinds: vec![
                kind(
                    "claude",
                    &["claude"],
                    &[
                        "Do you want to",
                        "Allow",
                        "Esc to cancel",
                        "^\\s*[>❯]\\s*$",
                        "\\(y/n\\)",
                    ],
                ),
                kind(
                    "codex",
                    &["codex"],
                    &[
                        "Allow",
                        "approve",
                        "\\[y/N\\]",
                        "\\(y/n\\)",
                        "^\\s*[>❯›]\\s*$",
                    ],
                ),
                kind(
                    "aider",
                    &["aider"],
                    &["^\\s*[>❯]\\s*$", "\\(Y\\)es", "\\[Y/n\\]", "\\?\\s*$"],
                ),
                kind("gemini", &["gemini"], &common),
            ],
        }
    }

    /// The built-in table plus whatever `path` adds; a missing or broken file
    /// is the built-in table alone, never a failed start.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(json) => Self::from_json(&json),
            Err(_) => Self::builtin(),
        }
    }

    /// The built-in table extended by rows from `agents.json` text. A row
    /// named like a built-in replaces it; a row with no usable regex still
    /// badges the pane (it just never reads as waiting).
    pub fn from_json(json: &str) -> Self {
        let rows: Vec<AgentRow> =
            serde_json::from_str(&crate::keymap::strip_line_comments(json)).unwrap_or_default();
        let mut table = Self::builtin();
        for row in rows {
            let name = row.name.trim().to_lowercase();
            if name.is_empty() {
                continue;
            }
            let process: Vec<String> = if row.process.is_empty() {
                vec![name.clone()]
            } else {
                row.process
                    .iter()
                    .map(|p| p.trim().to_lowercase())
                    .collect()
            };
            let prompt: Vec<Regex> = row
                .prompt
                .iter()
                .filter_map(|p| Regex::new(p).ok())
                .collect();
            let k = AgentKind {
                name: name.clone(),
                process,
                prompt,
            };
            match table.kinds.iter_mut().find(|k| k.name == name) {
                Some(existing) => *existing = k,
                None => table.kinds.push(k),
            }
        }
        table
    }

    /// The agent a foreground process name means, if any. The name may be a
    /// path (`/opt/homebrew/bin/claude`) and any case.
    pub fn classify(&self, process_name: &str) -> Option<&AgentKind> {
        let base = process_name
            .rsplit('/')
            .next()
            .unwrap_or(process_name)
            .trim()
            .to_lowercase();
        if base.is_empty() {
            return None;
        }
        self.kinds
            .iter()
            .find(|k| k.process.iter().any(|p| p.eq_ignore_ascii_case(&base)))
    }

    pub fn names(&self) -> Vec<&str> {
        self.kinds.iter().map(|k| k.name.as_str()).collect()
    }
}

/// What an agent is doing, from how long its pane has been quiet and what
/// its last screen rows say. `quiet_after` is the threshold ([`QUIET_AFTER`]
/// in the app; tests pass 0 to judge the screen alone).
pub fn judge(
    kind: &AgentKind,
    quiet_for: Duration,
    last_rows: &[String],
    quiet_after: Duration,
) -> AgentStatus {
    if quiet_for < quiet_after {
        return AgentStatus::Working;
    }
    let asks = last_rows
        .iter()
        .rev()
        .filter(|r| !r.trim().is_empty())
        .take(6)
        .any(|row| kind.prompt.iter().any(|re| re.is_match(row)));
    if asks {
        AgentStatus::Waiting
    } else {
        AgentStatus::Idle
    }
}

/// The events one sample produces for a pane, given what it carried before
/// and what it carries now. `Waiting` fires only on the way INTO waiting, so
/// a prompt that sits on screen across many samples notifies once.
pub fn transition(
    pane: &str,
    prev: Option<&AgentLane>,
    next: Option<&AgentLane>,
) -> Vec<AgentEvent> {
    let pane = pane.to_string();
    match (prev, next) {
        (None, None) => Vec::new(),
        (None, Some(n)) => {
            let mut ev = vec![AgentEvent::Seated {
                pane: pane.clone(),
                agent: n.name.clone(),
            }];
            if n.status == AgentStatus::Waiting {
                ev.push(AgentEvent::Waiting {
                    pane,
                    agent: n.name.clone(),
                });
            }
            ev
        }
        (Some(p), None) => vec![AgentEvent::Gone {
            pane,
            agent: p.name.clone(),
        }],
        (Some(p), Some(n)) if p.name != n.name => {
            let mut ev = vec![
                AgentEvent::Gone {
                    pane: pane.clone(),
                    agent: p.name.clone(),
                },
                AgentEvent::Seated {
                    pane: pane.clone(),
                    agent: n.name.clone(),
                },
            ];
            if n.status == AgentStatus::Waiting {
                ev.push(AgentEvent::Waiting {
                    pane,
                    agent: n.name.clone(),
                });
            }
            ev
        }
        (Some(p), Some(n)) => match (p.status, n.status) {
            (a, b) if a == b => Vec::new(),
            (_, AgentStatus::Waiting) => vec![AgentEvent::Waiting {
                pane,
                agent: n.name.clone(),
            }],
            (_, AgentStatus::Working) => vec![AgentEvent::Working {
                pane,
                agent: n.name.clone(),
            }],
            (_, AgentStatus::Idle) => Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_builtin_table_recognises_the_four_agents_by_name_or_path() {
        let t = AgentTable::builtin();
        assert_eq!(t.names(), ["claude", "codex", "aider", "gemini"]);
        for (proc_name, want) in [
            ("claude", "claude"),
            ("/opt/homebrew/bin/claude", "claude"),
            ("Codex", "codex"),
            ("aider", "aider"),
            ("gemini", "gemini"),
        ] {
            assert_eq!(
                t.classify(proc_name).map(|k| k.name.as_str()),
                Some(want),
                "{proc_name}"
            );
        }
        for not in ["zsh", "node", "cargo", "", "claudette", "/usr/bin/vim"] {
            assert!(t.classify(not).is_none(), "{not:?} is not an agent");
        }
    }

    #[test]
    fn agents_json_extends_the_table_without_a_rebuild_and_can_replace_a_builtin() {
        let t = AgentTable::from_json(
            r#"// a comment
            [
              { "name": "goose", "process": ["goose", "goose-cli"], "prompt": ["^\\s*>\\s*$"] },
              { "name": "claude", "process": ["claude", "claude-code"], "prompt": ["Proceed\\?"] }
            ]"#,
        );
        assert_eq!(
            t.classify("goose-cli").map(|k| k.name.as_str()),
            Some("goose")
        );
        let claude = t
            .classify("claude-code")
            .expect("the replaced row matches its new process");
        assert_eq!(claude.name, "claude");
        assert_eq!(
            claude.prompt.len(),
            1,
            "the row replaced the built-in prompts"
        );
        assert_eq!(t.names().len(), 5);
        // A broken file is the built-in table, not a failed start.
        assert_eq!(AgentTable::from_json("{ not json").names().len(), 4);
        assert_eq!(
            AgentTable::load(Path::new("/definitely/missing.json"))
                .names()
                .len(),
            4
        );
        // TEMPLATE parses (comments stripped) to zero rows.
        assert_eq!(AgentTable::from_json(TEMPLATE).names().len(), 4);
    }

    #[test]
    fn status_is_working_while_output_flows_then_waiting_or_idle_by_the_screen() {
        let t = AgentTable::builtin();
        let claude = t.classify("claude").unwrap();
        let prompt = rows(&["Edited src/main.rs", "", "Do you want to proceed? (y/n)"]);
        let quiet = rows(&["Edited src/main.rs", "Done."]);
        let after = Duration::from_secs(3);
        assert_eq!(
            judge(claude, Duration::from_millis(500), &prompt, after),
            AgentStatus::Working
        );
        assert_eq!(
            judge(claude, Duration::from_secs(10), &prompt, after),
            AgentStatus::Waiting
        );
        assert_eq!(
            judge(claude, Duration::from_secs(10), &quiet, after),
            AgentStatus::Idle
        );
        // A bare prompt line counts; a question ten rows up does not.
        assert_eq!(
            judge(claude, after, &rows(&["> "]), after),
            AgentStatus::Waiting
        );
        let mut buried = vec!["Allow this? (y/n)".to_string()];
        buried.extend((0..8).map(|i| format!("log line {i}")));
        assert_eq!(judge(claude, after, &buried, after), AgentStatus::Idle);
    }

    #[test]
    fn waiting_fires_once_per_prompt_and_seated_gone_bracket_the_lane() {
        let lane = |s| AgentLane {
            name: "claude".into(),
            status: s,
        };
        let pane = "Terminal 2";
        assert_eq!(
            transition(pane, None, Some(&lane(AgentStatus::Working))),
            vec![AgentEvent::Seated {
                pane: pane.into(),
                agent: "claude".into()
            }]
        );
        let waiting = vec![AgentEvent::Waiting {
            pane: pane.into(),
            agent: "claude".into(),
        }];
        assert_eq!(
            transition(
                pane,
                Some(&lane(AgentStatus::Working)),
                Some(&lane(AgentStatus::Waiting))
            ),
            waiting
        );
        assert_eq!(
            transition(
                pane,
                Some(&lane(AgentStatus::Waiting)),
                Some(&lane(AgentStatus::Waiting))
            ),
            Vec::<AgentEvent>::new(),
            "a prompt that stays on screen fires once"
        );
        assert_eq!(
            transition(
                pane,
                Some(&lane(AgentStatus::Waiting)),
                Some(&lane(AgentStatus::Working))
            ),
            vec![AgentEvent::Working {
                pane: pane.into(),
                agent: "claude".into()
            }]
        );
        assert_eq!(
            transition(
                pane,
                Some(&lane(AgentStatus::Working)),
                Some(&lane(AgentStatus::Idle))
            ),
            Vec::<AgentEvent>::new()
        );
        assert_eq!(
            transition(pane, Some(&lane(AgentStatus::Idle)), None),
            vec![AgentEvent::Gone {
                pane: pane.into(),
                agent: "claude".into()
            }]
        );
        // Seated straight into a prompt is one Seated and one Waiting.
        assert_eq!(
            transition(pane, None, Some(&lane(AgentStatus::Waiting))).len(),
            2
        );
        // A different agent taking the pane is Gone then Seated.
        let other = AgentLane {
            name: "aider".into(),
            status: AgentStatus::Working,
        };
        let ev = transition(pane, Some(&lane(AgentStatus::Idle)), Some(&other));
        assert!(
            matches!(ev[0], AgentEvent::Gone { .. }) && matches!(ev[1], AgentEvent::Seated { .. })
        );
    }
}
