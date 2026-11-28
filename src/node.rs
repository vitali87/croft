//! Where Node.js is (#2809): one lookup that the language servers, the
//! JavaScript debugger, Prettier, vitest and jest, the TypeScript project
//! check and the ▷ on a JavaScript fence all share, and the `PATH` a Node
//! program croft starts runs with.
//!
//! Version managers (nvm, fnm, asdf, volta) keep node off croft's inherited
//! `PATH` (they add it only when a shell sources its init files). croft is
//! exec'd without a shell, so it finds node itself: first by reading the
//! well-known on-disk layouts, then by asking the user's shell in a detached
//! session that can't touch croft's terminal. A Node program croft starts,
//! often a `node_modules/.bin` script that begins `#!/usr/bin/env node`, gets
//! that node's folder at the front of its `PATH`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// The folder to put at the front of `PATH` so a program croft starts finds
/// `node` and `npm`, or `None` when they are on croft's `PATH` already
/// (nothing to add) or no node was found. A folder found is kept for the
/// process; a miss is looked for again next time, as [`NodeDirMemo`] says
/// (#2806).
///
/// The first miss on disk asks the login shell, which can take 15 s: call
/// this where a wait is fine, never from a key press or a frame
/// ([`known_dir`] is for those).
pub(crate) fn dir() -> Option<PathBuf> {
    if on_path() {
        return None;
    }
    FOUND.get(on_disk_node_dir, detached_shell_node_dir)
}

/// [`dir`] without asking the login shell: what the disk shows, or what the
/// shell said if something asked it before. For a check made where nothing
/// may wait.
pub(crate) fn known_dir() -> Option<PathBuf> {
    if on_path() {
        return None;
    }
    FOUND.peek(on_disk_node_dir)
}

/// The one memo [`dir`] and [`known_dir`] share.
static FOUND: NodeDirMemo = NodeDirMemo::new();

/// Whether `node` and `npm` are both on croft's own `PATH`.
fn on_path() -> bool {
    crate::proc::find::on_path("node").is_some() && crate::proc::find::on_path("npm").is_some()
}

/// True when croft can find a usable `node` + `npm`, either already on `PATH`
/// or in the folder [`dir`] found.
pub(crate) fn available() -> bool {
    on_path() || dir().is_some()
}

/// Whether croft knows a node to run, without asking the login shell: one on
/// its `PATH`, or [`known_dir`]'s.
pub(crate) fn known() -> bool {
    crate::proc::find::on_path("node").is_some() || known_dir().is_some()
}

/// `cmd`, a Node program, with `dir`, the folder of the node [`dir`] found,
/// at the front of its `PATH`; unchanged for `None` (node is on croft's
/// `PATH`, or none was found). A program named without a path is looked up
/// on that `PATH` too, so `npx` resolves to the one beside that node.
pub(crate) fn put<'a>(cmd: &'a mut Command, dir: Option<&Path>) -> &'a mut Command {
    if let Some(dir) = dir {
        cmd.env("PATH", path_with(dir));
    }
    cmd
}

/// croft's `PATH` with `dir` in front, so it wins over any stale entry.
pub(crate) fn path_with(dir: &Path) -> OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(std::env::split_paths(&current));
    std::env::join_paths(dirs).unwrap_or(current)
}

/// What [`dir`] keeps between calls: the node directory once found, and the
/// login shell's answer. Reading the on-disk layouts costs a few file
/// checks, so after a miss they are read again, and a node installed while
/// croft runs is found without a restart (#2806). Asking the shell can take
/// 15 s, so a miss there is its answer for the process; a folder it named
/// that has since gone (an upgrade removed that version) is asked about
/// again, as is a kept folder whose node has gone.
///
/// Its locks are held only to read or write what is kept, never while the
/// shell is asked: [`NodeDirMemo::peek`], for a check that may not wait,
/// never waits on a shell another thread is asking.
struct NodeDirMemo {
    found: Mutex<Option<PathBuf>>,
    shell: Mutex<Option<Option<PathBuf>>>,
    /// Held while the shell is asked, so two lookups ask it once.
    asking: Mutex<()>,
}

/// Whether `dir` still holds a node that can run.
fn has_node(dir: &Path) -> bool {
    crate::proc::find::runnable(&dir.join("node"))
}

