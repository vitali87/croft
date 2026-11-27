//! debugpy environment provisioning.
//!
//! croft owns a dedicated debug virtualenv at `~/.croft/debug-venv` rather than
//! installing debugpy into the user's interpreter: PEP 668 marks the uv-managed
//! CPython externally-managed (pip refuses), and polluting the user's Python
//! would be wrong regardless. Mirrors the `~/.croft/servers` LSP store. The venv
//! is built from CPython 3.14+ (`uv venv -p 3.14`), with no fallback to older
//! interpreters; it hosts the debugpy adapter, while a launched program runs
//! under the project's own interpreter (#864). The 3.14 floor on the program
//! itself is attach's (PEP 768), not launch's.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::dap::remote_attach::PyVersion;

/// Minimum CPython line croft debugs. `uv` resolves the newest matching.
const PYTHON_VERSION: &str = "3.14";

/// Pinned vscode-js-debug release. The `js-debug-dap-vX.Y.Z.tar.gz` asset is the
/// bundled standalone debug server (the same artifact Zed and nvim/Mason use);
/// it extracts to `js-debug/src/dapDebugServer.js`. Bump deliberately, and
/// [`JS_DEBUG_SHA256`] with it.
const JS_DEBUG_VERSION: &str = "v1.117.0";

/// SHA-256 of the [`JS_DEBUG_VERSION`] release's `js-debug-dap-*.tar.gz` asset.
/// The download is checked against it before a byte is unpacked: an altered
/// asset or a tampered delivery path is refused, never installed and run.
const JS_DEBUG_SHA256: &str = "ad8d04ede9d4b75cc290fd5438a65047a06f786d04f604b6112485b36f090772";

/// Cap on the js-debug tarball download (the v1.117 asset is ~1.2 MB; this is a
/// generous ceiling that still refuses a runaway/redirected body).
const MAX_JS_DEBUG_BYTES: u64 = 64 * 1024 * 1024;

/// Oldest CPython a launched program may run under: debugpy 1.8 declares
/// `Requires-Python >=3.10`, and the code it injects into the program's
/// interpreter does not load on an older one (#864). Attach's own floor is
/// [`crate::dap::remote_attach::MIN_ATTACH_VERSION`].
pub const DEBUGPY_MIN_PYTHON: PyVersion = PyVersion {
    major: 3,
    minor: 10,
    patch: 0,
};

/// The `pyvenv.cfg` of the venv `python` belongs to (`<venv>/bin/python`),
/// when there is one.
pub fn pyvenv_cfg(python: &Path) -> Option<PathBuf> {
    let cfg = python.parent()?.parent()?.join("pyvenv.cfg");
    cfg.is_file().then_some(cfg)
}

/// The version of the interpreter at `python` (#864): from its venv's
/// `pyvenv.cfg` when it has one (`version =` from `python -m venv`,
/// `version_info =` from uv and virtualenv), else by running it once. None
/// when neither says.
pub fn python_version(python: &Path) -> Option<PyVersion> {
    let from_cfg = pyvenv_cfg(python)
        .and_then(|cfg| std::fs::read_to_string(cfg).ok())
        .and_then(|text| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                matches!(key.trim(), "version" | "version_info").then(|| PyVersion::parse(value))?
            })
        });
    if from_cfg.is_some() {
        return from_cfg;
    }
    let out = run_probe(
        Command::new(python).args(["-c", "import sys; print('%d.%d' % sys.version_info[:2])"]),
        Duration::from_secs(5),
        crate::proc::Apart::No,
    )?;
    PyVersion::parse(&out)
}

/// Ensure the debug venv exists with debugpy, creating it via `uv` on first use.
/// Returns the venv interpreter path. Blocking: the first call shells out to
/// `uv venv` + `uv pip install debugpy` (a few seconds); subsequent calls are a
/// cheap existence check.
pub fn ensure_debug_venv() -> Result<PathBuf> {
    let home =
        std::env::var_os("HOME").context("$HOME unset; cannot locate ~/.croft/debug-venv")?;
    ensure_debug_venv_in(Path::new(&home), std::env::var_os("PATH").as_deref())
}

