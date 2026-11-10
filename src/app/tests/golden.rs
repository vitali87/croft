//! Screen snapshots ("goldens"): whole frames drawn exactly as the running
//! app draws them, compared against text files saved beside this module.
//!
//! Each scene builds an [`App`], draws it with `render_for_terminal` (the
//! path the real main loop takes, which also fits colours to the terminal's
//! depth) into ratatui's `TestBackend`, and writes the result as text: a
//! header, every row's symbols, the cells that are not one plain narrow
//! glyph, and every row's style runs with exact colours. A refactor that
//! changes any cell, colour, modifier, glyph or the caret fails the scene
//! with a diff of the first differing lines. The actual text is written to
//! `target/golden-actual/<scene>.txt`.
//!
//! `CROFT_BLESS=1 cargo test --bin croft -- app::tests::golden` rewrites
//! the saved files in `src/app/tests/golden/` instead of comparing. A scene
//! with no saved file fails unless blessed, and a saved file no scene owns
//! fails `every_golden_belongs_to_a_scene`. The published crate leaves the
//! saved files out (`exclude` in Cargo.toml), so `cargo test` run from its
//! source skips every scene with a note instead.
//!
//! # Determinism
//!
//! A golden that changes on its own is worse than none, so every input the
//! frame reads is pinned at its source:
//!
//! * Workspace: a temp dir holding a folder named `golden`, so the Explorer
//!   and every relative path show the same name. No scene draws an absolute
//!   path.
//! * The user's own files. Under `cfg(test)` `App::new` already ignores the
//!   user's settings layers, `config.json` and the theme in it, keymap,
//!   problem matchers, profile, locale, layout and command history (see the
//!   `cfg!(test)` guards there and in `config_layers`), and lists only the
//!   bundled extensions, whose manifests `mcp::registry` reads without the
//!   user's. It still reads the macros, snippets, terminal triggers and
//!   agent table from the user's config folder and the hosts refused a
//!   remote offer from the cache folder, and keeps the real config folder
//!   as `config_dir`, where the Extensions view and the test worker look for
//!   added extensions. [`app_with_theme`] points all of these at an empty
//!   folder beside the workspace. Only the config watcher, which nothing but
//!   the tick polls, and the embedded terminal's shell, which no scene
//!   shows, still look at the real folders.
//! * The terminal's environment (`TERM`, `COLORTERM`, `TERM_PROGRAM`,
//!   `SSH_*`, Termux's markers, `CROFT_FORCE_*`) is read into App fields at
//!   startup; [`hermetic`] overwrites them (`color_depth`,
//!   `inline_protocol`, `sixel_supported`, `cell_pixel`, `is_remote`,
//!   `persistence_warning`, `osk_auto`) as soon as the App is built, so what
//!   a scene does runs on the pinned values, and again just before drawing,
//!   since a theme switch reads the environment again. The one read made
//!   during a frame, `iterm2_inline::detect_iterm2_inline_support`
//!   (`TERM_PROGRAM` and `CROFT_FORCE_INLINE_IMAGES`; under iTerm2 or
//!   WezTerm the frame leaves the canvas to the session background), has no
//!   App field, and `set_var` would race every other test in the process,
//!   so [`draw`] answers it for its own thread through the `cfg(test)`-only
//!   `PLAIN_HOST_FRAMES` ([`PlainHost`]).
//! * The status line starts from a process-wide latch other tests may set
//!   (`VIEW_BIND_ERROR`); scenes pin it to the "Ready" a clean start shows.
//! * The caret blinks on the wall clock. [`draw`] resets the blink and
//!   redraws if a frame took longer than the visible half-period
//!   (`cursor_blink::HALF`), so every frame is drawn in the caret's visible
//!   phase.
//! * Syntax colouring runs on a background thread; under test the editor
//!   waits for it before it returns from `open` (`highlight_budget` is
//!   `None`). The code palette it reads is process-wide, and under test it
//!   stays Base16 whatever the theme, since other tests pin those colours
//!   (`highlight::set_syntax_palette`). So each scene's thread gets a
//!   palette of its own ([`ThreadPalette`]): the push of the theme's palette
//!   that `App::new` makes lands there, the editor's highlight pass carries
//!   it to its worker, and the code shows each theme's own colours, as in
//!   the running app.
//! * Scenes never run the main loop's tick: git, the file watcher, search,
//!   language servers and the embedded terminal answer on background
//!   threads at a time no test controls, and only the tick drains them. A
//!   scene sets the state those answers would set (the Problems list, for
//!   one) directly, or drains the one answer it shows and waits until that
//!   work is done. So no spinner or progress mark is turning when a scene
//!   is drawn. The embedded terminal's shell output depends on the user's
//!   shell and rc files, so no scene shows the terminal tab.
//! * Time: no scene draws the clock, a date or an age.
//! * Git: no scene's workspace is a repository.
//! * Host and user names: the hostname is drawn only beside a forwarded
//!   port in PORTS and a shell's host only on a terminal tab; no scene
//!   shows either, and `is_remote` is pinned to a local session.
//! * Each scene is drawn until two frames in a row match, and the header
//!   says how many frames that took. Every scene's first frame is final
//!   today, so a change that leaves the first frame after an action wrong
//!   fails even when the next frame would repair it. A scene that never
//!   settles fails.
//! * Platform: chords are spelled per platform (`Cmd` against `Ctrl`) and
//!   paths per OS, so the module runs on Linux only.
//!
//! # Masks
//!
//! Only things a build changes on purpose are masked, each with a fixed
//! fill (`MASK_SYMBOL`, reset colours) over whole rows of a named region
//! so a longer or shorter value cannot shift what is compared:
//!
//! * `version`: any editor-area row showing this build's version
//!   (`CARGO_PKG_VERSION`). On the welcome page that is the wordmark row,
//!   whose wordmark and version pill are centred as one group, and the
//!   release card's heading row, which also says whether the build carries
//!   unreleased notes. The card's notes are pinned with `set_test_notes`,
//!   so they are compared, not masked.
//!
//! # Not covered
//!
//! Only the plain-terminal path is drawn. The inline-image paths (iTerm2,
//! Kitty and sixel graphics, which draw croft's icons and wordmark on the
//! terminals that offer them) and the caret's shape, which
//! `sync_caret_style` sends after each frame, are not compared.

