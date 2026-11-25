//! The diff and merge views: side-by-side and inline diffs, the three-way merge panes and
//! their accept chips, merge conflicts, and a file as it was at a commit.

use super::*;

/// The colour of a SARIF analysis step drawn after its line (#577).
pub(super) const STEP_MARK_COLOR: Color = Color::Rgb(0x4f, 0xc1, 0xff);

/// The fewest body rows a merge editor source pane needs before it gives
/// one to the accept buttons above a conflict (#1169).
pub(super) const MERGE_BUTTON_ROW_MIN_BODY: usize = 4;

/// Render the merge editor's source panes (#253) into the TOP of `inner`
/// and return the remaining rect for the ordinary text path — the
/// editable Result pane. One header row carries the resolved counter,
/// then Current | (Base) | Incoming as columns (stacked vertically when
/// the terminal is narrow), then a separator row labelling the Result.
/// Per-conflict checkboxes are painted in a small gutter on the region's
/// first row, accept buttons on a row above it, and their hit rects
/// recorded on the view for mouse routing.
pub(super) fn render_merge_panes(
    mv: &mut crate::merge_editor::MergeView,
    inner: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
) -> Rect {
    use crate::merge_editor::{CheckSide, ConflictState};
    mv.check_spans.clear();
    mv.action_spans.clear();
    mv.last_panes_area = Rect::default();
    // Too small for source panes: the Result gets everything, and the
    // commands (F7, accepts) still work off the tracked regions.
    if inner.height < 9 || inner.width < 24 {
        return inner;
    }
    let top_h = (inner.height / 2).min(inner.height - 4);
    let sep_y = inner.y + top_h;
    mv.last_panes_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: top_h + 1,
    };

    // Header: the acceptance counter plus the working chords.
    let resolved = mv.resolved_count();
    let total = mv.conflicts.len();
    let head_style = Style::default()
        .fg(Color::White)
        .bg(theme.ui(Color::Rgb(0x09, 0x4d, 0x77)))
        .add_modifier(Modifier::BOLD);
    for x in inner.x..inner.x + inner.width {
        buf[(x, inner.y)].set_style(head_style);
        buf[(x, inner.y)].set_symbol(" ");
    }
    let src = if let Some(agent) = mv.proposal_from.as_deref() {
        format!("{agent}'s proposal \u{2194} your unsaved edits")
    } else if mv.from_markers {
        String::from("markers")
    } else {
        String::from("git stages")
    };
    let header = format!(
        " MERGE ({src})  {resolved}/{total} conflict{} resolved \u{2022} F7 next \u{2022} Alt+\u{2191}\u{2193} scroll sources ",
        if total == 1 { "" } else { "s" }
    );
    buf.set_stringn(inner.x, inner.y, &header, inner.width as usize, head_style);

    // Separator row above the Result.
    let sep_style = Style::default()
        .fg(theme.ui(Color::Rgb(0x9a, 0xa4, 0xb2)))
        .bg(theme.ui(Color::Rgb(0x20, 0x24, 0x2c)));
    for x in inner.x..inner.x + inner.width {
        buf[(x, sep_y)].set_style(sep_style);
        buf[(x, sep_y)].set_symbol(" ");
    }
    let result_hint = if mv.proposal_from.is_some() {
        " RESULT (editable) \u{2014} save to approve \u{b7} close unsaved to go back "
    } else {
        " RESULT (editable) \u{2014} \"Merge: Complete Merge\" stages the file "
    };
    buf.set_stringn(inner.x, sep_y, result_hint, inner.width as usize, sep_style);

    // The panes: Current | (Base) | Incoming, or stacked when narrow.
    struct Pane<'a> {
        title: &'a str,
        lines: &'a [String],
        scroll: usize,
        side: Option<CheckSide>,
        // (start row in this pane's text, len, conflict idx, checked)
        regions: Vec<(usize, usize, usize, bool)>,
        tint: Color,
        active_tint: Color,
    }
    let region = |start: usize, len: usize, idx: usize, checked: bool| (start, len, idx, checked);
    let checked_cur = |s: ConflictState| {
        matches!(
            s,
            ConflictState::Current | ConflictState::Both | ConflictState::BothReverse
        )
    };
    let checked_inc = |s: ConflictState| {
        matches!(
            s,
            ConflictState::Incoming | ConflictState::Both | ConflictState::BothReverse
        )
    };
    let mut panes: Vec<Pane> = Vec::new();
    panes.push(Pane {
        title: if mv.deleted == Some(CheckSide::Current) {
            "CURRENT (yours): deleted"
        } else if mv.proposal_from.is_some() {
            "CURRENT (your unsaved edits)"
        } else {
            "CURRENT (yours)"
        },
        lines: &mv.ours,
        scroll: mv.ours_scroll,
        side: Some(CheckSide::Current),
        regions: mv
            .conflicts
            .iter()
            .enumerate()
            .map(|(i, c)| region(c.ours_start, c.ours.len(), i, checked_cur(c.state)))
            .collect(),
        tint: theme.ui(Color::Rgb(0x1b, 0x33, 0x22)),
        active_tint: theme.ui(Color::Rgb(0x2a, 0x4f, 0x33)),
    });
    if mv.show_base {
        panes.push(Pane {
            title: "BASE",
            lines: &mv.base,
            scroll: mv.base_scroll,
            side: None,
            regions: mv
                .conflicts
                .iter()
                .enumerate()
                .map(|(i, c)| region(c.base_start, c.base.len(), i, false))
                .collect(),
            tint: theme.ui(Color::Rgb(0x2a, 0x2a, 0x2a)),
            active_tint: theme.ui(Color::Rgb(0x3a, 0x3a, 0x3a)),
        });
    }
    panes.push(Pane {
        title: if mv.deleted == Some(CheckSide::Incoming) {
            "INCOMING (theirs): deleted"
        } else if mv.proposal_from.is_some() {
            "INCOMING (the agent's proposal)"
        } else {
            "INCOMING (theirs)"
        },
        lines: &mv.theirs,
        scroll: mv.theirs_scroll,
        side: Some(CheckSide::Incoming),
        regions: mv
            .conflicts
            .iter()
            .enumerate()
            .map(|(i, c)| region(c.theirs_start, c.theirs.len(), i, checked_inc(c.state)))
            .collect(),
        tint: theme.ui(Color::Rgb(0x16, 0x2b, 0x44)),
        active_tint: theme.ui(Color::Rgb(0x1f, 0x41, 0x66)),
    });

    let panes_top = inner.y + 1;
    let panes_h = top_h.saturating_sub(1);
    let n = panes.len() as u16;
    let stacked = inner.width < 72;
    let mut rects: Vec<Rect> = Vec::new();
    if stacked {
        // Vertical stack: each pane gets an equal share of the rows.
        let share = panes_h / n;
        for i in 0..n {
            rects.push(Rect {
                x: inner.x,
                y: panes_top + i * share,
                width: inner.width,
                height: if i == n - 1 {
                    panes_h - share * (n - 1)
                } else {
                    share
                },
            });
        }
    } else {
        // Columns with a 1-cell seam between neighbours.
        let w = (inner.width - (n - 1)) / n;
        let mut x = inner.x;
        for i in 0..n {
            let width = if i == n - 1 {
                inner.x + inner.width - x
            } else {
                w
            };
            rects.push(Rect {
                x,
                y: panes_top,
                width,
                height: panes_h,
            });
            x += w;
            if i != n - 1 {
                for y in panes_top..panes_top + panes_h {
                    buf[(x, y)].set_symbol("\u{2502}");
                    buf[(x, y)]
                        .set_style(Style::default().fg(theme.ui(Color::Rgb(0x3a, 0x42, 0x52))));
                }
                x += 1;
            }
        }
    }

    let title_style = Style::default()
        .fg(theme.ui(Color::Rgb(0xc8, 0xd0, 0xdc)))
        .bg(theme.ui(Color::Rgb(0x20, 0x24, 0x2c)))
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(theme.ui(Color::Rgb(0xb6, 0xbd, 0xc8)));
    let gutter = 4u16; // "[x] " on a region's first row
    for (pane, rect) in panes.iter().zip(&rects) {
        if rect.height < 2 || rect.width < gutter + 4 {
            continue;
        }
        for x in rect.x..rect.x + rect.width {
            buf[(x, rect.y)].set_style(title_style);
            buf[(x, rect.y)].set_symbol(" ");
        }
        buf.set_stringn(
            rect.x + 1,
            rect.y,
            pane.title,
            rect.width.saturating_sub(1) as usize,
            title_style,
        );
        let body_h = (rect.height - 1) as usize;
        let text_w = (rect.width - gutter) as usize;
        // A side pane puts a row of accept buttons above each conflict
        // (#1169), VS Code's merge editor CodeLens, so the lines below it
        // move down a row. An empty side (a deletion) still gets its row. A
        // pane too short to spare the row keeps only its checkboxes, rather
        // than pushing the conflict itself out of sight.
        let mut idx = pane.scroll;
        let mut buttons_drawn: Vec<usize> = Vec::new();
        let button_rows = body_h >= MERGE_BUTTON_ROW_MIN_BODY;
        for vis in 0..body_h {
            let y = rect.y + 1 + vis as u16;
            let buttons = pane.side.filter(|_| button_rows).and_then(|side| {
                pane.regions
                    .iter()
                    .find(|(start, _, ci, _)| *start == idx && !buttons_drawn.contains(ci))
                    .map(|&(_, _, ci, _)| (side, ci))
            });
            if let Some((side, ci)) = buttons {
                buttons_drawn.push(ci);
                let tint = if ci == mv.active {
                    pane.active_tint
                } else {
                    pane.tint
                };
                for x in rect.x..rect.x + rect.width {
                    buf[(x, y)].set_style(text_style.bg(tint));
                    buf[(x, y)].set_symbol(" ");
                }
                let state = mv.conflicts[ci].state;
                let both = matches!(state, ConflictState::Both | ConflictState::BothReverse);
                let (own, own_state) = match side {
                    CheckSide::Current => (
                        AcceptChip::new("Accept Current", "Current", ACCEPT_CURRENT_FILL)
                            .ticked(state == ConflictState::Current),
                        ConflictState::Current,
                    ),
                    CheckSide::Incoming => (
                        AcceptChip::new("Accept Incoming", "Incoming", ACCEPT_INCOMING_FILL)
                            .ticked(state == ConflictState::Incoming),
                        ConflictState::Incoming,
                    ),
                };
                let chips = [
                    own,
                    AcceptChip::new("Accept Both", "Both", ACCEPT_BOTH_FILL).ticked(both),
                ];
                let states = [own_state, ConflictState::Both];
                for (i, xs) in
                    paint_accept_chips(buf, &chips, rect.x + 1, rect.x + rect.width, y, theme)
                {
                    mv.action_spans.push((y, xs, ci, states[i]));
                }
                continue;
            }
            if idx >= pane.lines.len() {
                break;
            }
            let in_region = pane
                .regions
                .iter()
                .find(|(start, len, _, _)| *len > 0 && idx >= *start && idx < *start + *len);
            let style = match in_region {
                Some(&(_, _, ci, _)) if ci == mv.active => text_style.bg(pane.active_tint),
                Some(_) => text_style.bg(pane.tint),
                None => text_style,
            };
            if in_region.is_some() {
                for x in rect.x..rect.x + rect.width {
                    buf[(x, y)].set_style(style);
                    buf[(x, y)].set_symbol(" ");
                }
            }
            if let Some(&(start, _, ci, checked)) = in_region
                && idx == start
                && let Some(side) = pane.side
            {
                let mark = if checked { "[x]" } else { "[ ]" };
                buf.set_stringn(rect.x, y, mark, 3, style.add_modifier(Modifier::BOLD));
                mv.check_spans.push((y, rect.x..rect.x + 3, ci, side));
            }
            buf.set_stringn(
                rect.x + gutter,
                y,
                blank_tabs(&pane.lines[idx]),
                text_w,
                style,
            );
            idx += 1;
        }
    }

    Rect {
        x: inner.x,
        y: sep_y + 1,
        width: inner.width,
        height: inner.height - top_h - 1,
    }
}

