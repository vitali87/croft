//! Auto-detected project tasks.
//!
//! Croft deliberately has no tasks.json requirement of its own: the repo
//! already declares its rituals in the manifests it ships anyway. This
//! module reads them and turns each into a runnable shell command for the
//! "Tasks: Run Task" picker and the `Cmd+Shift+B` default build:
//!
//! - `.vscode/tasks.json` (JSONC tolerated) — honoured when present so
//!   VS Code repos work unchanged, but never required
//! - `Makefile` / `makefile` / `GNUmakefile` targets → `make <target>`
//! - `justfile` / `Justfile` recipes → `just <recipe>`
//! - `package.json` scripts → `npm|pnpm|yarn|bun run <script>` (runner
//!   picked from the lockfile present)
//! - `Cargo.toml` → the standard cargo verbs
//! - `pyproject.toml` `[project.scripts]` → `uv run <name>` (plus
//!   `uv run pytest` when the file mentions pytest)
//!
//! The command line is written to a shell pane's PTY, so PATH, aliases,
//! and env resolve exactly as if the user typed it.

use std::collections::BTreeSet;
use std::path::Path;

/// One runnable task. `label` names it in the picker, `command` is the
/// line written to the pane, `source` says which manifest declared it,
/// and `is_build` marks candidates for the `Cmd+Shift+B` default build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub label: String,
    pub command: String,
    pub source: String,
    pub is_build: bool,
}

/// Every task the workspace's manifests declare, in source priority
/// order (tasks.json first so an explicit VS Code default-build wins).
/// Duplicate command lines are dropped, first source wins.
pub fn discover_tasks(root: &Path) -> Vec<Task> {
    let mut out = Vec::new();
    out.extend(vscode_tasks(root));
    out.extend(makefile_tasks(root));
    out.extend(justfile_tasks(root));
    out.extend(package_json_tasks(root));
    out.extend(cargo_tasks(root));
    out.extend(pyproject_tasks(root));
    dedup(out)
}

/// The task `Cmd+Shift+B` runs: the first build-flagged task in source
/// priority order.
pub fn default_build_task(tasks: &[Task]) -> Option<&Task> {
    tasks.iter().find(|t| t.is_build)
}

fn read_first(root: &Path, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|n| std::fs::read_to_string(root.join(n)).ok())
}

fn vscode_tasks(root: &Path) -> Vec<Task> {
    let Some(text) = read_first(root, &[".vscode/tasks.json"]) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)) else {
        return Vec::new();
    };
    let Some(tasks) = v.get("tasks").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    tasks
        .iter()
        .filter_map(|t| {
            let command = t.get("command")?.as_str()?;
            let mut line = command.to_string();
            if let Some(args) = t.get("args").and_then(|a| a.as_array()) {
                for a in args.iter().filter_map(|a| a.as_str()) {
                    line.push(' ');
                    line.push_str(a);
                }
            }
            let label = t
                .get("label")
                .and_then(|l| l.as_str())
                .unwrap_or(command)
                .to_string();
            let is_build = match t.get("group") {
                Some(serde_json::Value::String(s)) => s == "build",
                Some(g) => g.get("kind").and_then(|k| k.as_str()) == Some("build"),
                None => false,
            };
            Some(Task {
                label,
                command: line,
                source: "tasks.json".to_string(),
                is_build,
            })
        })
        .collect()
}

fn makefile_tasks(root: &Path) -> Vec<Task> {
    let Some(text) = read_first(root, &["Makefile", "makefile", "GNUmakefile"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        // Recipe lines are tab-indented; rules start at column zero.
        if line.starts_with(['\t', ' ', '#']) {
            continue;
        }
        // `target: deps` — but not `var := value` / `var = value`, not
        // pattern rules (`%.o:`), not special targets (`.PHONY:`).
        let Some((head, rest)) = line.split_once(':') else {
            continue;
        };
        if rest.starts_with('=') {
            continue;
        }
        let name = head.trim();
        if name.is_empty()
            || name.starts_with('.')
            || name.contains(['%', '$', '=', ' ', '\t'])
            || !seen.insert(name.to_string())
        {
            continue;
        }
        out.push(Task {
            label: format!("make {name}"),
            command: format!("make {name}"),
            source: "Makefile".to_string(),
            is_build: name == "build" || name == "all",
        });
    }
    out
}

fn justfile_tasks(root: &Path) -> Vec<Task> {
    let Some(text) = read_first(root, &["justfile", "Justfile", ".justfile"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '#', '@']) || line.starts_with("set ") {
            continue;
        }
        // `recipe arg="x":` — the name is the first word; assignments
        // (`name := value`) carry `:=` and are skipped.
        let Some((head, rest)) = line.split_once(':') else {
            continue;
        };
        if rest.starts_with('=') {
            continue;
        }
        let name = head.split_whitespace().next().unwrap_or("");
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || "_-".contains(c))
        {
            continue;
        }
        out.push(Task {
            label: format!("just {name}"),
            command: format!("just {name}"),
            source: "justfile".to_string(),
            is_build: name == "build",
        });
    }
    out
}

