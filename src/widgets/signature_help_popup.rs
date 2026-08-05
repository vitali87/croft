//! LSP Signature Help (parameter hints): a floating one-line popup showing the
//! active call's signature with the current parameter bolded, anchored to the
//! cursor. Auto-triggered when typing `(` or `,` inside a call; dismissed on
//! `)` or Esc. Purely informational — it never captures editing keys.

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::lsp::manager::SignatureInfo;

const MIN_WIDTH: u16 = 20;
const MAX_WIDTH: u16 = 100;

pub struct SignatureHelpPopup {
    pub signatures: Vec<SignatureInfo>,
    pub active_signature: usize,
    pub anchor: (u16, u16),
    pub path: PathBuf,
    pub request_id: u64,
    /// Black theme: gradient border instead of the legacy bright-blue. Set by
    /// the app before render from `popup_gradient`.
    pub gradient: bool,
}

impl SignatureHelpPopup {
    pub fn new(
        signatures: Vec<SignatureInfo>,
        active_signature: usize,
        anchor: (u16, u16),
        path: PathBuf,
        request_id: u64,
    ) -> Self {
        Self {
            signatures,
            active_signature,
            anchor,
            path,
            request_id,
            gradient: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.signatures.is_empty()
    }

    fn active(&self) -> Option<&SignatureInfo> {
        self.signatures.get(self.active_signature)
    }

    /// The popup rect, anchored to the cursor. Prefers sitting ABOVE the caret
    /// (VS Code's parameter hints), so a completion popup below the caret and
    /// the signature above don't fight for the same row; flips below only when
    /// there's no room above.
    pub fn area_for(&self, viewport: Rect) -> Rect {
        let label_w = self
            .active()
            .map(|s| s.label.chars().count() as u16)
            .unwrap_or(MIN_WIDTH);
        // +1 each side of padding, +2 border.
        let width = label_w.saturating_add(4).clamp(MIN_WIDTH, MAX_WIDTH);
        let height = 3; // one content row + top/bottom border
        let (cx, cy) = self.anchor;
        let mut x = cx;
        if x.saturating_add(width) > viewport.right() {
            x = viewport.right().saturating_sub(width);
        }
        // Back inside the pane's own left edge. The overflow clamp above pushes
        // a wide signature left without a floor, and the width clamp below
        // cannot repair it: `x - viewport.x` saturates to 0 once x is left of
        // the pane, so the popup kept its full width and painted over the
        // sidebar (or the other split). Same clamp `hover_popup` already has.
        if x < viewport.x {
            x = viewport.x;
        }
        // Above the caret if it fits, else below.
        let y = if cy >= viewport.y + height {
            cy - height
        } else {
            cy.saturating_add(1)
        };
        Rect {
            x,
            y,
            width: width.min(viewport.width.saturating_sub(x.saturating_sub(viewport.x))),
            height: height.min(viewport.height.saturating_sub(y.saturating_sub(viewport.y))),
        }
    }
}

impl Widget for &SignatureHelpPopup {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(sig) = self.active() else {
            return;
        };
        // Split the label into (before, active param, after) so the active
        // parameter renders bold-and-bright while the rest stays dim.
        let base = Style::default().fg(Color::Rgb(0xc8, 0xce, 0xda));
        let active = Style::default()
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .add_modifier(Modifier::BOLD);
        let chars: Vec<char> = sig.label.chars().collect();
        let spans: Vec<Span<'static>> = match sig.active_param {
            Some((s, e)) if s < e && e <= chars.len() => {
                let take = |r: std::ops::Range<usize>| chars[r].iter().collect::<String>();
                vec![
                    Span::styled(format!(" {}", take(0..s)), base),
                    Span::styled(take(s..e), active),
                    Span::styled(format!("{} ", take(e..chars.len())), base),
                ]
            }
            _ => vec![Span::styled(format!(" {} ", sig.label), base)],
        };

