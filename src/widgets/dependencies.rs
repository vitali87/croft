//! The Explorer's DEPENDENCIES section: a collapsible, language-aware list of
//! the packages the workspace depends on, mirroring the dependency views the
//! three reference editors contribute. The key behaviour, shared by VS Code
//! (rust-analyzer's "Rust Dependencies"), Zed, and Neovim (crates.nvim /
//! package-info.nvim), is that the view is **gated on the project's manifest**
//! and is **specific to whichever manifest exists** — it never shows a fixed
//! language's dependencies in an unrelated folder.
//!
//! So this module first detects the workspace's ecosystem(s) from the manifest
//! files in its root ([`detect_ecosystems`]), then resolves the dependency set
//! per ecosystem off the render thread ([`fetch_dependencies`]): `cargo
//! metadata` for Rust, and a direct read of the manifest for Python / Node /
//! Go. When the root carries no recognised manifest the caller hides the
//! section entirely (and drops it from the ⋯ menu), so opening a plain folder
//! like `~/Documents` shows no dependency view at all.

use std::path::{Path, PathBuf};
use std::process::Command;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};
use serde::Deserialize;

use crate::theme::Theme;
use crate::widgets::scrollbar;

const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_NAME: Color = Color::Rgb(0xCC, 0xCC, 0xCC);
const COLOR_VERSION: Color = Color::Rgb(0x88, 0x9A, 0xC0);

const CONTENT_INDENT: u16 = 1;
/// Codicon `cod-package` — the box glyph VS Code paints on dependency rows.
const PACKAGE_GLYPH: char = '\u{eb29}';

const HEADER_GENERIC: &str = "DEPENDENCIES";
const EMPTY_MESSAGE: &str = "No dependencies";
const LOADING_MESSAGE: &str = "Loading\u{2026}";

/// A package ecosystem croft can list dependencies for, identified by the
/// manifest file it leaves in a project root. The discriminant order is the
/// order dependencies group under a polyglot root's combined list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecosystem {
    Rust,
    Python,
    Node,
    Go,
}

impl Ecosystem {
    /// Title-cased name used in the ⋯ menu toggle, e.g. "Rust Dependencies".
    fn title(self) -> &'static str {
        match self {
            Ecosystem::Rust => "Rust",
            Ecosystem::Python => "Python",
            Ecosystem::Node => "Node",
            Ecosystem::Go => "Go",
        }
    }

    /// Upper-cased name used in the section header, e.g. "RUST DEPENDENCIES".
    fn upper(self) -> &'static str {
        match self {
            Ecosystem::Rust => "RUST",
            Ecosystem::Python => "PYTHON",
            Ecosystem::Node => "NODE",
            Ecosystem::Go => "GO",
        }
    }
}

/// The label for the section header given the detected ecosystems: a single
/// ecosystem names itself ("RUST DEPENDENCIES"), several collapse to the
/// generic "DEPENDENCIES". An empty slice never reaches here — the caller hides
/// the section instead.
pub fn header_label(ecosystems: &[Ecosystem]) -> String {
    match ecosystems {
        [one] => format!("{} {}", one.upper(), HEADER_GENERIC),
        _ => HEADER_GENERIC.to_string(),
    }
}

/// The label for the ⋯-menu toggle given the detected ecosystems: mirrors
/// [`header_label`] in title case ("Rust Dependencies" / "Dependencies").
pub fn menu_label(ecosystems: &[Ecosystem]) -> String {
    match ecosystems {
        [one] => format!("{} Dependencies", one.title()),
        _ => "Dependencies".to_string(),
    }
}

/// One resolved dependency, tagged with the ecosystem it came from so a
/// polyglot root's list groups by language before name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub version: String,
    pub ecosystem: Ecosystem,
}

/// Detect which package ecosystems a workspace root belongs to, by the manifest
/// files present at its top level. Cheap (a handful of `exists` checks, no
/// subprocess), so it is safe to call on every re-root; the result drives both
/// the section's visibility and its header label. Order follows [`Ecosystem`]'s
/// discriminant order, de-duplicated (Python is listed once even with both a
/// `pyproject.toml` and a `requirements.txt`).
pub fn detect_ecosystems(root: &Path) -> Vec<Ecosystem> {
    let mut found = Vec::new();
    if root.join("Cargo.toml").is_file() {
        found.push(Ecosystem::Rust);
    }
    if root.join("pyproject.toml").is_file() || root.join("requirements.txt").is_file() {
        found.push(Ecosystem::Python);
    }
    if root.join("package.json").is_file() {
        found.push(Ecosystem::Node);
    }
    if root.join("go.mod").is_file() {
        found.push(Ecosystem::Go);
    }
    found
}

