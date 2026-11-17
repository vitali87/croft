//! Reading the grid as text: visible and wrapped lines, logical rows across soft wraps, find
//! lines, and the screen as ANSI with its colours.

use super::*;

/// The visible screen as `(plain, coloured, wraps)` rows plus the cursor's
/// `(row, col)` on it, `None` when hidden: [`PtyTerminal::screen_ansi_wrapped`].
pub type ScreenAnsi = (Vec<(String, String, bool)>, Option<(u16, u16)>);

impl PtyTerminal {
    pub fn visible_text(&self) -> String {
        let term = self.term.lock();
        let rows = term.screen_lines();
        let cols = term.columns();
        if rows == 0 || cols == 0 {
            return String::new();
        }
        let off = term.grid().display_offset() as i32;
        extract_selection_text(&term, -off, 0, rows as i32 - 1 - off, cols - 1)
    }

    /// Every readable grid line as plain text — oldest scrollback first, then
    /// the live screen — paired with the absolute alacritty `Line` index of
    /// row 0. Terminal find searches these lines; `top + row` maps a hit's
    /// row back to its grid line so it can be scrolled into view and
    /// highlighted. Wide-char spacer cells are skipped (see
    /// [`row_text_and_cols`]), so match positions are char indices, not grid
    /// columns.
    pub fn grid_lines(&self) -> (Vec<String>, i32) {
        let (lines, _, top) = self.grid_lines_wrapped();
        (lines, top)
    }

    /// [`Self::grid_lines`] plus, per row, whether it soft-wraps into the
    /// next, read under the same lock: redaction joins wrapped rows, since a
    /// secret split across two rows matches neither on its own.
    pub fn grid_lines_wrapped(&self) -> (Vec<String>, Vec<bool>, i32) {
        grid_rows_wrapped(&self.term.lock())
    }

    /// The grid as find's logical lines: soft-wrapped rows joined, so a
    /// match the pane's width cut in two is found (#1275). See
    /// [`join_wrapped_rows`].
    pub fn find_lines(&self) -> Vec<FindLine> {
        let (lines, wraps, top) = self.grid_lines_wrapped();
        join_wrapped_rows(lines, &wraps, top)
    }

    /// [`Self::find_lines`] plus the scroll-clock reading, captured under
    /// ONE term lock. Terminal find stamps its match anchor with the clock;
    /// separate reads would let output land between them and mis-pair a
    /// line with a reading it wasn't captured at.
    pub fn find_lines_and_clock(&self) -> (Vec<FindLine>, i64) {
        let term = self.term.lock();
        let (lines, wraps, top) = grid_rows_wrapped(&term);
        (join_wrapped_rows(lines, &wraps, top), self.clock_now(&term))
    }

    /// Every readable grid row as `(plain, coloured)`: the plain text
    /// exactly as [`Self::grid_lines`] gives it, and the same row with the
    /// cells' colours and attributes written back as SGR, so the text can be
    /// re-rendered elsewhere with the colours the pane showed (#257: "open
    /// scrollback in an editor tab" through the rendered log view). Both
    /// forms come from ONE pass under ONE term lock, so a caller that masks
    /// the plain row and writes the coloured one can never pair a row with
    /// its neighbour after output landed between two reads. Wide-char
    /// spacers are skipped as in `grid_lines`; trailing default-styled
    /// blanks are trimmed (a row padded with a coloured background keeps
    /// those cells, which the plain twin drops, so the two forms can differ
    /// in length there); every row that set a style ends with a reset.
    pub fn grid_lines_ansi(&self) -> Vec<(String, String)> {
        self.grid_lines_ansi_wrapped()
            .into_iter()
            .map(|(plain, ansi, _)| (plain, ansi))
            .collect()
    }

