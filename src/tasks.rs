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
    /// tasks.json `group.isDefault` — the one task VS Code's Run Build
    /// Task executes when several carry the build group. Always false for
    /// convention-derived sources (Makefile, Cargo, …).
    pub is_default: bool,
    /// tasks.json `problemMatcher`, kept raw ("$name" string, object, or
    /// array); `problem_matchers::from_tasks_json` translates it when the
    /// task runs (#252). Always None for convention-derived sources.
    pub problem_matcher: Option<serde_json::Value>,
}

/// Every task the workspace's manifests declare, in source priority
/// order (tasks.json first so an explicit VS Code default-build wins).
/// A repeated (label, command line) pair is dropped, first source wins.
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

/// The task `Cmd+Shift+B` runs: the explicit `isDefault` build task when
/// one is declared, else the first build-flagged task in source priority
/// order.
pub fn default_build_task(tasks: &[Task]) -> Option<&Task> {
    tasks
        .iter()
        .find(|t| t.is_build && t.is_default)
        .or_else(|| tasks.iter().find(|t| t.is_build))
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
            // VS Code labels a label-less entry by its full command line,
            // not the bare program: three label-less `npm` entries must
            // stay three tasks, not collapse to one "npm".
            let label = t
                .get("label")
                .and_then(|l| l.as_str())
                .unwrap_or(&line)
                .to_string();
            let is_build = match t.get("group") {
                Some(serde_json::Value::String(s)) => s == "build",
                Some(g) => g.get("kind").and_then(|k| k.as_str()) == Some("build"),
                None => false,
            };
            let is_default = t
                .get("group")
                .and_then(|g| g.get("isDefault"))
                .and_then(|d| d.as_bool())
                .unwrap_or(false);
            Some(Task {
                label,
                command: line,
                source: "tasks.json".to_string(),
                is_build,
                is_default,
                problem_matcher: t.get("problemMatcher").cloned(),
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
            is_default: false,
            problem_matcher: None,
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
            is_default: false,
            problem_matcher: None,
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
            is_default: false,
            problem_matcher: None,
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
        is_default: false,
        problem_matcher: None,
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
                is_default: false,
                problem_matcher: None,
            });
        }
    }
    if text.contains("pytest") {
        out.push(Task {
            label: "uv run pytest".to_string(),
            command: "uv run pytest".to_string(),
            source: "pyproject.toml".to_string(),
            is_build: false,
            is_default: false,
            problem_matcher: None,
        });
    }
    out
}

