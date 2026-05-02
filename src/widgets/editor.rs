use anyhow::Result;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::path::{Path, PathBuf};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, ThemeSet};
use syntect::parsing::SyntaxSet;

const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

pub struct Editor {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub focused: bool,
    pub status: String,
    pub last_area: Rect,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    pub theme_name: String,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            path: None,
            lines: Vec::new(),
            scroll: 0,
            cursor_row: 0,
            cursor_col: 0,
            focused: false,
            status: String::from("No file open"),
            last_area: Rect::default(),
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            theme_name: String::from("base16-ocean.dark"),
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
        self.cursor_col = self.cursor_col.min(self.lines.get(self.cursor_row).map(|s| s.len()).unwrap_or(0));
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.cursor_row = (self.cursor_row + n).min(self.lines.len().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.lines.get(self.cursor_row).map(|s| s.len()).unwrap_or(0));
    }

    pub fn open(&mut self, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_FILE_BYTES {
            anyhow::bail!("File too large ({} bytes)", meta.len());
        }
        let bytes = std::fs::read(path)?;
        if is_binary(&bytes) {
            anyhow::bail!("Binary file");
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.lines = text.lines().map(|s| s.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.path = Some(path.to_path_buf());
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.status = format!("Opened {}", path.display());
        Ok(())
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < self.lines[self.cursor_row].len() {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    pub fn page_up(&mut self, page: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(page);
        self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
    }

    pub fn page_down(&mut self, page: usize) {
        self.cursor_row = (self.cursor_row + page).min(self.lines.len().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
    }

    pub fn home_line(&mut self) {
        self.cursor_col = 0;
    }

    pub fn end_line(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    fn syntax_for_path(&self) -> Option<&syntect::parsing::SyntaxReference> {
        let path = self.path.as_ref()?;
        let ext = path.extension()?.to_str()?;
        self.syntax_set.find_syntax_by_extension(ext)
    }
}

fn is_binary(data: &[u8]) -> bool {
    let sample = &data[..data.len().min(4096)];
    if sample.contains(&0) {
        return true;
    }
    if sample.is_empty() {
        return false;
    }
    let nontext = sample
        .iter()
        .filter(|&&b| !(b >= 0x20 || matches!(b, b'\n' | b'\r' | b'\t' | 0x0c | 0x08)))
        .count();
    (nontext as f32 / sample.len() as f32) > 0.30
}

fn syn_to_ratatui(s: SynStyle) -> Style {
    let fg = Color::Rgb(s.foreground.r, s.foreground.g, s.foreground.b);
    let mut style = Style::default().fg(fg);
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

impl Widget for &mut Editor {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let title = match &self.path {
            Some(p) => format!(
                " {} ",
                p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ),
            None => String::from(" EDITOR "),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;

        let height = inner.height as usize;
        if self.cursor_row < self.scroll {
            self.scroll = self.cursor_row;
        } else if self.cursor_row >= self.scroll + height {
            self.scroll = self.cursor_row + 1 - height;
        }

        let gutter_width = (self.lines.len() + 1).to_string().len() as u16 + 1;
        let text_x = inner.x + gutter_width + 1;
        let text_width = inner.width.saturating_sub(gutter_width + 2);

        let theme = &self.theme_set.themes[&self.theme_name];
        let syntax = self.syntax_for_path();
        let mut highlighter = syntax.map(|s| HighlightLines::new(s, theme));

        let end = (self.scroll + height).min(self.lines.len());
        for (row_idx, line_idx) in (self.scroll..end).enumerate() {
            let y = inner.y + row_idx as u16;
            let line_no = format!("{:>width$} ", line_idx + 1, width = gutter_width as usize - 1);
            let gutter = Line::from(Span::styled(line_no, Style::default().fg(Color::DarkGray)));
            buf.set_line(inner.x, y, &gutter, gutter_width);

            let raw = &self.lines[line_idx];
            let mut line_with_nl = raw.clone();
            line_with_nl.push('\n');

            let spans: Vec<Span> = if let Some(h) = highlighter.as_mut() {
                match h.highlight_line(&line_with_nl, &self.syntax_set) {
                    Ok(ranges) => ranges
                        .into_iter()
                        .map(|(s, t)| {
                            let txt = t.trim_end_matches('\n').to_string();
                            Span::styled(txt, syn_to_ratatui(s))
                        })
                        .collect(),
                    Err(_) => vec![Span::raw(raw.clone())],
                }
            } else {
                vec![Span::raw(raw.clone())]
            };

            let line = Line::from(spans);
            buf.set_line(text_x, y, &line, text_width);

            if self.focused && line_idx == self.cursor_row {
                let col = (self.cursor_col as u16).min(text_width.saturating_sub(1));
                let cx = text_x + col;
                if cx < inner.x + inner.width {
                    let cell = &mut buf[(cx, y)];
                    cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }
}