    /// [`Self::grid_lines_ansi`] plus whether each row soft-wraps into the
    /// next (see [`Self::grid_lines_wrapped`]).
    pub fn grid_lines_ansi_wrapped(&self) -> Vec<(String, String, bool)> {
        let term = self.term.lock();
        if term.columns() == 0 {
            return Vec::new();
        }
        let top = term.grid().topmost_line().0;
        ansi_rows(&term, top)
    }

    /// The visible screen as [`Self::grid_lines_ansi_wrapped`] rows, with
    /// the shell cursor's cell on that screen, read under ONE term lock so
    /// the cursor belongs to the rows it is drawn over (#356: a recorded
    /// frame is the screen in colour, cursor included). The cursor is
    /// `None` when the program hid it; scrollback is never walked, so the
    /// cost is one screen however long the history is.
    pub fn screen_ansi_wrapped(&self) -> ScreenAnsi {
        let term = self.term.lock();
        if term.columns() == 0 {
            return (Vec::new(), None);
        }
        let rows = ansi_rows(&term, 0);
        let p = term.grid().cursor.point;
        let cursor = (term.mode().contains(TermMode::SHOW_CURSOR)
            && p.line.0 >= 0
            && (p.line.0 as usize) < rows.len())
        .then_some((p.line.0 as u16, p.column.0 as u16));
        (rows, cursor)
    }

    /// The visible viewport rows as spacer-skipped plain text, paired with
    /// the absolute grid line of the first row (`-display_offset`).
    /// Quick-select scans exactly what is on screen — labels the user cannot
    /// see cannot be typed.
    pub fn visible_lines(&self) -> (Vec<String>, i32) {
        let term = self.term.lock();
        if term.columns() == 0 {
            return (Vec::new(), 0);
        }
        let off = term.grid().display_offset() as i32;
        let rows = term.screen_lines() as i32;
        let mut lines = Vec::new();
        for l in -off..rows - off {
            let (s, _cols) = row_text_and_cols(&term, l);
            lines.push(s.trim_end().to_string());
        }
        (lines, -off)
    }

    /// [`Self::visible_lines`] plus per-row soft-wrap flags and the scroll
    /// clock, all from ONE term lock: coordinates and clock captured
    /// together so output landing between two separate reads can never
    /// mis-pair them (quick-select anchors its labels on this snapshot).
    /// `wraps[i]` says row `i` continues into row `i + 1` (WRAPLINE on its
    /// last cell); a wrapping row keeps its trailing spaces — they are real
    /// cells mid-line, and trimming them would glue the next row's first
    /// word onto this one when the rows are stitched for scanning (#64).
    pub fn visible_lines_and_clock(&mut self) -> (Vec<String>, Vec<bool>, i32, i64) {
        let mut term = self.term.lock();
        let clock = self.clock.lock().unwrap().tick(&mut term);
        if term.columns() == 0 {
            return (Vec::new(), Vec::new(), 0, clock);
        }
        let off = term.grid().display_offset() as i32;
        let rows = term.screen_lines() as i32;
        let cols = term.columns();
        let mut lines = Vec::new();
        let mut wraps = Vec::new();
        for l in -off..rows - off {
            let (s, _cols) = row_text_and_cols(&term, l);
            let wrapped = term.grid()[Point::new(Line(l), Column(cols - 1))]
                .flags
                .contains(Flags::WRAPLINE);
            lines.push(if wrapped { s } else { s.trim_end().to_string() });
            wraps.push(wrapped);
        }
        (lines, wraps, -off, clock)
    }

    /// The last `n` non-empty VISIBLE rows, oldest first, for prompt
    /// matching. Reads the viewport only: the whole scrollback is what
    /// `grid_lines` walks, and a prompt is on screen or it is not there.
    pub fn tail_rows(&self, n: usize) -> Vec<String> {
        let text = self.visible_text();
        let mut rows: Vec<String> = text
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(n)
            .map(|l| l.trim_end().to_string())
            .collect();
        rows.reverse();
        rows
    }

