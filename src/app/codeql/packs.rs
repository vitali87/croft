//! Suites and packs: installing a pack's dependencies, downloading packs on a worker
//! thread, running a suite, a pack, or a published pack, and refusing a suite whose
//! results cannot be shown.

use super::*;

impl App {
    /// VS Code's "CodeQL: Install Pack Dependencies" for pack `pack` of the
    /// Queries section (#578): `codeql pack install` on its folder.
    pub(in crate::app) fn install_codeql_pack_dependencies(&mut self, pack: usize) {
        let Some(p) = self.codeql.queries.get(pack) else {
            return;
        };
        if p.name == crate::codeql_query::NO_PACK {
            self.status =
                String::from("These queries are in no pack, so they have no dependencies");
            return;
        }
        let (name, args) = (
            p.name.clone(),
            crate::codeql_query::pack_install_args(&p.dir),
        );
        self.start_codeql_pack_job(
            args,
            format!("Installing the dependencies of {name}\u{2026}"),
            format!("Installed the dependencies of {name}"),
            format!("Could not install the dependencies of {name}"),
        );
    }

    /// Ask which packs "CodeQL: Download Packs" fetches (#578).
    pub(in crate::app) fn prompt_download_codeql_packs(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlDownloadPacks,
            String::from("Download Packs"),
            "packs to download, e.g. codeql/java-queries codeql/python-all@1.0.0",
        ));
    }

    pub(in crate::app) fn submit_download_codeql_packs(&mut self, value: &str) {
        let packs = crate::codeql_query::parse_pack_list(value);
        if packs.is_empty() {
            self.status = String::from("Name at least one pack to download");
            return;
        }
        let what = packs.join(", ");
        self.start_codeql_pack_job(
            crate::codeql_query::pack_download_args(&packs),
            format!("Downloading {what}\u{2026}"),
            format!("Downloaded {what}"),
            format!("Could not download {what}"),
        );
    }

    /// Run a `codeql pack` command on a worker thread (#578); one at a
    /// time, since installs and downloads write the same package cache.
    pub(in crate::app) fn start_codeql_pack_job(
        &mut self,
        args: Vec<String>,
        running: String,
        done: String,
        failed: String,
    ) {
        if self.codeql_pack_job.is_some() {
            self.status = String::from("A CodeQL pack install or download is already running");
            return;
        }
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::codeql_command(&program, &args));
        });
        self.status = running;
        self.codeql_pack_job = Some((rx, done, failed));
    }

    /// Collect a finished pack install or download (#578) onto the status
    /// line.
    pub fn drain_codeql_pack_job(&mut self) -> bool {
        let Some((rx, _, _)) = self.codeql_pack_job.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the pack command stopped unexpectedly"))
            }
        };
        let Some((_, done, failed)) = self.codeql_pack_job.take() else {
            return false;
        };
        self.status = match outcome {
            Ok(()) => done,
            Err(why) => format!("{failed}: {why}"),
        };
        true
    }
}

impl App {
    /// VS Code's "CodeQL: Run Query Suite" (#578): the open `.qls` suite on
    /// the current database, through `database analyze` into SARIF.
    pub(in crate::app) fn run_codeql_query_suite(&mut self) {
        if !self
            .editor
            .path
            .as_deref()
            .is_some_and(crate::codeql_query::is_suite)
        {
            self.status = String::from("Open a .qls query suite to run it");
            return;
        }
        self.run_codeql_query();
    }

    /// CodeQL: Run Queries in Published Pack (#578): ask which pack.
    pub(in crate::app) fn prompt_run_codeql_published_pack(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::CodeqlRunPublishedPack,
            String::from("Run Queries in Published Pack"),
            "e.g. codeql/python-queries, scope/name@1.0.0, scope/name:path/in/pack",
        ));
    }

    /// Download the pack `reference` names and run it, or the part of it
    /// after `:`, on the current database into SARIF (#578).
    pub(in crate::app) fn run_codeql_published_pack(&mut self, reference: &str) {
        let reference = reference.trim();
        if crate::codeql_query::published_pack(reference).is_none() {
            self.status = format!("{reference:?} is not a pack: write scope/name");
            return;
        }
        self.run_codeql_file_on(PathBuf::from(reference), "", CodeqlRunMode::Pack, None);
    }

    /// Refuse suite `suite` when it selects a query whose results are not
    /// alerts (#578): `database analyze` cannot produce those tables.
    pub(in crate::app) fn check_codeql_suite(program: &Path, suite: &Path) -> Result<(), String> {
        let out = crate::review_ops::output_retrying_busy(
            std::process::Command::new(program)
                .args(crate::codeql_query::resolve_suite_args(suite)),
        )
        .map_err(|e| format!("could not run codeql: {e}"))?;
        if !out.status.success() {
            return Err(crate::codeql_query::failure_reason(
                &String::from_utf8_lossy(&out.stderr),
            ));
        }
        let tables = crate::codeql_query::table_queries_in_suite(
            &String::from_utf8_lossy(&out.stdout),
            |p| std::fs::read_to_string(p).ok(),
        )?;
        if tables.is_empty() {
            return Ok(());
        }
        Err(format!(
            "the suite selects queries that are not alert queries ({}); run those on their own",
            tables.join(", ")
        ))
    }

    /// Run every query in pack `pack` on the current database, one after
    /// another (#578): the first starts now and [`Self::drain_codeql_run`]
    /// starts each next one as the one before finishes, failed or not.
    /// Each run keeps its own history entry. Refused while a query is
    /// running, so two pack runs never interleave.
    pub(in crate::app) fn run_codeql_pack(&mut self, pack: usize) {
        if self.codeql_run.is_some() {
            self.status = String::from("A CodeQL query is already running");
            return;
        }
        let Some(queries) = self.codeql.queries.get(pack).map(|p| p.queries.clone()) else {
            return;
        };
        self.run_codeql_batch(queries, "That pack has no queries");
    }
}