use super::*;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use unicode_width::UnicodeWidthStr;

/// The fill a masked cell is serialised with.
const MASK_SYMBOL: &str = "\u{2591}";

/// What a cell that is not one printable glyph is written as in its row,
/// so the row keeps one column per cell. The `cells` section says what the
/// cell really held.
const ODD_SYMBOL: &str = "\u{00a4}";

/// A named region painted over before a frame is compared.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Mask {
    name: &'static str,
    area: Rect,
}

/// The caret as the terminal would show it after the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Caret {
    shown: bool,
    at: Position,
}

/// One drawn frame: what the terminal holds after it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shot {
    buffer: Buffer,
    caret: Caret,
}

/// Paint `masks` over `shot` with the fixed fill.
fn apply_masks(shot: &mut Shot, masks: &[Mask]) {
    for mask in masks {
        for y in mask.area.top()..mask.area.bottom() {
            for x in mask.area.left()..mask.area.right() {
                let cell = &mut shot.buffer[(x, y)];
                cell.reset();
                cell.set_symbol(MASK_SYMBOL);
            }
        }
    }
}

/// How a colour is written: `#rrggbb`, `idx<n>` or the ANSI name.
fn colour(c: Color) -> String {
    match c {
        Color::Reset => String::from("reset"),
        Color::Black => String::from("black"),
        Color::Red => String::from("red"),
        Color::Green => String::from("green"),
        Color::Yellow => String::from("yellow"),
        Color::Blue => String::from("blue"),
        Color::Magenta => String::from("magenta"),
        Color::Cyan => String::from("cyan"),
        Color::Gray => String::from("gray"),
        Color::DarkGray => String::from("darkgray"),
        Color::LightRed => String::from("lightred"),
        Color::LightGreen => String::from("lightgreen"),
        Color::LightYellow => String::from("lightyellow"),
        Color::LightBlue => String::from("lightblue"),
        Color::LightMagenta => String::from("lightmagenta"),
        Color::LightCyan => String::from("lightcyan"),
        Color::White => String::from("white"),
        Color::Indexed(i) => format!("idx{i}"),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// Whether `c` is in a Unicode private use area, where Nerd Font icons live.
fn private_use(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

/// A cell's style as one run key: colours, then modifiers and `skip`.
fn style_key(cell: &Cell) -> String {
    let mut key = format!("fg={} bg={}", colour(cell.fg), colour(cell.bg));
    if cell.underline_color != Color::Reset {
        key.push_str(&format!(" ul={}", colour(cell.underline_color)));
    }
    if !cell.modifier.is_empty() {
        let names: Vec<&str> = cell.modifier.iter_names().map(|(name, _)| name).collect();
        key.push_str(&format!(" mod={}", names.join("+")));
    }
    if cell.skip {
        key.push_str(" skip");
    }
    key
}

/// What a scene is, for the header.
struct Header<'a> {
    scene: &'a str,
    theme: crate::theme::Theme,
    depth: crate::color_depth::ColorDepth,
    /// The first frame that already showed the compared one (1: the first
    /// frame drawn was final).
    settled: usize,
}

/// `shot` as the golden text: header, symbols, odd cells, style runs.
fn serialise(header: &Header, shot: &Shot, masks: &[Mask]) -> String {
    use std::fmt::Write as _;
    let area = shot.buffer.area;
    let depth = match header.depth {
        crate::color_depth::ColorDepth::TrueColor => "truecolor",
        crate::color_depth::ColorDepth::Ansi256 => "256",
        crate::color_depth::ColorDepth::Ansi16 => "16",
    };
    let mut out = String::new();
    let _ = writeln!(out, "croft screen golden v1");
    let _ = writeln!(out, "scene: {}", header.scene);
    let _ = writeln!(out, "size: {}x{}", area.width, area.height);
    let _ = writeln!(out, "theme: {}", header.theme.id());
    let _ = writeln!(out, "colour depth: {depth}");
    let frames = if header.settled == 1 {
        "frame"
    } else {
        "frames"
    };
    let _ = writeln!(out, "settled after: {} {frames}", header.settled);
    if shot.caret.shown {
        let _ = writeln!(
            out,
            "caret: row {} col {}",
            shot.caret.at.y, shot.caret.at.x
        );
    } else {
        let _ = writeln!(out, "caret: hidden");
    }
    for m in masks {
        let _ = writeln!(
            out,
            "mask: {} rows {}..{} cols {}..{}",
            m.name,
            m.area.top(),
            m.area.bottom(),
            m.area.left(),
            m.area.right()
        );
    }
    let _ = writeln!(out, "--- symbols");
    let mut odd = Vec::new();
    for y in 0..area.height {
        let mut row = String::new();
        // Cells a wide glyph to their left already covers: the terminal
        // shows the glyph there, so the row adds nothing for them.
        let mut covered = 0usize;
        for x in 0..area.width {
            let sym = shot.buffer[(x, y)].symbol();
            if covered > 0 {
                covered -= 1;
                if sym != " " {
                    odd.push(format!("r{y:02} c{x:03} hidden {sym:?}"));
                }
                continue;
            }
            let width = sym.width();
            if width == 0 || sym.chars().any(char::is_control) {
                odd.push(format!("r{y:02} c{x:03} odd {sym:?}"));
                row.push_str(ODD_SYMBOL);
                continue;
            }
            row.push_str(sym);
            if width > 1 {
                odd.push(format!("r{y:02} c{x:03} wide {sym:?} {width} cells"));
                covered = width - 1;
            } else if sym.chars().any(private_use) {
                // A Nerd Font icon is one cell to ratatui and to the
                // terminal, but a viewer without the font may draw it
                // narrower or wider, so the row alone could read shifted.
                let points: Vec<String> =
                    sym.chars().map(|c| format!("U+{:04X}", c as u32)).collect();
                odd.push(format!("r{y:02} c{x:03} icon {}", points.join(" ")));
            }
        }
        let _ = writeln!(out, "{y:02}|{row}|");
    }
    let _ = writeln!(out, "--- cells");
    for line in odd {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out, "--- styles");
    for y in 0..area.height {
        let mut x = 0;
        while x < area.width {
            let key = style_key(&shot.buffer[(x, y)]);
            let start = x;
            while x < area.width && style_key(&shot.buffer[(x, y)]) == key {
                x += 1;
            }
            let _ = writeln!(out, "r{y:02} c{start:03}-{:03} {key}", x - 1);
        }
    }
    out
}

/// Where goldens are read from (at run time, so a blessed file needs no
/// rebuild) and where a mismatching scene's actual text is written.
struct Dirs {
    golden: PathBuf,
    actual: PathBuf,
    /// Whether this is a published crate's source, which leaves the
    /// goldens out.
    packaged: bool,
}

impl Dirs {
    fn repo() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        Dirs {
            golden: root.join("src/app/tests/golden"),
            actual: root.join("target/golden-actual"),
            // `cargo package` keeps the manifest it was given under this
            // name beside the one it publishes; a checkout never has it.
            packaged: root.join("Cargo.toml.orig").is_file(),
        }
    }
}

/// Whether this run rewrites goldens instead of comparing them.
fn blessing() -> bool {
    std::env::var_os("CROFT_BLESS").is_some_and(|v| v == "1")
}

/// The first differing lines of `expected` and `actual` (symbol rows,
/// cells and style runs alike), as a unified diff; `None` when they match.
fn first_differences(expected: &str, actual: &str) -> Option<String> {
    const MAX_LINES: usize = 60;
    if expected == actual {
        return None;
    }
    let diff = similar::TextDiff::from_lines(expected, actual);
    let text = diff
        .unified_diff()
        .context_radius(1)
        .header("golden", "actual")
        .to_string();
    let mut lines: Vec<&str> = text.lines().take(MAX_LINES + 1).collect();
    if lines.len() > MAX_LINES {
        lines.truncate(MAX_LINES);
        lines.push("... (more differences follow)");
    }
    Some(lines.join("\n"))
}

/// Compare `actual` with scene `name`'s golden in `dirs`, or rewrite the
/// golden when `bless`.
fn compare(dirs: &Dirs, name: &str, actual: &str, bless: bool) -> Result<(), String> {
    let path = dirs.golden.join(format!("{name}.txt"));
    if bless {
        std::fs::create_dir_all(&dirs.golden).unwrap();
        std::fs::write(&path, actual).unwrap();
        return Ok(());
    }
    if dirs.packaged && !dirs.golden.exists() {
        eprintln!(
            "scene {name:?} skipped: the published crate leaves its golden out; \
             run the suite from a checkout to compare it"
        );
        return Ok(());
    }
    let expected = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "no golden for scene {name:?} at {} ({e}); run with CROFT_BLESS=1 to write it",
            path.display()
        )
    })?;
    let Some(diff) = first_differences(&expected, actual) else {
        return Ok(());
    };
    let out = dirs.actual.join(format!("{name}.txt"));
    std::fs::create_dir_all(&dirs.actual).unwrap();
    std::fs::write(&out, actual).unwrap();
    Err(format!(
        "scene {name:?} no longer matches its golden {}.\nThe actual frame is in {}; \
         if the change is intended, rerun with CROFT_BLESS=1.\n{diff}",
        path.display(),
        out.display(),
    ))
}

