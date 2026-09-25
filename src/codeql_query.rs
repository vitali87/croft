//! Running a CodeQL query against the current database (#578), as VS
//! Code's "CodeQL: Run Query on Selected Database" does: `codeql` argument
//! lists and the query history, so the process itself stays with the
//! caller.
//!
//! A query whose metadata says `@kind problem` or `@kind path-problem`
//! produces alerts, so it runs through `database analyze` into SARIF and
//! opens in the SARIF viewer. Any other query produces a table, which runs
//! through `query run` and is decoded to CSV.

use std::path::{Path, PathBuf};

/// What a query's results look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Alerts, read as SARIF.
    Sarif,
    /// A result table, read as CSV.
    Table,
}

/// The `@kind` in a query's leading QLDoc comment, if it has one.
pub fn query_kind(source: &str) -> Option<String> {
    let doc = source.trim_start().strip_prefix("/**")?;
    let doc = &doc[..doc.find("*/")?];
    let after = &doc[doc.find("@kind")? + "@kind".len()..];
    after
        .split_whitespace()
        .next()
        .filter(|k| !k.starts_with('*') && !k.starts_with('@'))
        .map(str::to_string)
}

/// How a query with this source is run and read.
pub fn output_for(source: &str) -> Output {
    match query_kind(source).as_deref() {
        Some("problem" | "path-problem") => Output::Sarif,
        _ => Output::Table,
    }
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

/// `codeql` arguments running `query` on `db` into SARIF at `out`.
/// `--rerun` because a history entry run again means run again, not
/// "reuse the cached answer".
pub fn analyze_args(query: &Path, db: &Path, out: &Path) -> Vec<String> {
    vec![
        String::from("database"),
        String::from("analyze"),
        path(db),
        path(query),
        String::from("--format=sarif-latest"),
        format!("--output={}", path(out)),
        String::from("--rerun"),
    ]
}

/// `codeql` arguments running `query` on `db` into a BQRS file.
pub fn run_args(query: &Path, db: &Path, bqrs: &Path) -> Vec<String> {
    vec![
        String::from("query"),
        String::from("run"),
        format!("--database={}", path(db)),
        format!("--output={}", path(bqrs)),
        path(query),
    ]
}

/// `codeql` arguments decoding a BQRS file to CSV at `out`.
pub fn decode_args(bqrs: &Path, out: &Path) -> Vec<String> {
    vec![
        String::from("bqrs"),
        String::from("decode"),
        String::from("--format=csv"),
        format!("--output={}", path(out)),
        path(bqrs),
    ]
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RunStatus {
    Running,
    Succeeded,
    /// The first line of what `codeql` said.
    Failed(String),
}

/// One query history entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub query: PathBuf,
    pub database: String,
    /// Seconds since the Unix epoch.
    pub started: u64,
    pub seconds: u64,
    pub status: RunStatus,
    /// The SARIF or CSV the run wrote.
    pub output: PathBuf,
}

impl HistoryEntry {
    /// A history line: "✓ query.ql · db · 12s", "✗ … · failed: why",
    /// "… running".
    pub fn label(&self) -> String {
        let name = self
            .query
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match &self.status {
            RunStatus::Succeeded => format!(
                "\u{2713} {name} \u{b7} {} \u{b7} {}s",
                self.database, self.seconds
            ),
            RunStatus::Failed(why) => format!(
                "\u{2717} {name} \u{b7} {} \u{b7} failed: {why}",
                self.database
            ),
            RunStatus::Running => {
                format!("\u{2026} {name} \u{b7} {} \u{b7} running", self.database)
            }
        }
    }
}

/// The query history, newest first, capped at [`History::CAP`] entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct History {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

impl History {
    pub const CAP: usize = 100;

    pub fn load(path: &Path) -> History {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// Record a new run at the top, dropping the oldest past the cap.
    pub fn push(&mut self, entry: HistoryEntry) {
        self.entries.insert(0, entry);
        self.entries.truncate(Self::CAP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBLEM: &str = "/**\n * @name SQL injection\n * @kind path-problem\n * @id rust/sql\n */\nimport rust\nselect 1";

    #[test]
    fn the_kind_comes_from_the_leading_qldoc() {
        assert_eq!(query_kind(PROBLEM).as_deref(), Some("path-problem"));
        assert_eq!(
            query_kind("/** @kind problem */ select 1").as_deref(),
            Some("problem")
        );
        assert_eq!(query_kind("import rust\nselect 1"), None);
        // A @kind after the query body starts is not metadata.
        assert_eq!(query_kind("select 1\n/** @kind problem */"), None);
    }

    #[test]
    fn problems_read_as_sarif_and_everything_else_as_a_table() {
        assert_eq!(output_for(PROBLEM), Output::Sarif);
        assert_eq!(output_for("/** @kind problem */ select 1"), Output::Sarif);
        assert_eq!(output_for("/** @kind graph */ select 1"), Output::Table);
        assert_eq!(output_for("select 1"), Output::Table);
    }

    #[test]
    fn argument_lists_name_the_database_query_and_output() {
        let (q, db) = (Path::new("/w/q.ql"), Path::new("/dbs/app"));
        assert_eq!(
            analyze_args(q, db, Path::new("/out/r.sarif")),
            vec![
                "database",
                "analyze",
                "/dbs/app",
                "/w/q.ql",
                "--format=sarif-latest",
                "--output=/out/r.sarif",
                "--rerun"
            ]
        );
        assert_eq!(
            run_args(q, db, Path::new("/out/r.bqrs")),
            vec![
                "query",
                "run",
                "--database=/dbs/app",
                "--output=/out/r.bqrs",
                "/w/q.ql"
            ]
        );
        assert_eq!(
            decode_args(Path::new("/out/r.bqrs"), Path::new("/out/r.csv")),
            vec![
                "bqrs",
                "decode",
                "--format=csv",
                "--output=/out/r.csv",
                "/out/r.bqrs"
            ]
        );
    }

    fn entry(status: RunStatus) -> HistoryEntry {
        HistoryEntry {
            query: PathBuf::from("/w/sql.ql"),
            database: String::from("app"),
            started: 1,
            seconds: 12,
            status,
            output: PathBuf::from("/out/r.sarif"),
        }
    }

    #[test]
    fn history_labels_say_how_each_run_went() {
        assert_eq!(
            entry(RunStatus::Succeeded).label(),
            "\u{2713} sql.ql \u{b7} app \u{b7} 12s"
        );
        assert_eq!(
            entry(RunStatus::Failed(String::from("bad query"))).label(),
            "\u{2717} sql.ql \u{b7} app \u{b7} failed: bad query"
        );
        assert_eq!(
            entry(RunStatus::Running).label(),
            "\u{2026} sql.ql \u{b7} app \u{b7} running"
        );
    }

    #[test]
    fn history_is_newest_first_capped_and_survives_a_round_trip() {
        let mut h = History::default();
        for i in 0..(History::CAP as u64 + 5) {
            let mut e = entry(RunStatus::Succeeded);
            e.started = i;
            h.push(e);
        }
        assert_eq!(h.entries.len(), History::CAP);
        assert_eq!(h.entries[0].started, History::CAP as u64 + 4);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h/history.json");
        h.save(&path).unwrap();
        assert_eq!(History::load(&path), h);
        assert_eq!(
            History::load(&dir.path().join("missing.json")),
            History::default()
        );
    }
}
