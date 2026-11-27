//! Finding each file's server: its language and project root, the server's config, and
//! the executable on PATH, in a toolchain's folders or through rustup, and the note shown
//! once when a server is missing.

use super::*;

/// The language server language for `path`: its `files_associations` id
/// (#1574), else what its name and extension map to (#1084). The app's
/// sync uses it to decide which tabs go to the servers at all.
pub(crate) fn path_to_language(path: &Path) -> Option<Language> {
    // `files_associations` first (#1574), so the server serves the
    // language the editor highlights: a known id with no server gets none.
    // An id croft does not know falls through to the name and extension,
    // as it does for highlighting.
    if let Some(id) = crate::highlight::associated_language_id(path) {
        let lang = crate::lsp::languages::resolve(&id);
        if lang.is_some()
            || id == "plaintext"
            || crate::highlight::lang_for_language_id(&id).is_some()
        {
            return lang;
        }
    }
    Language::from_path(path)
}

/// Project-manifest filenames whose presence marks a directory as the root of a
/// `lang` project. Used to anchor the language server at the right sub-project
/// (so basedpyright finds that project's `.venv`, rust-analyzer its `Cargo.toml`,
/// etc.). An empty slice means "no per-project rooting" → use the workspace root.
pub(super) fn manifest_names(lang: Language) -> &'static [&'static str] {
    lang.root_markers()
}

/// [`project_root_within`] with the multi-root bound (#147): the walk stays
/// inside the workspace root OWNING the file (longest prefix over all
/// roots), and the fallback is that owning root — so a file under a
/// secondary root roots its servers there, never at the primary. A file
/// outside every root keeps the historical primary-root fallback.
pub(super) fn project_root_for_roots(
    path: &Path,
    lang: Language,
    primary: &Path,
    extra_roots: &[PathBuf],
) -> PathBuf {
    let owner = std::iter::once(primary)
        .chain(extra_roots.iter().map(PathBuf::as_path))
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.components().count())
        .unwrap_or(primary);
    project_root_within(path, lang, owner)
}

/// The project root a file's language server should be anchored at: the nearest
/// ancestor holding a language manifest (e.g. `pyproject.toml`, `Cargo.toml`,
/// `go.mod`, `package.json`), bounded by the workspace root `bound`. Falls back
/// to `bound` when none is found, so a plain single-project workspace behaves
/// exactly as before.
pub(super) fn project_root_within(path: &Path, lang: Language, bound: &Path) -> PathBuf {
    let manifests = manifest_names(lang);
    if !manifests.is_empty() {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !d.starts_with(bound) {
                break;
            }
            if manifests.iter().any(|m| d.join(m).exists()) {
                return d.to_path_buf();
            }
            if d == bound {
                break;
            }
            dir = d.parent();
        }
    }
    bound.to_path_buf()
}

