//! The CodeQL evaluator log viewer (#578), as VS Code's "Show Evaluator
//! Log (Viewer)" shows it: every evaluated predicate with its time and
//! result size, slowest first, each expanding into the RA pipelines it ran
//! and the predicates it depends on.
//!
//! The input is what `codeql generate log-summary --format=predicates`
//! writes: one JSON object per line. Lines that are not predicate records
//! (headers, totals, anything a newer CLI adds) are skipped, and unknown
//! fields are ignored. The output is plain indented text, so the editor's
//! indentation folding makes it a tree.

use serde_json::Value;

/// One RA pipeline of a predicate: its name in the summary's `ra` map
/// (`pipeline` for a simple predicate, `base`, `standard` and so on for a
/// recursive one), how many times it ran, and its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub name: String,
    pub runs: usize,
    pub lines: Vec<String>,
}

/// One evaluated predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    pub name: String,
    pub strategy: String,
    pub millis: u64,
    pub result_size: u64,
    /// How many iterations a recursive predicate took; 0 otherwise.
    pub iterations: usize,
    pub pipelines: Vec<Pipeline>,
    pub dependencies: Vec<String>,
}

/// The predicate records in a predicates-format summary, slowest first
/// (ties by name, so the order is stable).
pub fn parse(summary: &str) -> Vec<Predicate> {
    let mut predicates: Vec<Predicate> = summary
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter_map(|v| predicate(&v))
        .collect();
    predicates.sort_by(|a, b| b.millis.cmp(&a.millis).then_with(|| a.name.cmp(&b.name)));
    predicates
}

fn predicate(v: &Value) -> Option<Predicate> {
    let name = v.get("predicateName")?.as_str()?.to_string();
    let strategy = v.get("evaluationStrategy")?.as_str()?.to_string();
    let number = |key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    let iterations = v
        .get("predicateIterationMillis")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut dependencies: Vec<String> = v
        .get("dependencies")
        .and_then(Value::as_object)
        .map(|d| d.keys().cloned().collect())
        .unwrap_or_default();
    dependencies.sort();
    Some(Predicate {
        name,
        strategy,
        millis: number("millis"),
        result_size: number("resultSize"),
        iterations,
        pipelines: pipelines(v),
        dependencies,
    })
}

/// The pipelines in `ra`, in the order `pipelineRuns` first ran them, with
/// any that never ran after. `ra` is a map of named pipelines, or a bare
/// list of lines from an older CLI.
fn pipelines(v: &Value) -> Vec<Pipeline> {
    let lines = |p: &Value| -> Vec<String> {
        p.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|l| l.trim_end().to_string())
            .filter(|l| !l.trim().is_empty())
            .collect()
    };
    let runs: Vec<&str> = v
        .get("pipelineRuns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| r.get("raReference").and_then(Value::as_str))
        .collect();
    match v.get("ra") {
        Some(Value::Object(ra)) => {
            let mut names: Vec<&str> = Vec::new();
            for name in runs.iter().copied().chain(ra.keys().map(String::as_str)) {
                if ra.contains_key(name) && !names.contains(&name) {
                    names.push(name);
                }
            }
            names
                .into_iter()
                .map(|name| Pipeline {
                    name: name.to_string(),
                    runs: runs.iter().filter(|r| **r == name).count(),
                    lines: lines(&ra[name]),
                })
                .collect()
        }
        Some(ra @ Value::Array(_)) => vec![Pipeline {
            name: String::from("pipeline"),
            runs: runs.len(),
            lines: lines(ra),
        }],
        _ => Vec::new(),
    }
}

/// `n` with thousands separators: 1234567 is "1,234,567".
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The header line of `p` in the tree.
pub fn header(p: &Predicate) -> String {
    let rows = if p.result_size == 1 { "row" } else { "rows" };
    let mut line = format!(
        "\u{25b8} {} ms  {} {rows}  {} ({})",
        grouped(p.millis),
        grouped(p.result_size),
        p.name,
        p.strategy
    );
    if p.iterations > 0 {
        let s = if p.iterations == 1 { "" } else { "s" };
        line.push_str(&format!(", {} iteration{s}", p.iterations));
    }
    line
}