/// `mutex` locked, whatever a thread that panicked holding it left.
fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl NodeDirMemo {
    const fn new() -> Self {
        Self {
            found: Mutex::new(None),
            shell: Mutex::new(None),
            asking: Mutex::new(()),
        }
    }

    /// The directory kept, while its node is still there.
    fn kept(&self) -> Option<PathBuf> {
        let mut found = locked(&self.found);
        if found.as_deref().is_some_and(|dir| !has_node(dir)) {
            *found = None;
        }
        found.clone()
    }

    /// Keep `dir`, the one found.
    fn keep(&self, dir: &Path) {
        *locked(&self.found) = Some(dir.to_path_buf());
    }

    /// The shell's answer, when it was asked and its answer still holds: a
    /// miss stays one, a folder whose node has gone is asked about again.
    fn shell_answer(&self) -> Option<Option<PathBuf>> {
        let mut shell = locked(&self.shell);
        if let Some(Some(dir)) = shell.as_ref()
            && !has_node(dir)
        {
            *shell = None;
        }
        shell.clone()
    }

    /// The node directory: the one kept, else `on_disk`'s, else the shell's.
    fn get(
        &self,
        on_disk: impl FnOnce() -> Option<PathBuf>,
        shell: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        if let Some(dir) = self.kept().or_else(on_disk) {
            self.keep(&dir);
            return Some(dir);
        }
        let _asking = locked(&self.asking);
        // Another lookup may have asked while this one waited.
        if let Some(dir) = self.kept() {
            return Some(dir);
        }
        let answer = match self.shell_answer() {
            Some(answer) => answer,
            None => {
                let answer = shell();
                *locked(&self.shell) = Some(answer.clone());
                answer
            }
        };
        if let Some(dir) = &answer {
            self.keep(dir);
        }
        answer
    }

    /// [`Self::get`] without asking the shell, and without waiting on a
    /// lookup that is: the directory kept, else `on_disk`'s, else what the
    /// shell said if it was asked before.
    fn peek(&self, on_disk: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
        let dir = self
            .kept()
            .or_else(on_disk)
            .or_else(|| self.shell_answer().flatten())?;
        self.keep(&dir);
        Some(dir)
    }
}

/// A directory with real `node` + `npm` binaries in a well-known on-disk
/// layout (nvm's versions dir, Volta, Homebrew), read with no process spawn.
/// When none has one, [`dir`] asks the user's own
/// login+interactive shell where `node` resolves, detached from croft's
/// controlling terminal so it can never do tty job control; that is what makes
/// fnm / asdf / volta-shim / custom setups work, not just the ones with a
/// guessable directory.
fn on_disk_node_dir() -> Option<PathBuf> {
    if let Some(dir) = nvm_node_dir() {
        return Some(dir);
    }
    well_known_node_dirs().into_iter().find(|dir| {
        crate::proc::find::runnable(&dir.join("node"))
            && crate::proc::find::runnable(&dir.join("npm"))
    })
}

/// Marker the probe wraps `process.execPath` in, so the real path is
/// recoverable even if the user's shell init prints noise to stdout first.
const NODE_PROBE_MARKER: &str = "__CROFT_NODE__";

/// Ask the user's login+interactive shell where `node` lives, detached from
/// croft's controlling terminal. Runs `node -e` (so it works whether `node` is
/// a real binary or a version-manager shell function) and reports the absolute
/// `process.execPath`. Returns its directory.
///
/// SAFETY / why detached: an interactive shell does terminal job control
/// (`tcsetpgrp`) on its controlling tty. Sharing croft's tty would background
/// croft and corrupt the terminal. `setsid` in `pre_exec` puts the child in a
/// brand-new session with NO controlling terminal, so it physically cannot
/// touch croft's tty; stdin is `/dev/null` and stdout is captured. A timeout
/// guards against a pathological init file that never returns.
fn detached_shell_node_dir() -> Option<PathBuf> {
    let dir = node_dir_from_probe(node_probe_command(), std::time::Duration::from_secs(15));
    if dir.is_none() {
        crate::lsp::log_file::log("node: the login shell named no node within 15s");
    }
    dir
}

