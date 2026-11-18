//! Drawing the panel: each input field with its fill, caret and glob caption, and the
//! whole panel, results included.

use super::*;

/// Per-channel lift applied to the theme background to fill an idle input row,
/// mirroring VS Code's `input.background` sitting a touch lighter than the
/// panel. On Black (#000) this lands a neutral #141414; on Croft Dark
/// (#1e222e) a lighter navy. Single source of truth for both helpers below.
pub(super) const FIELD_FILL_LIFT_IDLE: u8 = 0x14;

/// The focused input row lifts further still, standing in for VS Code's
/// `focusBorder` ring — in a cell grid we cannot draw a 1px sub-cell edge, so
/// the brighter fill plus the accent left-bar carry the focus signal.
pub(super) const FIELD_FILL_LIFT_FOCUS: u8 = 0x22;

/// The dim left-edge bar an unfocused field wears, so every input reads as a
/// touch target even before it gains the accent bar on focus.
pub(super) const FIELD_BAR_IDLE: Color = Color::Rgb(0x2b, 0x31, 0x3b);

/// Lift each channel of `(r, g, b)` by `d`, saturating. Used to derive the
/// input fill from the active theme background.
pub(super) fn lift_bg((r, g, b): (u8, u8, u8), d: u8) -> Color {
    Color::Rgb(
        r.saturating_add(d),
        g.saturating_add(d),
        b.saturating_add(d),
    )
}

/// Draw a faint hairline rule from column `x0` to `x1` (inclusive) on row `y`,
/// used to separate the stacked Search inputs into distinct groups. A terminal
/// cannot gap by less than one whole cell, so the divider turns the separator
/// row into a deliberate rule instead of dead space. It is dimmer than, and
/// spans less width than, the full results separator so the two never read as
/// the same element.
pub(super) fn draw_field_divider(
    buf: &mut Buffer,
    x0: u16,
    x1: u16,
    y: u16,
    theme: crate::theme::Theme,
) {
    let style = Style::default().fg(theme.ui(FIELD_BAR_IDLE));
    for x in x0..=x1 {
        buf.set_string(x, y, "─", style);
    }
}

/// Parameters for [`render_field`], bundled so the long argument list stays
/// readable at each call site.
pub(super) struct FieldArgs<'a> {
    /// Column of the accent/idle left-edge bar; the filled content begins one
    /// cell to its right.
    bar_x: u16,
    /// The single content row this field occupies.
    y: u16,
    /// Last column of the fill (inclusive).
    fill_right: u16,
    /// Optional leading glyph (e.g. the magnifier) drawn at the fill start.
    lead_glyph: Option<&'a str>,
    /// Current text of the field.
    text: &'a str,
    /// The caret as a char index into `text` (#1729).
    caret: usize,
    /// Placeholder shown italic-grey when `text` is empty.
    placeholder: &'a str,
    /// Selection range within `text`, already normalised (a <= b).
    selection: Option<(usize, usize)>,
    /// Whether THIS field is focused (drives the accent bar, brighter fill and
    /// whether the inline cursor renders here).
    field_focused: bool,
    /// Accent color for the bar / lead glyph / cursor when focused.
    accent: Color,
    /// The active theme background, lifted to derive the idle/focused fill so
    /// the field reads as elevated on Black and on Croft Dark alike.
    bg: (u8, u8, u8),
    /// Cells kept clear at the right of the fill for inset controls (the query
    /// row's toggles). The returned hit rect also stops short of them so a
    /// toggle click is not swallowed as a click-to-focus.
    reserve_right: u16,
}

/// A glob box's caption. An entry that doesn't compile replaces the label
/// with its name in red, right above the box, so a typo can't silently
/// change what the search covers (#1251).
pub(super) fn draw_glob_caption(
    buf: &mut Buffer,
    (x, y, right): (u16, u16, u16),
    label: &str,
    raw: &str,
    style: Style,
    theme: crate::theme::Theme,
) {
    let bad = invalid_globs(raw);
    if bad.is_empty() {
        buf.set_string(x, y, label, style);
        return;
    }
    buf.set_stringn(
        x,
        y,
        format!("invalid glob: {}", bad.join(", ")),
        right.saturating_sub(x) as usize + 1,
        Style::default().fg(theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c))),
    );
}

