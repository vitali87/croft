//! The "No repository detected" card: its text, its buttons and the Git fork drawing, full
//! size or compact.

use super::*;

impl SourceControlPanel {
    /// Paint the "No repository detected" empty-state card. `inner` is the
    /// panel's inner rect (border already drawn); the SOURCE CONTROL header
    /// sits on `inner.y` so the card starts two rows below.
    pub(super) fn render_no_repo_empty_state(&mut self, inner: Rect, buf: &mut Buffer) {
        if inner.height < 4 || inner.width < 8 {
            return;
        }
        let card_top = inner.y + 2;
        let card_bottom = inner.y + inner.height - 1;
        let card_h = card_bottom.saturating_sub(card_top);
        if card_h < 3 {
            return;
        }
        let side_pad: u16 = if inner.width >= 24 { 2 } else { 1 };
        let card_x = inner.x + side_pad;
        let card_w = inner.width.saturating_sub(side_pad * 2);
        let card_area = Rect {
            x: card_x,
            y: card_top,
            width: card_w,
            height: card_h,
        };
        let card_border = Style::default().fg(self.theme.ui(Color::Rgb(0x60, 0x68, 0x78)));
        let card_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(card_border);
        let card_inner = card_block.inner(card_area);
        // Croft Loch: a raised card with a rim (it never takes the keyboard,
        // so no accent lip) in place of the rounded line box; same inset.
        let tones = crate::widgets::surface::tones(self.theme);
        match tones {
            Some(t) => {
                buf.set_style(card_area, Style::default().bg(t.raised()));
                crate::widgets::surface::paint_quiet_frame(buf, card_area, self.theme);
            }
            None => card_block.render(card_area, buf),
        }
        if card_inner.width < 4 || card_inner.height < 3 {
            return;
        }

        // Black theme: the brand's teal card accent and button fill replace
        // the legacy blue line art and VS Code button blue; Croft Dark keeps
        // the historical blues.
        let blue = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::CARD_ACCENT)
        } else {
            self.theme.ui(Color::Rgb(0x60, 0x9a, 0xfe))
        };
        let dim_blue = self.theme.ui(Color::Rgb(0x4b, 0x50, 0x5a));
        let text_white = tones.map_or_else(
            || self.theme.ui(Color::Rgb(0xff, 0xff, 0xff)),
            |t| t.strong(),
        );
        let text_dim = tones.map_or_else(
            || self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
            |t| t.muted(),
        );
        let blue_bg = if self.focus_gradient || tones.is_some() {
            crate::gradient::rgb_color(crate::gradient::PRIMARY_BTN_BG)
        } else {
            Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2)
        };

        let mut y = card_inner.y + 2;
        let bottom = card_inner.y + card_inner.height;

        // Illustration: reserve a centred block for the OSC-1337 hero
        // image (App paints the PNG over these cells on iTerm2-class
        // terminals). Paint the ASCII Y-fork as a fallback inside the
        // same rect so non-iTerm2 terminals still see a logo.
        let hero_h: u16 = if card_inner.height >= 18 {
            8
        } else if card_inner.height >= 14 {
            7
        } else {
            5
        };
        let hero_w: u16 = (hero_h * 2).min(card_inner.width).max(7);
        let hero_x = card_inner.x + (card_inner.width.saturating_sub(hero_w)) / 2;
        if y + hero_h <= bottom {
            self.last_hero_area = Rect {
                x: hero_x,
                y,
                width: hero_w,
                height: hero_h,
            };
            // Only paint the ASCII Y-fork when there's NO inline-image
            // hero owning these cells. With OSC-1337 active, leave the
            // rect blank: ratatui's text would briefly flash through the
            // image during a resize, which the user (correctly) called
            // out as old-logo glimpses peeking through the new one.
            if !self.inline_hero_image_active {
                if card_inner.width >= 13 && hero_h >= 7 {
                    paint_y_fork_illustration(buf, card_inner, y, dim_blue, blue);
                } else {
                    paint_y_fork_compact(buf, card_inner, y, blue);
                }
            }
            y += hero_h + 1;
        }

        // Title and description go through ratatui's `Paragraph` with
        // word wrapping so a resized panel reflows the text instead of
        // truncating mid-word - that's the "text inside is not even
        // adjusted" bug from the user's screenshot.
        let nested = !self.nested_repos.is_empty();
        let title_text = if nested {
            "Repositories found in subfolders"
        } else {
            "No repository detected"
        };
        let title_h = paragraph_line_count(title_text, card_inner.width).min(3) as u16;
        if y + title_h <= bottom && title_h > 0 {
            let title_rect = Rect {
                x: card_inner.x,
                y,
                width: card_inner.width,
                height: title_h,
            };
            let title = Paragraph::new(title_text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(text_white).add_modifier(Modifier::BOLD));
            title.render(title_rect, buf);
            y += title_h + 1;
        }

        let desc_text = if nested {
            "This folder is not a Git repository, but these subfolders are. Open one to use Source Control."
        } else {
            "Open a folder under Git or create a new repository to start tracking changes."
        };
        let desc_h = paragraph_line_count(desc_text, card_inner.width).min(6) as u16;
        if y + desc_h <= bottom && desc_h > 0 {
            let desc_rect = Rect {
                x: card_inner.x,
                y,
                width: card_inner.width,
                height: desc_h,
            };
            let desc = Paragraph::new(desc_text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(text_dim));
            desc.render(desc_rect, buf);
            y += desc_h + 2;
        }

        // Primary button, centered, width capped so it doesn't sprawl on
        // a wide sidebar.
        let max_btn_w: u16 = 40;
        let btn_w = card_inner.width.saturating_sub(2).min(max_btn_w);
        let btn_x = card_inner.x + (card_inner.width.saturating_sub(btn_w)) / 2;
        if nested {
            // One Open button per repo while they fit; Initialize would
            // `git init` this folder around them, so it is not offered.
            for (label, path) in &self.nested_repos {
                if y + 3 > bottom || btn_w < 6 {
                    break;
                }
                let area = Rect {
                    x: btn_x,
                    y,
                    width: btn_w,
                    height: 3,
                };
                let text = format!("Open {label}");
                let hovered = crate::widgets::hover::contains(area, self.hover_pointer);
                render_button(buf, area, &text, blue_bg, text_white, self.theme, hovered);
                self.nested_repo_button_areas.push((area, path.clone()));
                y += 4;
            }
            return;
        }
        let init_label: &str = if btn_w >= 23 {
            "Initialize Repository"
        } else if btn_w >= 12 {
            "Initialize"
        } else {
            "Init"
        };
        if y + 3 <= bottom && btn_w >= 6 {
            let init_area = Rect {
                x: btn_x,
                y,
                width: btn_w,
                height: 3,
            };
            self.last_init_repo_button_area = init_area;
            let hovered = crate::widgets::hover::contains(init_area, self.hover_pointer);
            render_button(
                buf, init_area, init_label, blue_bg, text_white, self.theme, hovered,
            );
        }
    }
}

