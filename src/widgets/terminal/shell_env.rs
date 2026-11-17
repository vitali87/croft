//! The environment a pane's shell starts in: shell integration for each shell, the terminal
//! and locale variables, and croft's helpers on PATH.

use super::*;

/// Inject croft's shell-integration environment for supported shells, and
/// return the shell's argv: `args` (its login flag) with whatever the
/// integration adds. Long options go first, because bash only recognises
/// them (`--posix`) ahead of single-character ones (`-l`).
/// zsh: point `ZDOTDIR` at croft's shim. bash: the kitty/Ghostty trick —
/// `--posix` plus `ENV` pointing at croft's shim (posix-mode bash reads
/// only `$ENV`; the shim backs out and replays the real startup files).
/// Below 4.4 bash ignores that `$ENV`, so it reads the shim through
/// `--rcfile`, non-login, and the shim replays the login files itself.
/// fish: prepend a `vendor_conf.d` data dir to `XDG_DATA_DIRS`; the script
/// defers to fish 4's native marks. Every shim sources the user's real
/// dotfiles unchanged and emits OSC 133 prompt marks + the OSC 7 cwd
/// report. Opt out with `CROFT_SHELL_INTEGRATION=0`. Failures are
/// non-fatal — the shell still spawns, just without marks.
pub(super) fn apply_shell_integration_env(
    cmd: &mut CommandBuilder,
    shell_path: &str,
    config_dir: &std::path::Path,
    args: Vec<String>,
) -> Vec<String> {
    if std::env::var("CROFT_SHELL_INTEGRATION").as_deref() == Ok("0") {
        return args;
    }
    let base = std::path::Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match base {
        "zsh" => {
            let Ok(shim) = crate::shell_integration::ensure_zsh_shim(config_dir) else {
                return args;
            };
            // An inherited ZDOTDIR pointing at croft's own shim (croft
            // launched from a croft pane) is poisoned — the user's dotfiles
            // live in HOME.
            let inherited = std::env::var_os("ZDOTDIR").map(std::path::PathBuf::from);
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            if home.as_os_str().is_empty() {
                return args;
            }
            let user_zdotdir = crate::shell_integration::resolve_user_zdotdir(
                inherited.as_deref(),
                &config_dir.join("shell-integration"),
                &home,
            );
            cmd.env("CROFT_USER_ZDOTDIR", user_zdotdir);
            cmd.env("ZDOTDIR", &shim);
            args
        }
        "bash" => {
            let Ok(shim) = crate::shell_integration::ensure_bash_shim(config_dir) else {
                return args;
            };
            // Preserve a user $ENV (rare for bash) for the shim to restore.
            if let Ok(env) = std::env::var("ENV") {
                cmd.env("CROFT_BASH_ENV", env);
            }
            cmd.env("CROFT_BASH_INJECT", "1");
            if !bash_env_injection_supported(shell_path) {
                // No `$ENV` in posix mode (macOS's 3.2), so the shim goes in
                // as the rcfile, as VS Code does it. A login bash ignores
                // `--rcfile`, so the shell starts non-login and the shim
                // replays the login files in its place.
                if args.iter().any(|a| a == "-l" || a == "--login") {
                    cmd.env("CROFT_BASH_LOGIN", "1");
                }
                return vec!["--rcfile".to_string(), shim.display().to_string()];
            }
            cmd.env("ENV", &shim);
            // Posix mode defaults HISTFILE to ~/.sh_history; pre-seed the
            // bash default and let the shim unexport it.
            if std::env::var_os("HISTFILE").is_none()
                && let Some(home) = std::env::var_os("HOME")
            {
                cmd.env(
                    "HISTFILE",
                    std::path::Path::new(&home).join(".bash_history"),
                );
                cmd.env("CROFT_BASH_UNEXPORT_HISTFILE", "1");
            }
            std::iter::once("--posix".to_string()).chain(args).collect()
        }
        "fish" => {
            let Ok(dir) = crate::shell_integration::ensure_fish_integration(config_dir) else {
                return args;
            };
            let inherited = std::env::var("XDG_DATA_DIRS").ok();
            cmd.env(
                "XDG_DATA_DIRS",
                crate::shell_integration::fish_xdg_data_dirs(&dir, inherited.as_deref()),
            );
            cmd.env("CROFT_FISH_XDG_DATA_DIR", &dir);
            args
        }
        _ => args,
    }
}