    /// Text of the logical line under screen cell `(col, row)`, plus the
    /// 0-based char index within that text the click landed on. Drives
    /// Cmd/Ctrl+click URL and `path:line` detection. A line the terminal
    /// soft-wrapped (WRAPLINE) is stitched back together from all its rows,
    /// so a long URL resolves whole from any of them (#1355). `None` when
    /// the cell is outside the content area.
    pub fn line_text_at(&self, col: u16, row: u16) -> Option<(String, usize)> {
        let (r, c) = self.cell_at(col, row)?;
        let term = self.term.lock();
        let line = r as i32 - term.grid().display_offset() as i32;
        let (row_text, cols_map) = row_text_and_cols(&term, line);
        // Grid column → char index in the spacer-skipped text: the last
        // produced char at-or-before the clicked column, so clicking a wide
        // char's spacer cell resolves to the wide char itself.
        let idx = cols_map.iter().rposition(|&gc| gc <= c as usize)?;
        let top = -(term.grid().history_size() as i32);
        let bottom = term.screen_lines() as i32 - 1;
        let mut first = line;
        while first > top && row_wraps(&term, first - 1) {
            first -= 1;
        }
        let mut last = line;
        while last < bottom && row_wraps(&term, last) {
            last += 1;
        }
        if first == line && last == line {
            return Some((row_text, idx));
        }
        let mut text = String::new();
        let mut offset = 0;
        for l in first..=last {
            let part = if l == line {
                offset = text.chars().count();
                row_text.clone()
            } else {
                row_text_and_cols(&term, l).0
            };
            text.push_str(&part);
        }
        Some((text, offset + idx))
    }

    /// One grid row as spacer-skipped text plus its char-index → grid-column
    /// map (the free `row_text_and_cols` behind the term lock).
    pub fn row_text(&self, line: i32) -> (String, Vec<usize>) {
        let term = self.term.lock();
        row_text_and_cols(&term, line)
    }
}

/// The logical line grid row `line_idx` belongs to (its soft-wrapped
/// neighbours joined, at most 64 rows either way) and the char offset of
/// that row's text within it, both in `row_text_and_cols` terms.
pub fn logical_row_text(term: &Term<VoidListener>, line_idx: i32) -> (String, usize) {
    let (joined, offset, _start) = logical_row_span(term, line_idx);
    (joined, offset)
}

/// [`logical_row_text`] plus the absolute grid line the logical line starts
/// on, which names a find match (see [`FindLine::start`]).
pub fn logical_row_span(term: &Term<VoidListener>, line_idx: i32) -> (String, usize, i32) {
    const REACH: i32 = 64;
    let top = term.grid().topmost_line().0;
    let bottom = term.screen_lines() as i32 - 1;
    let mut start = line_idx;
    while start > top && line_idx - start < REACH && row_wraps(term, start - 1) {
        start -= 1;
    }
    let mut end = line_idx;
    while end < bottom && end - line_idx < REACH && row_wraps(term, end) {
        end += 1;
    }
    let mut joined = String::new();
    let mut offset = 0;
    for l in start..=end {
        let (s, _) = row_text_and_cols(term, l);
        if l == line_idx {
            offset = joined.chars().count();
        }
        joined.push_str(&s);
    }
    (joined, offset, start)
}

/// Every grid row from the top of the scrollback, with its soft-wrap flag
/// and the absolute line of row 0: see [`PtyTerminal::grid_lines_wrapped`].
pub(super) fn grid_rows_wrapped(term: &Term<VoidListener>) -> (Vec<String>, Vec<bool>, i32) {
    if term.columns() == 0 {
        return (Vec::new(), Vec::new(), 0);
    }
    let top = term.grid().topmost_line().0;
    let bottom = term.screen_lines() as i32 - 1;
    let mut lines = Vec::new();
    let mut wraps = Vec::new();
    let mut l = top;
    while l <= bottom {
        let (s, _cols) = row_text_and_cols(term, l);
        // A wrapped row keeps its trailing blanks: a space in its last
        // column separates words of the logical line (`Bearer ` then the
        // token), and trimming it glued them so a redact rule missed.
        let wraps_on = l < bottom && row_wraps(term, l);
        lines.push(if wraps_on {
            s
        } else {
            s.trim_end().to_string()
        });
        wraps.push(wraps_on);
        l += 1;
    }
    (lines, wraps, top)
}

