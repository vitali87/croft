//! The welcome screen, its release highlights and the tour.

use super::*;

fn note_fixture(
    kind: crate::release_notes::NoteKind,
    summary: &'static str,
) -> crate::release_notes::ReleaseNote {
    crate::release_notes::ReleaseNote { kind, summary }
}

#[test]
fn welcome_note_summary_wraps_long_lines() {
    let n = note_fixture(
        crate::release_notes::NoteKind::Feature,
        "Code Actions and Quick Fix on Cmd+., aggregated across every running language server in the workspace",
    );
    let lines = wrapped_welcome_note_summary(&n, 42);
    assert!(lines.len() > 1, "long highlight summaries should wrap");
    for line in &lines {
        assert!(line.chars().count() <= welcome_note_summary_width(42) as usize);
    }
}

#[test]
fn welcome_recents_height_accounts_for_wrapped_note_rows() {
    let n = note_fixture(
        crate::release_notes::NoteKind::Fix,
        "A reasonably long highlight summary that will certainly wrap to more than a single line in a narrow column",
    );
    let h = welcome_recents_height(&[n], 42);
    // header + blank + at least two wrapped rows.
    assert!(
        h > 1 + 1 + 1,
        "height must include wrapped continuation rows; got {h}"
    );
}

/// The user dislikes underlined text in the welcome panel: it adds visual
/// noise. Assert the rendered buffer contains zero cells with the UNDERLINED
/// modifier so this preference is enforced by the test suite.
#[test]
fn welcome_panel_renders_without_any_underlined_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(vec![
        note_fixture(crate::release_notes::NoteKind::Feature, "Did a new thing"),
        note_fixture(crate::release_notes::NoteKind::Fix, "Fixed an old thing"),
    ]);
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, area)).unwrap();
    let buffer = term.backend().buffer();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            assert!(
                !cell.modifier.contains(Modifier::UNDERLINED),
                "cell at ({x}, {y}) symbol={:?} has UNDERLINED set; the welcome panel must render no underlines",
                cell.symbol()
            );
        }
    }
}

/// #954: a note written in Markdown shows its `**bold**` text bold and its
/// `` `code` `` in the code colour, with none of the markers on the card.
#[test]
fn welcome_notes_draw_inline_markdown_without_its_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(vec![note_fixture(
        crate::release_notes::NoteKind::Feature,
        "Press **Add Database** or `Cmd+K V` to start",
    )]);
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, area)).unwrap();
    let buffer = term.backend().buffer();
    let rows: Vec<String> = (0..area.height)
        .map(|y| (0..area.width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();
    let (y, row) = rows
        .iter()
        .enumerate()
        .find(|(_, r)| r.contains("Press Add Database or Cmd+K V to start"))
        .expect("the note is shown without its markers");
    assert!(!rows.iter().any(|r| r.contains("**") || r.contains('`')));
    let at = |needle: &str| row[..row.find(needle).unwrap()].chars().count() as u16;
    let cell = |x: u16| &buffer[(x, y as u16)];
    assert!(cell(at("Add")).modifier.contains(Modifier::BOLD));
    assert!(!cell(at("Press")).modifier.contains(Modifier::BOLD));
    assert_ne!(
        cell(at("Cmd")).fg,
        cell(at("Press")).fg,
        "code is drawn in its own colour"
    );
}

#[test]
fn welcome_panel_does_not_paint_the_blazingly_fast_slogan_footer() {
    // The user asked to drop the chevron-bracketed
    // "Blazingly fast by design. Secure by default. Loved by developers."
    // line. Render the welcome panel into a buffer and confirm none
    // of the slogan fragments appear anywhere on the canvas.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, area)).unwrap();
    let buffer = term.backend().buffer();
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        for needle in [
            "Blazingly fast by design",
            "Secure by default",
            "Loved by developers",
        ] {
            assert!(
                !row.contains(needle),
                "welcome row {y} still carries removed slogan {needle:?}: {row:?}"
            );
        }
    }
}

#[test]
fn render_welcome_does_not_panic_in_default_80x25_with_many_notes() {
    // Repro for the index-(41,70) panic at startup: ratatui's initial
    // backend size is 80x25 before the alt-screen reflow. With a long
    // highlights list the box could compute taller than 25, run past the
    // buffer, and panic inside set_string — so clamp to the area.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(
        (0..40)
            .map(|_| {
                note_fixture(
                    crate::release_notes::NoteKind::Feature,
                    "this is a long highlight summary that will wrap to multiple lines on a narrow column",
                )
            })
            .collect(),
    );
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_welcome(f, area)).unwrap();
}

#[test]
fn welcome_tagline_constant_is_present() {
    assert!(WELCOME_TAGLINE.contains("LIGHTWEIGHT"));
    assert!(WELCOME_TAGLINE.contains("BLAZINGLY FAST"));
    assert!(WELCOME_TAGLINE.contains("DEVELOPERS"));
}

/// #1843: the version never shares a cell with the logo or the tagline, at
/// any window size and whatever height the release-note card takes. The
/// logo's cells are the image's, edge to edge; the tagline's last column
/// reaches the badge's first, and once painted over its corner. Croft
/// Black boxes the version beside the logo; Croft Loch's picture logo sets it
/// as a one-row pill on the box's middle row, in the same columns (the
/// picture path is forced here: its cells stay blank while it bakes).
#[test]
fn the_welcome_version_never_overlaps_the_logo_or_the_tagline() {
    let tmp = tempfile::tempdir().unwrap();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    for theme in [crate::theme::Theme::BLACK, crate::theme::Theme::LOCH] {
        let loch = theme.surfaces().is_some();
        let tagline = if loch {
            App::WELCOME_TAGLINE_CALM
        } else {
            WELCOME_TAGLINE
        };
        let mut seen_badge = false;
        let mut seen_below = false;
        for notes in [Vec::new(), long_test_note()] {
            let mut app = App::new(tmp.path().to_path_buf()).unwrap();
            app.apply_theme(theme);
            if loch {
                app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
                app.cell_pixel = Some((8, 16));
            }
            app.welcome.set_test_notes(notes);
            for w in (30..=200).step_by(3) {
                for h in (8..=56).step_by(2) {
                    let mut term =
                        ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
                    term.draw(|f| app.render_welcome(f, f.area())).unwrap();
                    let buf = term.backend().buffer();
                    let rows: Vec<String> = (0..h)
                        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
                        .collect();
                    let logo = *app.overlays.welcome.layout().expect("laid out");
                    // Beside the logo or in the row under it; the release-note
                    // card further down names the version too.
                    let hits: Vec<(u16, u16)> = rows
                        .iter()
                        .enumerate()
                        .take((logo.cell_y + logo.cell_h + 1) as usize)
                        .filter_map(|(y, r)| {
                            r.find(&version)
                                .map(|i| (r[..i].chars().count() as u16, y as u16))
                        })
                        .collect();
                    let Some(&(vx, vy)) = hits.first() else {
                        continue;
                    };
                    assert_eq!(hits.len(), 1, "one version at {w}x{h}");
                    let x1 = vx + version.chars().count() as u16 + 1;
                    let boxed = !loch && vx >= 2 && buf[(vx - 2, vy)].symbol() == "\u{2502}";
                    let pill = loch && vx >= 2 && buf[(vx - 2, vy)].symbol() == "\u{2590}";
                    let (x0, x1, y0, y1) = if boxed {
                        seen_badge = true;
                        for (x, y, corner) in [
                            (vx - 2, vy - 1, "\u{256d}"),
                            (x1, vy - 1, "\u{256e}"),
                            (vx - 2, vy + 1, "\u{2570}"),
                            (x1, vy + 1, "\u{256f}"),
                        ] {
                            assert_eq!(buf[(x, y)].symbol(), corner, "badge corner at {w}x{h}");
                        }
                        (vx - 2, x1, vy - 1, vy + 1)
                    } else if pill {
                        seen_badge = true;
                        assert_eq!(buf[(x1, vy)].symbol(), "\u{258c}", "pill cap at {w}x{h}");
                        for y in [vy.saturating_sub(1), vy + 1] {
                            for x in vx - 2..=x1 {
                                assert!(
                                    !"\u{256d}\u{256e}\u{2570}\u{256f}\u{2500}\u{2502}"
                                        .contains(buf[(x, y)].symbol()),
                                    "Croft Loch draws no box round the version at {w}x{h}"
                                );
                            }
                        }
                        (vx - 2, x1, vy, vy)
                    } else {
                        seen_below = true;
                        (vx, vx + version.chars().count() as u16 - 1, vy, vy)
                    };
                    let logo_x1 = logo.cell_x + logo.cell_w - 1;
                    let logo_y1 = logo.cell_y + logo.cell_h - 1;
                    assert!(
                        x1 < logo.cell_x || x0 > logo_x1 || y1 < logo.cell_y || y0 > logo_y1,
                        "the version overlaps the logo at {w}x{h}"
                    );
                    if let Some(t) = rows.iter().position(|r| r.contains(tagline)) {
                        let t = t as u16;
                        assert!(
                            t < y0 || t > y1,
                            "the tagline crosses the version at {w}x{h}"
                        );
                    }
                }
            }
        }
        assert!(seen_badge, "the badge beside the logo must be exercised");
        assert!(
            seen_below,
            "the version under a narrow logo must be exercised"
        );
    }
}