/// Strip `//` and `/* */` comments (outside strings) plus trailing commas
/// so VS Code's JSONC tasks.json parses with strict serde_json. Two
/// passes: comments first, then trailing commas, so a comma followed by
/// a comment followed by `}` still counts as trailing. Shared with
/// `dap::configs` (launch.json) and `config_layers` (settings layers),
/// which read their files under the same tolerance.
pub(crate) fn strip_jsonc(text: &str) -> String {
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

/// One task per (label, command line), first source winning. The label is
/// the identifier everything else keys on (`preLaunchTask`, the Run Task
/// picker), so a label the workspace declares must survive; the command is
/// what runs, so a distinct command must survive too. De-duplicating by
/// command line alone dropped a convention source's label whenever
/// tasks.json reused its command under another name, and the lookup then
/// reported "not found" for a task the workspace plainly declared (#336).
/// The one thing that still collapses is a true repeat: the same label
/// running the same line. A user who relabels `cargo build` sees it listed
/// under both names, by design. Two tasks sharing a label with DIFFERENT
/// commands both survive here; the lookups keyed on the label alone
/// (`preLaunchTask`, task-pane reuse) then resolve to the first, so a
/// workspace that reuses a label across commands gets first-source-wins at
/// the lookup, not in this list.
fn dedup(tasks: Vec<Task>) -> Vec<Task> {
    let mut seen = BTreeSet::new();
    tasks
        .into_iter()
        .filter(|t| seen.insert((t.label.clone(), t.command.clone())))
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
    fn tasks_json_problem_matcher_rides_along_raw() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".vscode/tasks.json"),
            r#"{ "tasks": [
  { "label": "cc", "command": "make", "problemMatcher": "$gcc" },
  { "label": "plain", "command": "true" }
] }"#,
        )
        .unwrap();
        let tasks = discover_tasks(tmp.path());
        let cc = tasks.iter().find(|t| t.label == "cc").unwrap();
        assert_eq!(
            cc.problem_matcher,
            Some(serde_json::json!("$gcc")),
            "the raw problemMatcher value travels with the task"
        );
        let plain = tasks.iter().find(|t| t.label == "plain").unwrap();
        assert_eq!(plain.problem_matcher, None);
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

    /// Everything that looks a task up does so by label, so de-duplication
    /// must key on labels too: a tasks.json entry that reuses a convention
    /// source's command line under its own name must not make the
    /// convention label unfindable (#336).
    #[test]
    fn a_relabelled_command_keeps_the_convention_label_findable() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".vscode/tasks.json"),
            r#"{ "tasks": [ { "label": "Build Project", "type": "shell", "command": "cargo build" } ] }"#,
        )
        .unwrap();
        let tasks = discover_tasks(tmp.path());
        let labels: Vec<&str> = tasks.iter().map(|t| t.label.as_str()).collect();
        assert!(labels.contains(&"Build Project"), "{labels:?}");
        assert!(
            labels.contains(&"cargo build"),
            "the Cargo label must survive a tasks.json entry with the same command: {labels:?}"
        );
    }

    /// A label-less tasks.json entry is labelled by its whole command line
    /// (VS Code's rule), so several entries sharing a program stay distinct
    /// tasks instead of collapsing to one bare "npm".
    #[test]
    fn label_less_entries_sharing_a_program_stay_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".vscode/tasks.json"),
            r#"{ "tasks": [
              { "type": "shell", "command": "npm", "args": ["run", "build"] },
              { "type": "shell", "command": "npm", "args": ["run", "test"] },
              { "type": "shell", "command": "npm", "args": ["run", "lint"] }
            ] }"#,
        )
        .unwrap();
        let tasks = discover_tasks(tmp.path());
        let pairs: Vec<(&str, &str)> = tasks
            .iter()
            .map(|t| (t.label.as_str(), t.command.as_str()))
            .collect();
        for cmd in ["npm run build", "npm run test", "npm run lint"] {
            assert!(
                pairs.contains(&(cmd, cmd)),
                "{cmd} must survive as its own task, labelled by its line: {pairs:?}"
            );
        }
    }

    /// Only a true repeat collapses: the same label running the same line.
    /// One that relabels a convention command lists under both names, by
    /// design.
    #[test]
    fn only_a_repeated_label_and_command_pair_collapses() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Makefile"), "build:\n\ttrue\n").unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".vscode/tasks.json"),
            r#"{ "tasks": [
              { "label": "make build", "type": "shell", "command": "make build" },
              { "label": "Build", "type": "shell", "command": "make build" }
            ] }"#,
        )
        .unwrap();
        let tasks = discover_tasks(tmp.path());
        let rows: Vec<(&str, &str, &str)> = tasks
            .iter()
            .map(|t| (t.label.as_str(), t.command.as_str(), t.source.as_str()))
            .collect();
        let repeats: Vec<_> = rows
            .iter()
            .filter(|r| (r.0, r.1) == ("make build", "make build"))
            .collect();
        assert_eq!(repeats.len(), 1, "the exact repeat folds to one: {rows:?}");
        assert_eq!(
            repeats[0].2, "tasks.json",
            "and the first source wins over the Makefile's"
        );
        assert!(
            rows.iter().any(|r| (r.0, r.1) == ("Build", "make build")),
            "a relabelled command keeps its own entry: {rows:?}"
        );
    }

    /// A label reused for a DIFFERENT command is not a repeat: both tasks
    /// survive, and the label-keyed lookup (the `preLaunchTask` rule) takes
    /// the first declared.
    #[test]
    fn a_label_reused_for_a_different_command_keeps_both_and_the_lookup_takes_the_first() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".vscode/tasks.json"),
            r#"{ "tasks": [
              { "label": "Build", "type": "shell", "command": "make a" },
              { "label": "Build", "type": "shell", "command": "make b" }
            ] }"#,
        )
        .unwrap();
        let tasks = discover_tasks(tmp.path());
        let builds: Vec<&str> = tasks
            .iter()
            .filter(|t| t.label == "Build")
            .map(|t| t.command.as_str())
            .collect();
        assert_eq!(
            builds,
            ["make a", "make b"],
            "both survive, declaration order kept"
        );
        assert_eq!(
            tasks
                .iter()
                .find(|t| t.label == "Build")
                .map(|t| t.command.as_str()),
            Some("make a"),
            "a lookup by label resolves to the first declared"
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
    fn an_explicit_vscode_default_build_wins_over_an_earlier_build_task() {
        // Two build-group tasks; only the second carries `isDefault: true`.
        // VS Code runs the default on Cmd+Shift+B, not the first-listed.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        std::fs::write(
            tmp.path().join(".vscode/tasks.json"),
            r#"{"tasks":[
  {"label":"build A","type":"shell","command":"make a","group":"build"},
  {"label":"build B","type":"shell","command":"make b","group":{"kind":"build","isDefault":true}}
]}"#,
        )
        .unwrap();
        let tasks = discover_tasks(tmp.path());
        assert_eq!(
            default_build_task(&tasks).unwrap().label,
            "build B",
            "the explicit default build outranks the first build-group task"
        );
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
