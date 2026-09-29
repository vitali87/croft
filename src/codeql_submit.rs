//! Submitting a CodeQL variant analysis (#578): the query goes to GitHub as
//! a bundled query pack, posted to the controller repository, which runs it
//! on Actions against the selected repositories, list or owner.
//!
//! The pack is built as VS Code's CodeQL extension builds it. A query inside
//! a pack travels with that pack's own sources and dependencies; one outside
//! any pack gets a generated pack depending on `codeql/<language>-all`.
//! Either way the pack's `defaultSuite` names the one query, so the run
//! evaluates that query and nothing else in the pack.

use std::path::{Path, PathBuf};

use crate::codeql_variant::{Selection, VariantConfig};

/// What a run targets, in the request's own terms. A user-defined list is
/// sent as its repositories: the API's `repository_lists` names GitHub's
/// lists (`top_100`), not the user's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targets {
    pub repositories: Vec<String>,
    pub owners: Vec<String>,
}

impl Targets {
    /// How the status line names what the run went to.
    pub fn describe(&self) -> String {
        match (self.repositories.as_slice(), self.owners.as_slice()) {
            ([one], []) => one.clone(),
            (many, []) => format!("{} repositories", many.len()),
            ([], [owner]) => format!("every repository of {owner}"),
            _ => String::from("the selected owners"),
        }
    }
}

/// The targets of `config`'s selection, or why there is nothing to run on.
pub fn targets(config: &VariantConfig) -> Result<Targets, String> {
    match &config.selected {
        None => Err(String::from(
            "Select a repository list, repository or owner under Variant Analysis Repositories",
        )),
        Some(Selection::List { list }) => {
            let repos = config
                .lists
                .iter()
                .find(|l| &l.name == list)
                .map(|l| l.repos.clone())
                .ok_or_else(|| format!("The list {list} is no longer there"))?;
            if repos.is_empty() {
                return Err(format!("The list {list} has no repositories"));
            }
            Ok(Targets {
                repositories: repos,
                owners: Vec::new(),
            })
        }
        Some(Selection::Repo { nwo, .. }) => Ok(Targets {
            repositories: vec![nwo.clone()],
            owners: Vec::new(),
        }),
        Some(Selection::Owner { owner }) => Ok(Targets {
            repositories: Vec::new(),
            owners: vec![owner.clone()],
        }),
    }
}