/// The command for an interactive pane shell: `shell_path` as a login
/// shell, with croft's shell integration.
pub(super) fn interactive_shell_command(
    shell_path: &str,
    config_dir: &std::path::Path,
) -> CommandBuilder {
    let (program, args) = interactive_shell_invocation(shell_path);
    let mut cmd = CommandBuilder::new(&program);
    for a in apply_shell_integration_env(&mut cmd, shell_path, config_dir, args) {
        cmd.arg(a);
    }
    cmd
}

/// Whether this bash reads `$ENV` in posix mode, i.e. the injection works
/// at all. Verified empirically: macOS's system bash 3.2 never reads $ENV
/// under `--posix`, so injecting would strip its startup files for
/// nothing; Homebrew/Linux 5.x does. 4.4 is the ecosystem floor (kitty,
/// Ghostty). One short probe subprocess per pane spawn — pane creation
/// already forks a shell, so this is noise.
pub(super) fn bash_env_injection_supported(shell_path: &str) -> bool {
    let Ok(out) = std::process::Command::new(shell_path)
        .args(["-c", "echo \"${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}\""])
        .output()
    else {
        return false;
    };
    parse_bash_version_supported(&String::from_utf8_lossy(&out.stdout))
}

/// `major.minor` >= 4.4, tolerant of anything a non-bash prints.
pub(super) fn parse_bash_version_supported(s: &str) -> bool {
    let mut it = s.trim().split('.');
    let (Some(maj), Some(min)) = (
        it.next().and_then(|v| v.parse::<u32>().ok()),
        it.next().and_then(|v| v.parse::<u32>().ok()),
    ) else {
        return false;
    };
    maj > 4 || (maj == 4 && min >= 4)
}

/// Pick the program + args to spawn the user's interactive shell so it
/// behaves like the one iTerm2 / Terminal.app launches. Both run the
/// shell as a *login* shell (`Login shell` is iTerm2's default Command
/// setting), which sources `~/.zprofile` / `~/.profile` in addition to
/// the interactive rc file. Without that, anything the user puts in
/// `.zprofile` — PATH munging, framework loaders, plugin managers like
/// zinit/oh-my-zsh that install keybindings, vi-mode toggles — is
/// silently absent inside croft. For mainstream POSIX shells we pass
/// `-l`; for anything exotic we leave argv empty rather than risk
/// breaking the spawn with an unknown flag.
pub fn interactive_shell_invocation(shell_path: &str) -> (String, Vec<String>) {
    let basename = std::path::Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(shell_path);
    let supports_login_flag = matches!(
        basename,
        "zsh" | "bash" | "fish" | "ksh" | "mksh" | "dash" | "tcsh" | "sh"
    );
    let args = if supports_login_flag {
        vec!["-l".to_string()]
    } else {
        Vec::new()
    };
    (shell_path.to_string(), args)
}

/// Parse `/etc/shells` into `(path, basename)` terminal profiles: skip comments
/// and blanks, keep absolute paths only, and dedupe by basename (first wins, so
/// `/bin/bash` shadows `/usr/local/bin/bash`). The basename is the label shown.
pub fn parse_shells(contents: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('/') {
            continue;
        }
        let base = std::path::Path::new(line)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(line)
            .to_string();
        if out.iter().any(|(_, b)| *b == base) {
            continue;
        }
        out.push((line.to_string(), base));
    }
    out
}