/// `text` from char `skip` on, cut and space-padded to exactly `width`
/// screen cells for one side of the side-by-side diff. A double-width (CJK,
/// emoji) char takes two cells, and one that would straddle the edge is left
/// out, so the text never runs over the seam into the other column. A tab
/// takes its blank cell ([`blank_tabs`]).
pub(super) fn clip_pad_cells(text: &str, skip: usize, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    let from = byte_index_of_char(text, skip);
    for (c, w) in char_cells_from(&text[from..], cells_from(&text[..from], 0)) {
        if used + w > width {
            break;
        }
        match c {
            '\t' => out.extend(std::iter::repeat_n(' ', w)),
            _ => out.push(c),
        }
        used += w;
    }
    out.extend(std::iter::repeat_n(' ', width - used));
    out
}

/// Lifts the inks laid on one diff fill (a changed row's tint, or its
/// changed characters') so each reads at 4.5:1 on a `surfaces` theme
/// ([`crate::widgets::surface::legible_ink`]). It remembers the last ink
/// it lifted, so a run of cells in one colour costs one contrast check,
/// not one per cell. Without tones (a boxed theme, an unchanged row) it
/// hands every ink back untouched.
pub(super) struct DiffInkLift {
    tones: Option<crate::widgets::surface::Tones>,
    bg: (u8, u8, u8),
    last: Option<(Color, Color)>,
}

impl DiffInkLift {
    /// A lift onto fill `bg` with `legible`'s tones; `None`, or a fill
    /// that is not an RGB colour, lifts nothing.
    pub(super) fn new(legible: Option<crate::widgets::surface::Tones>, bg: Color) -> Self {
        let (tones, bg) = match (legible, bg) {
            (Some(t), Color::Rgb(r, g, b)) => (Some(t), (r, g, b)),
            _ => (None, (0, 0, 0)),
        };
        Self {
            tones,
            bg,
            last: None,
        }
    }

    /// `fg` as it must be drawn on the fill to read.
    pub(super) fn ink(&mut self, fg: Color) -> Color {
        let (Some(t), Color::Rgb(r, g, b)) = (self.tones, fg) else {
            return fg;
        };
        if let Some((from, to)) = self.last
            && from == fg
        {
            return to;
        }
        let to =
            crate::gradient::rgb_color(crate::widgets::surface::legible_ink((r, g, b), self.bg, t));
        self.last = Some((fg, to));
        to
    }
}

/// Paint the changed characters `range` (char indices) of a diff line
/// drawn at `text_x` from char `skip`, `width` cells wide, in `bg` (#1351).
/// Cells follow display width, so a wide character before the change does
/// not shift the mark (#1248). A range wholly past either edge puts a `›`
/// or `‹` on that edge in `bg` instead, so a change off-screen still shows.
/// With `legible` (a `surfaces` theme) an ink that would not read on the
/// brighter `bg`, the code's or the edge flag's, is lifted until it does.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_intraline_range(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    width: u16,
    text: &str,
    skip: usize,
    (start, end): (usize, usize),
    bg: Color,
    fg: Color,
    legible: Option<crate::widgets::surface::Tones>,
) {
    if start >= end || width == 0 {
        return;
    }
    let mut lift = DiffInkLift::new(legible, bg);
    let edge = Style::default()
        .fg(lift.ink(fg))
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    if end <= skip {
        buf.set_string(text_x, y, "\u{2039}", edge);
        return;
    }
    use unicode_segmentation::UnicodeSegmentation;
    let mut cell = 0u16;
    let mut painted = false;
    let from = byte_index_of_char(text, skip);
    let mut col = cells_from(&text[..from], 0);
    let mut i = skip;
    // Cluster by cluster, as the text is drawn: a change to any character of
    // one (`👍🏻` to `👍🏽` changes only the skin tone) marks the cells the
    // whole cluster takes.
    for cluster in text[from..].graphemes(true) {
        let w = cluster_cells_at(cluster, col);
        let n = cluster.chars().count();
        col += w;
        let w = w as u16;
        if cell + w > width {
            break;
        }
        if start < i + n && i < end {
            for dx in 0..w {
                let slot = &mut buf[(text_x + cell + dx, y)];
                let ink = lift.ink(slot.fg);
                slot.set_bg(bg).set_fg(ink);
            }
            painted = true;
        }
        cell += w;
        i += n;
    }
    if !painted {
        buf.set_string(text_x + width - 1, y, "\u{203a}", edge);
    }
}

