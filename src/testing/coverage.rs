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

/// Coverage for a run, by absolute path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Coverage {
    pub files: HashMap<PathBuf, FileCoverage>,
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
        Self { files }
    }

    /// The whole run's line percentage.
    pub fn percent(&self) -> Option<f64> {
        let covered = self.files.values().map(FileCoverage::covered).sum();
        let total = self.files.values().map(|f| f.hits.len()).sum();
        percent(covered, total)
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