/// The viewer's text for `predicates` run by `query`: a title, then each
/// predicate's header with its pipelines (RA lines indented beneath) and
/// dependencies as children. Children are indented, so each predicate
/// folds.
pub fn render(query: &str, predicates: &[Predicate]) -> String {
    let total: u64 = predicates.iter().map(|p| p.millis).sum();
    let count = predicates.len();
    let s = if count == 1 { "" } else { "s" };
    let mut out = format!(
        "Evaluator log for {query}: {count} predicate{s}, {} ms in all, slowest first\n",
        grouped(total)
    );
    for p in predicates {
        out.push('\n');
        out.push_str(&header(p));
        out.push('\n');
        for pipeline in &p.pipelines {
            out.push_str(&format!("    Pipeline {}", pipeline.name));
            if pipeline.runs > 1 {
                out.push_str(&format!(" ({} runs)", pipeline.runs));
            }
            out.push('\n');
            for line in &pipeline.lines {
                out.push_str("        ");
                out.push_str(line.trim_start());
                out.push('\n');
            }
        }
        if !p.dependencies.is_empty() {
            out.push_str(&format!("    Dependencies ({})\n", p.dependencies.len()));
            for d in &p.dependencies {
                out.push_str("        ");
                out.push_str(d);
                out.push('\n');
            }
        }
    }
    out
}

/// The evaluator log the side bar's Evaluator Log Viewer shows (#578):
/// whose it is, its predicates slowest first, and which are unfolded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogView {
    pub query: String,
    pub predicates: Vec<Predicate>,
    pub open: std::collections::HashSet<usize>,
}

/// One row of the side bar's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogRow {
    /// Predicate `.0`'s header.
    Predicate(usize, String),
    /// A line under predicate `.0`: a pipeline name or one of its RA
    /// lines.
    Detail(usize, String),
    /// A dependency of predicate `.0`, and the index of the predicate it
    /// names when the log has it.
    Dependency(usize, Option<usize>, String),
}

impl LogView {
    /// The rows to show: every predicate, and the pipelines and
    /// dependencies of the unfolded ones.
    pub fn rows(&self) -> Vec<LogRow> {
        let mut out = Vec::new();
        for (i, p) in self.predicates.iter().enumerate() {
            let head = header(p);
            let head = if self.open.contains(&i) {
                head.replacen('\u{25b8}', "\u{25be}", 1)
            } else {
                head
            };
            out.push(LogRow::Predicate(i, head));
            if !self.open.contains(&i) {
                continue;
            }
            for pipeline in &p.pipelines {
                let runs = match pipeline.runs {
                    0 | 1 => String::new(),
                    n => format!(" ({n} runs)"),
                };
                out.push(LogRow::Detail(
                    i,
                    format!("  Pipeline {}{runs}", pipeline.name),
                ));
                for line in &pipeline.lines {
                    out.push(LogRow::Detail(i, format!("    {}", line.trim_start())));
                }
            }
            for d in &p.dependencies {
                let target = self.predicates.iter().position(|q| &q.name == d);
                out.push(LogRow::Dependency(i, target, format!("  \u{2192} {d}")));
            }
        }
        out
    }

    /// Fold or unfold predicate `i`.
    pub fn toggle(&mut self, i: usize) {
        if !self.open.remove(&i) {
            self.open.insert(i);
        }
    }
}

/// Which of two runs evaluated a predicate in a performance comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    OnlyOld,
    OnlyNew,
    Both,
}

/// One predicate in VS Code's "Compare Performance": its time and result
/// size in each run (0 in a run that did not evaluate it) and how much
/// slower the newer run was (negative when it got faster).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The newer run's name for it when both have it.
    pub name: String,
    pub presence: Presence,
    pub old_millis: u64,
    pub new_millis: u64,
    pub delta: i64,
    pub old_rows: u64,
    pub new_rows: u64,
}