/// Recolour one diff line drawn at `text_x` from char `skip`, `width` cells
/// wide, in its syntax colours (#2020): each character takes its span's
/// foreground and weight, and keeps the row's background, which is what
/// marks it removed, added or unchanged. Cells follow display width, as
/// [`clip_pad_cells`] lays them down. On a changed row of a `surfaces`
/// theme `lift` raises a colour that would not read on the row's tint.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_diff_syntax(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    width: u16,
    text: &str,
    skip: usize,
    spans: &[HiSpan],
    mut lift: DiffInkLift,
) {
    if spans.is_empty() || width == 0 {
        return;
    }
    let mut cell = 0u16;
    let mut next = 0;
    let from = byte_index_of_char(text, skip);
    let cells = char_cells_from(&text[from..], cells_from(&text[..from], 0));
    for ((byte, _), (_, w)) in text[from..].char_indices().zip(cells) {
        let byte = from + byte;
        // Spans arrive in order and do not overlap.
        while spans.get(next).is_some_and(|s| s.range().end <= byte) {
            next += 1;
        }
        let w = w as u16;
        if cell + w > width {
            break;
        }
        if let Some(span) = spans.get(next).filter(|s| s.range().contains(&byte)) {
            let style = Style {
                bg: None,
                fg: span.style.fg.map(|fg| lift.ink(fg)),
                ..span.style
            };
            for dx in 0..w {
                buf[(text_x + cell + dx, y)].set_style(style);
            }
        }
        cell += w;
    }
}

/// Blend shares of the red (removed) and the green (added) a `surfaces`
/// diff row is tinted with over the canvas, and the stronger shares its
/// changed characters take. On Croft Loch's black a row lands near 1.6:1
/// against the canvas, so a changed line is plain at a glance without
/// shouting, and its changed characters a further 1.6:1 against the row,
/// so a one-character edit is found without hunting. Croft's red and green
/// carry about the same luminance, so equal shares give the two sides the
/// same weight.
pub(super) const DIFF_ROW_TINT: (f32, f32) = (0.32, 0.32);

pub(super) const DIFF_CHANGE_TINT: (f32, f32) = (0.53, 0.53);

/// How far a `surfaces` diff tint's hue is greyed toward the body ink
/// before it is laid over the canvas: at the strength a row needs to be
/// seen, the git red and green straight would read as brick and emerald;
/// a fifth of grey keeps them plainly red and green but calm.
pub(super) const DIFF_TINT_GREY: f32 = 0.2;

/// A `surfaces` diff fill: `hue`, greyed by [`DIFF_TINT_GREY`], laid
/// `share` of the way over the canvas.
pub(super) fn diff_tint(t: crate::widgets::surface::Tones, hue: (u8, u8, u8), share: f32) -> Color {
    use crate::gradient::{lerp_rgb, rgb_color};
    let calm = lerp_rgb(hue, t.text, DIFF_TINT_GREY);
    rgb_color(lerp_rgb(t.canvas, calm, share))
}

/// The [`DiffInks`] for `theme`.
pub(super) fn diff_inks(theme: crate::theme::Theme) -> DiffInks {
    use crate::gradient::{lerp_rgb, rgb_color};
    let Some(t) = crate::widgets::surface::tones(theme) else {
        let removed_bg = theme.ui(Color::Rgb(0x4a, 0x1f, 0x1f));
        let removed_fg = theme.ui(Color::Rgb(0xff, 0xb3, 0xb3));
        let added_bg = theme.ui(Color::Rgb(0x1f, 0x42, 0x2a));
        let added_fg = theme.ui(Color::Rgb(0xb6, 0xee, 0xc4));
        let (rr, rg, rb) = crate::widgets::diff::INTRALINE_REMOVED_BG;
        let (ar, ag, ab) = crate::widgets::diff::INTRALINE_ADDED_BG;
        return DiffInks {
            removed_bg,
            added_bg,
            removed_fg,
            added_fg,
            equal_fg: theme.ui(Color::Rgb(0xc5, 0xcd, 0xd9)),
            removed_sign: removed_fg,
            added_sign: added_fg,
            sign_modifier: Modifier::BOLD,
            gutter_fg: theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c)),
            changed_gutter_fg: theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c)),
            intraline_removed: theme.ui(Color::Rgb(rr, rg, rb)),
            intraline_added: theme.ui(Color::Rgb(ar, ag, ab)),
            filler_removed: removed_bg,
            filler_added: added_bg,
            hatch: None,
            seam: ("\u{2502}", theme.ui(Color::Rgb(0x3a, 0x42, 0x52))),
            legible: None,
        };
    };
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => t.text,
    };
    let (red, green) = (rgb(theme.git_deleted()), rgb(theme.git_added()));
    DiffInks {
        removed_bg: diff_tint(t, red, DIFF_ROW_TINT.0),
        added_bg: diff_tint(t, green, DIFF_ROW_TINT.1),
        removed_fg: rgb_color(lerp_rgb(t.text, red, 0.3)),
        added_fg: rgb_color(lerp_rgb(t.text, green, 0.3)),
        equal_fg: t.text(),
        removed_sign: rgb_color(red),
        added_sign: rgb_color(green),
        sign_modifier: Modifier::empty(),
        gutter_fg: t.faint(),
        changed_gutter_fg: t.muted(),
        intraline_removed: diff_tint(t, red, DIFF_CHANGE_TINT.0),
        intraline_added: diff_tint(t, green, DIFF_CHANGE_TINT.1),
        filler_removed: t.canvas(),
        filler_added: t.canvas(),
        hatch: Some(("\u{2571}", rgb_color(lerp_rgb(t.canvas, t.faint, 0.34)))),
        seam: ("\u{258f}", t.hairline()),
        legible: Some(t),
    }
}

/// Hatch the blank side of an inserted or deleted line, cells `x0..x1` of
/// row `y`, when the theme hatches filler (VS Code's diagonal fill).
pub(super) fn paint_diff_filler(buf: &mut Buffer, x0: u16, x1: u16, y: u16, inks: &DiffInks) {
    let Some((glyph, fg)) = inks.hatch else {
        return;
    };
    for x in x0..x1 {
        buf[(x, y)].set_symbol(glyph).set_fg(fg);
    }
}

/// A file label in a diff header split into the file's name and the
/// qualifier after it (`main.rs (HEAD)` is `main.rs` and ` (HEAD)`), which
/// a `surfaces` header mutes.
pub(super) fn split_diff_label(label: &str) -> (&str, &str) {
    match label.rfind(" (") {
        Some(i) if label.ends_with(')') && i > 0 => label.split_at(i),
        _ => (label, ""),
    }
}

/// The notes a diff header adds after the file names: the lenses it is
/// seen through and what it could not do, each of which a reader must be
/// told about (a hidden change must never read as an absent one).
pub(super) fn diff_header_notes(diff: &crate::widgets::diff::DiffData) -> Vec<String> {
    let mut notes = Vec::new();
    if diff.bytes_differ_but_lines_equal {
        notes.push(String::from(
            "whitespace-only change (trailing newline / CRLF / BOM) \u{2014} no line-level diff",
        ));
    }
    if diff.ws_mode != crate::widgets::diff::DiffWhitespace::Off {
        notes.push(format!("ignoring whitespace: {}", diff.ws_mode.label()));
    }
    if diff.group_by_seat {
        notes.push(format!("by seat: {}", diff.seat_summary()));
    }
    if diff.coarse {
        notes.push(String::from("large change: diff stopped early"));
    }
    notes
}

/// The diff header on a `surfaces` theme: the chrome tone instead of the
/// teal band, and the type ramp instead of one bold white line. Each side's
/// file name sits over its own column's code (the current side's in the
/// strong ink, the other in body text), a qualifier like `(HEAD)` muted,
/// `↔` faint on the seam between them, and the notes muted after; on a pane
/// too narrow for two columns, or a diff with one side, they run on from
/// the left. The text stops short of the `‹ ›` arrows, which are muted at
/// rest and strong, on a faint chip, under the pointer. Returns the arrows'
/// hit rects, the same cells the boxed header gives them.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_surface_diff_header(
    buf: &mut Buffer,
    inner: Rect,
    t: crate::widgets::surface::Tones,
    left: &str,
    right: Option<&str>,
    notes: &[String],
    columns: Option<(u16, u16, u16)>,
    pointer: Option<(u16, u16)>,
) -> (Rect, Rect) {
    let y = inner.y;
    let chrome = t.chrome();
    buf.set_style(
        Rect { height: 1, ..inner },
        Style::default().bg(chrome).fg(t.text()),
    );
    for x in inner.x..inner.right() {
        buf[(x, y)].set_symbol(" ");
    }
    let (prev, next) = paint_diff_nav_arrows(inner, chrome, buf);
    // The header's text stops a cell before the arrows' strip.
    let text_end = if prev.width > 0 {
        prev.x.saturating_sub(2)
    } else {
        inner.right()
    };
    let ink = |fg: Color| Style::default().fg(fg).bg(chrome);
    let label = |name: &str, strong: bool| {
        let (file, qualifier) = split_diff_label(name);
        vec![
            Span::styled(
                file.to_string(),
                ink(if strong { t.strong() } else { t.text() }),
            ),
            Span::styled(qualifier.to_string(), ink(t.muted())),
        ]
    };
    let mut tail: Vec<Span> = Vec::new();
    for note in notes {
        tail.push(Span::styled("  \u{b7}  ", ink(t.faint())));
        tail.push(Span::styled(note.clone(), ink(t.muted())));
    }
    let put = |buf: &mut Buffer, x: u16, spans: Vec<Span>| {
        if x < text_end {
            buf.set_line(x, y, &Line::from(spans), text_end - x);
        }
    };
    match (right, columns) {
        (Some(right), Some((l_text_x, seam_x, r_text_x))) => {
            put(buf, l_text_x, label(left, false));
            put(buf, seam_x, vec![Span::styled("\u{2194}", ink(t.faint()))]);
            let mut spans = label(right, true);
            spans.extend(tail);
            put(buf, r_text_x, spans);
        }
        (Some(right), None) => {
            let mut spans = label(left, false);
            spans.push(Span::styled("  \u{2194}  ", ink(t.faint())));
            spans.extend(label(right, true));
            spans.extend(tail);
            put(buf, inner.x + 1, spans);
        }
        (None, _) => {
            let mut spans = label(left, true);
            spans.extend(tail);
            put(buf, inner.x + 1, spans);
        }
    }
    for arrow in [prev, next] {
        if arrow.width == 0 {
            continue;
        }
        let hovered = pointer == Some((arrow.x, arrow.y));
        let style = if hovered {
            Style::default().fg(t.strong()).bg(t.hover_on(t.chrome))
        } else {
            Style::default().fg(t.muted()).bg(chrome)
        };
        let cell = &mut buf[(arrow.x, arrow.y)];
        cell.set_style(style);
        cell.modifier.remove(Modifier::BOLD);
    }
    (prev, next)
}

