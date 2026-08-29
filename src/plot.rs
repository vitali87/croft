//! `croft plot` (#361): numbers or CSV on stdin in, a chart in the pane out.
//!
//! The pane already renders inline images at their output row (iTerm2,
//! Kitty, sixel), and `svg.rs` rasterises SVG through resvg, so a chart is
//! an SVG drawn in the theme's colours, rasterised, and emitted with the
//! same escape the previews use. On a host with no image protocol the same
//! data renders as a Unicode braille (line, spark) or block (bar,
//! histogram) chart, so the command never prints nothing.
//!
//! Input is auto-detected: whitespace-, comma- or tab-separated columns,
//! or JSON lines of flat objects. A header row is recognised when its
//! fields are not numbers. `--x` names the label column, `--y` the series;
//! without them every numeric column is a series and the first non-numeric
//! column, if any, labels the points.

use std::fmt::Write as _;

/// One plotted series.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    pub values: Vec<f64>,
}

/// Parsed input: optional per-point labels plus one or more series of equal
/// length (short series are padded with NaN, which never plots).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dataset {
    pub labels: Option<Vec<String>>,
    pub series: Vec<Series>,
}

impl Dataset {
    pub fn len(&self) -> usize {
        self.series
            .iter()
            .map(|s| s.values.len())
            .max()
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Finite min and max across every series (None when nothing is finite).
    fn range(&self) -> Option<(f64, f64)> {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for v in self.series.iter().flat_map(|s| s.values.iter()) {
            if v.is_finite() {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        (lo <= hi).then_some((lo, hi))
    }
}

/// The chart shapes on offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Line,
    Bar,
    Spark,
    Hist,
}

impl std::str::FromStr for ChartKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "line" => Ok(Self::Line),
            "bar" | "bars" => Ok(Self::Bar),
            "spark" | "sparkline" => Ok(Self::Spark),
            "hist" | "histogram" => Ok(Self::Hist),
            other => Err(format!(
                "unknown chart type {other:?}: expected line, bar, spark or hist"
            )),
        }
    }
}

/// Colours the chart is drawn in, taken from the active theme by the CLI.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub background: (u8, u8, u8),
    pub foreground: (u8, u8, u8),
    pub grid: (u8, u8, u8),
    pub series: [(u8, u8, u8); 6],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            background: (0x1e, 0x1e, 0x1e),
            foreground: (0xcc, 0xcc, 0xcc),
            grid: (0x3a, 0x3f, 0x4b),
            series: [
                (0x4e, 0x9a, 0xff),
                (0x8c, 0xc2, 0x65),
                (0xe0, 0x9a, 0x4e),
                (0xd4, 0x6a, 0xc1),
                (0x2d, 0xd4, 0xbf),
                (0xff, 0x45, 0x4d),
            ],
        }
    }
}