/// One logical line of terminal output as find sees it: a run of rows the
/// pane soft-wrapped, joined back into the text that was printed (#1275).
/// A match position is a char index into `text`; `start` is the absolute
/// grid line of its first row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindLine {
    pub text: String,
    pub start: i32,
    /// Chars per row, in order, so a position maps back to its row.
    pub row_lens: Vec<usize>,
}

impl FindLine {
    /// The absolute grid line holding char `col` of this logical line.
    pub fn line_of(&self, col: usize) -> i32 {
        let mut seen = 0;
        for (i, len) in self.row_lens.iter().enumerate() {
            seen += len;
            if col < seen {
                return self.start + i as i32;
            }
        }
        self.start + self.row_lens.len().saturating_sub(1) as i32
    }
}

/// Join each run of rows whose wrap flag is set into one [`FindLine`]. A
/// row without the flag ends its line, so rows that merely sit next to each
/// other are never searched as one.
pub fn join_wrapped_rows(lines: Vec<String>, wraps: &[bool], top: i32) -> Vec<FindLine> {
    let mut out: Vec<FindLine> = Vec::new();
    let mut continues = false;
    for (i, row) in lines.into_iter().enumerate() {
        let len = row.chars().count();
        match out.last_mut() {
            Some(line) if continues => {
                line.text.push_str(&row);
                line.row_lens.push(len);
            }
            _ => out.push(FindLine {
                text: row,
                start: top + i as i32,
                row_lens: vec![len],
            }),
        }
        continues = wraps.get(i).copied().unwrap_or(false);
    }
    out
}

/// Grid rows from `top` to the bottom of the screen as `(plain, coloured,
/// wraps)`: see [`PtyTerminal::grid_lines_ansi_wrapped`], which reads from
/// the top of the scrollback, and [`PtyTerminal::screen_ansi_wrapped`],
/// which reads the visible screen only.
pub(super) fn ansi_rows(term: &Term<VoidListener>, top: i32) -> Vec<(String, String, bool)> {
    let bottom = term.screen_lines() as i32 - 1;
    let ncols = term.columns();
    let mut lines = Vec::new();
    let mut l = top;
    while l <= bottom {
        // Cells are collected as (char, style key) - three Copy values,
        // no allocation per cell - so trailing default-styled blanks can
        // be trimmed before anything is serialised, and the SGR string
        // is built once per run of equal style rather than once per
        // cell. This runs under the term lock the reader thread shares,
        // and a saturated scrollback is hundreds of thousands of rows.
        let mut cells: Vec<(char, (AnsiColor, AnsiColor, Flags))> = Vec::with_capacity(ncols);
        for c in 0..ncols {
            let cell = &term.grid()[Point::new(Line(l), Column(c))];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let ch = cell_glyph(cell.c);
            // Only the flags an SGR carries: the soft-wrap mark on a row's
            // last cell and a wide char's own flag are not styles, and keyed
            // on them a run of one colour was reset and set again around them
            // (a cast wrote `w\e[0m\e[33mr` at the wrap column, #356).
            cells.push((ch, (cell.fg, cell.bg, cell.flags & SGR_FLAGS)));
        }
        let wraps_on = l < bottom && row_wraps(term, l);
        let plain: String = cells.iter().map(|(ch, _)| *ch).collect::<String>();
        // Trailing blanks trimmed only where the row does not wrap (see
        // `grid_lines_wrapped`).
        let plain = if wraps_on {
            plain
        } else {
            plain.trim_end().to_string()
        };
        while cells
            .last()
            .is_some_and(|(ch, (fg, bg, flags))| *ch == ' ' && style_is_default(*fg, *bg, *flags))
        {
            cells.pop();
        }
        let mut out = String::new();
        let mut current: Option<(AnsiColor, AnsiColor, Flags)> = None;
        let mut styled = false;
        for (ch, key) in cells {
            if current != Some(key) {
                if styled {
                    out.push_str("\x1b[0m");
                }
                let sgr = cell_sgr(key.0, key.1, key.2);
                styled = !sgr.is_empty();
                out.push_str(&sgr);
                current = Some(key);
            }
            out.push(ch);
        }
        if styled {
            out.push_str("\x1b[0m");
        }
        lines.push((plain, out, wraps_on));
        l += 1;
    }
    lines
}