/// The diff footer on a `surfaces` theme: muted text on the chrome tone,
/// not a raised grey band, with key names as keycaps. `position` leads;
/// each `(keys, words)` hint follows while the row has room for all of it,
/// so a narrow pane drops whole hints from the end instead of cutting one.
pub(super) fn paint_surface_diff_footer(
    buf: &mut Buffer,
    row: Rect,
    t: crate::widgets::surface::Tones,
    position: &str,
    hints: &[(&str, &str)],
) {
    let chrome = t.chrome();
    buf.set_style(row, Style::default().bg(chrome));
    for x in row.left()..row.right() {
        buf[(x, row.y)].set_symbol(" ");
    }
    let muted = Style::default().fg(t.muted()).bg(chrome);
    let mut spans = vec![Span::styled(format!(" {position}"), muted)];
    let mut used: usize = spans.iter().map(|s| s.width()).sum();
    for (keys, words) in hints {
        let mut hint = vec![Span::styled("   ", muted)];
        if !keys.is_empty() {
            hint.extend(crate::widgets::surface::keycap_spans(
                keys, t, chrome, false,
            ));
        }
        if !words.is_empty() {
            hint.push(Span::styled(format!(" {words}"), muted));
        }
        let w: usize = hint.iter().map(|s| s.width()).sum();
        if used + w > usize::from(row.width) {
            break;
        }
        used += w;
        spans.extend(hint);
    }
    buf.set_line(row.x, row.y, &Line::from(spans), row.width);
}