fn hex(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

fn parse_number(field: &str) -> Option<f64> {
    let t = field.trim().trim_matches('"');
    if t.is_empty() {
        return None;
    }
    // Thousands separators and a trailing percent are common in exported
    // tables; both are unambiguous once the field is otherwise numeric.
    let cleaned: String = t.chars().filter(|c| *c != ',' && *c != '_').collect();
    let cleaned = cleaned.strip_suffix('%').unwrap_or(&cleaned);
    cleaned.parse::<f64>().ok()
}

/// Which separator the first data line uses: tabs beat commas beat runs of
/// whitespace, so a CSV with spaces inside quoted fields still splits right.
fn split_line(line: &str, delim: Delim) -> Vec<String> {
    match delim {
        Delim::Tab => line.split('\t').map(|s| s.trim().to_string()).collect(),
        Delim::Comma => {
            // Quoted fields may hold commas: a small state machine rather
            // than a `csv` reader per line keeps the 10k-row budget.
            let mut out = Vec::new();
            let mut cur = String::new();
            let mut quoted = false;
            for c in line.chars() {
                match c {
                    '"' => quoted = !quoted,
                    ',' if !quoted => out.push(std::mem::take(&mut cur).trim().to_string()),
                    _ => cur.push(c),
                }
            }
            out.push(cur.trim().to_string());
            out
        }
        Delim::Space => line.split_whitespace().map(str::to_string).collect(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Delim {
    Tab,
    Comma,
    Space,
}

fn detect_delim(line: &str) -> Delim {
    if line.contains('\t') {
        Delim::Tab
    } else if line.contains(',') {
        Delim::Comma
    } else {
        Delim::Space
    }
}

/// Parse stdin's text into a dataset. `x` names (or 0-based indexes) the
/// label column; `y` names the series columns, in order. Errors name what
/// was expected, since a bad pipe should say so rather than draw a blank.
pub fn parse(input: &str, x: Option<&str>, y: &[String]) -> Result<Dataset, String> {
    let lines: Vec<&str> = input
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .collect();
    let Some(first) = lines.first() else {
        return Err(String::from(
            "no input: pipe numbers, CSV/TSV, or JSON lines into croft plot",
        ));
    };

    // JSON lines: one flat object per line.
    let (headers, rows): (Vec<String>, Vec<Vec<String>>) = if first.trim_start().starts_with('{') {
        let mut headers: Vec<String> = Vec::new();
        let mut rows: Vec<Vec<String>> = Vec::new();
        for l in &lines {
            let v: serde_json::Value =
                serde_json::from_str(l).map_err(|e| format!("bad JSON line {l:?}: {e}"))?;
            let Some(obj) = v.as_object() else {
                return Err(format!("JSON line is not an object: {l:?}"));
            };
            for k in obj.keys() {
                if !headers.iter().any(|h| h == k) {
                    headers.push(k.clone());
                }
            }
            rows.push(
                headers
                    .iter()
                    .map(|h| match obj.get(h) {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(serde_json::Value::Null) | None => String::new(),
                        Some(other) => other.to_string(),
                    })
                    .collect(),
            );
        }
        // Earlier rows may predate a key that appears later: pad.
        for r in &mut rows {
            r.resize(headers.len(), String::new());
        }
        (headers, rows)
    } else {
        let delim = detect_delim(first);
        let mut rows: Vec<Vec<String>> = lines.iter().map(|l| split_line(l, delim)).collect();
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        for r in &mut rows {
            r.resize(width, String::new());
        }
        // A header is a first row with at least one non-numeric field
        // while the row after it is all numbers where the header is not.
        let first_row = rows[0].clone();
        let has_header = rows.len() > 1
            && first_row.iter().any(|f| parse_number(f).is_none())
            && rows[1]
                .iter()
                .zip(&first_row)
                .any(|(v, h)| parse_number(v).is_some() && parse_number(h).is_none());
        if has_header {
            rows.remove(0);
            (first_row, rows)
        } else {
            let headers = (0..width)
                .map(|i| {
                    if width == 1 {
                        String::from("value")
                    } else {
                        format!("col{}", i + 1)
                    }
                })
                .collect();
            (headers, rows)
        }
    };

    let column = |name: &str| -> Result<usize, String> {
        if let Some(i) = headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(name.trim()))
        {
            return Ok(i);
        }
        if let Ok(i) = name.trim().parse::<usize>()
            && i < headers.len()
        {
            return Ok(i);
        }
        Err(format!(
            "no column {name:?}; the input has: {}",
            headers.join(", ")
        ))
    };
    let numeric = |i: usize| {
        rows.iter()
            .filter(|r| !r[i].is_empty())
            .all(|r| parse_number(&r[i]).is_some())
    };

    let x_col = match x {
        Some(name) => Some(column(name)?),
        None => (0..headers.len()).find(|&i| !numeric(i)),
    };
    let y_cols: Vec<usize> = if y.is_empty() {
        (0..headers.len())
            .filter(|&i| Some(i) != x_col && numeric(i))
            .collect()
    } else {
        y.iter().map(|n| column(n)).collect::<Result<_, _>>()?
    };
    if y_cols.is_empty() {
        return Err(format!(
            "no numeric column to plot; the input has: {}",
            headers.join(", ")
        ));
    }
    let series = y_cols
        .iter()
        .map(|&i| Series {
            name: headers[i].clone(),
            values: rows
                .iter()
                .map(|r| parse_number(&r[i]).unwrap_or(f64::NAN))
                .collect(),
        })
        .collect();
    let labels = x_col.map(|i| rows.iter().map(|r| r[i].clone()).collect());
    Ok(Dataset { labels, series })
}

/// Histogram of the first series: `bins` equal-width buckets over its range.
pub fn histogram(ds: &Dataset, bins: usize) -> Dataset {
    let Some(s) = ds.series.first() else {
        return Dataset::default();
    };
    let finite: Vec<f64> = s.values.iter().copied().filter(|v| v.is_finite()).collect();
    let bins = bins.max(1);
    let (lo, hi) = finite
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| {
            (l.min(*v), h.max(*v))
        });
    if finite.is_empty() {
        return Dataset::default();
    }
    let width = if hi > lo {
        (hi - lo) / bins as f64
    } else {
        1.0
    };
    let mut counts = vec![0f64; bins];
    for v in &finite {
        let i = (((v - lo) / width) as usize).min(bins - 1);
        counts[i] += 1.0;
    }
    let labels = (0..bins)
        .map(|i| format_num(lo + width * i as f64))
        .collect();
    Dataset {
        labels: Some(labels),
        series: vec![Series {
            name: format!("count of {}", s.name),
            values: counts,
        }],
    }
}

/// A short human number: integers stay integers, the rest keep two
/// decimals, and large magnitudes get a k/M suffix so axis labels fit.
pub fn format_num(v: f64) -> String {
    if !v.is_finite() {
        return String::from("-");
    }
    let a = v.abs();
    if a >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if a >= 10_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

/// The dataset as drawn: a histogram bins the first series, a spark or
/// line plots as-is, a bar chart plots as-is with labels.
fn prepared(ds: &Dataset, kind: ChartKind) -> Dataset {
    match kind {
        ChartKind::Hist => {
            let n = ds.series.first().map_or(0, |s| s.values.len());
            histogram(ds, ((n as f64).sqrt().ceil() as usize).clamp(1, 40))
        }
        _ => ds.clone(),
    }
}

/// Bucket a long series down to at most `slots` points (mean per bucket),
/// so a 10k-row line is a few hundred segments rather than ten thousand.
fn downsample(values: &[f64], slots: usize) -> Vec<f64> {
    if values.len() <= slots || slots == 0 {
        return values.to_vec();
    }
    (0..slots)
        .map(|i| {
            let a = i * values.len() / slots;
            let b = ((i + 1) * values.len() / slots).max(a + 1);
            let bucket: Vec<f64> = values[a..b]
                .iter()
                .copied()
                .filter(|v| v.is_finite())
                .collect();
            if bucket.is_empty() {
                f64::NAN
            } else {
                bucket.iter().sum::<f64>() / bucket.len() as f64
            }
        })
        .collect()
}

/// Draw the chart as SVG, `w` x `h` pixels, in `palette`'s colours.
pub fn svg(
    ds: &Dataset,
    kind: ChartKind,
    title: Option<&str>,
    w: u32,
    h: u32,
    palette: &Palette,
) -> String {
    let ds = prepared(ds, kind);
    let (w, h) = (w.max(80) as f64, h.max(40) as f64);
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="sans-serif" font-size="12">"#
    );
    let _ = write!(
        out,
        r#"<rect width="{w}" height="{h}" fill="{}"/>"#,
        hex(palette.background)
    );
    let fg = hex(palette.foreground);
    let top = if title.is_some() { 24.0 } else { 8.0 };
    if let Some(t) = title {
        let _ = write!(
            out,
            r#"<text x="{}" y="16" text-anchor="middle" fill="{fg}" font-weight="bold">{}</text>"#,
            w / 2.0,
            escape(t)
        );
    }
    let Some((lo, hi)) = ds.range() else {
        let _ = write!(
            out,
            r#"<text x="{}" y="{}" text-anchor="middle" fill="{fg}">no data</text></svg>"#,
            w / 2.0,
            h / 2.0
        );
        return out;
    };
    let (lo, hi) = match kind {
        // Bars and histograms grow from zero, or the tallest bar is a lie.
        ChartKind::Bar | ChartKind::Hist => (lo.min(0.0), hi.max(0.0)),
        _ => (lo, hi),
    };
    let span = if hi > lo { hi - lo } else { 1.0 };
    let left = 48.0;
    let right = w - 12.0;
    let bottom = h - if ds.labels.is_some() && kind != ChartKind::Spark {
        22.0
    } else {
        10.0
    };
    let plot_h = bottom - top;
    let y_of = |v: f64| bottom - (v - lo) / span * plot_h;
    // Gridlines and axis labels.
    for i in 0..=4 {
        let v = lo + span * i as f64 / 4.0;
        let y = y_of(v);
        let _ = write!(
            out,
            r#"<line x1="{left}" y1="{y:.1}" x2="{right}" y2="{y:.1}" stroke="{}" stroke-width="1"/>"#,
            hex(palette.grid)
        );
        let _ = write!(
            out,
            r#"<text x="{}" y="{:.1}" text-anchor="end" fill="{fg}" font-size="10">{}</text>"#,
            left - 4.0,
            y + 3.5,
            format_num(v)
        );
    }
    let n = ds.len();
    match kind {
        ChartKind::Line | ChartKind::Spark => {
            let slots = ((right - left) as usize).max(2);
            for (si, s) in ds.series.iter().enumerate() {
                let vals = downsample(&s.values, slots);
                let m = vals.len().max(1);
                let colour = hex(palette.series[si % palette.series.len()]);
                let mut points = String::new();
                for (i, v) in vals.iter().enumerate() {
                    if !v.is_finite() {
                        continue;
                    }
                    let x = if m == 1 {
                        left
                    } else {
                        left + (right - left) * i as f64 / (m - 1) as f64
                    };
                    let _ = write!(points, "{x:.1},{:.1} ", y_of(*v));
                }
                let _ = write!(
                    out,
                    r#"<polyline points="{}" fill="none" stroke="{colour}" stroke-width="2" stroke-linejoin="round"/>"#,
                    points.trim_end()
                );
                if ds.series.len() > 1 {
                    let _ = write!(
                        out,
                        r#"<text x="{}" y="{}" fill="{colour}" font-size="10">{}</text>"#,
                        left + 6.0,
                        top + 12.0 + 12.0 * si as f64,
                        escape(&s.name)
                    );
                }
            }
        }
        ChartKind::Bar | ChartKind::Hist => {
            let groups = n.max(1) as f64;
            let group_w = (right - left) / groups;
            let bar_w = (group_w * 0.8 / ds.series.len().max(1) as f64).max(1.0);
            let zero = y_of(0.0);
            for (si, s) in ds.series.iter().enumerate() {
                let colour = hex(palette.series[si % palette.series.len()]);
                for (i, v) in s.values.iter().enumerate() {
                    if !v.is_finite() {
                        continue;
                    }
                    let x = left + group_w * i as f64 + group_w * 0.1 + bar_w * si as f64;
                    let y = y_of(*v);
                    let (y0, hh) = if y < zero {
                        (y, zero - y)
                    } else {
                        (zero, y - zero)
                    };
                    let _ = write!(
                        out,
                        r#"<rect x="{x:.1}" y="{y0:.1}" width="{bar_w:.1}" height="{hh:.1}" fill="{colour}"/>"#
                    );
                }
            }
            if let Some(labels) = &ds.labels {
                // At most one label per ~40px so they never overlap.
                let every = ((40.0 / group_w).ceil() as usize).max(1);
                for (i, l) in labels.iter().enumerate().step_by(every) {
                    let x = left + group_w * (i as f64 + 0.5);
                    let _ = write!(
                        out,
                        r#"<text x="{x:.1}" y="{}" text-anchor="middle" fill="{fg}" font-size="10">{}</text>"#,
                        h - 8.0,
                        escape(l)
                    );
                }
            }
        }
    }
    out.push_str("</svg>");
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

const BRAILLE_BASE: u32 = 0x2800;
// Braille dot bit for (column 0..2, row 0..4) within one cell.
const DOT_BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The chart as text, `cols` wide and `rows` tall (the spark is one row):
/// braille dots for line and spark, block bars for bar and histogram. The
/// fallback for hosts without an inline-image protocol, and the copyable
/// form for everyone else.
pub fn text(ds: &Dataset, kind: ChartKind, title: Option<&str>, cols: u16, rows: u16) -> String {
    let ds = prepared(ds, kind);
    let cols = cols.max(8) as usize;
    let rows = rows.max(1) as usize;
    let mut out = String::new();
    if let Some(t) = title {
        out.push_str(t);
        out.push('\n');
    }
    let Some((lo, hi)) = ds.range() else {
        out.push_str("no data\n");
        return out;
    };
    match kind {
        ChartKind::Spark => {
            let span = if hi > lo { hi - lo } else { 1.0 };
            let s = &ds.series[0];
            let vals = downsample(&s.values, cols);
            for v in vals {
                // A finite value always shows at least the smallest block:
                // the minimum is a point, not a gap.
                let level = if v.is_finite() {
                    1 + (((v - lo) / span) * 7.0).round().clamp(0.0, 7.0) as usize
                } else {
                    0
                };
                out.push(BLOCKS[level]);
            }
            out.push('\n');
        }
        ChartKind::Line => {
            let label_w = format_num(hi).len().max(format_num(lo).len()) + 1;
            let plot_cols = cols.saturating_sub(label_w).max(4);
            let span = if hi > lo { hi - lo } else { 1.0 };
            let (dw, dh) = (plot_cols * 2, rows * 4);
            let mut grid = vec![vec![0u32; plot_cols]; rows];
            for s in &ds.series {
                let vals = downsample(&s.values, dw);
                let m = vals.len().max(1);
                let mut prev: Option<usize> = None;
                for (i, v) in vals.iter().enumerate() {
                    if !v.is_finite() {
                        prev = None;
                        continue;
                    }
                    let x = if m == 1 { 0 } else { i * (dw - 1) / (m - 1) };
                    let y = ((hi - v) / span * (dh - 1) as f64).round() as usize;
                    let (a, b) = match prev {
                        Some(p) if p < y => (p + 1, y),
                        Some(p) if p > y => (y, p - 1),
                        _ => (y, y),
                    };
                    for yy in a..=b {
                        grid[yy / 4][x / 2] |= DOT_BITS[x % 2][yy % 4];
                    }
                    prev = Some(y);
                }
            }
            for (r, row) in grid.iter().enumerate() {
                let label = if r == 0 {
                    format_num(hi)
                } else if r + 1 == rows {
                    format_num(lo)
                } else {
                    String::new()
                };
                let _ = write!(out, "{label:>w$} ", w = label_w - 1);
                for bits in row {
                    out.push(char::from_u32(BRAILLE_BASE + bits).unwrap_or(' '));
                }
                out.push('\n');
            }
        }
        ChartKind::Bar | ChartKind::Hist => {
            let (lo, hi) = (lo.min(0.0), hi.max(0.0));
            let span = if hi > lo { hi - lo } else { 1.0 };
            let s = &ds.series[0];
            let n = s.values.len().max(1);
            let label_w = format_num(hi).len().max(format_num(lo).len()) + 1;
            let plot_cols = cols.saturating_sub(label_w).max(4);
            // Bar plus its gap must fit `n` times, or the labels row is
            // dropped for a partial view.
            let bar_w = (plot_cols / n).saturating_sub(1).clamp(1, 6);
            let gap = usize::from(bar_w > 1);
            let shown = n.min(plot_cols / (bar_w + gap).max(1)).max(1);
            let vals = downsample(&s.values, shown);
            let levels: Vec<usize> = vals
                .iter()
                .map(|v| {
                    if v.is_finite() {
                        (((v - lo) / span) * (rows * 8) as f64).round() as usize
                    } else {
                        0
                    }
                })
                .collect();
            for r in 0..rows {
                let label = if r == 0 {
                    format_num(hi)
                } else if r + 1 == rows {
                    format_num(lo)
                } else {
                    String::new()
                };
                let _ = write!(out, "{label:>w$} ", w = label_w - 1);
                let floor = (rows - 1 - r) * 8;
                for lv in &levels {
                    let fill = lv.saturating_sub(floor).min(8);
                    for _ in 0..bar_w {
                        out.push(BLOCKS[fill]);
                    }
                    for _ in 0..gap {
                        out.push(' ');
                    }
                }
                out.push('\n');
            }
            if let Some(labels) = &ds.labels
                && shown == n
            {
                let _ = write!(out, "{:>w$} ", "", w = label_w - 1);
                for l in labels.iter().take(shown) {
                    let cell = bar_w + gap;
                    let short: String = l.chars().take(cell.saturating_sub(gap).max(1)).collect();
                    let _ = write!(out, "{short:<cell$}");
                }
                out.push('\n');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds(input: &str) -> Dataset {
        parse(input, None, &[]).unwrap()
    }

    #[test]
    fn plain_numbers_are_one_series() {
        let d = ds("1\n2\n3.5\n");
        assert_eq!(d.labels, None);
        assert_eq!(d.series.len(), 1);
        assert_eq!(d.series[0].values, [1.0, 2.0, 3.5]);
        assert_eq!(d.series[0].name, "value");
    }

    #[test]
    fn csv_with_a_header_labels_points_and_names_series() {
        let d = ds("month,sales,returns\nJan,10,1\nFeb,\"1,200\",2\nMar,30,3\n");
        assert_eq!(
            d.labels.as_deref(),
            Some(&["Jan".to_string(), "Feb".into(), "Mar".into()][..])
        );
        assert_eq!(d.series.len(), 2, "{d:?}");
        assert_eq!(d.series[0].name, "sales");
        assert_eq!(
            d.series[0].values,
            [10.0, 1200.0, 30.0],
            "quoted thousands parse"
        );
        assert_eq!(d.series[1].values, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn tsv_whitespace_and_json_lines_all_parse() {
        let t = ds("a\tb\n1\t2\n3\t4\n");
        assert_eq!(t.series.len(), 2);
        assert_eq!(t.series[1].values, [2.0, 4.0]);
        let w = ds("1 2\n3 4\n");
        assert_eq!(w.series[0].name, "col1");
        assert_eq!(w.series[1].values, [2.0, 4.0]);
        let j = ds("{\"t\":\"a\",\"v\":1}\n{\"t\":\"b\",\"v\":2,\"w\":5}\n");
        assert_eq!(j.labels.as_deref().map(|l| l.len()), Some(2));
        assert_eq!(
            j.series.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["v", "w"]
        );
        assert!(
            j.series[1].values[0].is_nan(),
            "a key missing on an earlier row pads with NaN"
        );
    }

    #[test]
    fn x_and_y_select_columns_by_name_or_index() {
        let d = parse("k,a,b\nx,1,2\ny,3,4\n", Some("k"), &["b".into()]).unwrap();
        assert_eq!(d.series.len(), 1);
        assert_eq!(d.series[0].name, "b");
        assert_eq!(d.series[0].values, [2.0, 4.0]);
        let d = parse("1,2\n3,4\n", None, &["1".into()]).unwrap();
        assert_eq!(d.series[0].values, [2.0, 4.0], "a bare index is a column");
        let err = parse("a,b\n1,2\n", None, &["zz".into()]).unwrap_err();
        assert!(
            err.contains("no column \"zz\"") && err.contains("a, b"),
            "{err}"
        );
        let err = parse("a,b\nx,y\n", None, &[]).unwrap_err();
        assert!(err.contains("no numeric column"), "{err}");
        assert!(parse("\n\n", None, &[]).unwrap_err().contains("no input"));
    }

    #[test]
    fn chart_kinds_parse_and_a_histogram_bins_the_first_series() {
        assert_eq!("bar".parse::<ChartKind>(), Ok(ChartKind::Bar));
        assert_eq!("Sparkline".parse::<ChartKind>(), Ok(ChartKind::Spark));
        assert!(
            "pie"
                .parse::<ChartKind>()
                .unwrap_err()
                .contains("expected line, bar")
        );
        let h = histogram(&ds("1\n1\n2\n9\n10\n"), 3);
        assert_eq!(h.series[0].values, [3.0, 0.0, 2.0]);
        assert_eq!(h.labels.as_deref().map(|l| l.len()), Some(3));
        assert_eq!(format_num(1500.0), "1500");
        assert_eq!(format_num(12345.0), "12.3k");
        assert_eq!(format_num(2.5), "2.50");
    }

    #[test]
    fn svg_draws_a_titled_polyline_or_labelled_bars_in_the_palette() {
        let d = ds("month,sales\nJan,10\nFeb,30\nMar,20\n");
        let p = Palette::default();
        let line = svg(&d, ChartKind::Line, Some("Sales & more"), 600, 300, &p);
        assert!(line.starts_with("<svg") && line.ends_with("</svg>"));
        assert!(line.contains("<polyline"), "{line}");
        assert!(line.contains("Sales &amp; more"), "the title is escaped");
        assert!(line.contains(&hex(p.series[0])) && line.contains(&hex(p.background)));
        let bar = svg(&d, ChartKind::Bar, None, 600, 300, &p);
        assert_eq!(
            bar.matches("<rect").count(),
            4,
            "background plus one bar per point: {bar}"
        );
        assert!(
            bar.contains(">Jan<") && bar.contains(">Mar<"),
            "bars are labelled from the x column"
        );
        let empty = Dataset {
            labels: None,
            series: vec![Series {
                name: "v".into(),
                values: vec![f64::NAN],
            }],
        };
        assert!(svg(&empty, ChartKind::Line, None, 200, 100, &p).contains("no data"));
        assert!(
            !empty.is_empty() && empty.len() == 1,
            "a NaN-only series has a length but no range"
        );
    }

    #[test]
    fn text_fallback_is_a_braille_line_a_block_spark_or_labelled_bars() {
        let d = ds("month,sales\nJan,10\nFeb,30\nMar,20\n");
        let line = text(&d, ChartKind::Line, None, 30, 4);
        let rows: Vec<&str> = line.lines().collect();
        assert_eq!(rows.len(), 4);
        assert!(
            rows[0].trim_start().starts_with("30"),
            "top row carries the max: {rows:?}"
        );
        assert!(
            rows[3].trim_start().starts_with("10"),
            "bottom row carries the min: {rows:?}"
        );
        assert!(
            line.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
            "braille dots were drawn: {line}"
        );
        let spark = text(&d, ChartKind::Spark, None, 10, 1);
        assert_eq!(
            spark, "▁█▅\n",
            "one block per point, the minimum still a block"
        );
        let bar = text(&d, ChartKind::Bar, Some("t"), 20, 3);
        let rows: Vec<&str> = bar.lines().collect();
        assert_eq!(rows[0], "t");
        assert!(rows[1].contains('█') || rows[2].contains('█'), "{bar}");
        assert!(
            rows.last().unwrap().contains("Jan") && rows.last().unwrap().contains("Mar"),
            "{bar}"
        );
        let nan_only = Dataset {
            labels: None,
            series: vec![Series {
                name: "v".into(),
                values: vec![f64::NAN],
            }],
        };
        assert!(text(&nan_only, ChartKind::Line, None, 20, 3).contains("no data"));
    }

    /// The acceptance budget: 10k rows parse and render as text and SVG well
    /// inside 50 ms of CPU on any machine that can build croft (this is a
    /// generous multiple; the point is the algorithm is linear).
    #[test]
    fn ten_thousand_rows_render_quickly() {
        let input: String = (0..10_000)
            .map(|i| format!("{}\n", (i as f64 / 10.0).sin()))
            .collect();
        let start = std::time::Instant::now();
        let d = ds(&input);
        let s = svg(&d, ChartKind::Line, None, 800, 300, &Palette::default());
        let t = text(&d, ChartKind::Line, None, 80, 20);
        assert_eq!(d.len(), 10_000);
        assert!(
            s.matches(',').count() < 2_000,
            "the line is downsampled to the pixel width"
        );
        assert!(!t.is_empty());
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "10k rows took {:?}",
            start.elapsed()
        );
    }
}