/// [`ensure_debug_venv`] under the home folder `home`, with `path_var` as
/// `PATH`: the venv is `~/.croft/debug-venv`, and its interpreter runs
/// `-m debugpy.adapter` only. The program being debugged runs under the
/// project's own interpreter (#864): debugpy injects itself there, so
/// neither debugpy nor 3.14 is required of it. The venv is built with the
/// uv the language servers use (#2804): on PATH, else croft's own copy or
/// one where uv's installer puts it.
fn ensure_debug_venv_in(home: &Path, path_var: Option<&std::ffi::OsStr>) -> Result<PathBuf> {
    let dir = home.join(".croft").join("debug-venv");
    let py = dir.join("bin").join("python");
    if py.exists() {
        return Ok(py);
    }
    let uv = crate::lsp::install::uv_program_on(Some(home), path_var)
        .context("uv is not installed; install it (https://docs.astral.sh/uv) to debug Python")?;

    // .output(), never .status(): uv paints progress bars on stderr, and an
    // inherited TTY would spray them over the running TUI (same class as the
    // pdftoppm trailer-dictionary spray).
    let venv = Command::new(&uv)
        .args(["venv", "-p", PYTHON_VERSION])
        .arg(&dir)
        .output()
        .context("running `uv venv`")?;
    if !venv.status.success() {
        bail!(
            "`uv venv -p {PYTHON_VERSION}` failed (is CPython {PYTHON_VERSION} available?): {}",
            String::from_utf8_lossy(&venv.stderr).trim()
        );
    }

    let pip = Command::new(&uv)
        .args(["pip", "install", "--python"])
        .arg(&py)
        .arg("debugpy")
        .output()
        .context("running `uv pip install debugpy`")?;
    if !pip.status.success() {
        bail!(
            "`uv pip install debugpy` failed: {}",
            String::from_utf8_lossy(&pip.stderr).trim()
        );
    }
    Ok(py)
}

/// `~/.croft/js-debug`, the install root for the vscode-js-debug server.
pub(crate) fn js_debug_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".croft").join("js-debug"))
}

/// Path to the extracted `dapDebugServer.js` (the standalone DAP server).
fn js_debug_server_path(dir: &Path) -> PathBuf {
    dir.join("js-debug").join("src").join("dapDebugServer.js")
}

/// Resolve the `node` runtime croft launches the js-debug server (and the
/// debuggee) with, returning an absolute path or the bare `node` command.
///
/// Version managers (nvm, fnm, asdf, volta) expose node only inside an
/// interactive login shell, and nvm *lazy-loads* it, so node's directory is not
/// on the inherited PATH and is not even on a captured login-shell PATH until
/// node first runs. So when the plain PATH scan misses, croft asks the user's
/// login+interactive shell to run node and print its own path — which triggers
/// the lazy loader. This mirrors how Zed resolves the shell environment for
/// GUI/launcher-started processes. Errors with an actionable hint when node
/// genuinely cannot be found.
pub fn node_program() -> Result<String> {
    if which("node") {
        return Ok(String::from("node"));
    }
    if let Some(path) = resolve_node_via_login_shell() {
        return Ok(path);
    }
    bail!("`node` not found on PATH; install Node.js to debug JavaScript/TypeScript")
}

/// The directory containing the resolved node binary, when it is an absolute
/// path. Prepended to the js-debug server's PATH so the *debuggee* node process
/// js-debug spawns (with a bare `node`) resolves to the same runtime.
pub fn node_bin_dir(node: &str) -> Option<PathBuf> {
    let p = Path::new(node);
    p.is_absolute().then(|| p.parent().map(Path::to_path_buf))?
}

