//! Test coverage (#263): one model for every runner, read from LCOV.
//!
//! pytest-cov (`--cov-report=lcov`), vitest (`--coverage.reporter=lcov`),
//! jest (`--coverageReporters=lcov`) and cargo-llvm-cov (`--lcov`) all write
//! LCOV, a line-based format with per-branch records, so one small parser
//! stands in for the three JSON formats those tools also have.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// How one source line fared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCov {
    Covered,
    Uncovered,
    /// Run, but some of the branches starting on it never were.
    Partial,
}

/// One file's coverage: executable lines (1-based) and their hit counts,
/// and per line the branches (taken or not) that start there.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileCoverage {
    pub hits: BTreeMap<u32, u64>,
    pub branches: BTreeMap<u32, Vec<bool>>,
}

impl FileCoverage {
    pub fn line(&self, line: u32) -> Option<LineCov> {
        let hits = *self.hits.get(&line)?;
        if hits == 0 {
            return Some(LineCov::Uncovered);
        }
        let missed = self
            .branches
            .get(&line)
            .is_some_and(|taken| taken.iter().any(|t| !t));
        Some(if missed {
            LineCov::Partial
        } else {
            LineCov::Covered
        })
    }

    /// Covered executable lines over all executable lines, in percent.
    /// `None` for a file with no executable lines.
    pub fn percent(&self) -> Option<f64> {
        percent(self.covered(), self.hits.len())
    }

    fn covered(&self) -> usize {
        self.hits.values().filter(|h| **h > 0).count()
    }
}

fn percent(covered: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| covered as f64 * 100.0 / total as f64)
}

/// Where a suite's source may be, relative to the runner's root: see
/// [`Coverage::suite_percent`].
fn suite_source_candidates(suite: &str) -> Vec<PathBuf> {
    let head = suite.split("::").next().unwrap_or(suite);
    let file_name = head.rsplit('/').next().unwrap_or(head);
    if file_name.contains('.') {
        return vec![PathBuf::from(head)];
    }
    let mut segs: Vec<&str> = suite.split("::").collect();
    while segs.last().is_some_and(|s| *s == "tests" || *s == "test") {
        segs.pop();
    }
    if segs.is_empty() {
        return vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")];
    }
    let module = segs.join("/");
    vec![
        PathBuf::from(format!("src/{module}.rs")),
        PathBuf::from(format!("src/{module}/mod.rs")),
    ]
}

/// Coverage for a run, by absolute path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Coverage {
    pub files: HashMap<PathBuf, FileCoverage>,
    /// Where the runner ran, which relative report and suite paths are
    /// taken against.
    pub root: PathBuf,
}

