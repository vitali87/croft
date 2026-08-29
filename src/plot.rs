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
    let t = t.strip_suffix('%').unwrap_or(t);
    // A thousands separator is only ever in thousands position: groups of
    // three after a leading group of one to three digits. Anything else
    // with a comma ("1,2,3") is several fields, never one number.
    let cleaned = strip_thousands(t)?;
    cleaned.parse::<f64>().ok()
}

fn strip_thousands(t: &str) -> Option<String> {
    let sep = if t.contains(',') {
        ','
    } else if t.contains('_') {
        '_'
    } else {
        return Some(t.to_string());
    };
    let (sign, body) = match t.strip_prefix('-') {
        Some(b) => ("-", b),
        None => ("", t.strip_prefix('+').unwrap_or(t)),
    };
    let (int, frac) = match body.find('.') {
        Some(i) => (&body[..i], Some(&body[i..])),
        None => (body, None),
    };
    let mut groups = int.split(sep);
    let first = groups.next()?;
    // The leading group is 1-3 digits with no leading zero: "0,100" is a
    // decimal comma or two fields, never one hundred.
    if first.is_empty() || first.len() > 3 || !first.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut groups = groups.peekable();
    // A leading zero before a separator ("0,100", "01,000") is never a
    // thousands grouping.
    if first.starts_with('0') && groups.peek().is_some() {
        return None;
    }
    let mut out = format!("{sign}{first}");
    for g in groups {
        if g.len() != 3 || !g.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        out.push_str(g);
    }
    if let Some(f) = frac {
        if f.contains(sep) {
            return None;
        }
        out.push_str(f);
    }
    Some(out)
}

/// One line's fields under `delim`; a comma-separated line honours quoted
/// fields, so a CSV with spaces or commas inside quotes still splits right.
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

