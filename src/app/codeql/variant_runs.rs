//! Variant analysis runs: submitting the open query, following the runs, their logs,
//! fetching and opening their results, and exporting them.

use super::*;

impl App {
    /// Mirror the remembered variant analyses into the side bar.
    pub(in crate::app) fn refresh_codeql_variant_runs(&mut self) {
        self.codeql.variant_runs =
            crate::codeql_submit::load_submitted(&Self::codeql_variant_runs_path())
                .iter()
                .map(crate::codeql_submit::run_label)
                .collect();
    }

    /// Open the report of remembered variant analysis `index`: its status
    /// and a row per repository. It is written from the last progress read;
    /// an unfinished run is read again at once.
    pub(in crate::app) fn open_codeql_variant_run(&mut self, index: usize) {
        let runs = crate::codeql_submit::load_submitted(&Self::codeql_variant_runs_path());
        let Some(run) = runs.get(index) else {
            return;
        };
        if !run.progress.as_ref().is_some_and(|p| p.is_done()) {
            self.codeql_variant_polled = None;
        }
        let path = croft_cache_dir()
            .join("codeql")
            .join("variant-reports")
            .join(format!(
                "{}-{}.md",
                run.controller.replace('/', "-"),
                run.id
            ));
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, crate::codeql_submit::run_report(run)));
        if let Err(e) = written {
            self.status = format!("{}: {e}", path.display());
            return;
        }
        match self.editor.open(&path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.focus_pane(Pane::Editor);
            }
            Err(e) => self.status = format!("{}: {e}", path.display()),
        }
    }

    /// Follow submitted variant analyses (#578): every
    /// [`CODEQL_VARIANT_POLL`], read the progress of those GitHub has not
    /// finished, off the UI thread, and record what comes back. A run
    /// finishing says so on the status line. Runs older than
    /// [`CODEQL_VARIANT_FOLLOW`] are left alone: GitHub keeps their results
    /// only so long, and one that never finishes would be read forever.
    /// Nothing is read until the side bar has loaded the runs (opening the
    /// CodeQL view, or a submission), so a session that never touches
    /// CodeQL never calls GitHub for it. Returns true when progress landed.
    pub fn poll_codeql_variant_runs(&mut self) -> bool {
        use crate::codeql_submit as cs;
        if let Some(rx) = self.codeql_variant_poll.as_ref() {
            let polled = match rx.try_recv() {
                Ok(p) => p,
                Err(std::sync::mpsc::TryRecvError::Empty) => return false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Vec::new(),
            };
            self.codeql_variant_poll = None;
            let path = Self::codeql_variant_runs_path();
            for (mut run, outcome) in polled {
                let Ok(progress) = outcome else { continue };
                let was_done = run.progress.as_ref().is_some_and(|p| p.is_done());
                if progress.is_done() && !was_done {
                    let (done, total) = progress.counts();
                    self.status = format!(
                        "Variant analysis {} of {} {}: {done}/{total} repositories, {} results",
                        run.id,
                        run.query.file_name().unwrap_or_default().to_string_lossy(),
                        progress.status,
                        progress.results()
                    );
                }
                run.progress = Some(progress);
                let _ = cs::update_submitted(&path, &run);
            }
            self.refresh_codeql_variant_runs();
            return true;
        }
        if self.codeql.variant_runs.is_empty()
            || !self.is_extension_enabled("codeql")
            || self
                .codeql_variant_polled
                .is_some_and(|t| t.elapsed() < CODEQL_VARIANT_POLL)
        {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let unfinished: Vec<cs::Submitted> = cs::load_submitted(&Self::codeql_variant_runs_path())
            .into_iter()
            .filter(|r| !r.progress.as_ref().is_some_and(|p| p.is_done()))
            .filter(|r| now.saturating_sub(r.submitted_at) < CODEQL_VARIANT_FOLLOW.as_secs())
            .collect();
        self.codeql_variant_polled = Some(std::time::Instant::now());
        if unfinished.is_empty() {
            return false;
        }
        let gh = self.gh_program.to_string_lossy().into_owned();
        let root = self.active_workspace_root().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let polled = unfinished
                .into_iter()
                .map(|run| {
                    let outcome = crate::pr_review::run_gh(
                        &gh,
                        &cs::progress_args(&run.controller, run.id),
                        &root,
                        crate::pr_review::GH_TIMEOUT,
                    )
                    .and_then(|answer| cs::parse_progress(&answer));
                    (run, outcome)
                })
                .collect();
            let _ = tx.send(polled);
        });
        self.codeql_variant_poll = Some(rx);
        false
    }

    /// The submitted variant analysis run `index`, else the newest; says
    /// so when none has been submitted.
    pub(in crate::app) fn codeql_variant_run(
        &mut self,
        index: Option<usize>,
    ) -> Option<crate::codeql_submit::Submitted> {
        let runs = crate::codeql_submit::load_submitted(&Self::codeql_variant_runs_path());
        let run = index.map_or(runs.last(), |i| runs.get(i)).cloned();
        if run.is_none() {
            self.status = String::from("No variant analysis has been submitted yet");
        }
        run
    }

    /// Copy the repositories variant analysis run `index` (else the newest)
    /// covers, one per line (#578).
    pub(in crate::app) fn copy_codeql_variant_repo_list(&mut self, index: Option<usize>) {
        let Some(run) = self.codeql_variant_run(index) else {
            return;
        };
        self.status = match crate::codeql_submit::repository_list(&run) {
            Some(list) => {
                let landing = self.copy_where_pasted(&list);
                format!(
                    "Copied the {} repositories of variant analysis {}{}",
                    list.lines().count(),
                    run.id,
                    landing.note()
                )
            }
            None => format!(
                "Variant analysis {} has not reported its repositories yet",
                run.id
            ),
        };
    }

    /// Open the GitHub Actions run of variant analysis run `index` (else
    /// the newest), where its logs are (#578).
    pub(in crate::app) fn view_codeql_variant_logs(&mut self, index: Option<usize>) {
        let Some(run) = self.codeql_variant_run(index) else {
            return;
        };
        match crate::codeql_submit::workflow_run_url(&run) {
            Some(url) => self.open_url_for_user(&url),
            None => {
                self.status = format!(
                    "Variant analysis {} has no Actions run yet; GitHub starts one shortly",
                    run.id
                )
            }
        }
    }

    /// Fetch the results of remembered variant analysis `index` (the newest
    /// when `None`) and open them (#578): each repository that succeeded
    /// with results has its artifact downloaded (once; later opens use the
    /// copy), its alerts gathered into one SARIF log, one run per
    /// repository, and its tables into one CSV with a repository column.
    /// The fetching happens on a worker thread;
    /// [`Self::drain_codeql_variant_results`] opens what it made.
    pub(in crate::app) fn open_codeql_variant_results(&mut self, index: Option<usize>) {
        use crate::codeql_submit as cs;
        if self.codeql_variant_results.is_some() {
            self.status = String::from("Variant analysis results are already being fetched");
            return;
        }
        let runs = cs::load_submitted(&Self::codeql_variant_runs_path());
        let Some(run) = index.map_or(runs.last(), |i| runs.get(i)).cloned() else {
            self.status = String::from("No variant analysis has been submitted yet");
            return;
        };
        let repos = cs::repos_with_results(&run);
        if repos.is_empty() {
            self.status = match &run.progress {
                None => format!(
                    "Variant analysis {} has not reported any progress yet",
                    run.id
                ),
                Some(_) => format!("Variant analysis {} has no results yet", run.id),
            };
            return;
        }
        let dir = croft_cache_dir()
            .join("codeql")
            .join("variant-results")
            .join(format!("{}-{}", run.controller.replace('/', "-"), run.id));
        let gh = self.gh_program.to_string_lossy().into_owned();
        let program = self.codeql_program.clone();
        let root = self.active_workspace_root().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let count = repos.len();
        std::thread::spawn(move || {
            let _ = tx.send(Self::fetch_codeql_variant_results(
                &gh, &program, &root, &run, &repos, &dir,
            ));
        });
        self.codeql_variant_results = Some(rx);
        self.status = format!(
            "Fetching the results of {count} repositor{}\u{2026}",
            if count == 1 { "y" } else { "ies" }
        );
    }

    /// The worker behind [`Self::open_codeql_variant_results`].
    pub(in crate::app) fn fetch_codeql_variant_results(
        gh: &str,
        program: &Path,
        root: &Path,
        run: &crate::codeql_submit::Submitted,
        repos: &[String],
        dir: &Path,
    ) -> VariantResults {
        use crate::codeql_submit::{self as cs, RepoResults};
        let mut sarif = Vec::new();
        let mut tables = Vec::new();
        let mut failed = Vec::new();
        for nwo in repos {
            let repo_dir = dir.join(nwo.replace('/', "-"));
            let fetched = (|| -> Result<(Option<String>, RepoResults), String> {
                let sha_file = repo_dir.join("commit");
                if !repo_dir.join("done").is_file() {
                    let answer = crate::pr_review::run_gh(
                        gh,
                        &cs::repo_task_args(&run.controller, run.id, nwo),
                        root,
                        crate::pr_review::GH_TIMEOUT,
                    )?;
                    let (url, sha) = cs::parse_artifact(&answer)?;
                    if !cs::artifact_url_allowed(&url) {
                        return Err(String::from(
                            "GitHub gave a results link croft will not fetch",
                        ));
                    }
                    let _ = std::fs::remove_dir_all(&repo_dir);
                    std::fs::create_dir_all(&repo_dir)
                        .map_err(|e| format!("{}: {e}", repo_dir.display()))?;
                    let resp = crate::net::builder(&url)
                        .timeout(std::time::Duration::from_secs(300))
                        .build()
                        .get(&url)
                        .call()
                        .map_err(|e| format!("download failed: {e}"))?;
                    let zip = repo_dir.join("results.zip");
                    let mut f = std::fs::File::create(&zip).map_err(|e| e.to_string())?;
                    std::io::copy(&mut resp.into_reader(), &mut f)
                        .map_err(|e| format!("download failed: {e}"))?;
                    crate::codeql_db::extract_zip(&zip, &repo_dir, Some(cs::ARTIFACT_LIMIT))?;
                    let _ = std::fs::remove_file(&zip);
                    std::fs::write(&sha_file, sha.unwrap_or_default())
                        .map_err(|e| e.to_string())?;
                    std::fs::write(repo_dir.join("done"), "").map_err(|e| e.to_string())?;
                }
                let sha = std::fs::read_to_string(&sha_file)
                    .ok()
                    .filter(|s| !s.is_empty());
                let found = |name: &str| cs::find_named(&repo_dir, name);
                if let Some(path) = found("results.sarif") {
                    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    return Ok((sha, RepoResults::Sarif(text)));
                }
                let bqrs = found("results.bqrs")
                    .ok_or_else(|| String::from("its results hold neither SARIF nor BQRS"))?;
                let csv = repo_dir.join("results.csv");
                if !csv.is_file() {
                    Self::codeql_decode_sets(program, &bqrs, &csv, &|args| {
                        Self::codeql_command(program, &args)
                    })?;
                }
                let text = std::fs::read_to_string(&csv).map_err(|e| e.to_string())?;
                Ok((sha, RepoResults::Table(text)))
            })();
            match fetched {
                Ok((sha, RepoResults::Sarif(text))) => sarif.push((nwo.clone(), sha, text)),
                Ok((_, RepoResults::Table(text))) => tables.push((nwo.clone(), text)),
                Err(why) => failed.push((nwo.clone(), why)),
            }
        }
        let write = |name: &str, text: String| -> Result<PathBuf, String> {
            let path = dir.join(name);
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(path)
        };
        let sarif_path = match sarif.is_empty() {
            true => None,
            false => Some(write("results.sarif", cs::combine_sarif(&sarif)?)?),
        };
        let csv_path = match tables.is_empty() {
            true => None,
            false => Some(write("results.csv", cs::combine_csv(&tables))?),
        };
        Ok(VariantResultFiles {
            sarif: sarif_path,
            csv: csv_path,
            repos: sarif.len() + tables.len(),
            failed,
        })
    }

    /// Open the results a finished fetch made: the SARIF log in the viewer
    /// (else the CSV), saying what else there is and what failed.
    pub fn drain_codeql_variant_results(&mut self) -> bool {
        let Some(rx) = self.codeql_variant_results.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the fetch stopped without answering"))
            }
        };
        self.codeql_variant_results = None;
        let files = match outcome {
            Ok(files) => files,
            Err(why) => {
                self.status = format!("Could not open the variant analysis results: {why}");
                return true;
            }
        };
        let failed = match files.failed.as_slice() {
            [] => String::new(),
            [(nwo, why)] => format!("; {nwo} failed: {why}"),
            [(nwo, why), rest @ ..] => {
                format!("; {nwo} failed: {why} (and {} more)", rest.len())
            }
        };
        let Some(open) = files.sarif.clone().or_else(|| files.csv.clone()) else {
            self.status = format!("No repository's results could be fetched{failed}");
            return true;
        };
        if let Err(e) = self.editor.open(&open) {
            self.status = format!("{}: {e}", open.display());
            return true;
        }
        self.sync_open_file_poll_mtime();
        self.focus_pane(Pane::Editor);
        let tables = match (&files.sarif, &files.csv) {
            (Some(_), Some(csv)) => format!("; tables in {}", csv.display()),
            _ => String::new(),
        };
        self.status = format!(
            "Results of {} repositor{}{tables}{failed}",
            files.repos,
            if files.repos == 1 { "y" } else { "ies" }
        );
        true
    }

    /// Ask where to export the results of remembered variant analysis
    /// `index` (the newest when `None`): a folder, offered as one in the
    /// workspace, or `gist` (#578, VS Code's "Export results").
    pub(in crate::app) fn prompt_export_codeql_variant_results(&mut self, index: Option<usize>) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let runs = crate::codeql_submit::load_submitted(&Self::codeql_variant_runs_path());
        let Some(i) = index.or(runs.len().checked_sub(1)) else {
            self.status = String::from("No variant analysis has been submitted yet");
            return;
        };
        let Some(run) = runs.get(i) else {
            return;
        };
        if crate::codeql_submit::repos_with_results(run).is_empty() {
            self.status = format!("Variant analysis {} has no results to export", run.id);
            return;
        }
        let folder = self
            .workspace_root()
            .join(format!("codeql-variant-analysis-{}", run.id));
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlExportVariantResults { index: i },
                format!("Export Variant Analysis {} Results", run.id),
                "a folder for the Markdown files, or gist for a secret gist",
            )
            .with_value(folder.display().to_string()),
        );
    }

    /// Export on a worker thread: fetch the results as
    /// [`Self::open_codeql_variant_results`] does (downloaded copies are
    /// reused), write them as Markdown, and for `gist` post the files with
    /// `gh gist create`.
    pub(in crate::app) fn start_export_codeql_variant_results(
        &mut self,
        index: usize,
        value: &str,
    ) {
        use crate::codeql_submit as cs;
        if self.codeql_variant_export.is_some() {
            self.status = String::from("Variant analysis results are already being exported");
            return;
        }
        let value = value.trim();
        if value.is_empty() {
            self.status = String::from("Give a folder, or gist");
            return;
        }
        let runs = cs::load_submitted(&Self::codeql_variant_runs_path());
        let Some(run) = runs.get(index).cloned() else {
            self.status = String::from("That variant analysis is no longer remembered");
            return;
        };
        let gist = value.eq_ignore_ascii_case("gist");
        let base = croft_cache_dir()
            .join("codeql")
            .join("variant-results")
            .join(format!("{}-{}", run.controller.replace('/', "-"), run.id));
        let target = if gist {
            base.join("export")
        } else {
            self.typed_path(value)
        };
        let repos = cs::repos_with_results(&run);
        let gh = self.gh_program.to_string_lossy().into_owned();
        let program = self.codeql_program.clone();
        let root = self.active_workspace_root().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = (|| {
                let files =
                    Self::fetch_codeql_variant_results(&gh, &program, &root, &run, &repos, &base)?;
                if files.repos == 0 {
                    let why = files.failed.first().map_or("", |(_, w)| w.as_str());
                    return Err(format!("no repository's results could be fetched: {why}"));
                }
                let read = |p: &Option<PathBuf>| -> Result<Option<String>, String> {
                    p.as_ref()
                        .map(|p| {
                            std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
                        })
                        .transpose()
                };
                let markdown = cs::export_markdown(
                    &run,
                    read(&files.sarif)?.as_deref(),
                    read(&files.csv)?.as_deref(),
                )?;
                if gist {
                    let _ = std::fs::remove_dir_all(&target);
                }
                std::fs::create_dir_all(&target)
                    .map_err(|e| format!("{}: {e}", target.display()))?;
                let mut written = Vec::new();
                for (name, text) in &markdown {
                    let path = target.join(name);
                    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
                    written.push(path);
                }
                let url = match gist {
                    false => None,
                    true => {
                        let name = run
                            .query
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let desc = format!(
                            "{name} (variant analysis {} of {} repositories)",
                            run.id, files.repos
                        );
                        let out = crate::pr_review::run_gh(
                            &gh,
                            &cs::gist_args(&desc, &written),
                            &root,
                            crate::pr_review::GH_TIMEOUT,
                        )?;
                        Some(
                            out.lines()
                                .rev()
                                .find(|l| l.starts_with("https://"))
                                .ok_or_else(|| String::from("gh made no gist"))?
                                .trim()
                                .to_string(),
                        )
                    }
                };
                Ok(VariantExport {
                    summary: (!gist).then(|| target.join("_summary.md")),
                    gist: url,
                    repos: files.repos,
                    failed: files.failed,
                })
            })();
            let _ = tx.send(outcome);
        });
        self.codeql_variant_export = Some(rx);
        self.status = String::from("Exporting the variant analysis results\u{2026}");
    }

    /// Report a finished export: open a folder export's summary, or name
    /// the gist.
    pub fn drain_codeql_variant_export(&mut self) -> bool {
        let Some(rx) = self.codeql_variant_export.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the export stopped without answering"))
            }
        };
        self.codeql_variant_export = None;
        let done = match outcome {
            Ok(done) => done,
            Err(why) => {
                self.status = format!("Could not export the variant analysis results: {why}");
                return true;
            }
        };
        let failed = match done.failed.as_slice() {
            [] => String::new(),
            [(nwo, why)] => format!("; {nwo} left out: {why}"),
            [(nwo, why), rest @ ..] => format!("; {nwo} left out: {why} (and {} more)", rest.len()),
        };
        let plural = if done.repos == 1 { "y" } else { "ies" };
        self.status = match (done.gist, done.summary) {
            (Some(url), _) => format!("Exported {} repositor{plural} to {url}{failed}", done.repos),
            (None, Some(summary)) => {
                if self.editor.open(&summary).is_ok() {
                    self.sync_open_file_poll_mtime();
                    self.focus_pane(Pane::Editor);
                }
                let dir = summary.parent().unwrap_or(&summary).display().to_string();
                format!("Exported {} repositor{plural} to {dir}{failed}", done.repos)
            }
            (None, None) => String::from("Exported nothing"),
        };
        true
    }

    /// Where submitted variant analyses are remembered (#578).
    pub(in crate::app) fn codeql_variant_runs_path() -> PathBuf {
        croft_cache_dir().join("codeql-variant-analyses.json")
    }

    /// Submit the open query as a variant analysis (#578, VS Code's "CodeQL:
    /// Run Variant Analysis"): bundle it into a query pack and post it to
    /// the controller repository, which runs it on GitHub Actions against
    /// the selected list, repository or owner. The bundling and the post
    /// happen on a worker thread; [`Self::drain_codeql_variant_submit`]
    /// reports the run.
    pub(in crate::app) fn run_codeql_variant_analysis(&mut self) {
        use crate::codeql_submit as cs;
        if self.codeql_variant_submit.is_some() {
            self.status = String::from("A variant analysis is already being submitted");
            return;
        }
        let query = match self.editor.path.clone() {
            Some(p) if p.extension().is_some_and(|e| e == "ql") => p,
            _ => {
                self.status = String::from("Open a .ql query to run a variant analysis");
                return;
            }
        };
        // The bundle is built from the file on disk.
        if self.editor.dirty {
            self.status = String::from("Save the query before running a variant analysis");
            return;
        }
        let config = match crate::codeql_variant::VariantConfig::load(&Self::codeql_variant_path())
        {
            Ok(c) => c,
            Err(why) => {
                self.status = format!("Could not read the variant analysis config: {why}");
                return;
            }
        };
        let Some(controller) = config.controller_repo.clone() else {
            self.status = String::from(
                "Set up a controller repository under Variant Analysis Repositories first",
            );
            return;
        };
        let targets = match cs::targets(&config) {
            Ok(t) => t,
            Err(why) => {
                self.status = why;
                return;
            }
        };
        let pack_language = cs::enclosing_pack(&query)
            .and_then(|(_, file)| std::fs::read_to_string(file).ok())
            .and_then(|text| crate::codeql_query::pack_language(&text));
        let Some(language) =
            pack_language.or_else(|| self.codeql_search_language().map(str::to_string))
        else {
            self.status = String::from(
                "The query's pack names no language: pick one in the CodeQL side bar's Language section",
            );
            return;
        };
        let submitted_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stem = query
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let work = croft_cache_dir()
            .join("codeql")
            .join("variant")
            .join(format!("{submitted_at}-{stem}"));
        let program = self.codeql_program.clone();
        let gh = self.gh_program.to_string_lossy().into_owned();
        let root = self.active_workspace_root().to_path_buf();
        let describe = targets.describe();
        let (tx, rx) = std::sync::mpsc::channel();
        let query_for_run = query.clone();
        std::thread::spawn(move || {
            let query = query_for_run;
            let outcome = (|| {
                let _ = std::fs::remove_dir_all(&work);
                let pack = cs::prepare_pack(&query, &language, &work)?;
                let run = |args: Vec<String>| Self::codeql_command(&program, &args);
                run(crate::codeql_query::pack_install_args(&pack))
                    .map_err(|e| format!("installing the pack's dependencies failed: {e}"))?;
                let bundle = work.join("pack.tgz");
                run(cs::pack_bundle_args(&pack, &bundle))
                    .map_err(|e| format!("bundling the query failed: {e}"))?;
                let bytes =
                    std::fs::read(&bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
                let body = work.join("body.json");
                std::fs::write(
                    &body,
                    cs::submission_body(&language, &bytes, &targets).to_string(),
                )
                .map_err(|e| format!("{}: {e}", body.display()))?;
                let answer = crate::pr_review::run_gh(
                    &gh,
                    &cs::submit_args(&controller, &body),
                    &root,
                    crate::pr_review::GH_TIMEOUT,
                )?;
                cs::parse_submission(&answer, &controller, &query, &language, submitted_at)
            })();
            let _ = std::fs::remove_dir_all(&work);
            let _ = tx.send(outcome);
        });
        self.codeql_variant_submit = Some(rx);
        self.status = format!(
            "Submitting {} as a variant analysis against {describe}\u{2026}",
            query.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    /// Report a finished variant analysis submission, and remember the run.
    /// Returns true when one finished.
    pub fn drain_codeql_variant_submit(&mut self) -> bool {
        let Some(rx) = self.codeql_variant_submit.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the submission stopped without answering"))
            }
        };
        self.codeql_variant_submit = None;
        self.status = match outcome {
            Ok(run) => {
                let name = run
                    .query
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let skipped = match run.skipped {
                    0 => String::new(),
                    n => format!(
                        ", {n} repositor{} skipped",
                        if n == 1 { "y" } else { "ies" }
                    ),
                };
                let url = run.url();
                let id = run.id;
                match crate::codeql_submit::record_submitted(&Self::codeql_variant_runs_path(), run)
                {
                    Ok(()) => {
                        // Follow it from now on, starting with the next tick.
                        self.codeql_variant_polled = None;
                        self.refresh_codeql_variant_runs();
                        format!("Variant analysis {id} of {name} submitted{skipped}: {url}")
                    }
                    Err(e) => format!(
                        "Variant analysis {id} of {name} submitted{skipped}: {url} (not remembered: {e})"
                    ),
                }
            }
            Err(why) => format!("Could not submit the variant analysis: {why}"),
        };
        true
    }
}