/// Returns the hit-test rects of the prev / next change arrows painted
/// in the diff header (in that order). Both are `Rect::default()` when
/// the header was too narrow to allocate them. `pointer` is the screen
/// cell under the mouse, which lifts an arrow under it on a `surfaces`
/// theme.
pub(super) fn render_diff(
    diff: &mut crate::widgets::diff::DiffData,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
    pointer: Option<(u16, u16)>,
) -> (Rect, Rect) {
    use crate::widgets::diff::DiffRow;
    // Background fill so the diff sits on a clean canvas (see [`canvas_bg`]).
    let bg_style = Style::default().bg(bg);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            buf[(x, y)].set_style(bg_style);
            buf[(x, y)].set_symbol(" ");
        }
    }
    if inner.height < 3 || inner.width < 16 {
        return (Rect::default(), Rect::default());
    }
    // A small diff colours in this frame; a large one paints plain now and
    // takes its colours when `poll_highlights` lands them.
    diff.colour_syntax(Some(HIGHLIGHT_INLINE_BUDGET));
    if diff.unified {
        return render_unified_deletion(diff, inner, buf, bg, theme, pointer);
    }
    let left_name = diff
        .left_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| diff.left_path.display().to_string());
    let right_is_real = diff.right_path != std::path::Path::new("/dev/null")
        && !diff.right_path.as_os_str().is_empty();
    let right_name = if right_is_real {
        Some(
            diff.right_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| diff.right_path.display().to_string()),
        )
    } else {
        None
    };
    // Two columns split exactly down the middle. Each column has a small
    // line-number gutter on its left and a 1-cell sign column showing
    // -, +, or space.
    let half = inner.width / 2;
    let left_max = diff.left_lines.len();
    let right_max = diff.right_lines.len();
    let l_gutter = (left_max + 1).to_string().len() as u16 + 1;
    let r_gutter = (right_max + 1).to_string().len() as u16 + 1;
    // Per-column layout: gutter + sign + content.
    let l_x = inner.x;
    let l_sign_x = l_x + l_gutter;
    let l_text_x = l_sign_x + 2;
    let l_text_w = half.saturating_sub(l_gutter + 2 + 1); // -1 spacer
    let r_x = inner.x + half + 1;
    let r_sign_x = r_x + r_gutter;
    let r_text_x = r_sign_x + 2;
    let r_text_w = (inner.width - (half + 1)).saturating_sub(r_gutter + 2);
    let seam_x = inner.x + half;
    let inks = diff_inks(theme);
    let tones = crate::widgets::surface::tones(theme);

    let (prev_arrow, next_arrow) = if let Some(t) = tones {
        let columns = (half >= 8).then_some((l_text_x, seam_x, r_text_x));
        paint_surface_diff_header(
            buf,
            inner,
            t,
            &left_name,
            right_name.as_deref(),
            &diff_header_notes(diff),
            columns,
            pointer,
        )
    } else {
        let header = match (&right_name, diff.bytes_differ_but_lines_equal) {
            (Some(r), true) => format!(
                " diff: {left_name}  \u{2194}  {r}   \u{2022} whitespace-only change (trailing newline / CRLF / BOM) — no line-level diff "
            ),
            (Some(r), false) => format!(" diff: {left_name}  \u{2194}  {r} "),
            // Synthetic git-diff text view: no real right-side path, so the
            // header reads as the diff command instead of trailing "↔ null".
            (None, _) => format!(" {left_name} "),
        };
        // Ignore-whitespace is a lens over the real diff, so say so in the header:
        // a reader must never mistake a hidden change for an absent one.
        let header = match diff.ws_mode {
            crate::widgets::diff::DiffWhitespace::Off => header,
            mode => format!("{header}\u{2022} ignoring whitespace: {} ", mode.label()),
        };
        // Same reason as the whitespace lens: a reader seeing coloured bars must
        // be told what they mean and who is in them (#349).
        let header = if diff.group_by_seat {
            format!("{header}\u{2022} by seat: {} ", diff.seat_summary())
        } else {
            header
        };
        let header = if diff.coarse {
            format!("{header}\u{2022} large change: diff stopped early ")
        } else {
            header
        };
        let head_bg = if diff.bytes_differ_but_lines_equal {
            theme.ui(Color::Rgb(0x8a, 0x4a, 0x10))
        } else {
            theme.ui(Color::Rgb(0x09, 0x4d, 0x77))
        };
        let head_style = Style::default()
            .fg(Color::White)
            .bg(head_bg)
            .add_modifier(Modifier::BOLD);
        for x in inner.x..inner.x + inner.width {
            buf[(x, inner.y)].set_style(head_style);
            buf[(x, inner.y)].set_symbol(" ");
        }
        buf.set_string(inner.x, inner.y, &header, head_style);
        paint_diff_nav_arrows(inner, head_bg, buf)
    };

    let body_top = inner.y + 1;
    let body_height = inner.height.saturating_sub(2);
    let status_y = inner.y + inner.height - 1;
    if body_height == 0 {
        return (prev_arrow, next_arrow);
    }
    if half < 8 {
        return (prev_arrow, next_arrow);
    }

    // Vertical seam between the two columns.
    for y in body_top..body_top + body_height {
        buf[(seam_x, y)].set_symbol(inks.seam.0);
        buf[(seam_x, y)].set_style(Style::default().fg(inks.seam.1));
    }

    let total = diff.rows.len();
    let viewport = body_height as usize;
    let max_scroll = total.saturating_sub(viewport);
    if diff.scroll > max_scroll {
        diff.scroll = max_scroll;
    }

    let DiffInks {
        removed_bg,
        added_bg,
        removed_fg,
        added_fg,
        equal_fg,
        gutter_fg,
        ..
    } = inks;

    let find_needle = diff.find.needle.clone();
    let find_opts = diff.find.opts;
    let find_active = diff.find.active;
    // VS Code-style selection highlight: when a single word/run is selected,
    // every OTHER occurrence on screen gets a muted-blue box, mirroring the
    // plain editor. Painted per row below, under the brighter active band.
    let occ_needle = diff.selection_occurrence_needle();

    let end = (diff.scroll + viewport).min(total);
    for (vis_row, row_idx) in (diff.scroll..end).enumerate() {
        let y = body_top + vis_row as u16;
        // Paint what the ignore-whitespace toggle says; staging reads the real
        // rows independently (see DiffData::display_rows).
        let row = diff.display_rows()[row_idx];
        let (l_cell_bg, l_sign, l_text) = match row {
            DiffRow::Equal { left, .. } => (
                bg,
                ' ',
                diff.left_lines.get(left).cloned().unwrap_or_default(),
            ),
            DiffRow::Removed { left } => (
                removed_bg,
                '-',
                diff.left_lines.get(left).cloned().unwrap_or_default(),
            ),
            DiffRow::Replaced { left, .. } => (
                removed_bg,
                '-',
                diff.left_lines.get(left).cloned().unwrap_or_default(),
            ),
            DiffRow::Added { .. } => (inks.filler_added, ' ', String::new()),
        };
        let (r_cell_bg, r_sign, r_text) = match row {
            DiffRow::Equal { right, .. } => (
                bg,
                ' ',
                diff.right_lines.get(right).cloned().unwrap_or_default(),
            ),
            DiffRow::Added { right } => (
                added_bg,
                '+',
                diff.right_lines.get(right).cloned().unwrap_or_default(),
            ),
            DiffRow::Replaced { right, .. } => (
                added_bg,
                '+',
                diff.right_lines.get(right).cloned().unwrap_or_default(),
            ),
            DiffRow::Removed { .. } => (inks.filler_removed, ' ', String::new()),
        };

        // Left column.
        let l_left_idx = match row {
            DiffRow::Equal { left, .. }
            | DiffRow::Removed { left }
            | DiffRow::Replaced { left, .. } => Some(left),
            DiffRow::Added { .. } => None,
        };
        let l_changed = l_cell_bg == removed_bg;
        let l_label = l_left_idx
            .map(|i| format!("{:>width$} ", i + 1, width = l_gutter as usize - 1))
            .unwrap_or_else(|| " ".repeat(l_gutter as usize));
        buf.set_string(
            l_x,
            y,
            &l_label,
            Style::default()
                .fg(if l_changed {
                    inks.changed_gutter_fg
                } else {
                    gutter_fg
                })
                .bg(l_cell_bg),
        );
        buf.set_string(
            l_sign_x,
            y,
            format!("{l_sign} "),
            Style::default()
                .fg(if l_cell_bg == removed_bg {
                    inks.removed_sign
                } else {
                    equal_fg
                })
                .bg(l_cell_bg)
                .add_modifier(inks.sign_modifier),
        );
        let l_padded = clip_pad_cells(&l_text, diff.scroll_x, l_text_w as usize);
        buf.set_string(
            l_text_x,
            y,
            &l_padded,
            Style::default()
                .fg(if l_cell_bg == removed_bg {
                    removed_fg
                } else {
                    equal_fg
                })
                .bg(l_cell_bg),
        );
        if let Some(i) = l_left_idx {
            paint_diff_syntax(
                buf,
                l_text_x,
                y,
                l_text_w,
                &l_text,
                diff.scroll_x,
                diff.syntax.left(i),
                DiffInkLift::new(inks.legible.filter(|_| l_changed), l_cell_bg),
            );
        }
        // The changed characters of a replaced line (#1351), on both sides.
        let intraline = match row {
            DiffRow::Replaced { .. } => crate::widgets::diff::changed_char_ranges(&l_text, &r_text),
            _ => None,
        };
        if let Some((range, _)) = intraline {
            paint_intraline_range(
                buf,
                l_text_x,
                y,
                l_text_w,
                &l_text,
                diff.scroll_x,
                range,
                inks.intraline_removed,
                inks.removed_sign,
                inks.legible,
            );
        }
        if l_left_idx.is_none() {
            paint_diff_filler(buf, l_x, seam_x, y, &inks);
        } else if tones.is_some() {
            // The spacer before the seam takes the row's tint, so a changed
            // row's band runs right up to the hairline.
            buf[(seam_x - 1, y)].set_bg(l_cell_bg);
        }
        if tones.is_some() {
            // The seam's glyph hugs its cell's left edge; the rest of the
            // cell belongs to the right column's row.
            buf[(seam_x, y)].set_bg(r_cell_bg);
        }

        // Right column.
        let r_right_idx = match row {
            DiffRow::Equal { right, .. }
            | DiffRow::Added { right }
            | DiffRow::Replaced { right, .. } => Some(right),
            DiffRow::Removed { .. } => None,
        };
        let r_changed = r_cell_bg == added_bg;
        let r_label = r_right_idx
            .map(|i| format!("{:>width$} ", i + 1, width = r_gutter as usize - 1))
            .unwrap_or_else(|| " ".repeat(r_gutter as usize));
        buf.set_string(
            r_x,
            y,
            &r_label,
            Style::default()
                .fg(if r_changed {
                    inks.changed_gutter_fg
                } else {
                    gutter_fg
                })
                .bg(r_cell_bg),
        );
        // The group-by-seat lens (#349): a bar in this row's own hue, in the
        // gutter's last cell, on the lines this change ADDS. An unchanged
        // line's seat describes work the change did not do, and a line with
        // no record paints nothing rather than being guessed.
        // The REAL row, the basis `added_lines_by_seat` counts on: the
        // whitespace lens reclassifies a re-indented line as Equal, and a
        // line this change wrote must not lose its bar while the header
        // still counts it. `display_rows` is index-for-index with `rows`.
        let seat_row = diff.rows.get(row_idx).copied().unwrap_or(row);
        if diff.group_by_seat
            && matches!(seat_row, DiffRow::Added { .. } | DiffRow::Replaced { .. })
            && let Some(i) = r_right_idx
            && let Some(seat) = diff.seats.seat(i)
        {
            let (sr, sg, sb) = seat.hue();
            buf.set_string(
                r_x + r_gutter - 1,
                y,
                "\u{258e}",
                Style::default()
                    .fg(theme.ui(Color::Rgb(sr, sg, sb)))
                    .bg(r_cell_bg),
            );
        }
        buf.set_string(
            r_sign_x,
            y,
            format!("{r_sign} "),
            Style::default()
                .fg(if r_cell_bg == added_bg {
                    inks.added_sign
                } else {
                    equal_fg
                })
                .bg(r_cell_bg)
                .add_modifier(inks.sign_modifier),
        );
        let r_padded = clip_pad_cells(&r_text, diff.scroll_x, r_text_w as usize);
        buf.set_string(
            r_text_x,
            y,
            &r_padded,
            Style::default()
                .fg(if r_cell_bg == added_bg {
                    added_fg
                } else {
                    equal_fg
                })
                .bg(r_cell_bg),
        );
        if let Some(i) = r_right_idx {
            paint_diff_syntax(
                buf,
                r_text_x,
                y,
                r_text_w,
                &r_text,
                diff.scroll_x,
                diff.syntax.right(i),
                DiffInkLift::new(inks.legible.filter(|_| r_changed), r_cell_bg),
            );
        }
        if let Some((_, range)) = intraline {
            paint_intraline_range(
                buf,
                r_text_x,
                y,
                r_text_w,
                &r_text,
                diff.scroll_x,
                range,
                inks.intraline_added,
                inks.added_sign,
                inks.legible,
            );
        }
        if r_right_idx.is_none() {
            paint_diff_filler(buf, r_x, inner.right(), y, &inks);
        }

        // Inline-find highlight: overpaint every occurrence of the needle
        // on top of the text just laid down, lighting the active match in
        // a stronger colour. Reuses the plain editor's painter so the diff
        // and text views look identical under Cmd+F.
        if let Some(needle) = find_needle.as_deref() {
            use crate::widgets::diff::DiffSide;
            if l_left_idx.is_some() && l_text_w > 0 {
                let active = find_active
                    .filter(|a| a.row == row_idx && a.side == DiffSide::Left)
                    .map(|a| (a.col_chars, a.len_chars));
                paint_search_highlight(
                    buf,
                    l_text_x,
                    y,
                    l_text_w,
                    &l_text,
                    needle,
                    find_opts,
                    &RowCells::of_line(&l_text, diff.scroll_x),
                    active,
                    None,
                    None,
                    theme,
                );
            }
            if r_right_idx.is_some() && r_text_w > 0 {
                let active = find_active
                    .filter(|a| a.row == row_idx && a.side == DiffSide::Right)
                    .map(|a| (a.col_chars, a.len_chars));
                paint_search_highlight(
                    buf,
                    r_text_x,
                    y,
                    r_text_w,
                    &r_text,
                    needle,
                    find_opts,
                    &RowCells::of_line(&r_text, diff.scroll_x),
                    active,
                    None,
                    None,
                    theme,
                );
            }
        }

        // Selection-occurrence highlight: muted-blue box over every match of
        // the selected word on both columns. Painted after the find pass so
        // the two layers compose like the plain editor; the active selection
        // band overpaints these in a brighter blue below the row loop.
        if let Some(needle) = occ_needle.as_deref() {
            if l_left_idx.is_some() && l_text_w > 0 {
                paint_selection_occurrences(
                    buf,
                    l_text_x,
                    y,
                    l_text_w,
                    &l_text,
                    needle,
                    &RowCells::of_line(&l_text, diff.scroll_x),
                    theme,
                );
            }
            if r_right_idx.is_some() && r_text_w > 0 {
                paint_selection_occurrences(
                    buf,
                    r_text_x,
                    y,
                    r_text_w,
                    &r_text,
                    needle,
                    &RowCells::of_line(&r_text, diff.scroll_x),
                    theme,
                );
            }
        }
    }

    paint_diff_selection_band(
        diff,
        body_top,
        body_height,
        l_text_x,
        l_text_w,
        r_text_x,
        r_text_w,
        end,
        buf,
        theme,
    );

    // Status footer.
    let visible_first = diff.scroll + 1;
    let visible_last = end;
    if let Some(t) = tones {
        paint_surface_diff_footer(
            buf,
            Rect {
                y: status_y,
                height: 1,
                ..inner
            },
            t,
            &format!("{visible_first}\u{2013}{visible_last} of {total}"),
            &[
                ("F7", "next change"),
                ("Shift+F7", "previous"),
                ("PgUp PgDn", "scroll"),
            ],
        );
        return (prev_arrow, next_arrow);
    }
    let status = format!(
        " {visible_first}–{visible_last} of {total}  ·  ↑/↓ PgUp/PgDn  ·  ‹/› click arrows or F7 for next/prev change "
    );
    buf.set_string(
        inner.x,
        status_y,
        &status,
        Style::default()
            .fg(theme.ui(Color::Gray))
            .bg(theme.ui(Color::Rgb(0x14, 0x18, 0x22))),
    );
    (prev_arrow, next_arrow)
}