/// The delimiter, chosen over the first few lines rather than the first
/// alone: the one that splits the most sampled lines into several fields
/// (tabs, then commas, then runs of whitespace), the first line breaking
/// ties. A CSV whose first row is one column still reads as a CSV; a
/// whitespace file with one stray comma stays whitespace-separated.
fn detect_delim(lines: &[&str]) -> Delim {
    let sample: Vec<&str> = lines.iter().take(8).copied().collect();
    let splits = |d: Delim| sample.iter().filter(|l| split_line(l, d).len() > 1).count();
    let first_splits = |d: Delim| sample.first().is_some_and(|l| split_line(l, d).len() > 1);
    let mut best = Delim::Space;
    let mut best_score = (0usize, false);
    for d in [Delim::Tab, Delim::Comma, Delim::Space] {
        let score = (splits(d), first_splits(d));
        if score > best_score {
            best = d;
            best_score = score;
        }
    }
    best
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
        let delim = detect_delim(&lines);
        let mut rows: Vec<Vec<String>> = lines.iter().map(|l| split_line(l, delim)).collect();
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        for r in &mut rows {
            r.resize(width, String::new());
        }
        // A header is a first row with at least one non-numeric field
        // while the row after it is all numbers where the header is not.
        let first_row = rows[0].clone();
        let has_header = rows.len() > 1
            && first_row
                .iter()
                .any(|f| !f.is_empty() && parse_number(f).is_none())
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
        let cols = y.iter().map(|n| column(n)).collect::<Result<Vec<_>, _>>()?;
        if let Some(&bad) = cols.iter().find(|&&i| !numeric(i)) {
            return Err(format!(
                "column {:?} is not numeric; the input has: {}",
                headers[bad],
                headers.join(", ")
            ));
        }
        cols
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
    if finite.is_empty() {
        return Dataset::default();
    }
    let (lo, hi) = finite
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| {
            (l.min(*v), h.max(*v))
        });
    // Constant data is one bin; inventing neighbours would label values
    // that were never there.
    let bins = if hi > lo { bins.max(1) } else { 1 };
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
    // Band edges sit where the rounded lower band would print the next
    // unit ("1000.0k"), so no label ever exceeds seven characters.
    if a >= 999_950_000_000.0 {
        format!("{:.1e}", v)
    } else if a >= 999_950_000.0 {
        format!("{:.1}G", v / 1_000_000_000.0)
    } else if a >= 999_950.0 {
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

/// The half-open row range bucket `i` of `slots` covers when `n` rows are
/// squeezed into `slots` bars. One place for the arithmetic, so the bar a
/// bucket averages and the label it wears can never name different rows.
/// Every row lands in exactly one bucket and no bucket is empty while
/// `slots <= n`; with more slots than rows, buckets past the last row
/// repeat it rather than come back empty.
fn bucket_range(i: usize, n: usize, slots: usize) -> (usize, usize) {
    debug_assert!(slots > 0, "a bucket range needs at least one slot");
    let a = i * n / slots;
    let b = ((i + 1) * n / slots).max(a + 1);
    (a, b)
}

/// Bucket a long series down to at most `slots` points (mean per bucket),
/// so a 10k-row line is a few hundred segments rather than ten thousand.
fn downsample(values: &[f64], slots: usize) -> Vec<f64> {
    if values.len() <= slots || slots == 0 {
        return values.to_vec();
    }
    (0..slots)
        .map(|i| {
            let (a, b) = bucket_range(i, values.len(), slots);
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
    let (w, h) = (w.max(80) as f64, h.max(20) as f64);
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
    // A one-row spark has no room for a title.
    let title = title.filter(|_| h >= 40.0);
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
    let pad_b = if ds.labels.is_some() && kind != ChartKind::Spark {
        22.0
    } else {
        10.0
    };
    // A height too small for the axis padding yields the padding, not the
    // plot: a negative plot height would invert every y and draw the whole
    // chart above the viewBox.
    let bottom = (h - pad_b).max(top + 1.0);
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
                // One legend line per series, stacked from the top. A chart
                // too short for the WHOLE legend shows none of it: a legend
                // naming only the first few series would have a reader map
                // colours to the wrong names, and a line below the viewBox
                // is clipped away.
                let legend_y = top + 12.0 + 12.0 * si as f64;
                let last_legend_y = top + 12.0 + 12.0 * (ds.series.len() - 1) as f64;
                if ds.series.len() > 1 && last_legend_y <= bottom {
                    let _ = write!(
                        out,
                        r#"<text x="{}" y="{}" fill="{colour}" font-size="10">{}</text>"#,
                        left + 6.0,
                        legend_y,
                        escape(&s.name)
                    );
                }
            }
        }
        ChartKind::Bar | ChartKind::Hist => {
            // One bar per pixel column at most: 10k rows bucket to the
            // width like the line path, and the labels follow the buckets.
            let slots = ((right - left) as usize).max(2);
            // A group has to be wide enough to hold one bar per series. When
            // it is not, `bar_w`'s 1px floor makes the group's bars wider than
            // the group itself, and every series after the first is drawn over
            // the FOLLOWING group's x positions. Fewer, wider groups is the
            // honest answer: the chart already buckets to fit the width, and
            // this is the same bucketing with the series count taken into
            // account.
            let per_group = ds.series.len().max(1);
            let shown = n.min(slots / per_group).max(1);
            debug_assert!(shown <= n, "buckets never outnumber rows");
            // A bucket's bar is the mean of its rows, so its label names the
            // rows it covers: one label for a single row, `first…last` for
            // several - unless the rows share a label, when repeating it
            // says nothing.
            let labels = ds.labels.as_ref().map(|labels| {
                (0..shown)
                    .map(|i| {
                        let (start, end) = bucket_range(i, n, shown);
                        let first = labels.get(start).cloned().unwrap_or_default();
                        let last_row = end - 1;
                        let last = labels.get(last_row).cloned().unwrap_or_default();
                        if last_row > start && last != first {
                            format!("{first}\u{2026}{last}")
                        } else {
                            first
                        }
                    })
                    .collect::<Vec<_>>()
            });
            let groups = shown.max(1) as f64;
            let group_w = (right - left) / groups;
            let bar_w = (group_w * 0.8 / ds.series.len().max(1) as f64).max(1.0);
            let zero = y_of(0.0);
            for (si, s) in ds.series.iter().enumerate() {
                let colour = hex(palette.series[si % palette.series.len()]);
                let vals = downsample(&s.values, shown);
                for (i, v) in vals.iter().enumerate() {
                    if !v.is_finite() {
                        continue;
                    }
                    let x = left + group_w * i as f64 + group_w * 0.1 + bar_w * si as f64;
                    let y = y_of(*v);
                    // An exact zero still draws one pixel, as the text
                    // chart's zero bar keeps one unit.
                    let (y0, hh) = if y < zero {
                        (y, (zero - y).max(1.0))
                    } else {
                        (y0_floor(zero, y), (y - zero).max(1.0))
                    };
                    let _ = write!(
                        out,
                        r#"<rect x="{x:.1}" y="{y0:.1}" width="{bar_w:.1}" height="{hh:.1}" fill="{colour}"/>"#
                    );
                }
            }
            if let Some(labels) = &labels {
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

/// Columns a glyph occupies: 2 for the East Asian wide and emoji ranges,
/// else 1 (zero-width combining marks are rare in labels and rounded up).
fn display_width(c: char) -> usize {
    let u = c as u32;
    // 2E80..A4CF sweeps in a few narrow blocks (Yijing, Lisu, Vai): the
    // over-approximation pads a little too much, which never misaligns a
    // bar the way under-padding would.
    if (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE4F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1FAFF).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u)
    {
        2
    } else {
        1
    }
}

/// Where a bar that grows from `zero` starts: at `zero` when it has
/// height, one pixel above it when the value is exactly zero.
fn y0_floor(zero: f64, y: f64) -> f64 {
    if y > zero { zero } else { zero - 1.0 }
}

/// Text-node escaping only: every escaped value lands between tags, never
/// in an attribute, so quotes need no treatment here.
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
            // Bars grow from ZERO, as the SVG's do: a negative value hangs
            // down from the zero row, and an exact zero still shows one
            // unit so it reads as a point rather than a gap.
            let units = (rows * 8) as isize;
            let zero_units = (((0.0 - lo) / span) * units as f64).round() as isize;
            let spans: Vec<Option<(isize, isize)>> = vals
                .iter()
                .map(|v| {
                    if !v.is_finite() {
                        return None;
                    }
                    let mut level = (((v - lo) / span) * units as f64).round() as isize;
                    if level == zero_units {
                        level += if zero_units >= units { -1 } else { 1 };
                    }
                    Some((level.min(zero_units), level.max(zero_units)))
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
                let floor = ((rows - 1 - r) * 8) as isize;
                for sp in &spans {
                    let fill = match sp {
                        Some((from, to)) => {
                            ((to - floor).clamp(0, 8) - (from - floor).clamp(0, 8)) as usize
                        }
                        None => 0,
                    };
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
                    let budget = cell.saturating_sub(gap).max(1);
                    // Wide glyphs (CJK, emoji) take two columns: truncate and
                    // pad by display width so labels stay under their bars.
                    let mut short = String::new();
                    let mut width = 0;
                    for c in l.chars() {
                        let cw = display_width(c);
                        if width + cw > budget {
                            break;
                        }
                        short.push(c);
                        width += cw;
                    }
                    out.push_str(&short);
                    for _ in width..cell {
                        out.push(' ');
                    }
                }
                out.push('\n');
            }
        }
    }
    // The text bar and spark paths draw ds.series[0] only, while the SVG
    // draws every series. Default `--y` selection makes every numeric column
    // a series, and this path is what a non-TTY, a `--text` run, or a
    // terminal without an inline image protocol gets, so silently charting
    // one column of six is the common case rather than an exotic one.
    // Naming what was left out is cheaper than a second chart layout and
    // stops the output from being quietly wrong.
    if matches!(kind, ChartKind::Bar | ChartKind::Hist | ChartKind::Spark) && ds.series.len() > 1 {
        let shown = &ds.series[0].name;
        let rest = ds.series.len() - 1;
        out.push_str(&format!(
            "(showing {shown}; {rest} more series {} not drawn in text mode)\n",
            if rest == 1 { "is" } else { "are" }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds(input: &str) -> Dataset {
        parse(input, None, &[]).unwrap()
    }

    /// Review finding: with several series and many rows, a group could be
    /// narrower than the bars it hosts, so every series after the first was
    /// drawn over the FOLLOWING group's x positions.
    #[test]
    fn grouped_bars_never_overflow_their_group() {
        // Six series and far more rows than a narrow chart has columns.
        let mut input = String::from("a,b,c,d,e,f\n");
        for i in 0..500 {
            input.push_str(&format!("{i},{i},{i},{i},{i},{i}\n"));
        }
        let d = ds(&input);
        assert_eq!(d.series.len(), 6);

        let out = svg(&d, ChartKind::Bar, None, 320, 200, &Palette::default());

        // Collect (x, width) for every rect, then drop the ones that are
        // chart furniture rather than bars. Measuring "the widest rect" alone
        // picked up the 320px background and compared bars against it, which
        // is the wrong object: the test failed while the code was right.
        let rects: Vec<(f64, f64)> = out
            .split("<rect")
            .skip(1)
            .filter_map(|r| {
                let x = r
                    .split("x=\"")
                    .nth(1)?
                    .split('"')
                    .next()?
                    .parse::<f64>()
                    .ok()?;
                let w = r
                    .split("width=\"")
                    .nth(1)?
                    .split('"')
                    .next()?
                    .parse::<f64>()
                    .ok()?;
                Some((x, w))
            })
            .collect();
        let bars: Vec<(f64, f64)> = rects.iter().copied().filter(|(_, w)| *w < 100.0).collect();
        assert!(bars.len() > 10, "the chart drew bars: {} found", bars.len());

        // The defect's signature is two bars sharing an x: when a group is
        // narrower than its bars, series `si` of group `i` lands on exactly
        // the column of series 0 of group `i + si`. Comparing consecutive
        // gaps against the bar width does NOT see that, because the
        // overlapping bars are one width apart like every other pair; the
        // first version of this test made that comparison and passed against
        // the bug.
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (x, _) in &bars {
            *seen.entry(format!("{x:.3}")).or_default() += 1;
        }
        let shared: Vec<_> = seen.iter().filter(|(_, n)| **n > 1).collect();
        assert!(
            shared.is_empty(),
            "{} x positions carry more than one bar, so bars from different \
             groups are painted over each other: {:?}",
            shared.len(),
            shared.iter().take(3).collect::<Vec<_>>()
        );
    }

    /// Review finding: the text bar and spark paths draw the first series
    /// only, while the SVG draws them all, and the text path is what a
    /// non-TTY or `--text` run gets. Silently charting one column of six is
    /// the common case, so the output says what it left out.
    #[test]
    fn the_text_chart_names_the_series_it_does_not_draw() {
        let d = ds("cpu,mem,disk\n1,2,3\n4,5,6\n");
        assert_eq!(d.series.len(), 3);

        let out = text(&d, ChartKind::Bar, None, 40, 8);
        assert!(
            out.contains("cpu") && out.contains("2 more series"),
            "the note must name what was drawn and how many were not: {out}"
        );

        // A single series has nothing to report, and must not gain a note.
        let one = ds("cpu\n1\n4\n");
        let out = text(&one, ChartKind::Bar, None, 40, 8);
        assert!(
            !out.contains("more series"),
            "a single-series chart drops nothing: {out}"
        );
    }

    #[test]
    fn separators_count_only_in_thousands_position() {
        assert_eq!(parse_number("1,200"), Some(1200.0));
        assert_eq!(parse_number("1,234,567.5"), Some(1234567.5));
        assert_eq!(parse_number("-1_000"), Some(-1000.0));
        assert_eq!(parse_number("12%"), Some(12.0));
        assert_eq!(
            parse_number("1,2,3"),
            None,
            "several fields, never one number"
        );
        assert_eq!(
            parse_number("+1,000"),
            Some(1000.0),
            "an explicit plus is a sign"
        );
        assert_eq!(
            parse_number("0,100"),
            None,
            "a leading zero is not a thousands group"
        );
        assert_eq!(parse_number("0.5"), Some(0.5));
        assert_eq!(parse_number("1,,,2"), None);
        assert_eq!(parse_number("1_0_0"), None);
        // A CSV whose first row is one column still splits on commas.
        let d = ds("10\n1,2\n3,4\n");
        assert_eq!(d.series.len(), 2, "{d:?}");
        assert_eq!(d.series[0].values, [10.0, 1.0, 3.0]);
        assert!(d.series[1].values[0].is_nan());
        assert_eq!(d.series[1].values[1..], [2.0, 4.0]);
        let err = parse("k,a\nx,1\ny,2\n", None, &["k".into()]).unwrap_err();
        assert!(err.contains("\"k\" is not numeric"), "{err}");
        // Delimiter votes: a line with both a tab and a comma is a TSV (tabs
        // win the tie), an empty sample is whitespace, and a TSV whose first
        // line has no tab still reads as a TSV.
        assert_eq!(detect_delim(&["a\tb,c", "1\t2,3"]), Delim::Tab);
        assert_eq!(detect_delim(&[]), Delim::Space);
        assert_eq!(detect_delim(&["header", "1\t2", "3\t4"]), Delim::Tab);
        assert_eq!(detect_delim(&["1,2", "3 4", "5 6"]), Delim::Space);
        // A whitespace file with one stray comma stays whitespace-separated:
        // the stray line is one field, which makes the first column a label
        // column rather than folding "3,4" into a number.
        let d = ds("1 2\n3,4\n");
        assert_eq!(
            d.labels.as_deref(),
            Some(&[String::from("1"), String::from("3,4")][..]),
            "{d:?}"
        );
        assert_eq!(d.series.len(), 1, "{d:?}");
        assert_eq!(d.series[0].values[0], 2.0);
        let d = ds("1 2\n3 4\n5,6\n");
        assert_eq!(&d.series[0].values[..2], [2.0, 4.0], "{d:?}");
        assert!(
            d.series[0].values[2].is_nan(),
            "the stray line is one unparseable field"
        );
    }

    /// The image and text charts agree on a zero bar, a spark keeps its
    /// one-row aspect without a title, and downsampled bar labels stay
    /// under their bars.
    #[test]
    fn svg_zero_bars_sparks_and_bucketed_labels() {
        let p = Palette::default();
        let zeros = Dataset {
            labels: None,
            series: vec![Series {
                name: "v".into(),
                values: vec![0.0, 0.0, 0.0],
            }],
        };
        let out = svg(&zeros, ChartKind::Bar, None, 600, 300, &p);
        let heights: Vec<&str> = out
            .split("<rect")
            .skip(2)
            .filter_map(|r| {
                r.split("height=\"")
                    .nth(1)
                    .and_then(|h| h.split('"').next())
            })
            .collect();
        assert_eq!(
            heights,
            ["1.0", "1.0", "1.0"],
            "an exact zero still draws a pixel: {out}"
        );
        let spark = svg(&ds("1\n2\n3\n"), ChartKind::Spark, Some("t"), 600, 20, &p);
        assert!(
            spark.contains(r#"height="20""#),
            "the floor is 20px, not 40: {spark}"
        );
        assert!(!spark.contains(">t<"), "no room for a title on one row");
        let titled = svg(&ds("1\n2\n3\n"), ChartKind::Line, Some("t"), 600, 40, &p);
        assert!(titled.contains(">t<"));
        // 1000 labelled bars into a 600px chart: fewer rects than rows, and
        // each label names the rows its bucket averages.
        let input: String = (0..1000).map(|i| format!("l{i},{i}\n")).collect();
        let d = ds(&format!("label,v\n{input}"));
        let out = svg(&d, ChartKind::Bar, None, 600, 300, &p);
        assert!(
            out.matches("<rect").count() < 1000,
            "bars bucket to the width"
        );
        // 1000 rows into 540 slots: buckets alternate one and two rows, so
        // single-row labels and range labels both appear.
        assert!(
            out.contains(">l0<"),
            "a one-row bucket keeps its label: {out}"
        );
        assert!(
            out.contains("\u{2026}l"),
            "a two-row bucket is labelled by its range: {out}"
        );
        let few = ds("k,v\na,1\nb,2\n");
        let out = svg(&few, ChartKind::Bar, None, 600, 300, &p);
        assert!(
            out.contains(">a<") && out.contains(">b<"),
            "single-row buckets keep their label"
        );
        let wide = text(&ds("k,v\n日本語,2\nααα,1\n"), ChartKind::Bar, None, 20, 2);
        let rows: Vec<&str> = wide.lines().collect();
        let widths: Vec<usize> = rows
            .iter()
            .map(|r| r.chars().map(display_width).sum())
            .collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "every row the same display width: {rows:?}"
        );
    }

    /// A height smaller than the axis padding (`--height 1` with labels, a
    /// titled chart at 40px) must still draw inside the viewBox, for every
    /// chart kind: each `y=`/`y1=`/`y2=` attribute and each polyline point
    /// the SVG emits lies within `0..=h`, and a two-series line's legend
    /// either fits or is left out.
    #[test]
    fn a_tiny_height_keeps_the_chart_inside_the_viewbox() {
        let p = Palette::default();
        let two = ds("label,a,b\nx,1,2\ny,3,4\nz,2,1\n");
        let kinds = [
            ChartKind::Bar,
            ChartKind::Line,
            ChartKind::Spark,
            ChartKind::Hist,
        ];
        for kind in kinds {
            for (title, h) in [(None, 20u32), (Some("t"), 40)] {
                let out = svg(&two, kind, title, 600, h, &p);
                let mut ys: Vec<f64> = out
                    .split([' ', '/'])
                    .filter_map(|a| {
                        a.strip_prefix("y=\"")
                            .or_else(|| a.strip_prefix("y1=\""))
                            .or_else(|| a.strip_prefix("y2=\""))
                            .and_then(|v| v.trim_end_matches('"').parse().ok())
                    })
                    .collect();
                // Polyline points are `x,y` pairs inside one attribute.
                for pts in out.split("points=\"").skip(1) {
                    let pts = pts.split('"').next().unwrap_or("");
                    ys.extend(
                        pts.split_whitespace()
                            .filter_map(|pair| pair.split(',').nth(1)?.parse::<f64>().ok()),
                    );
                }
                assert!(
                    !ys.is_empty(),
                    "{kind:?} h={h}: no y coordinates parsed: {out}"
                );
                for (i, y) in ys.iter().enumerate() {
                    assert!(
                        *y >= 0.0 && *y <= f64::from(h),
                        "{kind:?} h={h}: y[{i}]={y} escaped the viewBox: {ys:?}"
                    );
                }
            }
        }
        // With room for it the legend is there.
        let tall = svg(&two, ChartKind::Line, None, 600, 200, &p);
        assert!(
            tall.contains(">a<") && tall.contains(">b<"),
            "legend at 200px: {tall}"
        );
    }

    /// A legend is all or nothing: ten series at a height with room for
    /// seven names shows no legend rather than a misleading prefix, and
    /// at a height with room for all ten shows every name.
    #[test]
    fn a_legend_that_does_not_fit_whole_is_left_out_whole() {
        let p = Palette::default();
        let header = (0..10)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let row = |r: usize| {
            (0..10)
                .map(|i| (r + i).to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        let ten = ds(&format!("{header}\n{}\n{}\n{}\n", row(1), row(2), row(3)));
        let short = svg(&ten, ChartKind::Line, None, 600, 120, &p);
        assert_eq!(
            short.matches("<polyline").count(),
            10,
            "every series is drawn: {short}"
        );
        assert!(
            (0..10).all(|i| !short.contains(&format!(">s{i}<"))),
            "no partial legend at 120px: {short}"
        );
        let tall = svg(&ten, ChartKind::Line, None, 600, 300, &p);
        assert!(
            (0..10).all(|i| tall.contains(&format!(">s{i}<"))),
            "the whole legend at 300px: {tall}"
        );
    }

    #[test]
    fn a_bucket_of_identical_labels_wears_one_label() {
        let p = Palette::default();
        let input: String = (0..1000).map(|i| format!("x,{i}\n")).collect();
        let out = svg(
            &ds(&format!("label,v\n{input}")),
            ChartKind::Bar,
            None,
            600,
            300,
            &p,
        );
        assert!(out.contains(">x<"), "the shared label survives: {out}");
        assert!(!out.contains("x\u{2026}x"), "no `x…x`: {out}");
    }

    /// The label range and the averaged bucket are one computation, so
    /// every row lands in exactly one bucket - checked on a row count that
    /// is not a multiple of the slot count, where the last bucket is the one
    /// that goes wrong.
    #[test]
    fn bucket_ranges_tile_the_rows_exactly() {
        for (n, slots) in [(1081usize, 540usize), (1000, 540), (7, 3), (5, 5)] {
            let mut next = 0;
            for i in 0..slots {
                let (a, b) = bucket_range(i, n, slots);
                assert_eq!(
                    a, next,
                    "bucket {i} of {n}/{slots} starts where the last ended"
                );
                assert!(b > a, "bucket {i} of {n}/{slots} is not empty");
                next = b;
            }
            assert_eq!(next, n, "{n}/{slots}: the last bucket ends at the last row");
        }
    }

    /// More slots than rows: the `.max(a + 1)` is what keeps every bucket
    /// non-empty, so this is the case that pins it.
    #[test]
    fn a_bucket_is_never_empty_even_with_more_slots_than_rows() {
        for i in 0..7 {
            let (a, b) = bucket_range(i, 3, 7);
            assert!(b > a, "bucket {i} of 3/7 is empty");
            assert!(a < 3, "bucket {i} of 3/7 starts past the last row");
        }
    }

    #[test]
    fn text_bars_grow_from_zero_like_the_svg_does() {
        let d = Dataset {
            labels: None,
            series: vec![Series {
                name: "v".into(),
                values: vec![-1.0, -5.0, -3.0],
            }],
        };
        let rows: Vec<String> = text(&d, ChartKind::Bar, None, 30, 5)
            .lines()
            .map(str::to_string)
            .collect();
        let ink = |col: usize| {
            rows.iter()
                .filter(|r| r.chars().nth(col).is_some_and(|c| c != ' '))
                .count()
        };
        // Sample each bar's first column: the bars start where the zero
        // row (row 0, every bar touches zero) shows its first block.
        let mut starts: Vec<usize> = Vec::new();
        let mut prev = ' ';
        for (i, c) in rows[0].chars().enumerate() {
            if c != ' ' && prev == ' ' {
                starts.push(i);
            }
            prev = c;
        }
        let starts: Vec<usize> = starts.into_iter().skip(1).collect(); // the "0" label
        assert_eq!(starts.len(), 3, "three bars on the zero row: {rows:?}");
        let (a, b, c) = (ink(starts[0]), ink(starts[1]), ink(starts[2]));
        assert!(
            b > c && c > a,
            "-5 out-draws -3 out-draws -1: a={a} b={b} c={c}\n{}",
            rows.join("\n")
        );
        assert!(
            rows[0].trim_start().starts_with('0'),
            "zero is the top label: {rows:?}"
        );
        let mixed = Dataset {
            labels: None,
            series: vec![Series {
                name: "v".into(),
                values: vec![-5.0, 0.0, 5.0],
            }],
        };
        let t = text(&mixed, ChartKind::Bar, None, 30, 4);
        // Bars start at columns 3, 10, 17 (a 3-wide label gutter, 6-wide
        // bars, one-column gaps); the zero bar is the middle one.
        let zero_col_has_ink = t
            .lines()
            .any(|r| r.chars().nth(10).is_some_and(|c| "▁▂▃▄▅▆▇█".contains(c)));
        assert!(zero_col_has_ink, "an exact zero still shows a mark:\n{t}");
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
        let flat = histogram(&ds("5\n5\n5\n5\n"), 4);
        assert_eq!(flat.series[0].values, [4.0], "constant data is one bin");
        assert_eq!(flat.labels.as_deref(), Some(&[String::from("5")][..]));
        assert_eq!(format_num(1500.0), "1500");
        assert_eq!(format_num(12345.0), "12.3k");
        assert_eq!(format_num(2.5), "2.50");
        assert_eq!(format_num(999_999.0), "1.0M");
        assert_eq!(format_num(2.5e9), "2.5G");
        assert!(format_num(1e20).len() <= 7, "{}", format_num(1e20));
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
        let bars = svg(&d, ChartKind::Bar, None, 800, 300, &Palette::default());
        assert!(
            bars.matches("<rect").count() < 2_000,
            "bars bucket to the width too"
        );
        assert!(!t.is_empty());
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "10k rows took {:?}",
            start.elapsed()
        );
    }
}
