use crate::icons;
use ignore::WalkBuilder;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::path::{Path, PathBuf};

pub struct Node {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub loaded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            nodes: vec![Node {
                path: root,
                depth: 0,
                is_dir: true,
                expanded: true,
                loaded: false,
            }],
            selected: 0,
            scroll: 0,
            focused: true,
        };
        tree.load_children(0);
        tree
    }

    fn load_children(&mut self, idx: usize) {
        if self.nodes[idx].loaded {
            return;
        }
        let path = self.nodes[idx].path.clone();
        let depth = self.nodes[idx].depth + 1;
        let mut entries: Vec<(PathBuf, bool)> = WalkBuilder::new(&path)
            .max_depth(Some(1))
            .git_ignore(true)
            .hidden(false)
            .build()
            .filter_map(Result::ok)
            .filter_map(|e| {
                let p = e.path().to_path_buf();
                if p == path {
                    return None;
                }
                let is_dir = p.is_dir();
                Some((p, is_dir))
            })
            .collect();
        entries.sort_by(|a, b| match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.file_name().cmp(&b.0.file_name()),
        });
        let new_nodes: Vec<Node> = entries
            .into_iter()
            .map(|(p, is_dir)| Node {
                path: p,
                depth,
                is_dir,
                expanded: false,
                loaded: false,
            })
            .collect();
        let insert_at = idx + 1;
        for (i, n) in new_nodes.into_iter().enumerate() {
            self.nodes.insert(insert_at + i, n);
        }
        self.nodes[idx].loaded = true;
    }

    fn collapse(&mut self, idx: usize) {
        let depth = self.nodes[idx].depth;
        self.nodes[idx].expanded = false;
        let mut end = idx + 1;
        while end < self.nodes.len() && self.nodes[end].depth > depth {
            end += 1;
        }
        self.nodes.drain((idx + 1)..end);
        self.nodes[idx].loaded = false;
        if self.selected >= self.nodes.len() {
            self.selected = self.nodes.len().saturating_sub(1);
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.nodes.len() {
            self.selected += 1;
        }
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page);
    }

    pub fn page_down(&mut self, page: usize) {
        self.selected = (self.selected + page).min(self.nodes.len().saturating_sub(1));
    }

    pub fn home(&mut self) {
        self.selected = 0;
    }

    pub fn end(&mut self) {
        self.selected = self.nodes.len().saturating_sub(1);
    }

    /// Activate the selected node. Returns Some(path) if a file should be opened.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let idx = self.selected;
        if idx >= self.nodes.len() {
            return None;
        }
        if self.nodes[idx].is_dir {
            if self.nodes[idx].expanded {
                self.collapse(idx);
            } else {
                self.nodes[idx].expanded = true;
                self.load_children(idx);
            }
            None
        } else {
            Some(self.nodes[idx].path.clone())
        }
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.nodes.get(self.selected).map(|n| n.path.as_path())
    }
}

impl Widget for &mut FileTree {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(Span::styled(
                " EXPLORER ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);

        let visible_height = inner.height as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible_height {
            self.scroll = self.selected + 1 - visible_height;
        }

        let end = (self.scroll + visible_height).min(self.nodes.len());
        for (row, idx) in (self.scroll..end).enumerate() {
            let node = &self.nodes[idx];
            let is_selected = idx == self.selected;
            let y = inner.y + row as u16;

            let indent = "  ".repeat(node.depth);
            let mut spans: Vec<Span> = Vec::with_capacity(6);
            spans.push(Span::raw(indent));

            let name = node
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| node.path.display().to_string());

            if node.is_dir {
                let chev = if node.expanded {
                    icons::CHEVRON_OPEN
                } else {
                    icons::CHEVRON_CLOSED
                };
                let icon = if node.expanded {
                    icons::FOLDER_OPEN
                } else {
                    icons::FOLDER_CLOSED
                };
                spans.push(Span::styled(
                    format!("{chev} "),
                    Style::default().fg(Color::Gray),
                ));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(icon.color),
                ));
                spans.push(Span::styled(
                    name,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                let suffix = node
                    .path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let icon = icons::for_path(&name, &suffix);
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("{} ", icon.glyph),
                    Style::default().fg(icon.color),
                ));
                spans.push(Span::styled(name, Style::default().fg(Color::White)));
            }

            let line = Line::from(spans);
            let line_style = if is_selected {
                Style::default().bg(Color::Rgb(0x09, 0x4d, 0x77))
            } else {
                Style::default()
            };
            buf.set_style(Rect { x: inner.x, y, width: inner.width, height: 1 }, line_style);
            buf.set_line(inner.x, y, &line, inner.width);
        }
    }
}