#[test]
fn welcome_logo_png_asset_is_present_and_decodable() {
    // Regression guard for the welcome screen: the bundled logo asset
    // must remain a valid PNG so the OSC-1337 path can bake it. If
    // someone replaces it with a half-block fallback or empties the
    // asset, this catches it on every CI run.
    let bytes = crate::iterm2_inline::WELCOME_LOGO_PNG;
    assert!(bytes.len() > 1024, "welcome logo asset suspiciously small");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG file");
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .expect("welcome logo PNG must decode");
    assert!(img.width() > 0 && img.height() > 0);
}

#[test]
fn welcome_image_bake_produces_osc1337_carrying_logo_pixels() {
    // End-to-end regression for the welcome render path: feeding the
    // bundled logo PNG through the same `fit_image` + `build_inline_image_osc`
    // pipeline `render_welcome` uses must yield an OSC-1337 sequence
    // that (a) starts with the iTerm2 introducer, (b) advertises the
    // requested cell dimensions, and (c) carries an opaque canvas
    // filled with `EDITOR_BG_RGB`. If anyone swaps OSC-1337 for a
    // half-block or text fallback, this test will refuse to compile
    // or fail the byte assertions.
    let canvas_w = 48u32 * 8; // approx welcome cell w * cell pixel w
    let canvas_h = 14u32 * 16;
    let bg_rgb = crate::theme::Theme::DARK_BLUE.editor_bg_rgb();
    let bg = image::Rgba([bg_rgb.0, bg_rgb.1, bg_rgb.2, 0xff]);
    let baked = crate::iterm2_inline::fit_image(
        crate::iterm2_inline::WELCOME_LOGO_PNG,
        canvas_w,
        canvas_h,
        bg,
    )
    .expect("baked welcome PNG");
    assert_eq!(
        &baked[..8],
        b"\x89PNG\r\n\x1a\n",
        "fit_image must emit a PNG"
    );
    let osc = crate::iterm2_inline::build_inline_image_osc(&baked, 48, 14, false);
    assert!(osc.starts_with("\x1b]1337;File=inline=1"));
    assert!(osc.ends_with('\x07'));
    assert!(osc.contains("width=48"));
    assert!(osc.contains("height=14"));

    // Verify the canvas corner pixels are exactly the dark-blue theme bg so
    // the welcome image bg matches the sRGB-decoded session bg.
    let img = image::load_from_memory_with_format(&baked, image::ImageFormat::Png)
        .unwrap()
        .to_rgba8();
    for &(x, y) in &[
        (0u32, 0u32),
        (canvas_w - 1, 0),
        (0, canvas_h - 1),
        (canvas_w - 1, canvas_h - 1),
    ] {
        let p = img.get_pixel(x, y);
        assert_eq!(
            (p.0[0], p.0[1], p.0[2], p.0[3]),
            (bg_rgb.0, bg_rgb.1, bg_rgb.2, 0xff),
            "canvas corner ({x}, {y}) must be opaque editor bg"
        );
    }
}

/// The cells of `area` in `buf`, as text.
fn cells_text(buf: &ratatui::buffer::Buffer, area: crate::app::WelcomeLayout) -> String {
    let mut text = String::new();
    for y in area.cell_y..area.cell_y + area.cell_h {
        for x in area.cell_x..area.cell_x + area.cell_w {
            text.push_str(buf[(x, y)].symbol());
        }
    }
    text
}

/// #1973: the frame that lays the welcome screen out never waits for its
/// logo. The bake runs on a thread of its own, the logo's cells stay blank
/// rather than flashing the text label, and the picture goes up once it
/// lands.
#[test]
fn the_welcome_logo_bakes_off_the_ui_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.cell_pixel = Some((8, 16));
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.render_welcome(f, f.area())).unwrap();
    let logo = *app.overlays.welcome.layout().expect("laid out");
    assert!(
        app.overlays.welcome.image_is_none(),
        "the frame waited for the logo bake"
    );
    let text = cells_text(term.backend().buffer(), logo);
    assert!(
        !text.contains("croft") && !text.contains("cr<>ft"),
        "the text label flashed where the logo is about to go"
    );
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the welcome logo bake to land",
        || app.poll_welcome_bake(),
    );
    let osc = app.overlays.welcome.image().expect("the logo is up");
    assert!(
        osc.contains(&format!("c={},r={}", logo.cell_w, logo.cell_h)),
        "the logo was baked for another layout"
    );
    assert!(app.overlays.welcome.is_dirty(), "the logo is due on screen");
}

/// A bake for a layout the welcome screen has since left is dropped: only
/// the picture for the layout on screen ever goes up.
#[test]
fn a_welcome_logo_baked_for_an_old_layout_never_goes_up() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.cell_pixel = Some((8, 16));
    let mut wide = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 50)).unwrap();
    wide.draw(|f| app.render_welcome(f, f.area())).unwrap();
    let old = *app.overlays.welcome.layout().expect("laid out");
    let mut narrow = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 30)).unwrap();
    narrow.draw(|f| app.render_welcome(f, f.area())).unwrap();
    let new = *app.overlays.welcome.layout().expect("laid out");
    assert_ne!(old, new, "the two sizes must lay the logo out apart");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the welcome logo bake to land",
        || app.poll_welcome_bake(),
    );
    let osc = app.overlays.welcome.image().expect("the logo is up");
    assert!(
        osc.contains(&format!("c={},r={}", new.cell_w, new.cell_h)),
        "a logo baked for the old layout went up"
    );
}

