//! Durable user preferences.
//!
//! Distinct from [`crate::session_state`], which is a per-pid handoff file
//! deleted after a self-re-exec: these settings persist across every launch.
//! Stored as JSON at `~/.config/croft/config.json`, an XDG path that resolves
//! the same on macOS and Linux so the local and remote builds stay in lockstep
//! (the golden rule: identical behavior on both targets).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::app::{PanelAlignment, QuickInputPosition, SideBarPosition};
use crate::theme::Theme;

/// The layout chrome chosen in the "Customize Layout" popup (croft's analog of
/// VS Code's title-bar layout controls). Every field carries its own
/// `#[serde(default)]` so a config written before this block existed still
/// parses straight into these defaults. The primary side bar (⌘B) and panel
/// (⌃J) visibility stay ephemeral per launch and so are NOT stored here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPrefs {
    #[serde(default = "default_true")]
    pub activity_bar: bool,
    #[serde(default = "default_true")]
    pub status_bar: bool,
    #[serde(default)]
    pub side_bar_position: SideBarPosition,
    #[serde(default)]
    pub secondary_side_bar: bool,
    #[serde(default)]
    pub panel_alignment: PanelAlignment,
    #[serde(default)]
    pub quick_input_position: QuickInputPosition,
}

impl Default for LayoutPrefs {
    fn default() -> Self {
        Self {
            activity_bar: true,
            status_bar: true,
            side_bar_position: SideBarPosition::default(),
            secondary_side_bar: false,
            panel_alignment: PanelAlignment::default(),
            quick_input_position: QuickInputPosition::default(),
        }
    }
}

/// Which of the Explorer's stacked sub-views are shown, toggled from the
/// "Views and More Actions" (⋯) menu on the EXPLORER header. Mirrors VS Code's
/// defaults: Open Editors hidden, every other view shown. Each field carries
/// its own `#[serde(default)]` so a config written before this block existed
/// still parses straight into these defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorerViewsPrefs {
    #[serde(default)]
    pub open_editors: bool,
    #[serde(default = "default_true")]
    pub folders: bool,
    #[serde(default = "default_true")]
    pub outline: bool,
    #[serde(default = "default_true")]
    pub timeline: bool,
    /// The DEPENDENCIES view. `alias` keeps configs written under the old
    /// Rust-only `rust_dependencies` key parsing into this generalized field.
    #[serde(default = "default_true", alias = "rust_dependencies")]
    pub dependencies: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ExplorerViewsPrefs {
    fn default() -> Self {
        Self {
            open_editors: false,
            folders: true,
            outline: true,
            timeline: true,
            dependencies: true,
        }
    }
}

/// One per-host pane accent rule (iTerm2's automatic profile switching,
/// scoped to what a TUI can dress): panes whose shell-reported hostname
/// (OSC 7) matches `pattern` (glob, e.g. "prod-*") wear `accent` (hex
/// `#rrggbb`, danger red when absent) on their border and name pill, plus
/// a translucent `badge` watermark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostAccentRule {
    pub pattern: String,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub badge: Option<String>,
}

/// One notification sink (#358): where an event goes and which events.
///
/// `kind` is `ntfy` (`topic`, optional `server`), `webhook` (`url`,
/// optional `headers`), `termux` (runs `termux-notification`), or
/// `command` (`argv`, with the notification in `CROFT_*` environment
/// variables). `events` names the kinds it takes — `command_finished`,
/// `tests_failed`, `osc9`, `agent_waiting` — or is empty for all of them;
/// `min_duration_secs` is the threshold for a finished command (default
/// 10). Webhook headers may carry a secret: keep them in
/// `config.local.json`, the machine-local layer that is never synced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NotificationSink {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub min_duration_secs: Option<u64>,
}

