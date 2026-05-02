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
    pub last_inner: Rect,
    pub last_area: Rect,
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
            last_inner: Rect::default(),
            last_area: Rect::default(),
        };
        tree.load_children(0);
        tree
    }

    /// Map a screen y coordinate to a node index, if any.
    pub fn node_at_y(&self, y: u16) -> Option<usize> {
        if y < self.last_inner.y || y >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        let row = (y - self.last_inner.y) as usize;
        let idx = self.scroll + row;
        if idx < self.nodes.len() {
            Some(idx)
        } else {
            None
        }
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.nodes.len() {
            self.selected = idx;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, FileTree) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("README.md"), "# hi\n").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        let tree = FileTree::new(root.to_path_buf());
        (tmp, tree)
    }

    #[test]
    fn new_lists_root_and_children() {
        let (_tmp, tree) = fixture();
        // Root + 3 children (src/, main.rs, README.md). Hidden filtered by default.
        assert_eq!(tree.nodes.len(), 4);
        assert!(tree.nodes[0].is_dir);
        assert!(tree.nodes[0].expanded);
    }

    #[test]
    fn directories_sort_before_files() {
        let (_tmp, tree) = fixture();
        // Skip root (idx 0). Next should be the directory.
        assert!(tree.nodes[1].is_dir);
        assert!(!tree.nodes[2].is_dir);
        assert!(!tree.nodes[3].is_dir);
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let (_tmp, mut tree) = fixture();
        tree.selected = 0;
        tree.move_up();
        assert_eq!(tree.selected, 0);
    }

    #[test]
    fn move_down_clamps_at_last() {
        let (_tmp, mut tree) = fixture();
        let last = tree.nodes.len() - 1;
        tree.selected = last;
        tree.move_down();
        assert_eq!(tree.selected, last);
    }

    #[test]
    fn move_down_then_up() {
        let (_tmp, mut tree) = fixture();
        tree.move_down();
        assert_eq!(tree.selected, 1);
        tree.move_down();
        assert_eq!(tree.selected, 2);
        tree.move_up();
        assert_eq!(tree.selected, 1);
    }

    #[test]
    fn end_jumps_to_last() {
        let (_tmp, mut tree) = fixture();
        tree.end();
        assert_eq!(tree.selected, tree.nodes.len() - 1);
    }

    #[test]
    fn home_jumps_to_first() {
        let (_tmp, mut tree) = fixture();
        tree.selected = 3;
        tree.home();
        assert_eq!(tree.selected, 0);
    }

    #[test]
    fn activate_file_returns_path() {
        let (_tmp, mut tree) = fixture();
        // Find a file node.
        let file_idx = tree.nodes.iter().position(|n| !n.is_dir).unwrap();
        tree.selected = file_idx;
        let path = tree.activate();
        assert!(path.is_some());
        assert!(path.unwrap().is_file());
    }

    #[test]
    fn activate_directory_expands_and_collapses() {
        let (_tmp, mut tree) = fixture();
        // src/ at index 1 (after directories-first sort).
        tree.selected = 1;
        let total_before = tree.nodes.len();
        // Expand src/.
        let opened = tree.activate();
        assert!(opened.is_none()); // no file opened
        assert!(tree.nodes[1].expanded);
        assert!(tree.nodes.len() > total_before);
        // Collapse it again.
        let collapsed = tree.activate();
        assert!(collapsed.is_none());
        assert!(!tree.nodes[1].expanded);
        assert_eq!(tree.nodes.len(), total_before);
    }

    #[test]
    fn select_clamps_to_valid_index() {
        let (_tmp, mut tree) = fixture();
        let last = tree.nodes.len() - 1;
        tree.select(last);
        assert_eq!(tree.selected, last);
        // Out of range: select() should be a no-op.
        let prev = tree.selected;
        tree.select(9999);
        assert_eq!(tree.selected, prev);
    }

    #[test]
    fn collapse_resets_selection_if_above_new_len() {
        let (_tmp, mut tree) = fixture();
        // Expand src/, point selection at a child, then collapse.
        tree.selected = 1;
        tree.activate();
        // Move selection inside the expanded subtree.
        let inside = tree.nodes.len() - 1;
        tree.selected = inside;
        // Collapse src/.
        tree.selected = 1;
        tree.activate();
        assert!(tree.selected < tree.nodes.len());
    }

    #[test]
    fn page_up_and_page_down() {
        let (_tmp, mut tree) = fixture();
        let last = tree.nodes.len().saturating_sub(1);
        tree.page_down(100);
        assert_eq!(tree.selected, last);
        tree.page_up(100);
        assert_eq!(tree.selected, 0);
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
        self.last_inner = inner;
        self.last_area = area;

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
