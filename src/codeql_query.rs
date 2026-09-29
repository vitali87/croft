//! Running a CodeQL query against the current database (#578), as VS
//! Code's "CodeQL: Run Query on Selected Database" does: `codeql` argument
//! lists and the query history, so the process itself stays with the
//! caller.
//!
//! A query whose metadata says `@kind problem` or `@kind path-problem`
//! produces alerts, so it runs through `database analyze` into SARIF and
//! opens in the SARIF viewer. Any other query produces a table, which runs
//! through `query run` and is decoded to CSV.
//!
//! "CodeQL: Create Query" starts a new query from [`query_template`],
//! with a pack file beside it when it has none.

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

/// Whether `query` is a query suite (`.qls`), which `database analyze`
/// runs whole.
pub fn is_suite(query: &Path) -> bool {
    query.extension().is_some_and(|e| e == "qls")
}

/// `codeql` arguments listing the queries a suite selects, as JSON.
pub fn resolve_suite_args(suite: &Path) -> Vec<String> {
    vec![
        String::from("resolve"),
        String::from("queries"),
        String::from("--format=json"),
        path(suite),
    ]
}

/// The file names of the queries in `resolved` (the JSON array `codeql
/// resolve queries` prints) whose results are not alerts, going by each
/// file's text as `read` gives it. `database analyze`, the only way to run
/// a suite, cannot produce their tables, so a suite holding any is refused
/// up front rather than failing inside the CLI.
pub fn table_queries_in_suite(
    resolved: &str,
    read: impl Fn(&Path) -> Option<String>,
) -> Result<Vec<String>, String> {
    let paths: Vec<PathBuf> = serde_json::from_str(resolved)
        .map_err(|e| format!("could not read the suite's queries: {e}"))?;
    Ok(paths
        .iter()
        .filter(|p| read(p).is_some_and(|src| output_for(&src) == Output::Table))
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect())
}

/// How the file `query`, whose text is `source`, is run and read: a suite
/// always through `database analyze` into SARIF, since only that runs one;
/// a single query by its `@kind`.
pub fn output_of(query: &Path, source: &str) -> Output {
    if is_suite(query) {
        Output::Sarif
    } else {
        output_for(source)
    }
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

/// The evaluator log of the run whose results are `output`: every run
/// writes one into its own folder, beside its results.
pub fn evaluator_log(output: &Path) -> PathBuf {
    output.with_file_name("evaluator-log.jsonl")
}

/// The folder the run whose results are `output` writes its query log into
/// (`--logdir`), beside its results.
pub fn query_log_dir(output: &Path) -> PathBuf {
    output.with_file_name("logs")
}

/// The newest of `logs`, each a `*.log` file with its modification time:
/// the latest time wins, the greater file name breaks a tie.
pub fn newest_query_log(logs: Vec<(PathBuf, std::time::SystemTime)>) -> Option<PathBuf> {
    logs.into_iter()
        .filter(|(p, _)| p.extension().is_some_and(|e| e == "log"))
        .max_by(|(a, at), (b, bt)| at.cmp(bt).then_with(|| a.file_name().cmp(&b.file_name())))
        .map(|(p, _)| p)
}

/// The human-readable summary of the run whose results are `output`, made
/// from its evaluator log on first request.
pub fn evaluator_log_summary(output: &Path) -> PathBuf {
    output.with_file_name("evaluator-log.summary.txt")
}

/// `codeql` arguments summarising the evaluator log `log` as text at `out`
/// (VS Code's "Show Evaluator Log (Summary Text)").
pub fn log_summary_args(log: &Path, out: &Path) -> Vec<String> {
    vec![
        String::from("generate"),
        String::from("log-summary"),
        String::from("--format=text"),
        path(log),
        path(out),
    ]
}

/// The structured, one-predicate-per-line summary of the run whose results
/// are `output`, made from its evaluator log for the log viewer.
pub fn evaluator_log_predicates(output: &Path) -> PathBuf {
    output.with_file_name("evaluator-log.predicates.jsonl")
}

/// `codeql` arguments summarising the evaluator log `log` as predicate
/// records at `out` (VS Code's "Show Evaluator Log (Viewer)").
pub fn log_predicates_args(log: &Path, out: &Path) -> Vec<String> {
    vec![
        String::from("generate"),
        String::from("log-summary"),
        String::from("--format=predicates"),
        path(log),
        path(out),
    ]
}

/// `codeql` arguments rendering the help of `query`, its `.qhelp` or `.md`
/// beside it, as Markdown at `out` (VS Code's "CodeQL: Preview Query
/// Help").
pub fn query_help_args(query: &Path, out: &Path) -> Vec<String> {
    vec![
        String::from("generate"),
        String::from("query-help"),
        String::from("--format=markdown"),
        format!("--output={}", path(out)),
        path(query),
    ]
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
        format!("--evaluator-log={}", path(&evaluator_log(out))),
        format!("--logdir={}", path(&query_log_dir(out))),
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
        format!("--evaluator-log={}", path(&evaluator_log(bqrs))),
        format!("--logdir={}", path(&query_log_dir(bqrs))),
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

/// `codeql` arguments writing the alerts `query` found in `db` as CSV at
/// `out`, from the results the database keeps (#578, "View Alerts (CSV)").
pub fn interpret_csv_args(db: &Path, query: &Path, out: &Path) -> Vec<String> {
    vec![
        String::from("database"),
        String::from("interpret-results"),
        String::from("--format=csv"),
        format!("--output={}", path(out)),
        path(db),
        path(query),
    ]
}

/// The BQRS `db` keeps for `query` from its last analysis: the newest
/// `<stem>.bqrs` under its `results` folder, which the CLI files by pack.
pub fn kept_bqrs(db: &Path, query: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, want: &std::ffi::OsStr, depth: usize, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 12 {
                    walk(&p, want, depth + 1, out);
                }
            } else if p.file_name() == Some(want) {
                out.push(p);
            }
        }
    }
    let want = std::ffi::OsString::from(format!("{}.bqrs", query.file_stem()?.to_string_lossy()));
    let mut found = Vec::new();
    walk(&db.join("results"), &want, 0, &mut found);
    found
        .into_iter()
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}

/// `codeql` arguments dereferencing a `.qlref` test file to the query it
/// names (#578, "open a referenced file").
pub fn qlref_args(qlref: &Path) -> Vec<String> {
    vec![String::from("resolve"), String::from("qlref"), path(qlref)]
}

/// The query `codeql resolve qlref` answered with: its `resolvedPath`.
pub fn parse_qlref(json: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    v.get("resolvedPath")?.as_str().map(PathBuf::from)
}

/// `codeql` arguments decoding result set `set` of `bqrs` as JSON with each
/// entity's label and location, for navigating the results (#578).
pub fn decode_locations_args(bqrs: &Path, out: &Path, set: &str) -> Vec<String> {
    vec![
        String::from("bqrs"),
        String::from("decode"),
        String::from("--format=json"),
        String::from("--entities=url,string"),
        format!("--result-set={set}"),
        format!("--output={}", path(out)),
        path(bqrs),
    ]
}

/// Where the locations of a results CSV's cells are kept: beside it, as
/// `<name>.locations.json`.
pub fn locations_path(csv: &Path) -> PathBuf {
    csv.with_extension("locations.json")
}

/// Where a result cell's entity is: a file and a 1-based line and column.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CellLoc {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
}

/// One result row: each cell's text as the CSV shows it, and where each
/// cell's entity is, if it is an entity with a location.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RowLocs {
    pub cells: Vec<String>,
    pub locs: Vec<Option<CellLoc>>,
}

/// Read a result set decoded by [`decode_locations_args`] into rows.
pub fn parse_row_locations(json: &str) -> Vec<RowLocs> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let cell = |c: &serde_json::Value| -> (String, Option<CellLoc>) {
        match c {
            serde_json::Value::Object(o) => {
                let text = o
                    .get("label")
                    .and_then(|l| l.as_str())
                    .unwrap_or_default()
                    .to_string();
                let loc = o.get("url").and_then(|u| {
                    let uri = u.get("uri")?.as_str()?;
                    let rest = uri.strip_prefix("file://")?;
                    Some(CellLoc {
                        path: PathBuf::from(crate::sarif::resolve::percent_decode(rest)),
                        line: u.get("startLine")?.as_u64()? as u32,
                        column: u.get("startColumn").and_then(|c| c.as_u64()).unwrap_or(1) as u32,
                    })
                });
                (text, loc)
            }
            serde_json::Value::String(s) => (s.clone(), None),
            other => (other.to_string(), None),
        }
    };
    v.get("tuples")
        .and_then(|t| t.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.as_array())
                .map(|r| {
                    let (cells, locs) = r.iter().map(cell).unzip();
                    RowLocs { cells, locs }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The locations of the row whose cells read `cells`, found by content
/// rather than position, so a sorted table still leads to the right code.
pub fn row_locations<'a>(rows: &'a [RowLocs], cells: &[String]) -> Option<&'a [Option<CellLoc>]> {
    rows.iter()
        .find(|r| r.cells == cells)
        .map(|r| r.locs.as_slice())
}