/// `#rrggbb` → bytes; anything else is treated as unset.
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// The on-disk preferences document. New fields must default so an older
/// config still parses; `#[serde(default)]` covers that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Prefs {
    /// Active color theme, stored by its stable [`Theme::id`].
    #[serde(default)]
    pub theme: String,
    /// On-screen keyboard split layout (two thumb clusters on foldables),
    /// toggled by the OSK's `split` key.
    #[serde(default)]
    pub osk_split: bool,
    /// When true, the startup "switch to iTerm2/Ghostty" nudge shown in
    /// terminals that can't render croft's inline images is silenced (the
    /// user dismissed it with "don't show again").
    #[serde(default)]
    pub suppress_terminal_warning: bool,
    /// Visibility of the Explorer's stacked sub-views (⋯ menu toggles).
    #[serde(default)]
    pub explorer_views: ExplorerViewsPrefs,
    /// Collapse the sidebar when focus moves to the editor or a terminal
    /// (#260). Off by default: hiding chrome on focus surprises people who
    /// did not ask for it, so this is opt-in.
    #[serde(default)]
    pub sidebar_auto_hide: bool,
    /// Which files the PROBLEMS panel lists (#256): `whole_project` (default,
    /// every file a server or build tool reported) or `open_files`.
    /// Unrecognised values read as `whole_project`, so a typo shows more
    /// rather than silently hiding diagnostics.
    #[serde(default)]
    pub problems_scope: String,
    /// Whether the whole-project check may run without being asked (#256):
    /// `auto` (under a file-count cap), `on`, or `off`. Unrecognised values
    /// read as `auto`, so a typo cannot start a compile on a huge root.
    #[serde(default)]
    pub problems_project_scope: String,
    /// Default whitespace handling for new diff views: `off` (every byte
    /// counts), `leading` (ignore indentation changes), or `all` (ignore every
    /// whitespace-only difference). Unrecognised values read as `off`, so a
    /// typo degrades to the safe, byte-exact view rather than hiding changes.
    /// Per-view toggling never writes back here.
    #[serde(default)]
    pub diff_ignore_whitespace: String,
    /// Extension ids the user has disabled in the Extensions panel. Absent or
    /// empty means every bundled and installed extension is enabled (the
    /// default), so disabling is opt-in and an older config still parses.
    #[serde(default)]
    pub disabled_extensions: BTreeSet<String>,
    /// MCP sidecar extension ids the user has consented to spawn (the first-run
    /// consent gate). croft never launches a sidecar process until its extension
    /// id is in this set.
    #[serde(default)]
    pub mcp_consented: BTreeSet<String>,
    /// Trust-on-first-use tool fingerprints, keyed by command id. Recorded the
    /// first time a command's tool is called; a later mismatch (a silent tool
    /// rug-pull) makes croft refuse the call. See
    /// [`crate::mcp::client::tool_fingerprint`].
    #[serde(default)]
    pub mcp_tool_fingerprints: BTreeMap<String, String>,
    /// Layout chrome chosen in the "Customize Layout" popup.
    #[serde(default)]
    pub layout: LayoutPrefs,
    /// When true, saving a file first asks the language server to format it
    /// (VS Code's `editor.formatOnSave`). Off by default, matching VS Code, so
    /// an older config parses straight to disabled.
    #[serde(default)]
    pub format_on_save: bool,
    /// When true, typing one of the language server's on-type trigger
    /// characters (`}`, `;`, newline, per its capability) runs
    /// `textDocument/onTypeFormatting` on the just-typed spot (VS Code's
    /// `editor.formatOnType`). Off by default, matching VS Code.
    #[serde(default)]
    pub format_on_type: bool,
    /// When true, a dirty buffer writes itself to disk about a second after
    /// the last edit (VS Code's `files.autoSave: afterDelay`). Off by
    /// default, matching VS Code.
    #[serde(default)]
    pub auto_save: bool,
    /// When true, a dirty buffer writes itself the moment it loses focus
    /// (VS Code's `files.autoSave: onFocusChange`): the editor pane losing
    /// focus, or the active tab changing. Independent of `auto_save`, so
    /// both can be on. Off by default, matching VS Code.
    #[serde(default)]
    pub auto_save_on_focus_change: bool,
    /// Opt-out for the GitLens-style current-line inline blame annotation,
    /// which is on by default. Stored as the disable flag (like
    /// `suppress_terminal_warning`) so the derived `Default` and an older
    /// config both mean "blame shown".
    #[serde(default)]
    pub disable_inline_blame: bool,
    /// Auto-closing pairs (#121) are ON by default; this stores the opt-out
    /// (a default-false field keeps old configs valid, like inline blame).
    #[serde(default)]
    pub disable_auto_close_pairs: bool,
    /// Opt-out for debugger inline values (#135), on by default like VS
    /// Code's `debug.inlineValues: "auto"`. Stored as the disable flag so
    /// the derived `Default` and an older config both mean "values shown".
    #[serde(default)]
    pub disable_inline_values: bool,
    /// Whitespace rendering mode (#133): "selection" (VS Code's default,
    /// also what the empty string an older config deserializes to means),
    /// "all", or "none". Parsed by `WhitespaceMode::from_pref`.
    #[serde(default)]
    pub render_whitespace: String,
    /// Opt-out for bracket-pair colorization (#131), on by default like VS
    /// Code's `editor.bracketPairColorization.enabled` (default since 1.67).
    /// Stored as the disable flag so the derived `Default` and an older
    /// config both mean "brackets coloured".
    #[serde(default)]
    pub disable_bracket_colors: bool,
    /// Opt-out for indentation guides (#129), which are on by default like
    /// VS Code's `editor.guides.indentation`. Stored as the disable flag so
    /// the derived `Default` and an older config both mean "guides shown".
    #[serde(default)]
    pub disable_indent_guides: bool,
    /// Opt-out for LSP inlay hints (inline type / parameter annotations),
    /// which are on by default like VS Code's `editor.inlayHints.enabled`.
    /// Stored as the disable flag so the derived `Default` and an older
    /// config both mean "hints shown".
    #[serde(default)]
    pub disable_inlay_hints: bool,
    /// Opt-out for the navigator's proactive comment-only looks (a newly
    /// completed construct plus a typing pause hands it the floor). On by
    /// default while a navigator is seated; stored as the disable flag so
    /// the derived `Default` and an older config both mean "proactive".
    #[serde(default)]
    pub disable_proactive_navigator: bool,
    /// Copy a finished terminal mouse selection straight to the clipboard
    /// (VS Code's `terminal.integrated.copyOnSelection`). Off by default,
    /// matching VS Code.
    #[serde(default)]
    pub copy_on_select: bool,
    /// Opt-out for tailspin highlighting in the rendered log view (#466):
    /// dates, numbers, UUIDs, IPs, URLs, paths, quotes and severity keywords
    /// are coloured on lines that carry no colour of their own. Stored as
    /// the disable flag so the derived `Default` and an older config both
    /// mean "highlighted".
    #[serde(default)]
    pub disable_log_highlight: bool,
    /// Opt-out for the built-in secret redaction rules (#360): AWS keys,
    /// `sk-`/`ghp_`/`xox` tokens, JWTs and bearer tokens are masked in
    /// terminal panes by default. Stored as the disable flag so the
    /// derived `Default` and an older config both mean "masked".
    #[serde(default)]
    pub disable_secret_redaction: bool,
    /// Per-host pane accent rules; see [`HostAccentRule`].
    #[serde(default)]
    pub host_accents: Vec<HostAccentRule>,
    /// Opt-out for the ssh-pane workspace offer (#364): when a pane's
    /// foreground becomes `ssh <host>` for a host in `~/.ssh/config`, the
    /// status bar offers to open the workspace on that host. Stored as the
    /// disable flag so the derived `Default` and an older config both mean
    /// "offer". User layers only: an offer is a prompt to connect somewhere.
    #[serde(default)]
    pub disable_remote_offer: bool,
    /// Hosts (ssh config aliases, matched case-insensitively) the offer
    /// never prompts for (#364): the per-host `auto_offer = off`. A jump
    /// host, a box you only ever tunnel through. User layers only.
    #[serde(default)]
    pub remote_offer_excluded_hosts: Vec<String>,
    /// Named sets of SSH hosts for "Terminal: Fleet Run" (#363), so a fleet
    /// can be named once rather than retyped per run.
    ///
    /// User layers only, and for the same reason as the list above: a fleet
    /// run executes arbitrary text on every host in the set, so a group is a
    /// list of machines to run commands on. A repo-controlled layer naming
    /// one could add a production box to a group the user then broadcasts
    /// to. `WORKSPACE_ALLOWED_KEYS` refuses it, and the refusal is visible
    /// rather than silent.
    ///
    /// Ordered, so the hosts a group names come back in the same order on
    /// every run: a HashMap would shuffle them, and a fleet comparison is
    /// read by scanning down the list.
    #[serde(default)]
    pub fleet_groups: std::collections::BTreeMap<String, Vec<String>>,
    /// The agent a new worktree lane starts in its pane (#348): the name of
    /// an `agents.json` row (built in: claude, codex, aider, gemini), whose
    /// `launch` line is typed into the lane's fresh shell. Unset, a lane
    /// opens a plain shell. User layers only: it names a command to run.
    #[serde(default)]
    pub lane_agent: Option<String>,
    /// Scrollback lines kept per terminal pane (VS Code's
    /// `terminal.integrated.scrollback`). 0 — the default for older configs —
    /// means the built-in 5000. Applies to panes opened after the change.
    #[serde(default)]
    pub terminal_scrollback: usize,
    /// What exported navigator comments start with (#368), marking them as
    /// AI-authored on GitHub. Unset means `[AI, croft navigator] `; an
    /// empty string turns the marker off.
    #[serde(default)]
    pub review_ai_prefix: Option<String>,
    /// Screen reader mode (#621): a steady caret kept on the focused text
    /// and a one-line description of each change in the status bar. Off by
    /// default.
    #[serde(default)]
    pub screen_reader: bool,
    /// A speech program croft runs with each screen reader line as its last
    /// argument (`spd-say`, `say`). User layers only: it names a command.
    #[serde(default)]
    pub screen_reader_command: Option<String>,
    /// UI language (#621), such as `de` or `es`. Unset, croft follows
    /// `LC_ALL` / `LC_MESSAGES` / `LANG`.
    #[serde(default)]
    pub locale: Option<String>,
    /// Notification sinks; see [`NotificationSink`]. User layers only: not
    /// in `WORKSPACE_ALLOWED_KEYS`, so a cloned repo cannot make croft run
    /// a command or post to a URL.
    #[serde(default)]
    pub notifications: Vec<NotificationSink>,
    /// The file as it was read, kept so a save rewrites only what changed.
    #[serde(skip)]
    pub(crate) source: SourceDoc,
}