        // A "2 of 3" counter when the server offers overloads, like VS Code.
        let mut title_spans = Vec::new();
        if self.signatures.len() > 1 {
            title_spans.push(Span::styled(
                format!(" {}/{} ", self.active_signature + 1, self.signatures.len()),
                Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0)),
            ));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
            .title(Line::from(title_spans))
            .style(Style::default().bg(Color::Rgb(0x1e, 0x21, 0x2a)));

        Widget::render(Clear, area, buf);
        Widget::render(Paragraph::new(Line::from(spans)).block(block), area, buf);
        if self.gradient {
            crate::gradient::paint_gradient_box(buf, area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(label: &str, active: Option<(usize, usize)>) -> SignatureInfo {
        SignatureInfo {
            label: label.to_string(),
            active_param: active,
        }
    }

    /// The overflow clamp pushed `x` left to fit a wide signature, but nothing
    /// clamped it back to the pane's own left edge — and the trailing width
    /// clamp cannot repair it, since `x - viewport.x` saturates to 0 once `x`
    /// is left of the pane. With the sidebar open (or in a split) the popup
    /// painted over the Explorer, outside the editor entirely. `hover_popup`
    /// has the missing clamp; this one, with MAX_WIDTH 100 against the
    /// completion popup's 60, is the one wide enough to hit it.
    #[test]
    fn a_wide_signature_stays_inside_its_pane() {
        let label = format!("fn {}(a: i32, b: i32)", "x".repeat(80)); // 97 chars
        let p = SignatureHelpPopup::new(
            vec![sig(&label, Some((3, 5)))],
            0,
            (60, 20),
            PathBuf::from("f.rs"),
            1,
        );
        // 120-col terminal with the Explorer sidebar open.
        let vp = Rect {
            x: 37,
            y: 1,
            width: 83,
            height: 40,
        };
        let r = p.area_for(vp);
        assert!(
            r.x >= vp.x,
            "popup must not start left of its pane (x {} < {})",
            r.x,
            vp.x
        );
        assert!(
            r.right() <= vp.right(),
            "and must not run past its right edge ({} > {})",
            r.right(),
            vp.right()
        );
    }

    #[test]
    fn area_prefers_above_the_caret() {
        let p = SignatureHelpPopup::new(
            vec![sig("fn foo(a: i32, b: i32)", Some((7, 13)))],
            0,
            (10, 20),
            PathBuf::from("f.rs"),
            1,
        );
        let vp = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        let r = p.area_for(vp);
        // height 3, caret row 20 → sits at 17 (above).
        assert_eq!(r.y, 17);
    }

    #[test]
    fn area_flips_below_when_no_room_above() {
        let p = SignatureHelpPopup::new(
            vec![sig("fn foo(a: i32)", Some((7, 13)))],
            0,
            (5, 1),
            PathBuf::from("f.rs"),
            1,
        );
        let vp = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        let r = p.area_for(vp);
        // caret row 1 (< height 3) → drop below to row 2.
        assert_eq!(r.y, 2);
    }

    #[test]
    fn renders_active_parameter_in_bold() {
        use ratatui::buffer::Buffer;
        let p = SignatureHelpPopup::new(
            vec![sig("foo(a, b)", Some((4, 5)))], // bold the "a"
            0,
            (0, 5),
            PathBuf::from("f.rs"),
            1,
        );
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 3,
        };
        let mut buf = Buffer::empty(area);
        (&p).render(area, &mut buf);
        // Find the 'a' cell on the content row and assert it's bold.
        let row = 1;
        let mut found_bold_a = false;
        for x in area.x..area.right() {
            let cell = &buf[(x, row)];
            if cell.symbol() == "a" && cell.modifier.contains(Modifier::BOLD) {
                found_bold_a = true;
            }
        }
        assert!(found_bold_a, "active parameter 'a' must render bold");
    }
}