/// A logo that cannot be baked at all (no image protocol to send it with)
/// falls back to the text label: the plain name on Croft Black, the
/// `cr<>ft` wordmark on Croft Loch, its `<>` in the brand orange.
#[test]
fn a_welcome_logo_that_fails_to_bake_falls_back_to_the_label() {
    for (theme, label) in [
        (crate::theme::Theme::BLACK, "croft"),
        (crate::theme::Theme::LOCH, "cr<>ft"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.apply_theme(theme);
        app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::None;
        app.cell_pixel = Some((8, 16));
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.render_welcome(f, f.area())).unwrap();
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(500),
            "the welcome logo bake to land",
            || app.poll_welcome_bake(),
        );
        assert!(app.overlays.welcome.image_is_none());
        term.draw(|f| app.render_welcome(f, f.area())).unwrap();
        let logo = *app.overlays.welcome.layout().expect("laid out");
        let buf = term.backend().buffer();
        let text = cells_text(buf, logo);
        assert!(text.contains(label), "{label:?} in {text:?}");
        if theme.surfaces().is_some() {
            let at = text.find("<>").unwrap();
            let (x, y) = (
                logo.cell_x + (at as u16 % logo.cell_w),
                logo.cell_y + (at as u16 / logo.cell_w),
            );
            assert_eq!(buf[(x, y)].fg, crate::theme::BRAND_ORANGE);
        }
    }
}

#[test]
fn consume_welcome_image_clear_fires_once_when_editor_opens_a_file() {
    // Repro for the "logo bleeds through under an open file" bug:
    // after the welcome OSC-1337 has been written to iTerm, opening a
    // file must signal a one-shot screen clear so ratatui repaints
    // every cell on the next draw and iTerm's image cache for those
    // cells is wiped.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(true);
    let f = tmp.path().join("hi.txt");
    std::fs::write(&f, "hi").unwrap();
    app.editor.open_pinned(&f).unwrap();
    assert!(app.consume_welcome_image_clear(), "first call must fire");
    assert!(
        !app.overlays.welcome.is_displayed(),
        "flag must reset after consume"
    );
    assert!(
        !app.consume_welcome_image_clear(),
        "second call must be a no-op until welcome is re-shown"
    );
}

#[test]
fn consume_welcome_image_clear_is_noop_while_editor_pane_is_blank() {
    // The image is still meant to be visible, so don't clear.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(true);
    assert!(!app.consume_welcome_image_clear());
    assert!(
        app.overlays.welcome.is_displayed(),
        "flag must NOT reset while image is still meant to show"
    );
}

#[test]
fn consume_welcome_image_clear_handles_reposition_request_while_blank() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(true);
    app.overlays.welcome.request_clear();

    assert!(app.consume_welcome_image_clear());
    assert!(!app.overlays.welcome.is_displayed());
    assert!(!app.overlays.welcome.clear_requested());
}

#[test]
fn consume_welcome_image_clear_is_noop_when_image_was_never_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays.welcome.set_displayed(false);
    let f = tmp.path().join("hi.txt");
    std::fs::write(&f, "hi").unwrap();
    app.editor.open_pinned(&f).unwrap();
    assert!(!app.consume_welcome_image_clear());
}

#[test]
fn welcome_paints_a_full_bordered_box_around_the_editor_area() {
    // Without a visible envelope, users on the welcome page can't
    // perceive that the explorer is resizable and can't see where the
    // editor pane ends. Regression guard: the welcome render must
    // paint a full Borders::ALL frame around `area` so all four edges
    // are perceptible. On a boxed theme: Croft Loch's seams do that job.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.apply_theme(crate::theme::Theme::BLACK);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let area = ratatui::layout::Rect::new(40, 0, 80, 40);
    term.draw(|f| app.render_welcome(f, area)).unwrap();
    let buf = term.backend().buffer();
    let left = area.x;
    let right = area.x + area.width - 1;
    let top = area.y;
    let bot = area.y + area.height - 1;
    // Vertical edges: left and right columns must be vertical bars on
    // most rows (corners use ─/└/┌/┘).
    let mut left_bars = 0;
    let mut right_bars = 0;
    for y in (top + 1)..bot {
        if buf[(left, y)].symbol() == "│" {
            left_bars += 1;
        }
        if buf[(right, y)].symbol() == "│" {
            right_bars += 1;
        }
    }
    let inner_h = (bot - top - 1) as usize;
    assert!(
        left_bars >= inner_h / 2,
        "left edge underpainted: {left_bars}"
    );
    assert!(
        right_bars >= inner_h / 2,
        "right edge underpainted: {right_bars}"
    );
    // Horizontal edges: top and bottom rows must be horizontal bars on
    // most columns.
    let mut top_bars = 0;
    let mut bot_bars = 0;
    for x in (left + 1)..right {
        if buf[(x, top)].symbol() == "─" {
            top_bars += 1;
        }
        if buf[(x, bot)].symbol() == "─" {
            bot_bars += 1;
        }
    }
    let inner_w = (right - left - 1) as usize;
    assert!(top_bars >= inner_w / 2, "top edge underpainted: {top_bars}");
    assert!(
        bot_bars >= inner_w / 2,
        "bottom edge underpainted: {bot_bars}"
    );
    // Corners.
    assert_eq!(buf[(left, top)].symbol(), "┌");
    assert_eq!(buf[(right, top)].symbol(), "┐");
    assert_eq!(buf[(left, bot)].symbol(), "└");
    assert_eq!(buf[(right, bot)].symbol(), "┘");
}

/// Croft Loch draws the welcome page the way it draws an open editor: its
/// frame is padding on the canvas, not a line box, and the page inside it
/// is drawn in seams and surfaces too: no box-drawing glyph anywhere, the
/// release card a raised surface rather than the gradient box.
#[test]
fn croft_loch_welcome_is_padding_on_the_canvas_with_the_boxed_inset() {
    let tmp = tempfile::tempdir().unwrap();
    let area = Rect::new(40, 0, 80, 40);
    let render = |theme: crate::theme::Theme| {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.apply_theme(theme);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render_welcome(f, area)).unwrap();
        term.backend().buffer().clone()
    };
    let black = render(crate::theme::Theme::BLACK);
    let loch = render(crate::theme::Theme::LOCH);
    let cells = |r: Rect| {
        (r.top()..r.bottom()).flat_map(move |y| (r.left()..r.right()).map(move |x| (x, y)))
    };
    let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
    let frame: Vec<(u16, u16)> = cells(area).filter(|&p| !inner.contains(p.into())).collect();
    let lines = |b: &ratatui::buffer::Buffer| {
        frame
            .iter()
            .filter(|&&p| "─│┌┐└┘".contains(b[p].symbol()))
            .count()
    };
    assert!(lines(&black) > 0, "Croft Black keeps its box");
    assert_eq!(lines(&loch), 0, "no line box around Croft Loch's welcome");
    // Below the top row, which carries the layout icons, the frame is blank.
    for &(x, y) in frame.iter().filter(|p| p.1 > area.y) {
        assert_eq!(loch[(x, y)].symbol(), " ", "padding at ({x}, {y})");
    }
    for (x, y) in cells(inner) {
        assert!(
            !"─│┌┐└┘╭╮╰╯".contains(loch[(x, y)].symbol()),
            "a line-box glyph at ({x}, {y}) on Croft Loch's welcome"
        );
    }
}