/// Overlay the diff's drag-select highlight on top of whatever the row
/// loop just painted. Walks the visible window once and paints the same
/// `paint_selection_band` overlay the regular text editor uses, so the
/// user sees an identical blue band over selected cells on whichever
/// column they're dragging in.
// Render helper: each argument is an independent painting input (data,
// geometry, target buffer); bundling them into a struct would add indirection
// without improving clarity.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_diff_selection_band(
    diff: &crate::widgets::diff::DiffData,
    body_top: u16,
    body_height: u16,
    l_text_x: u16,
    l_text_w: u16,
    r_text_x: u16,
    r_text_w: u16,
    end: usize,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
) {
    use crate::widgets::diff::DiffSide;
    let Some(sel) = diff.selection else {
        return;
    };
    if !sel.has_area() {
        return;
    }
    let (start, stop) = sel.normalized();
    let (text_x, text_w) = match sel.side {
        DiffSide::Left => (l_text_x, l_text_w),
        DiffSide::Right => (r_text_x, r_text_w),
    };
    if text_w == 0 {
        return;
    }
    let first_visible = diff.scroll;
    let last_visible = end;
    let row_start = start.0.max(first_visible);
    let row_end = stop.0.min(last_visible.saturating_sub(1));
    if row_end < row_start {
        return;
    }
    for row_idx in row_start..=row_end {
        let y = body_top + (row_idx - first_visible) as u16;
        if y >= body_top + body_height {
            break;
        }
        let (cs, ce) = if start.0 == stop.0 {
            (start.1, stop.1)
        } else if row_idx == start.0 {
            (start.1, usize::MAX)
        } else if row_idx == stop.0 {
            (0, stop.1)
        } else {
            (0, usize::MAX)
        };
        // The band is in cells: a tab inside the selection is Tab Size wide.
        let line = diff_side_line(diff, row_idx, sel.side);
        let to_cell = |col: usize| {
            if col == usize::MAX {
                return usize::MAX;
            }
            let from = byte_index_of_char(line, diff.scroll_x);
            let to = byte_index_of_char(line, col.max(diff.scroll_x));
            let drawn = cells_from(&line[from..to], cells_from(&line[..from], 0));
            drawn + col.saturating_sub(line.chars().count().max(diff.scroll_x))
        };
        let cs_screen = to_cell(cs);
        let ce_screen = to_cell(ce);
        paint_selection_band(buf, text_x, y, text_w, cs_screen, ce_screen, theme);
    }
}

/// Hit-test a click against the side-by-side diff body. Returns
/// `Some((side, diff_row_idx, char_col))` when the click landed inside
/// either column's text area; `None` when the click was in the header,
/// status footer, seam, gutter outside the body, or when the diff isn't
/// renderable at this size. The math mirrors `render_diff` exactly so a
/// click on a character returns the char column the user can see at that
/// cell.
pub fn diff_hit_test(
    diff: &crate::widgets::diff::DiffData,
    last_inner: Rect,
    col: u16,
    row: u16,
) -> Option<(crate::widgets::diff::DiffSide, usize, usize)> {
    use crate::widgets::diff::DiffSide;
    if diff.unified {
        return None;
    }
    if last_inner.width < 16 || last_inner.height < 3 {
        return None;
    }
    let body_top = last_inner.y + 1;
    let body_height = last_inner.height.saturating_sub(2);
    if body_height == 0 {
        return None;
    }
    if row < body_top || row >= body_top + body_height {
        return None;
    }
    let half = last_inner.width / 2;
    if half < 8 {
        return None;
    }
    let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
    let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
    let l_x = last_inner.x;
    let l_sign_x = l_x + l_gutter;
    let l_text_x = l_sign_x + 2;
    let l_text_w = half.saturating_sub(l_gutter + 2 + 1);
    let r_x = last_inner.x + half + 1;
    let r_sign_x = r_x + r_gutter;
    let r_text_x = r_sign_x + 2;
    let r_text_w = (last_inner.width - (half + 1)).saturating_sub(r_gutter + 2);
    let seam_x = last_inner.x + half;

    let vis_row = (row - body_top) as usize;
    let row_idx = diff.scroll + vis_row;
    if row_idx >= diff.rows.len() {
        return None;
    }

    let (side, text_x, text_w) = if col < seam_x {
        (DiffSide::Left, l_text_x, l_text_w)
    } else if col > seam_x {
        (DiffSide::Right, r_text_x, r_text_w)
    } else {
        return None;
    };
    let screen_col = if col >= text_x {
        (col - text_x).min(text_w.saturating_sub(1)) as usize
    } else {
        0
    };
    // The text is drawn with each tab as its Tab Size cells, so a cell maps to
    // the char drawn on it, not to `cell + scroll_x`.
    let line = diff_side_line(diff, row_idx, side);
    let char_col = char_at_cell_from(line, diff.scroll_x, screen_col);
    Some((side, row_idx, char_col))
}

/// The text a diff row draws in `side`'s column, `""` when that side has no
/// line on the row (an added or removed line's other half).
pub(super) fn diff_side_line(
    diff: &crate::widgets::diff::DiffData,
    row_idx: usize,
    side: crate::widgets::diff::DiffSide,
) -> &str {
    use crate::widgets::diff::{DiffRow, DiffSide};
    let (left, right) = match diff.rows.get(row_idx) {
        Some(DiffRow::Equal { left, right } | DiffRow::Replaced { left, right }) => {
            (Some(*left), Some(*right))
        }
        Some(DiffRow::Removed { left }) => (Some(*left), None),
        Some(DiffRow::Added { right }) => (None, Some(*right)),
        None => (None, None),
    };
    let (lines, idx) = match side {
        DiffSide::Left => (&diff.left_lines, left),
        DiffSide::Right => (&diff.right_lines, right),
    };
    idx.and_then(|i| lines.get(i)).map_or("", String::as_str)
}