/// Walk `$PATH` looking for an executable file named `cmd`. Avoids
/// invoking the binary with `--help` or `--version` because LSP servers
/// are JSON-RPC daemons with no consistent CLI flags (basedpyright-
/// langserver exits 1 on --help and crashes on --version, rustup shims
/// pretend to exist even when their component isn't installed).
/// Resolve a server config to a spawnable command, or `None` to skip it.
/// A server carrying a `provision` is handled by croft's managed installer
/// (resolved to an absolute path under `~/.croft/servers`; a lazy background
/// install is kicked off when it's absent). Every other server is resolved
/// against PATH unchanged. `log_skip` gates the "not available" log so empty
/// re-probes don't spam the log on every request.
pub(super) fn resolve_config(
    config: &ServerConfig,
    log_skip: bool,
) -> Option<(ServerConfig, Vec<PathBuf>)> {
    if let Some(provision) = &config.provision {
        // Native-binary servers (rust-analyzer, clangd, taplo) are toolchain
        // tools: a real copy on PATH or in a toolchain dir (~/.cargo/bin) must
        // win over a croft-downloaded one so it matches the user's compiler, and
        // for rust-analyzer `resolve_path_only` prepends that dir to the child
        // PATH so the server can find cargo/rustc under a stripped GUI launch.
        // Only when no real binary exists anywhere do we fall back to a managed
        // download / Termux `pkg` install (`resolve_for_root` also does when the
        // binary is a rustup proxy with no component behind it). npm/uv servers
        // keep their own managed-first (vtsls) or PATH-first (ty/ruff) ordering
        // inside `resolve_managed`, so this preference is scoped to the Binary
        // and Go backends (a gopls in ~/go/bin is the user's own, #930).
        if matches!(
            provision,
            crate::lsp::install::Provision::Binary { .. }
                | crate::lsp::install::Provision::Go { .. }
        ) && let Some(resolved) = resolve_path_only(
            config,
            is_on_path(&config.command),
            &toolchain_fallback_dirs(),
        ) {
            return Some(resolved);
        }
        return crate::lsp::install::resolve_managed(config, provision, log_skip);
    }
    if let Some(resolved) = resolve_path_only(
        config,
        is_on_path(&config.command),
        &toolchain_fallback_dirs(),
    ) {
        return Some(resolved);
    }
    if log_skip {
        log_file::log(&format!(
            "lsp[{}] skip: `{}` not on PATH",
            config.name, config.command
        ));
    }
    None
}

/// What to tell the user when `lang` is left with no language server (#930):
/// every server it registers is missing from PATH and croft cannot install
/// any of them, because it has no provision or `installer_missing` names
/// the tool its install needs. `None` when the language registers no server,
/// or one of them can still be installed (that install reports its own
/// progress and failures).
pub(super) fn missing_servers_note(
    lang: Language,
    configs: &[ServerConfig],
    installer_missing: impl Fn(&crate::lsp::install::Provision) -> Option<&'static str>,
) -> Option<String> {
    if configs.is_empty() {
        return None;
    }
    let mut why = Vec::with_capacity(configs.len());
    for config in configs {
        match config.provision.as_ref().map(&installer_missing) {
            None => why.push(format!("{} isn't on PATH", config.command)),
            Some(Some(tool)) => why.push(format!(
                "{} isn't on PATH, and installing it needs {tool}",
                config.command
            )),
            Some(None) => return None,
        }
    }
    let label = crate::highlight::lang_for_language_id(lang.lsp_id())
        .map_or(lang.lsp_id(), |kind| {
            crate::highlight::language_label(Some(kind))
        });
    Some(format!(
        "{label}: no language server, so no diagnostics or completion ({})",
        why.join("; ")
    ))
}

/// Show `note` in the status bar and OUTPUT › Server Provisioning the first
/// time `lang` is found without a server this session; later files of that
/// language, and other project roots, stay quiet.
pub(super) fn report_missing_servers_once(lang: Language, note: &str) {
    static REPORTED: StdMutex<std::collections::BTreeSet<&'static str>> =
        StdMutex::new(std::collections::BTreeSet::new());
    let first = REPORTED
        .lock()
        .map(|mut seen| seen.insert(lang.lsp_id()))
        .unwrap_or(false);
    if !first {
        return;
    }
    log_file::log(&format!("lsp[{}] {note}", lang.lsp_id()));
    crate::output::push(
        crate::output::CHANNEL_PROVISION,
        crate::output::OutputLevel::Warn,
        note,
    );
    crate::lsp::install::set_status(note);
}

/// Whether `cmd` is a program on croft's PATH, by [`crate::proc::find`]'s
/// rule.
pub(crate) fn is_on_path(cmd: &str) -> bool {
    crate::proc::find::on_path(cmd).is_some()
}

