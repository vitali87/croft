use std::time::Instant;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Widget},
};

use crate::remote_connect::PromptKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogPhase {
    Connecting,
    AwaitingPassword,
    AwaitingPassphrase,
    AwaitingVerificationCode,
    AwaitingHostKeyConfirmation,
    AwaitingGeneric,
    Installing,
    Authenticated,
    Failed,
}

impl DialogPhase {
    pub fn awaits_input(&self) -> bool {
        matches!(
            self,
            DialogPhase::AwaitingPassword
                | DialogPhase::AwaitingPassphrase
                | DialogPhase::AwaitingVerificationCode
                | DialogPhase::AwaitingHostKeyConfirmation
                | DialogPhase::AwaitingGeneric
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, DialogPhase::Authenticated | DialogPhase::Failed)
    }
}

pub struct ConnectDialog {
    pub host: String,
    pub phase: DialogPhase,
    pub prompt_text: String,
    pub input: String,
    pub status_line: String,
    pub error: Option<String>,
    pub show_logs: bool,
    pub logs: Vec<String>,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub toggle_logs_btn: Rect,
    pub cancel_btn: Rect,
    pub submit_btn: Rect,
    pub field_rect: Rect,
    pub caret_pos: Option<(u16, u16)>,
    pub submitted: bool,
    pub install_started_at: Option<Instant>,
}

const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

const MAX_LOG_LINES: usize = 1000;
const ACCENT: Color = Color::Rgb(0x4e, 0x9a, 0xff);
const TEAL: Color = Color::Rgb(0x3e, 0xd8, 0xc4);
const RED: Color = Color::Rgb(0xff, 0x6b, 0x6b);
const PANEL_BG: Color = Color::Rgb(0x14, 0x1a, 0x24);
const FIELD_BG: Color = Color::Rgb(0x0b, 0x12, 0x1c);

impl ConnectDialog {
    pub fn new(host: String) -> Self {
        Self {
            host,
            phase: DialogPhase::Connecting,
            prompt_text: String::from("Establishing SSH connection…"),
            input: String::new(),
            status_line: String::from("Negotiating with the remote host"),
            error: None,
            show_logs: false,
            logs: Vec::new(),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            toggle_logs_btn: Rect::default(),
            cancel_btn: Rect::default(),
            submit_btn: Rect::default(),
            field_rect: Rect::default(),
            caret_pos: None,
            submitted: false,
            install_started_at: None,
        }
    }

    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        self.caret_pos
    }

    fn spinner_glyph(&self) -> char {
        let frames = SPINNER_FRAMES.len() as u128;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let idx = ((now / 90) % frames) as usize;
        SPINNER_FRAMES[idx]
    }

    pub fn push_log(&mut self, chunk: &str) {
        for line in chunk.split('\n') {
            if line.is_empty() {
                continue;
            }
            self.logs.push(line.to_string());
        }
        if self.logs.len() > MAX_LOG_LINES {
            let drop = self.logs.len() - MAX_LOG_LINES;
            self.logs.drain(..drop);
        }
    }

    pub fn set_prompt(&mut self, kind: PromptKind, line: String) {
        let trimmed = line.trim().to_string();
        let new_phase = match kind {
            PromptKind::Password => DialogPhase::AwaitingPassword,
            PromptKind::Passphrase => DialogPhase::AwaitingPassphrase,
            PromptKind::VerificationCode => DialogPhase::AwaitingVerificationCode,
            PromptKind::HostKeyConfirmation => DialogPhase::AwaitingHostKeyConfirmation,
            PromptKind::Generic => DialogPhase::AwaitingGeneric,
        };
        let same_prompt = self.phase == new_phase && self.prompt_text == trimmed;
        self.prompt_text = trimmed;
        self.phase = new_phase;
        if !same_prompt {
            self.input.clear();
            self.status_line = default_status_for(&self.phase, &self.host);
            self.submitted = false;
        } else if self.submitted {
            self.input.clear();
            self.status_line = match self.phase {
                DialogPhase::AwaitingPassword => {
                    String::from("Authentication failed, type the password again")
                }
                DialogPhase::AwaitingPassphrase => String::from("Passphrase rejected, try again"),
                DialogPhase::AwaitingVerificationCode => String::from("Code rejected, try again"),
                _ => default_status_for(&self.phase, &self.host),
            };
            self.submitted = false;
        }
    }

    pub fn set_authenticated(&mut self) {
        self.phase = DialogPhase::Authenticated;
        self.status_line = format!("Authenticated to {}", self.host);
        self.input.clear();
    }

    pub fn set_installing(&mut self) {
        self.phase = DialogPhase::Installing;
        self.status_line = format!("Preparing remote croft on {}…", self.host);
        self.input.clear();
        self.show_logs = true;
        self.submitted = false;
        self.install_started_at = Some(Instant::now());
    }

    pub fn set_failed(&mut self, detail: String) {
        self.phase = DialogPhase::Failed;
        self.status_line = format!("Connection to {} failed", self.host);
        self.error = Some(detail);
        self.input.clear();
    }

    pub fn toggle_logs(&mut self) {
        self.show_logs = !self.show_logs;
    }

    pub fn push_input_char(&mut self, c: char) {
        if !self.phase.awaits_input() {
            return;
        }
        self.input.push(c);
    }

    pub fn pop_input_char(&mut self) {
        if !self.phase.awaits_input() {
            return;
        }
        self.input.pop();
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
    }

    pub fn input_for_submit(&self) -> String {
        if matches!(self.phase, DialogPhase::AwaitingHostKeyConfirmation) && self.input.is_empty() {
            String::from("yes")
        } else {
            self.input.clone()
        }
    }

    /// True when the active prompt hides what the user types (passwords,
    /// passphrases). 2FA codes and host-key confirmations are visible.
    pub fn input_is_secret(&self) -> bool {
        matches!(
            self.phase,
            DialogPhase::AwaitingPassword | DialogPhase::AwaitingPassphrase
        )
    }
}

