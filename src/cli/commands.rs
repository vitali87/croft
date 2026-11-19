//! Every subcommand and its flags, as clap parses them: `CliCommand`, and `croft hook`'s
//! actions.

use super::*;

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