impl Coverage {
    /// Read an LCOV report. Relative `SF:` paths are taken against `root`,
    /// where the runner ran.
    pub fn from_lcov(text: &str, root: &Path) -> Self {
        let mut files: HashMap<PathBuf, FileCoverage> = HashMap::new();
        let mut current: Option<PathBuf> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(path) = line.strip_prefix("SF:") {
                current = Some(root.join(path));
                continue;
            }
            if line == "end_of_record" {
                current = None;
                continue;
            }
            let Some(file) = current.as_ref() else {
                continue;
            };
            if let Some(rest) = line.strip_prefix("DA:") {
                let mut parts = rest.split(',');
                let (Some(Ok(n)), Some(Ok(hits))) = (
                    parts.next().map(str::parse::<u32>),
                    parts.next().map(str::parse::<u64>),
                ) else {
                    continue;
                };
                *files
                    .entry(file.clone())
                    .or_default()
                    .hits
                    .entry(n)
                    .or_insert(0) += hits;
            } else if let Some(rest) = line.strip_prefix("BRDA:") {
                let parts: Vec<&str> = rest.split(',').collect();
                let [n, _, _, taken] = parts[..] else {
                    continue;
                };
                let Ok(n) = n.parse::<u32>() else {
                    continue;
                };
                // `-` is a branch whose line never ran; `0` one never taken.
                let taken = taken.parse::<u64>().is_ok_and(|t| t > 0);
                files
                    .entry(file.clone())
                    .or_default()
                    .branches
                    .entry(n)
                    .or_default()
                    .push(taken);
            }
        }
        Self {
            files,
            root: root.to_path_buf(),
        }
    }

    /// The line percentage of the source file a Testing-tree suite lives in
    /// (#263), when it can be told from the suite's name alone: a pytest or
    /// JS suite is a file path (`tests/test_x.py`, `src/a.test.ts`); a cargo
    /// suite is a module path whose `tests` tail is the in-file test module
    /// (`widgets::testing::tests` is `src/widgets/testing.rs`, or its
    /// `mod.rs`). `None` when no such file is in the report.
    pub fn suite_percent(&self, suite: &str) -> Option<f64> {
        suite_source_candidates(suite)
            .into_iter()
            .find_map(|rel| self.files.get(&self.root.join(rel)))
            .and_then(FileCoverage::percent)
    }

    /// The whole run's line percentage.
    pub fn percent(&self) -> Option<f64> {
        let covered = self.files.values().map(FileCoverage::covered).sum();
        let total = self.files.values().map(|f| f.hits.len()).sum();
        percent(covered, total)
    }

    /// The run as text, one file per line, least covered first (the files
    /// that need tests lead), paths relative to `root` where they can be.
    /// Files with no executable lines are left out.
    pub fn report(&self, root: &Path) -> String {
        let mut rows: Vec<(f64, usize, usize, String)> = self
            .files
            .iter()
            .filter_map(|(path, f)| {
                let pct = f.percent()?;
                let name = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                Some((pct, f.covered(), f.hits.len(), name))
            })
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.3.cmp(&b.3)));
        let total: usize = rows.iter().map(|r| r.2).sum();
        let mut out = match self.percent() {
            Some(pct) => format!("Coverage: {pct:.1}% of {total} lines\n\n"),
            None => String::from("Coverage: no executable lines reported\n"),
        };
        for (pct, covered, lines, name) in rows {
            out.push_str(&format!(
                "{pct:>6.1}%  {:>11}  {name}\n",
                format!("{covered}/{lines}")
            ));
        }
        out
    }
}

/// One file's coverage as the editor paints it: 0-based logical lines,
/// the file's percentage, and whether the file has changed since the run
/// (a stale lens dims, so nobody trusts marks for code that moved).
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageLens {
    pub lines: HashMap<usize, LineCov>,
    pub percent: Option<f64>,
    pub stale: bool,
}

