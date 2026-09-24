//! The SARIF viewer's state: every result of every open log flattened into
//! [`Entry`]s, projected into grouped, filtered, sorted [`Row`]s for the list.
//!
//! Nothing here draws; the renderer reads [`SarifView::rows`] and the frame
//! rects it writes back. Keeping the projection pure is what lets the
//! filter grammar, grouping and navigation be tested without a terminal.

use super::model::{Run, SarifLog, SarifResult};
use super::resolve::{expand, uri_to_path};
use super::semantics::{self as sem, BaselineState, Kind, Level, SuppressionState};
use std::collections::HashSet;
use std::path::PathBuf;

/// Largest log the viewer loads into memory. SARIF from a monorepo scan runs
/// to hundreds of megabytes; past this the JSON tree alone would dwarf it.
pub const MAX_LOG_BYTES: u64 = 512 * 1024 * 1024;

pub fn extension_is_sarif(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("sarif")
}

/// One result, with everything the list, filters and sort need resolved up
/// front so the projection never walks the log again.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub log: usize,
    pub run: usize,
    pub result: usize,
    pub rule_id: String,
    pub rule_name: String,
    pub tool: String,
    pub level: Level,
    pub kind: Kind,
    pub baseline: BaselineState,
    pub suppression: SuppressionState,
    /// Plain message text (placeholders substituted, links as their text).
    pub message: String,
    /// The primary location's URI after `uriBaseId` expansion; empty when
    /// the result has no location.
    pub uri: String,
    /// `uri` shortened for display (workspace-relative when it can be).
    pub file: String,
    /// 1-based, as in the log; 0 when absent.
    pub line: i64,
    pub column: i64,
    pub tags: Vec<String>,
}

/// A loaded log and where it came from.
#[derive(Debug, Clone)]
pub struct LoadedLog {
    pub path: PathBuf,
    pub log: SarifLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Locations,
    Rules,
    Logs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Line,
    File,
    Message,
    Rule,
    Level,
}

/// Row-level filters (the chips). A value is hidden when it is in the set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filters {
    pub hidden_levels: HashSet<Level>,
    pub hidden_baselines: HashSet<BaselineState>,
    pub hidden_suppressions: HashSet<SuppressionState>,
    pub hidden_kinds: HashSet<Kind>,
}

impl Default for Filters {
    /// VS Code's defaults: absent results and accepted suppressions hidden.
    fn default() -> Self {
        Filters {
            hidden_levels: HashSet::new(),
            hidden_baselines: HashSet::from([BaselineState::Absent]),
            hidden_suppressions: HashSet::from([SuppressionState::Suppressed]),
            hidden_kinds: HashSet::new(),
        }
    }
}

/// A parsed keyword query. Space-separated clauses must all match; a clause
/// is one or more alternatives joined by `|`. An alternative may be negated
/// with a leading `-` and restricted to a field with `rule:`, `file:`,
/// `level:`, `tag:`, `tool:` or `msg:`. Matching is case-insensitive
/// substring.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub clauses: Vec<Vec<Term>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub field: Option<Field>,
    pub needle: String,
    pub negated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Rule,
    File,
    Level,
    Tag,
    Tool,
    Message,
}

pub fn parse_query(text: &str) -> Query {
    let clauses = text
        .split_whitespace()
        .map(|clause| {
            clause
                .split('|')
                .filter(|alt| !alt.is_empty())
                .map(parse_term)
                .collect::<Vec<_>>()
        })
        .filter(|alts| !alts.is_empty())
        .collect();
    Query { clauses }
}

fn parse_term(alt: &str) -> Term {
    let (negated, rest) = match alt.strip_prefix('-') {
        Some(r) if !r.is_empty() => (true, r),
        _ => (false, alt),
    };
    let (field, needle) = match rest.split_once(':') {
        Some((name, value)) if !value.is_empty() => match name.to_ascii_lowercase().as_str() {
            "rule" => (Some(Field::Rule), value),
            "file" => (Some(Field::File), value),
            "level" => (Some(Field::Level), value),
            "tag" => (Some(Field::Tag), value),
            "tool" => (Some(Field::Tool), value),
            "msg" => (Some(Field::Message), value),
            _ => (None, rest),
        },
        _ => (None, rest),
    };
    Term {
        field,
        needle: needle.to_lowercase(),
        negated,
    }
}

