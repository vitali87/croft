//! A single-line text input modal for Source Control operations that need
//! a typed value: a clone URL, a new branch name, a remote name/URL, a tag
//! name. Croft's design rule keeps inputs in a visible popup (never the
//! status line), so every such prompt routes through this widget.
//!
//! The widget is pure: it owns the prompt text, the typed value, and the
//! caret. The App holds it inside `Option<InputPrompt>` with a `purpose`
//! tag so a single key/mouse/render path serves every prompt, and decides
//! what to do with the submitted string.

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

/// Why an input prompt is open, so the App can route the submitted value
/// to the right git operation. Variants carry whatever context the second
/// step needs (e.g. the base ref a new branch forks from).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputPurpose {
    /// Extract one archive member (#179): the typed value is the
    /// destination folder.
    ArchiveExtract {
        member: String,
    },
    /// Find in a hex tab (#172): the typed value is hex byte pairs
    /// ("de ad be ef") or, when it does not parse as hex, literal ASCII.
    /// Submitting stores the query on the view and jumps to the first
    /// match at or after the cursor; F3 repeats it.
    HexFind,
    /// Save the current folder set as a VS Code-compatible workspace
    /// file (#163): the typed value is the destination path.
    SaveWorkspaceAs,
    CloneUrl,
    RenameBranch,
    CreateBranchFrom {
        base: String,
    },
    AddRemoteName,
    AddRemoteUrl {
        name: String,
    },
    CreateTag,
    /// Add a debugger watch expression (#112): submitting appends it to the
    /// App's watch list and re-evaluates at the current stop.
    AddWatch,
    /// First-run consent to spawn an MCP sidecar: submitting confirms, then the
    /// command (`command_id`) proceeds to its argument prompt or runs.
    McpConsent {
        command_id: String,
    },
    /// Collect the single string argument for an MCP command before calling its
    /// tool. The submitted value fills the tool's declared argument.
    McpArg {
        command_id: String,
    },
    /// Confirm uninstalling an installed extension. Submitting (Enter) performs
    /// the removal; Esc keeps it. The submitted value is a sentinel, ignored.
    ExtensionUninstall {
        id: String,
    },
    /// Confirm moving Explorer file-tree entries to the OS Trash. Submitting
    /// (Enter) trashes `paths`; Esc keeps them. The value is a sentinel.
    TreeDelete {
        paths: Vec<PathBuf>,
    },
    /// A dirty buffer collided with an external write. Submitting (Enter)
    /// reloads `paths` from disk, discarding local edits; Esc keeps the edits.
    /// The value is a sentinel.
    ReloadConflict {
        paths: Vec<PathBuf>,
    },
    /// Instruction for the resident navigator (`croft pair`), scoped to the
    /// 0-based inclusive line `range` of `file`; `selection` carries the
    /// selected text when the scope came from a selection. Submitting sends
    /// the ask turn (the navigator may edit on it).
    AskNavigator {
        file: String,
        range: (usize, usize),
        selection: String,
    },
}

pub struct InputPrompt {
    pub purpose: InputPurpose,
    pub title: String,
    pub placeholder: String,
    pub value: String,
    pub cursor: usize,
    pub last_rect: Rect,
}

impl InputPrompt {
    pub fn new(
        purpose: InputPurpose,
        title: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        Self {
            purpose,
            title: title.into(),
            placeholder: placeholder.into(),
            value: String::new(),
            cursor: 0,
            last_rect: Rect::default(),
        }
    }

    /// Seed the field with an initial value (e.g. the current branch name
    /// for a rename), caret at the end.
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self.cursor = self.value.chars().count();
        self
    }

    fn char_count(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_offset(&self, char_idx: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.value.len())
    }

    pub fn push_char(&mut self, c: char) {
        if c == '\n' || c == '\r' {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.value.insert(at, c);
        self.cursor += 1;
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.value.remove(at);
        self.cursor -= 1;
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.value.remove(at);
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor = self.char_count();
    }

    /// The trimmed submitted value, or `None` when blank (Enter on an empty
    /// field is a no-op the caller treats as "keep waiting").
    pub fn submit_value(&self) -> Option<String> {
        let v = self.value.trim();
        (!v.is_empty()).then(|| v.to_string())
    }
}

pub fn render_input_prompt(
    prompt: &mut InputPrompt,
    screen: Rect,
    buf: &mut Buffer,
    gradient: bool,
) {
    let width = (screen.width.saturating_mul(6) / 10).clamp(30, 90.min(screen.width));
    let height: u16 = 5;
    let rect = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y + (screen.height.saturating_sub(height)) / 3,
        width,
        height: height.min(screen.height),
    };
    prompt.last_rect = rect;
    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        format!(" {} ", prompt.title),
        Style::default()
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(title.clone())
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 2,
        y: rect.y + 1,
        width: rect.width.saturating_sub(4),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    if gradient {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Value row with a blinking caret, or the placeholder when empty.
    let value_y = inner.y;
    if prompt.value.is_empty() {
        buf.set_stringn(
            inner.x,
            value_y,
            &prompt.placeholder,
            inner.width as usize,
            Style::default()
                .fg(Color::Rgb(0x6c, 0x7d, 0x9c))
                .add_modifier(Modifier::ITALIC),
        );
    } else {
        let cursor = prompt.cursor.min(prompt.value.chars().count());
        let before: String = prompt.value.chars().take(cursor).collect();
        let at: String = prompt.value.chars().skip(cursor).take(1).collect();
        let after: String = prompt.value.chars().skip(cursor + 1).collect();
        let caret = if at.is_empty() { " ".to_string() } else { at };
        let value_style = Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4));
        let caret_style = Style::default()
            .fg(Color::Rgb(0x16, 0x18, 0x1f))
            .bg(Color::Rgb(0xec, 0xef, 0xf4))
            .add_modifier(Modifier::SLOW_BLINK);
        let line = Line::from(vec![
            Span::styled(before, value_style),
            Span::styled(caret, caret_style),
            Span::styled(after, value_style),
        ]);
        Widget::render(
            Paragraph::new(line),
            Rect {
                x: inner.x,
                y: value_y,
                width: inner.width,
                height: 1,
            },
            buf,
        );
    }

    if inner.height >= 3 {
        buf.set_string(
            inner.x,
            inner.y + 2,
            "Enter to confirm · Esc to cancel",
            Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_value_trims_and_rejects_blank() {
        let mut p = InputPrompt::new(InputPurpose::CreateTag, "Tag", "name");
        assert_eq!(p.submit_value(), None);
        for c in "  v1  ".chars() {
            p.push_char(c);
        }
        assert_eq!(p.submit_value().as_deref(), Some("v1"));
    }

    #[test]
    fn with_value_seeds_text_and_caret_at_end() {
        let p =
            InputPrompt::new(InputPurpose::RenameBranch, "Rename", "new name").with_value("main");
        assert_eq!(p.value, "main");
        assert_eq!(p.cursor, 4);
    }

    #[test]
    fn cursor_edits_mid_string() {
        let mut p = InputPrompt::new(InputPurpose::CloneUrl, "Clone", "url");
        for c in "abc".chars() {
            p.push_char(c);
        }
        p.move_cursor_left();
        p.push_char('X');
        assert_eq!((p.value.as_str(), p.cursor), ("abXc", 3));
        p.pop_char();
        assert_eq!((p.value.as_str(), p.cursor), ("abc", 2));
        p.delete_char();
        assert_eq!(p.value.as_str(), "ab");
    }
}