/// Croft Loch's release card: a raised surface with a quiet rim, headed
/// "What's new" in the strong ink, bold (a heading outranks its notes), with
/// the version right-aligned in the muted ink, its notes marked `+`
/// (feature) and `•` (fix) in the muted ink rather than a rocket and a bug.
#[test]
fn croft_loch_release_card_is_a_raised_surface_with_quiet_marks() {
    let t = crate::widgets::surface::tones(crate::theme::Theme::LOCH).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(vec![
        note_fixture(crate::release_notes::NoteKind::Feature, "Did a new thing"),
        note_fixture(crate::release_notes::NoteKind::Fix, "Fixed an old thing"),
    ]);
    let area = Rect::new(0, 0, 120, 44);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 44)).unwrap();
    term.draw(|f| app.render_welcome(f, area)).unwrap();
    let buf = term.backend().buffer();
    let find = |needle: &str| -> (u16, u16) {
        for y in 0..area.height {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(i) = row.find(needle) {
                return (row[..i].chars().count() as u16, y);
            }
        }
        panic!("{needle} not on screen");
    };
    let (hx, hy) = find("What's new");
    assert_eq!(
        buf[(hx, hy)].fg,
        t.strong(),
        "the heading outranks the notes"
    );
    assert!(buf[(hx, hy)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(hx, hy)].bg, t.raised(), "on the raised card");
    let header: String = (0..area.width).map(|x| buf[(x, hy)].symbol()).collect();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let vx = header
        .find(&version)
        .expect("the version on the header row") as u16;
    assert!(vx > hx + 10, "the version right-aligned on the header");
    assert_eq!(buf[(vx, hy)].fg, t.muted());
    let (fx, fy) = find("Did a new thing");
    assert_eq!(buf[(fx - 2, fy)].symbol(), "+");
    assert_eq!(buf[(fx - 2, fy)].fg, t.muted(), "a muted mark, not teal");
    assert_eq!(buf[(fx, fy)].fg, t.text());
    let (bx, by) = find("Fixed an old thing");
    assert_eq!(buf[(bx - 2, by)].symbol(), "\u{2022}");
    assert_eq!(buf[(bx - 2, by)].fg, t.muted());
    for (x, y) in (0..area.height).flat_map(|y| (0..area.width).map(move |x| (x, y))) {
        let s = buf[(x, y)].symbol();
        assert!(
            s != "\u{f135}" && s != "\u{f188}",
            "no rocket or bug at ({x},{y})"
        );
    }
    // The tagline is the calm one, muted and italic.
    let (tx, ty) = find(App::WELCOME_TAGLINE_CALM);
    assert_eq!(buf[(tx, ty)].fg, t.muted());
    assert!(buf[(tx, ty)].modifier.contains(Modifier::ITALIC));
}

/// Croft Loch's welcome offers a Start list when the window has the
/// height: each action with its chord as keycaps (the palette's own chord
/// text), and a click runs it. The tour rides along as a row.
#[test]
fn croft_loch_welcome_start_list_shows_keycaps_and_runs_on_click() {
    let tmp = tempfile::tempdir().unwrap();
    let area = Rect::new(0, 0, 120, 50);
    let draw = |app: &mut App| {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 50)).unwrap();
        term.draw(|f| app.render_welcome(f, area)).unwrap();
        term.backend().buffer().clone()
    };
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(long_test_note());
    app.tour_done = false;
    let buf = draw(&mut app);
    let rows: Vec<String> = (0..area.height)
        .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    let row_of = |needle: &str| rows.iter().position(|r| r.contains(needle));
    let start = row_of("Start").expect("the Start heading");
    let new_file = row_of("New File").expect("a New File row");
    let card = row_of("deliberately").expect("the card is whole");
    assert!(
        start < new_file && new_file < card,
        "Start sits above the card"
    );
    assert!(rows[card..].iter().any(|r| r.contains("cardclip-endcap.")));
    let quick = crate::widgets::command_palette::platform_hint("Cmd+P");
    let (_, key) = quick.split_once('+').unwrap();
    let go_to = row_of("Go to File").unwrap();
    assert!(
        rows[go_to].contains(&format!(" {key} ")),
        "the chord as keycaps: {:?}",
        rows[go_to]
    );
    // The tour is a row of the list, and its click target.
    let tour = row_of("Take the Tour").unwrap();
    assert_eq!(app.welcome_tour_button.y, tour as u16);
    assert_eq!(
        rows.iter().filter(|r| r.contains("Take the Tour")).count(),
        1
    );

    let rects = app.welcome.start_rows().to_vec();
    let palette = rects
        .iter()
        .find(|(_, a)| *a == crate::app::welcome::StartAction::CommandPalette)
        .unwrap()
        .0;
    assert!(app.click_welcome_start(palette.x + 3, palette.y));
    assert!(app.command_palette.is_some(), "the palette opened");
    app.close_all_modals_for_test();
    let go = rects
        .iter()
        .find(|(_, a)| {
            *a == crate::app::welcome::StartAction::Command(
                crate::widgets::command_palette::Command::QuickOpen,
            )
        })
        .unwrap()
        .0;
    assert!(app.click_welcome_start(go.x + 3, go.y));
    assert!(app.file_finder.is_some(), "Go to File opened the finder");
    assert!(
        !app.click_welcome_start(go.x + 3, go.y + 30),
        "not on a row"
    );
}

/// Croft Loch's Start list pairs each chord with its label: the list hangs
/// from the card's text column, its keycaps in one column four cells past
/// the widest label, and it is only as wide as that, not the card's whole
/// column. Each row's lift and click rect is the painted list and a cell
/// either side, nothing more. "Start" is a heading in the strong ink, bold.
#[test]
fn croft_loch_start_keycaps_sit_in_one_column_beside_the_labels() {
    let t = crate::widgets::surface::tones(crate::theme::Theme::LOCH).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(long_test_note());
    app.tour_done = false;
    let (w, h) = (120u16, 50u16);
    let draw = |app: &mut App| {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render_welcome(f, Rect::new(0, 0, w, h)))
            .unwrap();
        term.backend().buffer().clone()
    };
    let buf = draw(&mut app);
    // One char per cell, so a char index is a column.
    let rows: Vec<String> = (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect()
        })
        .collect();
    let row_of = |needle: &str| {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} on screen:\n{}", rows.join("\n"))) as u16
    };
    let col = |y: u16, needle: &str| -> u16 {
        let r = &rows[y as usize];
        r[..r.find(needle).unwrap()].chars().count() as u16
    };
    let start = row_of("Start   ");
    let x0 = col(start, "Start");
    assert_eq!(
        buf[(x0, start)].fg,
        t.strong(),
        "the heading outranks its rows"
    );
    assert!(buf[(x0, start)].modifier.contains(Modifier::BOLD));
    let card = row_of("What's new");
    assert_eq!(col(card, "What's new"), x0, "the card's text column");

    let widest = "Command Palette".len() as u16;
    let keys_x = x0 + widest + 4;
    let cap_bg = t.keycap_on(t.canvas());
    for label in ["New File", "Go to File", "Command Palette"] {
        let y = row_of(label);
        assert_eq!(col(y, label), x0, "{label} on the list's edge");
        let first_cap = (x0..w).find(|&x| buf[(x, y)].bg == cap_bg).unwrap();
        assert_eq!(first_cap, keys_x, "{label}'s keycaps in the one column");
        let label_end = x0 + label.len() as u16;
        assert!(
            (label_end..keys_x).all(|x| buf[(x, y)].symbol() == " "),
            "only the gap between {label} and its keycaps"
        );
    }

    // Only as wide as it needs: the row rects end a cell past the last
    // painted keycap, well inside the card's text column.
    let painted_right = app
        .welcome
        .start_rows()
        .iter()
        .map(|(r, _)| {
            (x0..w)
                .rev()
                .find(|&x| buf[(x, r.y)].symbol() != " " || buf[(x, r.y)].bg == cap_bg)
                .unwrap()
                + 1
        })
        .max()
        .unwrap();
    let card_version = col(card, &format!("v{}", env!("CARGO_PKG_VERSION")));
    assert!(
        painted_right + 10 < card_version,
        "the list hugs its chords"
    );
    let mut rects: Vec<Rect> = app.welcome.start_rows().iter().map(|(r, _)| *r).collect();
    rects.push(app.welcome_tour_button);
    for r in &rects {
        assert_eq!(
            (r.x, r.right()),
            (x0 - 1, painted_right + 1),
            "a row's rect is the painted list and a cell either side"
        );
    }

    // The lift is the rect: its last cell lifts, the cell past it does not,
    // and a click lands exactly where the lift is.
    let palette = app
        .welcome
        .start_rows()
        .iter()
        .find(|(_, a)| *a == crate::app::welcome::StartAction::CommandPalette)
        .unwrap()
        .0;
    app.pointer_cell = Some((palette.right() - 1, palette.y));
    let buf = draw(&mut app);
    let lift = t.hover_on(t.canvas);
    assert_eq!(buf[(palette.x, palette.y)].bg, lift);
    assert_eq!(buf[(palette.right() - 1, palette.y)].bg, lift);
    assert_eq!(buf[(palette.right(), palette.y)].bg, t.canvas());
    assert!(!app.click_welcome_start(palette.right(), palette.y));
    assert!(app.click_welcome_start(palette.right() - 1, palette.y));
    assert!(app.command_palette.is_some(), "the palette opened");
}

