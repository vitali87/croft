//! Adding a CodeQL database: the places one comes from (a folder, an archive, a URL,
//! GitHub), the database picker, downloading and unpacking it on a worker thread,
//! registering it, and adding a database's source to the workspace.

use super::*;

impl App {
    /// The four places a CodeQL database comes from, in a picker whose rows
    /// run the matching side-bar action.
    pub(in crate::app) fn open_codeql_database_sources(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows = CODEQL_DB_SOURCES
            .iter()
            .enumerate()
            .map(|(i, (_, label))| ListRow {
                id: i.to_string(),
                label: String::from(*label),
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(ListPurpose::CodeqlDbSource, "Add CodeQL Database", rows),
            "",
        );
    }

    /// The databases, under the side bar's language, in a picker whose
    /// choice becomes the current one (#578): the database card's switch.
    pub(in crate::app) fn open_codeql_database_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows: Vec<ListRow> = self
            .codeql
            .database_choices()
            .into_iter()
            .map(|(i, label)| ListRow {
                id: i.to_string(),
                label,
            })
            .collect();
        if rows.is_empty() {
            self.status = String::from("No CodeQL databases in this language");
            return;
        }
        self.open_list_picker(
            ListPicker::new(ListPurpose::CodeqlDatabase, "Select CodeQL Database", rows),
            "",
        );
    }

    /// Add database `index`'s source to the workspace (#578, VS Code's
    /// "CodeQL: Add Database Source to Workspace"): its `src` folder, else
    /// its `src.zip`, extracted once into croft's cache.
    pub(in crate::app) fn add_codeql_database_source(&mut self, index: usize) {
        use crate::codeql_db::DbSource;
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.databases.get(index) else {
            return;
        };
        let folder = match crate::codeql_db::database_source(&db.path) {
            None => {
                self.status = format!("CodeQL database {} has no src folder or src.zip", db.name);
                return;
            }
            Some(DbSource::Folder(folder)) => folder,
            Some(DbSource::Zip(zip)) => {
                let cache = Self::codeql_source_cache_dir();
                let dest = cache.join(crate::codeql_db::source_cache_name(&db.path, &zip));
                if !dest.is_dir() {
                    // An older extraction of this database's source is out
                    // of date: its archive changed, which is why the name
                    // no longer matches. Drop it, unless it is open as a
                    // workspace root.
                    let prefix = crate::codeql_db::source_cache_prefix(&db.path);
                    for old in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
                        let path = old.path();
                        let stale = old.file_name().to_string_lossy().starts_with(&prefix);
                        let open = path
                            .canonicalize()
                            .is_ok_and(|p| self.roots.iter().any(|r| r == p));
                        if stale && !open {
                            let _ = std::fs::remove_dir_all(&path);
                        }
                    }
                    // Extract beside it and rename, so an interrupted
                    // extraction never passes for a finished one.
                    let partial = PathBuf::from(format!("{}.partial", dest.display()));
                    let _ = std::fs::remove_dir_all(&partial);
                    let done = crate::codeql_db::extract_zip(
                        &zip,
                        &partial,
                        Some(crate::codeql_db::SOURCE_ZIP_LIMIT),
                    )
                    .and_then(|_| std::fs::rename(&partial, &dest).map_err(|e| e.to_string()));
                    if let Err(e) = done {
                        let _ = std::fs::remove_dir_all(&partial);
                        self.status = format!("Could not extract {}: {e}", zip.display());
                        return;
                    }
                }
                dest
            }
        };
        let folder = folder.canonicalize().unwrap_or(folder);
        let name = db.name.clone();
        if self.roots.iter().any(|r| r == folder) {
            self.status =
                format!("The source of CodeQL database {name} is already in the workspace");
            return;
        }
        self.add_workspace_folder(folder.clone());
        if self.roots.iter().any(|r| r == folder) {
            self.status = format!("Added the source of CodeQL database {name} to the workspace");
        }
    }
}