/// A hand-built shot of `width` x 1 cells for the harness's own tests.
fn shot_of(width: u16, paint: impl FnOnce(&mut Buffer)) -> Shot {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
    paint(&mut buffer);
    Shot {
        buffer,
        caret: Caret {
            shown: false,
            at: Position::new(0, 0),
        },
    }
}

/// `shot` serialised under a fixed header.
fn text_of(shot: &Shot, masks: &[Mask]) -> String {
    let header = Header {
        scene: "harness",
        theme: crate::theme::Theme::default(),
        depth: crate::color_depth::ColorDepth::TrueColor,
        settled: 1,
    };
    serialise(&header, shot, masks)
}

/// The lines of `section` (`symbols`, `cells` or `styles`) in `text`.
fn section<'a>(text: &'a str, section: &str) -> Vec<&'a str> {
    let start = format!("--- {section}");
    text.lines()
        .skip_while(|l| *l != start)
        .skip(1)
        .take_while(|l| !l.starts_with("--- "))
        .collect()
}

#[test]
fn golden_rows_keep_one_column_per_cell_around_wide_and_icon_glyphs() {
    let red = Style::default().fg(Color::Red);
    let shot = shot_of(12, |b| {
        b.set_string(0, 0, "a\u{4e2d}b\u{1f980}\u{e7a8}", Style::default());
        b.set_string(7, 0, "d", red);
        b[(8, 0)].set_symbol("");
        b.set_string(9, 0, "e", red);
    });
    let text = text_of(&shot, &[]);
    // Each wide glyph is followed by nothing for the cell it covers, so the
    // row is exactly 12 columns wide and `d` and `e` stay where they were
    // drawn; the empty cell is written as one placeholder column.
    let row = section(&text, "symbols")[0];
    assert_eq!(row, "00|a\u{4e2d}b\u{1f980}\u{e7a8}d\u{a4}e  |");
    assert_eq!(UnicodeWidthStr::width(&row[3..row.len() - 1]), 12);
    assert_eq!(
        section(&text, "cells"),
        vec![
            "r00 c001 wide \"\u{4e2d}\" 2 cells",
            "r00 c004 wide \"\u{1f980}\" 2 cells",
            "r00 c006 icon U+E7A8",
            "r00 c008 odd \"\"",
        ]
    );
    let styles = section(&text, "styles");
    assert!(
        styles.contains(&"r00 c007-007 fg=red bg=reset"),
        "{styles:?}"
    );
    assert!(
        styles.contains(&"r00 c009-009 fg=red bg=reset"),
        "{styles:?}"
    );
}