/// The Start list's columns: the keycaps a fixed gap past the widest label,
/// and the list as wide as the labels and keycaps, or the labels alone.
#[test]
fn welcome_start_columns_put_the_keycaps_a_gap_past_the_widest_label() {
    assert_eq!(welcome_start_columns(15, 20), (19, 39));
    assert_eq!(welcome_start_columns(13, 0), (17, 13));
}

/// Five notes for the Croft Loch layout tests, each wrapping to a few rows
/// in a card and ending in a token of its own, so a test can tell which
/// notes the card shows and that each one it shows is whole.
fn five_test_notes() -> Vec<crate::release_notes::ReleaseNote> {
    use crate::release_notes::{NoteKind, ReleaseNote};
    [
        "The first highlight is long enough to wrap onto a second row of the card \
         at the common widths, and it ends in its token: note-one-end.",
        "The second highlight wraps the same way, a sentence or two that a release \
         card carries for a change worth naming: note-two-end.",
        "A third, shorter one: note-three-end.",
        "The fourth highlight wraps again, a line about a fix the card lists in \
         the muted ink like any other: note-four-end.",
        "And a fifth to round it off: note-five-end.",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, summary)| ReleaseNote {
        kind: if i == 3 {
            NoteKind::Fix
        } else {
            NoteKind::Feature
        },
        summary,
    })
    .collect()
}

/// The tokens [`five_test_notes`] end in, in order.
const FIVE_NOTE_ENDS: [&str; 5] = [
    "note-one-end.",
    "note-two-end.",
    "note-three-end.",
    "note-four-end.",
    "note-five-end.",
];

/// Croft Loch's welcome keeps the Start list before the release card: in a
/// short editor the card gives up notes, whole ones, and says how many with
/// a "+N more" line, rather than the Start list giving way. The notes it
/// keeps are the first ones, each ending inside the card.
#[test]
fn croft_loch_release_card_shortens_before_the_start_list_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(five_test_notes());
    app.tour_done = false;
    let mut seen_short = false;
    for h in 8..=50 {
        let area = Rect::new(0, 0, 120, h);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, h)).unwrap();
        term.draw(|f| app.render_welcome(f, area)).unwrap();
        let buf = term.backend().buffer();
        let text: String = (0..h)
            .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        // Wordmark (1), tagline (2), Start heading and four rows (6): the
        // Start list shows from 9 inner rows, the frame's two padding rows
        // on top.
        assert_eq!(
            text.contains("New File"),
            h >= 11,
            "the Start list shows whenever it fits, at height {h}"
        );
        let shown = FIVE_NOTE_ENDS
            .iter()
            .take_while(|end| text.contains(*end))
            .count();
        assert!(
            FIVE_NOTE_ENDS[shown..]
                .iter()
                .all(|end| !text.contains(end)),
            "the card keeps the first notes at height {h}"
        );
        if text.contains("What's new") {
            assert!(shown > 0, "a card shows at least one note whole at {h}");
            assert!(
                text.contains("New File"),
                "the card never shows without the Start list at {h}"
            );
            let more = format!("+{} more", 5 - shown);
            assert_eq!(
                text.contains(&more),
                shown < 5,
                "the card says what it left out at height {h}"
            );
            seen_short |= shown < 5;
        } else {
            assert_eq!(shown, 0, "no note outside a card at height {h}");
        }
    }
    assert!(seen_short, "a shortened card must be exercised");
}

/// The Croft Loch welcome plan's priorities: the wordmark, the tagline and
/// the Start list first, the card shortened to whole notes after them, a
/// tour chip where the Start list cannot fit, and the picture logo growing
/// only once every note fits. Blocks sit one row apart, centred.
#[test]
fn croft_loch_welcome_plan_keeps_its_priorities() {
    // A 21-row editor: wordmark, tagline, Start (heading and four rows),
    // then a card of two whole notes and the "+N more" row: 21 rows.
    let plan = loch_welcome_plan(21, true, 4, true, &[3, 3, 2, 3, 2]);
    assert_eq!(plan.logo_y, 0);
    assert_eq!(plan.logo_h, 1);
    assert_eq!(plan.tagline_y, 2);
    assert_eq!(plan.start_y, Some(4));
    assert_eq!(plan.chip_y, None);
    assert_eq!((plan.card_y, plan.card_h, plan.notes), (10, 11, 2));
    // Too short for any note: the card goes, the rest is centred.
    let plan = loch_welcome_plan(14, true, 4, true, &[3, 3]);
    assert_eq!(plan.notes, 0);
    assert_eq!(plan.logo_y, 2, "nine rows centred in fourteen");
    // No room for the Start list: the tour chip takes its place.
    let plan = loch_welcome_plan(6, true, 4, true, &[3]);
    assert_eq!((plan.start_y, plan.chip_y), (None, Some(4)));
    // Every note fits: the picture grows, capped at fourteen rows.
    let plan = loch_welcome_plan(40, false, 4, true, &[3, 3]);
    assert_eq!(plan.logo_h, 14);
    assert_eq!(plan.notes, 2);
    assert_eq!(plan.logo_y, (40 - (14 + 2 + 6 + 12)) / 2);
    // Not every note fits: the picture keeps its floor and the card gives
    // up a note.
    let plan = loch_welcome_plan(23, false, 4, true, &[3, 3]);
    assert_eq!((plan.logo_h, plan.notes), (4, 1));
}