/// [`ansi_rows`] for another module's model of a pane (#694's pane host).
pub(crate) fn ansi_rows_from(term: &Term<VoidListener>, top: i32) -> Vec<(String, String, bool)> {
    ansi_rows(term, top)
}

/// Whether grid row `line_idx` soft-wraps into the next (WRAPLINE on its
/// last cell).
pub fn row_wraps(term: &Term<VoidListener>, line_idx: i32) -> bool {
    let cols = term.columns();
    cols > 0
        && term.grid()[Point::new(Line(line_idx), Column(cols - 1))]
            .flags
            .contains(Flags::WRAPLINE)
}

/// Walk the visible grid from (sr, sc) to (er, ec) inclusive, joining
/// cell contents row-by-row, trimming trailing whitespace per row, and
/// inserting `\n` between rows. Coordinates are viewport-relative;
/// `display_offset` is the alacritty grid's current scrollback offset
/// (number of rows the viewport has been scrolled back from the live
/// bottom). The grid line we read is `row - display_offset`, which is
/// negative when the user has scrolled into history — alacritty's
/// `Grid::index<Point>` accepts those negative `Line` values and
/// returns the scrollback cell, so the extracted text matches exactly
/// what the user sees highlighted on screen. Without the subtraction
/// the function silently re-reads the live grid at the viewport row,
/// which is a different cell entirely once `display_offset > 0` and
/// the user gets the wrong line on the clipboard.
/// What a grid cell shows: a blank for an empty cell and for a control
/// char. alacritty leaves `'\t'` in the cell a TAB started from so copy
/// can give the tab back, but painted raw the host terminal jumps to its
/// own tab stop while ratatui counts one cell, and the rest of the row
/// lands too far right over the previous frame's text (#1287).
pub(super) fn cell_glyph(c: char) -> char {
    if c == '\0' || c.is_control() { ' ' } else { c }
}

/// One grid row as text plus a char-index → grid-column map. A wide char
/// (CJK, emoji) occupies two grid columns — the `WIDE_CHAR` cell and a
/// spacer cell — so the spacers are skipped, making the text read
/// contiguously (`"日本語"`, never `"日 本 語"`). `cols[i]` is the grid
/// column the i-th char starts at, so highlight painters can map a match's
/// char range back onto grid cells.
pub fn row_text_and_cols(term: &Term<VoidListener>, line_idx: i32) -> (String, Vec<usize>) {
    let ncols = term.columns();
    let mut s = String::with_capacity(ncols);
    let mut cols = Vec::with_capacity(ncols);
    for c in 0..ncols {
        let cell = &term.grid()[Point::new(Line(line_idx), Column(c))];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        s.push(cell_glyph(cell.c));
        cols.push(c);
    }
    (s, cols)
}