impl Coverage {
    /// The lens for `path`, or `None` when the run did not cover it.
    pub fn lens(&self, path: &Path, stale: bool) -> Option<CoverageLens> {
        let file = self.files.get(path)?;
        Some(CoverageLens {
            lines: file
                .hits
                .keys()
                .filter_map(|&n| Some((n.checked_sub(1)? as usize, file.line(n)?)))
                .collect(),
            percent: file.percent(),
            stale,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_lists_files_least_covered_first() {
        let lcov = "SF:src/a.rs\nDA:1,1\nDA:2,1\nend_of_record\nSF:src/b.rs\nDA:1,0\nDA:2,1\nDA:3,0\nDA:4,0\nend_of_record\n";
        let cov = Coverage::from_lcov(lcov, Path::new("/w"));
        let report = cov.report(Path::new("/w"));
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines[0], "Coverage: 50.0% of 6 lines");
        assert!(
            lines[2].ends_with("src/b.rs")
                && lines[2].contains("25.0%")
                && lines[2].contains("1/4"),
            "{report}"
        );
        assert!(
            lines[3].ends_with("src/a.rs") && lines[3].contains("100.0%"),
            "{report}"
        );
    }

    const LCOV: &str = "TN:\nSF:src/a.py\nDA:1,1\nDA:2,0\nDA:3,4\nBRDA:3,0,0,2\nBRDA:3,0,1,0\nDA:5,1\nBRDA:5,0,0,1\nBRDA:5,0,1,-\nLF:4\nLH:3\nend_of_record\nSF:/abs/b.rs\nDA:10,0\nDA:11,0\nend_of_record\n";

    #[test]
    fn lines_are_covered_uncovered_or_partial() {
        let c = Coverage::from_lcov(LCOV, Path::new("/w"));
        let a = &c.files[Path::new("/w/src/a.py")];
        assert_eq!(a.line(1), Some(LineCov::Covered));
        assert_eq!(a.line(2), Some(LineCov::Uncovered));
        assert_eq!(a.line(3), Some(LineCov::Partial), "a branch never taken");
        assert_eq!(
            a.line(5),
            Some(LineCov::Partial),
            "`-` is a branch never reached"
        );
        assert_eq!(a.line(4), None, "not executable");
        let b = &c.files[Path::new("/abs/b.rs")];
        assert_eq!(b.line(10), Some(LineCov::Uncovered));
    }

    #[test]
    fn a_suite_reads_the_percentage_of_the_file_it_lives_in() {
        // #263: a cargo suite is a module path, its `tests` tail the in-file
        // test module; a pytest or JS suite is the file itself.
        let lcov = "SF:src/widgets/testing.rs\nDA:1,1\nDA:2,0\nend_of_record\n\
                    SF:src/parse/mod.rs\nDA:1,1\nend_of_record\n\
                    SF:tests/test_x.py\nDA:1,1\nDA:2,1\nDA:3,1\nDA:4,0\nend_of_record\n\
                    SF:src/lib.rs\nDA:1,0\nend_of_record\n";
        let c = Coverage::from_lcov(lcov, Path::new("/w"));
        assert_eq!(c.suite_percent("widgets::testing::tests"), Some(50.0));
        assert_eq!(c.suite_percent("widgets::testing"), Some(50.0));
        assert_eq!(
            c.suite_percent("parse::tests"),
            Some(100.0),
            "a mod.rs module"
        );
        assert_eq!(c.suite_percent("tests/test_x.py"), Some(75.0));
        assert_eq!(c.suite_percent("tests/test_x.py::TestX"), Some(75.0));
        assert_eq!(
            c.suite_percent("tests"),
            Some(0.0),
            "the crate root's tests"
        );
        assert_eq!(c.suite_percent("nowhere::tests"), None);
    }

    #[test]
    fn percentages_count_executable_lines() {
        let c = Coverage::from_lcov(LCOV, Path::new("/w"));
        assert_eq!(c.files[Path::new("/w/src/a.py")].percent(), Some(75.0));
        assert_eq!(c.files[Path::new("/abs/b.rs")].percent(), Some(0.0));
        assert_eq!(c.percent(), Some(50.0), "3 of 6 lines");
        assert_eq!(FileCoverage::default().percent(), None);
        assert_eq!(Coverage::default().percent(), None);
    }

    #[test]
    fn a_file_listed_twice_merges_and_junk_is_skipped() {
        // Test runners that shard (jest workers, per-binary llvm) can list a
        // file more than once; hits add up.
        let text = "SF:x.js\nDA:1,0\nDA:2,1\nend_of_record\nnonsense\nSF:x.js\nDA:1,3\nDA:oops\nend_of_record\n";
        let c = Coverage::from_lcov(text, Path::new("/w"));
        let x = &c.files[Path::new("/w/x.js")];
        assert_eq!(x.hits.get(&1), Some(&3));
        assert_eq!(x.hits.get(&2), Some(&1));
        assert_eq!(x.hits.len(), 2);
    }

    #[test]
    fn a_lens_is_zero_based_and_carries_the_files_percent() {
        let c = Coverage::from_lcov(LCOV, Path::new("/w"));
        let lens = c.lens(Path::new("/w/src/a.py"), false).unwrap();
        assert_eq!(lens.lines.get(&0), Some(&LineCov::Covered));
        assert_eq!(lens.lines.get(&1), Some(&LineCov::Uncovered));
        assert_eq!(lens.lines.get(&2), Some(&LineCov::Partial));
        assert_eq!(lens.percent, Some(75.0));
        assert!(c.lens(Path::new("/w/other.py"), false).is_none());
    }
}