/// Ask the user's login+interactive shell to print node's absolute path by
/// running it (`process.execPath`). `-i` is required because nvm defines its
/// lazy `node` function in the interactive rc; the call is bounded so a
/// misbehaving rc cannot hang croft. The shell runs detached from croft's
/// terminal and marks the path, through the same probe the language-server
/// install uses (#2064): an attached interactive shell can take croft's
/// terminal, and a line printed by the rc would read as the path.
fn resolve_node_via_login_shell() -> Option<String> {
    let out = run_probe(
        &mut crate::lsp::install::node_probe_command(),
        Duration::from_secs(8),
        crate::proc::Apart::Session,
    )?;
    let path = crate::lsp::install::parse_node_probe(&out)?;
    Path::new(path).is_file().then(|| path.to_string())
}

/// Run a probe, `cmd`, to its exit or `timeout`: a program asked one
/// question whose answer it prints last, such as a login shell asked where
/// `node` is (a shell with a chatty or blocking rc must never wedge croft).
/// Returns the end of its stdout, decoded lossily so one stray byte from an
/// rc cannot hide the rest, or `None` when it could not start or outlived
/// `timeout`.
///
/// Its stdout is read as it runs, so an rc that prints more than a pipe
/// holds cannot block the probe, and only the last [`PROBE_TAIL`] bytes are
/// kept, where the answer is. One that runs in a session of its own
/// ([`crate::proc::Apart::Session`]), as the node probe does
/// ([`crate::lsp::install::node_probe_command`]), is killed with
/// everything it started once croft is done with it, at its exit or at
/// `timeout`, so whatever its rc left running goes with it (#2755).
pub(crate) fn run_probe(
    cmd: &mut Command,
    timeout: Duration,
    apart: crate::proc::Apart,
) -> Option<String> {
    // A program written a moment ago (a test's stand-in interpreter) is "Text
    // file busy" while another thread's forked child still holds its write
    // descriptor; that read as "no version" instead of waiting it out.
    let ran = crate::proc::Bounded {
        stdout: crate::proc::Keep::Tail(PROBE_TAIL),
        stderr: crate::proc::Keep::Nothing,
        apart,
        limit: timeout,
        retry_busy: true,
    }
    .run(cmd)
    .ok()?;
    Some(ran.stdout.text())
}

/// How much of a probe's stdout [`run_probe`] keeps: its last 64 KiB, which
/// hold its answer after any rc chatter before it.
pub(crate) const PROBE_TAIL: usize = 64 * 1024;

/// Whether `bin` resolves on PATH (a `command -v` check via the shell-less
/// `which`-style scan of PATH entries).
fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

/// Ensure the vscode-js-debug server is installed, downloading + extracting the
/// pinned release tarball on first use. Returns the path to `dapDebugServer.js`.
/// Blocking: the first call downloads ~1.2 MB and shells out to `tar`; later
/// calls are a cheap existence check. Mirrors [`ensure_debug_venv`].
pub fn ensure_js_debug() -> Result<PathBuf> {
    let dir = js_debug_dir().context("$HOME unset; cannot locate ~/.croft/js-debug")?;
    let server = js_debug_server_path(&dir);
    if server.exists() {
        return Ok(server);
    }
    std::fs::create_dir_all(&dir).context("creating ~/.croft/js-debug")?;

    let url = format!(
        "https://github.com/microsoft/vscode-js-debug/releases/download/{JS_DEBUG_VERSION}/js-debug-dap-{JS_DEBUG_VERSION}.tar.gz"
    );
    let tarball = dir.join("js-debug-dap.tar.gz");
    download_to_file(&url, &tarball)?;

    // The asset extracts a top-level `js-debug/` directory; -C lands it under
    // ~/.croft/js-debug so the server is at js-debug/src/dapDebugServer.js.
    let tar = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&dir)
        .output()
        .context("running `tar -xzf` on the js-debug release")?;
    let _ = std::fs::remove_file(&tarball);
    if !tar.status.success() {
        bail!(
            "extracting the js-debug release failed: {}",
            String::from_utf8_lossy(&tar.stderr).trim()
        );
    }
    if !server.exists() {
        bail!("js-debug release did not contain dapDebugServer.js (layout changed?)");
    }
    Ok(server)
}