/// Resolve the workspace's dependency set off the render thread, across every
/// detected ecosystem. Each ecosystem contributes a sorted, de-duplicated block
/// and the blocks concatenate in [`Ecosystem`] order. An empty vec means the
/// manifests parsed to nothing (or the relevant toolchain was unavailable) — the
/// panel renders that as an empty state, never an error.
pub fn fetch_dependencies(root: &Path) -> Vec<Dependency> {
    let mut deps = Vec::new();
    for eco in detect_ecosystems(root) {
        let mut block = match eco {
            Ecosystem::Rust => fetch_rust(root),
            Ecosystem::Python => fetch_python(root),
            Ecosystem::Node => fetch_node(root),
            Ecosystem::Go => fetch_go(root),
        };
        block.sort_by(|a: &Dependency, b: &Dependency| {
            a.name.cmp(&b.name).then(a.version.cmp(&b.version))
        });
        block.dedup();
        deps.append(&mut block);
    }
    deps
}

// --- Rust: `cargo metadata` (resolved, transitive) --------------------------

/// Minimal `cargo metadata` shape — only the fields the dependency list needs.
#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<MetaPackage>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct MetaPackage {
    id: String,
    name: String,
    version: String,
}

/// Absolute path to the `cargo` binary. Prefers one on `PATH`, then the usual
/// rustup / Homebrew locations. A GUI-launched croft inherits the stripped
/// launchd `PATH` that omits `~/.cargo/bin`, so a bare `Command::new("cargo")`
/// silently fails to spawn there (the "No dependencies" bug) even though `git`
/// — which lives in `/usr/bin` — is found. Resolving by absolute path, like
/// croft already does for language servers, makes it launch-agnostic.
pub(crate) fn cargo_binary() -> PathBuf {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("cargo");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".cargo").join("bin").join("cargo");
        if candidate.is_file() {
            return candidate;
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let candidate = PathBuf::from(dir).join("cargo");
        if candidate.is_file() {
            return candidate;
        }
    }
    // Last resort: let the OS resolve it and fail honestly into the empty state.
    PathBuf::from("cargo")
}

fn fetch_rust(root: &Path) -> Vec<Dependency> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let cargo = cargo_binary();
    let mut cmd = Command::new(&cargo);
    cmd.args(["metadata", "--format-version", "1"])
        .current_dir(path_str);
    // Prepend cargo's own dir to the child PATH so the rustup shim can find its
    // sibling `rustc` even when croft launched with a stripped PATH.
    if let Some(dir) = cargo.parent() {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![dir.to_path_buf()];
        dirs.extend(std::env::split_paths(&existing));
        if let Ok(joined) = std::env::join_paths(dirs) {
            cmd.env("PATH", joined);
        }
    }
    let Ok(output) = cmd.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_cargo_metadata(&output.stdout)
}

/// Parse `cargo metadata --format-version 1` JSON into the list of non-workspace
/// dependency crates.
pub fn parse_cargo_metadata(json: &[u8]) -> Vec<Dependency> {
    let Ok(meta) = serde_json::from_slice::<CargoMetadata>(json) else {
        return Vec::new();
    };
    let members: std::collections::HashSet<&str> =
        meta.workspace_members.iter().map(String::as_str).collect();
    meta.packages
        .iter()
        .filter(|p| !members.contains(p.id.as_str()))
        .map(|p| Dependency {
            name: p.name.clone(),
            version: p.version.clone(),
            ecosystem: Ecosystem::Rust,
        })
        .collect()
}

// --- Node: package.json `dependencies` + `devDependencies` ------------------

fn fetch_node(root: &Path) -> Vec<Dependency> {
    let Ok(text) = std::fs::read_to_string(root.join("package.json")) else {
        return Vec::new();
    };
    parse_package_json(&text)
}

/// Parse `package.json`'s `dependencies` and `devDependencies` maps into the
/// direct dependency list (the version is the manifest's range spec, e.g.
/// `^1.2.3`, shown verbatim).
pub fn parse_package_json(text: &str) -> Vec<Dependency> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut deps = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        let Some(map) = value.get(key).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, spec) in map {
            deps.push(Dependency {
                name: name.clone(),
                version: spec.as_str().unwrap_or_default().to_string(),
                ecosystem: Ecosystem::Node,
            });
        }
    }
    deps
}