/// Render a single filled input row (VS Code's `input.background` model) and
/// return the click-to-focus hit `Rect` covering the bar + editable span (but
/// not any reserved control cells on the right). Shared by all four Search
/// inputs so they look identical: a one-cell accent/idle left bar, a subtly
/// lifted fill, an optional lead glyph, then the text/placeholder. Replaces the
/// old 3-row rounded box, which read as bulky next to VS Code / Zed's thin
/// single-line fields.
///
/// When the field is focused, `caret_out` is set to the screen cell where the
/// caret belongs (just past the text, at the field's left edge when empty) so
/// the app can drive the host's blinking hardware caret there, matching the
/// Source Control commit box. We no longer paint a static full block, which
/// never blinked and so read differently from the commit input.
pub(super) fn render_field(
    buf: &mut Buffer,
    args: FieldArgs,
    theme: crate::theme::Theme,
    caret_out: &mut Option<(u16, u16)>,
    origin_out: &mut (u16, usize),
) -> Rect {
    if args.fill_right <= args.bar_x {
        return Rect {
            x: args.bar_x,
            y: args.y,
            width: 0,
            height: 1,
        };
    }
    // Left-edge bar: accent when focused (standing in for VS Code's focus
    // border, which we cannot draw as a sub-cell stroke), dim otherwise so the
    // field still reads as a target at rest.
    let bar_color = if args.field_focused {
        args.accent
    } else {
        theme.ui(FIELD_BAR_IDLE)
    };
    buf.set_string(args.bar_x, args.y, "▏", Style::default().fg(bar_color));

    let fill_x = args.bar_x + 1;
    let lift = if args.field_focused {
        FIELD_FILL_LIFT_FOCUS
    } else {
        FIELD_FILL_LIFT_IDLE
    };
    let fill = lift_bg(args.bg, lift);
    let fill_style = Style::default().bg(fill);
    for x in fill_x..=args.fill_right {
        buf[(x, args.y)].set_symbol(" ").set_style(fill_style);
    }

    // Croft Loch writes the field in its ink ramp: the text in body ink,
    // the placeholder and an idle lead glyph muted. A boxed theme keeps its
    // historical colours.
    let tones = crate::widgets::surface::tones(theme);
    let lead_w: u16 = if args.lead_glyph.is_some() { 2 } else { 0 };
    if let Some(g) = args.lead_glyph {
        let lead_color = match tones {
            _ if args.field_focused => args.accent,
            Some(t) => t.muted(),
            None => theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
        };
        buf.set_string(fill_x, args.y, g, Style::default().fg(lead_color).bg(fill));
    }

    // Editable span: after the lead glyph, up to the reserved control cells.
    let text_x = fill_x + lead_w;
    let text_right = args.fill_right.saturating_sub(args.reserve_right);
    let text_w = text_right.saturating_sub(text_x).saturating_add(1);

    let mut spans: Vec<Span> = Vec::with_capacity(4);
    let win = crate::widgets::line_edit::window(args.text, args.caret, usize::from(text_w));
    *origin_out = (text_x, win.start);
    if args.text.is_empty() {
        let ink = match tones {
            Some(t) => t.muted(),
            None => theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c)),
        };
        spans.push(Span::styled(
            args.placeholder.to_string(),
            Style::default()
                .fg(ink)
                .bg(fill)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        let ink = match tones {
            Some(t) => t.text(),
            None => theme.ui(Color::White),
        };
        let plain = Style::default().fg(ink).bg(fill);
        let selected = Style::default()
            .fg(ink)
            .bg(theme.ui(Color::Rgb(0x26, 0x4f, 0x78)));
        // Only the part of the text the row has room for, scrolled so the
        // caret is in view; the selection is clipped to it.
        let (sb, eb) = (
            crate::widgets::line_edit::byte_of(args.text, win.start),
            crate::widgets::line_edit::byte_of(args.text, win.end),
        );
        let shown = &args.text[sb..eb];
        let sel = args
            .selection
            .map(|(a, b)| (a.max(sb) - sb.min(a.max(sb)), b.min(eb).saturating_sub(sb)))
            .filter(|&(a, b)| a < b && b <= shown.len());
        match sel {
            Some((a, b)) => {
                if a > 0 {
                    spans.push(Span::styled(shown[..a].to_string(), plain));
                }
                spans.push(Span::styled(shown[a..b].to_string(), selected));
                if b < shown.len() {
                    spans.push(Span::styled(shown[b..].to_string(), plain));
                }
            }
            None => spans.push(Span::styled(shown.to_string(), plain)),
        }
    }
    if text_w > 0 {
        buf.set_line(text_x, args.y, &Line::from(spans), text_w);
    }

    // The caret rides just past the text (at the field's left edge when empty),
    // clamped inside the editable span. The app shows the host's blinking
    // hardware caret here only while this field is focused, so we record the
    // cell for the focused field alone. The caret is a char index, the
    // commit box's model (`message_cursor`), drawn where the text window put it.
    if args.field_focused && text_w > 0 {
        let off = (win.caret_cell as u16).min(text_w.saturating_sub(1));
        *caret_out = Some((text_x + off, args.y));
    }

    // Hit rect: bar + editable span, excluding reserved control cells so the
    // query toggles (checked after `field_at`) still receive their clicks.
    Rect {
        x: args.bar_x,
        y: args.y,
        width: text_right.saturating_sub(args.bar_x).saturating_add(1),
        height: 1,
    }
}

