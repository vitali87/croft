//! Running what was parsed: `Cli::run` hands each subcommand to its code, or opens the
//! workspace; and a closed stdout ends a command that only prints quietly.

use super::*;

impl Cli {
    pub fn run(self) -> Result<()> {
        // `--build-info` is a pure query: answer and exit before any setup,
        // exactly as clap does for `--version`.
        if self.build_info {
            println!(
                "croft {}",
                build_info_line(crate::update_check::current_install_source())
            );
            return Ok(());
        }
        // Pure liveness probes answer before anything else runs: the remote
        // launch script fires them over SSH on every connect, and a remote
        // host handing SSH sessions a stripped PATH (macOS does) must not
        // buy them a login-shell probe - remote attach never waits.
        if matches!(
            self.command,
            Some(CliCommand::SessionHost { probe: true, .. })
                | Some(CliCommand::CollabRelay { probe: true, .. })
        ) {
            return Ok(());
        }
        // macOS launchd starts croft with a 256-fd soft limit; the editor's
        // PTYs/watchers/LSPs and especially the spawned cross-link (~250
        // rlibs open at once) need far more. Children inherit the raise.
        crate::remote::raise_fd_limit();
        // ...and a PATH with nothing but the four system directories in it,
        // so every tool croft shells out to (pdftoppm, git, ripgrep, the
        // language servers) would be invisible. Children inherit the repair.
        crate::gui_path::repair();
        crate::startup_trace::mark("PATH ready");
        match self.command {
            Some(CliCommand::SetupTerminal { font, size, yes }) => setup_terminal(&font, size, yes),
            Some(CliCommand::Keys { mouse }) => keys_diagnostic(mouse),
            Some(CliCommand::SetupIterm2 {
                font,
                nonascii,
                size,
                yes,
            }) => setup_iterm2(&font, &nonascii, size, yes),
            Some(CliCommand::Remote {
                host,
                path,
                solo,
                build,
            }) => {
                crate::remote::FORCE_SOURCE_BUILD
                    .store(build, std::sync::atomic::Ordering::Relaxed);
                match crate::remote::launch_croft(&host, path.as_deref(), solo)? {
                    crate::remote::RemoteOutcome::ReturnToLocal => {
                        let cwd = std::env::current_dir().context("resolving workspace path")?;
                        crate::app::run(cwd, None, None, None, false, Vec::new())
                    }
                    crate::remote::RemoteOutcome::Exited => Ok(()),
                }
            }
            Some(CliCommand::Attach { path, solo }) => crate::session_cli::attach(path, solo),
            Some(CliCommand::Record { path, out, stop }) => {
                crate::session_record::record(path, out, stop)
            }
            Some(CliCommand::Ls) => crate::session_cli::list(),
            Some(CliCommand::Plot {
                kind,
                x,
                y,
                title,
                width,
                height,
                text,
            }) => plot(
                &kind,
                x.as_deref(),
                &y,
                title.as_deref(),
                width,
                height,
                text,
            ),
            Some(CliCommand::PaneHost {
                socket,
                size,
                cwd,
                env,
                argv,
            }) => {
                let (cols, rows) = crate::pane_host::parse_size(&size)
                    .with_context(|| format!("--size {size:?} is not COLSxROWS"))?;
                let env = env
                    .iter()
                    .map(|e| {
                        crate::pane_host::parse_env(e)
                            .with_context(|| format!("--env {e:?} is not KEY=VALUE"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let code = crate::pane_host::serve(&crate::pane_host::PaneSpec {
                    socket,
                    cwd,
                    env,
                    cols,
                    rows,
                    argv,
                })?;
                if code != 0 {
                    std::process::exit(code);
                }
                Ok(())
            }
            Some(CliCommand::SessionHost {
                probe,
                serve,
                socket,
                workspace,
                inner,
            }) => {
                if probe {
                    return Ok(());
                }
                let socket = socket.context("session-host requires --socket")?;
                let code = if serve {
                    crate::session_host::serve(&socket, workspace.as_deref(), &inner)?
                } else {
                    crate::session_host::attach_or_create(&socket, workspace.as_deref(), &inner)?
                };
                // Propagate the inner croft's exit code (e.g. the remote
                // wrapper's drop-to-local 88) through the mux boundary.
                if code != 0 {
                    std::process::exit(code);
                }
                Ok(())
            }
            Some(CliCommand::CollabRelay {
                probe,
                ensure,
                socket,
            }) => {
                if probe {
                    return Ok(());
                }
                let socket = socket.context("collab-relay requires --socket")?;
                if ensure {
                    crate::collab::ensure_relay(&socket)
                } else {
                    crate::collab::relay_serve(&socket)
                }
            }
            Some(CliCommand::CollabAgent { workspace, name }) => {
                let workspace = workspace
                    .canonicalize()
                    .with_context(|| format!("workspace not found: {}", workspace.display()))?;
                let socket = crate::session::collab_socket_path(&workspace);
                if let Some(dir) = socket.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                crate::collab::ensure_relay(&socket)?;
                crate::collab_agent::run(
                    &socket,
                    &workspace,
                    name.unwrap_or_else(|| "claude".into()),
                )
            }
            Some(CliCommand::Pr { number }) => {
                let Some(selector) = crate::pr_review::parse_pr_selector(&number) else {
                    eprintln!("croft pr: {number:?} is not a pull request number or URL");
                    std::process::exit(2);
                };
                // Before the editor takes the terminal (#917): without a
                // usable gh the review can never load, and the fix is typed
                // at this prompt.
                if let Err(why) = crate::gh::check_ready("gh") {
                    eprintln!("croft pr: {why}");
                    std::process::exit(1);
                }
                crate::pr_review::set_startup_pr(selector);
                let cwd = std::env::current_dir()?;
                crate::app::run(cwd, None, None, None, false, Vec::new())
            }
            Some(CliCommand::Demo { tour }) => {
                if let Some(path) = tour {
                    let json = std::fs::read_to_string(&path)
                        .with_context(|| format!("reading {}", path.display()))?;
                    if let Err(e) = crate::tour::Tour::parse(&json) {
                        eprintln!("croft demo: {} is not a tour: {e}", path.display());
                        std::process::exit(2);
                    }
                    crate::tour::set_custom_tour(json);
                }
                crate::tour::request_startup_demo();
                let cwd = std::env::current_dir()?;
                crate::app::run(cwd, None, None, None, false, Vec::new())
            }
            Some(CliCommand::View { path, as_ext }) => {
                // Printed and exited here rather than returned: an `Err` out
                // of `run` reaches anyhow's `Debug`, which appends a full
                // Rust backtrace whenever RUST_BACKTRACE is set, and it is
                // set in this repo's CI, where the one-line-of-stderr test
                // measured 42. Someone running `croft view` at a prompt
                // should get a sentence about their file, not croft's stack.
                if let Err(e) = crate::view_ipc::run(
                    crate::view_ipc::Verb::View,
                    &path,
                    as_ext.as_deref(),
                    &crate::paths::croft_cache_dir(),
                ) {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
                Ok(())
            }
            Some(CliCommand::Web {
                workspace,
                bind,
                tls,
                rotate_token,
                off,
            }) => {
                let workspace = match workspace {
                    Some(w) => w,
                    None => std::env::current_dir()?,
                };
                let opts = crate::web::Options {
                    bind,
                    tls: tls.map(|mut pair| {
                        let key = pair.pop().unwrap_or_default();
                        (pair.pop().unwrap_or_default(), key)
                    }),
                    rotate_token,
                    off,
                };
                if let Err(e) = crate::web::run(&workspace, opts) {
                    eprintln!("croft web: {e:#}");
                    std::process::exit(1);
                }
                Ok(())
            }
            Some(CliCommand::Edit {
                path,
                wait,
                sequence_editor,
            }) => {
                if sequence_editor && let Some(configured) = configured_sequence_editor() {
                    std::process::exit(run_git_editor(&configured, &path));
                }
                match crate::view_ipc::edit(&path, wait, &crate::paths::croft_cache_dir()) {
                    Ok(()) => Ok(()),
                    // A shell that outlived its croft (a tmux server started
                    // from a pane) keeps GIT_SEQUENCE_EDITOR: git's rebase
                    // then needs an editor, not croft (#1452). A plain
                    // `croft edit` asked for croft, so it still fails.
                    Err(e)
                        if sequence_editor
                            && e.downcast_ref::<crate::view_ipc::NoCroft>().is_some() =>
                    {
                        let Some(editor) = git_editor() else {
                            eprintln!("{e}");
                            std::process::exit(1);
                        };
                        eprintln!("croft is not running; using {editor}");
                        std::process::exit(run_git_editor(&editor, &path));
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            }
            Some(CliCommand::Hook { action }) => {
                // Printed and exited, like `view`: a one-line reason, never
                // anyhow's backtrace.
                if let Err(e) = hook_command(action) {
                    eprintln!("croft hook: {e}");
                    std::process::exit(1);
                }
                Ok(())
            }
            Some(CliCommand::LocaleTemplate { lang, write }) => {
                // The language croft loads for this locale: `de_DE.UTF-8` and
                // `de-DE` both read `locales/de.json`, whose built-in catalog
                // seeds the template.
                let Some(code) = crate::i18n::language_of(&lang) else {
                    eprintln!(
                        "croft locale-template: {lang} names no language croft translates to (English is built in)"
                    );
                    std::process::exit(1);
                };
                let path = crate::paths::config_dir()
                    .join("locales")
                    .join(format!("{code}.json"));
                if write {
                    match crate::i18n::write_template(&path, &code) {
                        Ok((total, todo)) => {
                            eprintln!(
                                "Updated {}: {todo} of {total} strings still to translate.",
                                path.display()
                            );
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!("croft locale-template: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                // A shell redirect onto this same file has already emptied
                // it, which is why `--write` exists.
                eprintln!(
                    "Run `croft locale-template {code} --write` to update locales/{code}.json in croft's config directory in place."
                );
                let user = std::fs::read_to_string(&path).ok();
                println!(
                    "{}",
                    crate::i18n::template(&code, user.as_deref(), crate::i18n::TRANSLATABLE)
                );
                Ok(())
            }
            Some(CliCommand::Devcontainer { path, rebuild }) => {
                let root = path.unwrap_or_else(|| std::path::PathBuf::from("."));
                crate::devcontainer::up_and_attach(&root, rebuild)
            }
            Some(CliCommand::Pair {
                workspace,
                model,
                name,
                task,
                provider,
                base_url,
                off,
                repl,
            }) => {
                let workspace = workspace
                    .unwrap_or_else(|| std::env::current_dir().expect("cwd"))
                    .canonicalize()
                    .context("resolving workspace path")?;
                let record_path = crate::session::pair_record_path(&workspace);
                if let Some(dir) = record_path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                // The persisted record is the baseline; explicit flags
                // override it. Resolving bare flags in isolation made
                // `--provider ollama --off` clobber a custom endpoint with
                // the default one, and a plain `croft pair` re-activation
                // silently converted a disabled ollama record into a cloud
                // claude seat.
                let current = crate::session::read_pair_record(&record_path);
                // The caret name belongs to the seat, not to a backend, so
                // it is inherited even across a provider switch; only an
                // explicit --name changes it (#1907).
                let name = name
                    .or_else(|| current.as_ref().map(|r| r.name.clone()))
                    .unwrap_or_else(|| "claude".into());
                let recorded = current.as_ref().and_then(|r| r.provider.clone());
                // An explicit provider SWITCH starts that backend fresh:
                // inheriting the other backend's endpoint or model would
                // mis-configure the new seat (a claude record persists
                // provider None, so compare normalised names).
                let normal = |p: &Option<String>| {
                    p.as_deref()
                        .map(str::to_owned)
                        .unwrap_or_else(|| String::from("claude"))
                };
                let inherit = provider.is_none() || normal(&provider) == normal(&recorded);
                let model = model.or_else(|| {
                    inherit
                        .then(|| current.as_ref().and_then(|r| r.model.clone()))
                        .flatten()
                });
                let base_url = base_url.or_else(|| {
                    inherit
                        .then(|| current.as_ref().and_then(|r| r.base_url.clone()))
                        .flatten()
                });
                let provider = provider.or(recorded);
                let (provider, base_url) = resolve_pair_provider(provider, base_url)?;
                if off {
                    crate::session::write_pair_record(
                        &record_path,
                        &crate::session::PairRecord {
                            model,
                            name,
                            enabled: false,
                            task: None,
                            provider,
                            base_url,
                        },
                    )?;
                    println!("navigator deactivated for {}", workspace.display());
                    return Ok(());
                }
                anyhow::ensure!(
                    provider.as_deref() != Some("ollama") || model.is_some(),
                    "--provider ollama needs --model <name> (e.g. --model qwen3-coder:30b); \
                     a local endpoint has no CLI default"
                );
                if repl {
                    let socket = crate::session::collab_socket_path(&workspace);
                    crate::collab::ensure_relay(&socket)?;
                    return crate::pair::run(crate::pair::PairConfig {
                        socket,
                        workspace,
                        name,
                        model,
                        task,
                        provider: crate::pair::Provider::from_record(
                            provider.as_deref(),
                            base_url.as_deref(),
                        ),
                    });
                }
                crate::session::write_pair_record(
                    &record_path,
                    &crate::session::PairRecord {
                        model,
                        name: name.clone(),
                        enabled: true,
                        // Never persisted: the resident navigator takes its
                        // instructions in-editor (Cmd+K Q). A persisted task
                        // would re-fire on every launch.
                        task: None,
                        provider,
                        base_url,
                    },
                )?;
                // The relay is up before croft looks, so the seat connects
                // on the first tick that notices the record.
                let socket = crate::session::collab_socket_path(&workspace);
                crate::collab::ensure_relay(&socket)?;
                println!(
                    "navigator '{name}' active for {}; a running croft seats it \
                     within a second (croft pair --off deactivates)",
                    workspace.display()
                );
                Ok(())
            }
            Some(CliCommand::SetupCross { yes }) => setup_cross(yes),
            Some(CliCommand::ImportVscode { from, dry_run }) => {
                end_quietly_when_stdout_closes();
                import_vscode(from, dry_run)
            }
            Some(CliCommand::ThemeImport {
                file,
                theme,
                id,
                dry_run,
            }) => {
                end_quietly_when_stdout_closes();
                theme_import(&file, theme.as_deref(), id.as_deref(), dry_run)
            }
            Some(CliCommand::SetupGhostty { yes }) => setup_ghostty(yes),
            Some(CliCommand::OpenLink { url }) => {
                // One line, like `croft view`: a link click deserves a
                // sentence about the link, not croft's stack.
                if let Err(e) = open_link(&url) {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
                Ok(())
            }
            Some(CliCommand::Decide {
                token,
                decision,
                path,
            }) => {
                let cwd = match path {
                    Some(p) => p,
                    None => std::env::current_dir()?,
                };
                let decision = crate::agent_approval::RemoteDecision {
                    token,
                    allow: decision == "allow",
                };
                match crate::agent_approval::send_decision(&cwd, &decision) {
                    Ok(message) => {
                        println!("{message}");
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            }
            Some(CliCommand::SyncConfig {
                host,
                diff,
                take_local,
                keep_remote,
            }) => {
                let resolution = crate::remote::SyncResolution {
                    take_local,
                    keep_remote,
                };
                if let Err(e) = crate::remote::sync_config_now(&host, &resolution, diff.as_deref())
                {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
                Ok(())
            }
            Some(CliCommand::InstallLinkHandler) => install_link_handler(),
            Some(CliCommand::InstallLauncher { path, user, yes }) => {
                install_launcher(path, user, yes)
            }
            None => {
                let path = self
                    .path
                    .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
                let cwd = std::env::current_dir().context("reading the current directory")?;
                let (path, open_file, open_at) = split_launch_location(&path, self.open_file, &cwd);
                let (path, open_file, folders) = resolve_workspace(&path, open_file)?;
                crate::app::run(
                    path,
                    self.restore_session,
                    open_file,
                    open_at,
                    self.zen,
                    folders,
                )
            }
        }
    }
}

/// Let a closed stdout end this process the way it ends `ls | head`, for a
/// command that only prints after doing its work (#1179). Rust ignores
/// SIGPIPE, which turns the first `println!` after the reader has gone into
/// a "Broken pipe" panic and exit code 101. Never for croft itself or its
/// session processes: they write to sockets and ptys whose peer can vanish
/// and rely on that write failing instead of killing them.
pub(super) fn end_quietly_when_stdout_closes() {
    // SAFETY: setting SIGPIPE's disposition to its default installs no
    // handler; the call has no memory-safety preconditions.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
}