/// `name` without the `#hash` CodeQL appends to it, which changes whenever
/// the predicate or anything it depends on does.
fn unhashed(name: &str) -> &str {
    name.rsplit_once('#').map_or(name, |(base, _)| base)
}

/// Set the predicates of two runs side by side, biggest change in time
/// first (ties by name). Predicates pair up by full name, then the rest by
/// name without the `#hash` where that pairs one with one; anything left
/// ran in one run only.
pub fn compare(old: &[Predicate], new: &[Predicate]) -> Vec<Row> {
    use std::collections::HashMap;
    let mut pair: Vec<Option<usize>> = vec![None; new.len()];
    let mut taken = vec![false; old.len()];
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, p) in old.iter().enumerate().rev() {
        by_name.entry(p.name.as_str()).or_default().push(i);
    }
    for (j, p) in new.iter().enumerate() {
        if let Some(i) = by_name.get_mut(p.name.as_str()).and_then(Vec::pop) {
            pair[j] = Some(i);
            taken[i] = true;
        }
    }
    // The fallback: a hash-less name held by one leftover on each side.
    let mut old_bases: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, p) in old.iter().enumerate().filter(|&(i, _)| !taken[i]) {
        old_bases.entry(unhashed(&p.name)).or_default().push(i);
    }
    let mut new_bases: HashMap<&str, usize> = HashMap::new();
    for (p, _) in new.iter().zip(&pair).filter(|(_, i)| i.is_none()) {
        *new_bases.entry(unhashed(&p.name)).or_default() += 1;
    }
    for (j, p) in new.iter().enumerate() {
        let base = unhashed(&p.name);
        if pair[j].is_some() || new_bases.get(base) != Some(&1) {
            continue;
        }
        if let Some(&[i]) = old_bases.get(base).map(Vec::as_slice) {
            pair[j] = Some(i);
            taken[i] = true;
        }
    }
    let row = |name: &str, presence, old: Option<&Predicate>, new: Option<&Predicate>| {
        let old_millis = old.map_or(0, |p| p.millis);
        let new_millis = new.map_or(0, |p| p.millis);
        Row {
            name: name.to_string(),
            presence,
            old_millis,
            new_millis,
            delta: new_millis as i64 - old_millis as i64,
            old_rows: old.map_or(0, |p| p.result_size),
            new_rows: new.map_or(0, |p| p.result_size),
        }
    };
    let mut rows: Vec<Row> = new
        .iter()
        .zip(&pair)
        .map(|(p, i)| match i {
            Some(i) => row(&p.name, Presence::Both, Some(&old[*i]), Some(p)),
            None => row(&p.name, Presence::OnlyNew, None, Some(p)),
        })
        .chain(
            old.iter()
                .zip(&taken)
                .filter(|(_, t)| !**t)
                .map(|(p, _)| row(&p.name, Presence::OnlyOld, Some(p), None)),
        )
        .collect();
    rows.sort_by(|a, b| {
        b.delta
            .unsigned_abs()
            .cmp(&a.delta.unsigned_abs())
            .then_with(|| a.name.cmp(&b.name))
    });
    rows
}

/// A signed change in time: "+1,200 ms", "-30 ms", "±0 ms".
fn signed_millis(delta: i64) -> String {
    let sign = match delta.signum() {
        1 => "+",
        -1 => "-",
        _ => "\u{b1}",
    };
    format!("{sign}{} ms", grouped(delta.unsigned_abs()))
}