impl Widget for &mut SearchPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let focus_blue = self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff));
        // Black theme: inner stroke accents (chevron, input ring, magnifier,
        // cursor) wear the muted brand teal instead of the legacy blue.
        let accent = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::INNER_ACCENT)
        } else {
            focus_blue
        };
        let outer_style = if self.focused {
            Style::default().fg(focus_blue)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let outer = crate::widgets::surface::view_block(self.theme, true, outer_style);
        let inner = outer.inner(area);
        outer.render(area, buf);
        // Black theme: replace the solid focus border with the brand gradient.
        // No title sits on this border (the SEARCH header is inside), so there
        // is nothing to re-stamp.
        crate::widgets::surface::paint_view_focus(
            buf,
            area,
            self.theme,
            self.focused,
            self.focus_gradient,
        );
        self.last_area = area;
        self.last_inner = inner;
        // Clear last frame's caret up front so an early return (too small to
        // lay out a field) never leaves a stale hardware-caret position.
        self.caret_cell = None;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Croft Loch's inks and fills; `None` on a boxed theme, which keeps
        // the colours below exactly as they were.
        let tones = crate::widgets::surface::tones(self.theme);

        // Row 0: "SEARCH" header (top-left) plus the VS Code SEARCH header
        // actions (refresh / clear) at the top-right. Croft Loch titles the
        // view as the Explorer does: the strong ink while it has the
        // keyboard, muted otherwise, and never bold.
        let title_style = match tones {
            Some(t) => Style::default().fg(if self.focused { t.strong() } else { t.muted() }),
            None => Style::default()
                .fg(self.theme.ui(Color::Rgb(0xb0, 0xb8, 0xc8)))
                .add_modifier(Modifier::BOLD),
        };
        buf.set_string(inner.x, inner.y, "SEARCH", title_style);

        // Header action icons, laid out from the right edge: clear (rightmost)
        // then refresh, each a single-cell click target that lifts to the accent
        // on hover. Only drawn when the panel is wide enough to clear the label.
        self.refresh_x = 0;
        self.refresh_y = inner.y;
        self.clear_x = 0;
        self.clear_y = inner.y;
        if inner.width > 16 {
            let clear_x = inner.x + inner.width - 1;
            let refresh_x = clear_x - 3;
            for (x, glyph) in [
                (refresh_x, crate::icons::SEARCH_REFRESH),
                (clear_x, crate::icons::SEARCH_CLEAR_ALL),
            ] {
                let hot = self
                    .hover_pointer
                    .is_some_and(|(px, py)| py == inner.y && px == x);
                // Croft Loch: the side bar's header actions, muted at rest
                // and lifted to the strong ink on a faint chip under the
                // pointer; the accent stays for focus.
                let style = if tones.is_some() {
                    crate::widgets::header_pill::action_style(self.theme, hot)
                } else if hot {
                    Style::default().fg(accent)
                } else {
                    Style::default().fg(self.theme.ui(Color::Rgb(0x8b, 0x94, 0xa4)))
                };
                buf.set_string(x, inner.y, glyph.to_string(), style);
            }
            self.refresh_x = refresh_x;
            self.clear_x = clear_x;
        }

        // Below this the panel cannot host the header + a field row, or is too
        // narrow for the chevron column + a field bar + lead glyph + content.
        if inner.height < 4 || inner.width < 8 {
            return;
        }

        // ---- Field cluster geometry ----
        // VS Code / Zed model: each input is a single filled row, never a 3-row
        // box. A chevron sits at the far left (toggles the Replace row); every
        // field is indented two cells past it so the chevron column stays clear.
        // The query row's `Aa ab .*` toggles are inset INSIDE the field at its
        // right edge (VS Code's `.controls`), and the `...` details expander
        // sits in the rightmost cell, just past the field.
        let bg = self.theme.editor_bg_rgb();
        let chevron_x = inner.x;
        let bar_x = inner.x + 2; // left edge of every field's fill bar
        let inner_right = inner.x + inner.width - 1; // last usable column
        let content_y = inner.y + 2; // one blank row below the header

        // The `...` details expander occupies the rightmost cell of the query
        // row; the query field's fill ends two cells short of it.
        let ellipsis_x = inner_right;
        let query_fill_right = ellipsis_x.saturating_sub(2);

        // Inset toggles, right-aligned inside the query fill with a 1-cell pad.
        // Each glyph is two cells; one-cell gaps separate them. `toggle_at` keys
        // off these stored columns, so the layout must match cell-for-cell.
        let regex_x = query_fill_right.saturating_sub(2); // 1-cell pad + 2-cell glyph
        let word_x = regex_x.saturating_sub(3);
        let case_x = word_x.saturating_sub(3);
        // Cells the query field reserves on its right so its text never slides
        // under the toggles: from the first toggle column to the fill edge.
        let query_reserve = query_fill_right.saturating_sub(case_x).saturating_add(1);

        // Expand chevron (far left). ▾ when Replace is open, ▸ when collapsed.
        // Croft Loch: the side bar's codicon chevron in the muted ink, an
        // idle icon like the Explorer's; the field's bar already says where
        // focus is.
        let chevron_style = match tones {
            Some(t) => Style::default().fg(t.muted()),
            None => Style::default()
                .fg(if self.focused {
                    accent
                } else {
                    Color::DarkGray
                })
                .add_modifier(Modifier::BOLD),
        };
        let chevron_glyph = crate::widgets::surface::chevron(self.theme, self.replace_open);
        buf.set_string(
            chevron_x,
            content_y,
            chevron_glyph.to_string(),
            chevron_style,
        );
        self.chevron_x = chevron_x;
        self.chevron_y = content_y;

        // Caret cell of whichever field is focused. `render_field` writes it
        // only for the focused field (and never clears it for the others), so
        // it accumulates the single focused field's caret across the calls
        // below; committed to `self.caret_cell` once the cluster is laid out.
        let mut caret: Option<(u16, u16)> = None;
        let mut origins = self.field_origin;

        // Query field: filled single row, magnifier lead, inset toggles.
        let query_focused = self.focused && self.field == SearchField::Query;
        self.query_input_rect = render_field(
            buf,
            FieldArgs {
                bar_x,
                y: content_y,
                fill_right: query_fill_right,
                lead_glyph: Some("\u{ea6d}"),
                text: &self.query,
                caret: self.field_caret(SearchField::Query),
                placeholder: "Search",
                selection: self.selection_range(),
                field_focused: query_focused,
                accent,
                bg,
                reserve_right: query_reserve,
            },
            self.theme,
            &mut caret,
            &mut origins[SearchField::Query.index()],
        );

        // Inset toggles `Aa ab .*`, painted on top of the query fill at its
        // right edge. Active = a muted accent chip (standing in for VS Code's
        // translucent `inputOption.activeBackground`); inactive = a dim glyph
        // sitting on the same fill so the chips read as part of the field.
        self.paste_button_x = 0;
        self.paste_button_y = content_y;
        self.paste_button_w = 0;
        let toggle_fill = lift_bg(
            bg,
            if query_focused {
                FIELD_FILL_LIFT_FOCUS
            } else {
                FIELD_FILL_LIFT_IDLE
            },
        );
        // Croft Loch: an on toggle is the strong ink on the selection chip
        // (no bold: weight is rare), an off one the muted ink.
        let active_style = match tones {
            Some(t) => Style::default()
                .fg(t.strong())
                .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)),
            None => Style::default()
                .fg(Color::White)
                .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
                .add_modifier(Modifier::BOLD),
        };
        let inactive_style = Style::default()
            .fg(match tones {
                Some(t) => t.muted(),
                None => self.theme.ui(Color::Rgb(0x79, 0x82, 0x8f)),
            })
            .bg(toggle_fill);
        self.toggle_y = content_y;
        self.toggle_case_x = case_x;
        self.toggle_word_x = word_x;
        self.toggle_regex_x = regex_x;
        for (x, glyph, active) in [
            (case_x, "Aa", self.opts.case_sensitive),
            (word_x, "ab", self.opts.whole_word),
            (regex_x, ".*", self.opts.use_regex),
        ] {
            let style = if active { active_style } else { inactive_style };
            buf.set_string(x, content_y, glyph, style);
        }

        // `...` details expander, just past the query field. Accent when open.
        let ellipsis_style = match tones {
            Some(t) if self.details_open => Style::default().fg(t.accent()),
            Some(t) => Style::default().fg(t.muted()),
            None if self.details_open => Style::default().fg(accent).add_modifier(Modifier::BOLD),
            None => Style::default().fg(self.theme.ui(Color::Rgb(0x79, 0x82, 0x8f))),
        };
        buf.set_string(
            ellipsis_x,
            content_y,
            crate::icons::SEARCH_ELLIPSIS.to_string(),
            ellipsis_style,
        );
        self.ellipsis_x = ellipsis_x;
        self.ellipsis_y = content_y;

        // ---- Optional Replace row ----
        // `next_y` tracks the next content row as optional fields stack. Each is
        // now a single row, so the cluster is far shorter than the old boxes.
        let mut next_y = content_y + 1;
        self.replace_input_rect = Rect::default();
        self.toggle_preserve_x = 0;
        self.replace_all_x = 0;
        self.replace_all_y = 0;
        if self.replace_open && next_y + 1 < inner.y + inner.height {
            // A faint hairline between the query and replace fields so they read
            // as two separate inputs instead of one continuous filled container.
            draw_field_divider(buf, bar_x, query_fill_right, next_y, self.theme);
            next_y += 1;
            // The replace-all action icon sits at the far right (under the ...),
            // and the replace fill ends two cells short of it.
            let replace_all_x = inner_right;
            let replace_fill_right = replace_all_x.saturating_sub(2).max(bar_x + 1);
            let replace_focused = self.focused && self.field == SearchField::Replace;
            self.replace_input_rect = render_field(
                buf,
                FieldArgs {
                    bar_x,
                    y: next_y,
                    fill_right: replace_fill_right,
                    lead_glyph: Some(&crate::icons::SEARCH_REPLACE.to_string()),
                    text: &self.replace,
                    caret: self.field_caret(SearchField::Replace),
                    placeholder: "Replace (Tab switches)",
                    selection: if self.field == SearchField::Replace {
                        self.active_selection()
                    } else {
                        None
                    },
                    field_focused: replace_focused,
                    accent,
                    bg,
                    reserve_right: 3,
                },
                self.theme,
                &mut caret,
                &mut origins[SearchField::Replace.index()],
            );
            // `AB` (Preserve Case) inset at the replace field's right edge,
            // where VS Code keeps it, styled like the query's toggles.
            let preserve_x = replace_fill_right.saturating_sub(2);
            if preserve_x > bar_x + 1 {
                let fill = lift_bg(
                    bg,
                    if replace_focused {
                        FIELD_FILL_LIFT_FOCUS
                    } else {
                        FIELD_FILL_LIFT_IDLE
                    },
                );
                let style = if self.opts.preserve_case {
                    active_style
                } else {
                    inactive_style.bg(fill)
                };
                buf.set_string(preserve_x, next_y, "AB", style);
                self.toggle_preserve_x = preserve_x;
                self.toggle_preserve_y = next_y;
            }
            buf.set_string(
                replace_all_x,
                next_y,
                crate::icons::SEARCH_REPLACE_ALL.to_string(),
                Style::default().fg(match tones {
                    // Croft Loch: an action icon, muted like the header's.
                    Some(t) => t.muted(),
                    None if self.focused => accent,
                    None => self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
                }),
            );
            self.replace_all_x = replace_all_x;
            self.replace_all_y = next_y;
            next_y += 1;
        }

        // ---- Optional files-to-include / files-to-exclude rows ----
        // VS Code stacks an 11px caption above each thin field rather than using
        // placeholder-as-label, so each detail field is a caption row over a
        // single filled field.
        self.include_input_rect = Rect::default();
        self.exclude_input_rect = Rect::default();
        self.project_exclude_rect = Rect::default();
        if self.details_open {
            let detail_fill_right = inner_right.saturating_sub(1).max(bar_x + 1);
            let caption_style = Style::default().fg(match tones {
                Some(t) => t.muted(),
                None => self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
            });

            if next_y + 2 < inner.y + inner.height {
                // Hairline before the include group, matching the query/replace
                // separation so every input reads as its own row.
                draw_field_divider(buf, bar_x, detail_fill_right, next_y, self.theme);
                next_y += 1;
                draw_glob_caption(
                    buf,
                    (bar_x, next_y, detail_fill_right),
                    "files to include",
                    &self.include,
                    caption_style,
                    self.theme,
                );
                next_y += 1;
                let inc_focused = self.focused && self.field == SearchField::Include;
                self.include_input_rect = render_field(
                    buf,
                    FieldArgs {
                        bar_x,
                        y: next_y,
                        fill_right: detail_fill_right,
                        lead_glyph: None,
                        text: &self.include,
                        caret: self.field_caret(SearchField::Include),
                        placeholder: "e.g. src/**/*.rs",
                        selection: if self.field == SearchField::Include {
                            self.active_selection()
                        } else {
                            None
                        },
                        field_focused: inc_focused,
                        accent,
                        bg,
                        reserve_right: 0,
                    },
                    self.theme,
                    &mut caret,
                    &mut origins[SearchField::Include.index()],
                );
                next_y += 1;
            }
            if next_y + 2 < inner.y + inner.height {
                // Hairline before the exclude group, same as above.
                draw_field_divider(buf, bar_x, detail_fill_right, next_y, self.theme);
                next_y += 1;
                draw_glob_caption(
                    buf,
                    (bar_x, next_y, detail_fill_right),
                    "files to exclude",
                    &self.exclude,
                    caption_style,
                    self.theme,
                );
                next_y += 1;
                let exc_focused = self.focused && self.field == SearchField::Exclude;
                self.exclude_input_rect = render_field(
                    buf,
                    FieldArgs {
                        bar_x,
                        y: next_y,
                        fill_right: detail_fill_right,
                        lead_glyph: None,
                        text: &self.exclude,
                        caret: self.field_caret(SearchField::Exclude),
                        placeholder: "e.g. target, dist",
                        selection: if self.field == SearchField::Exclude {
                            self.active_selection()
                        } else {
                            None
                        },
                        field_focused: exc_focused,
                        accent,
                        bg,
                        reserve_right: 0,
                    },
                    self.theme,
                    &mut caret,
                    &mut origins[SearchField::Exclude.index()],
                );
                next_y += 1;
                if !self.project_exclude.is_empty() && next_y + 1 < inner.y + inner.height {
                    // VS Code's "Use Exclude Settings and Ignore Files", as a
                    // checkbox naming what the project hides (#1345): one
                    // click searches everything.
                    let text = format!(
                        "[{}] project excludes: {}",
                        if self.use_exclude_settings { "x" } else { " " },
                        self.project_exclude.join(", ")
                    );
                    let width = detail_fill_right.saturating_sub(bar_x) + 1;
                    let style = if self.use_exclude_settings {
                        caption_style
                    } else {
                        caption_style.add_modifier(Modifier::CROSSED_OUT)
                    };
                    buf.set_stringn(bar_x, next_y, &text, width as usize, style);
                    self.project_exclude_rect = Rect {
                        x: bar_x,
                        y: next_y,
                        width: (text.chars().count() as u16).min(width),
                        height: 1,
                    };
                    next_y += 1;
                }
            }
        }
        // Commit the focused field's caret now that the whole input cluster is
        // laid out. Always runs (the optional rows above only skip their own
        // render), so the app sees `None` whenever no field carried a caret.
        self.caret_cell = caret;
        self.field_origin = origins;

        // ---- Separator + match-count caption ----
        let separator_y = next_y;
        if separator_y < inner.y + inner.height {
            // Croft Loch: a hairline seam hugging the results below, as
            // Source Control draws above its changes, not a line twice as
            // bright as every other seam in the side bar.
            let (sep_glyph, sep_style) = match tones {
                Some(t) => ("\u{2581}", Style::default().fg(t.hairline())),
                None => (
                    "─",
                    Style::default().fg(self.theme.ui(Color::Rgb(0x40, 0x48, 0x58))),
                ),
            };
            for x in inner.x..inner.x + inner.width {
                buf.set_string(x, separator_y, sep_glyph, sep_style);
            }
        }
        let caption_y = separator_y + 1;
        let mut results_start_y = caption_y + 1;
        if caption_y < inner.y + inner.height && !as_typed(&self.query).is_empty() {
            if let Some(error) = self.query_error() {
                // Wrapped, not cut off: the sidebar is narrow and the
                // reason sits at the end of the message.
                let style = Style::default().fg(self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)));
                let rows = crate::sarif::render::wrap(&error, inner.width.max(1) as usize);
                let mut y = caption_y;
                for row in rows {
                    if y >= inner.y + inner.height {
                        break;
                    }
                    buf.set_stringn(inner.x, y, &row, inner.width as usize, style);
                    y += 1;
                }
                results_start_y = y.max(caption_y + 1);
            } else {
                let (results, files) = self.result_counts();
                let (r_s, f_s) = (
                    if results == 1 { "" } else { "s" },
                    if files == 1 { "" } else { "s" },
                );
                let caption = match tones {
                    // Croft Loch: a quiet sentence in the muted ink whose
                    // two counts, the part worth reading, are body text.
                    Some(t) => {
                        let (num, word) = (
                            Style::default().fg(t.text()),
                            Style::default().fg(t.muted()),
                        );
                        Line::from(vec![
                            Span::styled(results.to_string(), num),
                            Span::styled(format!(" result{r_s} in "), word),
                            Span::styled(files.to_string(), num),
                            Span::styled(format!(" file{f_s}"), word),
                        ])
                    }
                    None => Line::from(Span::styled(
                        format!("{results} result{r_s} in {files} file{f_s}"),
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)))
                            .add_modifier(Modifier::ITALIC),
                    )),
                };
                buf.set_line(inner.x, caption_y, &caption, inner.width);
            }
        }
        // Record where results begin so scrolling / click mapping stay aligned.
        self.results_start_offset = results_start_y.saturating_sub(inner.y);

        // Results
        self.last_scrollbar = Rect::default();
        if results_start_y >= inner.y + inner.height {
            return;
        }
        let visible = (inner.y + inner.height - results_start_y) as usize;
        if visible == 0 {
            return;
        }
        // Clamp scroll so the viewport is always full when there's enough
        // content; otherwise the user can wheel past the end and see blanks.
        // Critically, we do NOT auto-follow `selected` here: that would
        // fight the user every time a streamed `Hits` batch lands and
        // re-renders. Selection-follow lives in `move_up`/`move_down`,
        // where it belongs - keyboard nav, not wheel scroll.
        let max_scroll = self.hits.len().saturating_sub(visible);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        // Reserve the rightmost column for the scrollbar when the result
        // list overflows the viewport. `vertical_metrics` returns None when
        // there's no overflow, so the rightmost column is freed for content.
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: results_start_y,
            width: 1,
            height: inner.y + inner.height - results_start_y,
        };
        let scrollbar_metrics = crate::widgets::scrollbar::vertical_metrics(
            scrollbar_area,
            self.hits.len(),
            visible,
            self.scroll,
        );
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let row_width = inner
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));
        let end = (self.scroll + visible).min(self.hits.len());
        for (row_idx, hit_idx) in (self.scroll..end).enumerate() {
            let y = results_start_y + row_idx as u16;
            let hit = &self.hits[hit_idx];
            // Lift the row under the pointer (unless it is already the
            // selected row, which carries its own highlight). Pre-filling the
            // bg works because `set_line` only patches the cells it writes and
            // leaves their bg untouched when a span sets no bg of its own.
            let row_rect = Rect {
                x: inner.x,
                y,
                width: row_width,
                height: 1,
            };
            let is_selected = hit_idx == self.selected;
            // The fill under the row's text on Croft Loch (chrome, the hover
            // lift or the selected band), which a match's amber tint is
            // blended from so it reads the same on every row.
            let mut row_fill = tones.map(|t| t.chrome());
            if !is_selected
                && let Some(bg) =
                    crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, self.theme)
            {
                let fill = Style::default().bg(bg);
                for rx in 0..row_width {
                    buf[(inner.x + rx, y)].set_symbol(" ").set_style(fill);
                }
                row_fill = row_fill.map(|_| bg);
            }
            // Croft Loch: the one selected-row anatomy, the band across the
            // full row with the accent bar standing in the pane's left
            // padding column (as the Explorer's does) so the names keep
            // their column; greyed, barless, while another pane has the
            // keyboard.
            if is_selected && let Some(t) = tones {
                crate::widgets::surface::paint_selected_row(
                    buf,
                    Rect {
                        x: area.x,
                        width: row_width.saturating_add(inner.x - area.x),
                        ..row_rect
                    },
                    t,
                    t.chrome,
                    self.focused,
                );
                row_fill = Some(crate::gradient::rgb_color(
                    crate::widgets::surface::selected_row_fill(t, t.chrome, self.focused),
                ));
            }
            // Show just the basename so the matched line itself has room
            // to render inside a narrow side panel — without this, deep
            // monorepo paths consumed the whole row and the highlight fell
            // off the right edge.
            let path_display = hit
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    let owner = self
                        .roots
                        .iter()
                        .filter(|r| hit.path.starts_with(r))
                        .max_by_key(|r| r.components().count());
                    match owner {
                        Some(r) if self.roots.len() > 1 => {
                            let idx = self.roots.iter().position(|x| x == r).unwrap_or(0);
                            let name =
                                crate::workspace::root_display_labels(&self.roots).swap_remove(idx);
                            format!(
                                "{name}/{}",
                                hit.path
                                    .strip_prefix(r)
                                    .unwrap_or(hit.path.as_path())
                                    .display()
                            )
                        }
                        Some(r) => hit
                            .path
                            .strip_prefix(r)
                            .unwrap_or(hit.path.as_path())
                            .display()
                            .to_string(),
                        None => hit.path.display().to_string(),
                    }
                });
            // Black theme: selected result row matches the Explorer's
            // white-on-dark-teal selection; Croft Dark keeps black-on-blue.
            let header_style = if let Some(t) = tones {
                // Croft Loch: the file name is body text on every row; the
                // band says which one is selected.
                Style::default().fg(t.text())
            } else if is_selected {
                if self.focus_gradient {
                    Style::default()
                        .fg(Color::White)
                        .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::Black)
                        .bg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
                        .add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default().fg(self.theme.ui(Color::White))
            };
            let needle = as_typed(&self.query);
            // High-contrast yellow background like ripgrep / VS Code's
            // editor.findMatchHighlightBackground. Different treatment when
            // the row is selected so the highlight stays readable against
            // the blue row bg instead of fighting it.
            let highlight_style = Style::default()
                .fg(Color::Black)
                .bg(self.theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
                .add_modifier(Modifier::BOLD);
            // Croft Loch reads a row in two inks: the file name in body
            // text, the line number and the preview muted, with each match
            // tinted amber over the row's own fill. The line number is where
            // the result is, a fact read as often as the name, so it never
            // drops to the faint ink kept for marks. On the selected row the
            // preview steps up an ink, so it keeps its contrast on the band;
            // the muted line number already does. The selected row's first
            // match is the one opening it lands on, so it takes the current
            // match's stronger tint, as it will in the editor.
            let (plain_style, line_no_style) = match tones {
                Some(t) if is_selected => (
                    Style::default().fg(t.text()),
                    Style::default().fg(t.muted()),
                ),
                Some(t) => (
                    Style::default().fg(t.muted()),
                    Style::default().fg(t.muted()),
                ),
                None => (
                    Style::default().fg(self.theme.ui(Color::Gray)),
                    Style::default().fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
                ),
            };
            let match_style = |active: bool| match (tones, row_fill) {
                (Some(t), Some(fill)) => crate::widgets::surface::match_style(
                    plain_style.bg(fill),
                    t,
                    self.theme,
                    active,
                ),
                _ => highlight_style,
            };
            let mut active_pending = is_selected;
            let mut spans: Vec<Span> = vec![
                Span::styled(format!(" {path_display}"), header_style),
                Span::styled(format!(":{}: ", hit.line_no), line_no_style),
            ];
            // Inline replace preview (#123): while a replacement is typed,
            // each match renders dim and crossed out with the expanded
            // replacement (capture refs honoured) in green beside it — the
            // bad-pattern catch before Replace All commits anything.
            let preview = self.replace_open && !self.replace.is_empty();
            let mut chunks = split_for_highlight(&hit.line_text, needle, self.opts);
            // A match past the row's edge was clipped away with everything
            // after it (#860): drop leading context so it shows.
            if let Some(first) = chunks.iter().position(|(_, is_match)| *is_match) {
                let prefix_w: usize = spans.iter().map(Span::width).sum();
                let avail = usize::from(row_width).saturating_sub(prefix_w);
                let matched = &chunks[first].0;
                let mut need = matched.width();
                if preview {
                    need += expand_replacement(matched, needle, &self.replace, self.opts).width();
                }
                let lead: String = chunks[..first].iter().map(|(t, _)| t.as_str()).collect();
                if let Some(kept) = trim_leading_context(&lead, avail, need) {
                    chunks.splice(..first, std::iter::once((kept, false)));
                }
            }
            for (chunk, is_match) in chunks {
                if is_match && preview {
                    let new_text = expand_replacement(&chunk, needle, &self.replace, self.opts);
                    spans.push(Span::styled(
                        chunk,
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0x8b, 0x93, 0xa5)))
                            .add_modifier(Modifier::CROSSED_OUT),
                    ));
                    spans.push(Span::styled(
                        new_text,
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0xb6, 0xee, 0xc4)))
                            .add_modifier(Modifier::BOLD),
                    ));
                    continue;
                }
                let style = if is_match {
                    let active = std::mem::take(&mut active_pending);
                    match_style(active)
                } else {
                    plain_style
                };
                spans.push(Span::styled(chunk, style));
            }
            let line = Line::from(spans);
            buf.set_line(inner.x, y, &line, row_width);
        }
        if let Some(metrics) = scrollbar_metrics {
            crate::widgets::scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}