fn pack_file_in(dir: &Path) -> Option<PathBuf> {
    ["qlpack.yml", "codeql-pack.yml"]
        .into_iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// The folder of the nearest pack holding `query`, and its pack file.
pub fn enclosing_pack(query: &Path) -> Option<(PathBuf, PathBuf)> {
    query
        .ancestors()
        .skip(1)
        .find_map(|dir| pack_file_in(dir).map(|file| (dir.to_path_buf(), file)))
}

/// `text`, a pack file, with any top-level `defaultSuite` or
/// `defaultSuiteFile` (and the lines nested under it) replaced by one naming
/// only `query`, a path relative to the pack. A line scan, like the other
/// pack-file readers: top-level keys start in column 0.
pub fn with_default_suite(text: &str, query: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in text.lines() {
        let top_level = !line.starts_with([' ', '\t', '-', '#']) && !line.trim().is_empty();
        if top_level {
            let key = line.split(':').next().unwrap_or("").trim();
            skipping = key == "defaultSuite" || key == "defaultSuiteFile";
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(&format!(
        "defaultSuite:\n  - query: {}\n",
        yaml_string(query)
    ));
    out
}

fn yaml_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// The pack file of a generated pack running `file` alone, for `language`.
pub fn generated_pack_file(language: &str, file: &str) -> String {
    format!(
        "name: codeql-remote/query\nversion: 0.0.0\ndependencies:\n  codeql/{language}-all: \"*\"\ndefaultSuite:\n  - query: {}\n",
        yaml_string(file)
    )
}

/// Whether a file of a pack travels in the bundle: sources, pack files and
/// the lock file, not databases, results or test output.
fn is_pack_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ql" | "qll" | "yml" | "yaml" | "dbscheme")
    )
}

/// Most files a pack copy takes before refusing, so a query picked from
/// inside a huge checkout cannot fill the disk.
pub const PACK_FILE_CAP: usize = 20_000;

fn copy_pack_sources(from: &Path, to: &Path, copied: &mut usize) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        // `.codeql` holds downloaded dependencies and caches; hidden
        // folders never hold pack sources.
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            copy_pack_sources(&path, &to.join(&name), copied)?;
        } else if kind.is_file() && is_pack_source(&path) {
            *copied += 1;
            if *copied > PACK_FILE_CAP {
                return Err(format!(
                    "The query's pack holds over {PACK_FILE_CAP} source files; move the query into a smaller pack"
                ));
            }
            std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
            std::fs::copy(&path, to.join(&name)).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Lay out in `dest` (created, and expected empty) the pack that runs
/// `query`: a copy of its own pack's sources with the default suite pointed
/// at it, or a generated pack for `language` when it is in none. Returns
/// the pack's folder.
pub fn prepare_pack(query: &Path, language: &str, dest: &Path) -> Result<PathBuf, String> {
    let pack = dest.join("pack");
    match enclosing_pack(query) {
        Some((dir, file)) => {
            let rel = query
                .strip_prefix(&dir)
                .map_err(|_| format!("{} is outside its pack", query.display()))?;
            copy_pack_sources(&dir, &pack, &mut 0)?;
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            let name = file.file_name().unwrap_or_default();
            std::fs::write(pack.join(name), with_default_suite(&text, &rel))
                .map_err(|e| format!("{}: {e}", pack.display()))?;
        }
        None => {
            let name = query
                .file_name()
                .ok_or_else(|| format!("{} is not a query file", query.display()))?;
            std::fs::create_dir_all(&pack).map_err(|e| format!("{}: {e}", pack.display()))?;
            std::fs::copy(query, pack.join(name))
                .map_err(|e| format!("{}: {e}", query.display()))?;
            std::fs::write(
                pack.join("qlpack.yml"),
                generated_pack_file(language, &name.to_string_lossy()),
            )
            .map_err(|e| format!("{}: {e}", pack.display()))?;
        }
    }
    Ok(pack)
}

/// `codeql` arguments bundling the pack in `dir` into the archive `out`.
pub fn pack_bundle_args(dir: &Path, out: &Path) -> Vec<String> {
    vec![
        String::from("pack"),
        String::from("bundle"),
        format!("--output={}", out.display()),
        String::from("--"),
        dir.display().to_string(),
    ]
}

/// The body of the submission: the bundled pack, base64 encoded, the
/// query's language, and what it runs on.
pub fn submission_body(language: &str, bundle: &[u8], targets: &Targets) -> serde_json::Value {
    use base64::Engine;
    let mut body = serde_json::json!({
        "language": language,
        "query_pack": base64::engine::general_purpose::STANDARD.encode(bundle),
    });
    if !targets.repositories.is_empty() {
        body["repositories"] = serde_json::json!(targets.repositories);
    }
    if !targets.owners.is_empty() {
        body["repository_owners"] = serde_json::json!(targets.owners);
    }
    body
}

/// `gh` arguments posting the body in `body_file` to `controller`.
pub fn submit_args(controller: &str, body_file: &Path) -> Vec<String> {
    vec![
        String::from("api"),
        String::from("--method"),
        String::from("POST"),
        format!("repos/{controller}/code-scanning/codeql/variant-analyses"),
        String::from("--input"),
        body_file.display().to_string(),
    ]
}

/// What GitHub said about a submitted run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Submitted {
    pub id: u64,
    pub controller: String,
    pub query: PathBuf,
    pub language: String,
    /// The Actions run that does the work, once GitHub has started it.
    #[serde(default)]
    pub workflow_run: Option<u64>,
    /// Repositories GitHub will not run on (no database, private, …).
    #[serde(default)]
    pub skipped: usize,
    pub submitted_at: u64,
    /// The last progress read back from GitHub, when it has been.
    #[serde(default)]
    pub progress: Option<RunProgress>,
}

/// How far one repository of a run has got.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoProgress {
    pub nwo: String,
    /// GitHub's `analysis_status`: `pending`, `in_progress`, `succeeded`,
    /// `failed`, `canceled` or `timed_out`.
    pub status: String,
    #[serde(default)]
    pub results: Option<u64>,
    #[serde(default)]
    pub failure: Option<String>,
}

/// A run's progress as GitHub reports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunProgress {
    /// The run's own `status`: `in_progress`, `succeeded`, `failed` or
    /// `cancelled`.
    pub status: String,
    #[serde(default)]
    pub failure: Option<String>,
    pub repos: Vec<RepoProgress>,
}

/// Repository statuses that will not change again.
fn repo_finished(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "canceled" | "timed_out")
}

impl RunProgress {
    /// Whether GitHub will report nothing new for the run.
    pub fn is_done(&self) -> bool {
        self.status != "in_progress"
    }

    /// Repositories finished, and those the run covers.
    pub fn counts(&self) -> (usize, usize) {
        let done = self
            .repos
            .iter()
            .filter(|r| repo_finished(&r.status))
            .count();
        (done, self.repos.len())
    }

    /// Results across the repositories that reported a count.
    pub fn results(&self) -> u64 {
        self.repos.iter().filter_map(|r| r.results).sum()
    }
}

/// `gh` arguments reading run `id`'s progress from `controller`.
pub fn progress_args(controller: &str, id: u64) -> Vec<String> {
    vec![
        String::from("api"),
        format!("repos/{controller}/code-scanning/codeql/variant-analyses/{id}"),
    ]
}