/// The raw `config.json` object a [`Prefs`] was loaded from. The settings
/// loader reads keys this struct has no field for (`extends`, the
/// `macos` / `linux` / `android` blocks, settings of newer versions), so a
/// save that wrote the struct back alone deleted them. Ignored by equality:
/// two `Prefs` with the same settings are equal wherever they came from.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceDoc(Option<serde_json::Map<String, serde_json::Value>>);

impl PartialEq for SourceDoc {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for SourceDoc {}

/// Write into `doc` (the file as read) what changed between `loaded` (its
/// typed view then) and `current` (the settings now), key by key and into
/// nested objects: a changed `layout` field must not drop keys a newer
/// version put inside `layout`. A key the current settings no longer
/// serialize (an option cleared) leaves the file too.
fn merge_changed(
    doc: &mut serde_json::Map<String, serde_json::Value>,
    loaded: &serde_json::Value,
    current: &serde_json::Value,
) {
    use serde_json::Value;
    let (Value::Object(loaded), Value::Object(current)) = (loaded, current) else {
        return;
    };
    for key in loaded.keys().filter(|k| !current.contains_key(*k)) {
        doc.remove(key);
    }
    for (key, value) in current {
        let before = loaded.get(key);
        if before == Some(value) {
            continue;
        }
        match (doc.get_mut(key), before, value) {
            (Some(Value::Object(inner)), Some(b @ Value::Object(_)), Value::Object(_)) => {
                merge_changed(inner, b, value);
            }
            _ => {
                doc.insert(key.clone(), value.clone());
            }
        }
    }
}

/// The `config.json` that [`Prefs::load_or_default`] reads, or `None` in
/// test builds: the user's real `config.json` must never steer a test
/// (#624), the same rule `config_layers::load_merged` applies to the
/// settings layers.
fn saved_prefs_path() -> Option<PathBuf> {
    (!cfg!(test)).then(config_path)
}

impl Prefs {
    /// Load preferences from `config_path()`, falling back to defaults when
    /// the file is absent or unreadable. Preferences are best-effort: a
    /// corrupt config should never block startup. Test builds always get
    /// the defaults (see [`saved_prefs_path`]).
    pub fn load_or_default() -> Self {
        saved_prefs_path()
            .and_then(|p| Self::load(&p).ok())
            .unwrap_or_default()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&json).context("parsing prefs")
    }