#[test]
fn golden_lists_a_hidden_symbol_under_a_wide_glyph() {
    let shot = shot_of(4, |b| {
        b.set_string(0, 0, "\u{4e2d}", Style::default());
        b[(1, 0)].set_symbol("x");
    });
    let text = text_of(&shot, &[]);
    assert_eq!(section(&text, "symbols")[0], "00|\u{4e2d}  |");
    assert!(section(&text, "cells").contains(&"r00 c001 hidden \"x\""));
}

#[test]
fn golden_style_runs_write_every_colour_and_modifier_exactly() {
    let shot = shot_of(6, |b| {
        b.set_string(
            0,
            0,
            "ab",
            Style::default()
                .fg(Color::Rgb(0x12, 0xab, 0xef))
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        );
        b.set_string(2, 0, "c", Style::default().fg(Color::DarkGray));
        b.set_string(
            3,
            0,
            "d",
            Style::default()
                .fg(Color::Rgb(0x12, 0xab, 0xef))
                .underline_color(Color::LightMagenta)
                .add_modifier(Modifier::UNDERLINED),
        );
        b[(4, 0)].skip = true;
    });
    let text = text_of(&shot, &[]);
    assert_eq!(
        section(&text, "styles"),
        vec![
            "r00 c000-001 fg=#12abef bg=idx236 mod=BOLD+ITALIC",
            "r00 c002-002 fg=darkgray bg=reset",
            "r00 c003-003 fg=#12abef bg=reset ul=lightmagenta mod=UNDERLINED",
            "r00 c004-004 fg=reset bg=reset skip",
            "r00 c005-005 fg=reset bg=reset",
        ]
    );
}