fn term_hits(t: &Term, e: &Entry) -> bool {
    let has = |s: &str| s.to_lowercase().contains(&t.needle);
    let hit = match t.field {
        Some(Field::Rule) => has(&e.rule_id) || has(&e.rule_name),
        Some(Field::File) => has(&e.file) || has(&e.uri),
        Some(Field::Level) => has(e.level.as_str()),
        Some(Field::Tag) => e.tags.iter().any(|g| has(g)),
        Some(Field::Tool) => has(&e.tool),
        Some(Field::Message) => has(&e.message),
        None => {
            has(&e.rule_id)
                || has(&e.rule_name)
                || has(&e.file)
                || has(&e.message)
                || e.tags.iter().any(|g| has(g))
        }
    };
    hit != t.negated
}

pub fn matches(q: &Query, e: &Entry) -> bool {
    q.clauses
        .iter()
        .all(|alts| alts.iter().any(|t| term_hits(t, e)))
}

/// A list row: a group header or a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Group {
        key: String,
        label: String,
        /// Results in the group that pass the filters.
        count: usize,
        collapsed: bool,
    },
    Item {
        entry: usize,
    },
}

#[derive(Debug, Clone)]
pub struct SarifView {
    pub logs: Vec<LoadedLog>,
    pub entries: Vec<Entry>,
    pub tab: Tab,
    pub filters: Filters,
    pub query_text: String,
    pub sort: (Column, bool),
    pub collapsed: HashSet<String>,
    pub selected: usize,
    /// Keystrokes go to the filter box rather than the list.
    pub editing_query: bool,
    /// First list row painted (frame truth, kept by the renderer).
    pub scroll: usize,
    /// Screen row of the first list row and how many rows the list shows,
    /// written by the renderer for mouse hit-testing.
    pub rows_top: u16,
    pub rows_visible: u16,
    /// Screen columns the list occupies: `[list_x, list_x + list_width)`.
    pub list_x: u16,
    pub list_width: u16,
}

impl SarifView {
    pub fn new(logs: Vec<LoadedLog>, entries: Vec<Entry>) -> SarifView {
        SarifView {
            logs,
            entries,
            tab: Tab::Locations,
            filters: Filters::default(),
            query_text: String::new(),
            sort: (Column::Line, true),
            collapsed: HashSet::new(),
            selected: 0,
            editing_query: false,
            scroll: 0,
            rows_top: 0,
            rows_visible: 0,
            list_x: 0,
            list_width: 0,
        }
    }

    /// A viewer over one log file. File names display relative to the log's
    /// folder or the working directory when they sit under either.
    pub fn open(path: &std::path::Path, log: SarifLog) -> SarifView {
        let mut roots = Vec::new();
        if let Some(dir) = path.parent() {
            roots.push(dir.to_path_buf());
        }
        if let Ok(cwd) = std::env::current_dir() {
            roots.push(cwd);
        }
        let logs = vec![LoadedLog {
            path: path.to_path_buf(),
            log,
        }];
        let entries = build_entries(&logs, &roots);
        let mut view = SarifView::new(logs, entries);
        // Start on the first result rather than its group header, so the
        // details pane has something to show from the first frame.
        if matches!(view.rows().get(1), Some(Row::Item { .. })) {
            view.selected = 1;
        }
        view
    }

    /// Distinct tool names across every run, for the header.
    pub fn tools(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in &self.logs {
            for r in &l.log.runs {
                let n = &r.tool.driver.name;
                if !n.is_empty() && !out.contains(n) {
                    out.push(n.clone());
                }
            }
        }
        out
    }