    /// Parse a `config.json`, which is JSONC like every other settings file
    /// croft reads: the layer loader accepts comments and trailing commas,
    /// so a strict parse here threw away every setting in a file that
    /// merely had a comment, re-enabling disabled extensions among others.
    fn parse(json: &str) -> serde_json::Result<Self> {
        let value: serde_json::Value = serde_json::from_str(&crate::tasks::strip_jsonc(json))?;
        let mut prefs: Self = serde_json::from_value(value.clone())?;
        if let serde_json::Value::Object(map) = value {
            prefs.source = SourceDoc(Some(map));
        }
        Ok(prefs)
    }

    /// The preferences a read-modify-write starts from: defaults only when
    /// the file does not exist. A file that exists but can't be read or
    /// parsed is an error, so saving one toggle never replaces the user's
    /// other settings with defaults.
    pub fn load_for_update(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(json) => Self::parse(&json).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.document()?).context("serializing prefs")?;
        // Written aside and renamed in: a reader on another thread (the MCP
        // worker's fingerprint check) must never see a truncated file, and a
        // crash mid-write must not leave one.
        let tmp = path.with_extension(format!(
            "json.{}.{:?}.tmp",
            std::process::id(),
            std::thread::current().id()
        ));
        write_keeping_mode(&tmp, path, json.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| {
            let _ = std::fs::remove_file(&tmp);
            format!("replacing {}", path.display())
        })
    }

    /// What [`Prefs::save`] writes: the loaded file with only the settings
    /// that changed since it was read replaced, so keys this struct does not
    /// model survive. A value is compared against the file's own typed view,
    /// so an untouched setting is left exactly as the user wrote it.
    fn document(&self) -> Result<serde_json::Value> {
        let current = serde_json::to_value(self).context("serializing prefs")?;
        let Some(mut doc) = self.source.0.clone() else {
            return Ok(current);
        };
        let loaded: Self = serde_json::from_value(serde_json::Value::Object(doc.clone()))
            .context("re-reading prefs")?;
        let loaded = serde_json::to_value(&loaded).context("serializing prefs")?;
        merge_changed(&mut doc, &loaded, &current);
        Ok(serde_json::Value::Object(doc))
    }

    pub fn theme(&self) -> Theme {
        Theme::from_id(&self.theme)
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme.id().to_string();
    }
}

/// Persist `theme` to the config file, preserving any other settings already
/// stored. Best-effort: a write failure is swallowed by the caller.
pub fn save_theme(theme: Theme) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.set_theme(theme);
    prefs.save(&path)
}

/// Persist the on-screen keyboard split choice, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_osk_split(split: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.osk_split = split;
    prefs.save(&path)
}

/// Persist the "don't warn about this terminal again" choice, preserving
/// other settings. Best-effort: a write failure is swallowed by the caller.
pub fn save_suppress_terminal_warning(suppress: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.suppress_terminal_warning = suppress;
    prefs.save(&path)
}

/// Persist the Explorer sub-view visibility set, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_explorer_views(views: ExplorerViewsPrefs) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.explorer_views = views;
    prefs.save(&path)
}

/// Write `bytes` to `tmp`, created no more readable than `dest` already is
/// (0600 when `dest` is new): the file replaces `dest`, which may hold
/// notification headers, and must not widen to the umask's 0644.
pub(crate) fn write_keeping_mode(tmp: &Path, dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    // Created fresh, never opened through what is already there: the temp
    // names are predictable, and create+truncate followed a symlink planted
    // at one, writing the bytes wherever it pointed. `create_new` (O_EXCL)
    // refuses a symlink, and a stale file is removed first.
    let _ = std::fs::remove_file(tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mode = std::fs::metadata(dest).map_or(0o600, |m| m.permissions().mode() & 0o777);
        opts.mode(mode);
        mode
    };
    let mut file = opts.open(tmp)?;
    // `mode` at creation is still narrowed by the umask: set it outright.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    file.write_all(bytes)
}

/// The settings file under `config_dir`. The real config dir means the
/// active profile's file (#618), which is what startup reads; saving
/// consent or disabled extensions to the base file instead let a revoked
/// consent survive a restart under a profile.
fn prefs_file_in(config_dir: &Path) -> PathBuf {
    if config_dir == self::config_dir() {
        config_path()
    } else {
        config_dir.join("config.json")
    }
}

/// Persist the set of disabled extension ids, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_disabled_extensions_in(config_dir: &Path, disabled: &BTreeSet<String>) -> Result<()> {
    let path = prefs_file_in(config_dir);
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.disabled_extensions = disabled.clone();
    prefs.save(&path)
}