/// The progress in GitHub's answer to [`progress_args`].
pub fn parse_progress(json: &str) -> Result<RunProgress, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("GitHub's answer was not JSON: {e}"))?;
    let status = v
        .get("status")
        .and_then(|s| s.as_str())
        .ok_or_else(|| String::from("GitHub's answer gave the run no status"))?
        .to_string();
    let text = |v: &serde_json::Value, k: &str| {
        v.get(k)
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let repos = v
        .get("scanned_repositories")
        .and_then(|r| r.as_array())
        .map(|repos| {
            repos
                .iter()
                .filter_map(|r| {
                    Some(RepoProgress {
                        nwo: r.get("repository")?.get("full_name")?.as_str()?.to_string(),
                        status: text(r, "analysis_status")
                            .unwrap_or_else(|| String::from("pending")),
                        results: r.get("result_count").and_then(|c| c.as_u64()),
                        failure: text(r, "failure_message"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(RunProgress {
        status,
        failure: text(&v, "failure_reason"),
        repos,
    })
}

/// The side bar's line for `run`.
pub fn run_label(run: &Submitted) -> String {
    let name = run.query.file_name().unwrap_or_default().to_string_lossy();
    match &run.progress {
        None => format!("{name} · submitted"),
        Some(p) => {
            let (done, total) = p.counts();
            let state = match p.status.as_str() {
                "in_progress" => "running",
                other => other,
            };
            format!(
                "{name} · {state} · {done}/{total} repos · {} results",
                p.results()
            )
        }
    }
}

/// A Markdown report of `run`: its status, then a row per repository with
/// its status, result count and any failure, each repository linked.
pub fn run_report(run: &Submitted) -> String {
    let name = run.query.file_name().unwrap_or_default().to_string_lossy();
    let mut out = format!(
        "# Variant analysis {} of {name}\n\nController: {}  \nLanguage: {}  \nRun: {}\n\n",
        run.id,
        run.controller,
        run.language,
        run.url()
    );
    let Some(p) = &run.progress else {
        out.push_str("No progress read from GitHub yet.\n");
        return out;
    };
    let (done, total) = p.counts();
    out.push_str(&format!(
        "Status: {} · {done}/{total} repositories · {} results\n",
        p.status,
        p.results()
    ));
    if let Some(why) = &p.failure {
        out.push_str(&format!("Failure: {why}\n"));
    }
    if run.skipped > 0 {
        out.push_str(&format!("Skipped by GitHub: {}\n", run.skipped));
    }
    if !repos_with_results(run).is_empty() {
        out.push_str(
            "Results: press v on the run in the CodeQL side bar, or run \"CodeQL: Open Variant Analysis Results\".\n",
        );
    }
    out.push_str("\n| Repository | Status | Results | Failure |\n|---|---|---|---|\n");
    let cell = |s: &str| s.replace('|', "\\|").replace('\n', " ");
    for r in &p.repos {
        out.push_str(&format!(
            "| [{nwo}](https://github.com/{nwo}) | {} | {} | {} |\n",
            r.status,
            r.results.map(|n| n.to_string()).unwrap_or_default(),
            r.failure.as_deref().map(cell).unwrap_or_default(),
            nwo = r.nwo
        ));
    }
    out
}

/// `gh` arguments reading repository `nwo`'s part of run `id`: its status
/// and where its results are.
pub fn repo_task_args(controller: &str, id: u64, nwo: &str) -> Vec<String> {
    vec![
        String::from("api"),
        format!("repos/{controller}/code-scanning/codeql/variant-analyses/{id}/repos/{nwo}"),
    ]
}

/// Where a repository's results are, from GitHub's answer to
/// [`repo_task_args`]: the artifact's (signed, short-lived) URL, and the
/// commit its database was built from.
pub fn parse_artifact(json: &str) -> Result<(String, Option<String>), String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("GitHub's answer was not JSON: {e}"))?;
    let url = v
        .get("artifact_url")
        .and_then(|u| u.as_str())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| String::from("GitHub has no results for it"))?;
    let sha = v
        .get("database_commit_sha")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok((url.to_string(), sha))
}

/// The repositories of `run` whose results are worth fetching: those that
/// succeeded with at least one result.
pub fn repos_with_results(run: &Submitted) -> Vec<String> {
    run.progress
        .iter()
        .flat_map(|p| &p.repos)
        .filter(|r| r.status == "succeeded" && r.results.unwrap_or(0) > 0)
        .map(|r| r.nwo.clone())
        .collect()
}

/// One repository's results: its SARIF log, or its table as CSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoResults {
    Sarif(String),
    Table(String),
}

/// One SARIF log holding every repository's runs, each run marked with its
/// repository and commit (`versionControlProvenance`) so a location that is
/// not on this machine can be opened on GitHub.
pub fn combine_sarif(parts: &[(String, Option<String>, String)]) -> Result<String, String> {
    let mut runs = Vec::new();
    for (nwo, sha, text) in parts {
        let log: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("{nwo}'s SARIF: {e}"))?;
        for mut run in log
            .get("runs")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let mut provenance =
                serde_json::json!({ "repositoryUri": format!("https://github.com/{nwo}") });
            if let Some(sha) = sha {
                provenance["revisionId"] = serde_json::json!(sha);
            }
            run["versionControlProvenance"] = serde_json::json!([provenance]);
            run["automationDetails"] = serde_json::json!({ "id": format!("{nwo}/") });
            runs.push(run);
        }
    }
    let log = serde_json::json!({
        "version": "2.1.0",
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "runs": runs,
    });
    serde_json::to_string_pretty(&log).map_err(|e| e.to_string())
}

/// One CSV of every repository's table, a `repository` column first. The
/// header is the first table's; each table's own header row is dropped.
pub fn combine_csv(parts: &[(String, String)]) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut out = String::new();
    for (i, (nwo, csv)) in parts.iter().enumerate() {
        let mut lines = csv.lines();
        let header = lines.next().unwrap_or_default();
        if i == 0 {
            out.push_str(&format!("\"repository\",{header}\n"));
        }
        for line in lines.filter(|l| !l.is_empty()) {
            out.push_str(&format!("{},{line}\n", quote(nwo)));
        }
    }
    out
}