/// The directory of the `node` that `probe`, a [`node_probe_command`],
/// reports. A probe still running after `timeout` is killed with everything
/// it started (#2755): its shell leads a session of its own, and an init
/// file that hangs would otherwise keep it, and what it waits on, running
/// for as long as croft does.
fn node_dir_from_probe(mut probe: Command, timeout: std::time::Duration) -> Option<PathBuf> {
    let out = crate::dap::install::run_probe(&mut probe, timeout, crate::proc::Apart::Session)?;
    let node = Path::new(parse_node_probe(&out)?);
    if !crate::proc::find::runnable(node) {
        return None;
    }
    node.parent().map(Path::to_path_buf)
}

/// The shell probe that asks where `node` lives: the user's login+interactive
/// shell runs `node -e` and prints `process.execPath` after
/// [`NODE_PROBE_MARKER`], detached from croft's terminal (see
/// [`detached_shell_node_dir`] for why). Its stdout is read with
/// [`parse_node_probe`]. Shared by the language-server install and the
/// JavaScript debugger, so neither can run the shell attached (#2064).
fn node_probe_command() -> Command {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
    let script =
        format!("node -e 'process.stdout.write(\"{NODE_PROBE_MARKER}\"+process.execPath)'");
    detached_shell(&shell, &script)
}

/// `shell -l -i -c script` with stdin `/dev/null`, stdout piped and stderr
/// discarded, started by `setsid` in a session of its own with no
/// controlling terminal, so the interactive shell's job control can never
/// reach croft's tty.
fn detached_shell(shell: &std::ffi::OsStr, script: &str) -> Command {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd
}

/// Recover the node path from probe stdout: the absolute path written right
/// after the last marker. Returns `None` if the marker is absent (the probe
/// didn't run node) or the text after it isn't an absolute path. Tolerates
/// arbitrary shell-init noise printed before the marker.
fn parse_node_probe(stdout: &str) -> Option<&str> {
    let after = &stdout[stdout.rfind(NODE_PROBE_MARKER)? + NODE_PROBE_MARKER.len()..];
    let path = after.trim();
    (path.starts_with('/') && !path.is_empty()).then_some(path)
}

/// Highest installed nvm node's `bin` dir: `$NVM_DIR/versions/node/vX.Y.Z/bin`
/// (defaulting `NVM_DIR` to `~/.nvm`). Picks the greatest semver so we get a
/// recent, working node; we only need *a* functional node to install/run vtsls,
/// not the user's exact `default` alias.
fn nvm_node_dir() -> Option<PathBuf> {
    let nvm = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".nvm")))?;
    nvm_node_dir_in(&nvm.join("versions").join("node"))
}

/// Pure core of [`nvm_node_dir`], split out so it's testable against a temp
/// directory without reading `$NVM_DIR`.
fn nvm_node_dir_in(versions: &Path) -> Option<PathBuf> {
    let mut best: Option<((u64, u64, u64), PathBuf)> = None;
    for entry in std::fs::read_dir(versions).ok()?.flatten() {
        let bin = entry.path().join("bin");
        if !crate::proc::find::runnable(&bin.join("node")) {
            continue;
        }
        let version = parse_node_version(&entry.file_name().to_string_lossy());
        if best.as_ref().is_none_or(|(b, _)| version > *b) {
            best = Some((version, bin));
        }
    }
    best.map(|(_, bin)| bin)
}