/// Record a first-run consent for `ext_id` under an explicit config dir: the app carries the
/// dir it was built with, so a test can point it at a scratch dir instead
/// of mutating the process-wide environment (which races sibling tests).
pub fn save_mcp_consent_in(config_dir: &Path, ext_id: &str) -> Result<()> {
    let path = prefs_file_in(config_dir);
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.mcp_consented.insert(ext_id.to_string());
    prefs.save(&path)
}

/// Forget a recorded first-run consent under an explicit config dir; see
/// [`save_mcp_consent_in`] for why the dir is a parameter.
pub fn forget_mcp_consent_in(config_dir: &Path, ext_id: &str) -> Result<()> {
    let path = prefs_file_in(config_dir);
    let mut prefs = Prefs::load_for_update(&path)?;
    if !prefs.mcp_consented.remove(ext_id) {
        return Ok(());
    }
    prefs.save(&path)
}

/// Trust-on-first-use check of the tool a command calls, under an explicit
/// config dir (see [`save_mcp_consent_in`] for why the dir is a parameter).
/// The first fingerprint seen for `command_id` is recorded, preserving other
/// settings (best-effort: a write failure is swallowed); false when a
/// different one was recorded before, meaning the tool definition changed.
pub fn trust_mcp_tool_in(config_dir: &Path, command_id: &str, fingerprint: &str) -> bool {
    let path = prefs_file_in(config_dir);
    // An unreadable config can't vouch for a fingerprint, and saving over
    // it would lose every other setting: refuse the call instead.
    let Ok(mut prefs) = Prefs::load_for_update(&path) else {
        return false;
    };
    match prefs.mcp_tool_fingerprints.get(command_id) {
        Some(prev) => prev == fingerprint,
        None => {
            prefs
                .mcp_tool_fingerprints
                .insert(command_id.to_string(), fingerprint.to_string());
            let _ = prefs.save(&path);
            true
        }
    }
}

/// Forget the recorded tool fingerprints of `command_ids`, so each tool is
/// trusted afresh on its next run; see [`trust_mcp_tool_in`].
pub fn forget_mcp_tool_fingerprints_in(config_dir: &Path, command_ids: &[String]) -> Result<()> {
    let path = prefs_file_in(config_dir);
    let mut prefs = Prefs::load_for_update(&path)?;
    let before = prefs.mcp_tool_fingerprints.len();
    prefs
        .mcp_tool_fingerprints
        .retain(|id, _| !command_ids.contains(id));
    if prefs.mcp_tool_fingerprints.len() == before {
        return Ok(());
    }
    prefs.save(&path)
}

/// Persist the Customize Layout chrome choices, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_layout(layout: LayoutPrefs) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.layout = layout;
    prefs.save(&path)
}

/// Persist the auto-close-pairs choice, preserving other settings. Stored
/// negated (`disable_auto_close_pairs`) so an absent key means enabled.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_auto_close_pairs(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.disable_auto_close_pairs = !enabled;
    prefs.save(&path)
}

/// Persist the screen reader choice, preserving other settings.
pub fn save_screen_reader(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.screen_reader = enabled;
    prefs.save(&path)
}

/// Persist the format-on-type choice, preserving other settings.
/// Best-effort: a write failure is swallowed by the caller.
pub fn save_format_on_type(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.format_on_type = enabled;
    prefs.save(&path)
}

/// Persist the format-on-save choice, preserving other settings. Best-effort:
/// a write failure is swallowed by the caller.
pub fn save_format_on_save(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.format_on_save = enabled;
    prefs.save(&path)
}

/// Persist the secret-redaction toggle (#360), preserving other settings.
pub fn save_disable_secret_redaction(disabled: bool) -> Result<()> {
    set_disable_secret_redaction(&config_path(), disabled)
}

/// [`save_disable_secret_redaction`] against an explicit path. Goes through
/// [`prefs_for_update`] so a malformed config is refused, not replaced.
fn set_disable_secret_redaction(path: &Path, disabled: bool) -> Result<()> {
    let mut prefs = prefs_for_update(path)?;
    prefs.disable_secret_redaction = disabled;
    prefs.save(path)
}

/// Persist the log-highlight toggle (#466) as its disable flag. Goes through
/// [`prefs_for_update`] so a malformed config is refused, not replaced.
pub fn save_disable_log_highlight(disabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = prefs_for_update(&path)?;
    prefs.disable_log_highlight = disabled;
    prefs.save(&path)
}

pub fn save_copy_on_select(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.copy_on_select = enabled;
    prefs.save(&path)
}

/// The scrollback size terminal panes spawn with: the user's
/// `terminal_scrollback` when set, clamped to a sane range, else the
/// built-in default.
pub fn terminal_scrollback_lines(configured: usize, default: usize) -> usize {
    if configured == 0 {
        default
    } else {
        configured.clamp(100, 200_000)
    }
}

/// Persist the auto-save choice, preserving other settings. Best-effort:
/// a write failure is swallowed by the caller.
pub fn save_auto_save(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.auto_save = enabled;
    prefs.save(&path)
}

/// Persist the on-focus-change auto-save choice, preserving other
/// settings. Best-effort, like [`save_auto_save`].
pub fn save_auto_save_on_focus_change(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.auto_save_on_focus_change = enabled;
    prefs.save(&path)
}

/// Persist the auto-hide side bar choice (#260), preserving other settings.
/// Best-effort, like [`save_auto_save`].
pub fn save_sidebar_auto_hide(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = prefs_for_update(&path)?;
    prefs.sidebar_auto_hide = enabled;
    prefs.save(&path)
}