/// Croft Loch's wordmark carries the version as a pill two cells after it,
/// the pair centred as one group on the page's axis, at every width; the
/// logo's cells are the wordmark's, so nothing else lands on them.
#[test]
fn croft_loch_version_pill_sits_two_cells_after_the_wordmark() {
    let tmp = tempfile::tempdir().unwrap();
    let version = format!(" v{} ", env!("CARGO_PKG_VERSION"));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for w in (30..=200).step_by(7) {
        let area = Rect::new(0, 0, w, 36);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 36)).unwrap();
        term.draw(|f| app.render_welcome(f, area)).unwrap();
        let buf = term.backend().buffer();
        let logo = *app.overlays.welcome.layout().expect("laid out");
        assert_eq!((logo.cell_w, logo.cell_h), (6, 1), "the wordmark's cells");
        let row: String = (0..w).map(|x| buf[(x, logo.cell_y)].symbol()).collect();
        let mark_x = row.find("cr<>ft").expect("the wordmark") as u16;
        assert_eq!(mark_x, logo.cell_x);
        let cap = mark_x + 6 + 2;
        assert_eq!(buf[(cap - 1, logo.cell_y)].symbol(), " ", "a 2-cell gap");
        assert_eq!(
            buf[(cap, logo.cell_y)].symbol(),
            "\u{2590}",
            "the pill's cap"
        );
        let label: String = (cap + 1..cap + 1 + version.len() as u16)
            .map(|x| buf[(x, logo.cell_y)].symbol())
            .collect();
        assert_eq!(label, version, "the version inside the pill at {w}");
        let end = cap + 1 + version.len() as u16;
        assert_eq!(buf[(end, logo.cell_y)].symbol(), "\u{258c}");
        // One group on the axis: as much room left of the wordmark as
        // right of the pill, give or take the odd cell.
        let (inner_l, inner_r) = (area.x + 1, area.right() - 1);
        let (left, right) = (mark_x - inner_l, inner_r - (end + 1));
        assert!(
            left.abs_diff(right) <= 1,
            "centred at {w}: {left} vs {right}"
        );
    }
}

/// At the screenshots' 140x36 with the side bar and the terminal panel open
/// (an editor about twenty rows tall), Croft Loch's welcome shows the whole
/// stack: the wordmark and version, the tagline, the Start list with the
/// tour as its last row, and a release card shortened to whole notes with a
/// "+N more" line, its blocks one blank row apart, centred on one axis but
/// for the Start list, which hangs from the card's text column. The Start
/// rows answer the mouse where they are painted.
#[test]
fn croft_loch_welcome_fits_a_short_editor_with_the_start_list() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(five_test_notes());
    app.tour_done = false;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 36)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    // One char per cell, so a char index is a column.
    let rows: Vec<String> = (0..36)
        .map(|y| {
            (0..140)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect()
        })
        .collect();
    let col = |y: usize, needle: &str| -> u16 {
        let r = &rows[y];
        r[..r.find(needle).unwrap()].chars().count() as u16
    };
    let row_of = |needle: &str| {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} on screen:\n{}", rows.join("\n")))
    };
    let mark = row_of("cr<>ft  \u{2590}");
    let tagline = row_of(App::WELCOME_TAGLINE_CALM);
    let start = row_of("Start   ");
    let tour = row_of("Take the Tour");
    let card = row_of("What's new");
    for label in ["New File", "Go to File", "Command Palette"] {
        let y = row_of(label);
        assert!(start < y && y < tour, "{label} in the Start list");
    }
    assert_eq!(
        col(start, "Start"),
        col(card, "What's new"),
        "the Start list sits in the card's text column"
    );
    assert_eq!(tagline, mark + 2, "one blank row under the wordmark");
    assert_eq!(start, tagline + 2, "one blank row under the tagline");
    assert_eq!(tour, start + 4, "the tour is the list's last row");
    // The card's top edge sits one blank row under the list.
    assert_eq!(
        card,
        tour + 3,
        "one blank row between the list and the card"
    );
    let shown = FIVE_NOTE_ENDS
        .iter()
        .filter(|end| rows.iter().any(|r| r.contains(*end)))
        .count();
    assert!((1..5).contains(&shown), "a shortened card: {shown} notes");
    row_of(&format!("+{} more", 5 - shown));

    // One axis: the tagline and the card centre on the same column as the
    // wordmark-and-version group (compared doubled, as first plus last
    // column, so an odd width needs no rounding). The Start list hangs from
    // the card's text column, as asserted above.
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let pill_end = col(mark, &version) + version.len() as u16 + 1;
    let axis = col(mark, "cr<>ft") + pill_end;
    let tagline_axis = col(tagline, "Lightweight") + col(tagline, "developers.") + 10;
    let card_version = col(card, &version) + version.len() as u16 - 1;
    let card_end = card_version + u16::from(rows[card][..].contains(&format!("{version}+")));
    let card_axis = col(card, "What's new") + card_end;
    for (what, c) in [("tagline", tagline_axis), ("card", card_axis)] {
        assert!(c.abs_diff(axis) <= 2, "the {what} centred: {c} vs {axis}");
    }
    let go = app
        .welcome
        .start_rows()
        .iter()
        .find(|(_, a)| {
            *a == crate::app::welcome::StartAction::Command(
                crate::widgets::command_palette::Command::QuickOpen,
            )
        })
        .unwrap()
        .0;

    // The Start rows' hit rects are the painted rows: a real click on Go
    // to File opens the finder, and the tour row is the tour's button.
    assert_eq!(
        go.x,
        col(start, "Start") - 1,
        "a cell of lift left of the list"
    );
    assert_eq!(go.y as usize, row_of("Go to File"));
    assert_eq!(app.welcome_tour_button.y as usize, tour);
    left_click(&mut app, go.x + 2, go.y);
    assert!(app.file_finder.is_some(), "Go to File opened the finder");
}

/// A long fixed note for the welcome-card tests: the live notes change with
/// every release (`src/release_notes/<version>.md`, or the pending fragments in
/// `src/release_notes/unreleased/`), so pinning against them would let a future
/// one-line note quietly gut these tests. The distinctive final word is the
/// clip detector.
fn long_test_note() -> Vec<crate::release_notes::ReleaseNote> {
    vec![crate::release_notes::ReleaseNote {
        kind: crate::release_notes::NoteKind::Fix,
        summary: "A deliberately long highlight that wraps across several rows at \
                  every sane width so the card's height measure and its paint pass \
                  are both exercised, ending in a distinctive final token that no \
                  other part of the interface paints: cardclip-endcap.",
    }]
}