/// How many rows ratatui needs to render `text` as a centered, wrapped
/// `Paragraph` at `width`. Uses the same word-wrap rules as ratatui itself
/// (whitespace-separated, no mid-word breaks unless a word exceeds the
/// width). Returns 0 when `width == 0` or `text` is empty, otherwise at least 1.
pub(super) fn paragraph_line_count(text: &str, width: u16) -> usize {
    if width == 0 || text.is_empty() {
        return 0;
    }
    let max = width as usize;
    let mut lines: usize = 1;
    let mut current: usize = 0;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if current == 0 {
            current = wlen.min(max);
            if wlen > max {
                lines += (wlen - 1) / max;
                current = (wlen - 1) % max + 1;
            }
        } else if current + 1 + wlen <= max {
            current += 1 + wlen;
        } else {
            lines += 1;
            current = wlen.min(max);
            if wlen > max {
                lines += (wlen - 1) / max;
                current = (wlen - 1) % max + 1;
            }
        }
    }
    lines
}

/// 7-row "elegant" illustration: a Git Y-fork (top node, trunk vertical,
/// branch row with side node, trunk vertical, bottom node) framed by a
/// dashed ring of dots. The classic source-control silhouette - no file
/// outline competing for attention.
pub(super) fn paint_y_fork_illustration(
    buf: &mut Buffer,
    card_inner: Rect,
    top_y: u16,
    dim_blue: Color,
    blue: Color,
) {
    let dot = Style::default().fg(dim_blue);
    let trunk = Style::default().fg(blue);
    let node = Style::default().fg(blue).add_modifier(Modifier::BOLD);
    let cx = card_inner.x + card_inner.width / 2;

    // Top + bottom dashed arcs framing the tree.
    let arc = "· · · · ·";
    let arc_w = arc.chars().count() as u16;
    let arc_x = card_inner.x + (card_inner.width - arc_w) / 2;
    buf.set_string(arc_x, top_y, arc, dot);
    buf.set_string(arc_x, top_y + 6, arc, dot);

    // Side dots flanking the tree on the 5 inner rows, completing the
    // dashed-ring silhouette of the SVG mockup.
    let side_off: u16 = 5;
    if cx > card_inner.x + side_off {
        buf.set_string(cx - side_off, top_y + 1, "·", dot);
        buf.set_string(cx - side_off, top_y + 3, "·", dot);
        buf.set_string(cx - side_off, top_y + 5, "·", dot);
    }
    if cx + side_off < card_inner.x + card_inner.width {
        buf.set_string(cx + side_off, top_y + 1, "·", dot);
        buf.set_string(cx + side_off, top_y + 3, "·", dot);
        buf.set_string(cx + side_off, top_y + 5, "·", dot);
    }

    // Y-fork tree - three nodes (●) on the trunk column, branch out on
    // the middle row to a side node, trunk verticals between rows.
    //     ●
    //     │
    //     ●─●
    //     │
    //     ●
    buf.set_string(cx, top_y + 1, "●", node);
    buf.set_string(cx, top_y + 2, "│", trunk);
    buf.set_string(cx, top_y + 3, "●", node);
    buf.set_string(cx + 1, top_y + 3, "─", trunk);
    if cx + 2 < card_inner.x + card_inner.width {
        buf.set_string(cx + 2, top_y + 3, "●", node);
    }
    buf.set_string(cx, top_y + 4, "│", trunk);
    buf.set_string(cx, top_y + 5, "●", node);
}

/// 5-row compact fallback for narrow panels: just the Y-fork tree, no
/// surrounding ring. Width: 5 cells.
pub(super) fn paint_y_fork_compact(buf: &mut Buffer, card_inner: Rect, top_y: u16, blue: Color) {
    let trunk = Style::default().fg(blue);
    let node = Style::default().fg(blue).add_modifier(Modifier::BOLD);
    let cx = card_inner.x + card_inner.width / 2;
    buf.set_string(cx, top_y, "●", node);
    buf.set_string(cx, top_y + 1, "│", trunk);
    buf.set_string(cx, top_y + 2, "●", node);
    buf.set_string(cx + 1, top_y + 2, "─", trunk);
    if cx + 2 < card_inner.x + card_inner.width {
        buf.set_string(cx + 2, top_y + 2, "●", node);
    }
    buf.set_string(cx, top_y + 3, "│", trunk);
    buf.set_string(cx, top_y + 4, "●", node);
}