/// Croft's own environment for a pane's shell: the terminal identity every
/// pane needs, plus the `croft view` socket when one is bound (#362).
///
/// `view_sock` is passed IN rather than read from a global here, so this is a
/// pure function a test can assert against without touching process-wide
/// state; the spawn funnel supplies it from [`crate::view_ipc::SOCK_PATH`].
///
/// The socket travels this way rather than through the process environment
/// because it cannot travel that way at all. portable-pty snapshots
/// `std::env::vars_os()` inside `CommandBuilder::new` and then `env_clear()`s
/// before applying only that snapshot, so a `set_var` performed after a pane
/// was constructed never reaches its shell. The listener is bound while
/// `App::new` runs, which is after the FIRST pane exists, so publishing by
/// environment left `croft view` broken in the one pane most users type in
/// and working in every pane opened later: breakage that reads as
/// intermittent rather than as a bug.
///
/// A `OnceLock` read at spawn time has no ordering hazard to get wrong, and
/// no `unsafe` either: `std::env::set_var` is unsafe in edition 2024 precisely
/// because croft has threads running by then, which is the condition
/// `src/gui_path.rs` and `src/session.rs` both document for their own calls.
pub(crate) fn apply_pane_env(cmd: &mut CommandBuilder, view_sock: Option<&std::path::Path>) {
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // The pane is croft's emulator, not the host's (#1453). A program that
    // read Ghostty's `TERM_PROGRAM` here wrote Kitty graphics the pane drops,
    // so `croft plot` printed blank rows. VS Code does the same with
    // `TERM_PROGRAM=vscode`.
    cmd.env("TERM_PROGRAM", crate::iterm2_inline::CROFT_TERM_PROGRAM);
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    for host_only in HOST_TERMINAL_VARS {
        cmd.env_remove(host_only);
    }
    cmd.env(
        crate::iterm2_inline::CROFT_INLINE_IMAGES_ENV,
        if pane_images_shown() { "1" } else { "0" },
    );
    // A pane's shell must count characters the way the terminal draws them.
    // macOS forwards `LC_CTYPE=UTF-8` over ssh, which Linux does not know, so
    // a remote shell fell back to ASCII and counted a 3-byte prompt glyph
    // (`❯`, a Nerd Font icon) as 3 columns: every redraw landed 2 columns
    // off and left 2-character coloured tails behind (#537).
    if cfg!(target_os = "linux") {
        let var = |k: &str| std::env::var(k).ok();
        if let Some((key, value)) = utf8_ctype_fix(var("LC_ALL"), var("LC_CTYPE"), var("LANG")) {
            cmd.env(key, value);
        }
    }
    // `git rebase -i` opens its plan in this croft (#620), unless the user
    // chose their own sequence editor, which always wins: an exported
    // GIT_SEQUENCE_EDITOR here, or `sequence.editor` in git config, which
    // `croft edit --sequence-editor` looks up in the rebasing repo itself.
    if view_sock.is_some()
        && std::env::var_os("GIT_SEQUENCE_EDITOR").is_none()
        && let Some(exe) = std::env::current_exe().ok().and_then(live_exe)
    {
        cmd.env("GIT_SEQUENCE_EDITOR", sequence_editor_command(&exe));
    }
    match view_sock {
        Some(path) => cmd.env(crate::view_ipc::SOCK_ENV, path),
        // CLEARED, not merely not-added. `CommandBuilder::new` seeds itself
        // from `std::env::vars_os()`, so a croft launched from a croft pane
        // inherits the OUTER croft's socket. If this croft's own bind failed,
        // leaving that inherited value in place points its panes at the
        // parent: `croft view report.pdf` opens the file in the wrong window
        // and exits 0, which is worse than the honest refusal the field doc
        // promises ("panes then see no CROFT_VIEW_SOCK and the client says so
        // plainly").
        None => cmd.env_remove(crate::view_ipc::SOCK_ENV),
    }
}

/// Variables that name the HOST terminal (#1453). In a pane they would send
/// a program down a graphics or feature path croft's emulator does not relay.
pub(super) const HOST_TERMINAL_VARS: &[&str] = &[
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "ITERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "VTE_VERSION",
];