/// Whether croft fetches an artifact from `url`: GitHub hands out https
/// links only; tests serve theirs from a local port.
pub fn artifact_url_allowed(url: &str) -> bool {
    url.starts_with("https://") || (cfg!(test) && url.starts_with("http://127.0.0.1:"))
}

/// The file called `name` in `dir` or a folder below it (a results
/// archive may nest its files a level or two down), shallowest first.
pub fn find_named(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut level = vec![dir.to_path_buf()];
    for _ in 0..4 {
        let mut next = Vec::new();
        for d in &level {
            let direct = d.join(name);
            if direct.is_file() {
                return Some(direct);
            }
            if let Ok(entries) = std::fs::read_dir(d) {
                let mut dirs: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                dirs.sort();
                next.extend(dirs);
            }
        }
        level = next;
    }
    None
}

/// The most one repository's results archive may expand to.
pub const ARTIFACT_LIMIT: u64 = 2 << 30;

/// The fields of one CSV line, as `codeql bqrs decode --format=csv` writes
/// them: comma separated, quoted with `""` for a quote inside.
pub fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => fields.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    fields.push(field);
    fields
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// One repository's part of an export.
struct RepoExport {
    nwo: String,
    lines: Vec<String>,
    count: usize,
    /// Its table's header is written.
    table: bool,
}

/// The index of `nwo`'s part in `repos`, added when it is new.
fn repo_entry(repos: &mut Vec<RepoExport>, nwo: &str) -> usize {
    match repos.iter().position(|r| r.nwo == nwo) {
        Some(i) => i,
        None => {
            repos.push(RepoExport {
                nwo: nwo.to_string(),
                lines: Vec::new(),
                count: 0,
                table: false,
            });
            repos.len() - 1
        }
    }
}

