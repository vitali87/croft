//! The Explorer's RUST DEPENDENCIES section: a collapsible list of the crates
//! the workspace resolves to, mirroring the view rust-analyzer contributes to
//! VS Code's Explorer. The set is produced off-thread from `cargo metadata`
//! (see [`fetch_dependencies`]) and handed here with [`set_deps`]. Display-only
//! — it answers "what am I building against?" at a glance without leaving the
//! editor. Non-Cargo workspaces settle to an empty state, never an error.

use std::path::Path;
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

/// One resolved dependency crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RustDep {
    pub name: String,
    pub version: String,
}

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

/// Resolve the workspace's full (transitive) dependency set off the render
/// thread. Returns every non-workspace package as a `name version` row,
/// sorted and de-duplicated. An empty vec means "not a Cargo workspace" or
/// "cargo unavailable" — the panel renders that as an empty state.
pub fn fetch_dependencies(root: &Path) -> Vec<RustDep> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(path_str)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_cargo_metadata(&output.stdout)
}

/// Parse `cargo metadata --format-version 1` JSON into the sorted, de-duplicated
/// list of non-workspace dependency crates.
pub fn parse_cargo_metadata(json: &[u8]) -> Vec<RustDep> {
    let Ok(meta) = serde_json::from_slice::<CargoMetadata>(json) else {
        return Vec::new();
    };
    let members: std::collections::HashSet<&str> =
        meta.workspace_members.iter().map(String::as_str).collect();
    let mut deps: Vec<RustDep> = meta
        .packages
        .iter()
        .filter(|p| !members.contains(p.id.as_str()))
        .map(|p| RustDep {
            name: p.name.clone(),
            version: p.version.clone(),
        })
        .collect();
    deps.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    deps.dedup();
    deps
}

pub struct RustDepsPanel {
    pub collapsed: bool,
    deps: Vec<RustDep>,
    /// `true` once a fetch (even an empty one) has landed, so the panel shows
    /// the empty state instead of spinning on "Loading…".
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

impl RustDepsPanel {
    pub fn new() -> Self {
        Self {
            collapsed: false,
            deps: Vec::new(),
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

    pub fn set_deps(&mut self, deps: Vec<RustDep>) {
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

impl Default for RustDepsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut RustDepsPanel {
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
                "RUST DEPENDENCIES",
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
                "No Cargo workspace"
            } else {
                "Loading…"
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
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{PACKAGE_GLYPH} "), Style::default().fg(COLOR_DIM)),
                Span::styled(dep.name.clone(), Style::default().fg(COLOR_NAME)),
                Span::styled(
                    format!(" {}", dep.version),
                    Style::default().fg(COLOR_VERSION),
                ),
            ]))
            .render(row_rect, buf);
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

    const SAMPLE: &str = r#"{
        "packages": [
            {"id": "ws 0.1.0 (path+file:///repo)", "name": "ws", "version": "0.1.0"},
            {"id": "serde 1.0.0 (registry+x)", "name": "serde", "version": "1.0.0"},
            {"id": "anyhow 1.0.5 (registry+x)", "name": "anyhow", "version": "1.0.5"}
        ],
        "workspace_members": ["ws 0.1.0 (path+file:///repo)"]
    }"#;

    #[test]
    fn parse_excludes_workspace_members_and_sorts() {
        let deps = parse_cargo_metadata(SAMPLE.as_bytes());
        assert_eq!(
            deps,
            vec![
                RustDep {
                    name: "anyhow".into(),
                    version: "1.0.5".into()
                },
                RustDep {
                    name: "serde".into(),
                    version: "1.0.0".into()
                },
            ],
            "the workspace member itself is never listed, deps sort by name"
        );
    }

    #[test]
    fn parse_garbage_yields_empty() {
        assert!(parse_cargo_metadata(b"not json").is_empty());
    }

    #[test]
    fn collapsed_then_expanded_height() {
        let mut p = RustDepsPanel::new();
        p.set_deps(vec![RustDep {
            name: "serde".into(),
            version: "1.0".into(),
        }]);
        p.collapsed = true;
        assert_eq!(p.desired_height(40), 2);
        p.toggle_collapse();
        assert_eq!(p.desired_height(40), 3, "header + one dep + separator");
    }

    fn rendered_text(p: &mut RustDepsPanel, width: u16, height: u16) -> String {
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
    fn renders_name_and_version() {
        let mut p = RustDepsPanel::new();
        p.set_deps(vec![RustDep {
            name: "ratatui".into(),
            version: "0.30.0".into(),
        }]);
        let text = rendered_text(&mut p, 36, 4);
        assert!(text.contains("ratatui"), "got:\n{text}");
        assert!(text.contains("0.30.0"), "got:\n{text}");
    }

    #[test]
    fn empty_after_load_says_no_cargo_workspace() {
        let mut p = RustDepsPanel::new();
        p.set_deps(vec![]);
        let text = rendered_text(&mut p, 36, 4);
        assert!(text.contains("No Cargo workspace"), "got:\n{text}");
    }
}