/// Download `url` to `dest`, refusing a body over [`MAX_JS_DEBUG_BYTES`] and
/// one that is not the pinned release. Follows redirects (the GitHub release
/// URL 302s to a CDN).
fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    let resp = crate::net::get(url)
        .call()
        .context("downloading js-debug release")?;
    let bytes = crate::net::read_capped(resp.into_reader(), MAX_JS_DEBUG_BYTES)
        .context("reading js-debug release body")?;
    verify_js_debug(&bytes)?;
    std::fs::write(dest, &bytes).context("writing js-debug release tarball")?;
    Ok(())
}

/// Accept a js-debug download only when it is the pinned release byte for
/// byte ([`JS_DEBUG_SHA256`]).
fn verify_js_debug(bytes: &[u8]) -> Result<()> {
    if crate::net::sha256_matches(bytes, JS_DEBUG_SHA256) {
        return Ok(());
    }
    bail!("the js-debug {JS_DEBUG_VERSION} download failed its checksum; refusing to install it")
}

/// Where `dlv` is, for Go debugging (#264), in delve's own resolution order:
/// `PATH`, then `$GOBIN`, then each `bin` of the folders `$GOPATH` lists
/// (default `~/go/bin`), then croft's servers directory. Pure over its
/// inputs so the order is testable.
pub fn find_dlv(
    path_env: Option<&std::ffi::OsStr>,
    gobin: Option<&Path>,
    gopath: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = path_env
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    dirs.extend(gobin.map(Path::to_path_buf));
    // `go` reads GOPATH as a list and refuses an entry that is not
    // absolute (#2805); such an entry, read here, would be a folder of
    // croft's own.
    match gopath {
        Some(gp) => dirs.extend(
            std::env::split_paths(gp)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("bin")),
        ),
        None => dirs.extend(home.map(|h| h.join("go").join("bin"))),
    }
    dirs.extend(home.map(|h| h.join(".croft").join("servers").join("go")));
    dirs.into_iter()
        .map(|d| d.join("dlv"))
        .find(|p| p.is_file())
}

/// The command "Debug: Install Go Debugger (delve)" runs, visibly, in a
/// terminal pane (#264). `GOBIN` puts `dlv` in croft's servers directory,
/// the last place [`find_dlv`] looks, so it never shadows a delve the user
/// installed themselves.
///
/// The delve it installs is one that debugs with the Go on this machine:
/// delve refuses a Go older than it supports (1.27 wants Go 1.25 or later),
/// so Go 1.24 and older get delve 1.25.2, the last release that takes them,
/// and anything newer gets `@latest`. Wrapped in `sh -c`, since it is typed
/// into whatever shell the pane runs, fish included.
pub const DLV_INSTALL: &str = "sh -c 'case \"$(go env GOVERSION)\" in go1.[0-9]|go1.[0-9].*|go1.1[0-9]*|go1.2[0-4]*) v=v1.25.2 ;; *) v=latest ;; esac; GOBIN=\"$HOME/.croft/servers/go\" go install github.com/go-delve/delve/cmd/dlv@$v'";

/// Whether the `go` toolchain is on PATH: installing delve needs it.
pub fn go_on_path() -> bool {
    which("go")
}

/// Why Go debugging cannot start, and the next step. Without Go there is
/// nothing to install delve with, which is a different fix.
pub fn dlv_missing_message(go_available: bool) -> String {
    if go_available {
        String::from(
            "Go debugging needs delve: run Debug: Install Go Debugger (delve) to `go install` it",
        )
    } else {
        String::from(
            "Go debugging needs delve, and installing delve needs Go: install Go from https://go.dev/dl, then run Debug: Install Go Debugger (delve)",
        )
    }
}

