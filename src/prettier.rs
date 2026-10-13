//! Prettier as the formatter for projects that use it (#1255).
//!
//! VS Code's Prettier extension formats with the project's own Prettier and
//! `.prettierrc`; typescript-language-server's formatter knows neither, so a
//! Prettier project saved from croft failed `prettier --check`. When a file
//! Prettier handles sits under a Prettier config and the project has
//! `node_modules/.bin/prettier`, Format Document and Format on Save pipe the
//! buffer through `prettier --stdin-filepath` instead of the language server.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Extensions Prettier formats out of the box. Anything else keeps its
/// language server, so a Rust file in a repo with a `.prettierrc` still goes
/// to rustfmt.
const EXTENSIONS: &[&str] = &[
    "js",
    "jsx",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "mts",
    "cts",
    "json",
    "jsonc",
    "json5",
    "css",
    "scss",
    "less",
    "html",
    "htm",
    "vue",
    "md",
    "markdown",
    "mdx",
    "yaml",
    "yml",
    "graphql",
    "gql",
    "hbs",
    "handlebars",
];

/// The config files Prettier looks for, besides a `"prettier"` key in
/// `package.json`.
const CONFIGS: &[&str] = &[
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.json5",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.toml",
    ".prettierrc.js",
    ".prettierrc.cjs",
    ".prettierrc.mjs",
    ".prettierrc.ts",
    ".prettierrc.cts",
    ".prettierrc.mts",
    "prettier.config.js",
    "prettier.config.cjs",
    "prettier.config.mjs",
    "prettier.config.ts",
    "prettier.config.cts",
    "prettier.config.mts",
];

#[cfg(windows)]
const BIN: &str = "node_modules/.bin/prettier.cmd";
#[cfg(not(windows))]
const BIN: &str = "node_modules/.bin/prettier";

/// The project's Prettier for `file`, when Prettier should format it: the
/// file is a kind Prettier formats, a folder above it holds a Prettier
/// config, and a folder above it has Prettier installed. Both are looked up
/// from the file upward, as Prettier and the VS Code extension do, so a
/// package in a monorepo finds the root's.
pub fn for_file(file: &Path) -> Option<PathBuf> {
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    if !EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let dirs = || file.ancestors().skip(1);
    dirs().find(|d| has_config(d))?;
    dirs().map(|d| d.join(BIN)).find(|b| b.is_file())
}

