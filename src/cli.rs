use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

const SETUP_FONT_PS_NAME: &str = "MesloLGSNFM-Regular";
const SETUP_FONT_SIZE: u32 = 13;

const ITERM2_FONT_PS_NAME: &str = "MesloLGSNFM-Regular";
const ITERM2_NONASCII_PS_NAME: &str = "SymbolsNFM";
const ITERM2_FONT_SIZE: u32 = 13;

#[derive(Parser, Debug)]
#[command(name = "croft", version, about = "Terminal-based VS Code replica")]
pub struct Cli {
    /// Workspace folder to open (defaults to current directory)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Open this file in the editor on launch (the workspace stays rooted at
    /// PATH). Used by "Move/Copy into New Window", and handy on its own:
    /// `croft ~/project --open-file ~/project/src/main.rs`.
    #[arg(long, value_name = "FILE")]
    pub open_file: Option<PathBuf>,

    /// Start focused on the editor: hide the Explorer sidebar and the terminal
    /// so just the file fills the window (toggle them back with Cmd/Ctrl+B and
    /// Cmd/Ctrl+J). "Move/Copy into New Window" launches the new window this way.
    #[arg(long, default_value_t = false)]
    pub zen: bool,

    /// Internal: restore tabs/layout from a session file written just
    /// before a self-update re-exec. Not intended for manual use.
    #[arg(long, hide = true)]
    pub restore_session: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Set macOS Terminal.app's default profile font to a Nerd Font.
    SetupTerminal {
        /// PostScript name of the font (read from the .ttf with `fontTools` or `fc-scan`)
        #[arg(long, default_value = SETUP_FONT_PS_NAME)]
        font: String,
        #[arg(long, default_value_t = SETUP_FONT_SIZE)]
        size: u32,
        /// Skip confirmation prompt
        #[arg(short, long, default_value_t = false)]
        yes: bool,
    },
    /// Diagnostic: print every key event the terminal delivers, with modifiers.
    /// Useful for confirming whether cmd / super reaches the app.  Press Ctrl+C to quit.
    Keys {
        /// Also enable mouse capture and print every mouse event (Down / Drag / Up / Move /
        /// Scroll). Use over SSH to confirm that drag events make it from the local terminal
        /// to the remote croft when editor selection misbehaves.
        #[arg(long)]
        mouse: bool,
    },
    /// Configure iTerm2 for Croft: fonts plus the Cmd+Shift+F search shortcut.
    SetupIterm2 {
        /// PostScript name of the primary font
        #[arg(long, default_value = ITERM2_FONT_PS_NAME)]
        font: String,
        /// PostScript name of the non-ASCII fallback font
        #[arg(long, default_value = ITERM2_NONASCII_PS_NAME)]
        nonascii: String,
        #[arg(long, default_value_t = ITERM2_FONT_SIZE)]
        size: u32,
        /// Skip confirmation prompt
        #[arg(short, long, default_value_t = false)]
        yes: bool,
    },
    /// Launch croft on a remote SSH host.
    Remote {
        /// SSH host alias from ~/.ssh/config, or any host accepted by ssh.
        host: String,
        /// Remote workspace folder to open.
        path: Option<String>,
        /// Open an independent viewport on the remote workspace instead of
        /// joining the shared screen (see `croft attach --solo`).
        #[arg(long, default_value_t = false)]
        solo: bool,
    },
    /// Attach to (or create) a persistent local session for a workspace, so its
    /// terminals, LSP, DAP, and editor state survive closing the window. Detach
    /// by closing the window; reattach by running `croft attach` again. Runs
    /// under croft's built-in session host, which also lets others co-attach (no
    /// external dependency); sessions started by an older croft under dtach keep
    /// reattaching through dtach until they end.
    Attach {
        /// Workspace folder to open (defaults to the current directory).
        path: Option<PathBuf>,
        /// Open an independent viewport on the workspace instead of joining
        /// the shared screen: your own croft process (scroll and navigate
        /// freely) with edits to shared files replicated between
        /// participants over the collab relay (see docs/MULTIPLAYER.md).
        #[arg(long, default_value_t = false)]
        solo: bool,
    },
    /// List the running persistent croft sessions started with `croft attach`.
    Ls,
    /// Internal: the multiplayer session host/client (croft's dtach
    /// replacement; see docs/MULTIPLAYER.md). `croft attach` and the remote
    /// launcher drive this; it is not intended for manual use.
    #[command(hide = true)]
    SessionHost {
        /// Exit 0 immediately; lets the remote launch script probe whether
        /// this binary supports mux sessions before preferring them.
        #[arg(long, default_value_t = false)]
        probe: bool,
        /// Run the server (spawned detached by the attach-or-create client).
        #[arg(long, default_value_t = false)]
        serve: bool,
        /// Session socket path.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Workspace recorded in the session sidecar for `croft ls`.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// The inner command after `--` (the real croft invocation).
        #[arg(last = true)]
        inner: Vec<String>,
    },
    /// Internal: the Phase D collab-op relay, a dumb fan-out of CRDT edit ops
    /// between independent-viewport participants over `<hash>.collab.sock`
    /// (see docs/MULTIPLAYER.md). Spawned detached by the solo-viewport
    /// launch; not intended for manual use.
    #[command(hide = true)]
    CollabRelay {
        /// Exit 0 immediately; lets the remote launch script probe whether
        /// this binary supports collab relays before preferring them.
        #[arg(long, default_value_t = false)]
        probe: bool,
        /// Ensure a detached relay is serving the socket, then exit.
        /// Attach-or-create: safe to run when one is already up.
        #[arg(long, default_value_t = false)]
        ensure: bool,
        /// Collab socket path (`<hash>.collab.sock`).
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Internal: a headless collab participant exposed as an MCP server on
    /// stdio (see docs/MULTIPLAYER.md). Joins the workspace's collab relay
    /// as a guest so an external agent (e.g. Claude Code via
    /// `claude mcp add croft-collab -- croft collab-agent --workspace <path>`)
    /// co-edits with a live named caret. Not intended for manual use.
    #[command(hide = true)]
    CollabAgent {
        /// The workspace whose collab session to join (keys the relay
        /// socket exactly like `croft attach --solo` does).
        #[arg(long)]
        workspace: PathBuf,
        /// The name this participant's caret wears on peers' screens.
        #[arg(long)]
        name: Option<String>,
    },
    /// Pair with Claude inside a live croft session: an AI collaborator
    /// whose edits stream into the shared buffers token by token (visible
    /// as they are written, with a named caret riding the stream), and
    /// which any participant can cancel mid-run from croft (the streamed
    /// text is reverted). Run it in a workspace someone has open via
    /// `croft attach`; type tasks at the prompt (`@<file> <task>` focuses
    /// a buffer). See docs/MULTIPLAYER.md.
    Pair {
        /// The workspace whose collab session to join (defaults to the
        /// current directory; keys the relay socket exactly like
        /// `croft attach --solo` does).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Model handed to the claude CLI (--model), e.g. a cheaper one
        /// for mechanical edits.
        #[arg(long)]
        model: Option<String>,
        /// The name this collaborator's caret wears on peers' screens.
        #[arg(long)]
        name: Option<String>,
        /// The first task; further tasks are read from stdin, one per line.
        task: Option<String>,
    },
    /// One-time setup for the cross-compile fast path used by `croft <host>`:
    /// installs cargo-zigbuild and adds the two rustup targets croft ships
    /// binaries for (x86_64 / aarch64 musl). After this finishes, the
    /// remote install on every connect ships a pre-built static binary
    /// instead of recompiling on the box (~2 s vs ~30 s).
    SetupCross {
        /// Skip confirmation prompt
        #[arg(short, long, default_value_t = false)]
        yes: bool,
    },
    /// Configure Ghostty for Croft: forward every croft chord (Cmd+T, Cmd+W,
    /// Cmd+[ / Cmd+], Cmd+1..9, etc.) to croft as a CSI-u sequence instead of
    /// letting Ghostty's own keybinds (new_tab, goto_tab, ...) swallow them.
    SetupGhostty {
        /// Skip confirmation prompt
        #[arg(short, long, default_value_t = false)]
        yes: bool,
    },
    /// Create a clickable macOS launcher (Croft.app) that opens Ghostty with
    /// croft already running. Reachable from Spotlight, Launchpad, and the Dock.
    InstallLauncher {
        /// Directory croft opens in (default: your home folder).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Install to ~/Applications instead of /Applications (no admin rights).
        #[arg(long, default_value_t = false)]
        user: bool,
        /// Skip confirmation prompt
        #[arg(short, long, default_value_t = false)]
        yes: bool,
    },
}

impl Cli {
    pub fn run(self) -> Result<()> {
        // macOS launchd starts croft with a 256-fd soft limit; the editor's
        // PTYs/watchers/LSPs and especially the spawned cross-link (~250
        // rlibs open at once) need far more. Children inherit the raise.
        crate::remote::raise_fd_limit();
        match self.command {
            Some(CliCommand::SetupTerminal { font, size, yes }) => setup_terminal(&font, size, yes),
            Some(CliCommand::Keys { mouse }) => keys_diagnostic(mouse),
            Some(CliCommand::SetupIterm2 {
                font,
                nonascii,
                size,
                yes,
            }) => setup_iterm2(&font, &nonascii, size, yes),
            Some(CliCommand::Remote { host, path, solo }) => {
                match crate::remote::launch_croft(&host, path.as_deref(), solo)? {
                    crate::remote::RemoteOutcome::ReturnToLocal => {
                        let cwd = std::env::current_dir().context("resolving workspace path")?;
                        crate::app::run(cwd, None, None, false)
                    }
                    crate::remote::RemoteOutcome::Exited => Ok(()),
                }
            }
            Some(CliCommand::Attach { path, solo }) => crate::session::attach(path, solo),
            Some(CliCommand::Ls) => crate::session::list(),
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
                crate::collab_agent::run(&socket, name.unwrap_or_else(|| "claude".into()))
            }
            Some(CliCommand::Pair {
                workspace,
                model,
                name,
                task,
            }) => {
                let workspace = workspace
                    .unwrap_or_else(|| std::env::current_dir().expect("cwd"))
                    .canonicalize()
                    .context("resolving workspace path")?;
                let socket = crate::session::collab_socket_path(&workspace);
                if let Some(dir) = socket.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                crate::collab::ensure_relay(&socket)?;
                crate::pair::run(crate::pair::PairConfig {
                    socket,
                    workspace,
                    name: name.unwrap_or_else(|| "claude".into()),
                    model,
                    task,
                })
            }
            Some(CliCommand::SetupCross { yes }) => setup_cross(yes),
            Some(CliCommand::SetupGhostty { yes }) => setup_ghostty(yes),
            Some(CliCommand::InstallLauncher { path, user, yes }) => {
                install_launcher(path, user, yes)
            }
            None => {
                let path = self
                    .path
                    .unwrap_or_else(|| std::env::current_dir().expect("cwd"))
                    .canonicalize()
                    .context("resolving workspace path")?;
                if !path.is_dir() {
                    anyhow::bail!("{} is not a directory", path.display());
                }
                crate::app::run(path, self.restore_session, self.open_file, self.zen)
            }
        }
    }
}