/// The Markdown files exporting `run`'s results (VS Code's "Export
/// results"): `_summary.md`, which sorts first (a gist shows it on top),
/// listing each repository with its result count and a link to its own
/// file, then a file per repository. `sarif` is the combined log
/// [`combine_sarif`] made; each alert is its message and a link to its line
/// on GitHub. `csv` is the table [`combine_csv`] made, split back by its
/// repository column.
pub fn export_markdown(
    run: &Submitted,
    sarif: Option<&str>,
    csv: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let mut repos: Vec<RepoExport> = Vec::new();
    let mut add = Vec::new();
    if let Some(text) = sarif {
        let log: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("the results' SARIF: {e}"))?;
        for r in log["runs"].as_array().into_iter().flatten() {
            let vcs = &r["versionControlProvenance"][0];
            let Some(nwo) = vcs["repositoryUri"]
                .as_str()
                .and_then(|u| u.strip_prefix("https://github.com/"))
            else {
                continue;
            };
            let rev = vcs["revisionId"].as_str().unwrap_or("HEAD");
            for result in r["results"].as_array().into_iter().flatten() {
                let message = result["message"]["text"].as_str().unwrap_or("(no message)");
                let loc = &result["locations"][0]["physicalLocation"];
                let line = match loc["artifactLocation"]["uri"].as_str() {
                    Some(uri) => {
                        let at = loc["region"]["startLine"].as_i64().unwrap_or(0);
                        let shown = if at > 0 {
                            format!("{uri}:{at}")
                        } else {
                            uri.to_string()
                        };
                        let fragment = if at > 0 {
                            format!("#L{at}")
                        } else {
                            String::new()
                        };
                        format!(
                            "- {} ([{shown}](https://github.com/{nwo}/blob/{rev}/{}{fragment}))",
                            md_cell(message),
                            uri.trim_start_matches('/')
                        )
                    }
                    None => format!("- {}", md_cell(message)),
                };
                add.push((nwo.to_string(), line));
            }
        }
    }
    for (nwo, line) in add {
        let i = repo_entry(&mut repos, &nwo);
        repos[i].lines.push(line);
        repos[i].count += 1;
    }
    if let Some(text) = csv {
        let mut lines = text.lines();
        let header: Vec<String> = lines
            .next()
            .map(csv_fields)
            .unwrap_or_default()
            .into_iter()
            .skip(1)
            .collect();
        let mut rows: Vec<(String, Vec<String>)> = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            let mut fields = csv_fields(line);
            if fields.is_empty() {
                continue;
            }
            let nwo = fields.remove(0);
            rows.push((nwo, fields));
        }
        for (nwo, fields) in rows {
            let i = repo_entry(&mut repos, &nwo);
            if !repos[i].table {
                // Alerts and a table for one repository: the table follows.
                if !repos[i].lines.is_empty() {
                    repos[i].lines.push(String::new());
                }
                let cells: Vec<String> = header.iter().map(|h| md_cell(h)).collect();
                repos[i].lines.push(format!("| {} |", cells.join(" | ")));
                repos[i]
                    .lines
                    .push(format!("|{}", "---|".repeat(header.len().max(1))));
                repos[i].table = true;
            }
            let cells: Vec<String> = fields.iter().map(|f| md_cell(f)).collect();
            repos[i].lines.push(format!("| {} |", cells.join(" | ")));
            repos[i].count += 1;
        }
    }
    let name = run
        .query
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let file_of = |nwo: &str| format!("{}.md", nwo.replace('/', "-"));
    let total: usize = repos.iter().map(|r| r.count).sum();
    let mut summary = format!(
        "# Variant analysis {} of {name}\n\nQuery: `{name}` ({})  \nController: {}  \nRun: {}\n\n{total} results in {} repositor{}\n\n| Repository | Results |\n|---|---|\n",
        run.id,
        run.language,
        run.controller,
        run.url(),
        repos.len(),
        if repos.len() == 1 { "y" } else { "ies" }
    );
    let mut files = Vec::new();
    for r in &repos {
        summary.push_str(&format!(
            "| [{}]({}) | {} |\n",
            r.nwo,
            file_of(&r.nwo),
            r.count
        ));
        files.push((
            file_of(&r.nwo),
            format!(
                "# {} in [{nwo}](https://github.com/{nwo})\n\n{} results\n\n{}\n",
                name,
                r.count,
                r.lines.join("\n"),
                nwo = r.nwo
            ),
        ));
    }
    files.insert(0, (String::from("_summary.md"), summary));
    Ok(files)
}

/// `gh` arguments creating a secret gist of `files`, described by `desc`.
pub fn gist_args(desc: &str, files: &[PathBuf]) -> Vec<String> {
    let mut args = vec![
        String::from("gist"),
        String::from("create"),
        String::from("--desc"),
        desc.to_string(),
    ];
    args.extend(files.iter().map(|f| f.display().to_string()));
    args
}

/// Replace the remembered run `id` in the file at `path` with `run`.
pub fn update_submitted(path: &Path, run: &Submitted) -> std::io::Result<()> {
    let mut runs = load_submitted(path);
    match runs
        .iter_mut()
        .find(|r| r.id == run.id && r.controller == run.controller)
    {
        Some(slot) => *slot = run.clone(),
        None => runs.push(run.clone()),
    }
    let text = serde_json::to_string_pretty(&runs).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

impl Submitted {
    /// Where to watch the run on GitHub: its Actions run when known, else
    /// the controller's Actions page.
    pub fn url(&self) -> String {
        match self.workflow_run {
            Some(run) => format!("https://github.com/{}/actions/runs/{run}", self.controller),
            None => format!("https://github.com/{}/actions", self.controller),
        }
    }
}

/// The run GitHub's answer to a submission describes.
pub fn parse_submission(
    json: &str,
    controller: &str,
    query: &Path,
    language: &str,
    submitted_at: u64,
) -> Result<Submitted, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("GitHub's answer was not JSON: {e}"))?;
    let id = v
        .get("id")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| String::from("GitHub's answer named no variant analysis"))?;
    // Each kind of skip lists its repositories under `repositories`.
    let skipped = v
        .get("skipped_repositories")
        .and_then(|s| s.as_object())
        .map(|kinds| {
            kinds
                .values()
                .filter_map(|k| k.get("repository_count").and_then(|c| c.as_u64()))
                .sum::<u64>() as usize
        })
        .unwrap_or(0);
    Ok(Submitted {
        id,
        controller: controller.to_string(),
        query: query.to_path_buf(),
        language: language.to_string(),
        workflow_run: v.get("actions_workflow_run_id").and_then(|r| r.as_u64()),
        skipped,
        submitted_at,
        progress: None,
    })
}