#[test]
fn welcome_release_note_never_clips_its_final_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(long_test_note());
    // Give the card room: with the sidebar and panel up, narrow widths
    // height-starve the welcome area and the card clamps legitimately —
    // that clamp is not what this test pins. Full-window welcome isolates
    // the wrap arithmetic.
    app.show_tree = false;
    app.show_terminal = false;
    // Sweep widths: the card clips only where the measure-time wrap and the
    // paint-time wrap disagree on the line count, which is width-dependent.
    for width in (60u16..=180).step_by(2) {
        let backend = ratatui::backend::TestBackend::new(width, 50);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buf = term.backend().buffer().clone();
        let a = buf.area;
        let text: String = (a.y..a.y + a.height)
            .map(|y| {
                (a.x..a.x + a.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Skip widths where the card itself is absent (too narrow for
        // the welcome stack) — only a partially painted card is a bug.
        if !text.contains("deliberately") {
            continue;
        }
        assert!(
            text.contains("cardclip-endcap."),
            "at {width} cols the release-note card clips its final line \
             (measure and paint must wrap at the same width)"
        );
    }
}

#[test]
fn welcome_logo_shrinks_before_the_release_note_card_clips() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.welcome.set_test_notes(long_test_note());
    // Default layout — sidebar and panel visible — at a common window size.
    // The welcome area is height-starved here, and the logo must yield rows
    // (down to its 4-row minimum) before the card is allowed to clip: a
    // full-height logo above a release note cut off mid-sentence is the
    // wrong trade (#99, observed at 140x40).
    let backend = ratatui::backend::TestBackend::new(140, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    let a = buf.area;
    let text: String = (a.y..a.y + a.height)
        .map(|y| {
            (a.x..a.x + a.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("deliberately"),
        "precondition: the release-note card must paint at 140x40"
    );
    assert!(
        text.contains("cardclip-endcap."),
        "at 140x40 the card clips its final line while the logo still \
         occupies its maximum height — the logo must shrink first"
    );
}

#[test]
fn the_demo_tour_runs_in_a_scratch_project_and_esc_cleans_up() {
    // #377: the tour never runs in the user's workspace, and Esc leaves a
    // normal croft with no scratch files behind.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        let scratch = app.workspace_root().to_path_buf();
        assert_ne!(scratch, tmp.path(), "not the user's workspace");
        assert!(scratch.join(crate::tour::SCRATCH_MARKER).is_file());
        assert_eq!(
            app.editor.path.as_deref(),
            Some(scratch.join("src/main.rs").as_path())
        );
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 36)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buf = term.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                screen.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(
            screen.contains("1/8"),
            "the caption chip shows progress:\n{screen}"
        );
        // Enter advances the tour instead of typing into the file.
        let lines = app.editor.lines.clone();
        app.focus_pane(Pane::Editor);
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.tour.as_ref().unwrap().tour.index, 1);
        assert_eq!(app.editor.lines, lines, "no newline typed into the sample");
        app.close_all_modals_for_test();
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.tour.is_none(), "Esc leaves the tour");
        assert_eq!(
            app.workspace_root(),
            tmp.path(),
            "back in the original workspace"
        );
        assert!(!scratch.exists(), "the sample project is gone");
    });
}

#[test]
fn walking_the_whole_tour_ends_it_and_cleans_up() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        let scratch = app.workspace_root().to_path_buf();
        let mut steps = 0;
        while app.tour.is_some() && steps < 40 {
            app.close_all_modals_for_test();
            app.advance_tour();
            steps += 1;
        }
        assert!(app.tour.is_none(), "the tour ends");
        assert_eq!(steps, 8, "one advance per step");
        assert_eq!(app.workspace_root(), tmp.path());
        assert!(!scratch.exists());
    });
}

#[test]
fn the_welcome_tour_button_starts_the_tour_and_goes_away_once_done() {
    // #377: shown until the first completed or skipped tour, persisted.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.config_dir = tmp.path().join("config");
        app.tour_done = false;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let button = app.welcome_tour_button;
        assert!(button.width > 0, "the welcome panel offers the tour");
        left_click(&mut app, button.x + 1, button.y);
        assert!(app.tour.is_some(), "clicking starts it: {}", app.status);
        app.close_all_modals_for_test();
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.tour_done, "a skipped tour counts as done");
        let saved = crate::prefs::Prefs::load(&tmp.path().join("config").join("config.json"))
            .unwrap_or_default();
        assert!(saved.tour_done, "persisted in prefs");
        // Back on the welcome screen, the button is gone.
        while app.editor.tab_count() > 1 {
            app.editor.close_active();
        }
        app.editor.close_active();
        term.draw(|f| app.render(f)).unwrap();
        assert_eq!(app.welcome_tour_button.width, 0, "not offered again");
    });
}

#[test]
fn take_the_tour_is_a_palette_command() {
    use crate::widgets::command_palette::Command;
    assert_eq!(
        Command::from_id("help_take_the_tour"),
        Some(Command::TakeTheTour)
    );
    assert_eq!(Command::TakeTheTour.title(), "Help: Take the Tour");
}

/// Starting the tour while it runs keeps the first one: a second start
/// would record the first tour's scratch project as the way home, and the
/// user's own workspace would be lost when it ended.
#[test]
fn starting_the_tour_again_while_it_runs_keeps_the_way_home() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        let scratch = app.workspace_root().to_path_buf();
        app.run_command(crate::widgets::command_palette::Command::TakeTheTour);
        assert!(app.status.contains("already running"), "{}", app.status);
        assert_eq!(app.workspace_root(), scratch);
        app.finish_tour();
        assert_eq!(
            app.workspace_root(),
            tmp.path(),
            "back in the user's workspace"
        );
        assert!(!scratch.exists());
    });
}

/// #377, as a user takes it: `croft demo`, then Enter and nothing else.
/// Every popup a step opens takes its own Enter, and that Enter must do the
/// harmless thing the caption says (open app.py, open the theme picker,
/// keep the theme), never whatever sorts first (the marker file, or Move
/// Line Up into the sample). The tour then ends with the sample's files as
/// they were created, and no tab left naming a deleted file.
#[test]
fn the_whole_tour_takes_enter_only_and_leaves_nothing_behind() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        let scratch = app.workspace_root().to_path_buf();
        let mut opened = Vec::new();
        let mut presses = 0;
        let mut saw_theme_picker = false;
        while app.tour.is_some() && presses < 30 {
            if let Some(step) = app.tour.as_ref().and_then(|r| r.tour.current()) {
                // The sample, as created, at every step: nothing typed into it.
                for (rel, text) in crate::tour::sample_files() {
                    if let Ok(on_disk) = std::fs::read_to_string(scratch.join(rel)) {
                        assert_eq!(on_disk, text, "{rel} at {:?}", step.action);
                    }
                }
            }
            app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
                .unwrap();
            presses += 1;
            saw_theme_picker |= app.context_menu.is_some();
            if let Some(path) = app.editor.path.clone()
                && !opened.contains(&path)
            {
                opened.push(path);
            }
            let dirty: Vec<_> = std::iter::once(&app.editor)
                .chain(app.editor_layout.inactive_groups())
                .flat_map(|g| g.editors.iter())
                .filter(|e| e.dirty)
                .filter_map(|e| e.path.clone())
                .collect();
            assert!(dirty.is_empty(), "Enter #{presses} edited {dirty:?}");
        }
        assert!(app.tour.is_none(), "Enter alone finishes the tour");
        assert!(presses <= 12, "{presses} presses for an 8-step tour");
        assert!(
            app.context_menu.is_none() && app.command_palette.is_none(),
            "no picker the tour opened outlives it"
        );
        assert!(
            saw_theme_picker,
            "the palette's Enter opened the theme picker, and Enter closed it"
        );
        assert!(
            opened.contains(&scratch.join("app.py")),
            "Quick Open's Enter opened app.py: {opened:?}"
        );
        assert!(
            !opened.contains(&scratch.join(crate::tour::SCRATCH_MARKER)),
            "never the marker file: {opened:?}"
        );
        assert!(!scratch.exists(), "the sample project is gone");
        assert_eq!(app.workspace_root(), tmp.path());
        let left: Vec<_> = std::iter::once(&app.editor)
            .chain(app.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter_map(|e| e.path.clone())
            .filter(|p| p.starts_with(&scratch))
            .collect();
        assert!(left.is_empty(), "tabs naming the deleted sample: {left:?}");
    });
}