/// `codeql` arguments listing a BQRS file's result sets as JSON.
pub fn info_args(bqrs: &Path) -> Vec<String> {
    vec![
        String::from("bqrs"),
        String::from("info"),
        String::from("--format=json"),
        path(bqrs),
    ]
}

/// The result sets `codeql bqrs info --format=json` lists, as (name, rows),
/// in its order.
pub fn parse_result_sets(json: &str) -> Vec<(String, u64)> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    v.get("result-sets")
        .and_then(|s| s.as_array())
        .map(|sets| {
            sets.iter()
                .filter_map(|s| {
                    Some((
                        s.get("name")?.as_str()?.to_string(),
                        s.get("rows").and_then(|r| r.as_u64()).unwrap_or(0),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The result set a run's results show first: `#select`, else the first.
pub fn main_result_set(sets: &[(String, u64)]) -> Option<&str> {
    sets.iter()
        .find(|(n, _)| n == "#select")
        .or(sets.first())
        .map(|(n, _)| n.as_str())
}

/// Where result set `set` of a run whose main table is `output` is
/// decoded: `output` itself for the main set, else `results-<set>.csv`
/// beside it (#578). A decoded file per set, since `bqrs decode` without a
/// set writes them all into one CSV, header rows and all.
pub fn result_set_file(output: &Path, set: &str, main: bool) -> PathBuf {
    if main {
        return output.to_path_buf();
    }
    let name: String = set
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    output.with_file_name(format!("results-{name}.csv"))
}

/// `codeql` arguments decoding result set `set` of `bqrs` to CSV at `out`.
pub fn decode_set_args(bqrs: &Path, out: &Path, set: &str) -> Vec<String> {
    let mut args = decode_args(bqrs, out);
    args.insert(3, format!("--result-set={set}"));
    args
}

/// A finished table run's result sets, as (name, file), the main one
/// first: the files [`result_set_file`] names that exist.
pub fn result_set_files(output: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if output.is_file() {
        out.push((String::from("#select"), output.to_path_buf()));
    }
    let mut others: Vec<(String, PathBuf)> = output
        .parent()
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let set = name
                .strip_prefix("results-")?
                .strip_suffix(".csv")?
                .to_string();
            Some((set, e.path()))
        })
        .collect();
    others.sort();
    out.extend(others);
    out
}

/// `codeql` arguments upgrading `db` to the CLI's current schema (VS Code's
/// "CodeQL: Upgrade Database").
pub fn upgrade_args(db: &Path) -> Vec<String> {
    vec![String::from("database"), String::from("upgrade"), path(db)]
}

/// A job that rewrites a database in place (#578): VS Code's "CodeQL:
/// Upgrade Database" and its three cache commands, which all run `codeql
/// database cleanup` with a different `--cache-cleanup` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbJob {
    Upgrade,
    ClearCache,
    TrimCache,
    TrimCacheToOverlay,
}

impl DbJob {
    /// The `codeql` arguments running this job on `db`.
    pub fn args(self, db: &Path) -> Vec<String> {
        let mode = match self {
            DbJob::Upgrade => return upgrade_args(db),
            DbJob::ClearCache => "clear",
            DbJob::TrimCache => "trim",
            DbJob::TrimCacheToOverlay => "overlay",
        };
        vec![
            String::from("database"),
            String::from("cleanup"),
            format!("--cache-cleanup={mode}"),
            path(db),
        ]
    }

    /// What the job is, for "already running" and "wait for" messages.
    pub fn noun(self) -> &'static str {
        match self {
            DbJob::Upgrade => "upgrade",
            _ => "cache cleanup",
        }
    }

    /// What the job does, after "before" in the message refusing it while a
    /// query runs.
    pub fn gerund(self) -> &'static str {
        match self {
            DbJob::Upgrade => "upgrading",
            _ => "cleaning up the cache",
        }
    }

    /// The status line while the job runs on database `name`.
    pub fn running(self, name: &str) -> String {
        match self {
            DbJob::Upgrade => format!("Upgrading CodeQL database {name}\u{2026}"),
            DbJob::ClearCache => format!("Clearing the cache of CodeQL database {name}\u{2026}"),
            DbJob::TrimCache | DbJob::TrimCacheToOverlay => {
                format!("Trimming the cache of CodeQL database {name}\u{2026}")
            }
        }
    }

    /// The status line once the job has finished: `Ok` or the CLI's error.
    pub fn finished(self, name: &str, outcome: Result<(), String>) -> String {
        match (self, outcome) {
            (DbJob::Upgrade, Ok(())) => format!("Upgraded CodeQL database {name}"),
            (DbJob::ClearCache, Ok(())) => format!("Cleared the cache of CodeQL database {name}"),
            (DbJob::TrimCache, Ok(())) => format!("Trimmed the cache of CodeQL database {name}"),
            (DbJob::TrimCacheToOverlay, Ok(())) => {
                format!("Trimmed the cache of CodeQL database {name} to its overlay base")
            }
            (DbJob::Upgrade, Err(why)) => {
                format!("Could not upgrade CodeQL database {name}: {why}")
            }
            (_, Err(why)) => {
                format!("Could not clean up the cache of CodeQL database {name}: {why}")
            }
        }
    }
}

/// `codeql` arguments installing the dependencies of the pack in `dir` (VS
/// Code's "CodeQL: Install Pack Dependencies").
pub fn pack_install_args(dir: &Path) -> Vec<String> {
    vec![String::from("pack"), String::from("install"), path(dir)]
}

/// `codeql` arguments downloading `packs` from the registry (VS Code's
/// "CodeQL: Download Packs").
pub fn pack_download_args(packs: &[String]) -> Vec<String> {
    let mut args = vec![String::from("pack"), String::from("download")];
    args.extend(packs.iter().cloned());
    args
}

/// The pack a "Run Queries in Published Pack" reference names (#578):
/// `scope/name`, optionally `@version`, without a `:path` into the pack,
/// which `pack download` does not take. `None` when it names no pack.
pub fn published_pack(reference: &str) -> Option<&str> {
    let pack = reference.split(':').next()?.trim();
    let name = pack.split('@').next()?;
    let (scope, rest) = name.split_once('/')?;
    // Pack scopes and names are lowercase letters, digits and hyphens.
    let ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    (ok(scope) && ok(rest)).then_some(pack)
}

/// The pack references in what the user typed: separated by spaces or
/// commas, each `scope/name`, optionally with `@version`.
pub fn parse_pack_list(input: &str) -> Vec<String> {
    input
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The pack file and query of VS Code's "CodeQL: Quick Query" for the
/// library `module` (see [`language_module`]): a throwaway pack depending on
/// `codeql/<module>-all`, and a query importing it that selects nothing yet.
pub fn quick_query(module: &str) -> (String, String) {
    let pack = format!(
        "name: croft/quick-query-{module}\nversion: 0.0.0\ndependencies:\n  codeql/{module}-all: \"*\"\n"
    );
    let query = format!(
        "/**\n * A quick query: edit it and run \"CodeQL: Run Query on Selected Database\".\n */\n\nimport {module}\n\nselect \"\"\n"
    );
    (pack, query)
}

/// VS Code's "Compare Results" for two result tables: the rows of `old`
/// that `new` lacks and those `new` has that `old` did not, as one CSV
/// whose first column names the run each row is from. Rows are compared
/// whole and counted, so a duplicated row that lost a copy still shows.
/// Tables with different columns cannot be compared.
pub fn compare_tables(
    old: &str,
    new: &str,
    old_label: &str,
    new_label: &str,
) -> Result<String, String> {
    fn read(text: &str) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
        let mut r = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(text.as_bytes());
        let header = r
            .headers()
            .map_err(|e| e.to_string())?
            .iter()
            .map(str::to_string)
            .collect();
        let rows = r
            .records()
            .map(|rec| rec.map(|rec| rec.iter().map(str::to_string).collect()))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        Ok((header, rows))
    }
    let (old_header, old_rows) = read(old)?;
    let (new_header, new_rows) = read(new)?;
    if old_header != new_header {
        return Err(String::from("the two runs' result columns differ"));
    }
    // Everything in `a` beyond what `b` has, row for row.
    fn missing<'a>(a: &'a [Vec<String>], b: &[Vec<String>]) -> Vec<&'a Vec<String>> {
        let mut left: std::collections::HashMap<&Vec<String>, usize> =
            std::collections::HashMap::new();
        for row in b {
            *left.entry(row).or_default() += 1;
        }
        a.iter()
            .filter(|row| match left.get_mut(row) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    false
                }
                _ => true,
            })
            .collect()
    }
    let mut w = csv::Writer::from_writer(Vec::new());
    let io = |e: csv::Error| e.to_string();
    w.write_record(std::iter::once("run").chain(old_header.iter().map(String::as_str)))
        .map_err(io)?;
    for (label, row) in missing(&old_rows, &new_rows)
        .into_iter()
        .map(|r| (old_label, r))
        .chain(
            missing(&new_rows, &old_rows)
                .into_iter()
                .map(|r| (new_label, r)),
        )
    {
        w.write_record(std::iter::once(label).chain(row.iter().map(String::as_str)))
            .map_err(io)?;
    }
    let bytes = w.into_inner().map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Why a `codeql` call failed, read from its stderr (#578): the reason
/// and the fix the CLI suggests, rather than its first line, which is
/// usually progress ("Compiling query plan for …"). Lines are kept in the
/// CLI's own words: the fatal-error line leads, then each distinct `ERROR:`
/// line, then every line suggesting
/// what to do ("Consider running …", "Try …", "Run …", "Use --…", a hint).
/// With none of those, the last non-empty line, where a tool usually
/// ends on its reason.
pub fn failure_reason(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut parts: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if !s.is_empty() && !parts.iter().any(|p| p == s || p.contains(s)) {
            parts.push(s.to_string());
        }
    };
    for l in &lines {
        if l.starts_with("A fatal error occurred:") {
            push(l);
        }
    }
    for l in &lines {
        if l.starts_with("ERROR:") {
            push(l);
        }
    }
    for l in &lines {
        let lower = l.to_ascii_lowercase();
        let suggests = ["consider ", "try ", "run ", "use --", "hint:", "please run"]
            .iter()
            .any(|w| lower.starts_with(w) || lower.contains(&format!("({w}")));
        if suggests {
            push(l);
        }
    }
    if parts.is_empty() {
        return lines
            .last()
            .map_or_else(|| String::from("codeql failed"), |l| l.to_string());
    }
    parts.join(" · ")
}