impl App {
    /// Add a CodeQL database from `value` (#578): a folder, an archive, a
    /// URL to a zip, or `owner/repo [language]` on GitHub.
    pub fn submit_codeql_database(
        &mut self,
        source: crate::widgets::input_prompt::CodeqlDbSource,
        value: &str,
    ) {
        use crate::widgets::input_prompt::CodeqlDbSource;
        if source == CodeqlDbSource::Folder {
            let dir = self.typed_path(value);
            match crate::codeql_db::find_database_in(&dir) {
                Some(found) => self.register_codeql_database(&found),
                None => {
                    self.status = format!(
                        "{} is not a CodeQL database (no codeql-database.yml)",
                        dir.display()
                    );
                }
            }
            return;
        }
        // Downloads and extraction run on a worker (#1155): a database is
        // commonly hundreds of megabytes, and inline the whole UI froze until
        // it was on disk.
        if let Some((what, _)) = &self.codeql_db_job {
            self.status = format!("Still adding the CodeQL database {what}");
            return;
        }
        let cache = Self::codeql_db_cache_dir();
        let (what, job): (String, Box<dyn FnOnce() -> Result<PathBuf, String> + Send>) =
            match source {
                CodeqlDbSource::Archive => {
                    let zip = self.typed_path(value);
                    let stem = zip
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| String::from("database"));
                    (
                        zip.display().to_string(),
                        Box::new(move || Self::extract_codeql_zip(&cache, &zip, &stem)),
                    )
                }
                CodeqlDbSource::Url => {
                    let url = value.trim().to_string();
                    (
                        url.clone(),
                        Box::new(move || Self::download_codeql_zip_from_url(&cache, &url)),
                    )
                }
                CodeqlDbSource::Github => {
                    let repo = value.trim().to_string();
                    let gh = self.gh_program.clone();
                    (
                        repo.clone(),
                        Box::new(move || Self::download_codeql_db_from_github(&gh, &cache, &repo)),
                    )
                }
                CodeqlDbSource::Folder => unreachable!("added above"),
            };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
        self.status = format!("Adding the CodeQL database {what}\u{2026}");
        self.codeql_db_job = Some((what, rx));
    }

    /// Register a database that finished downloading or extracting (#1155).
    pub fn drain_codeql_db_job(&mut self) -> bool {
        let Some((what, rx)) = self.codeql_db_job.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(format!(
                "Adding the CodeQL database {what} stopped unexpectedly"
            )),
        };
        self.codeql_db_job = None;
        match outcome {
            Ok(dir) => self.register_codeql_database(&dir),
            Err(why) => self.status = why,
        }
        true
    }

    /// Add a database folder to the saved list and make it current.
    pub(in crate::app) fn register_codeql_database(&mut self, dir: &std::path::Path) {
        let path = Self::codeql_db_store_path();
        let mut store = crate::codeql_db::DatabaseStore::load(&path);
        match store.add(dir) {
            Ok(i) => {
                if let Err(e) = store.save(&path) {
                    self.status = format!("Could not save the database list: {e}");
                    return;
                }
                let db = &store.databases[i];
                self.status = format!(
                    "Added CodeQL database {}{}",
                    db.name,
                    db.language
                        .as_deref()
                        .map(|l| format!(" ({l})"))
                        .unwrap_or_default()
                );
                self.refresh_codeql_databases();
            }
            Err(why) => self.status = why,
        }
    }

    /// Unpack `zip` into `cache/<name>`. Runs on the add-database worker.
    pub(in crate::app) fn extract_codeql_zip(
        cache: &std::path::Path,
        zip: &std::path::Path,
        name: &str,
    ) -> Result<PathBuf, String> {
        let dest = cache.join(name);
        crate::codeql_db::extract_zip(zip, &dest, None)?;
        crate::codeql_db::find_database_in(&dest)
            .ok_or_else(|| format!("{} holds no CodeQL database", zip.display()))
    }

    pub(in crate::app) fn download_codeql_zip_from_url(
        cache: &std::path::Path,
        url: &str,
    ) -> Result<PathBuf, String> {
        if !url.starts_with("https://") {
            return Err(String::from("A database URL must be https://"));
        }
        let resp = crate::net::get(url)
            .timeout(std::time::Duration::from_secs(300))
            .call()
            .map_err(|e| format!("Download failed: {e}"))?;
        let name = url
            .rsplit('/')
            .next()
            .unwrap_or("database")
            .trim_end_matches(".zip")
            .to_string();
        let tmp = cache.join(format!("{name}.download.zip"));
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        std::io::copy(&mut resp.into_reader(), &mut f).map_err(|e| e.to_string())?;
        let out = Self::extract_codeql_zip(cache, &tmp, &name);
        let _ = std::fs::remove_file(&tmp);
        out
    }

    /// `owner/repo` or `owner/repo language`. With no language, the one
    /// database GitHub has is used; several need the language named.
    pub(in crate::app) fn download_codeql_db_from_github(
        gh: &std::path::Path,
        cache: &std::path::Path,
        value: &str,
    ) -> Result<PathBuf, String> {
        let mut parts = value.split_whitespace();
        let repo = parts
            .next()
            .map(|r| {
                r.trim_start_matches("https://github.com/")
                    .trim_end_matches('/')
            })
            .filter(|r| r.split('/').count() == 2)
            .ok_or_else(|| {
                String::from("Give the repository as owner/repo (and optionally a language)")
            })?
            .to_string();
        let language = match parts.next() {
            Some(l) => l.to_string(),
            None => {
                let mut cmd = std::process::Command::new(gh);
                cmd.args([
                    "api",
                    &format!("/repos/{repo}/code-scanning/codeql/databases"),
                    "--jq",
                    ".[].language",
                ]);
                let out = crate::review_ops::output_retrying_busy(&mut cmd).map_err(|e| {
                    format!(
                        "Could not look up {repo}'s CodeQL databases: {}",
                        crate::gh::spawn_error(&e, None)
                    )
                })?;
                if crate::gh::signed_out(out.status) {
                    return Err(format!(
                        "Could not look up {repo}'s CodeQL databases: {}",
                        crate::gh::SIGNED_OUT
                    ));
                }
                if !out.status.success() {
                    return Err(format!(
                        "{repo} has no CodeQL databases on GitHub: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
                let langs: Vec<String> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect();
                match langs.as_slice() {
                    [one] => one.clone(),
                    [] => return Err(format!("{repo} has no CodeQL databases on GitHub")),
                    many => {
                        return Err(format!(
                            "{repo} has databases for {}; add one with '{repo} <language>'",
                            many.join(", ")
                        ));
                    }
                }
            }
        };
        // Streamed straight to disk: a database can be larger than the
        // memory croft should hold at once.
        let name = format!("{}-{language}", repo.replace('/', "-"));
        let tmp = cache.join(format!("{name}.download.zip"));
        std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut cmd = std::process::Command::new(gh);
        cmd.args(crate::codeql_db::github_database_args(&repo, &language))
            .stdin(std::process::Stdio::null())
            .stdout(file)
            .stderr(std::process::Stdio::piped());
        let out = crate::review_ops::spawn_retrying_busy(&mut cmd)
            .and_then(std::process::Child::wait_with_output)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("Download failed: {}", crate::gh::spawn_error(&e, None))
            })?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&tmp);
            let why = if crate::gh::signed_out(out.status) {
                std::borrow::Cow::Borrowed(crate::gh::SIGNED_OUT)
            } else {
                String::from_utf8_lossy(&out.stderr)
            };
            return Err(format!("Download failed: {}", why.trim()));
        }
        let result = Self::extract_codeql_zip(cache, &tmp, &name);
        let _ = std::fs::remove_file(&tmp);
        result
    }
}
