use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

#[derive(Clone, Copy)]
pub struct ShortcutEntry {
    pub keys: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Copy)]
pub struct ShortcutGroup {
    pub title: &'static str,
    pub entries: &'static [ShortcutEntry],
}

pub const SHORTCUT_GROUPS: &[ShortcutGroup] = &[
    ShortcutGroup {
        title: "Global",
        entries: &[
            ShortcutEntry { keys: "Ctrl+S", description: "Save the open file (also stages selected source-control entry)" },
            ShortcutEntry { keys: "Ctrl+Q", description: "Quit" },
            ShortcutEntry { keys: "F6", description: "Cycle focus across panes" },
            ShortcutEntry { keys: "Ctrl+B", description: "Toggle the sidebar" },
            ShortcutEntry { keys: "Ctrl+J", description: "Toggle the terminal pane" },
            ShortcutEntry { keys: "Cmd/Ctrl+P", description: "Quick Open: fuzzy-search workspace files by name" },
            ShortcutEntry { keys: "Cmd/Ctrl+Shift+E", description: "Jump to the Explorer sidebar" },
            ShortcutEntry { keys: "Cmd/Ctrl+Shift+F", description: "Jump to the Search sidebar" },
            ShortcutEntry { keys: "Cmd/Ctrl+Shift+S", description: "Jump to Source Control (from any pane)" },
            ShortcutEntry { keys: "Cmd/Ctrl+Shift+G", description: "Jump to Source Control (when editor not focused)" },
            ShortcutEntry { keys: "Cmd/Ctrl+Shift+D", description: "Jump to Run and Debug" },
            ShortcutEntry { keys: "Cmd/Ctrl+Shift+R", description: "Jump to Remote (SSH)" },
            ShortcutEntry { keys: "F1", description: "Open this shortcuts panel" },
            ShortcutEntry { keys: "Esc", description: "Close this panel (or clear selection / dismiss menus)" },
        ],
    },
    ShortcutGroup {
        title: "Explorer",
        entries: &[
            ShortcutEntry { keys: "Up / Down", description: "Move selection" },
            ShortcutEntry { keys: "Enter / Right", description: "Open file or expand folder" },
            ShortcutEntry { keys: "Left", description: "Collapse folder" },
            ShortcutEntry { keys: "Shift+Up/Down/PgUp/PgDn/Home/End", description: "Extend multi-selection" },
            ShortcutEntry { keys: "Alt-click / Ctrl-click", description: "Toggle a row in or out of the multi-selection" },
            ShortcutEntry { keys: "Cmd/Ctrl+A", description: "Select every visible row" },
            ShortcutEntry { keys: "Cmd/Ctrl+C / X / V", description: "Copy / cut / paste paths in the explorer clipboard" },
            ShortcutEntry { keys: "Cmd/Ctrl+F", description: "New file in selected folder" },
            ShortcutEntry { keys: "Cmd/Ctrl+Shift+F", description: "New folder in selected folder" },
            ShortcutEntry { keys: "Cmd/Ctrl+R or F2", description: "Rename" },
            ShortcutEntry { keys: "Cmd/Ctrl+/", description: "Re-root the workspace at the selected folder" },
            ShortcutEntry { keys: "Cmd/Ctrl+Shift+/", description: "Re-root at parent of the selected node" },
            ShortcutEntry { keys: "Cmd/Ctrl+D", description: "Compare-anchor / diff toggle on file" },
            ShortcutEntry { keys: "Delete / Backspace", description: "Move every selected path to OS Trash" },
            ShortcutEntry { keys: "Drag onto folder", description: "Move (Alt-drag copies)" },
        ],
    },
    ShortcutGroup {
        title: "Editor: text",
        entries: &[
            ShortcutEntry { keys: "Cmd/Ctrl+F", description: "Find in current file (Enter next, Shift+Enter prev, Esc close)" },
            ShortcutEntry { keys: "Arrows / Home / End", description: "Navigate; clears any active selection" },
            ShortcutEntry { keys: "Shift+arrows / Home / End / PgUp / PgDn", description: "Extend the selection" },
            ShortcutEntry { keys: "PgUp / PgDn", description: "Scroll exactly one viewport" },
            ShortcutEntry { keys: "Alt+Left / Right", description: "Word-step left / right" },
            ShortcutEntry { keys: "Shift+Alt+Up / Down", description: "Duplicate the current line or selection" },
            ShortcutEntry { keys: "Cmd/Ctrl+C", description: "Copy the selection (OSC 52 to system clipboard)" },
            ShortcutEntry { keys: "Cmd/Ctrl+X", description: "Cut the selection" },
            ShortcutEntry { keys: "Cmd/Ctrl+V", description: "Paste at the cursor" },
            ShortcutEntry { keys: "Cmd/Ctrl+Z", description: "Undo" },
            ShortcutEntry { keys: "Cmd+A", description: "Select the entire buffer" },
            ShortcutEntry { keys: "Cmd/Ctrl+W", description: "Close the active tab" },
            ShortcutEntry { keys: "Cmd/Ctrl+1..9", description: "Jump to that tab (when no vim chord is pending)" },
            ShortcutEntry { keys: "Ctrl+Space", description: "Trigger LSP completion" },
        ],
    },
    ShortcutGroup {
        title: "Editor: line motion",
        entries: &[
            ShortcutEntry { keys: "Ctrl+A", description: "Move to start of current line (readline-style)" },
            ShortcutEntry { keys: "Ctrl+E", description: "Move to end of current line" },
            ShortcutEntry { keys: "Ctrl+K", description: "Kill from cursor to end of line" },
            ShortcutEntry { keys: "Ctrl+U", description: "Kill from cursor to start of line" },
        ],
    },
    ShortcutGroup {
        title: "Editor: vim chords",
        entries: &[
            ShortcutEntry { keys: "Cmd+g g", description: "Go to the top of the file" },
            ShortcutEntry { keys: "Cmd+g N g", description: "Go to line N (digits between the chord keys)" },
            ShortcutEntry { keys: "Cmd+N Cmd+g g", description: "Same as above with the count first" },
            ShortcutEntry { keys: "Cmd+Shift+G", description: "Go to the bottom (or line N with a leading count)" },
            ShortcutEntry { keys: "Cmd+d d", description: "Delete the current line (yanks to system clipboard)" },
            ShortcutEntry { keys: "Cmd+N Cmd+d d", description: "Delete N lines" },
            ShortcutEntry { keys: "Cmd+y y", description: "Yank (copy) the current line" },
            ShortcutEntry { keys: "Cmd+N Cmd+y y", description: "Yank N lines" },
            ShortcutEntry { keys: "Cmd+o", description: "Open a new line below, inheriting indent" },
            ShortcutEntry { keys: "Cmd+Shift+O", description: "Open a new line above, inheriting indent" },
        ],
    },
    ShortcutGroup {
        title: "Editor: PDF preview",
        entries: &[
            ShortcutEntry { keys: "Right / PgDn / Space", description: "Next page" },
            ShortcutEntry { keys: "Left / PgUp", description: "Previous page" },
            ShortcutEntry { keys: "Home", description: "First page" },
            ShortcutEntry { keys: "End", description: "Last page" },
        ],
    },
    ShortcutGroup {
        title: "Editor: spreadsheet preview",
        entries: &[
            ShortcutEntry { keys: "Arrows", description: "Pan one row / column" },
            ShortcutEntry { keys: "PgUp / PgDn", description: "Pan a full viewport vertically" },
            ShortcutEntry { keys: "Home", description: "Jump to row 1, column 1" },
            ShortcutEntry { keys: "End", description: "Jump to the last visible page" },
            ShortcutEntry { keys: "Tab / Shift+Tab", description: "Switch worksheet (multi-sheet workbooks)" },
        ],
    },
    ShortcutGroup {
        title: "Terminal",
        entries: &[
            ShortcutEntry { keys: "Any key", description: "Forwarded to the shell PTY" },
            ShortcutEntry { keys: "Ctrl+Shift+C / Cmd+C", description: "Copy current selection" },
            ShortcutEntry { keys: "Ctrl+Shift+T", description: "Open another terminal next to the active one" },
            ShortcutEntry { keys: "Ctrl+Shift+W", description: "Close the active terminal" },
            ShortcutEntry { keys: "Ctrl+Shift+]", description: "Cycle to the next terminal" },
            ShortcutEntry { keys: "Cmd+V / Ctrl+V", description: "Paste local clipboard into the shell" },
        ],
    },
    ShortcutGroup {
        title: "Search sidebar",
        entries: &[
            ShortcutEntry { keys: "Type", description: "Live gitignore-aware workspace search" },
            ShortcutEntry { keys: "Click Aa / ab / .*", description: "Case-sensitive / whole-word / regex toggles" },
            ShortcutEntry { keys: "Up / Down + Enter", description: "Open the file at the matched line" },
        ],
    },
];

