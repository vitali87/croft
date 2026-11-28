//! The Explorer's DEPENDENCIES section: a collapsible, language-aware list of
//! the packages the workspace depends on, mirroring the dependency views the
//! three reference editors contribute. The key behaviour, shared by VS Code
//! (rust-analyzer's "Rust Dependencies"), Zed, and Neovim (crates.nvim /
//! package-info.nvim), is that the view is **gated on the project's manifest**
//! and is **specific to whichever manifest exists** — it never shows a fixed
//! language's dependencies in an unrelated folder.
//!
//! So the caller first detects the workspace's ecosystem(s) from the manifest
//! files in its root ([`crate::dependencies::detect_ecosystems`]), then
//! resolves the dependency set per ecosystem off the render thread
//! ([`crate::dependencies::fetch_dependencies`]): `cargo metadata` for Rust,
//! and a direct read of the manifest for Python / Node / Go. This module draws
//! the result. When the root carries no recognised manifest the caller hides
//! the section entirely (and drops it from the ⋯ menu), so opening a plain
//! folder like `~/Documents` shows no dependency view at all.

use crate::dependencies::{Dependency, Ecosystem};
use crate::widgets::scrollbar;
use crate::widgets::view_cx::ViewCx;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Borders, Paragraph, Widget},
};

const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_NAME: Color = Color::Rgb(0xCC, 0xCC, 0xCC);
const COLOR_VERSION: Color = Color::Rgb(0x88, 0x9A, 0xC0);

const CONTENT_INDENT: u16 = 1;
/// Codicon `cod-package` — the box glyph VS Code paints on dependency rows.
const PACKAGE_GLYPH: char = '\u{eb29}';

const HEADER_GENERIC: &str = "DEPENDENCIES";
const EMPTY_MESSAGE: &str = "No dependencies";
const LOADING_MESSAGE: &str = "Loading\u{2026}";
const FAILED_MESSAGE: &str = "Could not list dependencies";

/// Where the current root's list stands, which picks the message an empty
/// list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    /// The lookup has not answered yet.
    Loading,
    /// The lookup answered, even with nothing.
    Listed,
    /// The lookup crashed or never started (#2572).
    Failed,
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

pub struct DependenciesPanel {
    pub collapsed: bool,
    deps: Vec<Dependency>,
    /// Section header, set from the detected ecosystems via [`Self::set_header`].
    header: String,
    /// Whether the current root's lookup has answered, so an empty list
    /// shows the empty state or the failure instead of spinning on "Loading".
    listing: Listing,
    scroll: usize,

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
            listing: Listing::Loading,
            scroll: 0,
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
    /// A change goes back to "Loading" until the new root's fetch lands
    /// instead of flashing the old root's dependencies.
    pub fn set_header(&mut self, header: String) {
        if header != self.header {
            self.header = header;
            self.deps.clear();
            self.listing = Listing::Loading;
            self.scroll = 0;
        }
    }

    pub fn set_deps(&mut self, deps: Vec<Dependency>) {
        self.deps = deps;
        self.listing = Listing::Listed;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// The lookup crashed or could not start: say so rather than "Loading"
    /// for a list that is not coming (#2572).
    pub fn set_failed(&mut self) {
        self.deps.clear();
        self.listing = Listing::Failed;
        self.scroll = 0;
    }

    /// A new lookup started, as when the section is shown again: a list that
    /// had failed is loading again.
    pub fn lookup_started(&mut self) {
        if self.listing == Listing::Failed {
            self.listing = Listing::Loading;
        }
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

impl DependenciesPanel {
    /// Draw the section into `area` with this frame's [`ViewCx`].
    pub fn draw(&mut self, cx: &ViewCx, area: Rect, buf: &mut Buffer) {
        let block = crate::widgets::surface::section_block(
            cx.theme,
            Borders::BOTTOM,
            Style::default().fg(cx.theme.ui(COLOR_DIM)),
        );
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

        let header_y = inner.y;
        let header_row = Rect {
            x: inner.x,
            y: header_y,
            width: inner.width,
            height: 1,
        };
        crate::widgets::surface::paint_section_header(
            buf,
            header_row,
            cx.theme,
            !self.collapsed,
            &self.header,
            Vec::new(),
            crate::widgets::hover::contains(header_row, cx.pointer),
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
            let msg = match self.listing {
                Listing::Loading => LOADING_MESSAGE,
                Listing::Listed => EMPTY_MESSAGE,
                Listing::Failed => FAILED_MESSAGE,
            };
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(cx.theme.ui(COLOR_DIM)),
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
            if let Some(bg) = crate::widgets::hover::row_hover_bg(row_rect, cx.pointer, cx.theme) {
                buf.set_style(row_rect, Style::default().bg(bg));
            }
            let mut spans = vec![
                Span::styled(
                    format!("{PACKAGE_GLYPH} "),
                    Style::default().fg(cx.theme.ui(COLOR_DIM)),
                ),
                Span::styled(
                    dep.name.clone(),
                    Style::default().fg(cx.theme.ui(COLOR_NAME)),
                ),
            ];
            if !dep.version.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", dep.version),
                    Style::default().fg(cx.theme.ui(COLOR_VERSION)),
                ));
            }
            Paragraph::new(Line::from(spans)).render(row_rect, buf);
        }

        if let Some(metrics) = bar {
            self.last_scrollbar = metrics.area;
            scrollbar::render_vertical_on_chrome(buf, metrics, cx.focused, cx.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        p.draw(&ViewCx::default(), area, &mut buf);
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

    /// #2572: a lookup that crashed says so, and the next lookup
    /// shows "Loading" again rather than the old failure.
    #[test]
    fn a_failed_lookup_says_so_until_the_next_one_starts() {
        let mut p = DependenciesPanel::new();
        p.collapsed = false;
        p.set_failed();
        let text = rendered_text(&mut p, 36, 4);
        assert!(text.contains(FAILED_MESSAGE), "got:\n{text}");
        p.lookup_started();
        let text = rendered_text(&mut p, 36, 4);
        assert!(text.contains("Loading"), "got:\n{text}");
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