/// The submitted runs croft remembers, newest last, for following them up.
pub fn load_submitted(path: &Path) -> Vec<Submitted> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn record_submitted(path: &Path, run: Submitted) -> std::io::Result<()> {
    let mut runs = load_submitted(path);
    runs.push(run);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&runs).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_targets_the_selection_and_a_user_list_goes_as_its_repositories() {
        let mut c = VariantConfig::default();
        assert!(targets(&c).is_err(), "nothing selected");
        c.add_list("top").unwrap();
        c.select(crate::codeql_variant::Item::List(0)).unwrap();
        assert!(targets(&c).unwrap_err().contains("no repositories"));
        c.add_repo(Some(0), "a/b").unwrap();
        c.add_repo(Some(0), "c/d").unwrap();
        let t = targets(&c).unwrap();
        assert_eq!(t.repositories, ["a/b", "c/d"]);
        assert_eq!(t.describe(), "2 repositories");
        let body = submission_body("python", b"tgz", &t);
        assert_eq!(body["repositories"], serde_json::json!(["a/b", "c/d"]));
        assert_eq!(body["query_pack"], "dGd6");
        assert!(body.get("repository_lists").is_none());
        assert!(body.get("repository_owners").is_none());

        c.select(crate::codeql_variant::Item::Repo(Some(0), 1))
            .unwrap();
        assert_eq!(targets(&c).unwrap().describe(), "c/d");
        c.add_owner("octo").unwrap();
        c.select(crate::codeql_variant::Item::Owner(0)).unwrap();
        let t = targets(&c).unwrap();
        assert_eq!(t.describe(), "every repository of octo");
        let body = submission_body("go", b"", &t);
        assert_eq!(body["repository_owners"], serde_json::json!(["octo"]));
        assert!(body.get("repositories").is_none());
    }

    #[test]
    fn the_default_suite_is_replaced_by_the_one_query() {
        let text = "name: me/q\ndefaultSuiteFile: suites/all.qls\ndependencies:\n  codeql/python-all: \"*\"\ndefaultSuite:\n  - queries: .\n  - exclude:\n      kind: diagnostic\nversion: 1.0.0\n";
        let out = with_default_suite(text, "src/Find Me.ql");
        assert_eq!(
            out,
            "name: me/q\ndependencies:\n  codeql/python-all: \"*\"\nversion: 1.0.0\ndefaultSuite:\n  - query: \"src/Find Me.ql\"\n"
        );
    }

    #[test]
    fn a_query_in_a_pack_travels_with_its_sources_and_one_outside_gets_a_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("ws/pack");
        std::fs::create_dir_all(src.join("lib")).unwrap();
        std::fs::create_dir_all(src.join(".codeql/deps")).unwrap();
        std::fs::create_dir_all(src.join("queries")).unwrap();
        std::fs::write(src.join("qlpack.yml"), "name: me/q\nversion: 0.0.1\n").unwrap();
        std::fs::write(src.join("codeql-pack.lock.yml"), "lockVersion: 1.0.0\n").unwrap();
        std::fs::write(src.join("lib/Helpers.qll"), "predicate p() { any() }").unwrap();
        std::fs::write(src.join("queries/Q.ql"), "select 1").unwrap();
        std::fs::write(src.join("queries/notes.md"), "not a source").unwrap();
        std::fs::write(src.join(".codeql/deps/Big.qll"), "cached").unwrap();

        let out = tmp.path().join("out");
        let pack = prepare_pack(&src.join("queries/Q.ql"), "python", &out).unwrap();
        assert!(pack.join("lib/Helpers.qll").is_file());
        assert!(pack.join("queries/Q.ql").is_file());
        assert!(pack.join("codeql-pack.lock.yml").is_file());
        assert!(!pack.join("queries/notes.md").exists());
        assert!(!pack.join(".codeql").exists());
        let file = std::fs::read_to_string(pack.join("qlpack.yml")).unwrap();
        assert!(
            file.ends_with("defaultSuite:\n  - query: \"queries/Q.ql\"\n"),
            "{file}"
        );
        assert!(file.starts_with("name: me/q\n"), "{file}");

        let loose = tmp.path().join("ws/Loose.ql");
        std::fs::write(&loose, "select 2").unwrap();
        let out = tmp.path().join("out2");
        let pack = prepare_pack(&loose, "go", &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(pack.join("Loose.ql")).unwrap(),
            "select 2"
        );
        let file = std::fs::read_to_string(pack.join("qlpack.yml")).unwrap();
        assert!(file.contains("codeql/go-all: \"*\""), "{file}");
        assert!(file.contains("- query: \"Loose.ql\""), "{file}");
    }

    #[test]
    fn githubs_answer_names_the_run_and_its_skips() {
        let json = r#"{"id": 42, "query_language": "python", "status": "in_progress",
            "actions_workflow_run_id": 777,
            "skipped_repositories": {
                "access_mismatch_repos": {"repository_count": 2, "repositories": []},
                "no_codeql_db_repos": {"repository_count": 1, "repositories": []},
                "not_found_repos": {"repository_count": 0, "repository_full_names": []}
            }}"#;
        let run = parse_submission(json, "me/ctl", Path::new("/q.ql"), "python", 9).unwrap();
        assert_eq!((run.id, run.workflow_run, run.skipped), (42, Some(777), 3));
        assert_eq!(run.url(), "https://github.com/me/ctl/actions/runs/777");
        let bare = parse_submission(r#"{"id": 1}"#, "me/ctl", Path::new("/q.ql"), "go", 0).unwrap();
        assert_eq!(bare.url(), "https://github.com/me/ctl/actions");
        assert!(
            parse_submission(r#"{"message": "x"}"#, "me/ctl", Path::new("/q"), "go", 0).is_err()
        );
    }

    #[test]
    fn submitted_runs_are_remembered_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("runs.json");
        assert!(load_submitted(&path).is_empty());
        for id in [5, 6] {
            let run = parse_submission(
                &format!(r#"{{"id": {id}}}"#),
                "a/b",
                Path::new("/q.ql"),
                "go",
                id,
            )
            .unwrap();
            record_submitted(&path, run).unwrap();
        }
        let ids: Vec<u64> = load_submitted(&path).iter().map(|r| r.id).collect();
        assert_eq!(ids, [5, 6]);
    }
    #[test]
    fn a_runs_progress_counts_finished_repositories_and_results() {
        let json = r#"{"id": 7, "status": "in_progress",
            "scanned_repositories": [
                {"repository": {"full_name": "a/b"}, "analysis_status": "succeeded", "result_count": 3},
                {"repository": {"full_name": "c/d"}, "analysis_status": "failed", "failure_message": "no | db\nhere"},
                {"repository": {"full_name": "e/f"}, "analysis_status": "in_progress"},
                {"repository": {"full_name": "g/h"}, "analysis_status": "succeeded", "result_count": 0}
            ]}"#;
        let p = parse_progress(json).unwrap();
        assert!(!p.is_done());
        assert_eq!((p.counts(), p.results()), ((3, 4), 3));
        let mut run = parse_submission(
            r#"{"id": 7, "actions_workflow_run_id": 1}"#,
            "me/ctl",
            Path::new("/w/Find.ql"),
            "python",
            0,
        )
        .unwrap();
        assert_eq!(run_label(&run), "Find.ql · submitted");
        assert!(run_report(&run).contains("No progress read"));
        run.progress = Some(p);
        assert_eq!(run_label(&run), "Find.ql · running · 3/4 repos · 3 results");
        let report = run_report(&run);
        assert!(
            report.contains("| [a/b](https://github.com/a/b) | succeeded | 3 |  |"),
            "{report}"
        );
        assert!(
            report.contains("| failed |  | no \\| db here |"),
            "{report}"
        );
        assert!(
            report.contains("Run: https://github.com/me/ctl/actions/runs/1"),
            "{report}"
        );

        let done = parse_progress(r#"{"status": "cancelled", "failure_reason": ""}"#).unwrap();
        assert!(done.is_done() && done.repos.is_empty() && done.failure.is_none());
        assert!(parse_progress(r#"{"message": "Not Found"}"#).is_err());
        assert_eq!(
            progress_args("me/ctl", 7),
            [
                "api",
                "repos/me/ctl/code-scanning/codeql/variant-analyses/7"
            ]
        );
    }

    #[test]
    fn updating_a_remembered_run_replaces_it_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("runs.json");
        let run = |id: u64| {
            parse_submission(
                &format!(r#"{{"id": {id}}}"#),
                "a/b",
                Path::new("/q.ql"),
                "go",
                0,
            )
            .unwrap()
        };
        record_submitted(&path, run(1)).unwrap();
        record_submitted(&path, run(2)).unwrap();
        let mut first = run(1);
        first.progress = parse_progress(r#"{"status": "succeeded"}"#).ok();
        update_submitted(&path, &first).unwrap();
        let runs = load_submitted(&path);
        assert_eq!(runs.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 2]);
        assert!(runs[0].progress.as_ref().is_some_and(|p| p.is_done()));
    }
    #[test]
    fn repository_results_are_found_and_combined_per_repository() {
        let task = r#"{"analysis_status": "succeeded", "artifact_url": "https://x/a.zip", "database_commit_sha": "abc"}"#;
        assert_eq!(
            parse_artifact(task).unwrap(),
            (String::from("https://x/a.zip"), Some(String::from("abc")))
        );
        assert!(parse_artifact(r#"{"analysis_status": "failed", "artifact_url": ""}"#).is_err());
        assert_eq!(
            repo_task_args("me/ctl", 7, "a/b"),
            [
                "api",
                "repos/me/ctl/code-scanning/codeql/variant-analyses/7/repos/a/b"
            ]
        );

        let mut run =
            parse_submission(r#"{"id": 7}"#, "me/ctl", Path::new("/q.ql"), "go", 0).unwrap();
        run.progress = parse_progress(
            r#"{"status": "succeeded", "scanned_repositories": [
                {"repository": {"full_name": "a/b"}, "analysis_status": "succeeded", "result_count": 2},
                {"repository": {"full_name": "c/d"}, "analysis_status": "succeeded", "result_count": 0},
                {"repository": {"full_name": "e/f"}, "analysis_status": "failed"}]}"#,
        )
        .ok();
        assert_eq!(repos_with_results(&run), ["a/b"]);

        let one = r#"{"version": "2.1.0", "runs": [{"tool": {"driver": {"name": "CodeQL"}}, "results": [{"message": {"text": "x"}}]}]}"#;
        let combined = combine_sarif(&[
            (
                String::from("a/b"),
                Some(String::from("abc")),
                one.to_string(),
            ),
            (String::from("c/d"), None, one.to_string()),
        ])
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&combined).unwrap();
        assert_eq!(v["runs"].as_array().unwrap().len(), 2);
        assert_eq!(
            v["runs"][0]["versionControlProvenance"][0],
            serde_json::json!({"repositoryUri": "https://github.com/a/b", "revisionId": "abc"})
        );
        assert!(
            v["runs"][1]["versionControlProvenance"][0]
                .get("revisionId")
                .is_none()
        );
        assert!(
            crate::sarif::load::parse_log(&combined).is_ok(),
            "the viewer reads it"
        );
        assert!(combine_sarif(&[(String::from("a/b"), None, String::from("{"))]).is_err());

        let csv = combine_csv(&[
            (String::from("a/b"), String::from("\"col\"\n\"1\"\n")),
            (String::from("c/\"d"), String::from("\"col\"\n\"2\"\n\n")),
        ]);
        assert_eq!(
            csv,
            "\"repository\",\"col\"\n\"a/b\",\"1\"\n\"c/\"\"d\",\"2\"\n"
        );
        assert!(artifact_url_allowed(
            "https://objects.githubusercontent.com/x"
        ));
        assert!(!artifact_url_allowed("http://example.com/x"));
    }
    #[test]
    fn an_export_is_a_summary_and_a_file_per_repository() {
        assert_eq!(
            csv_fields(r#""a","b ""c"", d",3,"#),
            ["a", "b \"c\", d", "3", ""]
        );
        let run = parse_submission(
            r#"{"id": 9, "actions_workflow_run_id": 5}"#,
            "me/ctl",
            Path::new("/w/Find.ql"),
            "python",
            0,
        )
        .unwrap();
        let one = |msg: &str, line: i64| {
            format!(
                r#"{{"runs": [{{"tool": {{"driver": {{"name": "CodeQL"}}}}, "results": [{{"message": {{"text": "{msg}"}}, "locations": [{{"physicalLocation": {{"artifactLocation": {{"uri": "src/x.py"}}, "region": {{"startLine": {line}}}}}}}]}}]}}]}}"#
            )
        };
        let sarif = combine_sarif(&[
            (
                String::from("a/b"),
                Some(String::from("abc")),
                one("bad | thing", 4),
            ),
            (String::from("c/d"), None, one("other", 0)),
        ])
        .unwrap();
        let csv = combine_csv(&[(
            String::from("e/f"),
            String::from("\"name\",\"n\"\n\"x\",\"1\"\n\"y|z\",\"2\"\n"),
        )]);
        let files = export_markdown(&run, Some(&sarif), Some(&csv)).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["_summary.md", "a-b.md", "c-d.md", "e-f.md"]);
        let summary = &files[0].1;
        assert!(
            summary.starts_with("# Variant analysis 9 of Find.ql\n"),
            "{summary}"
        );
        assert!(summary.contains("4 results in 3 repositories"), "{summary}");
        assert!(summary.contains("| [a/b](a-b.md) | 1 |"), "{summary}");
        assert!(summary.contains("| [e/f](e-f.md) | 2 |"), "{summary}");
        assert!(
            summary.contains("Run: https://github.com/me/ctl/actions/runs/5"),
            "{summary}"
        );
        assert!(
            files[1].1.contains(
                "- bad \\| thing ([src/x.py:4](https://github.com/a/b/blob/abc/src/x.py#L4))"
            ),
            "{}",
            files[1].1
        );
        assert!(
            files[2]
                .1
                .contains("- other ([src/x.py](https://github.com/c/d/blob/HEAD/src/x.py))"),
            "{}",
            files[2].1
        );
        assert!(
            files[3]
                .1
                .contains("| name | n |\n|---|---|\n| x | 1 |\n| y\\|z | 2 |"),
            "{}",
            files[3].1
        );
        let empty = export_markdown(&run, None, None).unwrap();
        assert_eq!(empty.len(), 1);
        assert!(empty[0].1.contains("0 results in 0 repositories"));
        assert_eq!(
            gist_args("d", &[PathBuf::from("/t/_summary.md")]),
            ["gist", "create", "--desc", "d", "/t/_summary.md"]
        );
    }
}