/// The prefs to mutate in a `save_*` helper: the file's contents, or defaults
/// only when the file is genuinely ABSENT.
///
/// `Prefs::load` cannot tell "no config yet" from "config is malformed" —
/// both are `Err` — so `unwrap_or_default()` on a corrupt file would write
/// defaults over it and erase every unrelated setting the user had. A first
/// run must still work, so absence maps to defaults; anything else refuses,
/// leaving the file for the user to fix.
fn prefs_for_update(path: &Path) -> Result<Prefs> {
    match Prefs::load(path) {
        Ok(p) => Ok(p),
        // Branch on the error itself rather than a follow-up `path.exists()`:
        // a dangling symlink at the config path reports "does not exist" while
        // the open failed for a different reason, and defaults written through
        // it would clobber whatever it points at. `load` wraps the io error in
        // context, so the cause is in the chain rather than the top error.
        Err(e) => {
            let missing = e
                .chain()
                .filter_map(|c| c.downcast_ref::<std::io::Error>())
                .any(|io| io.kind() == std::io::ErrorKind::NotFound);
            if missing {
                return Ok(Prefs::default());
            }
            Err(e)
        }
    }
}

/// Persist whether the whole-project check may auto-run (#256).
/// Best-effort, like [`save_problems_scope`].
pub fn save_problems_project_scope(mode: &str) -> Result<()> {
    let path = config_path();
    let mut prefs = prefs_for_update(&path)?;
    prefs.problems_project_scope = mode.to_string();
    prefs.save(&path)
}

/// Persist the PROBLEMS scope (#256), preserving other settings.
/// Best-effort, like [`save_auto_save`].
pub fn save_problems_scope(mode: &str) -> Result<()> {
    let path = config_path();
    let mut prefs = prefs_for_update(&path)?;
    prefs.problems_scope = mode.to_string();
    prefs.save(&path)
}

pub fn save_inline_blame(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.disable_inline_blame = !enabled;
    prefs.save(&path)
}

/// Persist the debugger inline-values toggle (stored as its disable flag).
pub fn save_inline_values(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.disable_inline_values = !enabled;
    prefs.save(&path)
}

/// Persist the whitespace rendering mode ("selection" / "all" / "none").
pub fn save_render_whitespace(mode: &str) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.render_whitespace = mode.to_string();
    prefs.save(&path)
}

/// Persist the bracket-pair colorization toggle (stored as its disable flag).
pub fn save_bracket_colors(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.disable_bracket_colors = !enabled;
    prefs.save(&path)
}

/// Persist the indentation-guides toggle (stored as its disable flag).
pub fn save_indent_guides(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.disable_indent_guides = !enabled;
    prefs.save(&path)
}

pub fn save_inlay_hints(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.disable_inlay_hints = !enabled;
    prefs.save(&path)
}

pub fn save_proactive_navigator(enabled: bool) -> Result<()> {
    let path = config_path();
    let mut prefs = Prefs::load_for_update(&path)?;
    prefs.disable_proactive_navigator = !enabled;
    prefs.save(&path)
}

pub fn config_path() -> PathBuf {
    // Through the active profile (#618); the config dir when there is none.
    crate::profiles::file("config.json")
}

