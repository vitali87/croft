//! The CodeQL CLI: which `codeql` every run uses (settings, `PATH` or croft's own copy),
//! its version, downloading it into croft's cache, and checking for a newer release.

use super::*;

impl App {
    /// Point every CodeQL run at `program`: the side bar's queries and,
    /// through the test worker, the Testing view's CodeQL tests (#578).
    /// The one place the configured `codeql` changes, so the two never
    /// disagree.
    pub(in crate::app) fn set_codeql_program(&mut self, program: PathBuf) {
        self.test_worker.set_codeql_program(program.clone());
        self.codeql_program = program;
    }

    /// Pick the `codeql` to run from settings, `PATH` and croft's managed
    /// copies (#578), at startup and when the setting changes.
    pub(in crate::app) fn resolve_codeql_cli(&mut self) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let resolved = crate::codeql_cli::resolve(
            self.codeql_cli_setting.as_deref(),
            home.as_deref(),
            std::env::var_os("PATH").as_deref(),
            &croft_cache_dir(),
        );
        self.codeql_cli_source = resolved.source;
        self.set_codeql_program(resolved.program);
    }

    /// Ask the CodeQL CLI for its version on a worker thread (#578, VS
    /// Code's "CodeQL: Copy Version Information");
    /// [`Self::drain_codeql_version`] copies the report.
    pub(in crate::app) fn copy_codeql_version(&mut self) {
        if self.codeql_version.is_some() {
            return;
        }
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::read_codeql_version(&program));
        });
        self.status = String::from("Reading the CodeQL CLI version\u{2026}");
        self.codeql_version = Some(rx);
    }

    /// Copy the version report once the CLI has answered (#578). A missing
    /// CLI still copies croft's version and the platform, saying why the
    /// CLI's could not be read.
    pub fn drain_codeql_version(&mut self) -> bool {
        let Some(rx) = self.codeql_version.as_ref() else {
            return false;
        };
        let cli = match rx.try_recv() {
            Ok(cli) => cli,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the version check stopped unexpectedly"))
            }
        };
        self.codeql_version = None;
        let text = crate::codeql_query::version_information(env!("CARGO_PKG_VERSION"), &cli);
        let landing = self.copy_where_pasted(&text);
        self.status = match cli {
            Ok(_) => format!("Copied CodeQL version information{}", landing.note()),
            Err(why) => format!(
                "Copied version information{}; the CodeQL CLI is unavailable: {why}",
                landing.note()
            ),
        };
        true
    }

    /// `program`'s bare version number, or why it could not be read.
    pub(in crate::app) fn read_codeql_version(program: &std::path::Path) -> Result<String, String> {
        let out = crate::review_ops::output_retrying_busy(
            std::process::Command::new(program).args(crate::codeql_query::version_args()),
        );
        match out {
            Ok(o) if o.status.success() => {
                Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr)
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("codeql failed")
                .to_string()),
            Err(e) => Err(format!("could not run codeql: {e}")),
        }
    }

    /// Download the newest known CodeQL CLI release into croft's cache on a
    /// worker thread (#578, the VS Code extension's managed CLI);
    /// [`Self::drain_codeql_cli_download`] switches to it.
    pub(in crate::app) fn download_codeql_cli(&mut self) {
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let Some(asset) = crate::codeql_cli::asset_name(os, arch) else {
            self.status = format!(
                "GitHub publishes no CodeQL CLI for {os} on {arch}; install one and set codeql_cli_path"
            );
            return;
        };
        self.start_codeql_cli_download(asset);
    }

    pub(in crate::app) fn start_codeql_cli_download(&mut self, asset: &'static str) {
        use crate::codeql_cli::JobUpdate;
        if self.codeql_cli_download.is_some() {
            self.status = String::from("The CodeQL CLI is already downloading");
            return;
        }
        let version =
            crate::codeql_cli::install_version(self.codeql_cli_latest.as_deref()).to_string();
        let root = crate::codeql_cli::cli_root(&croft_cache_dir());
        let net = self.codeql_cli_net.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.status = format!("Downloading CodeQL CLI v{version}\u{2026}");
        std::thread::spawn(move || {
            let send = |update| {
                let _ = tx.send(update);
            };
            let out = crate::codeql_cli::download_and_install(&net, &root, &version, asset, &send);
            send(JobUpdate::Done(out.map(|program| (version, program))));
        });
        self.codeql_cli_download = Some(rx);
    }

    /// Show the CLI download's progress, and switch to the CLI once it is
    /// installed (#578), unless settings name another: that one always wins.
    pub fn drain_codeql_cli_download(&mut self) -> bool {
        use crate::codeql_cli::JobUpdate;
        use std::sync::mpsc::TryRecvError;
        let Some(rx) = self.codeql_cli_download.take() else {
            return false;
        };
        let mut changed = false;
        let outcome = loop {
            match rx.try_recv() {
                Ok(JobUpdate::Progress(line)) => {
                    self.status = line;
                    changed = true;
                }
                Ok(JobUpdate::Done(outcome)) => break outcome,
                Err(TryRecvError::Empty) => {
                    self.codeql_cli_download = Some(rx);
                    return changed;
                }
                Err(TryRecvError::Disconnected) => {
                    break Err(String::from("the download stopped unexpectedly"));
                }
            }
        };
        self.status = match outcome {
            Ok((version, _)) if self.codeql_cli_source == crate::codeql_cli::Source::Settings => {
                format!(
                    "Installed CodeQL CLI v{version}; codeql_cli_path in settings still takes precedence"
                )
            }
            Ok((version, program)) => {
                self.set_codeql_program(program);
                let status = format!("Installed CodeQL CLI v{version} and switched to it");
                self.codeql_cli_source = crate::codeql_cli::Source::Managed(version);
                status
            }
            Err(why) => format!("Could not install the CodeQL CLI: {why}"),
        };
        true
    }

    /// Look up the newest CodeQL CLI release and the version in use on a
    /// worker thread (#578, VS Code's "CodeQL: Check for CLI Updates");
    /// [`Self::drain_codeql_cli_check`] compares them.
    pub(in crate::app) fn check_codeql_cli_updates(&mut self) {
        if self.codeql_cli_check.is_some() {
            return;
        }
        // A managed copy's folder names its version; any other CLI is asked.
        let known = match &self.codeql_cli_source {
            crate::codeql_cli::Source::Managed(v) => Some(v.clone()),
            _ => None,
        };
        let program = self.codeql_program.clone();
        let latest = self.codeql_cli_net.latest.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let current = known.or_else(|| Self::read_codeql_version(&program).ok());
            let _ = tx.send((latest(), current));
        });
        self.status = String::from("Checking for CodeQL CLI updates\u{2026}");
        self.codeql_cli_check = Some(rx);
    }

    /// Report the update check (#578), remembering the latest release for
    /// "CodeQL: Download CLI".
    pub fn drain_codeql_cli_check(&mut self) -> bool {
        let Some(rx) = self.codeql_cli_check.as_ref() else {
            return false;
        };
        let (latest, current) = match rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                (Err(String::from("the check stopped unexpectedly")), None)
            }
        };
        self.codeql_cli_check = None;
        self.status = match latest {
            Ok(latest) => {
                let status = crate::codeql_cli::update_status(&latest, current.as_deref());
                self.codeql_cli_latest = Some(latest);
                status
            }
            Err(why) => format!("Could not check for CodeQL CLI updates: {why}"),
        };
        true
    }
}