#[test]
fn golden_masks_fill_their_region_and_are_named_in_the_header() {
    let mut shot = shot_of(6, |b| {
        b.set_string(0, 0, "v1.2.3", Style::default().fg(Color::Red));
    });
    let masks = [Mask {
        name: "version",
        area: Rect::new(1, 0, 4, 1),
    }];
    apply_masks(&mut shot, &masks);
    let text = text_of(&shot, &masks);
    assert!(
        text.contains("\nmask: version rows 0..1 cols 1..5\n"),
        "{text}"
    );
    assert_eq!(
        section(&text, "symbols")[0],
        "00|v\u{2591}\u{2591}\u{2591}\u{2591}3|"
    );
    assert!(section(&text, "styles").contains(&"r00 c001-004 fg=reset bg=reset"));
}

#[test]
fn golden_comparison_fails_on_one_changed_cell_or_colour_and_saves_the_actual() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = Dirs {
        golden: tmp.path().join("golden"),
        actual: tmp.path().join("actual"),
        packaged: false,
    };
    let paint = |b: &mut Buffer| {
        b.set_string(0, 0, "fn main", Style::default().fg(Color::Rgb(1, 2, 3)));
    };
    let saved = text_of(&shot_of(8, paint), &[]);
    let missing = compare(&dirs, "scene", &saved, false).unwrap_err();
    assert!(missing.contains("CROFT_BLESS=1"), "{missing}");
    compare(&dirs, "scene", &saved, true).unwrap();
    compare(&dirs, "scene", &saved, false).unwrap();

    let mut glyph = shot_of(8, paint);
    glyph.buffer[(3, 0)].set_symbol("M");
    let err = compare(&dirs, "scene", &text_of(&glyph, &[]), false).unwrap_err();
    assert!(
        err.contains("-00|fn main |") && err.contains("+00|fn Main |"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(dirs.actual.join("scene.txt")).unwrap(),
        text_of(&glyph, &[])
    );

    let mut colour = shot_of(8, paint);
    colour.buffer[(3, 0)].fg = Color::Rgb(1, 2, 4);
    let err = compare(&dirs, "scene", &text_of(&colour, &[]), false).unwrap_err();
    assert!(err.contains("+r00 c003-003 fg=#010204 bg=reset"), "{err}");
    assert!(err.contains("-r00 c000-006 fg=#010203 bg=reset"), "{err}");
}