fn has_config(dir: &Path) -> bool {
    CONFIGS.iter().any(|c| dir.join(c).is_file())
        || std::fs::read_to_string(dir.join("package.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|v| v.get("prettier").is_some())
}

/// A Prettier run on a background thread, so a slow `node` start never
/// stalls the UI.
pub struct Job {
    pub path: PathBuf,
    /// The text sent to Prettier: the result only applies over this text.
    pub sent: String,
    started: Instant,
    rx: Receiver<Result<String, String>>,
}

/// How long a run may take before the save it holds goes ahead without it.
const TIMEOUT: Duration = Duration::from_secs(20);

impl Job {
    pub fn spawn(bin: PathBuf, path: PathBuf, sent: String) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let (file, text) = (path.clone(), sent.clone());
        std::thread::spawn(move || {
            let _ = tx.send(run(&bin, &file, &text));
        });
        Self {
            path,
            sent,
            started: Instant::now(),
            rx,
        }
    }

    /// The formatted text or the failure, once the run is over.
    pub fn poll(&self, now: Instant) -> Option<Result<String, String>> {
        match self.rx.try_recv() {
            Ok(reply) => Some(reply),
            Err(TryRecvError::Disconnected) => {
                Some(Err(String::from("it stopped without a reply")))
            }
            Err(TryRecvError::Empty) if now.duration_since(self.started) > TIMEOUT => {
                Some(Err(format!("it took longer than {}s", TIMEOUT.as_secs())))
            }
            Err(TryRecvError::Empty) => None,
        }
    }
}

/// Run `bin --stdin-filepath file` over `text`. Prettier resolves the config
/// and `.prettierignore` from that path, so cwd is the file's folder.
fn run(bin: &Path, file: &Path, text: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(bin);
    cmd.arg("--stdin-filepath")
        .arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = file.parent() {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // Prettier reads all of stdin before it writes, so this cannot deadlock.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("no output");
        return Err(first.trim_start_matches("[error] ").to_string());
    }
    String::from_utf8(out.stdout)
        .map(|s| s.replace("\r\n", "\n"))
        .map_err(|_| String::from("its output was not UTF-8"))
}

/// The edit that turns `old` (the buffer's lines) into `new`: only the rows
/// between the unchanged head and tail, so the caret and folds above and
/// below the change stay put.
pub fn edits_for(old: &[String], new: &str) -> Vec<crate::widgets::editor::TextSpanEdit> {
    let new: Vec<&str> = new.split('\n').collect();
    let max = old.len().min(new.len());
    let head = (0..max).take_while(|&i| old[i] == new[i]).count();
    let tail = (0..max - head)
        .take_while(|&i| old[old.len() - 1 - i] == new[new.len() - 1 - i])
        .count();
    let edit = |start, end, new_text| crate::widgets::editor::TextSpanEdit {
        start,
        end,
        new_text,
        utf16: false,
    };
    if tail == 0 {
        let last = old.len().saturating_sub(1);
        let end = (last, old.get(last).map_or(0, |l| l.chars().count()));
        return vec![edit((0, 0), end, new.join("\n"))];
    }
    let middle: String = new[head..new.len() - tail]
        .iter()
        .map(|l| format!("{l}\n"))
        .collect();
    vec![edit((head, 0), (old.len() - tail, 0), middle)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project with `node_modules/.bin/prettier` (an empty stand-in) and
    /// `files` written under it.
    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        tmp
    }

    const BIN: (&str, &str) = ("node_modules/.bin/prettier", "");

    #[test]
    fn a_prettierrc_and_a_local_prettier_select_it() {
        let tmp = project(&[(".prettierrc", "{}"), BIN, ("app.ts", "")]);
        assert_eq!(
            for_file(&tmp.path().join("app.ts")),
            Some(tmp.path().join("node_modules/.bin/prettier"))
        );
    }

    #[test]
    fn every_config_spelling_counts() {
        for name in [
            ".prettierrc.json",
            ".prettierrc.yaml",
            ".prettierrc.toml",
            ".prettierrc.cjs",
            "prettier.config.js",
            "prettier.config.mjs",
        ] {
            let tmp = project(&[(name, ""), BIN, ("a.js", "")]);
            assert!(for_file(&tmp.path().join("a.js")).is_some(), "{name}");
        }
    }

    #[test]
    fn a_prettier_key_in_package_json_is_a_config() {
        let tmp = project(&[
            ("package.json", r#"{"name":"x","prettier":{"semi":false}}"#),
            BIN,
            ("a.tsx", ""),
        ]);
        assert!(for_file(&tmp.path().join("a.tsx")).is_some());
    }

    #[test]
    fn a_monorepo_package_finds_the_root_config_and_prettier() {
        let tmp = project(&[
            (".prettierrc", "{}"),
            BIN,
            ("packages/web/package.json", r#"{"name":"web"}"#),
            ("packages/web/src/page.css", ""),
        ]);
        assert_eq!(
            for_file(&tmp.path().join("packages/web/src/page.css")),
            Some(tmp.path().join("node_modules/.bin/prettier"))
        );
    }

    #[test]
    fn no_config_means_no_prettier() {
        let tmp = project(&[
            (
                "package.json",
                r#"{"name":"x","devDependencies":{"prettier":"3"}}"#,
            ),
            BIN,
            ("a.ts", ""),
        ]);
        assert_eq!(for_file(&tmp.path().join("a.ts")), None);
    }

    #[test]
    fn a_config_without_an_installed_prettier_is_not_used() {
        let tmp = project(&[(".prettierrc", "{}"), ("a.ts", "")]);
        assert_eq!(for_file(&tmp.path().join("a.ts")), None);
    }

    #[test]
    fn files_prettier_does_not_format_keep_their_server() {
        let tmp = project(&[(".prettierrc", "{}"), BIN, ("main.rs", ""), ("x.py", "")]);
        assert_eq!(for_file(&tmp.path().join("main.rs")), None);
        assert_eq!(for_file(&tmp.path().join("x.py")), None);
    }

    fn lines(s: &str) -> Vec<String> {
        s.split('\n').map(String::from).collect()
    }

    fn apply(old: &str, new: &str) -> String {
        let mut l = lines(old);
        let edits = edits_for(&l, new);
        crate::widgets::editor::apply_span_edits_to_lines(&mut l, &edits);
        l.join("\n")
    }

    #[test]
    fn edits_for_rewrites_only_the_changed_rows() {
        let old = "a\nb;\nc\n";
        let new = "a\nb\nc\n";
        let e = edits_for(&lines(old), new);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].start, e[0].end), ((1, 0), (2, 0)));
        assert_eq!(apply(old, new), new);
    }

    #[test]
    fn edits_for_handles_added_removed_and_unterminated_text() {
        for (old, new) in [
            ("a\nb\n", "a\nx\ny\nb\n"),
            ("a\nx\ny\nb\n", "a\nb\n"),
            ("one", "two\n"),
            ("a\nb", "a\nc"),
            ("", "x\n"),
            ("x\n", "x\n"),
        ] {
            assert_eq!(apply(old, new), new, "{old:?} -> {new:?}");
        }
    }
}