fn package_json_tasks(root: &Path) -> Vec<Task> {
    let Some(text) = read_first(root, &["package.json"]) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) else {
        return Vec::new();
    };
    // Pick the runner the repo actually uses, by its lockfile.
    let runner = if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun run"
    } else if root.join("pnpm-lock.yaml").exists() {
        "pnpm run"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else {
        "npm run"
    };
    scripts
        .keys()
        .map(|name| Task {
            label: format!("{runner} {name}"),
            command: format!("{runner} {name}"),
            source: "package.json".to_string(),
            is_build: name == "build",
        })
        .collect()
}

fn cargo_tasks(root: &Path) -> Vec<Task> {
    if !root.join("Cargo.toml").is_file() {
        return Vec::new();
    }
    [
        ("cargo build", true),
        ("cargo test", false),
        ("cargo clippy", false),
        ("cargo run", false),
    ]
    .into_iter()
    .map(|(cmd, is_build)| Task {
        label: cmd.to_string(),
        command: cmd.to_string(),
        source: "Cargo.toml".to_string(),
        is_build,
    })
    .collect()
}

fn pyproject_tasks(root: &Path) -> Vec<Task> {
    let Some(text) = read_first(root, &["pyproject.toml"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(v) = text.parse::<toml::Table>()
        && let Some(scripts) = v
            .get("project")
            .and_then(|p| p.get("scripts"))
            .and_then(|s| s.as_table())
    {
        for name in scripts.keys() {
            out.push(Task {
                label: format!("uv run {name}"),
                command: format!("uv run {name}"),
                source: "pyproject.toml".to_string(),
                is_build: false,
            });
        }
    }
    if text.contains("pytest") {
        out.push(Task {
            label: "uv run pytest".to_string(),
            command: "uv run pytest".to_string(),
            source: "pyproject.toml".to_string(),
            is_build: false,
        });
    }
    out
}

/// Strip `//` and `/* */` comments (outside strings) plus trailing commas
/// so VS Code's JSONC tasks.json parses with strict serde_json. Two
/// passes: comments first, then trailing commas, so a comma followed by
/// a comment followed by `}` still counts as trailing.
fn strip_jsonc(text: &str) -> String {
    let mut no_comments = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            no_comments.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        no_comments.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                no_comments.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        no_comments.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => no_comments.push(c),
        }
    }
    let mut out = String::with_capacity(no_comments.len());
    let mut chars = no_comments.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' => {
                let closes = chars
                    .clone()
                    .find(|n| !n.is_whitespace())
                    .is_some_and(|n| matches!(n, '}' | ']'));
                if !closes {
                    out.push(',');
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn dedup(tasks: Vec<Task>) -> Vec<Task> {
    let mut seen = BTreeSet::new();
    tasks
        .into_iter()
        .filter(|t| seen.insert(t.command.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(root: &Path) -> Vec<String> {
        discover_tasks(root)
            .into_iter()
            .map(|t| t.command)
            .collect()
    }

    #[test]
    fn discovers_makefile_targets_and_skips_machinery() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Makefile"),
            "CC := gcc\n\
             .PHONY: build test\n\
             build: src/main.c\n\
             \tgcc -o app src/main.c\n\
             test:\n\
             \t./run-tests\n\
             %.o: %.c\n\
             \t$(CC) -c $<\n",
        )
        .unwrap();
        let cmds = commands(tmp.path());
        assert!(cmds.contains(&"make build".to_string()), "{cmds:?}");
        assert!(cmds.contains(&"make test".to_string()), "{cmds:?}");
        assert!(
            !cmds
                .iter()
                .any(|c| c.contains('%') || c.contains(".PHONY") || c.contains("CC")),
            "pattern rules, special targets, and variables are not tasks: {cmds:?}"
        );
        let tasks = discover_tasks(tmp.path());
        let build = tasks.iter().find(|t| t.command == "make build").unwrap();
        assert!(build.is_build, "a target named build is the build task");
    }

    #[test]
    fn discovers_justfile_recipes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("justfile"),
            "set shell := [\"bash\", \"-c\"]\n\
             version := \"1.0\"\n\
             # deploy everything\n\
             deploy target=\"prod\":\n\
             \techo deploying\n\
             lint:\n\
             \tcargo clippy\n",
        )
        .unwrap();
        let cmds = commands(tmp.path());
        assert!(cmds.contains(&"just deploy".to_string()), "{cmds:?}");
        assert!(cmds.contains(&"just lint".to_string()), "{cmds:?}");
        assert!(
            !cmds
                .iter()
                .any(|c| c.contains("version") || c.contains("set ")),
            "assignments and settings are not recipes: {cmds:?}"
        );
    }

    #[test]
    fn package_json_scripts_use_the_lockfiles_runner() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"x","scripts":{"build":"tsc","dev":"vite"}}"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        let cmds = commands(tmp.path());
        assert!(cmds.contains(&"pnpm run build".to_string()), "{cmds:?}");
        assert!(cmds.contains(&"pnpm run dev".to_string()), "{cmds:?}");
        let tasks = discover_tasks(tmp.path());
        assert!(
            tasks
                .iter()
                .any(|t| t.command == "pnpm run build" && t.is_build),
            "the build script is the build task"
        );
    }

    #[test]
    fn cargo_projects_get_the_standard_verbs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let tasks = discover_tasks(tmp.path());
        let cmds: Vec<&str> = tasks.iter().map(|t| t.command.as_str()).collect();
        assert!(cmds.contains(&"cargo build"), "{cmds:?}");
        assert!(cmds.contains(&"cargo test"), "{cmds:?}");
        assert!(
            tasks
                .iter()
                .any(|t| t.command == "cargo build" && t.is_build),
            "cargo build is the build task"
        );
    }

    #[test]
    fn pyproject_scripts_run_through_uv() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\nname = \"x\"\nversion = \"0\"\ndependencies = [\"pytest\"]\n\n[project.scripts]\nserve = \"x.main:run\"\n",
        )
        .unwrap();
        let cmds = commands(tmp.path());
        assert!(cmds.contains(&"uv run serve".to_string()), "{cmds:?}");
        assert!(cmds.contains(&"uv run pytest".to_string()), "{cmds:?}");
    }

    #[test]
    fn vscode_tasks_json_with_comments_is_honoured_and_wins_the_build_slot() {
        let tmp = tempfile::tempdir().unwrap();
        // A Makefile build target would normally claim the build slot…
        std::fs::write(tmp.path().join("Makefile"), "build:\n\ttrue\n").unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".vscode/tasks.json"),
            r#"{
  // VS Code allows comments here
  "version": "2.0.0",
  "tasks": [
    {
      "label": "full build",
      "type": "shell",
      "command": "nix",
      "args": ["build"],
      "group": { "kind": "build", "isDefault": true }, /* default */
    },
  ],
}"#,
        )
        .unwrap();
        let tasks = discover_tasks(tmp.path());
        let t = tasks.iter().find(|t| t.label == "full build").unwrap();
        assert_eq!(t.command, "nix build", "command joins args");
        assert!(t.is_build);
        // …but tasks.json comes first, so its default-build wins.
        assert_eq!(
            default_build_task(&tasks).unwrap().label,
            "full build",
            "the explicit VS Code default build outranks the Makefile target"
        );
    }

    #[test]
    fn strip_jsonc_leaves_strings_intact() {
        let cleaned = strip_jsonc(
            r#"{"a": "http://x // not a comment", // real
"b": 1, /* gone */ }"#,
        );
        let v: serde_json::Value = serde_json::from_str(&cleaned).expect("valid strict JSON");
        assert_eq!(v["a"], "http://x // not a comment");
        assert_eq!(v["b"], 1);
    }

    #[test]
    fn empty_workspace_has_no_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover_tasks(tmp.path()).is_empty());
    }
}