/// Parse an nvm version dir name like `v23.7.0` into a comparable tuple;
/// unparseable components become 0.
fn parse_node_version(name: &str) -> (u64, u64, u64) {
    let mut parts = name
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Static install locations for other managers, checked after nvm.
fn well_known_node_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".volta").join("bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// A folder under `root` named `name` with a node that can run.
    fn node_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("node"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(dir.join("node"), std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    /// #2806: a node found is kept; a miss is no answer for good. The
    /// on-disk layouts are read again next time, so a node installed since
    /// (nvm, Volta, Homebrew) is found without a restart, while the login
    /// shell, which can take 15 s, is asked only once.
    #[test]
    fn a_node_installed_after_a_miss_is_found_and_the_shell_is_asked_once() {
        let tmp = TempDir::new().unwrap();
        let memo = NodeDirMemo::new();
        let asked = std::cell::Cell::new(0);
        let shell = || {
            asked.set(asked.get() + 1);
            None
        };
        assert_eq!(memo.get(|| None, shell), None);
        let nvm = node_dir(tmp.path(), "nvm-v22.1.0");
        assert_eq!(memo.get(|| Some(nvm.clone()), shell), Some(nvm.clone()));
        assert_eq!(asked.get(), 1, "the shell was asked once");
        assert_eq!(
            memo.get(|| panic!("a found node is kept"), shell),
            Some(nvm),
        );
    }

    /// #2809 review: a kept folder whose node has gone, an nvm version an
    /// upgrade removed, is not answered for good: the disk is read again,
    /// and a folder the shell named that has gone is asked about again.
    #[test]
    fn a_node_that_has_gone_is_looked_for_again() {
        let tmp = TempDir::new().unwrap();
        let memo = NodeDirMemo::new();
        let old = node_dir(tmp.path(), "v22.1.0");
        assert_eq!(memo.get(|| None, || Some(old.clone())), Some(old.clone()));
        std::fs::remove_file(old.join("node")).unwrap();
        let new = node_dir(tmp.path(), "v22.2.0");
        assert_eq!(
            memo.get(|| None, || Some(new.clone())),
            Some(new.clone()),
            "the shell is asked again: the folder it named has gone"
        );
        std::fs::remove_file(new.join("node")).unwrap();
        let disk = node_dir(tmp.path(), "volta");
        assert_eq!(
            memo.peek(|| Some(disk.clone())),
            Some(disk),
            "a check that may not wait reads the disk again too"
        );
    }

    #[test]
    fn parse_node_version_orders_by_semver_not_lexically() {
        assert_eq!(parse_node_version("v23.7.0"), (23, 7, 0));
        assert_eq!(parse_node_version("v8.17.1"), (8, 17, 1));
        // The bug a lexical sort would hit: v8 must NOT outrank v18.
        assert!(parse_node_version("v18.0.0") > parse_node_version("v8.17.1"));
        assert_eq!(parse_node_version("garbage"), (0, 0, 0));
    }

    #[test]
    fn nvm_node_dir_picks_highest_version_with_a_node_binary() {
        let tmp = TempDir::new().unwrap();
        let versions = tmp.path();
        // Two versions with node, one stray dir without — highest wins.
        for v in ["v18.19.0", "v23.7.0"] {
            let bin = versions.join(v).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("node"), b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(bin.join("node"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        std::fs::create_dir_all(versions.join("v20.0.0")).unwrap(); // no bin/node
        let dir = nvm_node_dir_in(versions).unwrap();
        assert_eq!(dir, versions.join("v23.7.0").join("bin"));
    }

    #[test]
    fn nvm_node_dir_is_none_when_no_versions_have_node() {
        let tmp = TempDir::new().unwrap();
        assert!(nvm_node_dir_in(tmp.path()).is_none());
    }

    #[test]
    fn parse_node_probe_extracts_path_after_marker_ignoring_init_noise() {
        // Shell init may print noise before the marker; the path is what
        // follows the last marker.
        let stdout = "Welcome to your shell!\nnvm loaded\n__CROFT_NODE__/Users/x/.nvm/versions/node/v23.7.0/bin/node";
        assert_eq!(
            parse_node_probe(stdout),
            Some("/Users/x/.nvm/versions/node/v23.7.0/bin/node")
        );
    }

    #[test]
    fn a_probe_that_never_finishes_is_killed_with_what_it_started() {
        use std::os::unix::process::CommandExt;
        let tmp = tempfile::tempdir().unwrap();
        let pids = tmp.path().join("pids");
        // A shell in a session of its own, as the real probe runs, whose
        // init hangs on a command it started.
        let mut probe = Command::new("sh");
        probe
            .arg("-c")
            .arg(format!("sleep 30 & echo $$ $! > {}; wait", pids.display()));
        unsafe {
            probe.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        assert_eq!(
            node_dir_from_probe(probe, std::time::Duration::from_millis(300)),
            None
        );
        let pids: Vec<i32> = std::fs::read_to_string(&pids)
            .expect("the probe started")
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2, "the shell and what it waits on");
        // Killed and not yet reaped by whoever inherited it is gone too.
        let gone = |pid: i32| {
            let signalled = unsafe { libc::kill(pid, 0) };
            signalled != 0
                || std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                    stat.rsplit(')')
                        .next()
                        .is_some_and(|rest| rest.trim_start().starts_with('Z'))
                })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !pids.iter().all(|&p| gone(p)) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for pid in pids {
            assert!(
                gone(pid),
                "process {pid} still runs after the probe gave up"
            );
        }
    }

    #[test]
    fn parse_node_probe_rejects_missing_marker_or_non_absolute() {
        assert_eq!(parse_node_probe("no marker here"), None);
        assert_eq!(parse_node_probe("__CROFT_NODE__"), None);
        assert_eq!(parse_node_probe("__CROFT_NODE__relative/path"), None);
    }

    /// The node probe's shell leads a session of its own (#2064), so it has
    /// no controlling terminal and its job control cannot reach croft's.
    /// Read from the shell's own `/proc/<pid>/stat`: `pid (comm) state ppid
    /// pgrp session ...`.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_node_probe_shell_runs_in_a_session_of_its_own() {
        let home = tempfile::tempdir().unwrap();
        let out = detached_shell(
            std::ffi::OsStr::new("/bin/sh"),
            "printf __STAT__; cat /proc/$$/stat",
        )
        .env("HOME", home.path())
        .env_remove("ENV")
        .output()
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stat = &stdout[stdout.rfind("__STAT__").expect("the probe ran") + "__STAT__".len()..];
        let pid: i32 = stat.split_whitespace().next().unwrap().parse().unwrap();
        let after_comm: Vec<&str> = stat[stat.rfind(')').unwrap() + 1..]
            .split_whitespace()
            .collect();
        let session: i32 = after_comm[3].parse().unwrap();
        assert_eq!(session, pid, "the shell leads its own session: {stat}");
        let ours = unsafe { libc::getsid(0) };
        assert_ne!(session, ours, "not croft's session");
    }

    /// #2809: a node's folder goes at the front of a Node program's `PATH`,
    /// ahead of croft's own, and a command is left alone without one.
    #[test]
    fn a_node_program_gets_the_found_nodes_folder_first_on_its_path() {
        let dir = PathBuf::from("/home/u/.nvm/versions/node/v22.1.0/bin");
        let mut cmd = Command::new("prettier");
        put(&mut cmd, Some(&dir));
        let path = cmd
            .get_envs()
            .find(|(k, _)| *k == "PATH")
            .and_then(|(_, v)| v)
            .expect("PATH is set");
        assert_eq!(std::env::split_paths(path).next(), Some(dir));
        let mut bare = Command::new("prettier");
        put(&mut bare, None);
        assert_eq!(bare.get_envs().count(), 0, "no node found, nothing set");
    }

    /// A check that may not wait reads what the disk shows and what the
    /// shell already said, and never asks the shell itself.
    #[test]
    fn a_check_that_may_not_wait_never_asks_the_shell() {
        let tmp = TempDir::new().unwrap();
        let memo = NodeDirMemo::new();
        assert_eq!(memo.peek(|| None), None);
        let shell_dir = node_dir(tmp.path(), "fnm");
        assert_eq!(
            memo.get(|| None, || Some(shell_dir.clone())),
            Some(shell_dir.clone())
        );
        let fresh = NodeDirMemo::new();
        assert_eq!(fresh.get(|| None, || None), None);
        assert_eq!(fresh.peek(|| None), None, "the shell's miss stays a miss");
        let answered = NodeDirMemo::new();
        *locked(&answered.shell) = Some(Some(shell_dir.clone()));
        assert_eq!(
            answered.peek(|| None),
            Some(shell_dir),
            "the shell's earlier answer"
        );
    }

    /// #2809 review: a check that may not wait does not wait on a lookup
    /// that is asking the shell, which can take 15 s.
    #[test]
    fn a_check_that_may_not_wait_does_not_wait_on_the_shell() {
        let memo = std::sync::Arc::new(NodeDirMemo::new());
        let (asking_tx, asking_rx) = std::sync::mpsc::channel();
        let (answer_tx, answer_rx) = std::sync::mpsc::channel::<()>();
        let lookup = {
            let memo = std::sync::Arc::clone(&memo);
            std::thread::spawn(move || {
                memo.get(
                    || None,
                    || {
                        asking_tx.send(()).unwrap();
                        let _ = answer_rx.recv();
                        None
                    },
                )
            })
        };
        asking_rx.recv().unwrap();
        let started = std::time::Instant::now();
        assert_eq!(memo.peek(|| None), None);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "the check waited {:?} on the shell",
            started.elapsed()
        );
        answer_tx.send(()).unwrap();
        assert_eq!(lookup.join().unwrap(), None);
    }
}