fn setup_terminal(font: &str, size: u32, yes: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("setup-terminal is macOS-only");
    }
    println!("This will set Terminal.app's default profile font to '{font}' at {size}pt.");
    println!("Existing custom profiles are not modified.");
    if !yes {
        print!("Apply this change? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    let script = format!(
        r#"tell application "Terminal"
    set the font name of the default settings to "{font}"
    set the font size of the default settings to {size}
    set the font name of the startup settings to "{font}"
    set the font size of the startup settings to {size}
end tell"#
    );
    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .context("running osascript")?;
    if !output.status.success() {
        anyhow::bail!(
            "AppleScript failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    println!("Set Terminal.app default profile font to {font} at {size}pt.");
    println!("Quit Terminal.app entirely (cmd+Q) and reopen it for the change to take effect.");
    Ok(())
}

fn keys_diagnostic(mouse: bool) -> Result<()> {
    use crossterm::event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    };
    use crossterm::{execute, terminal};
    use std::io::stdout;
    use std::time::Duration;

    println!("croft keys: press any key to inspect; Ctrl+C to quit.");
    println!("If the kitty keyboard protocol is negotiated, modifier keys");
    println!("(including Cmd/Super on macOS) will appear in the modifier list.");
    if mouse {
        println!("Mouse capture is on: click and drag to inspect mouse events.");
    }
    println!();

    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut out = stdout();
    let kbd_enhanced = execute!(
        out,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    )
    .is_ok();
    let mouse_capture_enabled = mouse && execute!(out, EnableMouseCapture).is_ok();

    let result = (|| -> Result<()> {
        loop {
            if event::poll(Duration::from_millis(500))? {
                match event::read()? {
                    Event::Key(k) => {
                        if k.kind != KeyEventKind::Press && k.kind != KeyEventKind::Repeat {
                            continue;
                        }
                        let mut mods: Vec<&str> = Vec::new();
                        if k.modifiers.contains(KeyModifiers::CONTROL) {
                            mods.push("CONTROL");
                        }
                        if k.modifiers.contains(KeyModifiers::ALT) {
                            mods.push("ALT");
                        }
                        if k.modifiers.contains(KeyModifiers::SHIFT) {
                            mods.push("SHIFT");
                        }
                        if k.modifiers.contains(KeyModifiers::SUPER) {
                            mods.push("SUPER (Cmd)");
                        }
                        if k.modifiers.contains(KeyModifiers::HYPER) {
                            mods.push("HYPER");
                        }
                        if k.modifiers.contains(KeyModifiers::META) {
                            mods.push("META");
                        }
                        let mods_s = if mods.is_empty() {
                            String::from("none")
                        } else {
                            mods.join(" + ")
                        };
                        // Quit on Ctrl+C.
                        if matches!(k.code, KeyCode::Char('c'))
                            && k.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            print!("\r\nQuitting (Ctrl+C).\r\n");
                            break;
                        }
                        let clipboard_probe = if is_paste_probe_key(k.code, k.modifiers) {
                            match crate::clipboard::read_string() {
                                Some(s) if s.is_empty() => String::from("  clipboard=empty"),
                                Some(s) => format!(
                                    "  clipboard=ok chars={} bytes={}",
                                    s.chars().count(),
                                    s.len()
                                ),
                                None => String::from("  clipboard=read_failed"),
                            }
                        } else {
                            String::new()
                        };
                        print!(
                            "\r  key={:?}  code={:?}  modifiers=[{}]  kitty={}{}\r\n",
                            k, k.code, mods_s, kbd_enhanced, clipboard_probe
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    Event::Paste(s) => {
                        print!(
                            "\r  paste_event chars={} bytes={} kitty={}\r\n",
                            s.chars().count(),
                            s.len(),
                            kbd_enhanced
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    Event::Mouse(mev) => {
                        let mut mods: Vec<&str> = Vec::new();
                        if mev.modifiers.contains(KeyModifiers::CONTROL) {
                            mods.push("CONTROL");
                        }
                        if mev.modifiers.contains(KeyModifiers::ALT) {
                            mods.push("ALT");
                        }
                        if mev.modifiers.contains(KeyModifiers::SHIFT) {
                            mods.push("SHIFT");
                        }
                        let mods_s = if mods.is_empty() {
                            String::from("none")
                        } else {
                            mods.join(" + ")
                        };
                        print!(
                            "\r  mouse kind={:?}  col={}  row={}  modifiers=[{}]\r\n",
                            mev.kind, mev.column, mev.row, mods_s
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    other => {
                        print!("\r  event={other:?} kitty={kbd_enhanced}\r\n");
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                }
            }
        }
        Ok(())
    })();

    if mouse_capture_enabled {
        execute!(stdout(), DisableMouseCapture).ok();
    }
    if kbd_enhanced {
        execute!(stdout(), PopKeyboardEnhancementFlags).ok();
    }
    terminal::disable_raw_mode().ok();
    result
}

fn is_paste_probe_key(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    let crossterm::event::KeyCode::Char(c) = code else {
        return false;
    };
    c == '\u{16}'
        || (c.eq_ignore_ascii_case(&'v')
            && (modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                || modifiers.contains(crossterm::event::KeyModifiers::SUPER)))
}

fn setup_iterm2(font: &str, nonascii: &str, size: u32, yes: bool) -> Result<()> {
    let plist_path = crate::iterm2::default_plist_path();
    println!(
        "This will configure iTerm2 for Croft:\n  Normal Font: {font} {size}\n  Non-ASCII Font: {nonascii} {size}\n  Use Non-ASCII Font: enabled\n  Global key: Cmd+Shift+F -> Croft Search\n  App menu shortcut: move Find Globally off Cmd+Shift+F\n  App menu shortcut: move Show Help Menu off Cmd+Shift+/ so Croft can reach its 'make root at parent' action\n  Global key: Cmd+Shift+/ -> Croft 'make root at parent'\n  Global key: Cmd+Shift+Return -> Croft Explorer expand/collapse\n  App menu shortcut: relocate Edit > Copy / Cut / Select All / Undo to Cmd+Opt+letter so Croft handles Cmd+C / Cmd+X / Cmd+A / Cmd+Z directly (note: any user-installed profile-level Cmd+letter -> Send Hex Code bindings need to be removed manually in iTerm2 Settings > Profiles > Keys > Key Bindings, since croft cannot tell them apart from intentional user customizations)\n  Global keys: Cmd+A / Cmd+C / Cmd+S / Cmd+X / Cmd+Z -> Croft as CSI-u so the NSResponder default copy:/cut:/selectAll:/undo: bindings cannot consume the chord before croft sees it\n  App menu shortcut: relocate New Tab off Cmd+T (to Cmd+Ctrl+T) so Croft opens a new terminal on Cmd+T\n  App menu shortcut: relocate Previous/Next Pane off Cmd+[ / Cmd+] (to Cmd+Opt+[ / Cmd+Opt+]) so Croft cycles terminals\n  Global keys: Cmd+T -> new terminal; Cmd+[ / Cmd+] -> cycle to previous / next terminal\n  Advanced setting: NoSyncNeverAskAboutMouseReportingFrustration=true to suppress iTerm2's 'Disable mouse reporting?' banner (croft owns the mouse; the banner is noise)\n  Cmd+V: left on iTerm2's native Paste action so it works locally and over SSH via bracketed paste; legacy Cmd+V hex bindings and Paste menu remaps from earlier croft versions are cleared"
    );
    println!("Plist target: {}", plist_path.display());
    println!("Existing custom profile fonts are not modified; global key mappings are updated.");
    if !yes {
        print!("Apply this change? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    crate::iterm2::install_croft_settings(&plist_path, font, nonascii, size)
        .with_context(|| "applying iTerm2 Croft settings")?;
    println!("Wrote Croft settings to {}.", plist_path.display());
    println!(
        "Quit iTerm2 entirely (cmd+Q) and reopen it. macOS caches plists; iTerm2 must be relaunched to pick up the change."
    );
    Ok(())
}

fn setup_ghostty(yes: bool) -> Result<()> {
    let config_path = crate::ghostty::default_config_path();
    println!(
        "This will add a managed keybind block to your Ghostty config so Croft, not Ghostty, owns its chords:"
    );
    println!(
        "  Cmd+T -> new terminal; Cmd+[ / Cmd+] -> cycle terminals; Cmd+W -> close editor tab"
    );
    println!(
        "  Cmd+1..Cmd+9 / Cmd+0 -> Croft (Ghostty's goto_tab is overridden) so they work as vim counts"
    );
    println!(
        "  Cmd+C / Cmd+X / Cmd+A / Cmd+Z / Cmd+S and the editor / Explorer / source-control chords"
    );
    println!(
        "  Each chord is re-emitted as the same CSI-u sequence Croft already receives under iTerm2"
    );
    println!("  Cmd+V is left on Ghostty's native paste so it keeps working locally and over SSH");
    println!("Config target: {}", config_path.display());
    println!(
        "Only Croft's marked block is rewritten; the rest of your Ghostty config is preserved."
    );
    if !yes {
        print!("Apply this change? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    crate::ghostty::install_keybinds(&config_path)
        .with_context(|| "applying Ghostty Croft keybindings")?;
    println!("Wrote Croft keybindings to {}.", config_path.display());
    println!(
        "Reload Ghostty's config (Cmd+Shift+,) or restart Ghostty for the change to take effect."
    );
    Ok(())
}

fn install_launcher(path: Option<PathBuf>, user: bool, yes: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("install-launcher is macOS-only");
    }
    // The croft that runs inside Ghostty is this very executable.
    let croft_bin = std::env::current_exe()
        .context("resolving croft binary path")?
        .canonicalize()
        .context("canonicalizing croft binary path")?;
    let open_dir = match path {
        Some(p) => p,
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?),
    };
    let open_dir = open_dir
        .canonicalize()
        .with_context(|| format!("resolving {}", open_dir.display()))?;
    if !open_dir.is_dir() {
        anyhow::bail!("{} is not a directory", open_dir.display());
    }
    let app_dir = crate::launcher::applications_dir(user)?;
    println!("This will create a clickable Croft.app launcher:");
    println!("  Location: {}", app_dir.join("Croft.app").display());
    println!(
        "  Opens:    a new Ghostty window running `croft {}`",
        open_dir.display()
    );
    println!("  Reachable from Spotlight (Cmd+Space, type 'Croft'), Launchpad, and the Dock.");
    println!(
        "  Your normal Ghostty windows are unaffected (this only sets the launcher's window)."
    );
    if !yes {
        print!("Create it? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    let app = crate::launcher::install(
        &app_dir,
        &croft_bin.to_string_lossy(),
        &open_dir.to_string_lossy(),
    )
    .with_context(|| "creating the Croft.app launcher")?;
    println!("Created {}.", app.display());
    println!("Find it in Spotlight (Cmd+Space, type 'Croft') or drag it to your Dock.");
    Ok(())
}

const CROSS_TARGETS: &[&str] = &["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

fn setup_cross(yes: bool) -> Result<()> {
    println!(
        "This installs the local-cross-build fast path used by `croft <host>`:\n  - homebrew package: zig (the linker)\n  - cargo subcommand: cargo-zigbuild\n  - rustup targets: {}",
        CROSS_TARGETS.join(", ")
    );
    println!(
        "After this finishes, every remote connect rsyncs a pre-built static binary instead of recompiling croft on the box."
    );
    if !yes {
        print!("Apply this change? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    install_zig_if_missing()?;
    install_cargo_zigbuild_if_missing()?;
    for triple in CROSS_TARGETS {
        install_rust_target_if_missing(triple)?;
    }
    println!("Cross-compile fast path ready. Next `croft <host>` will ship a pre-built binary.");
    Ok(())
}

fn install_zig_if_missing() -> Result<()> {
    if std::process::Command::new("zig")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        println!("zig is already installed.");
        return Ok(());
    }
    if std::process::Command::new("brew")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        println!("Installing zig via Homebrew");
        let status = std::process::Command::new("brew")
            .args(["install", "zig"])
            .status()
            .context("running brew install zig")?;
        if !status.success() {
            anyhow::bail!("brew install zig exited with {status}");
        }
    } else {
        anyhow::bail!(
            "zig is not installed and Homebrew is not available; install zig manually (https://ziglang.org/download/) and rerun this command"
        );
    }
    Ok(())
}

fn install_cargo_zigbuild_if_missing() -> Result<()> {
    if std::process::Command::new("cargo-zigbuild")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        println!("cargo-zigbuild is already installed.");
        return Ok(());
    }
    println!("Installing cargo-zigbuild via cargo");
    let status = std::process::Command::new("cargo")
        .args(["install", "cargo-zigbuild", "--locked"])
        .status()
        .context("running cargo install cargo-zigbuild")?;
    if !status.success() {
        anyhow::bail!("cargo install cargo-zigbuild exited with {status}");
    }
    Ok(())
}

fn install_rust_target_if_missing(triple: &str) -> Result<()> {
    let installed = std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .context("rustup target list --installed")?;
    if !installed.status.success() {
        anyhow::bail!("rustup not available; install Rust via https://rustup.rs and rerun");
    }
    if String::from_utf8_lossy(&installed.stdout)
        .lines()
        .any(|line| line.trim() == triple)
    {
        println!("rustup target {triple} is already installed.");
        return Ok(());
    }
    println!("Adding rustup target {triple}");
    let status = std::process::Command::new("rustup")
        .args(["target", "add", triple])
        .status()
        .context("rustup target add")?;
    if !status.success() {
        anyhow::bail!("rustup target add {triple} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_no_args() {
        let cli = Cli::parse_from(["croft"]);
        assert!(cli.path.is_none());
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_path_argument() {
        let cli = Cli::parse_from(["croft", "/tmp"]);
        assert_eq!(cli.path, Some(PathBuf::from("/tmp")));
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_setup_terminal_subcommand_with_defaults() {
        let cli = Cli::parse_from(["croft", "setup-terminal"]);
        assert!(cli.path.is_none());
        match cli.command {
            Some(CliCommand::SetupTerminal { font, size, yes }) => {
                assert_eq!(font, SETUP_FONT_PS_NAME);
                assert_eq!(size, SETUP_FONT_SIZE);
                assert!(!yes);
            }
            _ => panic!("expected SetupTerminal"),
        }
    }

    #[test]
    fn parses_setup_terminal_with_overrides() {
        let cli = Cli::parse_from([
            "croft",
            "setup-terminal",
            "--font",
            "FiraCodeNFM-Regular",
            "--size",
            "14",
            "--yes",
        ]);
        match cli.command {
            Some(CliCommand::SetupTerminal { font, size, yes }) => {
                assert_eq!(font, "FiraCodeNFM-Regular");
                assert_eq!(size, 14);
                assert!(yes);
            }
            _ => panic!("expected SetupTerminal"),
        }
    }

    #[test]
    fn parses_setup_terminal_with_short_yes() {
        let cli = Cli::parse_from(["croft", "setup-terminal", "-y"]);
        match cli.command {
            Some(CliCommand::SetupTerminal { yes, .. }) => assert!(yes),
            _ => panic!("expected SetupTerminal"),
        }
    }

    #[test]
    fn parses_keys_subcommand() {
        let cli = Cli::parse_from(["croft", "keys"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Keys { mouse: false })
        ));
    }

    #[test]
    fn parses_keys_subcommand_with_mouse() {
        let cli = Cli::parse_from(["croft", "keys", "--mouse"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Keys { mouse: true })
        ));
    }

    #[test]
    fn parses_setup_iterm2_with_defaults() {
        let cli = Cli::parse_from(["croft", "setup-iterm2"]);
        match cli.command {
            Some(CliCommand::SetupIterm2 {
                font,
                nonascii,
                size,
                yes,
            }) => {
                assert_eq!(font, ITERM2_FONT_PS_NAME);
                assert_eq!(nonascii, ITERM2_NONASCII_PS_NAME);
                assert_eq!(size, ITERM2_FONT_SIZE);
                assert!(!yes);
            }
            _ => panic!("expected SetupIterm2"),
        }
    }

    #[test]
    fn parses_setup_iterm2_with_overrides() {
        let cli = Cli::parse_from([
            "croft",
            "setup-iterm2",
            "--font",
            "FiraCodeNFM-Reg",
            "--nonascii",
            "SymbolsNFM",
            "--size",
            "15",
            "-y",
        ]);
        match cli.command {
            Some(CliCommand::SetupIterm2 {
                font,
                nonascii,
                size,
                yes,
            }) => {
                assert_eq!(font, "FiraCodeNFM-Reg");
                assert_eq!(nonascii, "SymbolsNFM");
                assert_eq!(size, 15);
                assert!(yes);
            }
            _ => panic!("expected SetupIterm2"),
        }
    }

    #[test]
    fn parses_attach_command_with_and_without_path() {
        let cli = Cli::parse_from(["croft", "attach"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Attach {
                path: None,
                solo: false
            })
        ));
        let cli = Cli::parse_from(["croft", "attach", "/work"]);
        match cli.command {
            Some(CliCommand::Attach { path, .. }) => assert_eq!(path, Some(PathBuf::from("/work"))),
            _ => panic!("expected Attach"),
        }
    }

    #[test]
    fn parses_ls_command() {
        let cli = Cli::parse_from(["croft", "ls"]);
        assert!(matches!(cli.command, Some(CliCommand::Ls)));
    }

    #[test]
    fn parses_session_host_probe_serve_and_inner_command() {
        let cli = Cli::parse_from(["croft", "session-host", "--probe"]);
        match cli.command {
            Some(CliCommand::SessionHost { probe, serve, .. }) => {
                assert!(probe);
                assert!(!serve);
            }
            _ => panic!("expected SessionHost"),
        }
        let cli = Cli::parse_from([
            "croft",
            "session-host",
            "--serve",
            "--socket",
            "/x/s.mux.sock",
            "--workspace",
            "/work/repo",
            "--",
            "croft",
            "/work/repo",
        ]);
        match cli.command {
            Some(CliCommand::SessionHost {
                probe,
                serve,
                socket,
                workspace,
                inner,
            }) => {
                assert!(!probe);
                assert!(serve);
                assert_eq!(socket, Some(PathBuf::from("/x/s.mux.sock")));
                assert_eq!(workspace, Some(PathBuf::from("/work/repo")));
                assert_eq!(inner, vec!["croft", "/work/repo"]);
            }
            _ => panic!("expected SessionHost"),
        }
    }

    #[test]
    fn parses_collab_relay_subcommand() {
        let cli = Cli::parse_from(["croft", "collab-relay", "--socket", "/x/s.collab.sock"]);
        match cli.command {
            Some(CliCommand::CollabRelay {
                socket,
                probe,
                ensure,
            }) => {
                assert_eq!(socket, Some(PathBuf::from("/x/s.collab.sock")));
                assert!(!probe);
                assert!(!ensure);
            }
            _ => panic!("expected CollabRelay"),
        }
        // The launch tails probe support and ensure a detached relay.
        let cli = Cli::parse_from(["croft", "collab-relay", "--probe"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::CollabRelay { probe: true, .. })
        ));
        let cli = Cli::parse_from(["croft", "collab-relay", "--ensure", "--socket", "/x/s.sock"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::CollabRelay { ensure: true, .. })
        ));
    }

    #[test]
    fn parses_collab_agent_subcommand() {
        let cli = Cli::parse_from([
            "croft",
            "collab-agent",
            "--workspace",
            "/w",
            "--name",
            "bot",
        ]);
        match cli.command {
            Some(CliCommand::CollabAgent { workspace, name }) => {
                assert_eq!(workspace, PathBuf::from("/w"));
                assert_eq!(name.as_deref(), Some("bot"));
            }
            _ => panic!("expected CollabAgent"),
        }
        // The name is optional; the dispatch defaults it.
        let cli = Cli::parse_from(["croft", "collab-agent", "--workspace", "/w"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::CollabAgent { name: None, .. })
        ));
    }

    #[test]
    fn parses_pair_subcommand() {
        let cli = Cli::parse_from([
            "croft",
            "pair",
            "--workspace",
            "/w",
            "--model",
            "claude-haiku-4-5-20251001",
            "--name",
            "navigator",
            "fix the loop in src/f.rs",
        ]);
        match cli.command {
            Some(CliCommand::Pair {
                workspace,
                model,
                name,
                task,
            }) => {
                assert_eq!(workspace, Some(PathBuf::from("/w")));
                assert_eq!(model.as_deref(), Some("claude-haiku-4-5-20251001"));
                assert_eq!(name.as_deref(), Some("navigator"));
                assert_eq!(task.as_deref(), Some("fix the loop in src/f.rs"));
            }
            _ => panic!("expected Pair"),
        }
        // Everything defaults: workspace = cwd, task read from stdin.
        let cli = Cli::parse_from(["croft", "pair"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Pair {
                workspace: None,
                model: None,
                name: None,
                task: None,
            })
        ));
    }

    #[test]
    fn parses_solo_viewport_flags() {
        // Local: an independent-viewport guest session for a workspace.
        let cli = Cli::parse_from(["croft", "attach", "--solo", "/work"]);
        match cli.command {
            Some(CliCommand::Attach { path, solo }) => {
                assert_eq!(path, Some(PathBuf::from("/work")));
                assert!(solo);
            }
            _ => panic!("expected Attach"),
        }
        let cli = Cli::parse_from(["croft", "attach"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Attach { solo: false, .. })
        ));
        // Remote: same opt-in on the SSH launch.
        let cli = Cli::parse_from(["croft", "remote", "reasoner", "/work", "--solo"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Remote { solo: true, .. })
        ));
        let cli = Cli::parse_from(["croft", "remote", "reasoner"]);
        assert!(matches!(
            cli.command,
            Some(CliCommand::Remote { solo: false, .. })
        ));
    }

    #[test]
    fn parses_remote_command() {
        let cli = Cli::parse_from(["croft", "remote", "reasoner", "/work"]);
        match cli.command {
            Some(CliCommand::Remote { host, path, .. }) => {
                assert_eq!(host, "reasoner");
                assert_eq!(path, Some(String::from("/work")));
            }
            _ => panic!("expected Remote"),
        }
    }
}