    fn passes_chips(&self, e: &Entry, with_levels: bool) -> bool {
        let f = &self.filters;
        (!with_levels || !f.hidden_levels.contains(&e.level))
            && !f.hidden_baselines.contains(&e.baseline)
            && !f.hidden_suppressions.contains(&e.suppression)
            && !f.hidden_kinds.contains(&e.kind)
    }

    /// Entries passing the chips and the keyword query.
    pub fn visible(&self) -> Vec<usize> {
        let q = parse_query(&self.query_text);
        (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                self.passes_chips(e, true) && matches(&q, e)
            })
            .collect()
    }

    fn group_of(&self, e: &Entry) -> (String, String) {
        match self.tab {
            Tab::Locations => {
                let label = if e.file.is_empty() {
                    "No location".to_string()
                } else {
                    e.file.clone()
                };
                (format!("f:{}", e.uri), label)
            }
            Tab::Rules => (
                format!("r:{}", e.rule_id),
                if e.rule_name.is_empty() {
                    e.rule_id.clone()
                } else {
                    format!("{} · {}", e.rule_id, e.rule_name)
                },
            ),
            Tab::Logs => {
                let label = self
                    .logs
                    .get(e.log)
                    .and_then(|l| l.path.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("log {}", e.log + 1));
                (format!("l:{}", e.log), label)
            }
        }
    }

    fn compare(&self, a: &Entry, b: &Entry) -> std::cmp::Ordering {
        let (col, asc) = self.sort;
        let lower = |s: &str| s.to_lowercase();
        let ord = match col {
            Column::Line => a.line.cmp(&b.line).then(a.column.cmp(&b.column)),
            Column::File => lower(&a.file).cmp(&lower(&b.file)),
            Column::Message => lower(&a.message).cmp(&lower(&b.message)),
            Column::Rule => lower(&a.rule_id).cmp(&lower(&b.rule_id)),
            Column::Level => a.level.cmp(&b.level),
        };
        let ord = ord
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| (a.log, a.run, a.result).cmp(&(b.log, b.run, b.result)));
        if asc { ord } else { ord.reverse() }
    }

    /// The grouped projection for the active tab. Groups are ordered by how
    /// many results they hold (most first, ties by label); items within a
    /// group follow `sort`. A group with nothing visible is dropped.
    pub fn rows(&self) -> Vec<Row> {
        let mut groups: Vec<(String, String, Vec<usize>)> = Vec::new();
        for i in self.visible() {
            let (key, label) = self.group_of(&self.entries[i]);
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => g.2.push(i),
                None => groups.push((key, label, vec![i])),
            }
        }
        groups.sort_by(|a, b| {
            b.2.len()
                .cmp(&a.2.len())
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        let mut rows = Vec::new();
        for (key, label, mut members) in groups {
            members.sort_by(|&x, &y| self.compare(&self.entries[x], &self.entries[y]));
            let collapsed = self.collapsed.contains(&key);
            rows.push(Row::Group {
                key,
                label,
                count: members.len(),
                collapsed,
            });
            if !collapsed {
                rows.extend(members.into_iter().map(|entry| Row::Item { entry }));
            }
        }
        rows
    }

    fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.rows().len();
        self.selected = self.selected.saturating_add_signed(delta);
        self.clamp(len);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.rows().len().saturating_sub(1);
    }

    /// The group key owning row `i`: the row itself if it is a header,
    /// otherwise the nearest header above it.
    fn owner(rows: &[Row], i: usize) -> Option<(usize, String)> {
        rows[..=i.min(rows.len().checked_sub(1)?)]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(n, r)| match r {
                Row::Group { key, .. } => Some((n, key.clone())),
                Row::Item { .. } => None,
            })
    }

    /// Fold or unfold the group under the selection (or the selected item's
    /// group, moving the selection onto its header when folding).
    pub fn toggle_fold(&mut self) {
        let rows = self.rows();
        let Some((_, key)) = Self::owner(&rows, self.selected) else {
            return;
        };
        let collapse = !self.collapsed.contains(&key);
        self.set_fold(collapse);
    }

    pub fn set_fold(&mut self, collapsed: bool) {
        let rows = self.rows();
        let Some((header, key)) = Self::owner(&rows, self.selected) else {
            return;
        };
        if collapsed {
            self.collapsed.insert(key);
            self.selected = header;
        } else {
            self.collapsed.remove(&key);
        }
    }

    pub fn fold_all(&mut self, collapsed: bool) {
        if collapsed {
            let keys: Vec<String> = self
                .rows()
                .into_iter()
                .filter_map(|r| match r {
                    Row::Group { key, .. } => Some(key),
                    Row::Item { .. } => None,
                })
                .collect();
            self.collapsed.extend(keys);
        } else {
            self.collapsed.clear();
        }
        let len = self.rows().len();
        self.clamp(len);
    }

    /// The selected result, if the selection is on an item.
    pub fn selected_entry(&self) -> Option<&Entry> {
        match self.rows().get(self.selected)? {
            Row::Item { entry } => self.entries.get(*entry),
            Row::Group { .. } => None,
        }
    }

    fn selected_index(&self) -> Option<usize> {
        match self.rows().get(self.selected)? {
            Row::Item { entry } => Some(*entry),
            Row::Group { .. } => None,
        }
    }

    /// Re-point the selection at `entry` after the projection changed, or
    /// clamp it when that result is no longer listed.
    fn reselect(&mut self, entry: Option<usize>) {
        let rows = self.rows();
        if let Some(e) = entry
            && let Some(n) = rows.iter().position(|r| *r == Row::Item { entry: e })
        {
            self.selected = n;
            return;
        }
        self.clamp(rows.len());
    }

    /// Clicking a column sorts by it; clicking the sorted column reverses it.
    pub fn sort_by(&mut self, col: Column) {
        let keep = self.selected_index();
        self.sort = if self.sort.0 == col {
            (col, !self.sort.1)
        } else {
            (col, true)
        };
        self.reselect(keep);
    }

    /// Replace the keyword query, keeping the selection on the same result
    /// when it is still visible.
    pub fn set_query(&mut self, text: &str) {
        let keep = self.selected_index();
        self.query_text = text.to_string();
        self.reselect(keep);
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.selected = 0;
    }

    /// Clear the query and show every chip value (VS Code's "Clear Filters").
    pub fn clear_filters(&mut self) {
        let keep = self.selected_index();
        self.query_text.clear();
        self.filters = Filters {
            hidden_levels: HashSet::new(),
            hidden_baselines: HashSet::new(),
            hidden_suppressions: HashSet::new(),
            hidden_kinds: HashSet::new(),
        };
        self.reselect(keep);
    }

    /// Per-level counts over results passing everything but the level chips,
    /// for the chip badges.
    pub fn level_counts(&self) -> [(Level, usize); 4] {
        let q = parse_query(&self.query_text);
        let mut out = [
            (Level::Error, 0),
            (Level::Warning, 0),
            (Level::Note, 0),
            (Level::None, 0),
        ];
        for e in &self.entries {
            if self.passes_chips(e, false)
                && matches(&q, e)
                && let Some(slot) = out.iter_mut().find(|(l, _)| *l == e.level)
            {
                slot.1 += 1;
            }
        }
        out
    }
}