/// `dlv` for this machine, or an error that says how to install it.
pub fn dlv_program() -> Result<PathBuf> {
    let gobin = std::env::var_os("GOBIN").map(PathBuf::from);
    let gopath = std::env::var_os("GOPATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_dlv(
        std::env::var_os("PATH").as_deref(),
        gobin.as_deref(),
        gopath.as_deref(),
        home.as_deref(),
    )
    .ok_or_else(|| anyhow::anyhow!(dlv_missing_message(go_on_path())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The js-debug pin is a full SHA-256, and a body that is not the pinned
    /// release (here a stand-in, an empty body, and a valid gzip header) is
    /// refused before anything is written or unpacked.
    #[test]
    fn a_js_debug_download_that_is_not_the_pinned_release_is_refused() {
        assert_eq!(JS_DEBUG_SHA256.len(), 64);
        assert!(
            JS_DEBUG_SHA256
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{JS_DEBUG_SHA256}"
        );
        for body in [&b"not the release"[..], b"", &[0x1f, 0x8b, 0x08, 0x00]] {
            let why = verify_js_debug(body).unwrap_err().to_string();
            assert!(why.contains("failed its checksum"), "{why}");
        }
    }

    /// The pinned js-debug release still hashes to [`JS_DEBUG_SHA256`].
    /// `#[ignore]`d because it hits the network; run it when bumping
    /// [`JS_DEBUG_VERSION`]: `cargo test --bin croft -- --ignored pinned_js_debug`.
    #[test]
    #[ignore = "downloads the js-debug release from GitHub"]
    fn the_pinned_js_debug_release_matches_its_checksum() {
        let url = format!(
            "https://github.com/microsoft/vscode-js-debug/releases/download/{JS_DEBUG_VERSION}/js-debug-dap-{JS_DEBUG_VERSION}.tar.gz"
        );
        let resp = crate::net::get(&url).call().expect("download js-debug");
        let bytes = crate::net::read_capped(resp.into_reader(), MAX_JS_DEBUG_BYTES).unwrap();
        verify_js_debug(&bytes).expect("the pinned digest matches the release");
    }

    /// #264: the install picks the delve that debugs with this machine's
    /// Go: 1.25.2 for Go 1.24 and older (delve 1.27 refuses them), latest
    /// otherwise. Run as the pane would, through `sh`, against a stub `go`.
    #[cfg(unix)]
    #[test]
    fn the_delve_install_follows_the_local_go_version() {
        use std::os::unix::fs::PermissionsExt;
        for (goversion, want) in [
            ("go1.24.7", "dlv@v1.25.2"),
            ("go1.22rc1", "dlv@v1.25.2"),
            ("go1.9", "dlv@v1.25.2"),
            ("go1.25.0", "dlv@latest"),
            ("go1.26.1", "dlv@latest"),
            ("go1.30", "dlv@latest"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let log = tmp.path().join("go.log");
            let go = tmp.path().join("go");
            std::fs::write(
                &go,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = env ]; then echo {goversion}; exit 0; fi\necho \"GOBIN=$GOBIN $*\" > \"{}\"\n",
                    log.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&go, std::fs::Permissions::from_mode(0o755)).unwrap();
            let ok = std::process::Command::new("sh")
                .arg("-c")
                .arg(DLV_INSTALL)
                .env("PATH", format!("{}:/usr/bin:/bin", tmp.path().display()))
                .env("HOME", tmp.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "{goversion}");
            let ran = std::fs::read_to_string(&log).unwrap();
            assert!(ran.trim_end().ends_with(want), "{goversion}: {ran}");
            assert!(
                ran.starts_with(&format!(
                    "GOBIN={}/.croft/servers/go install ",
                    tmp.path().display()
                )),
                "{ran}"
            );
        }
    }

    /// A login shell with a chatty rc (a `fastfetch` banner, `set -x`
    /// tracing) can emit more than the OS pipe buffer. The child then blocks
    /// in write() and never exits, so waiting for exit before reading
    /// deadlocks until the timeout and reports node as missing. Output must
    /// be drained while the child runs.
    #[test]
    fn a_probe_survives_output_larger_than_the_pipe_buffer() {
        let out = run_probe(
            Command::new("/bin/sh").args(["-c", "yes croft | head -c 200000; echo answer"]),
            Duration::from_secs(3),
            crate::proc::Apart::No,
        )
        .expect("the probe answers");
        assert_eq!(out.len(), PROBE_TAIL, "only the end is kept");
        assert!(out.ends_with("answer\n"), "{:?}", &out[out.len() - 20..]);
    }

    #[test]
    fn node_bin_dir_is_the_parent_of_an_absolute_node_path() {
        assert_eq!(
            node_bin_dir("/Users/x/.nvm/versions/node/v23.7.0/bin/node"),
            Some(PathBuf::from("/Users/x/.nvm/versions/node/v23.7.0/bin"))
        );
    }

    #[test]
    fn node_bin_dir_is_none_for_a_bare_command() {
        // A bare `node` (already on PATH) has no directory to inject.
        assert_eq!(node_bin_dir("node"), None);
    }

    #[test]
    fn a_missing_delve_says_whether_go_is_there_to_install_it() {
        let with_go = dlv_missing_message(true);
        assert!(with_go.contains("Debug: Install Go Debugger (delve)"));
        assert!(!with_go.contains("go.dev"));
        let without = dlv_missing_message(false);
        assert!(without.contains("installing delve needs Go"));
        assert!(without.contains("https://go.dev/dl"));
        // The install lands where find_dlv looks last.
        assert!(DLV_INSTALL.contains("GOBIN=\"$HOME/.croft/servers/go\" go install "));
    }

    /// #264: delve's own resolution order, PATH first; `~/go/bin` stands in
    /// for an unset GOPATH, and croft's servers dir comes last.
    #[test]
    fn find_dlv_searches_path_then_gobin_then_gopath_then_croft() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = |rel: &str| {
            let dir = tmp.path().join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("dlv"), "").unwrap();
            dir
        };
        let on_path = mk("path");
        let gobin = mk("gobin");
        let home = tmp.path().join("home");
        let home_go = mk("home/go/bin");
        let croft = mk("home/.croft/servers/go");
        let path_env = std::env::join_paths([on_path.clone()]).unwrap();
        assert_eq!(
            find_dlv(Some(&path_env), Some(&gobin), None, Some(&home)),
            Some(on_path.join("dlv"))
        );
        assert_eq!(
            find_dlv(None, Some(&gobin), None, Some(&home)),
            Some(gobin.join("dlv"))
        );
        assert_eq!(
            find_dlv(None, None, None, Some(&home)),
            Some(home_go.join("dlv"))
        );
        std::fs::remove_file(home_go.join("dlv")).unwrap();
        assert_eq!(
            find_dlv(None, None, None, Some(&home)),
            Some(croft.join("dlv"))
        );
        assert_eq!(find_dlv(None, None, None, None), None, "nowhere to look");
    }

    /// #2804: the debug venv is built with the uv croft finds for the
    /// language servers, its own copy among them, not only a uv on PATH:
    /// with croft's copy in its servers folder and none on PATH, the venv
    /// is made and debugpy installed with that copy.
    #[cfg(unix)]
    #[test]
    fn the_debug_venv_is_built_with_crofts_own_uv() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let dist = home.path().join(".croft/servers/uv-dist");
        std::fs::create_dir_all(&dist).unwrap();
        let calls = home.path().join("uv-calls");
        let uv = dist.join("uv");
        std::fs::write(
            &uv,
            format!(
                "#!/bin/sh\necho \"$1 $2\" >> '{}'\n\
                 if [ \"$1\" = venv ]; then mkdir -p \"$4/bin\" && : > \"$4/bin/python\"; fi\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let empty = tempfile::tempdir().unwrap();
        let path_var = std::env::join_paths([empty.path()]).unwrap();
        let py = ensure_debug_venv_in(home.path(), Some(&path_var)).unwrap();
        assert_eq!(py, home.path().join(".croft/debug-venv/bin/python"));
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            "venv -p\npip install\n"
        );
    }

    /// #2805: GOPATH may list several folders, as `go` reads it; delve is
    /// looked for in each one's `bin`, in order, not in a folder named after
    /// the whole list. An empty or relative entry, which `go` refuses, is
    /// passed over rather than read against croft's own folder.
    #[test]
    fn find_dlv_reads_each_folder_a_gopath_lists() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        for dir in [&first, &second] {
            std::fs::create_dir_all(dir.join("bin")).unwrap();
        }
        std::fs::write(second.join("bin").join("dlv"), "").unwrap();
        let gopath = std::env::join_paths([
            "",
            "relative",
            first.to_str().unwrap(),
            second.to_str().unwrap(),
        ])
        .unwrap();
        assert_eq!(
            find_dlv(None, None, Some(&gopath), None),
            Some(second.join("bin").join("dlv"))
        );
        std::fs::write(first.join("bin").join("dlv"), "").unwrap();
        assert_eq!(
            find_dlv(None, None, Some(&gopath), None),
            Some(first.join("bin").join("dlv")),
            "the first folder listed wins"
        );
    }

    /// A stand-in interpreter at `<dir>/<venv>/bin/python`: a shell script
    /// running `body`, with a `pyvenv.cfg` holding `cfg` beside `bin` when
    /// one is given.
    #[cfg(unix)]
    fn stub_python(dir: &Path, venv: &str, body: &str, cfg: Option<&str>) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join(venv).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let py = bin.join("python");
        std::fs::write(&py, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&py, std::fs::Permissions::from_mode(0o755)).unwrap();
        if let Some(cfg) = cfg {
            std::fs::write(dir.join(venv).join("pyvenv.cfg"), cfg).unwrap();
        }
        py
    }

    /// #864: a venv's version is read from its `pyvenv.cfg`, in both
    /// spellings (`version =` from `python -m venv`, `version_info =` from
    /// uv and virtualenv), without running an interpreter that would fail.
    #[cfg(unix)]
    #[test]
    fn python_version_reads_a_venvs_pyvenv_cfg_without_running_it() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = stub_python(
            tmp.path(),
            ".venv",
            "exit 1",
            Some("home = /usr/bin\ninclude-system-site-packages = false\nversion = 3.12.4\n"),
        );
        assert_eq!(
            python_version(&venv).map(|v| (v.major, v.minor)),
            Some((3, 12))
        );
        let uv = stub_python(
            tmp.path(),
            "uv-venv",
            "exit 1",
            Some("home = /opt/py/bin\nimplementation = CPython\nversion_info = 3.14.0rc2\n"),
        );
        assert_eq!(
            python_version(&uv).map(|v| (v.major, v.minor)),
            Some((3, 14))
        );
    }

    /// #864: an interpreter outside a venv (a system `python3`) is asked
    /// once for its version.
    #[cfg(unix)]
    #[test]
    fn python_version_asks_an_interpreter_with_no_pyvenv_cfg() {
        let tmp = tempfile::tempdir().unwrap();
        let py = stub_python(tmp.path(), "usr", "echo 3.11", None);
        assert_eq!(
            python_version(&py).map(|v| (v.major, v.minor)),
            Some((3, 11))
        );
    }

    /// #864 guard: no version is made up. A missing interpreter, one that
    /// prints something else, and a `pyvenv.cfg` without a version (with an
    /// interpreter that says nothing) all read as unknown.
    #[cfg(unix)]
    #[test]
    fn python_version_is_none_when_nothing_says() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(python_version(&tmp.path().join("nope/bin/python")), None);
        let chatty = stub_python(tmp.path(), "chatty", "echo Python", None);
        assert_eq!(python_version(&chatty), None);
        let bare_cfg = stub_python(
            tmp.path(),
            ".venv",
            "exit 0",
            Some("home = /usr/bin\ninclude-system-site-packages = false\n"),
        );
        assert_eq!(python_version(&bare_cfg), None);
    }
}