// --- Go: go.mod `require` directives ----------------------------------------

fn fetch_go(root: &Path) -> Vec<Dependency> {
    let Ok(text) = std::fs::read_to_string(root.join("go.mod")) else {
        return Vec::new();
    };
    parse_go_mod(&text)
}

/// Parse `go.mod`'s `require` directives — both the single-line form
/// (`require module/path v1.2.3`) and the block form (`require ( … )`) — into
/// the module dependency list, dropping any trailing `// indirect` marker.
pub fn parse_go_mod(text: &str) -> Vec<Dependency> {
    let mut deps = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim();
        if in_block {
            if line == ")" {
                in_block = false;
            } else if let Some(dep) = parse_go_require_entry(line) {
                deps.push(dep);
            }
            continue;
        }
        if line == "require (" {
            in_block = true;
        } else if let Some(rest) = line.strip_prefix("require ")
            && let Some(dep) = parse_go_require_entry(rest.trim())
        {
            deps.push(dep);
        }
    }
    deps
}

/// One `module/path v1.2.3` entry from a go.mod require directive; the trailing
/// `// indirect` comment (if any) is dropped.
fn parse_go_require_entry(line: &str) -> Option<Dependency> {
    if line.is_empty() || line.starts_with("//") {
        return None;
    }
    let mut parts = line.split_whitespace();
    let name = parts.next()?.to_string();
    let version = parts.next().unwrap_or_default().to_string();
    Some(Dependency {
        name,
        version,
        ecosystem: Ecosystem::Go,
    })
}

// --- Python: pyproject.toml (PEP 621 / poetry) or requirements.txt ----------

fn fetch_python(root: &Path) -> Vec<Dependency> {
    if let Ok(text) = std::fs::read_to_string(root.join("pyproject.toml")) {
        let deps = parse_pyproject(&text);
        if !deps.is_empty() {
            return deps;
        }
    }
    if let Ok(text) = std::fs::read_to_string(root.join("requirements.txt")) {
        return parse_requirements_txt(&text);
    }
    Vec::new()
}

/// Parse `pyproject.toml` for direct dependencies. Handles both PEP 621
/// (`[project].dependencies`, a list of PEP 508 requirement strings) and poetry
/// (`[tool.poetry.dependencies]`, a name → version-spec table).
pub fn parse_pyproject(text: &str) -> Vec<Dependency> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut deps = Vec::new();

    if let Some(list) = table
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for item in list {
            if let Some(spec) = item.as_str() {
                deps.push(parse_pep508(spec));
            }
        }
    }

    if let Some(map) = table
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for (name, spec) in map {
            if name == "python" {
                continue;
            }
            deps.push(Dependency {
                name: name.clone(),
                version: poetry_version_spec(spec),
                ecosystem: Ecosystem::Python,
            });
        }
    }

    deps
}

/// A poetry dependency value is either a bare version string (`"^2.0"`) or an
/// inline table carrying a `version` key alongside extras/markers.
fn poetry_version_spec(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Table(t) => t
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Parse a `requirements.txt`, one requirement per line, skipping blanks,
/// comments, and option lines (`-r other.txt`, `--hash …`).
pub fn parse_requirements_txt(text: &str) -> Vec<Dependency> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .map(parse_pep508)
        .collect()
}

/// Split a PEP 508 requirement string (`requests>=2.0,<3 ; python_version>"3"`)
/// into its package name and the remaining version/marker spec. The name runs
/// up to the first version operator, bracket (extras), or marker separator.
fn parse_pep508(spec: &str) -> Dependency {
    let spec = spec.trim();
    let end = spec
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(spec.len());
    let name = spec[..end].to_string();
    let version = spec[end..].trim().to_string();
    Dependency {
        name,
        version,
        ecosystem: Ecosystem::Python,
    }
}

pub struct DependenciesPanel {
    pub collapsed: bool,
    deps: Vec<Dependency>,
    /// Section header, set from the detected ecosystems via [`Self::set_header`].
    header: String,
    /// `true` once a fetch (even an empty one) has landed, so the panel shows
    /// the empty state instead of spinning on "Loading".
    loaded: bool,
    scroll: usize,
    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    pub last_scrollbar: Rect,
    last_header_row: u16,
    last_header_x: u16,
    last_header_w: u16,
    first_row_y: u16,
    visible_rows: u16,
    viewport_rows: u16,
}