/// The cell flags [`cell_sgr`] writes as SGR: everything else a cell carries
/// (wrap marks, wide-char spacers) is layout, not style.
pub(super) const SGR_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::DOUBLE_UNDERLINE)
    .union(Flags::UNDERCURL)
    .union(Flags::DOTTED_UNDERLINE)
    .union(Flags::DASHED_UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// The SGR sequence that reproduces a cell's colours and attributes, or an
/// empty string for a default cell (#257). Named and low-indexed colours go
/// out as the 16 standard codes so the reader resolves them through ITS
/// theme palette, the way the pane does; the 256-cube and truecolour pass
/// through as `38;5;n` / `38;2;r;g;b`.
pub(super) fn cell_sgr(fg: AnsiColor, bg: AnsiColor, flags: Flags) -> String {
    let mut codes: Vec<String> = Vec::new();
    if flags.contains(Flags::BOLD) {
        codes.push("1".into());
    }
    if flags.contains(Flags::DIM) {
        codes.push("2".into());
    }
    if flags.contains(Flags::ITALIC) {
        codes.push("3".into());
    }
    if flags.intersects(
        Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::UNDERCURL
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE,
    ) {
        codes.push("4".into());
    }
    if flags.contains(Flags::INVERSE) {
        codes.push("7".into());
    }
    if flags.contains(Flags::HIDDEN) {
        codes.push("8".into());
    }
    if flags.contains(Flags::STRIKEOUT) {
        codes.push("9".into());
    }
    if let Some(code) = color_sgr(fg, false) {
        codes.push(code);
    }
    if let Some(code) = color_sgr(bg, true) {
        codes.push(code);
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// Whether a cell carries no colour and no attribute the export would
/// write: the cheap test the trailing-blank trim uses, with no SGR string
/// built to find out.
pub(super) fn style_is_default(fg: AnsiColor, bg: AnsiColor, flags: Flags) -> bool {
    let plain = |c: AnsiColor| matches!(c, AnsiColor::Named(n) if named_slot(n).is_none());
    plain(fg)
        && plain(bg)
        && !flags.intersects(
            Flags::BOLD
                | Flags::DIM
                | Flags::ITALIC
                | Flags::UNDERLINE
                | Flags::DOUBLE_UNDERLINE
                | Flags::UNDERCURL
                | Flags::DOTTED_UNDERLINE
                | Flags::DASHED_UNDERLINE
                | Flags::INVERSE
                | Flags::HIDDEN
                | Flags::STRIKEOUT,
        )
}

/// One colour as its SGR parameter(s): `30..37`/`90..97` (or the `40..`
/// / `100..` background forms) for the 16 slots, `38;5;n` beyond them,
/// `38;2;r;g;b` for truecolour, `None` for the default colour.
pub(super) fn color_sgr(c: AnsiColor, background: bool) -> Option<String> {
    let base = if background { 40 } else { 30 };
    let bright = if background { 100 } else { 90 };
    let extended = if background { 48 } else { 38 };
    let slot = |i: u8| {
        if i < 8 {
            (base + i as u16).to_string()
        } else {
            (bright + (i - 8) as u16).to_string()
        }
    };
    match c {
        AnsiColor::Spec(rgb) => Some(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(i) if i < 16 => Some(slot(i)),
        AnsiColor::Indexed(i) => Some(format!("{extended};5;{i}")),
        AnsiColor::Named(named) => named_slot(named).map(slot),
    }
}

/// The 16-slot index a named colour maps to, mirroring `named_to_ratatui`;
/// the terminal's own defaults (`Foreground`, `Background`, cursor colours)
/// have no slot and stay default.
pub(super) fn named_slot(n: NamedColor) -> Option<u8> {
    use NamedColor::*;
    Some(match n {
        Foreground | Background | Cursor | DimForeground | BrightForeground => return None,
        Black | DimBlack => 0,
        Red | DimRed => 1,
        Green | DimGreen => 2,
        Yellow | DimYellow => 3,
        Blue | DimBlue => 4,
        Magenta | DimMagenta => 5,
        Cyan | DimCyan => 6,
        White | DimWhite => 7,
        BrightBlack => 8,
        BrightRed => 9,
        BrightGreen => 10,
        BrightYellow => 11,
        BrightBlue => 12,
        BrightMagenta => 13,
        BrightCyan => 14,
        BrightWhite => 15,
    })
}