#[derive(Default)]
pub struct ShortcutsModal {
    pub scroll: u16,
    pub last_inner_height: u16,
    pub last_content_height: u16,
    pub last_rect: Rect,
}

impl ShortcutsModal {
    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        for (i, group) in SHORTCUT_GROUPS.iter().enumerate() {
            if i > 0 {
                out.push(Line::from(""));
            }
            out.push(Line::from(Span::styled(
                group.title,
                Style::default()
                    .fg(Color::Rgb(0x4e, 0x9a, 0xff))
                    .add_modifier(Modifier::BOLD),
            )));
            out.push(Line::from(Span::styled(
                "─".repeat(group.title.chars().count()),
                Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)),
            )));
            for entry in group.entries {
                out.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<32} ", entry.keys),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        entry.description,
                        Style::default().fg(Color::Rgb(0xd0, 0xd0, 0xd0)),
                    ),
                ]));
            }
        }
        out
    }

    pub fn scroll_down(&mut self, by: u16) {
        let max_scroll = self
            .last_content_height
            .saturating_sub(self.last_inner_height);
        self.scroll = self.scroll.saturating_add(by).min(max_scroll);
    }

    pub fn scroll_up(&mut self, by: u16) {
        self.scroll = self.scroll.saturating_sub(by);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = self
            .last_content_height
            .saturating_sub(self.last_inner_height);
    }
}