/// The char index of `text` drawn on screen cell `cell` when the row starts
/// at char `skip` (cells as [`char_cells_from`] counts them). A cell past the
/// end of the text counts on from its length, one char per cell, as before
/// tabs had width.
pub(super) fn char_at_cell_from(text: &str, skip: usize, cell: usize) -> usize {
    let mut at = 0;
    let from = byte_index_of_char(text, skip);
    // Chars up to the row's first: `skip`, or the whole line when it is
    // scrolled past its end.
    let mut i = text[..from].chars().count();
    for (_, w) in char_cells_from(&text[from..], cells_from(&text[..from], 0)) {
        if cell < at + w {
            return i;
        }
        at += w;
        i += 1;
    }
    i.max(skip) + cell.saturating_sub(at)
}

/// Paint `‹` and `›` glyphs at the right edge of a diff header so the
/// user can click between change hunks without scrolling. Returns the
/// hit-test rects (prev, next) so the click handler can route mouse
/// events back to `prev_change_row` / `next_change_row`. Returns two
/// empty rects when the header doesn't have room for the glyphs (very
/// narrow editor pane).
pub(super) fn paint_diff_nav_arrows(inner: Rect, head_bg: Color, buf: &mut Buffer) -> (Rect, Rect) {
    // Reserve a 7-cell strip on the right: " ‹  ›  ".
    let strip_w: u16 = 7;
    if inner.width < strip_w + 4 {
        return (Rect::default(), Rect::default());
    }
    let strip_right = inner.x + inner.width;
    let prev_x = strip_right - strip_w + 1;
    let next_x = strip_right - 3;
    let y = inner.y;
    let arrow_style = Style::default()
        .fg(Color::White)
        .bg(head_bg)
        .add_modifier(Modifier::BOLD);
    buf.set_string(prev_x, y, "\u{2039}", arrow_style);
    buf.set_string(next_x, y, "\u{203a}", arrow_style);
    (
        Rect {
            x: prev_x,
            y,
            width: 1,
            height: 1,
        },
        Rect {
            x: next_x,
            y,
            width: 1,
            height: 1,
        },
    )
}

/// One-column unified diff for a deleted file. Every visible row is a
/// `Removed` row painted with a red band, `-` sign, and the HEAD line
/// number in the gutter — visually identical to `git diff` for a
/// removed file, but rendered inside the editor pane. Returns the
/// header-arrow hit rects the same way `render_diff` does so the click
/// handler can navigate deletion-view changes too.
pub(super) fn render_unified_deletion(
    diff: &mut crate::widgets::diff::DiffData,
    inner: Rect,
    buf: &mut Buffer,
    bg: Color,
    theme: crate::theme::Theme,
    pointer: Option<(u16, u16)>,
) -> (Rect, Rect) {
    use crate::widgets::diff::DiffRow;
    let name = diff
        .left_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| diff.left_path.display().to_string());
    let inks = diff_inks(theme);
    let tones = crate::widgets::surface::tones(theme);
    let (prev_arrow, next_arrow) = if let Some(t) = tones {
        let note = [String::from("deleted (HEAD \u{2192} /dev/null)")];
        paint_surface_diff_header(buf, inner, t, &name, None, &note, None, pointer)
    } else {
        let header = format!(" diff: {name}  \u{2022} deleted (HEAD \u{2192} /dev/null) ");
        let head_style = Style::default()
            .fg(Color::White)
            .bg(theme.ui(Color::Rgb(0x6b, 0x1f, 0x1f)))
            .add_modifier(Modifier::BOLD);
        for x in inner.x..inner.x + inner.width {
            buf[(x, inner.y)].set_style(head_style);
            buf[(x, inner.y)].set_symbol(" ");
        }
        buf.set_string(inner.x, inner.y, &header, head_style);
        paint_diff_nav_arrows(inner, theme.ui(Color::Rgb(0x6b, 0x1f, 0x1f)), buf)
    };

    let body_top = inner.y + 1;
    let body_height = inner.height.saturating_sub(2);
    let status_y = inner.y + inner.height - 1;
    if body_height == 0 {
        return (prev_arrow, next_arrow);
    }
    let left_max = diff.left_lines.len();
    let gutter = (left_max + 1).to_string().len() as u16 + 1;
    let sign_x = inner.x + gutter;
    let text_x = sign_x + 2;
    let text_w = inner.width.saturating_sub(gutter + 2);

    let total = diff.rows.len();
    let viewport = body_height as usize;
    let max_scroll = total.saturating_sub(viewport);
    if diff.scroll > max_scroll {
        diff.scroll = max_scroll;
    }

    let DiffInks {
        removed_bg,
        removed_fg,
        gutter_fg,
        ..
    } = inks;

    let end = (diff.scroll + viewport).min(total);
    for (vis_row, row_idx) in (diff.scroll..end).enumerate() {
        let y = body_top + vis_row as u16;
        // Paint what the ignore-whitespace toggle says; staging reads the real
        // rows independently (see DiffData::display_rows).
        let row = diff.display_rows()[row_idx];
        let (left_idx, sign, cell_bg) = match row {
            DiffRow::Removed { left } | DiffRow::Replaced { left, .. } => {
                (Some(left), '-', removed_bg)
            }
            DiffRow::Equal { left, .. } => (Some(left), ' ', bg),
            DiffRow::Added { .. } => (None, ' ', bg),
        };
        let changed = cell_bg == removed_bg;
        let label = left_idx
            .map(|i| format!("{:>width$} ", i + 1, width = gutter as usize - 1))
            .unwrap_or_else(|| " ".repeat(gutter as usize));
        buf.set_string(
            inner.x,
            y,
            &label,
            Style::default()
                .fg(if changed {
                    inks.changed_gutter_fg
                } else {
                    gutter_fg
                })
                .bg(cell_bg),
        );
        buf.set_string(
            sign_x,
            y,
            format!("{sign} "),
            Style::default()
                .fg(if cell_bg == removed_bg {
                    inks.removed_sign
                } else {
                    gutter_fg
                })
                .bg(cell_bg)
                .add_modifier(inks.sign_modifier),
        );
        let text = left_idx
            .and_then(|i| diff.left_lines.get(i).cloned())
            .unwrap_or_default();
        let padded = clip_pad_cells(&text, diff.scroll_x, text_w as usize);
        buf.set_string(
            text_x,
            y,
            &padded,
            Style::default()
                .fg(if cell_bg == removed_bg {
                    removed_fg
                } else {
                    inks.equal_fg
                })
                .bg(cell_bg),
        );
        if let Some(i) = left_idx {
            paint_diff_syntax(
                buf,
                text_x,
                y,
                text_w,
                &text,
                diff.scroll_x,
                diff.syntax.left(i),
                DiffInkLift::new(inks.legible.filter(|_| changed), cell_bg),
            );
        }
    }

    let visible_first = diff.scroll + 1;
    let visible_last = end;
    if let Some(t) = tones {
        paint_surface_diff_footer(
            buf,
            Rect {
                y: status_y,
                height: 1,
                ..inner
            },
            t,
            &format!("{visible_first}\u{2013}{visible_last} of {total}"),
            &[("", "every line removed at HEAD"), ("PgUp PgDn", "scroll")],
        );
        return (prev_arrow, next_arrow);
    }
    let status = format!(
        " {visible_first}\u{2013}{visible_last} of {total}  \u{00b7}  every line removed at HEAD  \u{00b7}  \u{2191}/\u{2193} PgUp/PgDn to scroll "
    );
    buf.set_string(
        inner.x,
        status_y,
        &status,
        Style::default()
            .fg(theme.ui(Color::Gray))
            .bg(theme.ui(Color::Rgb(0x14, 0x18, 0x22))),
    );
    (prev_arrow, next_arrow)
}

impl Editor {
    /// The buffer's merge-conflict blocks, rescanned only when the buffer
    /// changed since the last call — cheap enough for the render loop and
    /// every cursor-position query.
    pub fn conflicts(&mut self) -> &[crate::merge::ConflictBlock] {
        if self.conflicts_seq != self.edit_seq {
            self.conflicts_seq = self.edit_seq;
            self.conflicts = crate::merge::find_conflicts(&self.lines);
        }
        &self.conflicts
    }

    /// Resolve the conflict containing `row` (VS Code's Accept Current /
    /// Incoming / Both): one undo step replacing the whole marker block.
    /// Returns false when `row` is not inside a conflict.
    pub fn resolve_conflict_at(&mut self, row: usize, res: crate::merge::Resolution) -> bool {
        let Some(block) = crate::merge::conflict_containing(self.conflicts(), row) else {
            return false;
        };
        let replacement = crate::merge::resolution_lines(&self.lines, &block, res);
        self.undoable_edit(EditKind::Replace, |this| {
            this.clear_selection();
            this.lines
                .splice(block.ours_start..=block.theirs_end, replacement);
            if this.lines.is_empty() {
                this.lines.push(String::new());
            }
            this.cursor_row = block.ours_start.min(this.lines.len() - 1);
            this.cursor_col = this.cursor_col.min(this.line_char_len(this.cursor_row));
        });
        true
    }