/// Flatten every result of every log into entries. `roots` shortens file
/// URIs for display: a path under a workspace root shows relative to it.
pub fn build_entries(logs: &[LoadedLog], roots: &[PathBuf]) -> Vec<Entry> {
    let mut out = Vec::new();
    for (li, loaded) in logs.iter().enumerate() {
        for (ri, run) in loaded.log.runs.iter().enumerate() {
            for (xi, result) in run.results.iter().flatten().enumerate() {
                out.push(entry_for(run, result, (li, ri, xi), roots));
            }
        }
    }
    out
}

fn entry_for(
    run: &Run,
    result: &SarifResult,
    (log, run_index, result_index): (usize, usize, usize),
    roots: &[PathBuf],
) -> Entry {
    let rule = sem::rule_for(run, result);
    let component = sem::rule_component(run, result);
    let text = sem::message_text(&result.message, rule, Some(component));
    let message = sem::segments(&text)
        .into_iter()
        .map(|seg| match seg {
            sem::Segment::Text(t) => t,
            sem::Segment::LocationLink { text, .. } | sem::Segment::UriLink { text, .. } => text,
        })
        .collect();
    let physical = result
        .locations
        .first()
        .and_then(|l| l.physical_location.as_ref());
    let uri = physical
        .and_then(|p| p.artifact_location.as_ref())
        .and_then(|a| expand(run, a))
        .map(|e| e.uri)
        .unwrap_or_default();
    let region = physical.and_then(|p| p.region.as_ref());
    let mut tags = string_list(rule.and_then(|r| r.properties.get("tags")));
    for t in string_list(result.properties.get("tags")) {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    Entry {
        log,
        run: run_index,
        result: result_index,
        rule_id: sem::rule_id(run, result).unwrap_or_default(),
        rule_name: rule.and_then(|r| r.name.clone()).unwrap_or_default(),
        tool: component.name.clone(),
        level: sem::effective_level(result, rule),
        kind: sem::result_kind(result),
        baseline: sem::baseline_state(result),
        suppression: sem::suppression_state(result),
        message,
        file: display_file(&uri, roots),
        uri,
        line: region.and_then(|r| r.start_line).unwrap_or(0),
        column: region.and_then(|r| r.start_column).unwrap_or(0),
        tags,
    }
}

fn string_list(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A file URI under a workspace root shows relative to it; any other file
/// URI shows as its path; a relative reference shows as written.
fn display_file(uri: &str, roots: &[PathBuf]) -> String {
    if uri.is_empty() {
        return String::new();
    }
    let Some(path) = uri_to_path(uri) else {
        return uri.to_string();
    };
    if path.is_absolute() {
        for root in roots {
            if let Ok(rel) = path.strip_prefix(root) {
                return rel.to_string_lossy().into_owned();
            }
        }
    }
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, line: i64, rule: &str, level: Level, msg: &str) -> Entry {
        Entry {
            log: 0,
            run: 0,
            result: 0,
            rule_id: rule.into(),
            rule_name: format!("{rule}-name"),
            tool: "lint".into(),
            level,
            kind: Kind::Fail,
            baseline: BaselineState::Unspecified,
            suppression: SuppressionState::Unknown,
            message: msg.into(),
            uri: format!("file:///ws/{file}"),
            file: file.into(),
            line,
            column: 1,
            tags: vec![],
        }
    }

    fn sample() -> SarifView {
        let mut es = vec![
            entry("src/a.rs", 30, "R1", Level::Error, "SQL built from input"),
            entry("src/a.rs", 10, "R2", Level::Warning, "unused variable x"),
            entry("src/b.rs", 5, "R1", Level::Error, "SQL built from header"),
            entry("src/a.rs", 20, "R3", Level::Note, "prefer ? over unwrap"),
            entry("", 0, "R4", Level::None, "no location"),
        ];
        es[3].tags = vec!["style".into()];
        for (i, e) in es.iter_mut().enumerate() {
            e.result = i;
        }
        SarifView::new(Vec::new(), es)
    }

    fn items(rows: &[Row]) -> Vec<usize> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Item { entry } => Some(*entry),
                _ => None,
            })
            .collect()
    }

    fn groups(rows: &[Row]) -> Vec<(String, usize)> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Group { label, count, .. } => Some((label.clone(), *count)),
                _ => None,
            })
            .collect()
    }

    // ── query grammar ───────────────────────────────────────────────────

    #[test]
    fn query_terms_are_anded() {
        let v = sample();
        let q = parse_query("sql header");
        let hits: Vec<_> = v
            .entries
            .iter()
            .filter(|e| matches(&q, e))
            .map(|e| e.result)
            .collect();
        assert_eq!(hits, vec![2]);
    }

    #[test]
    fn query_pipe_is_or() {
        let v = sample();
        let q = parse_query("header|unused");
        let hits: Vec<_> = v
            .entries
            .iter()
            .filter(|e| matches(&q, e))
            .map(|e| e.result)
            .collect();
        assert_eq!(hits, vec![1, 2]);
    }

    #[test]
    fn query_negation_and_fields() {
        let v = sample();
        let hit = |s: &str| -> Vec<usize> {
            let q = parse_query(s);
            v.entries
                .iter()
                .filter(|e| matches(&q, e))
                .map(|e| e.result)
                .collect()
        };
        assert_eq!(hit("rule:r1 -file:b.rs"), vec![0]);
        assert_eq!(hit("level:note"), vec![3]);
        assert_eq!(hit("tag:style"), vec![3]);
        assert_eq!(hit("msg:sql"), vec![0, 2]);
        assert_eq!(hit("tool:lint").len(), 5);
        // A field name that is not a field is just text.
        assert_eq!(hit("zz:top"), Vec::<usize>::new());
    }

    #[test]
    fn query_is_case_insensitive_and_empty_matches_all() {
        let v = sample();
        let q = parse_query("SQL");
        assert_eq!(v.entries.iter().filter(|e| matches(&q, e)).count(), 2);
        let q = parse_query("   ");
        assert_eq!(v.entries.iter().filter(|e| matches(&q, e)).count(), 5);
    }

    #[test]
    fn query_plain_text_searches_rule_file_and_message() {
        let v = sample();
        let hit = |s: &str| {
            v.entries
                .iter()
                .filter(|e| matches(&parse_query(s), e))
                .count()
        };
        assert_eq!(hit("r2-name"), 1);
        assert_eq!(hit("b.rs"), 1);
        assert_eq!(hit("unwrap"), 1);
    }

    #[test]
    fn query_bare_minus_is_text() {
        let q = parse_query("-");
        assert_eq!(
            q.clauses,
            vec![vec![Term {
                field: None,
                needle: "-".into(),
                negated: false
            }]]
        );
    }

    // ── filters ─────────────────────────────────────────────────────────

    #[test]
    fn default_filters_hide_absent_and_suppressed_only() {
        let f = Filters::default();
        assert!(f.hidden_baselines.contains(&BaselineState::Absent));
        assert!(
            f.hidden_suppressions
                .contains(&SuppressionState::Suppressed)
        );
        assert!(
            !f.hidden_suppressions
                .contains(&SuppressionState::UnderReview)
        );
        assert!(f.hidden_levels.is_empty());
        assert!(f.hidden_kinds.is_empty());
    }

    #[test]
    fn chips_hide_results() {
        let mut v = sample();
        v.entries[1].suppression = SuppressionState::Suppressed;
        v.entries[2].baseline = BaselineState::Absent;
        assert_eq!(v.visible(), vec![0, 3, 4]);
        v.filters.hidden_levels.insert(Level::Note);
        assert_eq!(v.visible(), vec![0, 4]);
        v.clear_filters();
        assert_eq!(v.visible(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn level_counts_ignore_level_chips_but_respect_others() {
        let mut v = sample();
        v.filters.hidden_levels.insert(Level::Error);
        v.entries[1].suppression = SuppressionState::Suppressed;
        assert_eq!(
            v.level_counts(),
            [
                (Level::Error, 2),
                (Level::Warning, 0),
                (Level::Note, 1),
                (Level::None, 1)
            ]
        );
    }

    // ── grouping and sort ───────────────────────────────────────────────

    #[test]
    fn locations_group_by_file_biggest_first_items_by_line() {
        let v = sample();
        let rows = v.rows();
        assert_eq!(
            groups(&rows),
            vec![
                ("src/a.rs".to_string(), 3),
                ("No location".to_string(), 1),
                ("src/b.rs".to_string(), 1),
            ]
        );
        assert_eq!(items(&rows), vec![1, 3, 0, 4, 2]);
    }

    #[test]
    fn rules_tab_groups_by_rule() {
        let mut v = sample();
        v.set_tab(Tab::Rules);
        let g = groups(&v.rows());
        assert_eq!(g[0], ("R1 · R1-name".to_string(), 2));
        assert_eq!(g.len(), 4);
    }

    #[test]
    fn logs_tab_groups_by_log_file() {
        let mut v = sample();
        v.logs = vec![
            LoadedLog {
                path: PathBuf::from("/x/one.sarif"),
                log: SarifLog::default(),
            },
            LoadedLog {
                path: PathBuf::from("/x/two.sarif"),
                log: SarifLog::default(),
            },
        ];
        v.entries[4].log = 1;
        v.set_tab(Tab::Logs);
        assert_eq!(
            groups(&v.rows()),
            vec![("one.sarif".to_string(), 4), ("two.sarif".to_string(), 1)]
        );
    }

    #[test]
    fn sort_by_toggles_direction_and_switches_column() {
        let mut v = sample();
        v.sort_by(Column::Line);
        assert_eq!(v.sort, (Column::Line, false));
        assert_eq!(items(&v.rows())[..3], [0, 3, 1]);
        v.sort_by(Column::Message);
        assert_eq!(v.sort, (Column::Message, true));
        assert_eq!(items(&v.rows())[..3], [3, 0, 1]);
        v.sort_by(Column::Level);
        assert_eq!(items(&v.rows())[..3], [0, 1, 3]);
    }

    #[test]
    fn sort_by_file_and_rule() {
        let mut v = sample();
        v.set_tab(Tab::Rules);
        v.sort_by(Column::File);
        // R1 holds results 0 (src/a.rs) and 2 (src/b.rs).
        assert_eq!(items(&v.rows())[..2], [0, 2]);
        v.sort_by(Column::File);
        assert_eq!(items(&v.rows())[..2], [2, 0]);
        v.set_tab(Tab::Locations);
        v.sort_by(Column::Rule);
        assert_eq!(items(&v.rows())[..3], [0, 1, 3]);
    }

    const LOG: &str = r#"{"version":"2.1.0","runs":[{
        "tool":{"driver":{"name":"CodeQL","rules":[
            {"id":"js/sql-injection","name":"SqlInjection",
             "defaultConfiguration":{"level":"error"},
             "messageStrings":{"m":{"text":"Query built from {0}."}},
             "properties":{"tags":["security","external/cwe/cwe-089"]}}]}},
        "originalUriBaseIds":{"SRC":{"uri":"file:///ws/"}},
        "results":[
            {"ruleId":"js/sql-injection","ruleIndex":0,
             "message":{"id":"m","arguments":["[user input](1)"]},
             "locations":[{"physicalLocation":{
                "artifactLocation":{"uri":"src/db.js","uriBaseId":"SRC"},
                "region":{"startLine":42,"startColumn":7}}}],
             "baselineState":"new",
             "suppressions":[{"kind":"inSource","status":"underReview"}]},
            {"ruleId":"js/other","kind":"pass",
             "message":{"text":"ok"}}
        ]}]}"#;

    #[test]
    fn build_entries_resolves_everything_the_list_needs() {
        let log = crate::sarif::load::parse_log(LOG).unwrap();
        let logs = vec![LoadedLog {
            path: PathBuf::from("/ws/out.sarif"),
            log,
        }];
        let es = build_entries(&logs, &[PathBuf::from("/ws")]);
        assert_eq!(es.len(), 2);
        let e = &es[0];
        assert_eq!((e.log, e.run, e.result), (0, 0, 0));
        assert_eq!(e.rule_id, "js/sql-injection");
        assert_eq!(e.rule_name, "SqlInjection");
        assert_eq!(e.tool, "CodeQL");
        assert_eq!(e.level, Level::Error);
        assert_eq!(e.kind, Kind::Fail);
        assert_eq!(e.baseline, BaselineState::New);
        assert_eq!(e.suppression, SuppressionState::UnderReview);
        assert_eq!(e.message, "Query built from user input.");
        assert_eq!(e.uri, "file:///ws/src/db.js");
        assert_eq!(e.file, "src/db.js");
        assert_eq!((e.line, e.column), (42, 7));
        assert_eq!(
            e.tags,
            vec!["security".to_string(), "external/cwe/cwe-089".to_string()]
        );
        let p = &es[1];
        assert_eq!(p.kind, Kind::Pass);
        assert_eq!(p.level, Level::None);
        assert_eq!((p.uri.as_str(), p.file.as_str(), p.line), ("", "", 0));
        assert_eq!(p.rule_name, "");
    }

    #[test]
    fn build_entries_keeps_foreign_paths_whole() {
        let log = crate::sarif::load::parse_log(
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"t"}},"results":[
                {"message":{"text":"m"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///ci/x.c"}}}]},
                {"message":{"text":"m"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"rel/y.c"}}}]}
            ]}]}"#,
        )
        .unwrap();
        let es = build_entries(
            &[LoadedLog {
                path: PathBuf::new(),
                log,
            }],
            &[PathBuf::from("/ws")],
        );
        assert_eq!(es[0].file, "/ci/x.c");
        assert_eq!(es[1].file, "rel/y.c");
    }

    #[test]
    fn empty_groups_are_dropped() {
        let mut v = sample();
        v.set_query("header");
        assert_eq!(groups(&v.rows()), vec![("src/b.rs".to_string(), 1)]);
    }

    // ── folding and selection ───────────────────────────────────────────

    #[test]
    fn selection_moves_over_headers_and_items_and_clamps() {
        let mut v = sample();
        assert_eq!(v.selected, 0);
        assert!(v.selected_entry().is_none(), "row 0 is a header");
        v.move_selection(1);
        assert_eq!(v.selected_entry().unwrap().result, 1);
        v.move_selection(-10);
        assert_eq!(v.selected, 0);
        v.move_selection(100);
        assert_eq!(v.selected, v.rows().len() - 1);
        v.select_first();
        assert_eq!(v.selected, 0);
        v.select_last();
        assert_eq!(v.selected, v.rows().len() - 1);
    }

    #[test]
    fn fold_from_an_item_folds_its_group_and_lands_on_the_header() {
        let mut v = sample();
        v.move_selection(2); // second item of src/a.rs
        v.toggle_fold();
        let rows = v.rows();
        assert_eq!(v.selected, 0);
        assert!(matches!(
            &rows[0],
            Row::Group {
                collapsed: true,
                ..
            }
        ));
        assert_eq!(items(&rows), vec![4, 2]);
        v.toggle_fold();
        assert_eq!(items(&v.rows()), vec![1, 3, 0, 4, 2]);
    }

    #[test]
    fn fold_all_and_unfold_all() {
        let mut v = sample();
        v.fold_all(true);
        assert!(items(&v.rows()).is_empty());
        assert_eq!(v.rows().len(), 3);
        v.fold_all(false);
        assert_eq!(items(&v.rows()).len(), 5);
    }

    #[test]
    fn query_change_keeps_the_selected_result() {
        let mut v = sample();
        v.move_selection(3); // result 0 (src/a.rs line 30)
        assert_eq!(v.selected_entry().unwrap().result, 0);
        v.set_query("sql");
        assert_eq!(v.selected_entry().unwrap().result, 0);
    }

    #[test]
    fn query_change_clamps_when_the_selection_disappears() {
        let mut v = sample();
        v.select_last();
        v.set_query("unused");
        assert!(v.selected < v.rows().len());
    }

    #[test]
    fn tab_switch_resets_selection() {
        let mut v = sample();
        v.move_selection(3);
        v.set_tab(Tab::Rules);
        assert_eq!(v.selected, 0);
    }
}