fn log_line_color(line: &str) -> Color {
    let lower = line.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") || lower.contains("permission denied") {
        return RED;
    }
    if lower.contains("warning") || lower.contains("warn:") {
        return Color::Rgb(0xf9, 0xc8, 0x5b);
    }
    if lower.starts_with("installed ")
        || lower.contains("install complete")
        || lower.contains("up to date")
        || lower.contains("authenticated")
    {
        return TEAL;
    }
    if lower.starts_with("compiling ")
        || lower.starts_with("downloading ")
        || lower.starts_with("downloaded ")
        || lower.starts_with("updating ")
        || lower.starts_with("running ")
        || lower.starts_with("installing ")
        || lower.starts_with("syncing ")
        || lower.starts_with("rsyncing ")
        || lower.starts_with("cross-compiling ")
        || lower.starts_with("hashing ")
        || lower.starts_with("checking ")
        || lower.starts_with("adopting ")
        || lower.starts_with("local source stamp")
    {
        return ACCENT;
    }
    Color::Rgb(0xb4, 0xbe, 0xc8)
}

fn default_status_for(phase: &DialogPhase, host: &str) -> String {
    match phase {
        DialogPhase::AwaitingPassword => format!("{} wants your password", host),
        DialogPhase::AwaitingPassphrase => String::from("Enter the key passphrase"),
        DialogPhase::AwaitingVerificationCode => String::from("Enter the 2FA code"),
        DialogPhase::AwaitingHostKeyConfirmation => {
            String::from("First-time host key, type yes to continue")
        }
        DialogPhase::AwaitingGeneric => String::from("Server prompt"),
        _ => String::new(),
    }
}