/// #863: at 80x24 every built-in step shows its whole caption and the keys
/// that move the tour. One clamped line of progress, caption and keys cut
/// the keys first, and the theme picker the palette step opens covered the
/// caption's start.
#[test]
fn every_tour_caption_shows_whole_with_its_keys_at_80_columns() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        // Draw, then read the caption panel back off the screen: every
        // row but the last is the wrapped caption, the last the keys.
        let mut check = |app: &mut App, when: &str| {
            let step = app.tour.as_ref().and_then(|r| r.tour.current()).unwrap();
            let caption = step.caption_text();
            let progress = app.tour.as_ref().unwrap().tour.progress();
            term.draw(|f| app.render(f)).unwrap();
            let buf = term.backend().buffer();
            let (panel, _) = app.tour_caption_panel(buf.area).expect("a panel");
            let row = |y: u16| {
                (panel.x..panel.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            };
            let body: Vec<String> = (panel.y..panel.bottom() - 1).map(row).collect();
            assert_eq!(
                words(&body.join(" ")),
                words(&caption),
                "{when}: the caption is cut: {body:#?}"
            );
            let keys = row(panel.bottom() - 1);
            assert!(
                keys.contains("Enter next \u{b7} Esc leave") && keys.contains(&progress),
                "{when}: the keys row lost its hint: {keys:?}"
            );
            assert!(body.len() <= 3, "{when}: {} caption rows", body.len());
            panel
        };
        let mut steps = 0;
        let mut saw_picker = false;
        while app.tour.is_some() && steps < 20 {
            let action = app
                .tour
                .as_ref()
                .unwrap()
                .tour
                .current()
                .unwrap()
                .action
                .clone();
            check(&mut app, &format!("{action:?}"));
            if matches!(action, crate::tour::TourAction::Palette(_)) {
                // Enter runs the typed command: the theme picker, a menu
                // standing the frame's height along the left.
                app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
                    .unwrap();
                assert!(app.context_menu.is_some(), "the theme picker opened");
                let panel = check(&mut app, "the theme picker");
                let menu = app.menu_rect().unwrap();
                assert!(
                    !panel.intersects(menu),
                    "the picker {menu:?} covers the caption {panel:?}"
                );
                saw_picker = true;
            }
            app.close_all_modals_for_test();
            app.advance_tour();
            steps += 1;
        }
        assert_eq!(steps, 8, "every built-in step was checked");
        assert!(saw_picker, "the theme picker step was checked");
    });
}

/// #377: Esc mid-tour closes the sample's tabs too, an edited one
/// included, so no save can write a scratch file back.
#[test]
fn leaving_the_tour_closes_the_samples_tabs_even_edited_ones() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        let scratch = app.workspace_root().to_path_buf();
        app.focus_pane(Pane::Editor);
        app.editor.insert_char('x');
        assert!(app.editor.dirty);
        app.split_editor();
        app.close_all_modals_for_test();
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.tour.is_none());
        assert!(!scratch.exists());
        let left: Vec<_> = std::iter::once(&app.editor)
            .chain(app.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter_map(|e| e.path.clone())
            .filter(|p| p.starts_with(&scratch))
            .collect();
        assert!(left.is_empty(), "tabs naming the deleted sample: {left:?}");
    });
}

/// #863 regression, drawn through base APIs only: at 80x24 each step of the
/// tour shows the keys that move it. The caption shared one clamped line
/// with the progress and the keys, and the keys were cut at every step.
#[test]
fn the_tour_keys_show_at_every_step_at_80_columns() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        let mut steps = 0;
        while app.tour.is_some() && steps < 20 {
            app.command_palette = None;
            app.file_finder = None;
            app.context_menu = None;
            let screen = screen_text_863(&mut app, 80, 24);
            assert!(
                screen.contains("Enter next \u{b7} Esc leave"),
                "step {}: the keys are cut:\n{screen}",
                steps + 1
            );
            app.advance_tour();
            steps += 1;
        }
    });
}

/// #863 regression, drawn through base APIs only: the Quick Open step names
/// this platform's modifier, `Ctrl+P` off macOS, where `Cmd+P` is a key that
/// does nothing.
#[test]
fn the_tour_names_this_platform_s_modifier_on_screen() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        app.advance_tour();
        app.file_finder = None;
        let screen = screen_text_863(&mut app, 200, 50);
        let (want, not) = if cfg!(target_os = "macos") {
            ("Cmd+P finds", "Ctrl+P finds")
        } else {
            ("Ctrl+P finds", "Cmd+P finds")
        };
        assert!(
            screen.contains(want) && !screen.contains(not),
            "the Quick Open caption names {want:?}:\n{screen}"
        );
        app.finish_tour();
    });
}

/// #863 negative: the caption panel keeps out of a frame too small to hold
/// it, and drawing the tour there does not panic.
#[test]
fn the_tour_caption_panel_stays_out_of_a_frame_too_small_for_it() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        assert!(app.tour_caption_panel(Rect::new(0, 0, 80, 24)).is_some());
        assert!(app.tour_caption_panel(Rect::new(0, 0, 19, 24)).is_none());
        assert!(app.tour_caption_panel(Rect::new(0, 0, 80, 3)).is_none());
        for (w, h) in [(20, 4), (19, 24), (80, 3), (12, 2)] {
            let _ = screen_text_863(&mut app, w, h);
        }
        app.finish_tour();
        assert!(app.tour_caption_panel(Rect::new(0, 0, 80, 24)).is_none());
    });
}

/// #863 (issue comment): a tour ended by closing croft's terminal left its
/// `croft-demo-<pid>-<stamp>` folder in the cache, and the next `croft demo`
/// made its own beside it. Starting the tour now sweeps a marked folder
/// whose croft is gone, and keeps this croft's own and an unmarked one.
#[test]
fn starting_the_tour_sweeps_a_dead_tour_s_scratch_folder() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let demo = croft_cache_dir().join("demo");
        let make = |name: &str, marked: bool| {
            let dir = demo.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("README.md"), "# old tour\n").unwrap();
            if marked {
                std::fs::write(dir.join(crate::tour::SCRATCH_MARKER), "scratch\n").unwrap();
            }
            dir
        };
        // A pid above any kernel's pid_max: no such process.
        let dead = make("croft-demo-2147483647-1", true);
        let unmarked = make("croft-demo-2147483646-1", false);
        let mine = make(&format!("croft-demo-{}-1", std::process::id()), true);
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        assert!(app.tour.is_some());
        assert!(!dead.exists(), "the dead tour's folder is swept");
        assert!(
            unmarked.is_dir(),
            "an unmarked folder is not croft's to delete"
        );
        assert!(mine.is_dir(), "this croft's own folder stays");
        app.finish_tour();
    });
}

/// #863: step 1 says Esc leaves the tour at any time and the keys row says
/// "Esc leave", but with the theme picker a step opened, Esc closed only
/// the picker. Esc with a modal the tour opened (the palette, Quick Open,
/// the theme picker) now closes it and leaves the tour.
#[test]
fn esc_leaves_the_tour_from_a_picker_the_tour_opened() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.start_demo();
        let scratch = app.workspace_root().to_path_buf();
        let theme = app.theme;
        // Enter through the steps until one has a picker up.
        let mut presses = 0;
        while app.context_menu.is_none() && app.tour.is_some() && presses < 20 {
            app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
                .unwrap();
            presses += 1;
        }
        assert!(
            app.context_menu.is_some(),
            "fixture: the theme picker is up"
        );
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(
            app.tour.is_none(),
            "Esc leaves the tour, as its keys row says"
        );
        assert!(app.context_menu.is_none(), "and the picker goes with it");
        assert!(app.command_palette.is_none() && app.file_finder.is_none());
        assert_eq!(app.theme, theme, "leaving picked no theme");
        assert_eq!(app.workspace_root(), tmp.path());
        assert!(!scratch.exists());
    });
}