/// A published crate's source, which has no golden folder, skips its
/// scenes; a missing golden anywhere else fails.
#[test]
fn golden_scenes_are_skipped_only_in_a_published_crate_without_goldens() {
    let tmp = tempfile::tempdir().unwrap();
    let mut dirs = Dirs {
        golden: tmp.path().join("golden"),
        actual: tmp.path().join("actual"),
        packaged: true,
    };
    compare(&dirs, "scene", "text", false).unwrap();
    std::fs::create_dir_all(&dirs.golden).unwrap();
    compare(&dirs, "scene", "text", false).unwrap_err();
    std::fs::remove_dir(&dirs.golden).unwrap();
    dirs.packaged = false;
    compare(&dirs, "scene", "text", false).unwrap_err();
}

/// How many frames a scene may take to settle.
const MAX_FRAMES: usize = 4;

/// `TestBackend`, plus whether the caret is shown, which it keeps private.
struct GoldenBackend {
    inner: TestBackend,
    caret_shown: bool,
}

impl Backend for GoldenBackend {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.caret_shown = false;
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.caret_shown = true;
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

/// Pin every App field that startup reads from the terminal's environment,
/// as the module doc lists. Run as soon as the App is built and again just
/// before drawing, since a theme switch reads the environment again.
fn hermetic(app: &mut App, depth: crate::color_depth::ColorDepth) {
    app.color_depth = depth;
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::None;
    app.sixel_supported = false;
    app.cell_pixel = None;
    app.is_remote = false;
    app.persistence_warning = None;
    app.osk_auto = false;
}

/// While alive, this thread highlights code with a palette of its own
/// (`highlight::set_thread_syntax_palette`), seeded as the shared one is,
/// so the theme's palette an App pushes lands there, as it lands in the
/// shared one in the running app.
struct ThreadPalette {
    before: Option<crate::theme::SyntaxPalette>,
}

impl ThreadPalette {
    fn enter() -> Self {
        let before = crate::highlight::thread_syntax_palette();
        crate::highlight::set_thread_syntax_palette(Some(crate::theme::SyntaxPalette::BASE16));
        ThreadPalette { before }
    }
}

impl Drop for ThreadPalette {
    fn drop(&mut self) {
        crate::highlight::set_thread_syntax_palette(self.before);
    }
}

/// A scene's workspace and its thread's code palette, which last as long
/// as the test holds this.
struct Fixture {
    _dir: tempfile::TempDir,
    _palette: ThreadPalette,
}

/// A fresh workspace: a temp dir holding a folder named `golden`, so the
/// name the Explorer shows never varies, and a code palette of this
/// thread's own for the App built on it. Returns the guard and the root.
fn workspace(files: &[(&str, &str)]) -> (Fixture, PathBuf) {
    let palette = ThreadPalette::enter();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("golden");
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
    std::fs::create_dir_all(&root).unwrap();
    let fixture = Fixture {
        _dir: tmp,
        _palette: palette,
    };
    (fixture, root)
}

/// One frame of `app` at `width` x `height`, drawn as the main loop draws
/// it, in the caret's visible blink phase.
fn draw_once(app: &mut App, width: u16, height: u16) -> Shot {
    // A frame that takes longer than the blink's visible half could see the
    // caret both shown and hidden; draw it again until one is quick enough.
    for _ in 0..8 {
        let started = std::time::Instant::now();
        let backend = GoldenBackend {
            inner: TestBackend::new(width, height),
            caret_shown: false,
        };
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        app.cursor_blink.poke();
        terminal.draw(|f| app.render_for_terminal(f)).unwrap();
        if started.elapsed() < super::cursor_blink::HALF && app.cursor_visible_phase() {
            let backend = terminal.backend_mut();
            let at = backend.get_cursor_position().unwrap();
            return Shot {
                buffer: backend.inner.buffer().clone(),
                caret: Caret {
                    shown: backend.caret_shown,
                    at,
                },
            };
        }
    }
    panic!(
        "no frame drew within the caret's {:?} visible phase in 8 tries",
        super::cursor_blink::HALF
    );
}

/// While alive, frames drawn on this thread are drawn as a terminal without
/// iTerm2's session background draws them, whatever runs the suite.
struct PlainHost;

impl PlainHost {
    fn enter() -> Self {
        crate::iterm2_inline::PLAIN_HOST_FRAMES.with(|f| f.set(true));
        PlainHost
    }
}

impl Drop for PlainHost {
    fn drop(&mut self) {
        crate::iterm2_inline::PLAIN_HOST_FRAMES.with(|f| f.set(false));
    }
}

/// Draw `app` until two frames in a row match. Returns that frame and the
/// number of the first frame that already showed it (1: the first frame
/// drawn was final).
fn draw(app: &mut App, width: u16, height: u16) -> (Shot, usize) {
    let _plain = PlainHost::enter();
    let mut last = draw_once(app, width, height);
    for frame in 1..MAX_FRAMES {
        let next = draw_once(app, width, height);
        if next == last {
            return (next, frame);
        }
        last = next;
    }
    panic!("the scene did not settle: frame {MAX_FRAMES} still differed from the one before");
}

/// The symbols of row `y` of `buffer` from `x0` to `x1`, joined.
fn row_text(buffer: &Buffer, y: u16, x0: u16, x1: u16) -> String {
    (x0..x1).map(|x| buffer[(x, y)].symbol()).collect()
}

/// The `version` mask: every row of `area` that shows this build's version.
fn version_rows(shot: &Shot, area: Rect) -> Vec<Mask> {
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    (area.top()..area.bottom())
        .filter(|&y| row_text(&shot.buffer, y, area.left(), area.right()).contains(version))
        .map(|y| Mask {
            name: "version",
            area: Rect::new(area.x, y, area.width, 1),
        })
        .collect()
}

/// Pin `app` ([`hermetic`], at colour `depth`), draw it, mask what `masks`
/// picks from the frame, and compare the result with scene `name`'s golden.
fn scene(
    name: &str,
    app: &mut App,
    (width, height): (u16, u16),
    depth: crate::color_depth::ColorDepth,
    masks: impl Fn(&App, &Shot) -> Vec<Mask>,
) -> Result<(), String> {
    assert!(
        !app.show_terminal || app.bottom_panel_tab != BottomPanelTab::Terminal,
        "scene {name:?} shows the embedded terminal, whose shell prints what \
         the user's shell and rc files say, when they say it"
    );
    hermetic(app, depth);
    let (mut shot, settled) = draw(app, width, height);
    let picked = masks(app, &shot);
    apply_masks(&mut shot, &picked);
    let header = Header {
        scene: name,
        theme: app.theme,
        depth: app.color_depth,
        settled,
    };
    let text = serialise(&header, &shot, &picked);
    compare(&Dirs::repo(), name, &text, blessing())
}

/// [`scene`], failing the test on a mismatch.
fn check_scene(
    name: &str,
    app: &mut App,
    size: (u16, u16),
    depth: crate::color_depth::ColorDepth,
    masks: impl Fn(&App, &Shot) -> Vec<Mask>,
) {
    if let Err(e) = scene(name, app, size, depth, masks) {
        panic!("{e}");
    }
}

/// The masks a scene with nothing to mask uses.
fn no_masks(_: &App, _: &Shot) -> Vec<Mask> {
    Vec::new()
}

const MAIN_RS: &str = "\
//! A tiny greeter.

use std::fmt;

/// Who to greet, and how.
pub struct Greeting<'a> {
    name: &'a str,
    excited: bool,
}