impl DependenciesPanel {
    pub fn new() -> Self {
        Self {
            // Start collapsed (a 1-row chevron strip), like OUTLINE.
            collapsed: true,
            deps: Vec::new(),
            header: HEADER_GENERIC.to_string(),
            loaded: false,
            scroll: 0,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            last_header_row: 0,
            last_header_x: 0,
            last_header_w: 0,
            first_row_y: 0,
            visible_rows: 0,
            viewport_rows: 0,
        }
    }

    /// Set the section header for the current root's detected ecosystem(s).
    /// A change clears the loaded flag so the panel shows "Loading" until the
    /// new root's fetch lands instead of flashing the old root's dependencies.
    pub fn set_header(&mut self, header: String) {
        if header != self.header {
            self.header = header;
            self.deps.clear();
            self.loaded = false;
            self.scroll = 0;
        }
    }

    pub fn set_deps(&mut self, deps: Vec<Dependency>) {
        self.deps = deps;
        self.loaded = true;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn toggle_collapse(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn max_scroll(&self) -> usize {
        self.deps.len().saturating_sub(self.viewport_rows as usize)
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.deps.len(),
            self.viewport_rows as usize,
            self.scroll,
        ) else {
            return false;
        };
        let target = scrollbar::scroll_for_y(metrics, y);
        let moved = target != self.scroll;
        self.scroll = target;
        moved
    }

    pub fn desired_height(&self, available: u16) -> u16 {
        const BORDER: u16 = 1;
        if available == 0 {
            return 0;
        }
        let header = 1u16;
        let floor = header + BORDER;
        if self.collapsed {
            return floor.min(available);
        }
        let content = if self.deps.is_empty() {
            1
        } else {
            self.deps.len() as u16
        };
        let half = (available / 2).max(floor);
        (header + content + BORDER).min(half)
    }

    pub fn hit_header(&self, x: u16, y: u16) -> bool {
        y == self.last_header_row
            && x >= self.last_header_x
            && x < self.last_header_x.saturating_add(self.last_header_w)
    }
}

impl Default for DependenciesPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut DependenciesPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(COLOR_DIM));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.visible_rows = 0;
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let inner = Rect {
            x: inner.x + CONTENT_INDENT.min(inner.width),
            width: inner.width.saturating_sub(CONTENT_INDENT),
            ..inner
        };

        let chevron = if self.collapsed {
            crate::icons::CHEVRON_CLOSED
        } else {
            crate::icons::CHEVRON_OPEN
        };
        let header_y = inner.y;
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{chevron} "), Style::default().fg(COLOR_DIM)),
            Span::styled(
                self.header.clone(),
                Style::default()
                    .fg(COLOR_HEADER)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .render(
            Rect {
                x: inner.x,
                y: header_y,
                width: inner.width,
                height: 1,
            },
            buf,
        );
        self.last_header_row = header_y;
        self.last_header_x = inner.x;
        self.last_header_w = inner.width;

        if self.collapsed || inner.height < 2 {
            return;
        }

        let body_y = header_y + 1;
        let body_h = inner.height - 1;
        self.first_row_y = body_y;
        self.viewport_rows = body_h;

        if self.deps.is_empty() {
            let msg = if self.loaded {
                EMPTY_MESSAGE
            } else {
                LOADING_MESSAGE
            };
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(COLOR_DIM),
            )))
            .render(
                Rect {
                    x: inner.x,
                    y: body_y,
                    width: inner.width,
                    height: 1,
                },
                buf,
            );
            return;
        }

        self.scroll = self
            .scroll
            .min(self.deps.len().saturating_sub(body_h as usize));
        let bar = scrollbar::vertical_metrics(
            Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: body_y,
                width: 1,
                height: body_h,
            },
            self.deps.len(),
            body_h as usize,
            self.scroll,
        );
        let content_w = inner.width.saturating_sub(u16::from(bar.is_some()));

        let visible = (body_h as usize).min(self.deps.len().saturating_sub(self.scroll));
        self.visible_rows = visible as u16;

        for row in 0..visible {
            let idx = self.scroll + row;
            let dep = &self.deps[idx];
            let y = body_y + row as u16;
            let row_rect = Rect {
                x: inner.x,
                y,
                width: content_w,
                height: 1,
            };
            if let Some(bg) = crate::widgets::hover::row_hover_bg(
                row_rect,
                self.hover_pointer,
                self.focus_gradient,
            ) {
                buf.set_style(row_rect, Style::default().bg(bg));
            }
            let mut spans = vec![
                Span::styled(format!("{PACKAGE_GLYPH} "), Style::default().fg(COLOR_DIM)),
                Span::styled(dep.name.clone(), Style::default().fg(COLOR_NAME)),
            ];
            if !dep.version.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", dep.version),
                    Style::default().fg(COLOR_VERSION),
                ));
            }
            Paragraph::new(Line::from(spans)).render(row_rect, buf);
        }

        if let Some(metrics) = bar {
            self.last_scrollbar = metrics.area;
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_SAMPLE: &str = r#"{
        "packages": [
            {"id": "ws 0.1.0 (path+file:///repo)", "name": "ws", "version": "0.1.0"},
            {"id": "serde 1.0.0 (registry+x)", "name": "serde", "version": "1.0.0"},
            {"id": "anyhow 1.0.5 (registry+x)", "name": "anyhow", "version": "1.0.5"}
        ],
        "workspace_members": ["ws 0.1.0 (path+file:///repo)"]
    }"#;

    #[test]
    fn cargo_metadata_excludes_workspace_members() {
        let deps = parse_cargo_metadata(CARGO_SAMPLE.as_bytes());
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"serde") && names.contains(&"anyhow"));
        assert!(
            !names.contains(&"ws"),
            "the workspace member is never listed"
        );
        assert!(deps.iter().all(|d| d.ecosystem == Ecosystem::Rust));
    }

    #[test]
    fn cargo_metadata_garbage_yields_empty() {
        assert!(parse_cargo_metadata(b"not json").is_empty());
    }

    #[test]
    fn package_json_reads_both_dependency_maps() {
        let json = r#"{
            "dependencies": {"react": "^18.0.0"},
            "devDependencies": {"typescript": "5.4.0"}
        }"#;
        let deps = parse_package_json(json);
        let react = deps.iter().find(|d| d.name == "react").expect("react");
        assert_eq!(react.version, "^18.0.0");
        assert_eq!(react.ecosystem, Ecosystem::Node);
        assert!(
            deps.iter()
                .any(|d| d.name == "typescript" && d.version == "5.4.0")
        );
    }

    #[test]
    fn go_mod_reads_block_and_strips_indirect() {
        let go = "module example.com/x\n\ngo 1.22\n\nrequire (\n\tgithub.com/pkg/errors v0.9.1\n\tgolang.org/x/sys v0.1.0 // indirect\n)\n";
        let deps = parse_go_mod(go);
        let errs = deps
            .iter()
            .find(|d| d.name == "github.com/pkg/errors")
            .expect("errors");
        assert_eq!(errs.version, "v0.9.1");
        assert_eq!(errs.ecosystem, Ecosystem::Go);
        let sys = deps
            .iter()
            .find(|d| d.name == "golang.org/x/sys")
            .expect("sys");
        assert_eq!(sys.version, "v0.1.0", "the // indirect comment is dropped");
    }

    #[test]
    fn go_mod_reads_single_line_require() {
        let deps = parse_go_mod("require github.com/spf13/cobra v1.8.0\n");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "github.com/spf13/cobra");
        assert_eq!(deps[0].version, "v1.8.0");
    }

    #[test]
    fn pyproject_pep621_dependencies() {
        let py =
            "[project]\nname = \"x\"\ndependencies = [\n  \"requests>=2.0\",\n  \"loguru\",\n]\n";
        let deps = parse_pyproject(py);
        let req = deps
            .iter()
            .find(|d| d.name == "requests")
            .expect("requests");
        assert_eq!(req.version, ">=2.0");
        assert_eq!(req.ecosystem, Ecosystem::Python);
        let loguru = deps.iter().find(|d| d.name == "loguru").expect("loguru");
        assert_eq!(loguru.version, "", "a bare requirement has no version spec");
    }

    #[test]
    fn pyproject_poetry_table_skips_python() {
        let py = "[tool.poetry.dependencies]\npython = \"^3.11\"\nrequests = \"^2.0\"\nrich = { version = \"^13.0\", optional = true }\n";
        let deps = parse_pyproject(py);
        assert!(
            !deps.iter().any(|d| d.name == "python"),
            "the python pin is not a dependency"
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "requests" && d.version == "^2.0")
        );
        let rich = deps.iter().find(|d| d.name == "rich").expect("rich");
        assert_eq!(rich.version, "^13.0", "inline-table version is unwrapped");
    }

    #[test]
    fn requirements_txt_skips_comments_and_options() {
        let txt = "# top\nrequests==2.31.0\n\n-r other.txt\nloguru\n--hash=sha256:abc\n";
        let deps = parse_requirements_txt(txt);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["requests", "loguru"]);
        assert_eq!(deps[0].version, "==2.31.0");
    }

    #[test]
    fn detect_ecosystems_lists_each_manifest_once() {
        let dir = std::env::temp_dir().join(format!("croft-deps-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("cargo");
        std::fs::write(dir.join("pyproject.toml"), "[project]\n").expect("py");
        std::fs::write(dir.join("requirements.txt"), "loguru\n").expect("reqs");
        let ecos = detect_ecosystems(&dir);
        assert_eq!(
            ecos,
            vec![Ecosystem::Rust, Ecosystem::Python],
            "Python listed once despite two manifests"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_ecosystems_empty_for_plain_folder() {
        let dir = std::env::temp_dir().join(format!("croft-deps-plain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        assert!(detect_ecosystems(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn labels_track_single_vs_multiple_ecosystems() {
        assert_eq!(header_label(&[Ecosystem::Rust]), "RUST DEPENDENCIES");
        assert_eq!(header_label(&[Ecosystem::Python]), "PYTHON DEPENDENCIES");
        assert_eq!(
            header_label(&[Ecosystem::Rust, Ecosystem::Node]),
            "DEPENDENCIES"
        );
        assert_eq!(menu_label(&[Ecosystem::Python]), "Python Dependencies");
        assert_eq!(
            menu_label(&[Ecosystem::Rust, Ecosystem::Go]),
            "Dependencies"
        );
    }

    fn rendered_text(p: &mut DependenciesPanel, width: u16, height: u16) -> String {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn collapsed_then_expanded_height() {
        let mut p = DependenciesPanel::new();
        p.set_deps(vec![Dependency {
            name: "serde".into(),
            version: "1.0".into(),
            ecosystem: Ecosystem::Rust,
        }]);
        p.collapsed = true;
        assert_eq!(p.desired_height(40), 2);
        p.toggle_collapse();
        assert_eq!(p.desired_height(40), 3, "header + one dep + separator");
    }

    #[test]
    fn renders_dynamic_header_name_and_version() {
        let mut p = DependenciesPanel::new();
        p.set_header(header_label(&[Ecosystem::Rust]));
        p.collapsed = false;
        p.set_deps(vec![Dependency {
            name: "ratatui".into(),
            version: "0.30.0".into(),
            ecosystem: Ecosystem::Rust,
        }]);
        let text = rendered_text(&mut p, 36, 4);
        assert!(text.contains("RUST DEPENDENCIES"), "got:\n{text}");
        assert!(text.contains("ratatui"), "got:\n{text}");
        assert!(text.contains("0.30.0"), "got:\n{text}");
    }

    #[test]
    fn empty_after_load_says_no_dependencies() {
        let mut p = DependenciesPanel::new();
        p.collapsed = false;
        p.set_deps(vec![]);
        let text = rendered_text(&mut p, 36, 4);
        assert!(text.contains("No dependencies"), "got:\n{text}");
    }

    #[test]
    fn changing_header_resets_to_loading() {
        let mut p = DependenciesPanel::new();
        p.collapsed = false;
        p.set_header(header_label(&[Ecosystem::Rust]));
        p.set_deps(vec![Dependency {
            name: "serde".into(),
            version: "1".into(),
            ecosystem: Ecosystem::Rust,
        }]);
        // Re-rooting into a Python project changes the header and must not flash
        // the old root's crates while the new fetch is in flight.
        p.set_header(header_label(&[Ecosystem::Python]));
        let text = rendered_text(&mut p, 36, 4);
        assert!(text.contains("PYTHON DEPENDENCIES"), "got:\n{text}");
        assert!(text.contains("Loading"), "got:\n{text}");
        assert!(
            !text.contains("serde"),
            "stale crates cleared, got:\n{text}"
        );
    }
}