/// The locale variable to set so a Linux shell reads its text as UTF-8, or
/// None when the inherited locale already does (or the user pinned another
/// with `LC_ALL`, which wins over anything set here). The bare name `UTF-8`
/// is what macOS sends and no Linux locale is called that.
pub(super) fn utf8_ctype_fix(
    lc_all: Option<String>,
    lc_ctype: Option<String>,
    lang: Option<String>,
) -> Option<(&'static str, &'static str)> {
    let is_utf8 = |v: &str| {
        let codeset = v
            .split('@')
            .next()
            .unwrap_or(v)
            .rsplit('.')
            .next()
            .unwrap_or("");
        v.contains('.') && matches!(codeset.to_ascii_lowercase().as_str(), "utf-8" | "utf8")
    };
    let bare = |v: &str| v.eq_ignore_ascii_case("utf-8") || v.eq_ignore_ascii_case("utf8");
    match lc_all.filter(|v| !v.is_empty()) {
        Some(v) if bare(&v) => return Some(("LC_ALL", "C.UTF-8")),
        Some(_) => return None,
        None => {}
    }
    let effective = lc_ctype
        .filter(|v| !v.is_empty())
        .or(lang.filter(|v| !v.is_empty()));
    match effective {
        Some(v) if is_utf8(&v) => None,
        _ => Some(("LC_CTYPE", "C.UTF-8")),
    }
}

/// `exe` as a path that still runs. Once an update renames a new binary
/// over the running one, Linux reports it as `.../croft (deleted)`, which
/// no shell can start; the new binary at the same path can, and `croft
/// edit` is the same command in both.
pub(super) fn live_exe(exe: std::path::PathBuf) -> Option<std::path::PathBuf> {
    if exe.exists() {
        return Some(exe);
    }
    let replaced = std::path::PathBuf::from(exe.to_str()?.strip_suffix(" (deleted)")?);
    replaced.exists().then_some(replaced)
}

/// The `GIT_SEQUENCE_EDITOR` value naming this croft (#620). git runs it
/// through the shell, so the binary's path is single-quoted.
pub(crate) fn sequence_editor_command(exe: &std::path::Path) -> String {
    let quoted = exe.to_string_lossy().replace('\'', r"'\''");
    format!("'{quoted}' edit --wait --sequence-editor")
}

/// Put `bin` first on the pane's PATH, so `croft` in the pane is this croft
/// (#1854), and name it to the shell hooks. They put it first again once
/// the user's startup files have run, because those run after this: a
/// `.zprofile` running `brew shellenv` puts /opt/homebrew/bin, and a
/// Homebrew croft, back in front.
///
/// With no bin of its own, a pane drops the one it inherited. A croft
/// launched from a croft pane carries the OUTER croft's, and passing that
/// on would answer `croft` with a different croft than the window.
///
/// An unset PATH stays unset, for the shell to fill with its default: PATH
/// set to the bin alone would leave the pane without /usr/bin.
pub(crate) fn apply_pane_bin(cmd: &mut CommandBuilder, bin: Option<&std::path::Path>) {
    use crate::shell_integration::PANE_BIN_ENV;
    let inherited = cmd.get_env(PANE_BIN_ENV).map(std::path::PathBuf::from);
    let path = cmd.get_env("PATH").and_then(|path| {
        let rest = std::env::split_paths(path)
            .filter(|d| Some(d.as_path()) != bin && Some(d) != inherited.as_ref());
        std::env::join_paths(
            bin.map(std::path::Path::to_path_buf)
                .into_iter()
                .chain(rest),
        )
        .ok()
    });
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    match bin {
        Some(bin) => cmd.env(PANE_BIN_ENV, bin),
        None => cmd.env_remove(PANE_BIN_ENV),
    }
}

/// This croft's pane bin, made on first use. None when the running binary
/// is gone from disk (an upgrade removed it) or the cache cannot be
/// written; the pane then finds `croft` on the user's PATH, as before.
pub(super) fn pane_bin_dir() -> Option<std::path::PathBuf> {
    let exe = live_exe(std::env::current_exe().ok()?)?;
    crate::shell_integration::ensure_pane_bin(&crate::paths::croft_cache_dir(), &exe).ok()
}