/// Where a run keeps the text of `query` as it was run (#578, "View Query
/// Text"): a `query` folder beside the run's `output`, under the query's
/// own file name, so the tab reads as the query.
pub fn query_text_path(output: &Path, query: &Path) -> PathBuf {
    let name = query
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("query.ql"));
    output
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("query")
        .join(name)
}

/// The `.ql` queries `paths` name (#578, "run queries in selected files"):
/// each `.ql` file itself, and every `.ql` under a folder, skipping hidden
/// folders. Sorted and without repeats; a path that is neither is ignored.
pub fn queries_in(paths: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let hidden = p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'));
            if hidden {
                continue;
            }
            if p.is_dir() {
                if depth < 16 {
                    walk(&p, depth + 1, out);
                }
            } else if p.extension().is_some_and(|e| e == "ql") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(p, 0, &mut out);
        } else if p.is_file() && p.extension().is_some_and(|e| e == "ql") {
            out.push(p.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `codeql` arguments printing the CLI's bare version number.
pub fn version_args() -> Vec<String> {
    vec![String::from("version"), String::from("--format=terse")]
}

/// The text "CodeQL: Copy Version Information" puts on the clipboard: croft's
/// version, the CodeQL CLI's (or why it could not be read) and the platform,
/// one per line, ready to paste into a bug report.
pub fn version_information(croft: &str, cli: &Result<String, String>) -> String {
    let cli = match cli {
        Ok(v) => v.trim().to_string(),
        Err(why) => format!("unavailable ({why})"),
    };
    format!(
        "croft version: {croft}\nCodeQL CLI version: {cli}\nPlatform: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// The queries in one CodeQL pack, as the side bar's Queries section groups
/// them (#578).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPack {
    /// The pack's `name:`, its folder's name when it has none, or
    /// [`NO_PACK`] for queries outside any pack.
    pub name: String,
    /// The extractor id (`python`, `cpp`, …) the pack targets, when it says.
    pub language: Option<String>,
    /// The pack's folder; the workspace root for [`NO_PACK`]. Query rows are
    /// shown relative to it.
    pub dir: PathBuf,
    /// Its `.ql` files, sorted.
    pub queries: Vec<PathBuf>,
}

/// The group for queries with no `qlpack.yml` above them.
pub const NO_PACK: &str = "(no pack)";

/// Files [`discover`] looks at before it stops, so a huge tree cannot stall
/// opening the side bar.
pub const DISCOVER_CAP: usize = 50_000;

fn is_pack_file(name: &std::ffi::OsStr) -> bool {
    name == "qlpack.yml" || name == "codeql-pack.yml"
}

/// A pack file's top-level `name:`, unquoted. A line scan, not YAML: the
/// key is a plain scalar in every pack file the CLI writes.
pub fn pack_name(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix("name:")?.trim();
        let v = v.trim_matches(|c| c == '"' || c == '\'');
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// The language a pack file targets: its `extractor:`, else the `<lang>`
/// of a `codeql/<lang>-all` dependency.
pub fn pack_language(text: &str) -> Option<String> {
    let extractor = text.lines().find_map(|l| {
        let v = l.strip_prefix("extractor:")?.trim();
        let v = v.trim_matches(|c| c == '"' || c == '\'');
        (!v.is_empty()).then(|| v.to_string())
    });
    extractor.or_else(|| {
        text.lines().find_map(|l| {
            let rest = &l[l.find("codeql/")? + "codeql/".len()..];
            rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .next()?
                .strip_suffix("-all")
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    })
}

/// Every `.ql` under `root`, each grouped under its nearest ancestor pack
/// file (#578). Ignored files and noise folders are skipped, as the file
/// finder skips them. Packs sort by name then folder, with [`NO_PACK`] last.
pub fn discover(root: &Path) -> Vec<QueryPack> {
    use std::collections::BTreeMap;
    let mut packs: BTreeMap<PathBuf, (String, Option<String>)> = BTreeMap::new();
    let mut queries = Vec::new();
    let mut seen = 0usize;
    for entry in ignore::WalkBuilder::new(root)
        .git_ignore(true)
        .require_git(false)
        .hidden(false)
        .filter_entry(|e| {
            e.depth() == 0
                || !e.file_type().is_some_and(|t| t.is_dir())
                || !crate::widgets::file_finder::is_noise_dir(e.file_name())
        })
        .build()
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        seen += 1;
        if seen > DISCOVER_CAP {
            break;
        }
        let path = entry.into_path();
        if path.file_name().is_some_and(is_pack_file) {
            let Some(dir) = path.parent() else { continue };
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let name = pack_name(&text).unwrap_or_else(|| {
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            // Both files in one folder: the first read wins, as either names
            // the same pack.
            packs
                .entry(dir.to_path_buf())
                .or_insert((name, pack_language(&text)));
        } else if path.extension().is_some_and(|e| e == "ql") {
            queries.push(path);
        }
    }
    let mut grouped: BTreeMap<Option<PathBuf>, Vec<PathBuf>> = BTreeMap::new();
    for q in queries {
        let pack = q
            .ancestors()
            .skip(1)
            .take_while(|d| d.starts_with(root))
            .find(|d| packs.contains_key(*d))
            .map(Path::to_path_buf);
        grouped.entry(pack).or_default().push(q);
    }
    let mut out: Vec<QueryPack> = grouped
        .into_iter()
        .map(|(dir, mut queries)| {
            queries.sort();
            match dir {
                Some(dir) => {
                    let (name, language) = packs[&dir].clone();
                    QueryPack {
                        name,
                        language,
                        dir,
                        queries,
                    }
                }
                None => QueryPack {
                    name: NO_PACK.to_string(),
                    language: None,
                    dir: root.to_path_buf(),
                    queries,
                },
            }
        })
        .collect();
    out.sort_by(|a, b| {
        (a.name == NO_PACK, &a.name, &a.dir).cmp(&(b.name == NO_PACK, &b.name, &b.dir))
    });
    out
}

/// The CodeQL library for a language id, which is also what `import` names
/// and what `codeql/<id>-all` depends on. The ids a database or a pack file
/// may use for a language the library covers (`typescript`, `kotlin`, `c`)
/// map to it; anything else is `None`.
pub fn language_module(lang: &str) -> Option<&'static str> {
    Some(match lang.trim().to_ascii_lowercase().as_str() {
        "cpp" | "c" | "c++" => "cpp",
        "csharp" | "c#" => "csharp",
        "go" => "go",
        "java" | "kotlin" => "java",
        "javascript" | "typescript" => "javascript",
        "python" => "python",
        "ruby" => "ruby",
        "rust" => "rust",
        "swift" => "swift",
        "actions" => "actions",
        _ => return None,
    })
}

/// `name` in lower-case words joined by hyphens, as a query `@id` or a pack
/// name wants it: "Find SQL_injection" is `find-sql-injection`.
fn kebab(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// A starter query called `name`, as VS Code's "Create Query" writes one: a
/// problem query reporting every file, which runs as soon as it is saved.
/// With no known language there is no library to import, so it is a plain
/// `select` of a string and says no `@kind`.
pub fn query_template(lang: Option<&str>, name: &str) -> String {
    let id = match kebab(name) {
        k if k.is_empty() => String::from("query"),
        k => k,
    };
    match lang.and_then(language_module) {
        Some(module) => format!(
            "/**\n * @name {name}\n * @description Describe what this query finds.\n * @kind problem\n * @problem.severity warning\n * @id {module}/{id}\n */\n\nimport {module}\n\nfrom File f\nselect f, \"Hello, world!\"\n"
        ),
        None => format!(
            "/**\n * @name {name}\n * @description Describe what this query finds.\n * @id {id}\n */\n\nselect \"Hello, world!\"\n"
        ),
    }
}

/// The file name a typed query name becomes: trimmed, with a `.ql` the
/// user typed dropped and put back. Empty names, names with a path in them
/// and names that are only dots are refused.
pub fn query_file_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let stem = name.strip_suffix(".ql").unwrap_or(name).trim_end();
    if stem.is_empty() {
        return Err(String::from("A query name cannot be empty"));
    }
    if stem.contains(['/', '\\', '\0']) || stem.chars().all(|c| c == '.') {
        return Err(format!("'{name}' is not a file name"));
    }
    Ok(format!("{stem}.ql"))
}

/// Write a starter query called `name` into `dir` (#578) and return its
/// path. An existing file is never overwritten. When `lang` is known and
/// neither `dir` nor a folder above it (up to `root`) holds a pack file, a
/// minimal `qlpack.yml` depending on that language's library goes beside
/// it, so the query compiles.
pub fn scaffold_query(
    root: &Path,
    dir: &Path,
    name: &str,
    lang: Option<&str>,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind, Write};
    let file = query_file_name(name).map_err(|e| Error::new(ErrorKind::InvalidInput, e))?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(&file);
    let stem = file.strip_suffix(".ql").unwrap_or(&file);
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| match e.kind() {
            ErrorKind::AlreadyExists => {
                Error::new(ErrorKind::AlreadyExists, format!("{file} already exists"))
            }
            _ => e,
        })?;
    out.write_all(query_template(lang, stem).as_bytes())?;
    let module = lang.and_then(language_module);
    let in_pack = dir
        .ancestors()
        .take_while(|d| d.starts_with(root) || *d == dir)
        .any(|d| d.join("qlpack.yml").is_file() || d.join("codeql-pack.yml").is_file());
    if let (Some(module), false) = (module, in_pack) {
        let scope = dir
            .file_name()
            .map(|n| kebab(&n.to_string_lossy()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| String::from("local"));
        std::fs::write(
            dir.join("qlpack.yml"),
            format!(
                "name: {scope}/queries\nversion: 0.0.1\ndependencies:\n  codeql/{module}-all: \"*\"\n"
            ),
        )?;
    }
    Ok(path)
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RunStatus {
    Running,
    Succeeded,
    /// Why, in what `codeql` said.
    Failed(String),
    /// Stopped by the user before it finished (#578).
    Cancelled,
}

/// A query waiting in a batch run (#578): run from its file on the
/// current database, or with `source` and `database` set, from that text
/// on that database ("Run Query on Multiple Databases").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRun {
    pub query: PathBuf,
    pub source: Option<String>,
    pub database: Option<PathBuf>,
}

/// One query history entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub query: PathBuf,
    pub database: String,
    /// The database's folder, so "Delete Unused Databases" can tell which
    /// ones a run refers to after a rename. History saved before this
    /// field existed reads as `None` and falls back to the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_path: Option<PathBuf>,
    /// Seconds since the Unix epoch.
    pub started: u64,
    pub seconds: u64,
    pub status: RunStatus,
    /// The SARIF or CSV the run wrote. Each run writes into its own
    /// folder, so this also identifies the entry.
    pub output: PathBuf,
    /// The label the user gave it, shown in place of the default one.
    /// History saved before renaming existed reads as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How many results a successful run produced: table rows, or SARIF
    /// results (#578). `None` while running, after a failure, and for
    /// history saved before counts were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<u64>,
}

/// How many results `output` holds: the rows of a CSV table (its header
/// aside), or the results of every run of a SARIF log. `None` when it
/// cannot be read.
pub fn count_results(output: &Path) -> Option<u64> {
    let is_csv = output
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("csv"));
    if is_csv {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_path(output)
            .ok()?;
        let mut n = 0u64;
        for record in reader.records() {
            record.ok()?;
            n += 1;
        }
        return Some(n);
    }
    let log: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(output).ok()?).ok()?;
    Some(
        log.get("runs")?
            .as_array()?
            .iter()
            .filter_map(|r| r.get("results").and_then(|v| v.as_array()))
            .map(|r| r.len() as u64)
            .sum(),
    )
}

impl HistoryEntry {
    /// The query's file name.
    pub fn query_name(&self) -> String {
        self.query
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Whether this run was on the database at `path` that goes, or went,
    /// by one of `names`: by canonical path when the entry recorded one,
    /// else by name.
    pub fn refers_to(&self, path: &Path, names: &[&str]) -> bool {
        match &self.database_path {
            Some(p) => {
                let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
                canon(p) == canon(path)
            }
            None => names.contains(&self.database.as_str()),
        }
    }

    /// What the entry is called: the user's label, else the query's file
    /// name. The name sort orders by this.
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.query_name())
    }

    /// A history line: "✓ query.ql · db · 12s", "✗ … · failed: why",
    /// "… running". A renamed entry keeps only the status mark before the
    /// user's label.
    pub fn label(&self) -> String {
        if let Some(custom) = &self.name {
            let mark = match self.status {
                RunStatus::Succeeded => '\u{2713}',
                RunStatus::Failed(_) => '\u{2717}',
                RunStatus::Cancelled => '\u{2298}',
                RunStatus::Running => '\u{2026}',
            };
            return format!("{mark} {custom}");
        }
        let name = self.query_name();
        match &self.status {
            RunStatus::Succeeded => {
                let count = match self.results {
                    Some(1) => String::from(" \u{b7} 1 result"),
                    Some(n) => format!(" \u{b7} {n} results"),
                    None => String::new(),
                };
                format!(
                    "\u{2713} {name} \u{b7} {} \u{b7} {}s{count}",
                    self.database, self.seconds
                )
            }
            RunStatus::Failed(why) => format!(
                "\u{2717} {name} \u{b7} {} \u{b7} failed: {why}",
                self.database
            ),
            RunStatus::Running => {
                format!("\u{2026} {name} \u{b7} {} \u{b7} running", self.database)
            }
            RunStatus::Cancelled => {
                format!("\u{2298} {name} \u{b7} {} \u{b7} cancelled", self.database)
            }
        }
    }
}

/// The orders the Query History section sorts by, VS Code's three plus
/// grouping runs by how they ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HistSort {
    /// Newest first.
    #[default]
    Date,
    Name,
    /// Succeeded, then failed, cancelled, running; newest first within each.
    Status,
    /// Most results first; runs without a count last, newest first among
    /// equals.
    Count,
}