impl Widget for &mut ConnectDialog {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let installing = matches!(self.phase, DialogPhase::Installing);
        let target_width: u16 = if installing { 100 } else { 72 };
        let width = area.width.min(target_width).max(48);
        let (base_h, logs_h): (u16, u16) = if installing {
            (5, 24)
        } else if self.show_logs {
            (12, 12)
        } else {
            (12, 0)
        };
        let height = (base_h + logs_h).min(area.height.saturating_sub(2));
        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = area.y + area.height.saturating_sub(height) / 2;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        Clear.render(rect, buf);
        for dy in 0..rect.height {
            for dx in 0..rect.width {
                let cell = &mut buf[(rect.x + dx, rect.y + dy)];
                cell.set_char(' ');
                cell.set_style(Style::default().bg(PANEL_BG));
            }
        }
        let title_style = Style::default()
            .fg(Color::White)
            .bg(PANEL_BG)
            .add_modifier(Modifier::BOLD);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT).bg(PANEL_BG))
            .title(Span::styled(
                format!(" Connect — {} ", self.host),
                title_style,
            ));
        let inner = block.inner(rect);
        block.render(rect, buf);
        self.last_area = rect;
        self.last_inner = inner;

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        self.caret_pos = None;
        self.field_rect = Rect::default();

        let mut cy = inner.y;
        let phase_label = match self.phase {
            DialogPhase::Connecting => "Connecting",
            DialogPhase::AwaitingPassword => "Password",
            DialogPhase::AwaitingPassphrase => "Passphrase",
            DialogPhase::AwaitingVerificationCode => "2FA",
            DialogPhase::AwaitingHostKeyConfirmation => "Host key",
            DialogPhase::AwaitingGeneric => "Prompt",
            DialogPhase::Installing => "Installing",
            DialogPhase::Authenticated => "Done",
            DialogPhase::Failed => "Failed",
        };
        let phase_color = match self.phase {
            DialogPhase::Authenticated => TEAL,
            DialogPhase::Failed => RED,
            _ => ACCENT,
        };
        let indicator = if self.phase.is_terminal() {
            '●'
        } else {
            self.spinner_glyph()
        };
        let mut header_spans: Vec<Span<'_>> = vec![
            Span::styled(
                format!(" {indicator} "),
                Style::default().fg(phase_color).bg(PANEL_BG),
            ),
            Span::styled(
                phase_label,
                Style::default()
                    .fg(Color::White)
                    .bg(PANEL_BG)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", self.status_line),
                Style::default()
                    .fg(Color::Rgb(0xb4, 0xbe, 0xc8))
                    .bg(PANEL_BG),
            ),
        ];
        if installing && let Some(started) = self.install_started_at {
            let secs = started.elapsed().as_secs();
            let label = format!("  {:02}:{:02}", secs / 60, secs % 60);
            header_spans.push(Span::styled(
                label,
                Style::default()
                    .fg(TEAL)
                    .bg(PANEL_BG)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        buf.set_line(inner.x, cy, &Line::from(header_spans), inner.width);
        cy = cy.saturating_add(2);

        if !installing && cy < inner.y + inner.height {
            buf.set_line(
                inner.x,
                cy,
                &Line::from(vec![Span::styled(
                    truncate(&self.prompt_text, inner.width as usize),
                    Style::default()
                        .fg(Color::Rgb(0xe6, 0xed, 0xf5))
                        .bg(PANEL_BG),
                )]),
                inner.width,
            );
            cy = cy.saturating_add(2);
        }

        if self.phase.awaits_input() && cy + 1 < inner.y + inner.height {
            let field_w = inner.width.saturating_sub(2);
            let field_rect = Rect {
                x: inner.x + 1,
                y: cy,
                width: field_w,
                height: 1,
            };
            for dx in 0..field_rect.width {
                let cell = &mut buf[(field_rect.x + dx, field_rect.y)];
                cell.set_char(' ');
                cell.set_style(Style::default().bg(FIELD_BG));
            }
            let shown_text: String = if self.input_is_secret() {
                "•".repeat(self.input.chars().count())
            } else {
                self.input.clone()
            };
            let cursor_glyph = '▌';
            let shown = truncate(&shown_text, field_w.saturating_sub(1) as usize);
            buf.set_string(
                field_rect.x,
                field_rect.y,
                &shown,
                Style::default().fg(Color::White).bg(FIELD_BG),
            );
            let caret_col = field_rect
                .x
                .saturating_add(shown.chars().count() as u16)
                .min(field_rect.x + field_rect.width.saturating_sub(1));
            if self.input.is_empty() {
                let placeholder = match self.phase {
                    DialogPhase::AwaitingPassword => "  type your password and press Enter",
                    DialogPhase::AwaitingPassphrase => "  type your key passphrase and press Enter",
                    DialogPhase::AwaitingVerificationCode => "  type the 2FA code and press Enter",
                    DialogPhase::AwaitingHostKeyConfirmation => {
                        "  type yes and press Enter to trust this host"
                    }
                    DialogPhase::AwaitingGeneric => "  type a response and press Enter",
                    _ => "",
                };
                if !placeholder.is_empty() {
                    let truncated =
                        truncate(placeholder, field_rect.width.saturating_sub(2) as usize);
                    buf.set_string(
                        caret_col + 1,
                        field_rect.y,
                        &truncated,
                        Style::default()
                            .fg(Color::Rgb(0x6b, 0x74, 0x82))
                            .bg(FIELD_BG)
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            }
            buf.set_string(
                caret_col,
                field_rect.y,
                cursor_glyph.to_string(),
                Style::default()
                    .fg(ACCENT)
                    .bg(FIELD_BG)
                    .add_modifier(Modifier::BOLD),
            );
            self.field_rect = field_rect;
            self.caret_pos = Some((caret_col, field_rect.y));
            cy = cy.saturating_add(2);
        }

        if let Some(err) = &self.error
            && cy < inner.y + inner.height
        {
            buf.set_line(
                inner.x,
                cy,
                &Line::from(vec![Span::styled(
                    truncate(err, inner.width as usize),
                    Style::default().fg(RED).bg(PANEL_BG),
                )]),
                inner.width,
            );
            cy = cy.saturating_add(2);
        }

        let footer_y = inner.y + inner.height.saturating_sub(2);
        if footer_y >= cy {
            cy = footer_y;
        }
        let toggle_label = if self.show_logs {
            "[L] Hide logs"
        } else {
            "[L] Show logs"
        };
        let cancel_label = "[Esc] Cancel";
        let submit_label = if self.phase.is_terminal() {
            ""
        } else if matches!(self.phase, DialogPhase::AwaitingHostKeyConfirmation) {
            "[Enter] Continue"
        } else {
            "[Enter] Submit"
        };
        buf.set_line(
            inner.x,
            cy,
            &Line::from(vec![
                Span::styled(
                    toggle_label,
                    Style::default()
                        .fg(TEAL)
                        .bg(PANEL_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("    ", Style::default().bg(PANEL_BG)),
                Span::styled(
                    cancel_label,
                    Style::default()
                        .fg(Color::Rgb(0xb4, 0xbe, 0xc8))
                        .bg(PANEL_BG),
                ),
                Span::styled("    ", Style::default().bg(PANEL_BG)),
                Span::styled(
                    submit_label,
                    Style::default()
                        .fg(ACCENT)
                        .bg(PANEL_BG)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            inner.width,
        );
        self.toggle_logs_btn = Rect {
            x: inner.x,
            y: cy,
            width: toggle_label.chars().count() as u16,
            height: 1,
        };
        let cancel_x = inner.x + (toggle_label.chars().count() as u16) + 4;
        self.cancel_btn = Rect {
            x: cancel_x,
            y: cy,
            width: cancel_label.chars().count() as u16,
            height: 1,
        };
        let submit_x = cancel_x + (cancel_label.chars().count() as u16) + 4;
        self.submit_btn = Rect {
            x: submit_x,
            y: cy,
            width: submit_label.chars().count() as u16,
            height: 1,
        };

        if self.show_logs && cy > inner.y + 1 {
            let log_top = cy.saturating_sub(logs_h);
            if log_top > inner.y && log_top < cy {
                let log_rect = Rect {
                    x: inner.x,
                    y: log_top,
                    width: inner.width,
                    height: cy.saturating_sub(log_top).saturating_sub(1),
                };
                if log_rect.height > 0 {
                    let block = Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::DarkGray).bg(PANEL_BG))
                        .title(Span::styled(
                            " ssh transcript ",
                            Style::default()
                                .fg(Color::Rgb(0x9d, 0xa5, 0xb4))
                                .bg(PANEL_BG),
                        ));
                    let inner_log = block.inner(log_rect);
                    block.render(log_rect, buf);
                    let lines: Vec<&String> = self
                        .logs
                        .iter()
                        .rev()
                        .take(inner_log.height as usize)
                        .collect();
                    for (i, line) in lines.iter().rev().enumerate() {
                        let y = inner_log.y + i as u16;
                        if y >= inner_log.y + inner_log.height {
                            break;
                        }
                        let txt = truncate(line, inner_log.width as usize);
                        for dx in 0..inner_log.width {
                            let cell = &mut buf[(inner_log.x + dx, y)];
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(PANEL_BG));
                        }
                        let fg = log_line_color(line);
                        buf.set_string(inner_log.x, y, txt, Style::default().fg(fg).bg(PANEL_BG));
                    }
                }
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_dialog_starts_in_connecting_phase_with_no_input() {
        let d = ConnectDialog::new("genesis-cloud".to_string());
        assert_eq!(d.phase, DialogPhase::Connecting);
        assert!(d.input.is_empty());
        assert!(!d.show_logs);
    }

    #[test]
    fn set_prompt_switches_phase_per_prompt_kind() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_prompt(PromptKind::Password, "user@host's password: ".into());
        assert_eq!(d.phase, DialogPhase::AwaitingPassword);
        d.set_prompt(PromptKind::Passphrase, "Enter passphrase: ".into());
        assert_eq!(d.phase, DialogPhase::AwaitingPassphrase);
        d.set_prompt(PromptKind::VerificationCode, "Verification code: ".into());
        assert_eq!(d.phase, DialogPhase::AwaitingVerificationCode);
        d.set_prompt(
            PromptKind::HostKeyConfirmation,
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? ".into(),
        );
        assert_eq!(d.phase, DialogPhase::AwaitingHostKeyConfirmation);
    }

    #[test]
    fn input_is_secret_only_for_password_and_passphrase_phases() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_prompt(PromptKind::Password, "p".into());
        assert!(d.input_is_secret());
        d.set_prompt(PromptKind::Passphrase, "p".into());
        assert!(d.input_is_secret());
        d.set_prompt(PromptKind::VerificationCode, "c".into());
        assert!(!d.input_is_secret(), "2FA codes are not secret to the user");
        d.set_prompt(PromptKind::HostKeyConfirmation, "yes/no".into());
        assert!(!d.input_is_secret());
    }

    #[test]
    fn host_key_confirmation_defaults_submit_to_yes_when_user_just_hits_enter() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_prompt(PromptKind::HostKeyConfirmation, "yes/no".into());
        assert_eq!(d.input_for_submit(), "yes");
    }

    #[test]
    fn push_input_only_accumulates_during_awaiting_phases() {
        let mut d = ConnectDialog::new("h".to_string());
        d.push_input_char('x');
        assert!(d.input.is_empty(), "no input in Connecting phase");
        d.set_prompt(PromptKind::Password, "p".into());
        d.push_input_char('x');
        d.push_input_char('y');
        assert_eq!(d.input, "xy");
        d.pop_input_char();
        assert_eq!(d.input, "x");
    }

    #[test]
    fn toggle_logs_is_idempotent_pair() {
        let mut d = ConnectDialog::new("h".to_string());
        assert!(!d.show_logs);
        d.toggle_logs();
        assert!(d.show_logs);
        d.toggle_logs();
        assert!(!d.show_logs);
    }

    #[test]
    fn push_log_splits_on_newlines_and_caps_buffer() {
        let mut d = ConnectDialog::new("h".to_string());
        for _ in 0..(MAX_LOG_LINES + 50) {
            d.push_log("line\n");
        }
        assert_eq!(
            d.logs.len(),
            MAX_LOG_LINES,
            "log buffer must cap so a chatty ssh transcript doesn't grow without bound"
        );
        d.push_log("first\nsecond\n");
        assert!(d.logs.last().unwrap().contains("second"));
    }

    #[test]
    fn dialog_renders_password_field_as_bullets_not_plaintext() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_prompt(PromptKind::Password, "p".into());
        for c in "hunter2".chars() {
            d.push_input_char(c);
        }
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 18,
        };
        let mut buf = Buffer::empty(area);
        (&mut d).render(area, &mut buf);
        let mut joined = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                joined.push_str(buf[(x, y)].symbol());
            }
            joined.push('\n');
        }
        assert!(
            !joined.contains("hunter2"),
            "password modal must NEVER render the typed password verbatim"
        );
        assert!(
            joined.contains('•'),
            "password modal must render typed characters as • bullets"
        );
    }

    #[test]
    fn awaiting_phase_publishes_caret_position_inside_the_field() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_prompt(PromptKind::Password, "p".into());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 18,
        };
        let mut buf = Buffer::empty(area);
        (&mut d).render(area, &mut buf);
        let (cx, cy) =
            d.cursor_screen_pos().expect("password phase must publish the caret position so the host terminal blinks the OS caret where the user types");
        assert!(cx >= d.field_rect.x);
        assert!(cx < d.field_rect.x + d.field_rect.width);
        assert_eq!(cy, d.field_rect.y);
    }

    #[test]
    fn empty_field_still_shows_a_caret_and_a_placeholder_so_the_user_can_tell_typing_is_alive() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_prompt(PromptKind::Password, "p".into());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 18,
        };
        let mut buf = Buffer::empty(area);
        (&mut d).render(area, &mut buf);
        let mut field_row = String::new();
        for x in d.field_rect.x..(d.field_rect.x + d.field_rect.width) {
            field_row.push_str(buf[(x, d.field_rect.y)].symbol());
        }
        assert!(
            field_row.contains('▌'),
            "an empty password field must still paint a caret glyph (▌) so the user knows the field is live (regression: blank field looked dead, user could not tell typing was registering)"
        );
        assert!(
            field_row.contains("type your password"),
            "an empty password field must show placeholder text so the user knows what to type (got row: {field_row:?})"
        );
    }

    #[test]
    fn caret_position_advances_as_the_user_types_into_the_field() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_prompt(PromptKind::Password, "p".into());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 18,
        };
        let mut buf = Buffer::empty(area);
        (&mut d).render(area, &mut buf);
        let initial = d.cursor_screen_pos().unwrap();
        for c in "abcd".chars() {
            d.push_input_char(c);
        }
        (&mut d).render(area, &mut buf);
        let after = d.cursor_screen_pos().unwrap();
        assert!(
            after.0 > initial.0,
            "caret column must advance after typing — initial={initial:?}, after={after:?}"
        );
    }

    #[test]
    fn toggle_logs_button_registers_hit_rect_so_mouse_can_click_it() {
        let mut d = ConnectDialog::new("h".to_string());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 18,
        };
        let mut buf = Buffer::empty(area);
        (&mut d).render(area, &mut buf);
        assert!(
            d.toggle_logs_btn.width > 0,
            "toggle-logs button must register a hit rect so clicking it flips show_logs"
        );
        assert!(d.cancel_btn.width > 0);
    }

    #[test]
    fn set_authenticated_moves_to_terminal_phase_and_clears_input() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_prompt(PromptKind::Password, "p".into());
        d.push_input_char('x');
        d.set_authenticated();
        assert_eq!(d.phase, DialogPhase::Authenticated);
        assert!(d.phase.is_terminal());
        assert!(d.input.is_empty());
    }

    #[test]
    fn set_failed_records_the_error_and_moves_to_terminal_phase() {
        let mut d = ConnectDialog::new("h".to_string());
        d.set_failed("ssh exited 255".to_string());
        assert_eq!(d.phase, DialogPhase::Failed);
        assert!(d.phase.is_terminal());
        assert_eq!(d.error.as_deref(), Some("ssh exited 255"));
    }
}
