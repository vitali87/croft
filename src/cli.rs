use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;

mod git_editor;
mod hook;
mod keys;
mod links;
mod pair_provider;
mod plot;
mod terminal_setup;
mod typos;
mod version;
mod workspace;
use git_editor::*;
use hook::*;
use keys::*;
use links::*;
use pair_provider::*;
use plot::*;
use terminal_setup::*;
pub use typos::misspelled_subcommand;
use version::*;
use workspace::*;

#[derive(Parser, Debug)]
#[command(name = "croft", version = VERSION, about = "Terminal-based VS Code replica")]
pub struct Cli {
    /// Workspace folder to open (defaults to current directory). A file works
    /// too: the workspace roots at its parent and the file opens in the editor.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    // Ignoring a subcommand is left as-is deliberately: clap subcommands are
    // not argument ids, so `conflicts_with = "command"` is a runtime assert
    // failure rather than a guard. A plain comment rather than part of the
    // doc below, because clap prints a field's `///` as its long help and
    // this note is for maintainers, not `croft --help` readers (#853). The
    // flag itself came from #282; the issue number stays here, not in help.
    /// Print the version with build provenance (git hash and build time) and
    /// exit. `--version` prints the plain `x.y.z`; this is the flag to quote
    /// in a bug report, since two builds can share a version.
    ///
    /// It answers before anything else runs, so pairing it with a subcommand
    /// ignores that subcommand, as `--version` does.
    #[arg(long)]
    pub build_info: bool,

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

/// `croft hook ...`.
#[derive(Subcommand, Debug)]
pub enum HookAction {
    /// Register `croft hook claude-code` as Claude Code's `PreToolUse` hook
    /// for Edit, Write and MultiEdit. Prints the settings diff before
    /// writing it.
    Install {
        /// Install into `~/.claude/settings.json` (every project).
        #[arg(long, conflicts_with = "project")]
        global: bool,
        /// Install into `./.claude/settings.json` (this project; the default).
        #[arg(long)]
        project: bool,
        /// Remove croft's entry instead, restoring the file install replaced
        /// when nothing else has changed it since.
        #[arg(long)]
        uninstall: bool,
    },
    /// The hook itself: reads Claude Code's payload on stdin and answers
    /// with the running croft's decision. Not meant to be run by hand.
    ClaudeCode,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    // Issue #377.
    /// Take the guided tour in a throwaway sample project.
    Demo {
        /// Run this tour file (the same JSON shape as the built-in
        /// assets/tour/tour.json) instead of the built-in tour.
        #[arg(long)]
        tour: Option<PathBuf>,
    },
    // Issue #365.
    /// Review a pull request: open croft on this repository with the
    /// PR's changed files, checks and viewed marks in their own tab.
    Pr {
        /// The pull request number (`579`, `#579`, or its URL).
        number: String,
    },
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
        // Issue #261.
        /// Build croft for the remote from this source tree instead of
        /// installing the matching release binary: for a local tree ahead of
        /// the latest release.
        #[arg(long, default_value_t = false)]
        build: bool,
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
    /// Record a persistent session (started with `croft attach`) to an
    /// asciicast v2 file that `asciinema play` and the agg GIF converter read.
    /// The recorder watches read-only: it never resizes the session or types
    /// into it. Stop with Ctrl-C or `croft record --stop`.
    Record {
        /// Workspace whose session to record (defaults to the current directory).
        path: Option<PathBuf>,
        /// Where to write the cast (defaults to `croft-session-<time>.cast` in
        /// the workspace).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Stop the recording running for the workspace instead of starting one.
        #[arg(long, default_value_t = false)]
        stop: bool,
    },
    /// List the running persistent croft sessions started with `croft attach`.
    Ls,
    /// Chart numbers, CSV/TSV, or JSON lines from stdin inline in the pane:
    /// `seq 1 100 | awk '{print sin($1/10)}' | croft plot`. Draws an image on
    /// iTerm2 / Kitty terminals and a braille or block chart anywhere else.
    /// Headers are detected; `--x` names the label column, `--y` the series
    /// (repeatable).
    Plot {
        /// Chart shape: line, bar, spark (one row), or hist (histogram of
        /// the first series).
        #[arg(long = "type", default_value = "line")]
        kind: String,
        /// Column (name or 0-based index) that labels the points.
        #[arg(long)]
        x: Option<String>,
        /// Column(s) to plot; default: every numeric column.
        #[arg(long)]
        y: Vec<String>,
        #[arg(long)]
        title: Option<String>,
        /// Width in terminal columns.
        #[arg(long, default_value_t = 60)]
        width: u16,
        /// Height in terminal rows.
        #[arg(long, default_value_t = 15)]
        height: u16,
        /// Always print the text chart, even where an image would render.
        #[arg(long, default_value_t = false)]
        text: bool,
    },
    // Issue #694.
    /// Internal: hold one terminal pane's shell in a process of its own, so
    /// it outlives croft. croft starts it; not for manual use.
    #[command(hide = true)]
    PaneHost {
        /// The socket the pane attaches to.
        #[arg(long)]
        socket: PathBuf,
        /// The pane's size, COLSxROWS.
        #[arg(long, default_value = "80x24")]
        size: String,
        /// The shell's working directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// KEY=VALUE set for the shell; repeatable.
        #[arg(long = "env")]
        env: Vec<String>,
        /// The shell and its arguments, after `--`.
        #[arg(last = true)]
        argv: Vec<String>,
    },
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
    /// Activate the resident navigator for a workspace: an AI pair
    /// programmer that croft itself hosts. This command records the
    /// activation and exits; the running croft (or the next one to start
    /// there) seats the pilot. Inside croft: ask it on a line or selection
    /// (right-click the gutter, or Cmd+K Q), yield it the turn to review
    /// (Cmd+K Y, comment-only), watch its edits stream in token by token,
    /// and cancel them mid-run (Cmd+K X). See docs/MULTIPLAYER.md.
    Pair {
        /// The workspace to activate the navigator for (defaults to the
        /// current directory; keys the relay socket exactly like
        /// `croft attach --solo` does).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// The model that takes the seat: handed to the claude CLI on the
        /// default provider (e.g. a cheaper one for mechanical edits), or
        /// the local model name on --provider ollama (required there,
        /// e.g. qwen3-coder:30b).
        #[arg(long)]
        model: Option<String>,
        /// The name this collaborator's caret wears on peers' screens.
        #[arg(long)]
        name: Option<String>,
        /// A first task for the --repl driver to send once seated.
        // Hidden with --repl and refused without it: the resident navigator
        // takes its instructions in-editor (Cmd+K Q), so a task accepted
        // here would be dropped on the floor (#1908).
        #[arg(requires = "repl", hide = true)]
        task: Option<String>,
        /// Backend that seats the pilot: "claude" (default, the claude CLI)
        /// or "ollama" (a direct Anthropic-compatible local endpoint —
        /// Ollama, LM Studio, llama.cpp, vLLM; requires --model).
        #[arg(long, value_parser = ["claude", "ollama"])]
        provider: Option<String>,
        /// The local endpoint for --provider ollama (implies it); defaults
        /// to http://localhost:11434.
        #[arg(long)]
        base_url: Option<String>,
        /// Deactivate the navigator for the workspace instead.
        #[arg(long, default_value_t = false)]
        off: bool,
        /// Debug driver: run the pilot in THIS terminal with a line-based
        /// REPL instead of activating the croft-hosted seat.
        #[arg(long, default_value_t = false, hide = true)]
        repl: bool,
    },
    // Issue #362. `imgcat` works over SSH because escape sequences travel
    // through the terminal, but croft's richer previews are editor tabs, so
    // a shell prompt had no way to ask for one; this is that ask.
    /// Open a file in the croft that hosts this pane.
    ///
    /// Opens croft's richer previews (paged PDFs, spreadsheets, SQLite) from
    /// a shell prompt, over SSH too: the path is resolved against the
    /// shell's own cwd and handed to the running croft, which opens it
    /// exactly as an Explorer click would.
    View {
        /// File to open, or `-` to read the content from stdin.
        // `OsString`, not `String`: a filename is bytes on Unix, and the
        // wire format goes to some trouble to carry those bytes intact.
        // Taking the argument as a `String` would have thrown them away at
        // the entry point, one call before the encoding that preserves them.
        // A plain comment, because clap prints a field's whole `///` as its
        // long help and this note is for maintainers (#1172).
        path: std::ffi::OsString,
        /// Extension to stage piped bytes under, for content that has no
        /// magic bytes to sniff (`--as csv`, `--as json`). Only valid with
        /// `-`: on a named file it is REFUSED rather than ignored, since that
        /// file is routed by its own extension and a silently dropped flag
        /// shows up only as the wrong viewer opening.
        #[arg(long = "as")]
        as_ext: Option<String>,
    },
    // Issue #341.
    /// Serve this workspace's session to a browser over WebSocket.
    ///
    /// Each WebSocket connection is one participant of the running session
    /// (`croft attach` starts it). Every connection must present the
    /// per-run token printed at start-up. The listener stops when the
    /// session does.
    Web {
        /// Workspace whose session to serve (default: the current directory).
        workspace: Option<PathBuf>,
        /// Address to listen on (default 127.0.0.1:7681). A non-loopback
        /// address is served with a warning: other machines can reach it.
        #[arg(long)]
        bind: Option<std::net::SocketAddr>,
        /// Serve TLS with this PEM certificate chain and private key.
        #[arg(long, num_args = 2, value_names = ["CERT", "KEY"])]
        tls: Option<Vec<PathBuf>>,
        /// Restart this workspace's listener with a new token.
        #[arg(long, default_value_t = false, conflicts_with = "off")]
        rotate_token: bool,
        /// Stop this workspace's listener; the session keeps running.
        #[arg(long, default_value_t = false, conflicts_with_all = ["bind", "tls"])]
        off: bool,
    },
    // Issue #620.
    /// Open a file for editing in the croft that hosts this pane.
    ///
    /// Like `croft view`, plus `--wait`: return only once the tab is
    /// closed. croft points `GIT_SEQUENCE_EDITOR` at `croft edit --wait`, so
    /// `git rebase -i` opens its plan here. A file that does not exist yet
    /// is created empty (its folder must exist), so `EDITOR="croft edit
    /// --wait"` works for tools that name a new file.
    Edit {
        /// File to open, created empty if it does not exist yet.
        path: std::ffi::OsString,
        /// Block until the file's tab is closed.
        #[arg(long, default_value_t = false)]
        wait: bool,
        /// Running as git's sequence editor: defer to `sequence.editor`
        /// when the repository's git config names one, and to git's own
        /// editor when no croft is there to open the plan.
        #[arg(long, default_value_t = false, hide = true)]
        sequence_editor: bool,
    },
    // Issue #621.
    /// Print a JSON template for translating croft's UI into `lang`.
    ///
    /// Every palette title and every other string croft translates, with
    /// your own or the built-in translation filled in where one exists.
    /// `--write` updates `<config>/locales/<lang>.json` in place, keeping what
    /// it already holds; fill in the rest there.
    LocaleTemplate {
        /// Language code, such as `de` or `fr`.
        lang: String,
        // Issue #1148.
        /// Update `<config>/locales/<lang>.json` in place instead of printing,
        /// keeping every translation already in it.
        #[arg(long, default_value_t = false)]
        write: bool,
    },
    // Issue #617.
    /// Open a workspace inside its dev container.
    ///
    /// Reads `.devcontainer/devcontainer.json`, builds or pulls the image,
    /// starts (or reuses) the container, runs `postCreateCommand` once,
    /// installs croft into it and runs it there on this terminal.
    Devcontainer {
        /// Workspace folder (default: the current directory).
        path: Option<std::path::PathBuf>,
        /// Replace the existing container, discarding changes made inside
        /// it. Without it, an edited config keeps the container and says so.
        #[arg(long, default_value_t = false)]
        rebuild: bool,
    },
    // Issue #346.
    /// Route a coding agent's file edits through croft for approval.
    Hook {
        #[command(subcommand)]
        action: HookAction,
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
    // Issue #351.
    /// Bring a VS Code profile across: settings, keybindings and snippets.
    ///
    /// Finds your VS Code (or Cursor / VSCodium / Windsurf) user directory,
    /// converts what croft has an equivalent for, and MERGES the result into
    /// `~/.config/croft/`: an existing croft value always wins, so running
    /// this twice changes nothing the second time. Keys and commands croft
    /// has no equivalent for are listed rather than dropped silently.
    ImportVscode {
        /// The VS Code user directory to read (default: auto-detected).
        #[arg(long)]
        from: Option<PathBuf>,
        /// Print the full report and write nothing.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    // Issue #350.
    /// Import a VS Code colour theme (.json) as a croft theme.
    ///
    /// Reads a theme file you already have (from a cloned theme repo, or an
    /// installed VS Code extension's `themes/` directory), converts its
    /// workbench colours, terminal palette, TextMate token colours and
    /// `semanticTokenColors` (as a fallback where a scope is missing) into a
    /// croft `[[themes]]` manifest, and installs it under
    /// `~/.config/croft/extensions/`. No extension code is downloaded or run.
    /// The theme appears in the picker on croft's next launch.
    ThemeImport {
        /// Path to the theme JSON (JSONC and `include` chains are handled),
        /// or a marketplace reference: `publisher.name`, a marketplace item
        /// URL, or `vscode:extension/publisher.name`. A marketplace import
        /// downloads the `.vsix`, lifts out ONE theme JSON, and discards the
        /// rest — no extension code is installed or run.
        file: String,
        /// Which theme to take, when the extension contributes several
        /// (Catppuccin ships four). Matched against the label,
        /// case-insensitively.
        #[arg(long)]
        theme: Option<String>,
        /// Theme id to use instead of one derived from the theme's name.
        #[arg(long)]
        id: Option<String>,
        /// Print the manifest instead of installing it.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Configure Ghostty for Croft: forward every croft chord (Cmd+T, Cmd+W,
    /// Cmd+[ / Cmd+], Cmd+1..9, etc.) to croft as a CSI-u sequence instead of
    /// letting Ghostty's own keybinds (new_tab, goto_tab, ...) swallow them.
    SetupGhostty {
        /// Skip confirmation prompt
        #[arg(short, long, default_value_t = false)]
        yes: bool,
    },
    // Issue #359.
    /// Open a `croft://attach?host=…&path=…&focus=…` link: attach to the
    /// session it names. The host must be an alias in ~/.ssh/config.
    /// A `croft://decide?…` link answers a pending agent edit instead.
    OpenLink { url: String },
    // Issue #359.
    /// Approve or deny an agent's pending edit by the one-time token its
    /// notification carried, without attaching to the session.
    #[command(hide = true)]
    Decide {
        token: String,
        #[arg(value_parser = ["allow", "deny"])]
        decision: String,
        /// The workspace whose croft holds the edit (default: the current
        /// directory).
        #[arg(long)]
        path: Option<PathBuf>,
    },
    // Issue #262.
    /// Push your keybindings, snippets, triggers, matchers and macros to a
    /// remote now, as connecting does. A file edited on the remote
    /// since the last push is left alone: --diff compares it, and
    /// --take-local or --keep-remote settles it.
    SyncConfig {
        /// Host alias from ~/.ssh/config.
        host: String,
        /// Show how the remote's copy of this file differs from yours.
        #[arg(long, value_name = "FILE")]
        diff: Option<String>,
        /// Push this file even though the remote's copy was edited.
        #[arg(long, value_name = "FILE")]
        take_local: Vec<String>,
        /// Keep the remote's copy of this file until either side changes.
        #[arg(long, value_name = "FILE")]
        keep_remote: Vec<String>,
    },
    /// Make `croft://` links open croft: an xdg handler on Linux, Termux's
    /// URL opener on Android. On macOS the launcher (install-launcher)
    /// registers the scheme.
    InstallLinkHandler,
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

/// `croft import-vscode`: convert a VS Code profile into croft's config.
///
/// The report is the product, not a side effect. An import that only said
/// "done" would leave the user unable to tell a setting croft adopted from
/// one it has no equivalent for, which is the difference between "my
/// settings came across" and "my settings are gone".
fn import_vscode(from: Option<PathBuf>, dry_run: bool) -> Result<()> {
    let (dir, reading) = match from {
        Some(dir) => {
            // A mistyped path must not read as an empty profile. Every file
            // this scans is optional, so a wrong directory produces "0
            // settings, 0 keybindings, 0 snippets" and "Nothing to import",
            // which is exactly what a real but empty profile produces. The
            // auto-detected branch already fails loudly; this one now does
            // too.
            if !dir.is_dir() {
                anyhow::bail!(
                    "{} is not a directory. --from wants a VS Code user directory \
                     (the one holding settings.json), e.g. ~/.config/Code/User",
                    dir.display()
                );
            }
            let user = crate::import_vscode::user_dir_in(&dir);
            let reading = (user != dir).then(|| format!("Reading {}", user.display()));
            (user, reading)
        }
        None => {
            let found = crate::import_vscode::discover_profiles();
            let Some((label, dir)) = found.into_iter().next() else {
                anyhow::bail!(
                    "no VS Code user directory found. Pass one with --from \
                     (e.g. --from ~/.config/Code/User)"
                );
            };
            let reading = format!("Reading {label}: {}", dir.display());
            (dir, Some(reading))
        }
    };
    let mut report = crate::import_vscode::scan_profile(&dir)?;
    // Write first, report after (#1179): a reader that stops early (`| head`)
    // closes stdout, and whether the import lands must not depend on anyone
    // reading about it. A failed write is returned before the report too,
    // since a closed stdout ends the process at its first line and the
    // failure would go unsaid.
    let applied = (!dry_run && !report.is_empty())
        .then(|| crate::import_vscode::apply(&mut report))
        .transpose()?;
    if let Some(reading) = reading {
        println!("{reading}");
    }

    println!(
        "\n{} setting{} mapped, {} keybinding{} mapped, {} snippet{}",
        report.settings.len(),
        plural(report.settings.len()),
        report.keybindings.len(),
        plural(report.keybindings.len()),
        report.snippets.len(),
        plural(report.snippets.len()),
    );
    for (key, value) in &report.settings {
        println!("  {key} = {value}");
    }
    for (key, command, when) in &report.keybindings {
        match when {
            Some(when) => println!("  {key} -> {command} (when {when})"),
            None => println!("  {key} -> {command}"),
        }
    }
    if let Some(theme) = &report.theme {
        println!(
            "\nworkbench.colorTheme {:?} converts to croft theme {:?} (from {})",
            theme.label,
            theme.converted.id,
            theme.path.display()
        );
        for note in &theme.converted.notes {
            println!("  - {note}");
        }
    }
    if !report.unmapped_settings.is_empty() {
        println!(
            "\n{} setting{} croft has no equivalent for:",
            report.unmapped_settings.len(),
            plural(report.unmapped_settings.len())
        );
        for key in &report.unmapped_settings {
            println!("  {key}");
        }
    }
    if !report.dropped_keybindings.is_empty() {
        println!(
            "\n{} keybinding{} not carried over:",
            report.dropped_keybindings.len(),
            plural(report.dropped_keybindings.len())
        );
        for row in &report.dropped_keybindings {
            println!("  {row}");
        }
    }
    for warning in &report.warnings {
        println!("  warning: {warning}");
    }

    // Extensions (#352): what VS Code has installed, plus what the current
    // directory's workspace recommends, against croft's table. Read-only;
    // installing a croft equivalent is a click in the Extensions panel.
    let mut ids = crate::vscode_extensions::installed_via_code();
    let listed = !ids.is_empty();
    if let Ok(cwd) = std::env::current_dir() {
        ids.extend(crate::vscode_extensions::workspace_recommendations(&cwd));
    }
    let compared = crate::vscode_extensions::compare(ids);
    if !compared.is_empty() {
        println!(
            "\n{} VS Code extension{} ({}):",
            compared.len(),
            plural(compared.len()),
            if listed {
                "installed, plus this workspace's recommendations"
            } else {
                "this workspace's recommendations; `code` is not on PATH"
            }
        );
        for row in &compared {
            use crate::vscode_extensions::Status;
            let what = match row.mapping {
                Some(m) if m.status == Status::Builtin => {
                    format!("built in ({}): {}", m.croft, m.note)
                }
                Some(m) if m.status == Status::Installable => {
                    format!("installable ({}): {}", m.croft, m.note)
                }
                Some(m) => format!("no equivalent: {}", m.note),
                None => String::from("no equivalent: not in croft's table"),
            };
            println!("  {} -> {what}", row.id);
        }
        println!("  Open the Extensions panel (Cmd+Shift+X) to add an installable one.");
    }

    let Some(written) = applied else {
        if dry_run {
            println!("\nDry run: nothing was written.");
        } else {
            println!("\nNothing to import.");
        }
        return Ok(());
    };
    if !report.conflicts.is_empty() {
        println!(
            "\n{} value{} croft already had, left alone:",
            report.conflicts.len(),
            plural(report.conflicts.len())
        );
        for conflict in &report.conflicts {
            println!("  {conflict}");
        }
    }
    if !report.refusals.is_empty() {
        println!(
            "\n{} file{} croft declined to write:",
            report.refusals.len(),
            plural(report.refusals.len())
        );
        for refusal in &report.refusals {
            println!("  {refusal}");
        }
    }
    if written.is_empty() {
        // "Nothing needed changing" and "croft refused to write" are
        // different facts, and saying the first when the second is true tells
        // the user their settings arrived when they did not.
        if report.refusals.is_empty() {
            println!("\nEverything was already in place; nothing changed.");
        } else {
            println!("\nNothing was written: see the refusals above.");
        }
    } else {
        println!("\nWrote:");
        for path in &written {
            println!("  {}", path.display());
        }
        println!("\nRestart croft to pick the new configuration up.");
    }
    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Let a closed stdout end this process the way it ends `ls | head`, for a
/// command that only prints after doing its work (#1179). Rust ignores
/// SIGPIPE, which turns the first `println!` after the reader has gone into
/// a "Broken pipe" panic and exit code 101. Never for croft itself or its
/// session processes: they write to sockets and ptys whose peer can vanish
/// and rely on that write failing instead of killing them.
fn end_quietly_when_stdout_closes() {
    // SAFETY: setting SIGPIPE's disposition to its default installs no
    // handler; the call has no memory-safety preconditions.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
}

/// `croft theme-import <file>`: convert a VS Code theme and install it.
///
/// The notes are printed rather than buried: a converted theme fills croft
/// slots the source never named, and a user comparing it against VS Code
/// deserves to know which colours were derived rather than designed.
fn theme_import(
    reference: &str,
    theme: Option<&str>,
    id: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    // A FILE that exists is a path; anything else is a marketplace
    // reference. Checked in that order so a file literally named like an
    // extension id still wins — the user pointing at a file they can see
    // should never silently hit the network instead.
    //
    // `is_file`, not `exists`: a DIRECTORY named like an extension id would
    // otherwise be taken as the theme and fail deep inside the converter
    // with a read error, rather than being passed to the marketplace where
    // the user plainly meant it to go.
    //
    // Progress goes to stderr, so stdout carries only the result: the
    // manifest alone under --dry-run, and nothing at all before the install,
    // so a closed stdout cannot cost the user the theme (#1179).
    let local = Path::new(reference);
    let (source, _scratch) = if local.is_file() {
        eprintln!("Importing from {}", local.display());
        (local.to_path_buf(), None)
    } else {
        let (path, dir) = fetch_marketplace_theme(reference, theme)?;
        (path, Some(dir))
    };
    // `_scratch` is held until this function returns, so the archive and
    // everything lifted from it survive the conversion and go immediately
    // after it — on the error path as well, since the guard drops either
    // way. Nothing downloaded outlives this command.
    let converted = crate::vscode_theme::convert_file(&source, id)?;
    if dry_run {
        print!("{}", converted.manifest);
        return Ok(());
    }
    let installed = crate::vscode_theme::install(&converted)?;
    println!(
        "Imported \"{}\" as theme id {}",
        converted.label, converted.id
    );
    println!(
        "  {}{}",
        installed.path.display(),
        if installed.replaced {
            " (replaced the manifest already there)"
        } else {
            ""
        }
    );
    if !converted.notes.is_empty() {
        println!(
            "\n{} note{} about this import:",
            converted.notes.len(),
            if converted.notes.len() == 1 { "" } else { "s" }
        );
        for note in &converted.notes {
            println!("  - {note}");
        }
    }
    println!("\nPick it under the gear menu on croft's next launch.");
    Ok(())
}

/// A per-run value for the scratch directory's name. Not a secret and not a
/// CSPRNG: it only has to be unpredictable enough that another user cannot
/// pre-create the path, and `create_dir` failing on collision is what
/// actually enforces exclusivity.
fn scratch_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        ^ (&scratch_nonce as *const _ as u128)
}

/// Resolve a marketplace reference to a theme JSON on disk.
///
/// Returns the extracted file and the temporary directory holding it: the
/// caller keeps that alive until the conversion has read the file, and
/// dropping it takes the `.vsix` and everything lifted from it with it.
/// Nothing from the archive outlives this function.
fn fetch_marketplace_theme(
    reference: &str,
    wanted: Option<&str>,
) -> Result<(PathBuf, crate::marketplace::Scratch)> {
    use crate::marketplace;
    let id = marketplace::parse_ref(reference)?;
    // A private scratch in the temp dir rather than a dev-only temp crate:
    // this is a production path. `Scratch` owns it and removes it on drop,
    // so every failure below cleans up as reliably as success does.
    //
    // The name carries randomness, the directory is created with `create_dir`
    // so an existing path is an ERROR rather than something to delete, and it
    // is mode 0o700 so a permissive umask cannot leave it writable by another
    // local user — the writes here follow symlinks, unlike `extract_member`.
    let scratch = marketplace::Scratch::new(std::env::temp_dir().join(format!(
        "croft-theme-import-{}-{:x}",
        std::process::id(),
        scratch_nonce()
    )))?;
    let dir = scratch.path();
    eprintln!(
        "Downloading {}.{} from the marketplace…",
        id.publisher, id.name
    );
    let vsix = marketplace::download_vsix(&id, dir)?;
    let pkg = marketplace::read_member(&vsix, "extension/package.json", dir)?;
    // The NLS bundle is optional — most extensions have none — so a missing
    // or unreadable one leaves the labels as written rather than failing.
    let nls = marketplace::read_member(&vsix, "extension/package.nls.json", dir).ok();
    let themes = marketplace::contributed_themes(&pkg, nls.as_deref())?;
    let picked = marketplace::pick_theme(&themes, wanted)?;
    eprintln!("  taking \"{}\" ({})", picked.label, picked.path);
    let entry = marketplace::extract_theme_chain(&vsix, &picked.path, dir)?;
    Ok((entry, scratch))
}

fn install_launcher(path: Option<PathBuf>, user: bool, yes: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("install-launcher is macOS-only");
    }
    // The croft that runs inside Ghostty is this very executable, through
    // the link Homebrew moves on upgrade when Homebrew installed it.
    let croft_bin = crate::launcher::stable_croft_path(
        &std::env::current_exe()
            .context("resolving croft binary path")?
            .canonicalize()
            .context("canonicalizing croft binary path")?,
    );
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
    println!("  Also opens files: drop one on the app, or right-click it in Finder and pick Croft");
    println!("            under Open With (Get Info > Change All makes it the default).");
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
    // The launcher starts croft with launchd's bare PATH; the shell's is
    // remembered now so even its first launch never waits for the shell.
    crate::gui_path::remember_login_path();
    println!("Created {}.", app.display());
    println!("Find it in Spotlight (Cmd+Space, type 'Croft') or drag it to your Dock.");
    Ok(())
}

/// The triples `croft <host>` cross-builds for. A rustup target belongs to ONE
/// toolchain, so these must be re-added whenever `rust-toolchain.toml` moves;
/// `remote::tests::the_pinned_toolchain_has_every_cross_target` is the guard.
pub(crate) const CROSS_TARGETS: &[&str] =
    &["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

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

/// Add `triple` to the toolchain the cross-build actually uses.
///
/// Both rustup calls run FROM THE CHECKOUT so `rust-toolchain.toml` resolves
/// the pinned channel. Run from anywhere else, rustup answers for the default
/// toolchain: `setup-cross` would then print "already installed" for a target
/// that belongs to another channel entirely and exit successfully, having
/// fixed nothing, while `croft <host>` kept compiling on the remote box.
fn install_rust_target_if_missing(triple: &str) -> Result<()> {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    let installed = std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .current_dir(checkout)
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
        .current_dir(checkout)
        .status()
        .context("rustup target add")?;
    if !status.success() {
        anyhow::bail!("rustup target add {triple} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
