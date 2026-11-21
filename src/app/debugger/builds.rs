//! Debug builds: a launch.json `cargo` config's build, F5's C/C++ build and a single Rust
//! file's, each on a worker thread, and launching lldb-dap on the binary they make.

use super::*;

impl App {
    /// Run a launch's `cargo` build on a background thread (#1392); the
    /// drain launches the config once cargo names the executable.
    pub(in crate::app) fn start_cargo_program_build(
        &mut self,
        rc: crate::dap::configs::ResolvedConfig,
        slot: LaunchSlot,
        spec: crate::dap::configs::CargoSpec,
    ) {
        let cwd = rc
            .cwd
            .clone()
            .unwrap_or_else(|| self.active_workspace_root());
        let line = format!("cargo {}", spec.args.join(" "));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::dap::configs::build_cargo_program(&spec, &cwd));
        });
        self.run_debug.feedback = Some(format!("Building with {line}\u{2026}"));
        self.run_debug.feedback_is_error = false;
        self.status = format!(
            "config \"{}\": building with {line} \u{2014} debug starts when it succeeds",
            rc.name
        );
        let root = self.tree.root.clone();
        self.pending_cargo_launches
            .push((rx, Box::new(rc), slot, root));
    }

    /// Launch every `cargo` config whose build finished, or report why it
    /// did not. True when anything landed.
    pub(in crate::app) fn drain_pending_cargo_launch(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let mut landed = false;
        let mut i = 0;
        while i < self.pending_cargo_launches.len() {
            let result = match self.pending_cargo_launches[i].0.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => {
                    i += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(String::from("the build thread died")),
            };
            let (_, mut rc, slot, build_root) = self.pending_cargo_launches.remove(i);
            landed = true;
            // Re-rooting mid-build would otherwise launch the old
            // workspace's binary in the new one.
            if build_root != self.tree.root {
                self.status = format!(
                    "config \"{}\": cargo finished, but the workspace changed — launch it again from its workspace",
                    rc.name
                );
                continue;
            }
            match result {
                Ok(program) => {
                    rc.program = Some(program.to_string_lossy().into_owned());
                    self.launch_resolved_in(*rc, slot);
                }
                Err(e) => self.debug_error(format!("config \"{}\": {e}", rc.name)),
            }
        }
        landed
    }
}

impl App {
    /// Start building the program F5 on the C/C++ file `path` debugs
    /// (#1359): the project's CMake target, else the file with its
    /// `compile_commands.json` flags, else the file alone. The build runs
    /// on its own thread so a long one leaves the UI live; F5 again
    /// replaces it.
    pub(in crate::app) fn begin_debug_build(&mut self, path: &Path) {
        let (tx, rx) = std::sync::mpsc::channel();
        // The root that owns the file: an added folder's project, not the
        // primary's.
        let workspace = self
            .roots
            .owning_root(path)
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf();
        let source = path.to_path_buf();
        let cache = croft_cache_dir().join("debug-builds");
        std::thread::spawn(move || {
            let _ = tx.send(crate::cpp_build::build_for_debug(
                &workspace, &source, &cache,
            ));
        });
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let msg = format!("Building {name} for debugging…");
        self.run_debug.feedback = Some(msg.clone());
        self.run_debug.feedback_is_error = false;
        self.status = msg;
        self.debug_build = Some((path.to_path_buf(), rx));
    }

    /// F5's C/C++ build, once it is done: the source it debugs and the
    /// build's result. `None` while it runs or when none was started.
    pub(in crate::app) fn take_debug_build(
        &mut self,
    ) -> Option<(
        PathBuf,
        Result<crate::cpp_build::Built, crate::cpp_build::Failed>,
    )> {
        let (_, rx) = self.debug_build.as_ref()?;
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(crate::cpp_build::Failed {
                summary: String::from("the build stopped without a result"),
                output: String::new(),
            }),
        };
        let (source, _) = self.debug_build.take()?;
        Some((source, result))
    }

    /// Launch lldb-dap once F5's C/C++ build is done, or report why it
    /// failed with the build's whole output in OUTPUT's "Debug Build"
    /// channel. True when the build finished this tick.
    pub(in crate::app) fn poll_debug_build(&mut self) -> bool {
        let Some((source, result)) = self.take_debug_build() else {
            return false;
        };
        match result {
            Ok(built) => {
                self.launch_lldb(&built.binary, &source);
                if !self.run_debug.feedback_is_error {
                    let line = self.run_debug.feedback.take().unwrap_or_default();
                    self.run_debug.feedback = Some(format!("{line}, built by {}", built.how));
                }
            }
            Err(failed) => {
                const CHANNEL: &str = "Debug Build";
                crate::output::clear(CHANNEL);
                for line in failed.output.lines() {
                    crate::output::push(CHANNEL, crate::output::OutputLevel::Error, line);
                }
                self.debug_error(format!(
                    "Build for debug failed: {} (full output in OUTPUT › {CHANNEL})",
                    failed.summary
                ));
            }
        }
        true
    }

    /// Launch lldb-dap on an already-built `binary`, attributing status to
    /// `label_path` (the source for a compiled language, or the binary itself
    /// for a directly-debugged executable). Shared by the compiled-source path
    /// and the executable-binary path so neither duplicates the launch plumbing.
    pub(in crate::app) fn launch_lldb(&mut self, binary: &Path, label_path: &Path) {
        let Some(adapter) = lldb_dap_path() else {
            self.debug_error(lldb_dap_missing_message());
            return;
        };
        let breakpoints = self.collect_editor_breakpoints();
        let cwd = label_path
            .parent()
            .unwrap_or(self.workspace_root())
            .to_path_buf();
        match crate::dap::session::DapSession::launch_with(
            &adapter,
            &[],
            &cwd,
            crate::dap::session::lldb_launch_request(binary, false),
            breakpoints,
        ) {
            Ok(session) => {
                let name = label_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.adopt_debug_session(name.clone(), session, LaunchSlot::Replace);
                self.run_debug.feedback = Some(format!("Debugging {name} (lldb)"));
                self.run_debug.feedback_is_error = false;
                self.status =
                    format!("Debugging {name} via lldb-dap — F5 continue · Shift+F5 stop");
                self.reveal_debug_view();
            }
            Err(e) => {
                self.debug_error(format!("Failed to start lldb-dap: {e}"));
            }
        }
    }

    /// Compile the single Rust file `path` with debug info and return the
    /// binary, next to the source as `<stem>.dbg`. C and C++ build through
    /// their project instead (`begin_debug_build`, #1359).
    pub(in crate::app) fn build_debuggable_binary(&self, path: &Path) -> std::io::Result<PathBuf> {
        let out = path.with_extension("dbg");
        // .output(), never .status(): the compiler must not inherit croft's
        // TTY - a compile error would spray its diagnostics over the UI
        // (same class as the pdftoppm trailer-dictionary spray).
        let cmd_out = std::process::Command::new("rustc")
            .arg("-g")
            .arg("-o")
            .arg(&out)
            .arg(path)
            .output()?;
        if !cmd_out.status.success() {
            let stderr = String::from_utf8_lossy(&cmd_out.stderr);
            return Err(std::io::Error::other(format!(
                "rustc exited with {}: {}",
                cmd_out.status,
                stderr.trim()
            )));
        }
        Ok(out)
    }
}