impl HistSort {
    /// The next order in the cycle the side bar's sort row steps through.
    pub fn next(self) -> HistSort {
        match self {
            HistSort::Date => HistSort::Name,
            HistSort::Name => HistSort::Status,
            HistSort::Status => HistSort::Count,
            HistSort::Count => HistSort::Date,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HistSort::Date => "date",
            HistSort::Name => "name",
            HistSort::Status => "status",
            HistSort::Count => "result count",
        }
    }
}

/// The query history in the order the user chose (newest first unless
/// they sorted it otherwise), capped at [`History::CAP`] entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct History {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
    #[serde(default)]
    pub sort_by: HistSort,
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

    /// Pin older entries that only recorded the database's `name` to its
    /// `path`, so renaming the database does not orphan them (#578).
    /// Returns whether anything changed.
    pub fn adopt_legacy(&mut self, name: &str, path: &Path) -> bool {
        let mut changed = false;
        for e in &mut self.entries {
            if e.database_path.is_none() && e.database == name {
                e.database_path = Some(path.to_path_buf());
                changed = true;
            }
        }
        changed
    }

    /// Record a new run, dropping the oldest past the cap. It goes at the
    /// top, then takes its place in the chosen order.
    pub fn push(&mut self, entry: HistoryEntry) {
        self.entries.insert(0, entry);
        if self.entries.len() > Self::CAP {
            // The oldest run, wherever the order put it; among equals the
            // one furthest down goes, never the new entry at the top.
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .rev()
                .min_by_key(|(_, e)| e.started)
                .map_or(0, |(i, _)| i);
            self.entries.remove(oldest);
        }
        if self.sort_by != HistSort::Date {
            self.sort(self.sort_by);
        }
    }

    /// Forget entry `index` (its results are the caller's business).
    pub fn remove(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entries.remove(index);
        }
    }

    /// Give entry `index` a label of its own. A blank label is refused.
    pub fn rename(&mut self, index: usize, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(String::from("A query history label cannot be empty"));
        }
        let entry = self
            .entries
            .get_mut(index)
            .ok_or_else(|| String::from("No such query history entry"))?;
        entry.name = Some(name.to_string());
        Ok(())
    }

    /// Reorder the entries and remember the order.
    pub fn sort(&mut self, by: HistSort) {
        use std::cmp::Reverse;
        match by {
            HistSort::Date => self.entries.sort_by_key(|e| Reverse(e.started)),
            HistSort::Name => self
                .entries
                .sort_by_key(|e| (e.display_name().to_lowercase(), Reverse(e.started))),
            HistSort::Status => self.entries.sort_by_key(|e| {
                let rank = match e.status {
                    RunStatus::Succeeded => 0,
                    RunStatus::Failed(_) => 1,
                    RunStatus::Cancelled => 2,
                    RunStatus::Running => 3,
                };
                (rank, Reverse(e.started))
            }),
            HistSort::Count => self
                .entries
                .sort_by_key(|e| (e.results.is_none(), Reverse(e.results), Reverse(e.started))),
        }
        self.sort_by = by;
    }

    /// The index of the entry whose run wrote `output`.
    pub fn position(&self, output: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.output == output)
    }

    /// The run "Compare Results" sets entry `index` against: the latest
    /// earlier successful run of the same query with the same kind of
    /// results, on any database.
    pub fn compare_partner(&self, index: usize) -> Option<usize> {
        let this = self.entries.get(index)?;
        self.entries
            .iter()
            .enumerate()
            .filter(|&(i, e)| {
                i != index
                    && e.query == this.query
                    && e.status == RunStatus::Succeeded
                    && e.output.extension() == this.output.extension()
                    && e.started <= this.started
            })
            .max_by_key(|(i, e)| (e.started, std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
    }

    /// The run "Compare Performance" sets entry `index` against: the latest
    /// earlier successful run of the same query, whatever kind of results
    /// either wrote, on any database.
    pub fn perf_partner(&self, index: usize) -> Option<usize> {
        let this = self.entries.get(index)?;
        self.entries
            .iter()
            .enumerate()
            .filter(|&(i, e)| {
                i != index
                    && e.query == this.query
                    && e.status == RunStatus::Succeeded
                    && e.started <= this.started
            })
            .max_by_key(|(i, e)| (e.started, std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
    }

    /// The index of the most recent run, whatever the order.
    pub fn newest(&self) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .max_by_key(|(i, e)| (e.started, std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
    }

    /// The index of the most recent run that succeeded, whatever the order.
    pub fn newest_success(&self) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.status == RunStatus::Succeeded)
            .max_by_key(|(i, e)| (e.started, std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
    }
}

/// The file name "CodeQL: Export Results" suggests for the results
/// `output` of `query`: `<query-stem>-results.<csv|sarif>`.
pub fn export_file_name(query: &Path, output: &Path) -> String {
    let stem = query
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = output
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("csv"));
    format!("{stem}-results.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_cells_lead_to_their_entities_code() {
        // The real `bqrs decode --format=json --entities=url,string
        // --result-set=#select` shape (2.27.1).
        let json = r#"{"columns":[{"name":"f","kind":"Entity"},{"kind":"String"}],
          "tuples":[[{"label":"Function run","url":{"uri":"file:///w/my%20app.py","startLine":3,"startColumn":1,"endLine":3,"endColumn":13}},"run"],
                    [{"label":"Function go"},7]]}"#;
        let rows = parse_row_locations(json);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells, ["Function run", "run"]);
        assert_eq!(
            rows[0].locs[0],
            Some(CellLoc {
                path: PathBuf::from("/w/my app.py"),
                line: 3,
                column: 1
            })
        );
        assert_eq!(rows[0].locs[1], None, "a string has no location");
        assert_eq!(rows[1].cells, ["Function go", "7"]);
        assert_eq!(rows[1].locs, [None, None], "an entity without a url");
        let found = row_locations(&rows, &["Function run".to_string(), "run".to_string()]);
        assert_eq!(found.map(|l| l[0].as_ref().map(|c| c.line)), Some(Some(3)));
        assert!(row_locations(&rows, &["nope".to_string()]).is_none());
        assert_eq!(
            locations_path(Path::new("/r/results-calls.csv")),
            Path::new("/r/results-calls.locations.json")
        );
        assert_eq!(parse_row_locations("oops"), []);
    }

    #[test]
    fn a_qlref_resolves_to_the_query_the_cli_names() {
        // The real `codeql resolve qlref` answer (2.27.1).
        let json = "{\n  \"resolvedPath\" : \"/w/src/Alert.ql\",\n  \"resolvedPostprocessingPaths\" : [ ]\n}\n";
        assert_eq!(parse_qlref(json), Some(PathBuf::from("/w/src/Alert.ql")));
        assert_eq!(parse_qlref("{}"), None);
        assert_eq!(
            qlref_args(Path::new("/w/test/A.qlref")),
            ["resolve", "qlref", "/w/test/A.qlref"]
        );
    }

    #[test]
    fn an_alert_runs_kept_results_are_found_and_interpreted_as_csv() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        // Where the CLI keeps them: results/<pack scope>/<pack>/<query>.bqrs.
        let kept = db.join("results/me/q/Alert.bqrs");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "").unwrap();
        std::fs::write(db.join("results/me/q/Other.bqrs"), "").unwrap();
        assert_eq!(kept_bqrs(&db, Path::new("/w/q/Alert.ql")), Some(kept));
        assert_eq!(kept_bqrs(&db, Path::new("/w/q/Gone.ql")), None);
        assert_eq!(
            interpret_csv_args(&db, Path::new("/w/q/Alert.ql"), Path::new("/r/alerts.csv")),
            [
                "database",
                "interpret-results",
                "--format=csv",
                "--output=/r/alerts.csv",
                &db.display().to_string(),
                "/w/q/Alert.ql"
            ]
        );
    }

    #[test]
    fn each_result_set_gets_its_own_csv() {
        // The real `codeql bqrs info --format=json` shape (2.27.1).
        let sets = parse_result_sets(
            r##"{"result-sets":[{"name":"calls","rows":3,"columns":[]},{"name":"#select","rows":1,"columns":[]}],"compatible-query-kinds":["Table"]}"##,
        );
        assert_eq!(sets, [("calls".to_string(), 3), ("#select".to_string(), 1)]);
        assert_eq!(main_result_set(&sets), Some("#select"));
        assert_eq!(
            main_result_set(&sets[..1]),
            Some("calls"),
            "no #select: the first"
        );
        assert!(parse_result_sets("oops").is_empty());
        let out = Path::new("/r/1-q/results.csv");
        assert_eq!(result_set_file(out, "#select", true), out);
        assert_eq!(
            result_set_file(out, "calls/x", false),
            Path::new("/r/1-q/results-calls_x.csv")
        );
        assert_eq!(
            decode_set_args(Path::new("/r/b.bqrs"), out, "calls"),
            [
                "bqrs",
                "decode",
                "--format=csv",
                "--result-set=calls",
                "--output=/r/1-q/results.csv",
                "/r/b.bqrs"
            ]
        );
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("results.csv");
        for f in [
            "results.csv",
            "results-zeta.csv",
            "results-alpha.csv",
            "results.bqrs",
        ] {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        let names: Vec<String> = result_set_files(&main)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["#select", "alpha", "zeta"]);
    }

    #[test]
    fn queries_in_takes_ql_files_and_walks_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for f in [
            "a/One.ql",
            "a/b/Two.ql",
            "a/lib.qll",
            "a/.hidden/Three.ql",
            "Top.ql",
            "x.txt",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "select 1").unwrap();
        }
        let got = queries_in(&[
            root.join("a"),
            root.join("Top.ql"),
            root.join("a/One.ql"),
            root.join("x.txt"),
        ]);
        assert_eq!(
            got,
            [
                root.join("Top.ql"),
                root.join("a/One.ql"),
                root.join("a/b/Two.ql")
            ]
        );
    }

    #[test]
    fn a_failure_reads_the_reason_and_the_suggested_fix_not_the_progress() {
        let stderr = "Compiling query plan for /w/q.ql.\n\
            ERROR: could not resolve module cpp (/w/q.ql:1,8-11)\n\
            Failed [1/1] /w/q.ql.\n\
            A fatal error occurred: Could not compile the query.\n\
            Consider running `codeql pack install` in /w to fetch its dependencies.\n";
        assert_eq!(
            failure_reason(stderr),
            "A fatal error occurred: Could not compile the query. · \
             ERROR: could not resolve module cpp (/w/q.ql:1,8-11) · \
             Consider running `codeql pack install` in /w to fetch its dependencies."
        );
        // A fix given inside the fatal line is not repeated.
        assert_eq!(
            failure_reason(
                "A fatal error occurred: The database is too old. Run `codeql database upgrade /db`.\n"
            ),
            "A fatal error occurred: The database is too old. Run `codeql database upgrade /db`."
        );
        // Nothing recognisable: the last line, where a tool ends on its reason.
        assert_eq!(
            failure_reason("starting\nno such file: x\n"),
            "no such file: x"
        );
        assert_eq!(failure_reason("\n"), "codeql failed");
    }

    #[test]
    fn cache_jobs_run_database_cleanup_in_their_mode() {
        let db = Path::new("/dbs/app");
        assert_eq!(DbJob::Upgrade.args(db), upgrade_args(db));
        for (job, mode) in [
            (DbJob::ClearCache, "clear"),
            (DbJob::TrimCache, "trim"),
            (DbJob::TrimCacheToOverlay, "overlay"),
        ] {
            assert_eq!(
                job.args(db),
                vec![
                    String::from("database"),
                    String::from("cleanup"),
                    format!("--cache-cleanup={mode}"),
                    String::from("/dbs/app"),
                ]
            );
            assert_eq!(job.noun(), "cache cleanup");
        }
        assert_eq!(
            DbJob::TrimCacheToOverlay.finished("app", Ok(())),
            "Trimmed the cache of CodeQL database app to its overlay base"
        );
        assert_eq!(
            DbJob::TrimCache.finished("app", Err(String::from("locked"))),
            "Could not clean up the cache of CodeQL database app: locked"
        );
    }

    #[test]
    fn pack_commands_install_the_packs_dependencies_or_download_the_named_packs() {
        assert_eq!(
            pack_install_args(Path::new("/w/pack")),
            vec!["pack", "install", "/w/pack"]
        );
        let packs = parse_pack_list(" codeql/java-queries, codeql/python-all@1.0.0 ,,");
        assert_eq!(
            packs,
            vec!["codeql/java-queries", "codeql/python-all@1.0.0"]
        );
        assert_eq!(
            pack_download_args(&packs),
            vec![
                "pack",
                "download",
                "codeql/java-queries",
                "codeql/python-all@1.0.0"
            ]
        );
        assert!(parse_pack_list("  , ").is_empty());
    }

    #[test]
    fn a_published_pack_reference_names_the_pack_to_download() {
        assert_eq!(
            published_pack("codeql/python-queries"),
            Some("codeql/python-queries")
        );
        assert_eq!(published_pack(" acme/q@1.2.0 "), Some("acme/q@1.2.0"));
        assert_eq!(
            published_pack("codeql/python-queries:Security/CWE-078"),
            Some("codeql/python-queries")
        );
        assert_eq!(published_pack("acme/q@~1.0:x.ql"), Some("acme/q@~1.0"));
        for bad in [
            "",
            "python-queries",
            "/q",
            "acme/",
            "a b/c",
            "../x",
            "Acme/q",
        ] {
            assert_eq!(published_pack(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_suite_holding_table_queries_is_found_out() {
        let read = |p: &Path| -> Option<String> {
            Some(match p.file_name()?.to_str()? {
                "Alert.ql" => String::from("/** @kind problem */ select 1"),
                _ => String::from("select 1"),
            })
        };
        assert_eq!(
            table_queries_in_suite(r#"["/q/Alert.ql","/q/Table.ql"]"#, read),
            Ok(vec![String::from("Table.ql")])
        );
        assert_eq!(
            table_queries_in_suite(r#"["/q/Alert.ql"]"#, read),
            Ok(vec![])
        );
        assert!(table_queries_in_suite("not json", read).is_err());
        assert_eq!(
            resolve_suite_args(Path::new("/s.qls")),
            ["resolve", "queries", "--format=json", "/s.qls"]
        );
    }

    #[test]
    fn a_quick_query_imports_its_languages_library() {
        let (pack, query) = quick_query("python");
        assert!(pack.contains("name: croft/quick-query-python\n"));
        assert!(pack.contains("  codeql/python-all: \"*\"\n"));
        assert!(query.contains("\nimport python\n"));
        assert_eq!(output_for(&query), Output::Table);
    }

    #[test]
    fn comparing_tables_lists_the_rows_each_run_has_alone() {
        let old = "name,line\na,1\nb,2\nb,2\n";
        let new = "name,line\nb,2\nc,3\n";
        assert_eq!(
            compare_tables(old, new, "old", "new").unwrap(),
            "run,name,line\nold,a,1\nold,b,2\nnew,c,3\n"
        );
        assert_eq!(
            compare_tables(old, old, "old", "new").unwrap(),
            "run,name,line\n"
        );
        assert!(compare_tables(old, "other\nx\n", "old", "new").is_err());
    }

    #[test]
    fn a_runs_compare_partner_is_the_latest_earlier_success_of_the_same_query() {
        let entry = |query: &str, started: u64, status: RunStatus, out: &str| HistoryEntry {
            query: PathBuf::from(query),
            database: String::from("db"),
            database_path: None,
            started,
            seconds: 1,
            status,
            output: PathBuf::from(out),
            name: None,
            results: None,
        };
        let history = History {
            entries: vec![
                entry("q.ql", 4, RunStatus::Succeeded, "/r/4/results.csv"),
                entry(
                    "q.ql",
                    3,
                    RunStatus::Failed(String::from("x")),
                    "/r/3/results.csv",
                ),
                entry("other.ql", 3, RunStatus::Succeeded, "/r/3o/results.csv"),
                entry("q.ql", 2, RunStatus::Succeeded, "/r/2/results.sarif"),
                entry("q.ql", 1, RunStatus::Succeeded, "/r/1/results.csv"),
            ],
            ..Default::default()
        };
        assert_eq!(history.compare_partner(0), Some(4));
        assert_eq!(history.compare_partner(4), None);
        assert_eq!(history.compare_partner(2), None);
        // Performance compares any kind of results: the SARIF run counts.
        assert_eq!(history.perf_partner(0), Some(3));
        assert_eq!(history.perf_partner(3), Some(4));
        assert_eq!(history.perf_partner(4), None);
        assert_eq!(history.perf_partner(2), None);
        assert_eq!(history.perf_partner(9), None);
    }

    #[test]
    fn export_takes_the_newest_success_under_the_querys_name() {
        let entry = |started: u64, status: RunStatus| HistoryEntry {
            query: PathBuf::from("/w/q.ql"),
            database: String::from("db"),
            database_path: None,
            started,
            seconds: 1,
            status,
            output: PathBuf::from("/r/results.csv"),
            name: None,
            results: None,
        };
        let mut history = History {
            entries: vec![
                entry(5, RunStatus::Failed(String::from("x"))),
                entry(3, RunStatus::Succeeded),
                entry(4, RunStatus::Running),
                entry(1, RunStatus::Succeeded),
            ],
            ..Default::default()
        };
        assert_eq!(history.newest_success(), Some(1));
        history.entries.remove(1);
        history.entries.remove(2);
        assert_eq!(history.newest_success(), None);
        assert_eq!(
            export_file_name(Path::new("/w/q.ql"), Path::new("/r/results.sarif")),
            "q-results.sarif"
        );
        assert_eq!(
            export_file_name(Path::new("/w/s.qls"), Path::new("/r/results.csv")),
            "s-results.csv"
        );
    }

    #[test]
    fn version_information_names_croft_the_cli_and_the_platform() {
        let text = version_information("0.1.9", &Ok(String::from("2.19.3\n")));
        let platform = format!(
            "Platform: {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        assert_eq!(
            text,
            format!("croft version: 0.1.9\nCodeQL CLI version: 2.19.3\n{platform}")
        );
        let missing = version_information("0.1.9", &Err(String::from("not on PATH")));
        assert!(missing.contains("CodeQL CLI version: unavailable (not on PATH)"));
        assert_eq!(version_args(), ["version", "--format=terse"]);
    }

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
    fn a_suite_always_reads_as_sarif() {
        let suite = "- queries: .\n- include:\n    kind: table\n";
        assert!(is_suite(Path::new("/w/s.qls")));
        assert!(!is_suite(Path::new("/w/q.ql")));
        assert_eq!(output_of(Path::new("/w/s.qls"), suite), Output::Sarif);
        assert_eq!(output_of(Path::new("/w/q.ql"), "select 1"), Output::Table);
        assert_eq!(output_of(Path::new("/w/q.ql"), PROBLEM), Output::Sarif);
    }

    #[test]
    fn the_newest_query_log_is_the_latest_log_file() {
        let t = |s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s);
        let p = |s: &str| PathBuf::from(format!("/out/logs/{s}"));
        assert_eq!(newest_query_log(Vec::new()), None);
        assert_eq!(newest_query_log(vec![(p("notes.txt"), t(9))]), None);
        assert_eq!(
            newest_query_log(vec![
                (p("execute-b.log"), t(2)),
                (p("execute-a.log"), t(3)),
                (p("later.txt"), t(4)),
            ]),
            Some(p("execute-a.log"))
        );
        assert_eq!(
            newest_query_log(vec![(p("execute-b.log"), t(3)), (p("execute-a.log"), t(3))]),
            Some(p("execute-b.log")),
            "a tie goes to the greater name"
        );
        assert_eq!(
            query_log_dir(Path::new("/out/r.sarif")),
            Path::new("/out/logs")
        );
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
                "--evaluator-log=/out/evaluator-log.jsonl",
                "--logdir=/out/logs",
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
                "--evaluator-log=/out/evaluator-log.jsonl",
                "--logdir=/out/logs",
                "/w/q.ql"
            ]
        );
        assert_eq!(
            log_summary_args(
                &evaluator_log(Path::new("/out/results.csv")),
                &evaluator_log_summary(Path::new("/out/results.csv"))
            ),
            vec![
                "generate",
                "log-summary",
                "--format=text",
                "/out/evaluator-log.jsonl",
                "/out/evaluator-log.summary.txt"
            ]
        );
        assert_eq!(
            log_predicates_args(
                &evaluator_log(Path::new("/out/results.csv")),
                &evaluator_log_predicates(Path::new("/out/results.csv"))
            ),
            vec![
                "generate",
                "log-summary",
                "--format=predicates",
                "/out/evaluator-log.jsonl",
                "/out/evaluator-log.predicates.jsonl"
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
        assert_eq!(upgrade_args(db), vec!["database", "upgrade", "/dbs/app"]);
        assert_eq!(
            query_help_args(q, Path::new("/cache/help/q.md")),
            vec![
                "generate",
                "query-help",
                "--format=markdown",
                "--output=/cache/help/q.md",
                "/w/q.ql"
            ]
        );
    }

    #[test]
    fn a_run_refers_to_its_database_by_path_else_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("app");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::create_dir_all(tmp.path().join("x")).unwrap();
        let mut e = entry(RunStatus::Succeeded);
        assert!(e.refers_to(&db, &["app"]), "an old entry goes by name");
        assert!(!e.refers_to(&db, &["renamed"]));
        assert!(
            e.refers_to(&db, &["renamed", "app"]),
            "or by a name the database had before a rename"
        );
        e.database_path = Some(tmp.path().join("x/../app"));
        assert!(e.refers_to(&db, &["renamed"]), "a path survives a rename");
        assert!(!e.refers_to(&tmp.path().join("other"), &["app"]));
    }

    /// #578: runs that only named their database get its path before a
    /// rename, and runs that already have a path are left alone.
    #[test]
    fn legacy_runs_are_pinned_to_a_database_path() {
        let mut history = History::default();
        let mut pinned = entry(RunStatus::Succeeded);
        pinned.database_path = Some(PathBuf::from("/dbs/elsewhere"));
        history.entries = vec![entry(RunStatus::Succeeded), pinned.clone()];
        let name = history.entries[0].database.clone();
        assert!(history.adopt_legacy(&name, Path::new("/dbs/app")));
        assert_eq!(
            history.entries[0].database_path.as_deref(),
            Some(Path::new("/dbs/app"))
        );
        assert_eq!(history.entries[1], pinned);
        assert!(
            !history.adopt_legacy(&name, Path::new("/dbs/app")),
            "idempotent"
        );
    }

    #[test]
    fn pack_files_give_a_name_and_a_language() {
        let text = "name: \"acme/py-queries\"\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\n";
        assert_eq!(pack_name(text).as_deref(), Some("acme/py-queries"));
        assert_eq!(pack_language(text).as_deref(), Some("python"));
        let lib = "name: acme/lib\nextractor: cpp\nlibraryPathDependencies: codeql/go-all\n";
        assert_eq!(pack_language(lib).as_deref(), Some("cpp"), "extractor wins");
        // An indented `name:` belongs to something else.
        assert_eq!(pack_name("deps:\n  name: x\n"), None);
        assert_eq!(pack_language("name: x\n"), None);
        assert_eq!(
            pack_language("dependencies:\n  - codeql/javascript-queries\n"),
            None,
            "only the -all library names a language"
        );
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn discover_groups_queries_under_their_nearest_pack() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "outer/qlpack.yml", "name: acme/outer\nextractor: go\n");
        write(r, "outer/a.ql", "select 1");
        write(
            r,
            "outer/inner/codeql-pack.yml",
            "name: acme/inner\ndependencies:\n  codeql/rust-all: '*'\n",
        );
        write(r, "outer/inner/deep/b.ql", "select 1");
        write(r, "unnamed/qlpack.yml", "version: 1.0.0\n");
        write(r, "unnamed/c.ql", "select 1");
        write(r, "loose.ql", "select 1");
        write(r, "scratch/d.ql", "select 1");
        write(r, "lib.qll", "predicate p() { any() }");
        // Noise folders are never searched.
        write(r, "node_modules/pkg/e.ql", "select 1");
        write(r, "target/f.ql", "select 1");
        let packs = discover(r);
        let summary: Vec<(&str, Option<&str>, Vec<String>)> = packs
            .iter()
            .map(|p| {
                (
                    p.name.as_str(),
                    p.language.as_deref(),
                    p.queries
                        .iter()
                        .map(|q| q.strip_prefix(&p.dir).unwrap().display().to_string())
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("acme/inner", Some("rust"), vec![String::from("deep/b.ql")]),
                ("acme/outer", Some("go"), vec![String::from("a.ql")]),
                ("unnamed", None, vec![String::from("c.ql")]),
                (
                    NO_PACK,
                    None,
                    vec![String::from("loose.ql"), String::from("scratch/d.ql")]
                ),
            ]
        );
        assert_eq!(packs[3].dir, r);
        assert!(discover(&r.join("missing")).is_empty());
    }

    fn entry(status: RunStatus) -> HistoryEntry {
        HistoryEntry {
            query: PathBuf::from("/w/sql.ql"),
            database: String::from("app"),
            database_path: None,
            started: 1,
            seconds: 12,
            status,
            output: PathBuf::from("/out/r.sarif"),
            name: None,
            results: None,
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

    fn run(query: &str, started: u64, status: RunStatus) -> HistoryEntry {
        HistoryEntry {
            query: PathBuf::from("/w").join(query),
            started,
            status,
            output: PathBuf::from(format!("/out/{started}/r.sarif")),
            ..entry(RunStatus::Succeeded)
        }
    }

    fn started(h: &History) -> Vec<u64> {
        h.entries.iter().map(|e| e.started).collect()
    }

    #[test]
    fn a_history_entry_is_removed_by_index() {
        let mut h = History::default();
        for i in 1..=3 {
            h.push(run("q.ql", i, RunStatus::Succeeded));
        }
        h.remove(1);
        assert_eq!(started(&h), [3, 1]);
        h.remove(7);
        assert_eq!(started(&h), [3, 1], "out of range is a no-op");
        assert_eq!(h.position(Path::new("/out/1/r.sarif")), Some(1));
        assert_eq!(h.position(Path::new("/out/2/r.sarif")), None);
    }

    #[test]
    fn a_renamed_entry_shows_its_label_but_never_a_blank_one() {
        let mut h = History::default();
        h.push(entry(RunStatus::Succeeded));
        assert_eq!(h.rename(0, "  sqli on main  "), Ok(()));
        assert_eq!(h.entries[0].label(), "\u{2713} sqli on main");
        assert_eq!(h.entries[0].display_name(), "sqli on main");
        assert!(h.rename(0, "  ").is_err());
        assert_eq!(h.entries[0].name.as_deref(), Some("sqli on main"));
        assert!(h.rename(4, "x").is_err());
        h.entries[0].status = RunStatus::Failed(String::from("x"));
        assert_eq!(h.entries[0].label(), "\u{2717} sqli on main");
    }

    #[test]
    fn history_saved_before_labels_and_sorting_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(
            &path,
            r#"{"entries":[{"query":"/w/sql.ql","database":"app","started":1,"seconds":12,"status":"Succeeded","output":"/out/r.sarif"}]}"#,
        )
        .unwrap();
        let h = History::load(&path);
        assert_eq!(h.entries, vec![entry(RunStatus::Succeeded)]);
        assert_eq!(h.sort_by, HistSort::Date);
        // An unrenamed entry saves without the field.
        h.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("\"name\""));
    }

    #[test]
    fn history_sorts_by_date_name_or_status_and_keeps_the_order() {
        let mut h = History::default();
        h.push(run("b.ql", 1, RunStatus::Failed(String::from("x"))));
        h.push(run("A.ql", 2, RunStatus::Succeeded));
        h.push(run("c.ql", 3, RunStatus::Running));
        h.push(run("b.ql", 4, RunStatus::Succeeded));
        assert_eq!(started(&h), [4, 3, 2, 1], "newest first by default");
        h.sort(HistSort::Name);
        assert_eq!(started(&h), [2, 4, 1, 3], "case-insensitive, newest first");
        h.rename(3, "0 first").unwrap();
        h.sort(HistSort::Name);
        assert_eq!(started(&h), [3, 2, 4, 1], "a label sorts as its name");
        h.sort(HistSort::Status);
        assert_eq!(started(&h), [4, 2, 1, 3]);
        assert_eq!(h.newest(), Some(0));
        // A new run takes its place in the chosen order.
        h.push(run("z.ql", 5, RunStatus::Failed(String::from("y"))));
        assert_eq!(started(&h), [4, 2, 5, 1, 3]);
        assert_eq!(h.newest(), Some(2));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        h.save(&path).unwrap();
        assert_eq!(History::load(&path).sort_by, HistSort::Status, "saved");
        h.sort(HistSort::Date);
        assert_eq!(started(&h), [5, 4, 3, 2, 1]);
        assert_eq!(HistSort::Status.next(), HistSort::Count);
        assert_eq!(HistSort::Count.next(), HistSort::Date);
    }

    #[test]
    fn history_sorts_by_result_count_and_labels_show_it() {
        let mut h = History::default();
        let counted = |q: &str, t: u64, n: Option<u64>| HistoryEntry {
            results: n,
            ..run(q, t, RunStatus::Succeeded)
        };
        h.push(counted("a.ql", 1, Some(3)));
        h.push(counted("b.ql", 2, None));
        h.push(counted("c.ql", 3, Some(40)));
        h.push(counted("d.ql", 4, Some(3)));
        h.sort(HistSort::Count);
        assert_eq!(started(&h), [3, 4, 1, 2], "most first, uncounted last");
        assert!(h.entries[0].label().ends_with("12s \u{b7} 40 results"));
        assert!(h.entries[3].label().ends_with("12s"), "no count, no suffix");
        assert!(
            counted("e.ql", 5, Some(1))
                .label()
                .ends_with("\u{b7} 1 result")
        );
    }

    #[test]
    fn results_are_counted_from_a_table_or_a_sarif_log() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("results.csv");
        std::fs::write(&csv, "col0,col1\n1,\"two\nlines\"\n3,x\n").unwrap();
        assert_eq!(count_results(&csv), Some(2), "a quoted newline is one row");
        std::fs::write(&csv, "col0\n").unwrap();
        assert_eq!(count_results(&csv), Some(0));
        let sarif = dir.path().join("results.sarif");
        std::fs::write(
            &sarif,
            r#"{"version":"2.1.0","runs":[{"results":[{},{}]},{"results":[{}]},{}]}"#,
        )
        .unwrap();
        assert_eq!(count_results(&sarif), Some(3));
        assert_eq!(count_results(&dir.path().join("missing.sarif")), None);
    }

    #[test]
    fn the_cap_drops_the_oldest_run_in_any_order() {
        let mut h = History::default();
        for i in 0..History::CAP as u64 {
            h.push(run("q.ql", i + 1, RunStatus::Succeeded));
        }
        h.sort(HistSort::Name);
        h.push(run("q.ql", 1000, RunStatus::Succeeded));
        assert_eq!(h.entries.len(), History::CAP);
        assert!(h.entries.iter().all(|e| e.started != 1), "the oldest went");
        assert_eq!(h.entries[0].started, 1000);
    }

    #[test]
    fn a_template_imports_each_languages_library() {
        for (lang, module) in [
            ("python", "python"),
            ("cpp", "cpp"),
            ("java", "java"),
            ("kotlin", "java"),
            ("javascript", "javascript"),
            ("typescript", "javascript"),
            ("csharp", "csharp"),
            ("go", "go"),
            ("ruby", "ruby"),
            ("rust", "rust"),
            ("swift", "swift"),
            ("actions", "actions"),
        ] {
            let q = query_template(Some(lang), "Find SQL_injection");
            assert!(q.starts_with("/**\n * @name Find SQL_injection\n"), "{q}");
            assert!(q.contains(&format!("\nimport {module}\n")), "{lang}: {q}");
            assert!(
                q.contains(&format!(" * @id {module}/find-sql-injection\n")),
                "{q}"
            );
            assert!(q.contains(" * @problem.severity warning\n"), "{q}");
            assert!(q.contains("\nfrom File f\nselect f, "), "{q}");
            assert_eq!(query_kind(&q).as_deref(), Some("problem"));
            assert_eq!(output_for(&q), Output::Sarif);
        }
    }

    #[test]
    fn a_template_in_no_known_language_imports_nothing() {
        for lang in [None, Some("cobol")] {
            let q = query_template(lang, "hello");
            assert!(!q.contains("import"), "{q}");
            assert!(q.contains(" * @id hello\n"), "{q}");
            assert!(q.ends_with("select \"Hello, world!\"\n"), "{q}");
            assert_eq!(query_kind(&q), None);
        }
    }

    #[test]
    fn query_names_become_file_names_or_are_refused() {
        assert_eq!(query_file_name(" sqli ").as_deref(), Ok("sqli.ql"));
        assert_eq!(query_file_name("sqli.ql").as_deref(), Ok("sqli.ql"));
        assert_eq!(query_file_name("a.b").as_deref(), Ok("a.b.ql"));
        for bad in ["", "  ", ".ql", "a/b", "..\\x", "..", "."] {
            assert!(query_file_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn scaffolding_writes_the_query_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "pack/qlpack.yml",
            "name: acme/py\nextractor: python\n",
        );
        let dir = root.join("pack/sub");
        let path = scaffold_query(root, &dir, "sqli.ql", Some("python")).unwrap();
        assert_eq!(path, dir.join("sqli.ql"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, query_template(Some("python"), "sqli"));
        assert!(
            !dir.join("qlpack.yml").exists(),
            "the pack above already covers it"
        );
        std::fs::write(&path, "mine").unwrap();
        let e = scaffold_query(root, &dir, "sqli", Some("python")).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
        let e = scaffold_query(root, &dir, "a/b", Some("python")).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(scaffold_query(root, &dir, " ", None).is_err());
    }

    #[test]
    fn scaffolding_adds_a_pack_file_only_when_one_is_needed_and_possible() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("My App");
        scaffold_query(&root, &root, "loose", None).unwrap();
        assert!(!root.join("qlpack.yml").exists(), "no language, no pack");
        let path = scaffold_query(&root, &root, "hello", Some("go")).unwrap();
        let pack = std::fs::read_to_string(root.join("qlpack.yml")).unwrap();
        assert_eq!(
            pack,
            "name: my-app/queries\nversion: 0.0.1\ndependencies:\n  codeql/go-all: \"*\"\n"
        );
        assert_eq!(pack_language(&pack).as_deref(), Some("go"));
        let packs = discover(&root);
        assert_eq!(packs[0].name, "my-app/queries");
        assert!(packs[0].queries.contains(&path));
        // A pack file above the workspace root does not count.
        write(tmp.path(), "outer/codeql-pack.yml", "name: x/y\n");
        let inner = tmp.path().join("outer/ws");
        scaffold_query(&inner, &inner, "q", Some("ruby")).unwrap();
        assert!(inner.join("qlpack.yml").is_file());
    }
}