pub fn render_shortcuts_modal(
    modal: &mut ShortcutsModal,
    area: Rect,
    buf: &mut Buffer,
) {
    let width = area.width.saturating_mul(8) / 10;
    let width = width.clamp(40, 110.min(area.width));
    let height = area.height.saturating_mul(8) / 10;
    let height = height.clamp(10, area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let rect = Rect { x, y, width, height };
    modal.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(Span::styled(
            " Shortcuts — Esc/q to close, ↑/↓ PgUp/PgDn Home/End to scroll ",
            Style::default()
                .fg(Color::Rgb(0xff, 0xff, 0xff))
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);

    let lines = modal.lines();
    modal.last_content_height = lines.len() as u16;
    modal.last_inner_height = inner.height;
    let max_scroll = modal
        .last_content_height
        .saturating_sub(inner.height);
    if modal.scroll > max_scroll {
        modal.scroll = max_scroll;
    }
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((modal.scroll, 0));
    Widget::render(paragraph, inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_groups_cover_every_pane_the_user_can_focus() {
        let titles: Vec<&str> = SHORTCUT_GROUPS.iter().map(|g| g.title).collect();
        for required in [
            "Global",
            "Explorer",
            "Editor: text",
            "Editor: line motion",
            "Editor: vim chords",
            "Terminal",
            "Search sidebar",
        ] {
            assert!(
                titles.contains(&required),
                "shortcuts panel is missing a section for {required}; new panes / chord layers must be discoverable here so the user does not have to grep the source for the binding"
            );
        }
    }

    #[test]
    fn no_group_is_empty_because_an_empty_section_is_just_chrome() {
        for group in SHORTCUT_GROUPS {
            assert!(
                !group.entries.is_empty(),
                "group '{}' has no entries; remove the group or fill it in",
                group.title
            );
        }
    }

    #[test]
    fn lines_contain_every_keys_label_so_the_user_can_find_any_binding_in_the_modal() {
        let modal = ShortcutsModal::default();
        let rendered = modal
            .lines()
            .into_iter()
            .flat_map(|l| l.spans.into_iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        for group in SHORTCUT_GROUPS {
            for entry in group.entries {
                assert!(
                    rendered.contains(entry.keys),
                    "rendered modal is missing the keys label '{}' from group '{}'; the lines() builder must surface every entry so scrolling reaches everything",
                    entry.keys,
                    group.title
                );
            }
        }
    }

    #[test]
    fn scroll_down_is_clamped_so_the_view_cannot_overshoot_the_last_visible_line() {
        let mut modal = ShortcutsModal {
            scroll: 0,
            last_inner_height: 10,
            last_content_height: 25,
            last_rect: Rect::default(),
        };
        modal.scroll_down(100);
        assert_eq!(
            modal.scroll, 15,
            "scroll must clamp at content_height - inner_height (25 - 10 = 15) so the last line stays visible at the bottom of the viewport"
        );
    }

    #[test]
    fn scroll_up_clamps_at_zero_so_the_view_cannot_undershoot_the_first_line() {
        let mut modal = ShortcutsModal {
            scroll: 5,
            last_inner_height: 10,
            last_content_height: 25,
            last_rect: Rect::default(),
        };
        modal.scroll_up(100);
        assert_eq!(modal.scroll, 0);
    }

    #[test]
    fn scroll_to_bottom_lands_exactly_at_max_scroll() {
        let mut modal = ShortcutsModal {
            scroll: 0,
            last_inner_height: 8,
            last_content_height: 30,
            last_rect: Rect::default(),
        };
        modal.scroll_to_bottom();
        assert_eq!(modal.scroll, 22);
    }
}