/// The comparison's text: a title, the total time in each run with the
/// change, then one line per predicate with its change right-aligned. A
/// run that did not evaluate a predicate shows "–" for it.
pub fn render_comparison(old_label: &str, new_label: &str, rows: &[Row]) -> String {
    let old_total: u64 = rows.iter().map(|r| r.old_millis).sum();
    let new_total: u64 = rows.iter().map(|r| r.new_millis).sum();
    let count = |presence| rows.iter().filter(|r| r.presence == presence).count();
    let s = if rows.len() == 1 { "" } else { "s" };
    let mut out = format!(
        "Performance: {old_label} \u{2192} {new_label}\n\
         {} \u{2192} {} ms in all ({}), {} predicate{s}: {} in both, {} only before, {} only after\n",
        grouped(old_total),
        grouped(new_total),
        signed_millis(new_total as i64 - old_total as i64),
        rows.len(),
        count(Presence::Both),
        count(Presence::OnlyOld),
        count(Presence::OnlyNew),
    );
    if !rows.is_empty() {
        out.push('\n');
    }
    let deltas: Vec<String> = rows.iter().map(|r| signed_millis(r.delta)).collect();
    let width = deltas.iter().map(|d| d.chars().count()).max().unwrap_or(0);
    for (r, delta) in rows.iter().zip(&deltas) {
        let side = |present: bool, n: u64| {
            if present {
                grouped(n)
            } else {
                String::from("\u{2013}")
            }
        };
        let old = r.presence != Presence::OnlyNew;
        let new = r.presence != Presence::OnlyOld;
        out.push_str(&format!(
            "{delta:>width$}  {} \u{2192} {} ms  {} \u{2192} {} rows  {}\n",
            side(old, r.old_millis),
            side(new, r.new_millis),
            side(old, r.old_rows),
            side(new, r.new_rows),
            r.name,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_side_bar_tree_folds_predicates_and_links_dependencies() {
        let p = |name: &str, millis: u64, deps: &[&str]| Predicate {
            name: name.to_string(),
            strategy: String::from("SIMPLE"),
            millis,
            result_size: 3,
            iterations: 0,
            pipelines: vec![Pipeline {
                name: String::from("pipeline"),
                runs: 1,
                lines: vec![String::from("   {1} r1 = JOIN a WITH b")],
            }],
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
        };
        let mut v = LogView {
            query: String::from("q.ql"),
            predicates: vec![p("slow", 90, &["fast", "gone"]), p("fast", 5, &[])],
            ..LogView::default()
        };
        let rows = v.rows();
        assert_eq!(rows.len(), 2, "folded at first");
        assert!(
            matches!(&rows[0], LogRow::Predicate(0, h) if h.starts_with('\u{25b8}') && h.contains("slow"))
        );
        v.toggle(0);
        let rows = v.rows();
        assert!(matches!(&rows[0], LogRow::Predicate(0, h) if h.starts_with('\u{25be}')));
        assert_eq!(
            rows[1],
            LogRow::Detail(0, String::from("  Pipeline pipeline"))
        );
        assert_eq!(
            rows[2],
            LogRow::Detail(0, String::from("    {1} r1 = JOIN a WITH b"))
        );
        assert_eq!(
            rows[3],
            LogRow::Dependency(0, Some(1), String::from("  \u{2192} fast")),
            "a dependency the log has leads to it"
        );
        assert_eq!(
            rows[4],
            LogRow::Dependency(0, None, String::from("  \u{2192} gone"))
        );
        assert!(matches!(&rows[5], LogRow::Predicate(1, _)));
    }

    const SAMPLE: &str = concat!(
        r#"{"summaryLogVersion":"0.4.0","codeqlVersion":"2.19.0","startTime":"2026-09-28T10:00:00Z"}"#,
        "\n",
        r#"{"completionTime":"2026-09-28T10:00:01Z","raHash":"a1","predicateName":"Foo::bar#abc","appearsAs":{"Foo::bar#abc":{"q.ql":[1]}},"queryCausingWork":"q.ql","evaluationStrategy":"COMPUTE_SIMPLE","millis":1234,"resultSize":12345,"dependencies":{"Foo::baz#def":"b2","files":"c3"},"ra":{"pipeline":["    {2} r1 = SCAN files OUTPUT In.0, In.1","    {2} r2 = JOIN r1 WITH Foo::baz#def ON FIRST 1 OUTPUT Lhs.0, Rhs.1","","    return r2"]},"pipelineRuns":[{"raReference":"pipeline","counts":[10,12345]}],"futureField":{"x":1}}"#,
        "\n",
        "not json at all\n",
        r#"{"completionTime":"2026-09-28T10:00:02Z","raHash":"d4","predicateName":"Reach::step#rec","evaluationStrategy":"COMPUTE_RECURSIVE","millis":5000,"resultSize":1,"predicateIterationMillis":[100,2400,2500],"deltaSizes":[1,0,0],"dependencies":{"Foo::bar#abc":"a1"},"ra":{"base":["{1} r1 = Foo::bar#abc","return r1"],"standard":["{1} r1 = JOIN Reach::step#rec#prev_delta WITH Foo::bar#abc","return r1"]},"pipelineRuns":[{"raReference":"base","counts":[1]},{"raReference":"standard","counts":[0]},{"raReference":"standard","counts":[0]}]}"#,
        "\n",
        r#"{"completionTime":"2026-09-28T10:00:03Z","raHash":"e5","predicateName":"files","evaluationStrategy":"EXTENSIONAL","resultSize":3}"#,
        "\n",
    );

    #[test]
    fn parses_predicate_records_slowest_first_skipping_the_rest() {
        let ps = parse(SAMPLE);
        let names: Vec<&str> = ps.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Reach::step#rec", "Foo::bar#abc", "files"]);

        let rec = &ps[0];
        assert_eq!(rec.strategy, "COMPUTE_RECURSIVE");
        assert_eq!((rec.millis, rec.result_size, rec.iterations), (5000, 1, 3));
        assert_eq!(rec.dependencies, ["Foo::bar#abc"]);
        assert_eq!(
            rec.pipelines,
            vec![
                Pipeline {
                    name: String::from("base"),
                    runs: 1,
                    lines: vec![
                        String::from("{1} r1 = Foo::bar#abc"),
                        String::from("return r1")
                    ],
                },
                Pipeline {
                    name: String::from("standard"),
                    runs: 2,
                    lines: vec![
                        String::from("{1} r1 = JOIN Reach::step#rec#prev_delta WITH Foo::bar#abc"),
                        String::from("return r1")
                    ],
                },
            ]
        );

        let simple = &ps[1];
        assert_eq!((simple.millis, simple.result_size), (1234, 12345));
        assert_eq!(simple.dependencies, ["Foo::baz#def", "files"]);
        assert_eq!(simple.pipelines.len(), 1);
        assert_eq!(simple.pipelines[0].lines.len(), 3, "blank RA lines drop");

        // No time and no RA: an extensional table read from the database.
        let ext = &ps[2];
        assert_eq!((ext.millis, ext.result_size), (0, 3));
        assert!(ext.pipelines.is_empty() && ext.dependencies.is_empty());
    }

    #[test]
    fn a_bare_ra_list_is_one_pipeline() {
        let ps = parse(
            r#"{"predicateName":"p","evaluationStrategy":"COMPUTE_SIMPLE","millis":1,"ra":["return r1"]}"#,
        );
        assert_eq!(ps[0].pipelines[0].name, "pipeline");
        assert_eq!(ps[0].pipelines[0].lines, ["return r1"]);
        assert!(parse("").is_empty());
    }

    #[test]
    fn renders_an_indented_tree() {
        let text = render("q.ql", &parse(SAMPLE));
        let expected = "\
Evaluator log for q.ql: 3 predicates, 6,234 ms in all, slowest first

\u{25b8} 5,000 ms  1 row  Reach::step#rec (COMPUTE_RECURSIVE), 3 iterations
    Pipeline base
        {1} r1 = Foo::bar#abc
        return r1
    Pipeline standard (2 runs)
        {1} r1 = JOIN Reach::step#rec#prev_delta WITH Foo::bar#abc
        return r1
    Dependencies (1)
        Foo::bar#abc

\u{25b8} 1,234 ms  12,345 rows  Foo::bar#abc (COMPUTE_SIMPLE)
    Pipeline pipeline
        {2} r1 = SCAN files OUTPUT In.0, In.1
        {2} r2 = JOIN r1 WITH Foo::baz#def ON FIRST 1 OUTPUT Lhs.0, Rhs.1
        return r2
    Dependencies (2)
        Foo::baz#def
        files

\u{25b8} 0 ms  3 rows  files (EXTENSIONAL)
";
        assert_eq!(text, expected);
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(999), "999");
    }

    fn timed(name: &str, millis: u64, result_size: u64) -> Predicate {
        Predicate {
            name: name.to_string(),
            strategy: String::from("COMPUTE_SIMPLE"),
            millis,
            result_size,
            iterations: 0,
            pipelines: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn comparing_pairs_predicates_by_name_then_without_the_hash() {
        let old = [
            timed("Foo::bar#abc", 3400, 12),
            timed("Gone::p#111", 500, 4),
            timed("Same::q#222", 10, 1),
            // Two leftovers share a hash-less name, so neither pairs up.
            timed("Twin::t#a", 40, 1),
            timed("Twin::t#b", 50, 1),
        ];
        let new = [
            timed("Foo::bar#abc", 4600, 15),
            timed("Same::q#333", 12, 1),
            timed("Added::r#444", 900, 7),
            timed("Twin::t#c", 45, 1),
        ];
        let rows = compare(&old, &new);
        let summary: Vec<(&str, Presence, u64, u64, i64)> = rows
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.presence,
                    r.old_millis,
                    r.new_millis,
                    r.delta,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Foo::bar#abc", Presence::Both, 3400, 4600, 1200),
                ("Added::r#444", Presence::OnlyNew, 0, 900, 900),
                ("Gone::p#111", Presence::OnlyOld, 500, 0, -500),
                ("Twin::t#b", Presence::OnlyOld, 50, 0, -50),
                ("Twin::t#c", Presence::OnlyNew, 0, 45, 45),
                ("Twin::t#a", Presence::OnlyOld, 40, 0, -40),
                ("Same::q#333", Presence::Both, 10, 12, 2),
            ]
        );
        assert_eq!((rows[0].old_rows, rows[0].new_rows), (12, 15));
        assert!(compare(&[], &[]).is_empty());
    }

    #[test]
    fn renders_the_comparison_as_a_table() {
        let old = [
            timed("Foo::bar#abc", 3400, 12),
            timed("Gone::p#1", 500, 4),
            timed("Same::q#2", 10, 1),
        ];
        let new = [
            timed("Foo::bar#abc", 4600, 15),
            timed("Same::q#3", 10, 1),
            timed("Added::r", 90, 7),
        ];
        let text = render_comparison("earlier (app)", "later (app)", &compare(&old, &new));
        let expected = "\
Performance: earlier (app) \u{2192} later (app)
3,910 \u{2192} 4,700 ms in all (+790 ms), 4 predicates: 2 in both, 1 only before, 1 only after

+1,200 ms  3,400 \u{2192} 4,600 ms  12 \u{2192} 15 rows  Foo::bar#abc
  -500 ms  500 \u{2192} \u{2013} ms  4 \u{2192} \u{2013} rows  Gone::p#1
   +90 ms  \u{2013} \u{2192} 90 ms  \u{2013} \u{2192} 7 rows  Added::r
    \u{b1}0 ms  10 \u{2192} 10 ms  1 \u{2192} 1 rows  Same::q#3
";
        assert_eq!(text, expected);
        assert_eq!(
            render_comparison("a", "b", &[]),
            "Performance: a \u{2192} b\n0 \u{2192} 0 ms in all (\u{b1}0 ms), 0 predicates: 0 in both, 0 only before, 0 only after\n"
        );
    }
}