/// Toolchain `bin` dirs that a macOS GUI launch (Finder / Dock / the Croft.app
/// bundle, which `open`s Ghostty under the stripped launchd PATH) drops because
/// the login-shell profile is never sourced. rust-analyzer and gopls live here
/// but resolve by bare name, so a GUI-launched croft can't find them even
/// though a terminal launch (full PATH) can. Probed only when the command is
/// already absent from the inherited PATH, so a normal launch never reaches it,
/// which keeps local and remote behaviour identical.
pub(super) fn toolchain_fallback_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".cargo").join("bin"));
        dirs.push(home.join("go").join("bin"));
        dirs.push(home.join(".local").join("bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// Resolve a PATH-only server (no managed provision) to a spawnable
/// `(config, extra_path)` pair, or `None` to skip it. When `on_path` the
/// bare command spawns as-is; otherwise the toolchain `fallback_dirs` are
/// probed and a hit is pinned to its absolute path (with its dir prepended to
/// the child's PATH) so a stripped GUI PATH can't hide it.
pub(super) fn resolve_path_only(
    config: &ServerConfig,
    on_path: bool,
    fallback_dirs: &[PathBuf],
) -> Option<(ServerConfig, Vec<PathBuf>)> {
    if on_path {
        return Some((config.clone(), Vec::new()));
    }
    let abs = crate::proc::find::first_in(&config.command, fallback_dirs)?;
    let extra = abs.parent().map(Path::to_path_buf).into_iter().collect();
    let mut resolved = config.clone();
    resolved.command = abs.to_string_lossy().into_owned();
    Some((resolved, extra))
}

/// [`resolve_config`] for the servers of the project at `root` (#927). A
/// native server found on PATH or in `~/.cargo/bin` that is a rustup proxy is
/// only a server when the toolchain `root` selects has the component behind
/// it. rustup puts a `rust-analyzer` proxy there for every toolchain, but its
/// default profile installs no `rust-analyzer`, and that proxy only prints
/// "Unknown binary" and exits, so croft's managed copy serves instead.
pub(super) async fn resolve_for_root(
    config: &ServerConfig,
    root: &Path,
    log_skip: bool,
) -> Option<(ServerConfig, Vec<PathBuf>)> {
    let found = resolve_config(config, log_skip)?;
    let Some(provision @ crate::lsp::install::Provision::Binary { bin, .. }) = &config.provision
    else {
        return Some(found);
    };
    let Some(rustup) = command_path(&found.0.command).and_then(|p| rustup_behind(&p)) else {
        return Some(found);
    };
    let key = (config.name, root.to_path_buf());
    let known = RUSTUP_VERDICTS
        .lock()
        .ok()
        .and_then(|verdicts| verdicts.get(&key).cloned())
        .filter(|(asked, _)| asked.elapsed() < RUSTUP_RECHECK);
    let lacks = match known {
        Some((_, lacks)) => lacks,
        None => {
            let lacks = rustup_lacks(&rustup, bin, root).await;
            if let Ok(mut verdicts) = RUSTUP_VERDICTS.lock() {
                verdicts.insert(key, (std::time::Instant::now(), lacks.clone()));
            }
            lacks
        }
    };
    let Some(RustupLack { why, fix }) = lacks else {
        return Some(found);
    };
    if log_skip {
        let msg = format!(
            "{why} (at {}); falling back to croft's managed copy",
            root.display()
        );
        log_file::log(&format!("lsp[{}] {msg}", config.name));
        crate::output::push(
            crate::output::CHANNEL_PROVISION,
            crate::output::OutputLevel::Info,
            &format!("{}: {msg}", config.name),
        );
    }
    crate::lsp::install::resolve_managed_instead(config, provision, &why, &fix)
}

/// What [`rustup_lacks`] answered per (server, project root), and when, so
/// the re-probe each request makes while croft's copy downloads does not ask
/// rustup again. Each verdict is asked again after [`RUSTUP_RECHECK`]: `None`
/// also stands for a rustup that could not say in time, and a server that
/// runs is never re-probed, so only a question still open is asked again.
pub(super) static RUSTUP_VERDICTS: StdMutex<RustupVerdicts> =
    StdMutex::new(std::collections::BTreeMap::new());

/// [`RUSTUP_VERDICTS`]' map: (server, project root) to when rustup was
/// asked and what it said.
pub(super) type RustupVerdicts =
    std::collections::BTreeMap<(&'static str, PathBuf), (std::time::Instant, Option<RustupLack>)>;

/// How long a verdict holds before rustup is asked again: the status line
/// names `rustup component add`, and once the user has run it, rustup's own
/// server takes over without a restart (#927); a rustup too slow to answer
/// once is asked again rather than trusted for good.
pub(super) const RUSTUP_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// How long rustup gets to answer before croft stops asking and keeps the
/// proxy, as it did before it asked.
pub(super) const RUSTUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// The file a resolved server `command` runs: itself when absolute, else
/// the first executable of that name on PATH.
pub(super) fn command_path(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    let dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH")?).collect();
    crate::proc::find::first_in(command, &dirs)
}

/// The `rustup` that `candidate` is a proxy for, or `None` when it is a
/// program of its own. rustup installs its proxies as hard links to itself,
/// or symlinks where it cannot, so a proxy is the same file as the `rustup`
/// beside it, or beside wherever its links lead.
pub(super) fn rustup_behind(candidate: &Path) -> Option<PathBuf> {
    let id = file_id(candidate)?;
    let rustup = format!("rustup{}", std::env::consts::EXE_SUFFIX);
    std::iter::once(candidate.to_path_buf())
        .chain(std::fs::canonicalize(candidate).ok())
        .map(|p| p.with_file_name(&rustup))
        .find(|p| file_id(p) == Some(id))
}

/// What identifies the file `path` leads to, links followed.
#[cfg(unix)]
pub(super) fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
pub(super) fn file_id(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// Why the proxy `rustup` puts in front of `bin` cannot start it for the
/// project at `root`, and the `rustup component add` that mends it, named
/// with `--toolchain` so it holds from any directory; `None` when the proxy
/// can start it or rustup cannot say. Asked from
/// `root`, since that picks the toolchain (`rust-toolchain.toml`, a directory
/// override). `RUSTUP_AUTO_INSTALL=0` stops the question from installing a
/// toolchain. A toolchain that is not installed at all is left to the proxy,
/// which installs it as before, and so is a rustup too old to honour that
/// variable that is still busy at [`RUSTUP_DEADLINE`].
pub(super) async fn rustup_lacks(rustup: &Path, bin: &str, root: &Path) -> Option<RustupLack> {
    if rustup_says(rustup, root, &["which", bin])
        .await?
        .status
        .success()
    {
        return None;
    }
    let active = rustup_says(rustup, root, &["show", "active-toolchain"]).await?;
    if !active.status.success() {
        return None;
    }
    let toolchain = String::from_utf8_lossy(&active.stdout)
        .split_whitespace()
        .next()?
        .to_owned();
    // Named the way users name it: `stable`, not `stable-<host triple>`.
    let triple = format!("-{}-", std::env::consts::ARCH);
    let channel = toolchain
        .split_once(&triple)
        .map_or(&*toolchain, |(c, _)| c);
    Some(RustupLack {
        why: format!("rustup's '{channel}' toolchain has no {bin}"),
        fix: format!("rustup component add {bin} --toolchain {channel}"),
    })
}

/// `rustup` run with `args` from `root`, or `None` when it could not be run
/// or did not finish within [`RUSTUP_DEADLINE`] (it is killed then).
pub(super) async fn rustup_says(
    rustup: &Path,
    root: &Path,
    args: &[&str],
) -> Option<std::process::Output> {
    let run = tokio::process::Command::new(rustup)
        .args(args)
        .current_dir(root)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(RUSTUP_DEADLINE, run).await.ok()?.ok()
}