pub(crate) fn config_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("croft");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("croft")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_keeps_unknown_keys_nested_in_a_changed_object_and_drops_removed_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"explorer_views": {"open_editors": false, "future_view": true},
                "mcp_tool_fingerprints": {"a": "1", "b": "2"}}"#,
        )
        .unwrap();
        let mut prefs = Prefs::load(&path).unwrap();
        prefs.explorer_views.open_editors = true;
        prefs.mcp_tool_fingerprints.remove("a");
        prefs.save(&path).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["explorer_views"]["open_editors"], true);
        assert_eq!(doc["explorer_views"]["future_view"], true);
        assert!(doc["mcp_tool_fingerprints"].get("a").is_none(), "{doc}");
        assert_eq!(doc["mcp_tool_fingerprints"]["b"], "2");
    }

    #[test]
    fn saving_one_setting_keeps_keys_prefs_does_not_model_and_accepts_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
  // shared base
  "extends": "base.json",
  "macos": {"theme": "light"},
  "future_setting": 7,
  "theme": "dark",
  "osk_split": true,
}"#,
        )
        .unwrap();
        // A comment and trailing comma no longer reset everything.
        assert!(Prefs::load(&path).unwrap().osk_split);
        save_mcp_consent_in(dir.path(), "ext").unwrap();
        set_disable_secret_redaction(&path, true).unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["extends"], "base.json");
        assert_eq!(doc["macos"]["theme"], "light");
        assert_eq!(doc["future_setting"], 7);
        assert_eq!(doc["theme"], "dark");
        assert_eq!(doc["osk_split"], true);
        assert_eq!(doc["disable_secret_redaction"], true);
        let prefs = Prefs::load(&path).unwrap();
        assert!(prefs.mcp_consented.contains("ext"));
        assert!(prefs.disable_secret_redaction);
    }

    #[test]
    fn saving_prefs_replaces_the_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        save_mcp_consent_in(dir.path(), "ext").unwrap();
        assert!(
            Prefs::load_for_update(&path)
                .unwrap()
                .mcp_consented
                .contains("ext")
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("config.json")]);
        assert_eq!(prefs_file_in(dir.path()), path);
    }

    #[cfg(unix)]
    #[test]
    fn saving_prefs_keeps_a_private_config_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        save_mcp_consent_in(dir.path(), "ext").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// #624: a test build reads no saved `config.json`, so the MCP
    /// consents, the terminal warning and the extension toggles that
    /// `load_or_default` feeds are the defaults on every machine.
    #[test]
    fn a_test_build_reads_no_saved_prefs() {
        assert_eq!(saved_prefs_path(), None);
        assert_eq!(Prefs::load_or_default(), Prefs::default());
    }

    #[test]
    fn an_mcp_tool_is_trusted_on_first_use_and_refused_once_it_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        save_mcp_consent_in(dir, "ext").unwrap();
        assert!(trust_mcp_tool_in(dir, "ext.cmd", "fp1"));
        assert!(trust_mcp_tool_in(dir, "ext.cmd", "fp1"));
        assert!(!trust_mcp_tool_in(dir, "ext.cmd", "fp2"));
        let saved = Prefs::load(&dir.join("config.json")).unwrap();
        assert_eq!(
            saved
                .mcp_tool_fingerprints
                .get("ext.cmd")
                .map(String::as_str),
            Some("fp1")
        );
        assert!(
            saved.mcp_consented.contains("ext"),
            "other settings survive"
        );
    }

    /// Forgetting a command's fingerprint re-approves its tool: the next
    /// fingerprint seen is trusted, and other commands keep theirs.
    #[test]
    fn a_forgotten_fingerprint_is_trusted_afresh() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert!(trust_mcp_tool_in(dir, "ext.cmd", "fp1"));
        assert!(trust_mcp_tool_in(dir, "other.cmd", "o1"));
        forget_mcp_tool_fingerprints_in(dir, &[String::from("ext.cmd")]).unwrap();
        assert!(trust_mcp_tool_in(dir, "ext.cmd", "fp2"));
        assert!(!trust_mcp_tool_in(dir, "other.cmd", "o2"));
    }

    #[test]
    fn round_trips_theme_through_disk() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let mut prefs = Prefs::default();
        prefs.set_theme(Theme::BLACK);
        prefs.save(&path).expect("save");
        let loaded = Prefs::load(&path).expect("load");
        assert_eq!(loaded.theme(), Theme::BLACK);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_osk_split_and_old_configs_default_to_merged() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-osk-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            osk_split: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").osk_split);
        // A pre-split config (theme only) still parses, defaulting to merged.
        std::fs::write(&path, r#"{"theme":"black"}"#).expect("write old config");
        assert!(!Prefs::load(&path).expect("load old").osk_split);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_suppress_terminal_warning_and_old_configs_default_off() {
        let dir =
            std::env::temp_dir().join(format!("croft-prefs-warn-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            suppress_terminal_warning: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").suppress_terminal_warning);
        // A config written before this field existed still parses, defaulting
        // the nudge to enabled (false = not suppressed).
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        assert!(
            !Prefs::load(&path)
                .expect("load old")
                .suppress_terminal_warning
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_explorer_views_and_old_configs_default_to_vscode_defaults() {
        let dir =
            std::env::temp_dir().join(format!("croft-prefs-views-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            explorer_views: ExplorerViewsPrefs {
                open_editors: true,
                timeline: false,
                ..ExplorerViewsPrefs::default()
            },
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        let loaded = Prefs::load(&path).expect("load").explorer_views;
        assert!(loaded.open_editors);
        assert!(!loaded.timeline);
        assert!(loaded.folders);
        // A config written before the field existed parses to the VS Code
        // defaults: Open Editors hidden, every other view shown.
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        let old = Prefs::load(&path).expect("load old").explorer_views;
        assert_eq!(old, ExplorerViewsPrefs::default());
        assert!(!old.open_editors);
        assert!(old.folders && old.outline && old.timeline && old.dependencies);
        // A config written under the pre-generalization `rust_dependencies` key
        // still drives the renamed `dependencies` field via the serde alias.
        std::fs::write(&path, r#"{"explorer_views":{"rust_dependencies":false}}"#)
            .expect("write legacy config");
        let legacy = Prefs::load(&path).expect("load legacy").explorer_views;
        assert!(!legacy.dependencies, "old rust_dependencies key aliases in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_layout_and_old_configs_default_to_full_chrome() {
        let dir =
            std::env::temp_dir().join(format!("croft-prefs-layout-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            layout: LayoutPrefs {
                activity_bar: false,
                status_bar: false,
                side_bar_position: SideBarPosition::Right,
                secondary_side_bar: true,
                panel_alignment: PanelAlignment::Justify,
                quick_input_position: QuickInputPosition::Center,
            },
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        let loaded = Prefs::load(&path).expect("load").layout;
        assert_eq!(loaded, prefs.layout);
        // A config written before this block existed parses to full chrome:
        // activity bar + status bar shown, side bar left, no secondary bar,
        // panel centered, quick input on top.
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        let old = Prefs::load(&path).expect("load old").layout;
        assert_eq!(old, LayoutPrefs::default());
        assert!(old.activity_bar && old.status_bar);
        assert_eq!(old.side_bar_position, SideBarPosition::Left);
        assert!(!old.secondary_side_bar);
        assert_eq!(old.panel_alignment, PanelAlignment::Center);
        assert_eq!(old.quick_input_position, QuickInputPosition::Top);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_format_on_save_and_old_configs_default_off() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-fos-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            format_on_save: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").format_on_save);
        // A config written before this field existed still parses, defaulting
        // format-on-save to off (matching VS Code's `editor.formatOnSave`).
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        assert!(!Prefs::load(&path).expect("load old").format_on_save);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_format_on_type_and_old_configs_default_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let mut prefs = Prefs::default();
        assert!(!prefs.format_on_type, "off by default, matching VS Code");
        prefs.format_on_type = true;
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").format_on_type);
        std::fs::write(&path, "{}").expect("write old config");
        assert!(!Prefs::load(&path).expect("load old").format_on_type);
    }

    #[test]
    fn round_trips_inlay_hints_and_old_configs_default_on() {
        let dir =
            std::env::temp_dir().join(format!("croft-prefs-inlay-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            disable_inlay_hints: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").disable_inlay_hints);
        // A config written before this field existed still parses; hints
        // default ON (matching VS Code's `editor.inlayHints.enabled`).
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        assert!(!Prefs::load(&path).expect("load old").disable_inlay_hints);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_auto_save_and_old_configs_default_off() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-as-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            auto_save: true,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        assert!(Prefs::load(&path).expect("load").auto_save);
        // A config written before this field existed still parses, defaulting
        // auto-save to off (matching VS Code's `files.autoSave`).
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        assert!(!Prefs::load(&path).expect("load old").auto_save);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_copy_on_select_and_terminal_scrollback() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-cos-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let prefs = Prefs {
            copy_on_select: true,
            terminal_scrollback: 20_000,
            ..Prefs::default()
        };
        prefs.save(&path).expect("save");
        let loaded = Prefs::load(&path).expect("load");
        assert!(loaded.copy_on_select);
        assert_eq!(loaded.terminal_scrollback, 20_000);
        // A config written before these fields existed still parses: copy on
        // selection off (VS Code's default) and the built-in scrollback.
        std::fs::write(&path, r#"{"theme":"dark"}"#).expect("write old config");
        let old = Prefs::load(&path).expect("load old");
        assert!(!old.copy_on_select);
        assert_eq!(old.terminal_scrollback, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terminal_scrollback_lines_defaults_and_clamps() {
        assert_eq!(terminal_scrollback_lines(0, 5000), 5000, "0 = unset");
        assert_eq!(terminal_scrollback_lines(20_000, 5000), 20_000);
        assert_eq!(terminal_scrollback_lines(5, 5000), 100, "floor");
        assert_eq!(terminal_scrollback_lines(9_999_999, 5000), 200_000, "cap");
    }

    #[test]
    fn missing_file_yields_default_theme() {
        let path = std::env::temp_dir().join("croft-prefs-absent-xyz/config.json");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(
            Prefs::load(&path).unwrap_or_default().theme(),
            Theme::default()
        );
    }

    #[test]
    fn config_path_lives_under_config_croft() {
        // With XDG unset the path falls under ~/.config/croft; with it set it
        // honors the override. Exercise the default branch via a cleared env.
        let p = config_path();
        assert!(p.to_string_lossy().contains("croft"));
        assert_eq!(p.file_name().unwrap(), "config.json");
    }

    /// #294 review: a `save_*` helper must not overwrite a MALFORMED config
    /// with defaults — that silently erases every unrelated setting the user
    /// had. `Prefs::load` returns `Err` for both "absent" and "corrupt", so
    /// the distinction has to be made explicitly.
    #[test]
    fn an_update_refuses_a_corrupt_config_but_defaults_when_absent() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        // Absent: a first run must still work.
        let _ = std::fs::remove_file(&path);
        assert!(
            prefs_for_update(&path).is_ok(),
            "no config yet is not an error"
        );

        // Present but corrupt: refuse rather than clobber.
        std::fs::write(&path, "{ this is not json").unwrap();
        assert!(
            prefs_for_update(&path).is_err(),
            "a malformed config must not be replaced by defaults"
        );
        // The file is left exactly as the user wrote it.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ this is not json"
        );

        // Present and valid: the real contents come back.
        let p = Prefs {
            theme: String::from("some-theme"),
            ..Default::default()
        };
        p.save(&path).unwrap();
        assert_eq!(prefs_for_update(&path).unwrap().theme, "some-theme");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #360: toggling redaction on a malformed config refuses and leaves
    /// the file byte-for-byte; on a valid one it keeps the other settings.
    #[test]
    fn the_redaction_toggle_never_clobbers_a_corrupt_config() {
        let dir = std::env::temp_dir().join(format!("croft-prefs-redact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        std::fs::write(&path, "{ this is not json").unwrap();
        assert!(
            set_disable_secret_redaction(&path, true).is_err(),
            "a malformed config is refused, not overwritten with defaults"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ this is not json"
        );

        let p = Prefs {
            theme: String::from("some-theme"),
            ..Default::default()
        };
        p.save(&path).unwrap();
        set_disable_secret_redaction(&path, true).unwrap();
        let back = Prefs::load(&path).unwrap();
        assert!(back.disable_secret_redaction);
        assert_eq!(
            back.theme, "some-theme",
            "unrelated settings survive the toggle"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_one_setting_never_replaces_an_unreadable_config() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, "{ \"theme\": ").unwrap();
        assert!(Prefs::load_for_update(&path).is_err());
        assert!(
            Prefs::load_for_update(&tmp.path().join("absent.json")).is_ok(),
            "a missing file starts from defaults"
        );
        std::fs::write(&path, "{\n  // a comment\n  \"format_on_save\": true,\n}").unwrap();
        assert!(
            Prefs::load_for_update(&path).unwrap().format_on_save,
            "JSONC reads"
        );
    }
}