impl fmt::Display for Greeting<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = if self.excited { '!' } else { '.' };
        write!(f, \"Hello, {}{mark}\", self.name)
    }
}

fn main() {
    let greeting = Greeting { name: \"croft\", excited: true };
    println!(\"{greeting}\");
    let count = 42;
}
";

/// An app on `root` started with `theme` as its saved theme: what
/// `App::new` does with the theme it loads (`apply_theme` would also
/// re-detect the image protocol from the environment). It is pinned by
/// [`hermetic`] and reads nothing from the user's own folders.
fn app_with_theme(root: &Path, theme: crate::theme::Theme) -> App {
    let mut app = App::new(root.to_path_buf()).unwrap();
    hermetic(&mut app, crate::color_depth::ColorDepth::TrueColor);
    // What `App::new` read from the user's config and cache folders, read
    // again from an empty folder beside the workspace (the module doc says
    // what each is).
    let config = root.with_file_name("config");
    app.config_dir = config.clone();
    app.test_worker.set_extensions_dir(app.extensions_dir());
    app.macros_path = config.join("macros.json");
    app.macro_registers = crate::macros::load(&app.macros_path);
    app.snippets = crate::snippets::SnippetSet::load(&config.join("snippets.json"));
    app.triggers = std::sync::Arc::new(
        crate::triggers::TriggerSet::load(&config.join("triggers.json")).with_builtin_redactions(),
    );
    app.agents = crate::agents::AgentTable::load(&config.join("agents.json"));
    app.remote_offer_refused = Default::default();
    // A clean start says "Ready"; `App::new` reads that from a process-wide
    // latch other tests may set.
    app.status = String::from("Ready");
    // The panel opens on TERMINAL, whose shell prints when it likes, its
    // prompt naming the temp dir: every scene keeps PROBLEMS in front.
    app.bottom_panel_tab = BottomPanelTab::Problems;
    app.theme = theme;
    crate::highlight::set_syntax_palette(theme.syntax());
    app.sync_focus_flags();
    app
}

/// The workbench fixture: a small tree with `src/main.rs` open, the
/// Explorer showing the tree and the bottom panel on PROBLEMS with one
/// warning on `main.rs`.
fn workbench(theme: crate::theme::Theme) -> (Fixture, App) {
    let (fixture, root) = workspace(&[
        ("src/main.rs", MAIN_RS),
        ("src/greeting.rs", "pub fn hello() {}\n"),
        ("README.md", "# golden\n\nA fixture workspace.\n"),
        ("notes.txt", "Nothing to see.\n"),
    ]);
    let mut app = app_with_theme(&root, theme);
    let main = root.join("src/main.rs");
    assert!(app.open_file_at_launch(&main));
    assert!(app.tree.reveal_path(&main));
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rust-analyzer"),
        vec![crate::lsp::manager::Diagnostic {
            start_line: 20,
            start_char: 8,
            end_line: 20,
            end_char: 13,
            severity: crate::lsp::manager::DiagnosticSeverity::Warning,
            message: String::from("unused variable: `count`"),
            tags: Default::default(),
            code: None,
            origin: None,
        }],
    );
    app.lsp_diagnostics.insert(main.clone(), by_server);
    let merged = app.merged_diagnostics(&main);
    app.editor.apply_diagnostics(main, merged);
    app.rebuild_problems();
    app.refresh_problems_badge();
    (fixture, app)
}

#[test]
fn workbench_default_theme() {
    let (_fixture, mut app) = workbench(crate::theme::Theme::default());
    check_scene(
        "workbench",
        &mut app,
        (120, 40),
        crate::color_depth::ColorDepth::TrueColor,
        no_masks,
    );
}

#[test]
fn welcome_page() {
    let (_fixture, root) = workspace(&[("README.md", "# golden\n")]);
    let mut app = app_with_theme(&root, crate::theme::Theme::default());
    app.welcome.set_test_notes(vec![
        crate::release_notes::ReleaseNote {
            kind: crate::release_notes::NoteKind::Feature,
            summary: "Screen goldens compare **every cell** of a frame",
        },
        crate::release_notes::ReleaseNote {
            kind: crate::release_notes::NoteKind::Fix,
            summary: "A `wide` glyph never shifts the columns after it",
        },
    ]);
    check_scene(
        "welcome",
        &mut app,
        (120, 40),
        crate::color_depth::ColorDepth::TrueColor,
        |app, shot| version_rows(shot, app.editor.last_full_area),
    );
}

/// The name of every golden a scene in this module owns.
fn owned_goldens() -> Vec<String> {
    ["workbench", "welcome"].map(String::from).to_vec()
}

/// A golden no scene owns, left by a renamed scene or a theme that left the
/// bundle, fails here instead of lingering until a later scene takes its
/// name and passes against a frame nobody blessed for it.
#[test]
fn every_golden_belongs_to_a_scene() {
    let dirs = Dirs::repo();
    let Ok(entries) = std::fs::read_dir(&dirs.golden) else {
        assert!(
            dirs.packaged,
            "no golden folder at {}",
            dirs.golden.display()
        );
        return;
    };
    // Goldens another test module saves here: the key ledger's.
    const SAVED_ELSEWHERE: [&str; 1] = ["key-ledger"];
    let owned: Vec<String> = owned_goldens()
        .into_iter()
        .chain(SAVED_ELSEWHERE.map(String::from))
        .map(|name| format!("{name}.txt"))
        .collect();
    let mut orphans: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|file| !owned.contains(file))
        .collect();
    orphans.sort();
    assert!(
        orphans.is_empty(),
        "goldens no scene owns (delete them, or add their scene to owned_goldens): {orphans:?}"
    );
}