    /// Replace Result rows `[start, end)` with `replacement` as ONE undo
    /// step — the merge editor's accept-action primitive (#253), and (with
    /// the full range) the transform that turns a marker buffer into the
    /// initial Result, leaving the markers one Undo away. Mirrors
    /// `resolve_conflict_at`'s splice discipline.
    pub fn splice_result_rows(&mut self, start: usize, end: usize, replacement: Vec<String>) {
        let start = start.min(self.lines.len());
        let end = end.clamp(start, self.lines.len());
        self.undoable_edit(EditKind::Replace, |this| {
            this.clear_selection();
            this.lines.splice(start..end, replacement);
            if this.lines.is_empty() {
                this.lines.push(String::new());
            }
            this.cursor_row = start.min(this.lines.len() - 1);
            this.cursor_col = this.cursor_col.min(this.line_char_len(this.cursor_row));
        });
    }

    /// A read-only view of `path` as it was at some commit (#371): `text` is
    /// the file then, `baseline` the parent commit's version, so the git
    /// gutter shows that commit's own change (a root commit, or a file the
    /// commit added, has an empty baseline and marks every line added).
    pub fn historical(path: &Path, text: &str, baseline: Vec<String>) -> Editor {
        let mut e = Editor::new();
        e.path = Some(path.to_path_buf());
        e.lines = split_into_lines(text);
        if e.lines.is_empty() {
            e.lines.push(String::new());
        }
        e.lang = crate::highlight::lang_for_path(path);
        // Built off the UI thread, and no tick polls a scrubber view: take
        // the whole pass here rather than leaving it to land later.
        e.recompute_highlights_within(None);
        e.set_git_head_lines(path.to_path_buf(), Some(PackedLines::from(baseline)));
        // Never edited, so the marks are computed once, here.
        e.refresh_git_marks();
        e.warm_render_caches();
        e
    }

    /// [`Self::historical`] without the expensive parts: the text alone, no
    /// highlighting and no gutter marks. What the history scrubber shows for
    /// the frame or two until the full view is built off the UI thread
    /// (#371); cheap enough for every step of a held arrow key.
    pub fn historical_plain(path: &Path, text: &str) -> Editor {
        let mut e = Editor::new();
        e.path = Some(path.to_path_buf());
        e.lines = split_into_lines(text);
        if e.lines.is_empty() {
            e.lines.push(String::new());
        }
        e.highlights = vec![Vec::new(); e.lines.len()];
        e.warm_render_caches();
        e
    }
}

impl Editor {
    /// Screen cell of the diff view's read-only caret (its selection head),
    /// clamped to the viewport. The exact inverse of `diff_hit_test`: feed
    /// it the same layout (`last_inner`, gutters, seam, scroll) so the
    /// blinking caret lands on the cell a click there would map back to.
    /// `None` when there's no diff, no caret, or the caret is scrolled out.
    pub fn diff_caret_screen_pos(&self) -> Option<(u16, u16)> {
        use crate::widgets::diff::DiffSide;
        let diff = self.diff.as_ref()?;
        if diff.unified {
            return None;
        }
        let (side, row_idx, char_col) = diff.caret()?;
        let inner = self.last_inner;
        if inner.width < 16 || inner.height < 3 {
            return None;
        }
        let body_top = inner.y + 1;
        let body_height = inner.height.saturating_sub(2);
        if body_height == 0 || row_idx < diff.scroll {
            return None;
        }
        let vis_row = row_idx - diff.scroll;
        if vis_row as u16 >= body_height {
            return None;
        }
        let half = inner.width / 2;
        if half < 8 {
            return None;
        }
        let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
        let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
        let (text_x, text_w) = match side {
            DiffSide::Left => (
                inner.x + l_gutter + 2,
                half.saturating_sub(l_gutter + 2 + 1),
            ),
            DiffSide::Right => (
                inner.x + half + 1 + r_gutter + 2,
                (inner.width - (half + 1)).saturating_sub(r_gutter + 2),
            ),
        };
        if text_w == 0 || char_col < diff.scroll_x {
            return None;
        }
        // Cells, not chars: a tab before the caret is Tab Size cells wide.
        self.sync_tab_cells();
        let line = diff_side_line(diff, row_idx, side);
        let from = byte_index_of_char(line, diff.scroll_x);
        let to = byte_index_of_char(line, char_col.max(diff.scroll_x));
        let drawn = cells_from(&line[from..to], cells_from(&line[..from], 0));
        let line_len = line.chars().count();
        // Past the end of the text the caret counts one cell per char.
        let vis_col = drawn + char_col.saturating_sub(line_len.max(diff.scroll_x));
        if vis_col >= usize::from(text_w) {
            return None;
        }
        Some((text_x + vis_col as u16, body_top + vis_row as u16))
    }
}

pub(super) fn conflict_row_tint(
    blocks: &[crate::merge::ConflictBlock],
    row: usize,
    theme: crate::theme::Theme,
) -> Option<Color> {
    let block = blocks.iter().find(|b| b.contains(row))?;
    let ours_end = block.base_start.unwrap_or(block.sep);
    Some(if row == block.ours_start {
        theme.ui(Color::Rgb(0x2a, 0x4f, 0x33))
    } else if row < ours_end {
        theme.ui(Color::Rgb(0x1b, 0x33, 0x22))
    } else if row < block.sep {
        theme.ui(Color::Rgb(0x2a, 0x2a, 0x2a))
    } else if row == block.sep {
        theme.ui(Color::Rgb(0x28, 0x28, 0x28))
    } else if row == block.theirs_end {
        theme.ui(Color::Rgb(0x1f, 0x41, 0x66))
    } else {
        theme.ui(Color::Rgb(0x16, 0x2b, 0x44))
    })
}

impl AcceptChip {
    pub(super) fn new(label: &str, short: &str, fill: Color) -> Self {
        Self {
            label: label.to_string(),
            short: short.to_string(),
            fill,
        }
    }

    /// The button for `state`, ticked when the conflict is already resolved
    /// that way, in both its full and short forms.
    pub(super) fn ticked(mut self, ticked: bool) -> Self {
        if ticked {
            self.label = format!("\u{2713} {}", self.label);
            self.short = format!("\u{2713} {}", self.short);
        }
        self
    }
}

/// The accept buttons' fills, in the hues of the conflict tints they sit
/// on: green for current, blue for incoming, grey for both.
pub(super) const ACCEPT_CURRENT_FILL: Color = Color::Rgb(0x2f, 0x6b, 0x3e);

pub(super) const ACCEPT_INCOMING_FILL: Color = Color::Rgb(0x1f, 0x5a, 0x96);

pub(super) const ACCEPT_BOTH_FILL: Color = Color::Rgb(0x4b, 0x52, 0x63);

/// Lay `chips` out from column `x0`, ending at or before `limit`, a cell
/// apart: every full label when they all fit, else every short one, else as
/// many short ones as fit. Each placed chip comes back, in order, with its
/// column and the padded text to paint.
pub(super) fn lay_out_accept_chips(
    chips: &[AcceptChip],
    x0: u16,
    limit: u16,
) -> Vec<(u16, String)> {
    let place = |short: bool| {
        let mut out = Vec::new();
        let mut x = x0;
        for c in chips {
            let text = format!(" {} ", if short { &c.short } else { &c.label });
            let w = text.chars().count() as u16;
            if x.saturating_add(w) > limit {
                break;
            }
            out.push((x, text));
            x += w + 1;
        }
        out
    };
    let full = place(false);
    if full.len() == chips.len() {
        return full;
    }
    place(true)
}

/// Paint the accept buttons laid out for row `y`, returning each painted
/// button's index in `chips` and its column range for the click map.
pub(super) fn paint_accept_chips(
    buf: &mut Buffer,
    chips: &[AcceptChip],
    x0: u16,
    limit: u16,
    y: u16,
    theme: crate::theme::Theme,
) -> Vec<(usize, std::ops::Range<u16>)> {
    let mut spans = Vec::new();
    for (i, (x, text)) in lay_out_accept_chips(chips, x0, limit)
        .into_iter()
        .enumerate()
    {
        let w = text.chars().count() as u16;
        let style = Style::default()
            .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
            .bg(theme.ui(chips[i].fill))
            .add_modifier(Modifier::BOLD);
        buf.set_stringn(x, y, &text, w as usize, style);
        spans.push((i, x..x + w));
    }
    spans
}
