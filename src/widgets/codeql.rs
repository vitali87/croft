//! The CodeQL side bar (#578): the view behind the QL activity-bar icon,
//! laid out as VS Code's CodeQL extension lays out its container, in
//! croft's own dress: the Explorer's pane frame, and with no database a
//! first-run card whose one button adds one. Sections fold like VS Code's
//! view panes; an empty one starts folded to a one-line summary.

// The activity bar and side bar that use this land in the next change.
#![allow(dead_code)]

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Widget},
};

/// VS Code's `ql-container` views, in its order. The Evaluator Log Viewer is
/// canary-only there and is left out here for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Language,
    Databases,
    Queries,
    VariantAnalysis,
    /// The query run in flight, with its progress, shown only while one
    /// runs (#578).
    Running,
    QueryHistory,
    AstViewer,
    EvaluatorLog,
    MethodModeling,
    /// The AST Viewer, Evaluator Log and Method Modeling folded into one
    /// row while they have nothing to show.
    Tools,
}

impl Section {
    pub const ALL: [Section; 10] = [
        Section::Language,
        Section::Databases,
        Section::Queries,
        Section::VariantAnalysis,
        Section::Running,
        Section::QueryHistory,
        Section::AstViewer,
        Section::EvaluatorLog,
        Section::MethodModeling,
        Section::Tools,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Language => "LANGUAGE",
            Section::Databases => "DATABASES",
            Section::Queries => "QUERIES",
            Section::VariantAnalysis => "VARIANT ANALYSIS",
            Section::Running => "RUNNING",
            Section::QueryHistory => "QUERY HISTORY",
            Section::AstViewer => "AST VIEWER",
            Section::EvaluatorLog => "EVALUATOR LOG VIEWER",
            Section::MethodModeling => "METHOD MODELING",
            Section::Tools => "TOOLS",
        }
    }
}

/// Something a welcome line offers to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    AddDatabaseFromFolder,
    AddDatabaseFromArchive,
    AddDatabaseFromUrl,
    AddDatabaseFromGithub,
    CreateQuery,
    SetUpControllerRepository,
    ViewAst,
    /// Node `.0` of the AST shown: go to its code and fold or unfold it.
    AstNode(usize),
    /// Forget the AST shown.
    ClearAst,
    /// Predicate `.0` of the evaluator log shown, or a line under it: fold
    /// or unfold it.
    EvalPredicate(usize),
    /// A dependency: go to the predicate it names, when the log has it.
    EvalDependency(Option<usize>),
    /// Forget the evaluator log shown.
    ClearEvalLog,
    /// Read, or read again, the endpoints of the current database for the
    /// Model Editor.
    OpenModelEditor,
    /// Fold or unfold the Model Editor group endpoint `.0` belongs to.
    ModelGroup(usize),
    /// Model Editor endpoint `.0`: go to its code.
    ModelEndpoint(usize),
    SelectLanguage(usize),
    /// Make the listed database at this index the current one.
    SelectDatabase(usize),
    /// Step the Databases list to its next sort order.
    SortDatabases,
    /// Step the Query History list to its next sort order.
    SortHistory,
    /// Open the results of the query history entry at this index.
    OpenHistory(usize),
    /// Fold or unfold the query pack at this index of `queries`.
    TogglePack(usize),
    /// Run query `.1` of pack `.0` on the current database.
    RunQuery(usize, usize),
    /// Variant analysis list `.0`: select it, or fold it once selected.
    VariantList(usize),
    /// Select repository `.1` of variant analysis list `.0`, or of the
    /// single repositories.
    VariantRepo(Option<usize>, usize),
    /// Select the variant analysis owner at this index.
    VariantOwner(usize),
    AddVariantRepo,
    AddVariantList,
    AddVariantOwner,
    /// Open the variant analysis config file in an editor tab.
    OpenVariantConfig,
    /// Submitted variant analysis `.0` (in the order runs are remembered):
    /// open its per-repository report.
    VariantRun(usize),
    /// Offer the four places a database comes from in a picker.
    AddDatabase,
    /// Run CodeQL: Quick Query.
    QuickQuery,
    /// Run CodeQL: Show Evaluator Log (Viewer).
    ShowEvaluatorLog,
    /// Offer the databases in a picker whose choice becomes the current one.
    PickDatabase,
    /// Run the open query on the current database, as "Run Query on
    /// Selected Database" does.
    RunOpenQuery,
    /// Compare the results of query history entry `.0` with its last run.
    CompareHistory(usize),
    /// Export the results of query history entry `.0`.
    ExportHistory(usize),
    /// Show the query log of query history entry `.0`.
    HistoryLog(usize),
}

/// The languages CodeQL analyses, as VS Code's Language view lists them.
pub const LANGUAGES: [&str; 10] = [
    "C / C++",
    "C#",
    "GitHub Actions",
    "Go",
    "Java / Kotlin",
    "JavaScript / TypeScript",
    "Python",
    "Ruby",
    "Rust",
    "Swift",
];

/// The extractor id behind each [`LANGUAGES`] entry, in the same order: what
/// a pack's `extractor:` or `codeql/<id>-all` dependency says.
pub const LANGUAGE_IDS: [&str; 10] = [
    "cpp",
    "csharp",
    "actions",
    "go",
    "java",
    "javascript",
    "python",
    "ruby",
    "rust",
    "swift",
];

/// One painted line of the side bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Header(Section),
    Text(&'static str),
    Action(Action, String),
    /// An empty row.
    Blank,
    /// A row of the first-run card, drawn and never selected.
    Card(Card),
    /// Row `.1` of the three-row button for `.0`; its label row is the one
    /// selected, and a click on any of the three runs it.
    Button(Action, ButtonRow),
    /// A row of the current database's card; its name row is the one
    /// selected, and opens the database picker.
    DbCard(DbCard),
    /// Query history entry `.0`: its glyph, name, count and duration.
    History(usize),
    /// A detail line under expanded query history entry `.0`.
    HistoryDetail(usize, Detail),
    /// A row of the Running section, drawn and never selected.
    Running(RunRow),
}

/// The rows of the first-run card, top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Card {
    Top,
    Bottom,
    Blank,
    /// Row `.0` of the line-art illustration.
    Art(usize),
    Heading(String),
    Body(String),
    /// Step `.0` of the checklist.
    Step(usize),
    Caption(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonRow {
    Top,
    Label,
    Bottom,
}

/// The rows of the database card, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbCard {
    /// The top edge, titled.
    Top,
    /// The language mark, the name and the switch chevron.
    Name,
    /// The language, size and age; left out with no current database.
    Meta,
    Blank,
    /// The Run, Quick Query and AST chips.
    Chips,
    Bottom,
}

/// The detail lines of an expanded query history row, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// The result count and the format it is in.
    Summary,
    /// The database and when it ran.
    Where,
    /// Line `.0` of the action chips, which wrap to the width.
    Chips(usize),
}

/// The rows of the Running section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunRow {
    Name,
    /// The progress bar and the time elapsed.
    Progress,
}

/// A chip: what it runs, its label, and the key that runs it too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chip {
    pub action: Action,
    pub label: &'static str,
    pub key: Option<char>,
}

/// The database card's chips.
pub const DB_CHIPS: [Chip; 3] = [
    Chip {
        action: Action::RunOpenQuery,
        label: "\u{25b6} Run",
        key: Some('r'),
    },
    Chip {
        action: Action::QuickQuery,
        label: "\u{bb} Quick",
        key: Some('q'),
    },
    Chip {
        action: Action::ViewAst,
        label: "AST",
        key: Some('a'),
    },
];

/// The column of an expanded history row's chips, right of its tree line.
const HISTORY_CHIP_X: u16 = 6;

/// The chips under expanded query history entry `i`.
pub fn history_chips(i: usize) -> [Chip; 4] {
    let chip = |action, label| Chip {
        action,
        label,
        key: None,
    };
    [
        chip(Action::OpenHistory(i), "\u{2197} Open"),
        chip(Action::CompareHistory(i), "\u{2194} Compare"),
        chip(Action::ExportHistory(i), "\u{2193} Export"),
        chip(Action::HistoryLog(i), "\u{2261} Log"),
    ]
}

/// Lay `chips` out from column `x0` of a row, left of column `limit`: with
/// their keys and a gap between, else with no gap, else without their
/// keys, dropping from the end those that still do not fit. Each placed
/// chip comes with its column, its width and whether its key shows.
fn lay_out_chips(chips: &[Chip], x0: u16, limit: u16) -> Vec<(u16, u16, Chip, bool)> {
    let width = |c: &Chip, keys: bool| {
        let key = if keys && c.key.is_some() { 2 } else { 0 };
        c.label.chars().count() as u16 + 2 + key
    };
    let place = |keys: bool, gap: u16| {
        let mut out = Vec::new();
        let mut x = x0;
        for c in chips {
            let w = width(c, keys);
            if x + w > limit {
                break;
            }
            out.push((x, w, *c, keys && c.key.is_some()));
            x += w + gap;
        }
        out
    };
    for (keys, gap) in [(true, 1), (true, 0)] {
        let all = place(keys, gap);
        if all.len() == chips.len() {
            return all;
        }
    }
    place(false, 1)
}

/// One Query History row (#578): what its glyph, name and columns show,
/// and what its details say once expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    pub status: crate::codeql_query::RunStatus,
    /// The user's label, else the query's file name.
    pub name: String,
    pub database: String,
    pub results: Option<u64>,
    pub seconds: u64,
    /// When it started, in unix seconds.
    pub started: u64,
    /// The SARIF or CSV the run wrote, which also tells the entry apart.
    pub output: std::path::PathBuf,
}

impl HistoryRow {
    pub fn new(entry: &crate::codeql_query::HistoryEntry) -> Self {
        Self {
            status: entry.status.clone(),
            name: entry.display_name(),
            database: entry.database.clone(),
            results: entry.results,
            seconds: entry.seconds,
            started: entry.started,
            output: entry.output.clone(),
        }
    }

    fn is_sarif(&self) -> bool {
        self.output
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("sarif"))
    }

    /// The count column: the count, `err` for a failure, a dash when there
    /// is none, and nothing while it runs.
    fn count_text(&self) -> String {
        use crate::codeql_query::RunStatus;
        match (&self.status, self.results) {
            (RunStatus::Succeeded, Some(n)) => n.to_string(),
            (RunStatus::Failed(_), _) => String::from("err"),
            (RunStatus::Running, _) => String::new(),
            _ => String::from("\u{2014}"),
        }
    }

    /// The duration column, empty while it runs.
    fn duration_text(&self) -> String {
        match self.status {
            crate::codeql_query::RunStatus::Running => String::new(),
            _ => duration(self.seconds),
        }
    }
}

/// A run's length as the history's duration column shows it: `4s`,
/// `1m08`, `2h05`.
fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}", secs / 60, secs % 60),
        _ => format!("{}h{:02}", secs / 3600, secs / 60 % 60),
    }
}

/// How long ago something was, in the card's short form: `2h ago`.
fn short_age(secs: u64) -> String {
    const DAY: u64 = 86_400;
    match secs {
        0..60 => String::from("just now"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..DAY => format!("{}h ago", secs / 3600),
        _ if secs < 30 * DAY => format!("{}d ago", secs / DAY),
        _ if secs < 365 * DAY => format!("{}mo ago", secs / (30 * DAY)),
        _ => format!("{}y ago", secs / (365 * DAY)),
    }
}

/// A size in the card's short form: `214 MB`.
fn short_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let n = bytes as f64;
    if n < KB {
        format!("{bytes} B")
    } else if n < KB * KB {
        format!("{:.0} KB", n / KB)
    } else if n < KB * KB * KB {
        format!("{:.0} MB", n / (KB * KB))
    } else {
        format!("{:.1} GB", n / (KB * KB * KB))
    }
}

/// Seconds since the Unix epoch.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// When a run started: the local `HH:MM` within the last day, else how
/// long ago.
fn clock(started: u64, now: u64) -> String {
    let ago = now.saturating_sub(started);
    if ago >= 86_400 {
        return short_age(ago);
    }
    // libc's localtime, as the terminal's timestamps take it: no date
    // crate for one field. The alias is deprecated on musl; see there.
    #[allow(deprecated)]
    let secs = started as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return short_age(ago);
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// The colour the Explorer gives files of CodeQL language `lang`.
fn language_color(lang: &str) -> Option<Color> {
    let suffix = match lang {
        "cpp" | "c" | "c-cpp" => ".cpp",
        "csharp" => ".cs",
        "actions" => ".yml",
        "go" => ".go",
        "java" | "java-kotlin" => ".java",
        "kotlin" => ".kt",
        "javascript" | "javascript-typescript" => ".js",
        "typescript" => ".ts",
        "python" => ".py",
        "ruby" => ".rb",
        "rust" => ".rs",
        "swift" => ".swift",
        _ => return None,
    };
    Some(crate::icons::for_path("", suffix).color)
}

/// Which of a bar's `n` cells the indeterminate sweep lights `elapsed`
/// seconds into a run: a third of the bar, crossing it two cells a second
/// and starting over. The CLI reports no progress to fill it by.
pub fn sweep(n: u16, elapsed: u64) -> std::ops::Range<u16> {
    let len = (n / 3).max(1);
    // The head runs from the first cell to the last cell past the end that
    // still leaves one lit, so the bar is never dark.
    let span = u64::from(n + len - 1).max(1);
    let head = 1 + ((elapsed * 2 + u64::from(len) - 1) % span) as u16;
    head.saturating_sub(len)..head.min(n)
}

/// The checklist on the first-run card: the first step is the one to do.
pub const STEPS: [&str; 3] = [
    "Add a database",
    "Write or open a query",
    "Run it, read the flows",
];

/// The illustration on the first-run card: a dotted frame round a small
/// graph and a lens reading "QL". Each piece is a column, its text and
/// what it is drawn as.
const ART_W: u16 = 15;
const ART: [&[(u16, &str, Art)]; 7] = [
    &[(0, "· · · · · · · ·", Art::Dots)],
    &[
        (0, "·", Art::Dots),
        (4, "●───●", Art::Node),
        (14, "·", Art::Dots),
    ],
    &[
        (0, "·", Art::Dots),
        (4, "│", Art::Edge),
        (9, "╲", Art::Edge),
        (14, "·", Art::Dots),
    ],
    &[
        (0, "·", Art::Dots),
        (4, "●", Art::Node),
        (7, "╭──╮", Art::Lens),
        (14, "·", Art::Dots),
    ],
    &[
        (0, "·", Art::Dots),
        (5, "╲", Art::Edge),
        (7, "│", Art::Lens),
        (8, "QL", Art::Label),
        (10, "│", Art::Lens),
        (14, "·", Art::Dots),
    ],
    &[
        (0, "·", Art::Dots),
        (6, "●", Art::Node),
        (7, "╰──╯", Art::Lens),
        (11, "╲", Art::Lens),
        (14, "·", Art::Dots),
    ],
    &[(0, "· · · · · · · ·", Art::Dots)],
];

#[derive(Debug, Clone, Copy)]
enum Art {
    Dots,
    Node,
    Edge,
    Lens,
    Label,
}

/// Split `text` into lines of at most `width` characters at spaces; a word
/// longer than the width is cut.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word: String = word.chars().take(width).collect();
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// What a click or Enter landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Header(Section),
    Action(Action),
}

#[derive(Debug, Default)]
pub struct CodeqlPanel {
    pub collapsed: std::collections::HashSet<Section>,
    /// Index into `lines()` of the selected row (headers and actions only).
    pub selected: usize,
    pub scroll: usize,
    /// `None` = every language (VS Code's default).
    pub language: Option<usize>,
    pub focused: bool,
    pub theme: crate::theme::Theme,
    /// Frame truth for mouse routing.
    pub last_area: Rect,
    /// The databases the user added, and which one queries run against.
    pub databases: Vec<crate::codeql_db::DbEntry>,
    pub current_db: Option<usize>,
    /// The order the store keeps the databases in, shown on the sort row.
    pub db_sort: Option<crate::codeql_db::DbSort>,
    /// The query history rows, in the store's order (#578).
    pub history: Vec<HistoryRow>,
    /// The order the store keeps the history in, shown on the sort row.
    pub history_sort: crate::codeql_query::HistSort,
    /// The workspace's queries by pack, from the last discovery.
    pub queries: Vec<crate::codeql_query::QueryPack>,
    /// Folded packs, by folder, so a fold survives rediscovery.
    pub folded_packs: std::collections::HashSet<std::path::PathBuf>,
    /// A discovery of the workspace's queries is running off the UI thread
    /// (#840): an empty Queries section says so rather than "none found".
    pub discovering_queries: bool,
    /// The controller repository and the repositories variant analysis
    /// runs against, from the last load of their config.
    pub variant: crate::codeql_variant::VariantConfig,
    /// The variant analysis config file could not be read; the section
    /// says so instead of showing an empty config.
    pub variant_error: bool,
    /// Folded variant analysis lists, by name.
    pub folded_lists: std::collections::HashSet<String>,
    /// A line per submitted variant analysis, oldest first (#578).
    pub variant_runs: Vec<String>,
    /// The AST the AST Viewer section shows, once one has been read.
    pub ast: Option<crate::codeql_ast::AstView>,
    /// The evaluator log the Evaluator Log Viewer section shows.
    pub evallog: Option<crate::codeql_evallog::LogView>,
    /// The endpoints the Method Modeling section shows.
    pub model: Option<crate::codeql_model::ModelView>,
    /// Sections that start folded (an empty one, and Tools) the user
    /// unfolded.
    pub opened: std::collections::HashSet<Section>,
    /// The pane's frame wears the brand gradient when focused, as the
    /// Explorer's does. Set by the app.
    pub focus_gradient: bool,
    /// The rows inside the frame, from the last painted frame: clicks map
    /// to them and the card wraps to their width.
    pub last_inner: Rect,
    /// The output of the query history entry shown expanded, which keeps
    /// it expanded through a sort.
    pub expanded: Option<std::path::PathBuf>,
    /// The query in flight and the seconds it has run. Set by the app each
    /// frame.
    pub running: Option<(String, u64)>,
    /// The bytes under each database's folder, by folder: summed by the
    /// app when the list loads or changes, never while painting.
    pub db_sizes: std::collections::HashMap<std::path::PathBuf, u64>,
}

impl CodeqlPanel {
    /// The Language list starts folded, its choice shown on the header, so
    /// every section's header fits a normal-height side bar.
    pub fn new() -> Self {
        Self {
            collapsed: std::collections::HashSet::from([Section::Language]),
            ..Self::default()
        }
    }

    /// Whether pack `pack` shows under the selected language. A pack that
    /// does not say its language (and the no-pack group) always shows: it
    /// could be any language, and hiding it would hide queries.
    pub fn pack_matches_language(&self, pack: &crate::codeql_query::QueryPack) -> bool {
        match (self.language, pack.language.as_deref()) {
            (Some(i), Some(lang)) => LANGUAGE_IDS[i] == lang,
            _ => true,
        }
    }

    /// Whether database `db` shows under the selected language. One whose
    /// language is unknown shows under any, like a pack that does not say.
    pub fn db_matches_language(&self, db: &crate::codeql_db::DbEntry) -> bool {
        match (self.language, db.language.as_deref()) {
            (Some(i), Some(lang)) => crate::codeql_db::language_label(lang) == Some(LANGUAGES[i]),
            _ => true,
        }
    }

    /// The store index of the database the selection acts on: the current
    /// one, while the card's name row is selected.
    pub fn selected_database(&self) -> Option<usize> {
        match self.selected_hit() {
            Some(Hit::Action(Action::PickDatabase)) => self.current_db,
            _ => None,
        }
    }

    /// Put the selection on the database card's name row, when it shows.
    pub fn select_database_card(&mut self) {
        if let Some(n) = self
            .lines()
            .iter()
            .position(|l| *l == Line::DbCard(DbCard::Name))
        {
            self.selected = n;
        }
    }

    /// The databases the picker offers, under the selected language: each
    /// one's store index and its row, the current one marked.
    pub fn database_choices(&self) -> Vec<(usize, String)> {
        self.databases
            .iter()
            .enumerate()
            .filter(|(_, db)| self.db_matches_language(db))
            .map(|(i, db)| {
                let mark = if self.current_db == Some(i) {
                    "\u{25cf}"
                } else {
                    "\u{25cb}"
                };
                let lang = db
                    .language
                    .as_deref()
                    .map(|l| crate::codeql_db::language_label(l).unwrap_or(l))
                    .map(|l| format!(" \u{b7} {l}"))
                    .unwrap_or_default();
                (i, format!("{mark} {}{lang}", db.name))
            })
            .collect()
    }

    /// Expand or collapse query history entry `index`'s details, keeping
    /// the selection on its row.
    pub fn toggle_history(&mut self, index: usize) {
        let Some(output) = self.history.get(index).map(|r| r.output.clone()) else {
            return;
        };
        self.expanded = if self.expanded.as_ref() == Some(&output) {
            None
        } else {
            Some(output)
        };
        self.select_history(index);
    }

    /// Whether query history entry `index` shows its details.
    pub fn is_expanded(&self, index: usize) -> bool {
        self.expanded.is_some()
            && self.history.get(index).map(|r| &r.output) == self.expanded.as_ref()
    }

    /// Say which query runs and for how long. The Running section coming
    /// or going moves the rows under it, so the selection follows its row.
    pub fn set_running(&mut self, running: Option<(String, u64)>) {
        if running.is_some() == self.running.is_some() {
            self.running = running;
            return;
        }
        let hit = self.selected_hit();
        self.running = running;
        if let Some(hit) = hit
            && let Some(n) = self
                .lines()
                .iter()
                .position(|l| Self::selectable(l) == Some(hit))
        {
            self.selected = n;
        }
    }

    /// The Model Editor endpoint whose row is selected.
    pub fn selected_model_endpoint(&self) -> Option<usize> {
        match self.selected_hit() {
            Some(Hit::Action(Action::ModelEndpoint(i))) => Some(i),
            _ => None,
        }
    }

    /// Select the Evaluator Log Viewer row of predicate `i`.
    pub fn select_evallog_predicate(&mut self, i: usize) {
        let row = self.lines().iter().position(|l| {
            matches!(l, Line::Action(Action::EvalPredicate(p), t) if *p == i && !t.starts_with(' '))
        });
        if let Some(row) = row {
            self.selected = row;
        }
    }

    /// The store index of the query history entry whose row is selected.
    pub fn selected_history(&self) -> Option<usize> {
        match self.selected_hit() {
            Some(Hit::Action(Action::OpenHistory(i))) => Some(i),
            _ => None,
        }
    }

    /// Put the selection on history entry `index`'s row, when it shows.
    pub fn select_history(&mut self, index: usize) {
        if let Some(n) = self.lines().iter().position(|l| *l == Line::History(index)) {
            self.selected = n;
        }
    }

    /// Fold or unfold a query pack, keeping the selection on its line.
    pub fn toggle_pack(&mut self, pack: usize) {
        let Some(dir) = self.queries.get(pack).map(|p| p.dir.clone()) else {
            return;
        };
        if !self.folded_packs.remove(&dir) {
            self.folded_packs.insert(dir);
        }
        if let Some(n) = self
            .lines()
            .iter()
            .position(|l| matches!(l, Line::Action(Action::TogglePack(p), _) if *p == pack))
        {
            self.selected = n;
        }
    }

    /// Take a finished query discovery (#840). It lands after the view has
    /// opened, and the rows below the Queries section move with it, so the
    /// selection stays on the row it was on.
    pub fn set_queries(&mut self, queries: Vec<crate::codeql_query::QueryPack>) {
        let hit = self.selected_hit();
        self.queries = queries;
        self.discovering_queries = false;
        if let Some(n) = hit.and_then(|hit| {
            self.lines()
                .iter()
                .position(|l| Self::selectable(l) == Some(hit))
        }) {
            self.selected = n;
        }
    }

    /// The index into `queries` of the pack whose line, or one of whose
    /// query rows, is selected.
    pub fn selected_pack(&self) -> Option<usize> {
        match self.selected_hit() {
            Some(Hit::Action(Action::TogglePack(p) | Action::RunQuery(p, _))) => Some(p),
            _ => None,
        }
    }

    /// The variant analysis entry whose row is selected.
    pub fn selected_variant_item(&self) -> Option<crate::codeql_variant::Item> {
        use crate::codeql_variant::Item;
        match self.selected_hit() {
            Some(Hit::Action(Action::VariantList(i))) => Some(Item::List(i)),
            Some(Hit::Action(Action::VariantRepo(l, j))) => Some(Item::Repo(l, j)),
            Some(Hit::Action(Action::VariantOwner(i))) => Some(Item::Owner(i)),
            _ => None,
        }
    }

    /// The remembered variant analysis whose row is selected.
    pub fn selected_variant_run(&self) -> Option<usize> {
        match self.selected_hit() {
            Some(Hit::Action(Action::VariantRun(i))) => Some(i),
            _ => None,
        }
    }

    /// The list whose line, or one of whose repositories, is selected: where
    /// an added repository goes.
    pub fn selected_variant_list(&self) -> Option<usize> {
        use crate::codeql_variant::Item;
        match self.selected_variant_item() {
            Some(Item::List(i) | Item::Repo(Some(i), _)) => Some(i),
            _ => None,
        }
    }

    /// Put the selection on `item`'s row, when it shows.
    pub fn select_variant_item(&mut self, item: crate::codeql_variant::Item) {
        use crate::codeql_variant::Item;
        let action = match item {
            Item::List(i) => Action::VariantList(i),
            Item::Repo(l, j) => Action::VariantRepo(l, j),
            Item::Owner(i) => Action::VariantOwner(i),
        };
        self.select_action(action);
    }

    /// Put the selection on `action`'s row, when it shows.
    pub fn select_action(&mut self, action: Action) {
        if let Some(n) = self.lines().iter().position(
            |l| matches!(l, Line::Action(a, _) | Line::Button(a, ButtonRow::Label) if *a == action),
        ) {
            self.selected = n;
        }
    }

    /// Fold or unfold variant analysis list `list`, keeping the selection
    /// on its line.
    pub fn toggle_variant_list(&mut self, list: usize) {
        let Some(name) = self.variant.lists.get(list).map(|l| l.name.clone()) else {
            return;
        };
        if !self.folded_lists.remove(&name) {
            self.folded_lists.insert(name);
        }
        self.select_variant_item(crate::codeql_variant::Item::List(list));
    }

    /// The section the selected row is in.
    pub fn selected_section(&self) -> Option<Section> {
        let lines = self.lines();
        let end = self.selected.min(lines.len().saturating_sub(1));
        lines[..=end].iter().rev().find_map(|l| match l {
            Line::Header(s) => Some(*s),
            _ => None,
        })
    }

    /// The Variant Analysis Repositories section's lines under its header.
    fn variant_lines(&self, out: &mut Vec<Line>) {
        use crate::codeql_variant::Item;
        if self.variant_error {
            out.push(Line::Text("The config file can't be read."));
            out.push(Line::Action(
                Action::OpenVariantConfig,
                "Open config file".to_string(),
            ));
            return;
        }
        let v = &self.variant;
        match &v.controller_repo {
            None => {
                out.push(Line::Text("No controller repository yet."));
                out.push(Line::Action(
                    Action::SetUpControllerRepository,
                    "Set up controller repository".to_string(),
                ));
            }
            Some(c) => out.push(Line::Action(
                Action::SetUpControllerRepository,
                format!("Controller: {c}"),
            )),
        }
        let mark = |item| if v.is_selected(item) { "●" } else { "○" };
        for (i, list) in v.lists.iter().enumerate() {
            let folded = self.folded_lists.contains(&list.name);
            let chevron = if folded {
                crate::icons::CHEVRON_CLOSED
            } else {
                crate::icons::CHEVRON_OPEN
            };
            out.push(Line::Action(
                Action::VariantList(i),
                format!(
                    "{chevron} {} {} ({})",
                    mark(Item::List(i)),
                    list.name,
                    list.repos.len()
                ),
            ));
            if folded {
                continue;
            }
            for (j, nwo) in list.repos.iter().enumerate() {
                out.push(Line::Action(
                    Action::VariantRepo(Some(i), j),
                    format!("    {} {nwo}", mark(Item::Repo(Some(i), j))),
                ));
            }
        }
        for (j, nwo) in v.repos.iter().enumerate() {
            out.push(Line::Action(
                Action::VariantRepo(None, j),
                format!("{} {nwo}", mark(Item::Repo(None, j))),
            ));
        }
        for (i, owner) in v.owners.iter().enumerate() {
            out.push(Line::Action(
                Action::VariantOwner(i),
                format!("{} {owner} (owner)", mark(Item::Owner(i))),
            ));
        }
        if v.controller_repo.is_some() {
            for (a, label) in [
                (Action::AddVariantRepo, "Add repository"),
                (Action::AddVariantList, "Add repository list"),
                (Action::AddVariantOwner, "Add owner"),
            ] {
                out.push(Line::Action(a, label.to_string()));
            }
        }
        if !self.variant_runs.is_empty() {
            out.push(Line::Text("Runs (newest first):"));
            for (i, label) in self.variant_runs.iter().enumerate().rev() {
                out.push(Line::Action(Action::VariantRun(i), label.clone()));
            }
        }
    }

    /// The width the rows paint in: the last frame's, or the mockup's 32
    /// before the first frame.
    fn row_width(&self) -> usize {
        match self.last_inner.width {
            0 => 32,
            w => w as usize,
        }
    }

    /// Whether query history entry `i` is the run the Running section
    /// shows, which the history leaves to it.
    fn in_running(&self, i: usize) -> bool {
        self.running.is_some()
            && self
                .history
                .get(i)
                .is_some_and(|r| r.status == crate::codeql_query::RunStatus::Running)
    }

    /// The current database, when there is one.
    fn current(&self) -> Option<&crate::codeql_db::DbEntry> {
        self.current_db.and_then(|i| self.databases.get(i))
    }

    /// The chips under expanded history entry `i`, a line of them at a
    /// time as they fit between the tree's column and the frame.
    fn history_chip_lines(&self, i: usize) -> Vec<Vec<Chip>> {
        let limit = self.row_width().saturating_sub(1) as u16;
        let mut lines: Vec<Vec<Chip>> = Vec::new();
        let mut x = HISTORY_CHIP_X;
        for chip in history_chips(i) {
            let w = chip.label.chars().count() as u16 + 2;
            // Too wide for a line of its own: `lay_out_chips` would drop
            // it, so it never claims a row that paints empty.
            if HISTORY_CHIP_X + w > limit {
                continue;
            }
            if x > HISTORY_CHIP_X && x + w > limit {
                x = HISTORY_CHIP_X;
            }
            if x == HISTORY_CHIP_X {
                lines.push(Vec::new());
            }
            if let Some(line) = lines.last_mut() {
                line.push(chip);
            }
            x += w + 1;
        }
        lines
    }

    /// The chips `line` shows, placed: each one's column from the row's
    /// start, its width, and whether its key shows.
    fn chips_on(&self, line: &Line) -> Vec<(u16, u16, Chip, bool)> {
        let w = self.row_width() as u16;
        match line {
            // Inside the card, whose edges are its second and last but one
            // columns.
            Line::DbCard(DbCard::Chips) => lay_out_chips(&DB_CHIPS, 3, w.saturating_sub(2)),
            Line::HistoryDetail(i, Detail::Chips(k)) => self
                .history_chip_lines(*i)
                .get(*k)
                .map(|chips| lay_out_chips(chips, HISTORY_CHIP_X, w.saturating_sub(1)))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// The first-run card, shown while there is no database, and the Quick
    /// Query button under it.
    fn welcome_lines(&self, out: &mut Vec<Line>) {
        let w = self.row_width();
        // The card runs from column 1 to the one before the last; its text
        // keeps a column clear of each border.
        let text_w = w.saturating_sub(6).max(1);
        out.push(Line::Blank);
        out.push(Line::Card(Card::Top));
        if text_w + 2 >= ART_W as usize {
            for row in 0..ART.len() {
                out.push(Line::Card(Card::Art(row)));
            }
            out.push(Line::Card(Card::Blank));
        }
        for line in wrap("Query your code for bugs", text_w) {
            out.push(Line::Card(Card::Heading(line)));
        }
        out.push(Line::Card(Card::Blank));
        let body = "Build a database from your code, then ask it questions in QL.";
        for line in wrap(body, text_w) {
            out.push(Line::Card(Card::Body(line)));
        }
        out.push(Line::Card(Card::Blank));
        for step in 0..STEPS.len() {
            out.push(Line::Card(Card::Step(step)));
        }
        out.push(Line::Card(Card::Blank));
        for row in [ButtonRow::Top, ButtonRow::Label, ButtonRow::Bottom] {
            out.push(Line::Button(Action::AddDatabase, row));
        }
        for line in wrap("from a folder, archive, URL or GitHub", text_w) {
            out.push(Line::Card(Card::Caption(line)));
        }
        out.push(Line::Card(Card::Bottom));
        out.push(Line::Blank);
        for row in [ButtonRow::Top, ButtonRow::Label, ButtonRow::Bottom] {
            out.push(Line::Button(Action::QuickQuery, row));
        }
        out.push(Line::Blank);
    }

    /// Whether `s` has a header at all. Databases shows once there is one
    /// (the card stands in before); the three data views show once they
    /// hold something, and Tools while any of them does not.
    fn shows(&self, s: Section) -> bool {
        match s {
            Section::Databases => !self.databases.is_empty(),
            Section::Running => self.running.is_some(),
            Section::AstViewer => self.ast.is_some(),
            Section::EvaluatorLog => self.evallog.is_some(),
            Section::MethodModeling => self.model.is_some(),
            Section::Tools => !self.idle_tools().is_empty(),
            _ => true,
        }
    }

    /// The tools with nothing to show, which the Tools row offers.
    fn idle_tools(&self) -> Vec<(Action, &'static str)> {
        let mut out = Vec::new();
        if self.ast.is_none() {
            out.push((Action::ViewAst, "View AST"));
        }
        if self.evallog.is_none() {
            out.push((Action::ShowEvaluatorLog, "Evaluator Log"));
        }
        if self.model.is_none() {
            out.push((Action::OpenModelEditor, "Model Editor"));
        }
        out
    }

    /// Whether `s` has nothing to list yet. Queries being discovered have
    /// their "Discovering queries…" line (#840), so the section is not folded
    /// away under a "none yet" it may not be.
    fn is_empty(&self, s: Section) -> bool {
        let v = &self.variant;
        match s {
            Section::Queries => {
                !self.discovering_queries
                    && !self.queries.iter().any(|p| self.pack_matches_language(p))
            }
            Section::QueryHistory => self.history.is_empty(),
            Section::VariantAnalysis => {
                !self.variant_error
                    && v.controller_repo.is_none()
                    && v.lists.is_empty()
                    && v.repos.is_empty()
                    && v.owners.is_empty()
                    && self.variant_runs.is_empty()
            }
            _ => false,
        }
    }

    /// An empty section, and Tools, start folded and stay so until opened;
    /// the rest fold when the user folds them.
    fn starts_folded(&self, s: Section) -> bool {
        s == Section::Tools || self.is_empty(s)
    }

    /// Whether `s` shows only its header.
    pub fn folded(&self, s: Section) -> bool {
        if self.starts_folded(s) {
            !self.opened.contains(&s)
        } else {
            self.collapsed.contains(&s)
        }
    }

    /// The count on `s`'s header, when it lists anything.
    fn count(&self, s: Section) -> Option<usize> {
        let n = match s {
            Section::Databases => self.databases.len(),
            Section::Queries => self
                .queries
                .iter()
                .filter(|p| self.pack_matches_language(p))
                .map(|p| p.queries.len())
                .sum(),
            Section::QueryHistory => (0..self.history.len())
                .filter(|&i| !self.in_running(i))
                .count(),
            Section::VariantAnalysis => {
                let v = &self.variant;
                v.lists.iter().map(|l| l.repos.len()).sum::<usize>() + v.repos.len()
            }
            _ => 0,
        };
        (n > 0).then_some(n)
    }

    /// The dim note on `s`'s header in place of a count.
    fn summary(&self, s: Section) -> Option<String> {
        match s {
            Section::Tools => Some(
                self.idle_tools()
                    .iter()
                    .map(|(a, _)| match a {
                        Action::ViewAst => "AST",
                        Action::ShowEvaluatorLog => "Log",
                        _ => "Model",
                    })
                    .collect::<Vec<_>>()
                    .join(" \u{b7} "),
            ),
            // The key that cancels it.
            Section::Running => Some("esc".to_string()),
            Section::VariantAnalysis if self.is_empty(s) => Some("set up".to_string()),
            Section::Queries | Section::QueryHistory if self.is_empty(s) => {
                Some("none yet".to_string())
            }
            _ => None,
        }
    }

    /// Every line the panel paints, top to bottom.
    pub fn lines(&self) -> Vec<Line> {
        let mut out = Vec::new();
        if self.databases.is_empty() {
            self.welcome_lines(&mut out);
        }
        for s in Section::ALL {
            if !self.shows(s) {
                continue;
            }
            out.push(Line::Header(s));
            if self.folded(s) {
                continue;
            }
            match s {
                Section::Language => {
                    for (i, name) in LANGUAGES.iter().enumerate() {
                        let mark = if self.language == Some(i) {
                            "●"
                        } else {
                            "○"
                        };
                        out.push(Line::Action(
                            Action::SelectLanguage(i),
                            format!("{mark} {name}"),
                        ));
                    }
                }
                Section::Databases => {
                    // The current database's card stands in for the list,
                    // which its name row offers in a picker.
                    out.push(Line::DbCard(DbCard::Top));
                    out.push(Line::DbCard(DbCard::Name));
                    if self.current().is_some() {
                        out.push(Line::DbCard(DbCard::Meta));
                    }
                    out.push(Line::DbCard(DbCard::Blank));
                    out.push(Line::DbCard(DbCard::Chips));
                    out.push(Line::DbCard(DbCard::Bottom));
                    out.push(Line::Action(
                        Action::AddDatabase,
                        "Add Database".to_string(),
                    ));
                    if self.databases.len() > 1 {
                        let by = self.db_sort.map_or("date added", |b| b.label());
                        out.push(Line::Action(
                            Action::SortDatabases,
                            format!("Sort by: {by}"),
                        ));
                    }
                }
                Section::Queries if self.queries.iter().any(|p| self.pack_matches_language(p)) => {
                    for (i, pack) in self.queries.iter().enumerate() {
                        if !self.pack_matches_language(pack) {
                            continue;
                        }
                        let folded = self.folded_packs.contains(&pack.dir);
                        let chevron = if folded {
                            crate::icons::CHEVRON_CLOSED
                        } else {
                            crate::icons::CHEVRON_OPEN
                        };
                        let lang = pack
                            .language
                            .as_deref()
                            .map(|l| format!(" ({l})"))
                            .unwrap_or_default();
                        out.push(Line::Action(
                            Action::TogglePack(i),
                            format!("{chevron} {}{lang}", pack.name),
                        ));
                        if folded {
                            continue;
                        }
                        for (j, q) in pack.queries.iter().enumerate() {
                            let rel = q.strip_prefix(&pack.dir).unwrap_or(q);
                            out.push(Line::Action(
                                Action::RunQuery(i, j),
                                format!("  {}", rel.display()),
                            ));
                        }
                    }
                }
                Section::Queries if self.discovering_queries => {
                    out.push(Line::Text("Discovering queries\u{2026}"));
                }
                Section::Queries => {
                    out.push(Line::Text("No queries in this workspace."));
                    out.push(Line::Action(
                        Action::CreateQuery,
                        "Create a query".to_string(),
                    ));
                }
                Section::VariantAnalysis => self.variant_lines(&mut out),
                Section::QueryHistory if !self.history.is_empty() => {
                    out.push(Line::Action(
                        Action::SortHistory,
                        format!("Sort by: {}", self.history_sort.label()),
                    ));
                    for i in 0..self.history.len() {
                        if self.in_running(i) {
                            continue;
                        }
                        out.push(Line::History(i));
                        if self.is_expanded(i) {
                            out.push(Line::HistoryDetail(i, Detail::Summary));
                            out.push(Line::HistoryDetail(i, Detail::Where));
                            for k in 0..self.history_chip_lines(i).len() {
                                out.push(Line::HistoryDetail(i, Detail::Chips(k)));
                            }
                        }
                    }
                }
                Section::Running => {
                    out.push(Line::Running(RunRow::Name));
                    out.push(Line::Running(RunRow::Progress));
                }
                Section::QueryHistory => {
                    out.push(Line::Text("Run a query to see results."));
                }
                Section::AstViewer => {
                    // Shown only once it holds one; until then Tools offers it.
                    let Some(view) = &self.ast else {
                        continue;
                    };
                    let name = view
                        .file
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    out.push(Line::Action(
                        Action::ClearAst,
                        format!("Clear \u{b7} {name} in {}", view.database),
                    ));
                    for (depth, i) in view.visible() {
                        let node = &view.tree.nodes[i];
                        let mark = match (node.children.is_empty(), view.open.contains(&i)) {
                            (true, _) => ' ',
                            (false, true) => '\u{25be}',
                            (false, false) => '\u{25b8}',
                        };
                        let at = node
                            .location
                            .as_ref()
                            .map(|l| format!("  {}:{}", l.line, l.column))
                            .unwrap_or_default();
                        out.push(Line::Action(
                            Action::AstNode(i),
                            format!("{}{mark} {}{at}", "  ".repeat(depth), node.label),
                        ));
                    }
                }
                Section::EvaluatorLog => {
                    let Some(view) = &self.evallog else {
                        continue;
                    };
                    out.push(Line::Action(
                        Action::ClearEvalLog,
                        format!("Clear \u{b7} {}", view.query),
                    ));
                    use crate::codeql_evallog::LogRow;
                    for row in view.rows() {
                        out.push(match row {
                            LogRow::Predicate(i, text) | LogRow::Detail(i, text) => {
                                Line::Action(Action::EvalPredicate(i), text)
                            }
                            LogRow::Dependency(_, target, text) => {
                                Line::Action(Action::EvalDependency(target), text)
                            }
                        });
                    }
                }
                Section::MethodModeling => {
                    let Some(view) = &self.model else {
                        continue;
                    };
                    out.push(Line::Action(
                        Action::OpenModelEditor,
                        format!("Refresh \u{b7} {} ({})", view.database, view.language),
                    ));
                    use crate::codeql_model::ModelRow;
                    let rows = view.rows();
                    for (at, row) in rows.iter().enumerate() {
                        out.push(match row {
                            ModelRow::Group(group, modeled, total) => {
                                let mark = if view.folded.contains(group) {
                                    '\u{25b8}'
                                } else {
                                    '\u{25be}'
                                };
                                // A group's endpoints follow it; one is
                                // enough to name the group.
                                let first = rows[at..].iter().find_map(|r| match r {
                                    ModelRow::Endpoint(i, _) => Some(*i),
                                    _ => None,
                                });
                                let first = first.unwrap_or_else(|| {
                                    view.endpoints
                                        .iter()
                                        .position(|e| e.group() == *group)
                                        .unwrap_or(0)
                                });
                                Line::Action(
                                    Action::ModelGroup(first),
                                    format!("{mark} {group}  {modeled}/{total} modeled"),
                                )
                            }
                            ModelRow::Endpoint(i, text) => {
                                Line::Action(Action::ModelEndpoint(*i), text.clone())
                            }
                        });
                    }
                }
                Section::Tools => {
                    for (a, label) in self.idle_tools() {
                        out.push(Line::Action(a, label.to_string()));
                    }
                }
            }
        }
        out
    }

    fn selectable(line: &Line) -> Option<Hit> {
        match line {
            Line::Header(s) => Some(Hit::Header(*s)),
            Line::Action(a, _) | Line::Button(a, ButtonRow::Label) => Some(Hit::Action(*a)),
            Line::DbCard(DbCard::Name) => Some(Hit::Action(Action::PickDatabase)),
            Line::History(i) => Some(Hit::Action(Action::OpenHistory(*i))),
            _ => None,
        }
    }

    /// Move the selection to the next (or previous) header or action.
    pub fn move_selection(&mut self, down: bool) {
        let lines = self.lines();
        let mut i = self.selected.min(lines.len().saturating_sub(1));
        loop {
            let next = if down {
                i.checked_add(1)
            } else {
                i.checked_sub(1)
            };
            match next {
                Some(n) if n < lines.len() => {
                    i = n;
                    if Self::selectable(&lines[n]).is_some() {
                        self.selected = n;
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    /// Keep the selection on a row that can be selected: the first-run card
    /// puts drawn rows above its button, where a fresh panel's selection
    /// would otherwise sit.
    fn settle_selection(&mut self, lines: &[Line]) {
        let at = self.selected.min(lines.len().saturating_sub(1));
        if lines.get(at).and_then(Self::selectable).is_some() {
            self.selected = at;
            return;
        }
        let below = (at..lines.len()).find(|&i| Self::selectable(&lines[i]).is_some());
        let above = (0..at)
            .rev()
            .find(|&i| Self::selectable(&lines[i]).is_some());
        if let Some(i) = below.or(above) {
            self.selected = i;
        }
    }

    pub fn selected_hit(&self) -> Option<Hit> {
        self.lines().get(self.selected).and_then(Self::selectable)
    }

    /// Fold or unfold a section, keeping the selection on its header.
    pub fn toggle(&mut self, section: Section) {
        let set = if self.starts_folded(section) {
            &mut self.opened
        } else {
            &mut self.collapsed
        };
        if !set.remove(&section) {
            set.insert(section);
        }
        if let Some(n) = self
            .lines()
            .iter()
            .position(|l| *l == Line::Header(section))
        {
            self.selected = n;
        }
    }

    /// The row under a screen position, from the last painted frame. A
    /// button's edges count as the button. On a row of chips, the chip
    /// under `x`, with the row it belongs to (the card's name row, or the
    /// history entry's) as the one to select.
    pub fn hit_at(&self, x: u16, y: u16) -> Option<(usize, Hit)> {
        let a = self.last_inner;
        if a.height == 0 || y < a.y || y >= a.y + a.height {
            return None;
        }
        let idx = self.scroll + (y - a.y) as usize;
        let lines = self.lines();
        let line = lines.get(idx)?;
        if matches!(
            line,
            Line::DbCard(DbCard::Chips) | Line::HistoryDetail(_, Detail::Chips(_))
        ) {
            let dx = x.checked_sub(a.x)?;
            let (_, _, chip, _) = self
                .chips_on(line)
                .into_iter()
                .find(|&(cx, w, ..)| dx >= cx && dx < cx + w)?;
            let owner = (0..idx)
                .rev()
                .find(|&i| Self::selectable(&lines[i]).is_some())?;
            return Some((owner, Hit::Action(chip.action)));
        }
        let idx = match line {
            Line::Button(_, ButtonRow::Top) => idx + 1,
            Line::Button(_, ButtonRow::Bottom) => idx.checked_sub(1)?,
            _ => idx,
        };
        lines.get(idx).and_then(Self::selectable).map(|h| (idx, h))
    }

    pub fn scroll_down(&mut self, n: usize) {
        let max = self.lines().len().saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// The colours the pane paints with: the brand's card teal and button
    /// fill under the gradient themes, the theme's accent and button fill
    /// elsewhere, as Source Control's and Run and Debug's cards do.
    fn palette(&self) -> Palette {
        let t = self.theme;
        let brand = self.focus_gradient;
        let pick = |on: (u8, u8, u8), off: Color| {
            if brand {
                crate::gradient::rgb_color(on)
            } else {
                off
            }
        };
        Palette {
            fg: t.ui(Color::Rgb(0xcc, 0xcc, 0xcc)),
            bright: t.ui(Color::Rgb(0xff, 0xff, 0xff)),
            dim: t.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
            faint: t.ui(Color::Rgb(0x6c, 0x76, 0x86)),
            dots: t.ui(Color::Rgb(0x4b, 0x50, 0x5a)),
            accent: pick(crate::gradient::CARD_ACCENT, t.accent()),
            edge: pick(crate::gradient::INNER_ACCENT, t.accent()),
            lens: pick(crate::gradient::GRAD_TR, t.accent()),
            card: pick(
                crate::gradient::CARD_ACCENT,
                t.ui(Color::Rgb(0x60, 0x68, 0x78)),
            ),
            button: pick(crate::gradient::PRIMARY_BTN_BG, t.button()),
            badge: t.accent_chip_bg(),
            selection: t.selection(),
            ember: pick(crate::gradient::GRAD_TR, t.accent()),
            added: t.git_added(),
            deleted: t.git_deleted(),
        }
    }

    /// Paint one row of the first-run card inside the frame's row `row`.
    fn paint_card(&self, buf: &mut Buffer, row: Rect, card: &Card, p: &Palette) {
        let w = row.width;
        if w < 4 {
            return;
        }
        let (left, right) = (row.x + 1, row.x + w - 2);
        let border = Style::default().fg(p.card);
        let (l, r) = match card {
            Card::Top => ("╭", "╮"),
            Card::Bottom => ("╰", "╯"),
            _ => ("│", "│"),
        };
        buf.set_string(left, row.y, l, border);
        buf.set_string(right, row.y, r, border);
        if matches!(card, Card::Top | Card::Bottom) {
            for x in left + 1..right {
                buf.set_string(x, row.y, "─", border);
            }
            return;
        }
        let text_w = w.saturating_sub(4);
        let centred = |buf: &mut Buffer, text: &str, style: Style| {
            let n = (text.chars().count() as u16).min(text_w);
            let x = row.x + (w - n) / 2;
            buf.set_stringn(x, row.y, text, text_w as usize, style);
        };
        match card {
            Card::Art(i) => {
                // Centred as the mockup has it, a column right of true centre
                // when the width leaves an odd column.
                let x0 = row.x + (w - ART_W).div_ceil(2);
                for (dx, text, kind) in ART[*i] {
                    let style = match kind {
                        Art::Dots => Style::default().fg(p.dots),
                        Art::Node => Style::default().fg(p.accent),
                        Art::Edge => Style::default().fg(p.edge),
                        Art::Lens => Style::default().fg(p.lens),
                        Art::Label => Style::default().fg(p.bright).add_modifier(Modifier::BOLD),
                    };
                    buf.set_string(x0 + dx, row.y, text, style);
                }
            }
            Card::Heading(text) => centred(
                buf,
                text,
                Style::default().fg(p.bright).add_modifier(Modifier::BOLD),
            ),
            Card::Body(text) | Card::Caption(text) => {
                centred(buf, text, Style::default().fg(p.dim));
            }
            Card::Step(i) => {
                let (mark, mark_fg, fg) = if *i == 0 {
                    ("●", p.accent, p.bright)
                } else {
                    ("○", p.faint, p.dim)
                };
                // The checklist is centred as a block, its marks in a column.
                let widest = STEPS.iter().map(|s| s.chars().count()).max().unwrap_or(0);
                let x = row.x + (w.saturating_sub(widest as u16 + 2) / 2).max(3);
                if x + 3 < row.x + w - 2 {
                    buf.set_string(x, row.y, mark, Style::default().fg(mark_fg));
                    buf.set_stringn(
                        x + 2,
                        row.y,
                        STEPS[*i],
                        (row.x + w - 3 - (x + 2)) as usize,
                        Style::default().fg(fg),
                    );
                }
            }
            _ => {}
        }
    }

    /// Paint one row of the three-row button for `action`: the filled
    /// primary one inside the card, or the outlined one under it.
    fn paint_button(
        &self,
        buf: &mut Buffer,
        row: Rect,
        action: Action,
        part: ButtonRow,
        p: &Palette,
    ) {
        let w = row.width;
        let primary = action == Action::AddDatabase;
        let label = if primary {
            "+  Add Database"
        } else {
            "\u{bb} Try Quick Query"
        };
        if primary {
            // Inside the card, whose sides this row carries on.
            if w < 10 {
                return;
            }
            let border = Style::default().fg(p.card);
            buf.set_string(row.x + 1, row.y, "│", border);
            buf.set_string(row.x + w - 2, row.y, "│", border);
            let area = Rect::new(row.x + 3, row.y, w - 6, 1);
            // The label row is the shared rounded button's middle; the caps
            // are its fill with the rounded corners in the fill colour.
            crate::widgets::source_control::render_rounded_button(
                buf, area, label, p.button, p.bright,
            );
            if part != ButtonRow::Label {
                let (l, r) = if part == ButtonRow::Top {
                    ("╭", "╮")
                } else {
                    ("╰", "╯")
                };
                buf.set_string(area.x, row.y, " ", Style::default().bg(p.button));
                let corner = Style::default().fg(p.button).bg(Color::Reset);
                buf.set_string(area.x, row.y, l, corner);
                buf.set_string(area.x + area.width - 1, row.y, r, corner);
                for x in area.x + 1..area.x + area.width - 1 {
                    buf.set_string(x, row.y, " ", Style::default().bg(p.button));
                }
            }
            return;
        }
        if w < 6 {
            return;
        }
        let (left, right) = (row.x + 1, row.x + w - 2);
        let border = Style::default().fg(p.card);
        match part {
            ButtonRow::Top | ButtonRow::Bottom => {
                let (l, r) = if part == ButtonRow::Top {
                    ("╭", "╮")
                } else {
                    ("╰", "╯")
                };
                buf.set_string(left, row.y, l, border);
                buf.set_string(right, row.y, r, border);
                for x in left + 1..right {
                    buf.set_string(x, row.y, "─", border);
                }
            }
            ButtonRow::Label => {
                buf.set_string(left, row.y, "│", border);
                buf.set_string(right, row.y, "│", border);
                let inner_w = right - left - 1;
                let n = (label.chars().count() as u16).min(inner_w);
                buf.set_stringn(
                    left + 1 + (inner_w - n) / 2,
                    row.y,
                    label,
                    inner_w as usize,
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                );
            }
        }
    }

    /// Paint a section header: chevron, label, and the count or summary on
    /// the right.
    fn paint_header(&self, buf: &mut Buffer, row: Rect, s: Section, p: &Palette) {
        let w = row.width;
        let chevron = if self.folded(s) {
            crate::icons::CHEVRON_CLOSED
        } else {
            crate::icons::CHEVRON_OPEN
        };
        let label = match s {
            Section::Language => format!(
                "{} \u{b7} {}",
                s.title(),
                self.language.map_or("All", |i| LANGUAGES[i])
            ),
            _ => s.title().to_string(),
        };
        let end = row.x + w.saturating_sub(1);
        let mut room = end.saturating_sub(row.x + 3);
        if let Some(n) = self.count(s) {
            let badge = format!(" {n} ");
            let bw = badge.chars().count() as u16;
            if bw + 8 < w {
                let x = end - bw;
                buf.set_string(x, row.y, &badge, Style::default().fg(p.bright).bg(p.badge));
                room = x.saturating_sub(row.x + 4);
            }
        } else if let Some(note) = self.summary(s) {
            let nw = note.chars().count() as u16;
            if nw + label.chars().count() as u16 + 5 < w {
                let x = end - nw;
                buf.set_string(x, row.y, &note, Style::default().fg(p.faint));
                room = x.saturating_sub(row.x + 4);
            }
        }
        buf.set_string(
            row.x + 1,
            row.y,
            chevron.to_string(),
            Style::default().fg(p.faint),
        );
        buf.set_stringn(
            row.x + 3,
            row.y,
            &label,
            room as usize,
            Style::default().fg(p.dim).add_modifier(Modifier::BOLD),
        );
    }

    /// Paint the chips `line` shows on `row`: the label in the accent on
    /// the chip fill, its key after it in bold.
    fn paint_chips(&self, buf: &mut Buffer, row: Rect, line: &Line, p: &Palette) {
        for (dx, w, chip, key) in self.chips_on(line) {
            let x = row.x + dx;
            let fill = Style::default().bg(p.badge);
            buf.set_string(x, row.y, " ".repeat(w as usize), fill);
            buf.set_string(x + 1, row.y, chip.label, fill.fg(p.accent));
            if let (true, Some(k)) = (key, chip.key) {
                buf.set_string(
                    x + w - 2,
                    row.y,
                    k.to_string(),
                    fill.fg(p.bright).add_modifier(Modifier::BOLD),
                );
            }
        }
    }

    /// Paint one row of the current database's card: its titled edge, the
    /// language mark and name with the switch chevron, the language, size
    /// and age, and the chips.
    fn paint_db_card(&self, buf: &mut Buffer, row: Rect, card: DbCard, p: &Palette) {
        let w = row.width;
        if w < 8 {
            return;
        }
        let (left, right) = (row.x + 1, row.x + w - 2);
        let border = Style::default().fg(p.card);
        let (l, r) = match card {
            DbCard::Top => ("╭", "╮"),
            DbCard::Bottom => ("╰", "╯"),
            _ => ("│", "│"),
        };
        buf.set_string(left, row.y, l, border);
        buf.set_string(right, row.y, r, border);
        let db = self.current();
        match card {
            DbCard::Top | DbCard::Bottom => {
                for x in left + 1..right {
                    buf.set_string(x, row.y, "─", border);
                }
                if card == DbCard::Top {
                    buf.set_stringn(
                        left + 2,
                        row.y,
                        " DATABASE ",
                        right.saturating_sub(left + 3) as usize,
                        Style::default().fg(p.bright).add_modifier(Modifier::BOLD),
                    );
                }
            }
            DbCard::Name => {
                let chevron = right - 2;
                buf.set_string(chevron, row.y, "▾", Style::default().fg(p.dim));
                let room = chevron.saturating_sub(left + 5) as usize;
                match db {
                    Some(db) => {
                        let mark = db
                            .language
                            .as_deref()
                            .and_then(language_color)
                            .map_or(p.dim, |c| self.theme.ui(c));
                        buf.set_string(left + 2, row.y, "◆", Style::default().fg(mark));
                        buf.set_stringn(
                            left + 4,
                            row.y,
                            &db.name,
                            room,
                            Style::default().fg(p.bright).add_modifier(Modifier::BOLD),
                        );
                    }
                    None => {
                        buf.set_stringn(
                            left + 2,
                            row.y,
                            "Select a database",
                            room + 2,
                            Style::default().fg(p.dim),
                        );
                    }
                }
            }
            DbCard::Meta => {
                let Some(db) = db else {
                    return;
                };
                let mut parts = Vec::new();
                if let Some(lang) = db.language.as_deref() {
                    parts.push(
                        crate::codeql_db::language_label(lang)
                            .unwrap_or(lang)
                            .to_string(),
                    );
                }
                if let Some(&bytes) = self.db_sizes.get(&db.path) {
                    parts.push(short_size(bytes));
                }
                if db.added > 0 {
                    parts.push(short_age(now().saturating_sub(db.added)));
                }
                // Whole parts only: a narrow card drops the age, then the
                // size, rather than cutting "3d ago" to "3d a".
                let room = right.saturating_sub(left + 5) as usize;
                while parts.len() > 1 && parts.join(" \u{b7} ").chars().count() > room {
                    parts.pop();
                }
                buf.set_stringn(
                    left + 4,
                    row.y,
                    parts.join(" \u{b7} "),
                    room,
                    Style::default().fg(p.dim),
                );
            }
            DbCard::Blank => {}
            DbCard::Chips => self.paint_chips(buf, row, &Line::DbCard(card), p),
        }
    }

    /// Paint query history entry `i`: its status glyph, its name cut to
    /// fit, and the count and duration columns, right-aligned in the
    /// widths the frame measured so every row lines up.
    fn paint_history(
        &self,
        buf: &mut Buffer,
        row: Rect,
        i: usize,
        (count_w, dur_w): (u16, u16),
        p: &Palette,
    ) {
        use crate::codeql_query::RunStatus;
        let Some(h) = self.history.get(i) else {
            return;
        };
        let end = row.x + row.width.saturating_sub(1);
        let (glyph, glyph_fg, name_fg, dur_fg) = match h.status {
            RunStatus::Succeeded => ("✓", p.added, p.fg, p.dim),
            RunStatus::Failed(_) => ("✗", p.deleted, p.fg, p.dim),
            RunStatus::Running => ("◌", p.ember, p.bright, p.dim),
            RunStatus::Cancelled => ("−", p.dim, p.dim, p.faint),
        };
        let name_x = row.x + 4;
        let mut room = end.saturating_sub(name_x);
        // The columns go when the row is too narrow for a name beside them.
        let cols = count_w + dur_w + 3;
        if end >= name_x + cols + 6 {
            let dur = h.duration_text();
            let dur_x = end - dur.chars().count() as u16;
            buf.set_string(dur_x, row.y, &dur, Style::default().fg(dur_fg));
            let count = h.count_text();
            let count_end = end - dur_w - 2;
            let count_style = match (&h.status, h.results) {
                (RunStatus::Failed(_), _) => Style::default().fg(p.deleted),
                (RunStatus::Succeeded, Some(_)) => {
                    Style::default().fg(p.bright).add_modifier(Modifier::BOLD)
                }
                _ => Style::default().fg(p.faint),
            };
            buf.set_string(
                count_end - count.chars().count() as u16,
                row.y,
                &count,
                count_style,
            );
            room = (count_end - count_w).saturating_sub(name_x + 1);
        }
        buf.set_string(row.x + 2, row.y, glyph, Style::default().fg(glyph_fg));
        let name = if h.name.chars().count() > room as usize {
            let cut: String = h
                .name
                .chars()
                .take((room as usize).saturating_sub(1))
                .collect();
            format!("{cut}…")
        } else {
            h.name.clone()
        };
        let style = Style::default().fg(name_fg);
        let style = if h.status == RunStatus::Running {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        };
        buf.set_stringn(name_x, row.y, &name, room as usize, style);
    }

    /// Paint a detail line under expanded history entry `i`, on the tree
    /// line that joins them to its row.
    fn paint_detail(&self, buf: &mut Buffer, row: Rect, i: usize, detail: Detail, p: &Palette) {
        use crate::codeql_query::RunStatus;
        let Some(h) = self.history.get(i) else {
            return;
        };
        let end = row.x + row.width.saturating_sub(1);
        let (x, text_x) = (row.x + 4, row.x + HISTORY_CHIP_X);
        if text_x + 2 >= end {
            return;
        }
        let branch = match detail {
            Detail::Chips(0) => "└",
            Detail::Chips(_) => "",
            _ => "├",
        };
        buf.set_string(x, row.y, branch, Style::default().fg(p.faint));
        let room = |from: u16| end.saturating_sub(from) as usize;
        match detail {
            Detail::Summary => {
                let format = if h.is_sarif() { "SARIF" } else { "CSV" };
                let mut room = room(text_x);
                if format.len() + 12 < room {
                    buf.set_string(
                        end - format.len() as u16,
                        row.y,
                        format,
                        Style::default().fg(p.dim),
                    );
                    room -= format.len() + 1;
                }
                let noun = match (h.is_sarif(), h.results) {
                    (true, Some(1)) => "alert",
                    (true, _) => "alerts",
                    (false, Some(1)) => "row",
                    (false, _) => "rows",
                };
                let (text, style) = match (&h.status, h.results) {
                    (RunStatus::Succeeded, Some(n)) => {
                        (format!("{n} {noun}"), Style::default().fg(p.fg))
                    }
                    (RunStatus::Succeeded, None) => {
                        (format!("{noun} not counted"), Style::default().fg(p.dim))
                    }
                    (RunStatus::Failed(why), _) => {
                        (format!("failed: {why}"), Style::default().fg(p.deleted))
                    }
                    (RunStatus::Running, _) => ("running".to_string(), Style::default().fg(p.dim)),
                    (RunStatus::Cancelled, _) => {
                        ("cancelled".to_string(), Style::default().fg(p.dim))
                    }
                };
                buf.set_stringn(text_x, row.y, text, room, style);
            }
            Detail::Where => {
                let text = format!("{} \u{b7} {}", h.database, clock(h.started, now()));
                buf.set_stringn(
                    text_x,
                    row.y,
                    text,
                    room(text_x),
                    Style::default().fg(p.dim),
                );
            }
            Detail::Chips(_) => {
                self.paint_chips(buf, row, &Line::HistoryDetail(i, detail), p);
            }
        }
    }

    /// Paint a row of the Running section: the query's name, or the sweep
    /// in the gradient with the time elapsed.
    fn paint_running(&self, buf: &mut Buffer, row: Rect, part: RunRow, p: &Palette) {
        let Some((name, secs)) = &self.running else {
            return;
        };
        let end = row.x + row.width.saturating_sub(1);
        match part {
            RunRow::Name => {
                buf.set_string(row.x + 2, row.y, "◌", Style::default().fg(p.ember));
                buf.set_stringn(
                    row.x + 4,
                    row.y,
                    name,
                    end.saturating_sub(row.x + 4) as usize,
                    Style::default().fg(p.bright).add_modifier(Modifier::BOLD),
                );
            }
            RunRow::Progress => {
                let time = format!("{}:{:02}", secs / 60, secs % 60);
                let time_x = end.saturating_sub(time.chars().count() as u16);
                let x0 = row.x + 4;
                if time_x < x0 + 6 {
                    return;
                }
                buf.set_string(time_x, row.y, &time, Style::default().fg(p.ember));
                let n = time_x - 2 - x0;
                let lit = sweep(n, *secs);
                for c in 0..n {
                    let (cell, fg) = if lit.contains(&c) {
                        let fg = if self.focus_gradient {
                            let t = if n > 1 {
                                f32::from(c) / f32::from(n - 1)
                            } else {
                                0.0
                            };
                            crate::gradient::rgb_color(crate::gradient::lerp_rgb(
                                crate::gradient::GRAD_TL,
                                crate::gradient::GRAD_TR,
                                t,
                            ))
                        } else {
                            p.accent
                        };
                        ("━", fg)
                    } else {
                        ("─", p.faint)
                    };
                    buf.set_string(x0 + c, row.y, cell, Style::default().fg(fg));
                }
            }
        }
    }

    /// Paint an action row: a label behind its glyph.
    fn paint_action(&self, buf: &mut Buffer, row: Rect, action: Action, label: &str, p: &Palette) {
        let w = row.width;
        let end = row.x + w.saturating_sub(1);
        let glyph = match action {
            Action::RunQuery(..) => Some("▶"),
            Action::AddDatabase
            | Action::CreateQuery
            | Action::AddVariantRepo
            | Action::AddVariantList
            | Action::AddVariantOwner => Some("+"),
            Action::AddDatabaseFromFolder
            | Action::AddDatabaseFromArchive
            | Action::AddDatabaseFromUrl
            | Action::AddDatabaseFromGithub
            | Action::SortDatabases
            | Action::SortHistory
            | Action::SetUpControllerRepository
            | Action::OpenVariantConfig
            | Action::ViewAst
            | Action::ShowEvaluatorLog
            | Action::OpenModelEditor
            | Action::ClearAst
            | Action::ClearEvalLog
            | Action::QuickQuery => Some("›"),
            _ => None,
        };
        let room = |x: u16| end.saturating_sub(x) as usize;
        match glyph {
            Some(g) => {
                let text = label.trim_start();
                let indent = (label.len() - text.len()) as u16;
                let x = row.x + 2 + indent;
                if x + 2 < end {
                    buf.set_string(x, row.y, g, Style::default().fg(p.accent));
                    buf.set_stringn(x + 2, row.y, text, room(x + 2), Style::default().fg(p.fg));
                }
            }
            None => {
                buf.set_stringn(
                    row.x + 2,
                    row.y,
                    label,
                    room(row.x + 2),
                    Style::default().fg(p.fg),
                );
            }
        }
    }
}

/// The colours one frame of the pane paints with.
struct Palette {
    fg: Color,
    bright: Color,
    dim: Color,
    faint: Color,
    dots: Color,
    accent: Color,
    edge: Color,
    lens: Color,
    card: Color,
    button: Color,
    badge: Color,
    selection: Color,
    /// The gradient's warm end: what runs, and for how long.
    ember: Color,
    /// Succeeded and failed, as Source Control colours added and deleted.
    added: Color,
    deleted: Color,
}

impl Widget for &mut CodeqlPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.last_inner = Rect::default();
        if area.height < 3 || area.width < 8 {
            return;
        }
        let theme = self.theme;
        // The Explorer's frame: the brand's chipless title and gradient
        // border when focused under the gradient themes, the legacy chip
        // and solid border elsewhere.
        let block_style = if self.focused {
            Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let title = if self.focus_gradient {
            Span::styled(
                " CODEQL ",
                Style::default()
                    .fg(crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                " CODEQL ",
                Style::default()
                    .fg(theme.ui(Color::White))
                    .bg(theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e)))
                    .add_modifier(Modifier::BOLD),
            )
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(title.clone());
        let inner = block.inner(area);
        block.render(area, buf);
        if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
            buf.set_span(area.x + 1, area.y, &title, title.width() as u16);
        }
        self.last_inner = inner;
        if inner.height == 0 || inner.width < 4 {
            return;
        }
        let p = self.palette();
        let lines = self.lines();
        self.settle_selection(&lines);
        let rows = inner.height as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        let focused_row = self.focused.then_some(self.selected);
        // The count and duration columns are as wide as their widest entry,
        // measured once a frame rather than once a row.
        let widest = |f: fn(&HistoryRow) -> String| {
            self.history
                .iter()
                .map(|r| f(r).chars().count() as u16)
                .max()
                .unwrap_or(0)
        };
        let columns = (
            widest(HistoryRow::count_text),
            widest(HistoryRow::duration_text),
        );
        for (r, line) in lines.iter().enumerate().skip(self.scroll).take(rows) {
            let y = inner.y + (r - self.scroll) as u16;
            let row = Rect::new(inner.x, y, inner.width, 1);
            // A button's three rows share its label row's selection.
            let selected = match line {
                Line::Button(_, ButtonRow::Top) => focused_row == Some(r + 1),
                Line::Button(_, ButtonRow::Bottom) => Some(r) == focused_row.map(|s| s + 1),
                _ => focused_row == Some(r),
            };
            if selected && matches!(line, Line::Header(_) | Line::Action(..) | Line::History(_)) {
                buf.set_style(row, Style::default().bg(p.selection));
            }
            // The card's name row fills inside the card's edges.
            if selected && *line == Line::DbCard(DbCard::Name) && row.width > 4 {
                let inside = Rect::new(row.x + 2, y, row.width - 4, 1);
                buf.set_style(inside, Style::default().bg(p.selection));
            }
            match line {
                Line::Header(s) => self.paint_header(buf, row, *s, &p),
                Line::Text(t) => {
                    buf.set_stringn(
                        inner.x + 2,
                        y,
                        t,
                        inner.width.saturating_sub(3) as usize,
                        Style::default().fg(p.dim),
                    );
                }
                Line::Action(a, label) => self.paint_action(buf, row, *a, label, &p),
                Line::Blank => {}
                Line::Card(card) => self.paint_card(buf, row, card, &p),
                Line::Button(a, part) => self.paint_button(buf, row, *a, *part, &p),
                Line::DbCard(card) => self.paint_db_card(buf, row, *card, &p),
                Line::History(i) => self.paint_history(buf, row, *i, columns, &p),
                Line::HistoryDetail(i, detail) => self.paint_detail(buf, row, *i, *detail, &p),
                Line::Running(part) => self.paint_running(buf, row, *part, &p),
            }
            // The Explorer-style accent bar marks the selected row.
            if selected {
                buf.set_string(inner.x, y, "▎", Style::default().fg(p.accent));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_is_listed_with_its_welcome() {
        let p = CodeqlPanel::new();
        assert!(p.collapsed.contains(&Section::Language), "starts folded");
        let lines = p.lines();
        // The first-run card stands in for Databases, and one Tools row for
        // the three data views while they are empty.
        for s in [
            Section::Language,
            Section::Queries,
            Section::VariantAnalysis,
            Section::QueryHistory,
            Section::Tools,
        ] {
            assert!(lines.contains(&Line::Header(s)), "{s:?}");
            assert!(p.folded(s), "{s:?} starts folded");
        }
        for s in [
            Section::Databases,
            Section::Running,
            Section::AstViewer,
            Section::EvaluatorLog,
            Section::MethodModeling,
        ] {
            assert!(!lines.contains(&Line::Header(s)), "{s:?}");
        }
        assert!(lines.contains(&Line::Button(Action::AddDatabase, ButtonRow::Label)));
        assert!(lines.contains(&Line::Button(Action::QuickQuery, ButtonRow::Label)));
        assert!(lines.contains(&Line::Card(Card::Heading(
            "Query your code for bugs".into()
        ))));
        assert!(
            !lines.iter().any(|l| matches!(l, Line::Text(_))),
            "no paragraphs on first run"
        );
        assert_eq!(p.summary(Section::Queries).as_deref(), Some("none yet"));
        assert_eq!(
            p.summary(Section::QueryHistory).as_deref(),
            Some("none yet")
        );
        assert_eq!(
            p.summary(Section::VariantAnalysis).as_deref(),
            Some("set up")
        );
    }

    #[test]
    fn selection_skips_text_and_folding_keeps_it_on_the_header() {
        let mut p = CodeqlPanel::new();
        // The card's drawn rows are skipped: its button is the first stop.
        p.move_selection(true);
        assert_eq!(p.selected_hit(), Some(Hit::Action(Action::AddDatabase)));
        p.move_selection(true);
        assert_eq!(
            p.selected_hit(),
            Some(Hit::Action(Action::QuickQuery)),
            "the caption, the card's edge and the button caps are skipped"
        );
        p.move_selection(true);
        assert_eq!(p.selected_hit(), Some(Hit::Header(Section::Language)));
        p.toggle(Section::Language);
        p.move_selection(true);
        assert_eq!(
            p.selected_hit(),
            Some(Hit::Action(Action::SelectLanguage(0)))
        );
        p.toggle(Section::Language);
        assert_eq!(p.selected_hit(), Some(Hit::Header(Section::Language)));
        p.move_selection(true);
        assert_eq!(p.selected_hit(), Some(Hit::Header(Section::Queries)));
        p.toggle(Section::Queries);
        p.move_selection(true);
        assert_eq!(
            p.selected_hit(),
            Some(Hit::Action(Action::CreateQuery)),
            "the empty section's one text line is skipped"
        );
        for _ in 0..12 {
            p.move_selection(false);
        }
        assert_eq!(
            p.selected_hit(),
            Some(Hit::Action(Action::AddDatabase)),
            "clamps at the top selectable row"
        );
    }

    #[test]
    fn a_fresh_selection_settles_on_the_add_database_button() {
        let mut p = CodeqlPanel::new();
        let lines = p.lines();
        assert_eq!(p.selected_hit(), None, "row 0 is the card's margin");
        p.settle_selection(&lines);
        assert_eq!(p.selected_hit(), Some(Hit::Action(Action::AddDatabase)));
    }

    #[test]
    fn databases_list_with_the_current_one_marked() {
        let mut p = CodeqlPanel::new();
        p.databases = vec![
            crate::codeql_db::DbEntry {
                name: "a-db".into(),
                path: "/x/a-db".into(),
                language: Some("python".into()),
                added: 0,
                former_names: Vec::new(),
            },
            crate::codeql_db::DbEntry {
                name: "b-db".into(),
                path: "/x/b-db".into(),
                language: Some("go".into()),
                added: 0,
                former_names: Vec::new(),
            },
        ];
        p.current_db = Some(1);
        let lines = p.lines();
        let dbs: Vec<Line> = lines
            .iter()
            .skip_while(|l| **l != Line::Header(Section::Databases))
            .skip(1)
            .take_while(|l| !matches!(l, Line::Header(_)))
            .cloned()
            .collect();
        assert_eq!(
            dbs,
            [
                Line::DbCard(DbCard::Top),
                Line::DbCard(DbCard::Name),
                Line::DbCard(DbCard::Meta),
                Line::DbCard(DbCard::Blank),
                Line::DbCard(DbCard::Chips),
                Line::DbCard(DbCard::Bottom),
                Line::Action(Action::AddDatabase, "Add Database".into()),
                Line::Action(Action::SortDatabases, "Sort by: date added".into()),
            ],
            "the card stands in for the list; adding and sorting stay on offer"
        );
        assert!(
            !lines.iter().any(|l| matches!(l, Line::Card(_))),
            "no first-run card once there is a database"
        );
        assert_eq!(p.count(Section::Databases), Some(2));
        p.select_database_card();
        assert_eq!(p.selected_hit(), Some(Hit::Action(Action::PickDatabase)));
        assert_eq!(
            p.selected_database(),
            Some(1),
            "the card acts on the current one"
        );
        // One database: no sort row. None current: no meta row.
        p.databases.pop();
        p.current_db = None;
        let lines = p.lines();
        assert!(
            !lines
                .iter()
                .any(|l| matches!(l, Line::Action(Action::SortDatabases, _)))
        );
        assert!(!lines.contains(&Line::DbCard(DbCard::Meta)));
        assert!(lines.contains(&Line::DbCard(DbCard::Name)));
        assert_eq!(p.selected_database(), None);
    }

    #[test]
    fn databases_follow_the_language_and_keep_their_store_index() {
        let db = |name: &str, lang: Option<&str>| crate::codeql_db::DbEntry {
            name: name.into(),
            path: format!("/x/{name}").into(),
            language: lang.map(Into::into),
            added: 0,
            former_names: Vec::new(),
        };
        let mut p = CodeqlPanel::new();
        p.databases = vec![
            db("gin", Some("go")),
            db("web", Some("typescript")),
            db("odd", None),
        ];
        p.current_db = Some(1);
        assert!(
            p.lines().contains(&Line::Action(
                Action::SortDatabases,
                "Sort by: date added".into()
            )),
            "the list offers its order"
        );
        p.db_sort = Some(crate::codeql_db::DbSort::Name);
        assert!(
            p.lines()
                .contains(&Line::Action(Action::SortDatabases, "Sort by: name".into()))
        );
        assert_eq!(
            p.database_choices(),
            [
                (0, "○ gin · Go".to_string()),
                (1, "● web · JavaScript / TypeScript".to_string()),
                (2, "○ odd".to_string()),
            ],
            "the picker marks the current one"
        );
        // JavaScript / TypeScript (index 5) hides the Go database.
        p.language = Some(5);
        let ids: Vec<usize> = p.database_choices().into_iter().map(|(i, _)| i).collect();
        assert_eq!(
            ids,
            [1, 2],
            "store indices, and the unknown language shows under any"
        );
        // Swift: nothing but the one of unknown language.
        p.databases.pop();
        p.language = Some(9);
        assert!(p.database_choices().is_empty());
    }

    #[test]
    fn language_selection_marks_one() {
        let mut p = CodeqlPanel::new();
        p.toggle(Section::Language);
        p.language = Some(6);
        assert!(
            p.lines()
                .contains(&Line::Action(Action::SelectLanguage(6), "● Python".into()))
        );
    }

    #[test]
    fn query_history_lists_each_run_to_open_and_keeps_its_welcome_when_empty() {
        let mut p = CodeqlPanel::new();
        assert!(p.folded(Section::QueryHistory), "empty, it starts folded");
        p.toggle(Section::QueryHistory);
        assert!(
            p.lines()
                .contains(&Line::Text("Run a query to see results."))
        );
        use crate::codeql_query::RunStatus;
        p.history = vec![
            hist("a.ql", RunStatus::Succeeded, Some(2), 3),
            hist("b.ql", RunStatus::Failed("x".into()), None, 1),
        ];
        let lines = p.lines();
        assert!(!lines.contains(&Line::Text("Run a query to see results.")));
        assert_eq!(p.count(Section::QueryHistory), Some(2));
        assert_eq!(p.summary(Section::QueryHistory), None);
        assert!(lines.contains(&Line::History(0)));
        assert!(lines.contains(&Line::History(1)));
        assert!(
            lines.contains(&Line::Action(Action::SortHistory, "Sort by: date".into())),
            "the list offers its order"
        );
        p.history_sort = crate::codeql_query::HistSort::Name;
        assert!(
            p.lines()
                .contains(&Line::Action(Action::SortHistory, "Sort by: name".into()))
        );
        p.select_history(1);
        assert_eq!(p.selected_history(), Some(1));
    }

    fn hist(
        name: &str,
        status: crate::codeql_query::RunStatus,
        results: Option<u64>,
        seconds: u64,
    ) -> HistoryRow {
        HistoryRow {
            status,
            name: name.into(),
            database: "app".into(),
            results,
            seconds,
            started: 0,
            output: format!("/r/{name}/results.sarif").into(),
        }
    }

    fn pack(
        name: &str,
        language: Option<&str>,
        dir: &str,
        queries: &[&str],
    ) -> crate::codeql_query::QueryPack {
        crate::codeql_query::QueryPack {
            name: name.into(),
            language: language.map(Into::into),
            dir: dir.into(),
            queries: queries
                .iter()
                .map(|q| format!("{dir}/{q}").into())
                .collect(),
        }
    }

    #[test]
    fn queries_list_by_pack_and_follow_the_language() {
        let mut p = CodeqlPanel::new();
        p.toggle(Section::Queries);
        assert!(
            p.lines()
                .contains(&Line::Text("No queries in this workspace."))
        );
        assert!(
            p.lines()
                .contains(&Line::Action(Action::CreateQuery, "Create a query".into()))
        );
        p.queries = vec![
            pack("acme/go", Some("go"), "/w/go", &["a.ql", "sub/b.ql"]),
            pack("acme/py", Some("python"), "/w/py", &["c.ql"]),
        ];
        let lines = p.lines();
        assert!(!lines.contains(&Line::Text("No queries in this workspace.")));
        assert_eq!(p.count(Section::Queries), Some(3), "queries, not packs");
        let open = crate::icons::CHEVRON_OPEN;
        assert!(lines.contains(&Line::Action(
            Action::TogglePack(0),
            format!("{open} acme/go (go)")
        )));
        assert!(lines.contains(&Line::Action(Action::RunQuery(0, 1), "  sub/b.ql".into())));
        assert!(lines.contains(&Line::Action(Action::RunQuery(1, 0), "  c.ql".into())));
        // Python (index 6) hides the Go pack but keeps the indices stable.
        p.language = Some(6);
        let lines = p.lines();
        assert!(
            !lines
                .iter()
                .any(|l| matches!(l, Line::Action(Action::RunQuery(0, _), _)))
        );
        assert!(lines.contains(&Line::Action(Action::RunQuery(1, 0), "  c.ql".into())));
        // Nothing in Swift: the welcome comes back.
        p.language = Some(9);
        assert!(
            p.lines()
                .contains(&Line::Text("No queries in this workspace."))
        );
        // A pack that does not say its language shows under any.
        p.queries
            .push(pack(crate::codeql_query::NO_PACK, None, "/w", &["x.ql"]));
        assert!(
            p.lines()
                .contains(&Line::Action(Action::RunQuery(2, 0), "  x.ql".into()))
        );
    }

    #[test]
    fn a_discovery_in_flight_says_so_and_its_result_keeps_the_selection() {
        // #840: the queries land after the view has opened.
        let mut p = CodeqlPanel::new();
        p.discovering_queries = true;
        let lines = p.lines();
        assert!(lines.contains(&Line::Text("Discovering queries\u{2026}")));
        assert!(!lines.contains(&Line::Text("We didn't find any CodeQL queries in")));
        p.selected = lines
            .iter()
            .position(|l| *l == Line::Header(Section::QueryHistory))
            .unwrap();
        p.set_queries(vec![pack(
            "acme/go",
            Some("go"),
            "/w/go",
            &["a.ql", "b.ql"],
        )]);
        assert!(!p.discovering_queries);
        assert!(
            p.lines()
                .contains(&Line::Action(Action::RunQuery(0, 1), "  b.ql".into()))
        );
        assert_eq!(p.selected_hit(), Some(Hit::Header(Section::QueryHistory)));
    }

    #[test]
    fn a_pack_folds_and_keeps_the_selection_on_its_line() {
        let mut p = CodeqlPanel::new();
        p.queries = vec![pack("acme/go", Some("go"), "/w/go", &["a.ql"])];
        p.toggle_pack(0);
        assert_eq!(p.selected_hit(), Some(Hit::Action(Action::TogglePack(0))));
        assert!(
            !p.lines()
                .iter()
                .any(|l| matches!(l, Line::Action(Action::RunQuery(..), _)))
        );
        p.toggle_pack(0);
        assert_eq!(p.selected_pack(), Some(0));
        p.move_selection(true);
        assert_eq!(p.selected_hit(), Some(Hit::Action(Action::RunQuery(0, 0))));
        assert_eq!(p.selected_pack(), Some(0), "a query row is in its pack");
        p.toggle(Section::Queries);
        assert_eq!(p.selected_pack(), None);
    }

    #[test]
    fn variant_analysis_lists_the_controller_lists_repos_and_owners() {
        use crate::codeql_variant::Item;
        let mut p = CodeqlPanel::new();
        p.variant.lists.push(crate::codeql_variant::RepoList {
            name: "top".into(),
            repos: vec!["a/b".into(), "c/d".into()],
        });
        p.variant.repos.push("e/f".into());
        p.variant.owners.push("octo".into());
        // Without a controller the welcome stays, and there is no adding.
        let lines = p.lines();
        assert!(lines.contains(&Line::Action(
            Action::SetUpControllerRepository,
            "Set up controller repository".into()
        )));
        assert!(!lines.contains(&Line::Action(
            Action::AddVariantRepo,
            "Add repository".into()
        )));

        p.variant.controller_repo = Some("me/ctl".into());
        p.variant.select(Item::Repo(Some(0), 1)).unwrap();
        let open = crate::icons::CHEVRON_OPEN;
        let va: Vec<Line> = p
            .lines()
            .into_iter()
            .skip_while(|l| *l != Line::Header(Section::VariantAnalysis))
            .skip(1)
            .take_while(|l| !matches!(l, Line::Header(_)))
            .collect();
        assert_eq!(
            va,
            [
                Line::Action(
                    Action::SetUpControllerRepository,
                    "Controller: me/ctl".into()
                ),
                Line::Action(Action::VariantList(0), format!("{open} ○ top (2)")),
                Line::Action(Action::VariantRepo(Some(0), 0), "    ○ a/b".into()),
                Line::Action(Action::VariantRepo(Some(0), 1), "    ● c/d".into()),
                Line::Action(Action::VariantRepo(None, 0), "○ e/f".into()),
                Line::Action(Action::VariantOwner(0), "○ octo (owner)".into()),
                Line::Action(Action::AddVariantRepo, "Add repository".into()),
                Line::Action(Action::AddVariantList, "Add repository list".into()),
                Line::Action(Action::AddVariantOwner, "Add owner".into()),
            ]
        );

        // A repository row is in its list; folding hides the rows.
        p.select_variant_item(Item::Repo(Some(0), 1));
        assert_eq!(p.selected_variant_list(), Some(0));
        assert_eq!(p.selected_section(), Some(Section::VariantAnalysis));
        p.toggle_variant_list(0);
        assert_eq!(p.selected_variant_item(), Some(Item::List(0)));
        assert!(
            !p.lines()
                .iter()
                .any(|l| matches!(l, Line::Action(Action::VariantRepo(Some(_), _), _)))
        );
        p.select_variant_item(Item::Owner(0));
        assert_eq!(p.selected_variant_list(), None);

        // An unreadable config says so instead of looking empty.
        p.variant_error = true;
        let lines = p.lines();
        assert!(lines.contains(&Line::Action(
            Action::OpenVariantConfig,
            "Open config file".into()
        )));
        assert!(
            !lines
                .iter()
                .any(|l| matches!(l, Line::Action(Action::VariantOwner(_), _)))
        );
    }

    fn db(name: &str, lang: &str) -> crate::codeql_db::DbEntry {
        crate::codeql_db::DbEntry {
            name: name.into(),
            path: format!("/x/{name}").into(),
            language: Some(lang.into()),
            added: 0,
            former_names: Vec::new(),
        }
    }

    /// Paint `p` focused, under the Black theme, into a 32 x 44 side bar.
    fn draw(p: &mut CodeqlPanel) -> Buffer {
        draw_w(p, 32)
    }

    /// [`draw`] into a side bar `w` columns wide.
    fn draw_w(p: &mut CodeqlPanel, w: u16) -> Buffer {
        p.focused = true;
        p.focus_gradient = true;
        p.theme = crate::theme::Theme::BLACK;
        let backend = ratatui::backend::TestBackend::new(w, 44);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| f.render_widget(&mut *p, f.area())).unwrap();
        term.backend().buffer().clone()
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// The first row whose text contains `needle`, and where in it.
    fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
        (0..buf.area.height).find_map(|y| {
            let text = row(buf, y);
            let at = text.find(needle)?;
            Some((text[..at].chars().count() as u16, y))
        })
    }

    #[test]
    fn the_pane_draws_in_a_frame_titled_codeql() {
        let mut p = CodeqlPanel::new();
        let buf = draw(&mut p);
        let top = row(&buf, 0);
        assert!(top.starts_with("╭ CODEQL ─"), "{top}");
        assert_eq!(
            buf[(2, 0)].fg,
            crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG)
        );
        assert!(row(&buf, 43).starts_with("╰──"), "the bottom border");
        assert_eq!(p.last_inner, Rect::new(1, 1, 30, 42));
    }

    #[test]
    fn the_first_run_card_has_its_heading_steps_and_button() {
        let mut p = CodeqlPanel::new();
        let buf = draw(&mut p);
        let (x, y) = find(&buf, "Query your code for bugs").expect("the heading");
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
        assert!(find(&buf, "QL│").is_some(), "the lens of the line art");
        let accent = crate::gradient::rgb_color(crate::gradient::CARD_ACCENT);
        let (x, y) = find(&buf, "● Add a database").expect("the first step");
        assert_eq!(
            buf[(x, y)].fg,
            accent,
            "the step to do is filled in the accent"
        );
        let (x, y) = find(&buf, "○ Write or open a query").expect("the second step");
        assert_ne!(buf[(x, y)].fg, accent);
        assert!(find(&buf, "○ Run it, read the flows").is_some());
        let (x, y) = find(&buf, "+  Add Database").expect("the primary button");
        assert_eq!(
            buf[(x, y)].bg,
            crate::gradient::rgb_color(crate::gradient::PRIMARY_BTN_BG)
        );
        assert_eq!(row(&buf, y - 1).chars().nth(4), Some('╭'), "a rounded cap");
        assert!(
            find(&buf, "from a folder, archive,").is_some(),
            "the caption"
        );
        assert!(find(&buf, "» Try Quick Query").is_some());
        assert!(
            find(&buf, "AST · Log · Model").is_some(),
            "the folded Tools row"
        );
        assert!(
            find(&buf, "DATABASES").is_none(),
            "the card stands in for it"
        );
    }

    #[test]
    fn databases_and_history_show_count_badges_and_no_card() {
        let mut p = CodeqlPanel::new();
        use crate::codeql_query::RunStatus;
        p.databases = vec![db("croft-core", "rust"), db("flask", "python")];
        p.current_db = Some(0);
        p.history = vec![
            hist("a.ql", RunStatus::Succeeded, Some(1), 3),
            hist("b.ql", RunStatus::Succeeded, Some(1), 4),
        ];
        let buf = draw(&mut p);
        assert!(find(&buf, "Query your code").is_none());
        let (_, y) = find(&buf, "DATABASES").unwrap();
        assert!(row(&buf, y).ends_with(" 2  │"), "{}", row(&buf, y));
        assert_eq!(buf[(28, y)].bg, crate::theme::Theme::BLACK.accent_chip_bg());
        let (_, y) = find(&buf, "QUERY HISTORY").unwrap();
        assert!(row(&buf, y).ends_with(" 2  │"), "{}", row(&buf, y));
        assert!(find(&buf, "◆ croft-core").is_some(), "the current database");
        assert!(
            find(&buf, "flask").is_none(),
            "the others are in the picker"
        );
        assert!(find(&buf, "+ Add Database").is_some(), "adding more stays");
        let (_, y) = find(&buf, "QUERIES").unwrap();
        assert!(row(&buf, y).contains("none yet"));
    }

    #[test]
    fn the_tools_row_folds_the_idle_data_views() {
        let mut p = CodeqlPanel::new();
        let buf = draw(&mut p);
        assert!(find(&buf, "TOOLS").is_some());
        assert!(find(&buf, "View AST").is_none(), "folded");
        p.toggle(Section::Tools);
        assert_eq!(p.selected_hit(), Some(Hit::Header(Section::Tools)));
        let buf = draw(&mut p);
        let (_, y) = find(&buf, "TOOLS").unwrap();
        assert!(row(&buf, y + 1).contains("› View AST"));
        assert!(row(&buf, y + 2).contains("› Evaluator Log"));
        assert!(row(&buf, y + 3).contains("› Model Editor"));
        p.move_selection(true);
        assert_eq!(p.selected_hit(), Some(Hit::Action(Action::ViewAst)));
        p.move_selection(true);
        assert_eq!(
            p.selected_hit(),
            Some(Hit::Action(Action::ShowEvaluatorLog))
        );
        p.move_selection(true);
        assert_eq!(p.selected_hit(), Some(Hit::Action(Action::OpenModelEditor)));

        // A view with something to show is its own section again; Tools
        // keeps the others.
        p.evallog = Some(crate::codeql_evallog::LogView::default());
        let lines = p.lines();
        assert!(lines.contains(&Line::Header(Section::EvaluatorLog)));
        assert!(!lines.contains(&Line::Action(
            Action::ShowEvaluatorLog,
            "Evaluator Log".into()
        )));
        assert_eq!(p.summary(Section::Tools).as_deref(), Some("AST · Model"));
    }

    #[test]
    fn the_selected_row_gets_the_selection_fill_and_an_accent_bar() {
        let mut p = CodeqlPanel::new();
        p.select_action(Action::QuickQuery);
        p.move_selection(true);
        assert_eq!(p.selected_hit(), Some(Hit::Header(Section::Language)));
        let buf = draw(&mut p);
        let (_, y) = find(&buf, "LANGUAGE").unwrap();
        assert_eq!(buf[(1, y)].symbol(), "▎");
        assert_eq!(
            buf[(1, y)].fg,
            crate::gradient::rgb_color(crate::gradient::CARD_ACCENT)
        );
        let sel = crate::theme::Theme::BLACK.selection();
        assert_eq!(buf[(1, y)].bg, sel);
        assert_eq!(buf[(20, y)].bg, sel, "across the row");
        assert_ne!(buf[(20, y + 1)].bg, sel, "and only that row");
        // Unfocused, nothing is marked.
        p.focused = false;
        let backend = ratatui::backend::TestBackend::new(32, 44);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| f.render_widget(&mut p, f.area())).unwrap();
        assert_ne!(term.backend().buffer()[(1, y)].symbol(), "▎");
    }

    #[test]
    fn a_click_on_the_add_database_button_hits_its_action() {
        let mut p = CodeqlPanel::new();
        let buf = draw(&mut p);
        let (x, y) = find(&buf, "+  Add Database").unwrap();
        let (idx, hit) = p.hit_at(x, y).expect("the label row");
        assert_eq!(hit, Hit::Action(Action::AddDatabase));
        assert_eq!(
            p.lines()[idx],
            Line::Button(Action::AddDatabase, ButtonRow::Label)
        );
        assert_eq!(p.hit_at(x, y - 1), Some((idx, hit)), "the top cap");
        assert_eq!(p.hit_at(x, y + 1), Some((idx, hit)), "the bottom cap");
        let (x, y) = find(&buf, "Query your code").unwrap();
        assert_eq!(p.hit_at(x, y), None, "the card's text is not a row");
        assert_eq!(p.hit_at(5, 0), None, "the frame's top");
        assert_eq!(p.hit_at(5, 43), None, "the frame's bottom");
        let (x, y) = find(&buf, "LANGUAGE").unwrap();
        assert_eq!(
            p.hit_at(x, y).map(|(_, h)| h),
            Some(Hit::Header(Section::Language)),
            "rows map from the frame's inner top"
        );
        let (x, y) = find(&buf, "» Try Quick Query").unwrap();
        assert_eq!(
            p.hit_at(x, y).map(|(_, h)| h),
            Some(Hit::Action(Action::QuickQuery))
        );
    }

    #[test]
    fn the_database_card_shows_the_mark_name_meta_and_chips() {
        let mut p = CodeqlPanel::new();
        let mut core = db("croft-core", "rust");
        core.added = now() - 2 * 3600 - 30;
        p.databases = vec![core, db("flask", "python")];
        p.current_db = Some(0);
        p.db_sizes
            .insert("/x/croft-core".into(), 214 * 1024 * 1024 + 1000);
        let buf = draw(&mut p);
        let (x, y) = find(&buf, " DATABASE ").expect("the card's title");
        assert_eq!(buf[(x - 1, y)].symbol(), "─");
        assert_eq!(buf[(2, y)].symbol(), "╭");
        assert!(buf[(x + 1, y)].modifier.contains(Modifier::BOLD));
        let (x, y) = find(&buf, "◆ croft-core").expect("the name row");
        assert_eq!((x, y), (4, y), "the mark inside the card");
        let t = crate::theme::Theme::BLACK;
        assert_eq!(
            buf[(x, y)].fg,
            t.ui(crate::icons::for_path("main.rs", ".rs").color),
            "the Explorer's colour for Rust files"
        );
        assert!(buf[(x + 2, y)].modifier.contains(Modifier::BOLD));
        assert!(row(&buf, y).ends_with("▾ │ │"), "{}", row(&buf, y));
        assert_eq!(
            p.hit_at(x + 3, y).map(|(_, h)| h),
            Some(Hit::Action(Action::PickDatabase)),
            "the name row switches databases"
        );
        let (mx, my) = find(&buf, "Rust · 214 MB · 2h ago").expect("the meta row");
        assert_eq!((mx, my), (6, y + 1));
        assert_eq!(buf[(mx, my)].fg, p.palette().dim);
        // Too long for the card: the age goes whole, never cut to "3d a".
        p.databases[1].added = now() - 3 * 86400 - 30;
        p.db_sizes.insert("/x/flask".into(), 900 * 1024);
        p.current_db = Some(1);
        let buf = draw_w(&mut p, 30);
        let (_, fy) = find(&buf, "◆ flask").expect("the name row");
        let meta = row(&buf, fy + 1);
        assert!(meta.contains("Python · 900 KB "), "{meta}");
        assert!(!meta.contains("3d"), "{meta}");
        p.current_db = Some(0);
        let buf = draw(&mut p);
        let (_, y) = find(&buf, "◆ croft-core").expect("the name row");
        // Too narrow for the keys at 32 columns: the chips keep their
        // labels; wider, the keys show.
        let (cx, cy) = find(&buf, "▶ Run").expect("the Run chip");
        assert_eq!(cy, y + 3, "under a blank row");
        assert_eq!(buf[(cx - 1, cy)].bg, t.accent_chip_bg(), "a chip fill");
        assert!(find(&buf, "» Quick").is_some());
        assert!(find(&buf, "AST").is_some());
        assert!(find(&buf, "▶ Run r").is_none());
        assert!(row(&buf, cy + 1).contains("╰"), "the card's bottom");
        let buf = draw_w(&mut p, 36);
        let (x, y) = find(&buf, "▶ Run r").expect("the key after the label");
        assert!(buf[(x + 6, y)].modifier.contains(Modifier::BOLD));
        assert!(find(&buf, "» Quick q").is_some());
        assert!(find(&buf, "AST a").is_some());

        // No current database: the card asks for one.
        p.current_db = None;
        let buf = draw(&mut p);
        let (x, y) = find(&buf, "Select a database").expect("the prompt");
        assert_eq!(buf[(x, y)].fg, p.palette().dim);
        assert!(row(&buf, y).ends_with("▾ │ │"));
        assert!(find(&buf, "214 MB").is_none(), "no meta row");
    }

    #[test]
    fn history_rows_align_their_glyph_count_and_duration() {
        use crate::codeql_query::RunStatus;
        let mut p = CodeqlPanel::new();
        p.history = vec![
            hist("UnsafeDeref", RunStatus::Succeeded, Some(17), 4),
            hist("Broken.ql", RunStatus::Failed("x".into()), None, 68),
            hist("SlowJoin.ql", RunStatus::Cancelled, None, 160),
        ];
        let buf = draw(&mut p);
        let pal = p.palette();
        let (_, y0) = find(&buf, "✓ UnsafeDeref").expect("the succeeded row");
        let rows: Vec<String> = (y0..y0 + 3).map(|y| row(&buf, y)).collect();
        assert!(rows[0].ends_with("  17    4s │"), "{}", rows[0]);
        assert!(rows[1].ends_with(" err  1m08 │"), "{}", rows[1]);
        assert!(rows[2].ends_with("   —  2m40 │"), "{}", rows[2]);
        assert!(rows[1].contains("✗ Broken.ql"));
        assert!(rows[2].contains("− SlowJoin.ql"));
        let glyphs = [pal.added, pal.deleted, pal.dim];
        for (dy, fg) in glyphs.into_iter().enumerate() {
            assert_eq!(buf[(3, y0 + dy as u16)].fg, fg, "glyph {dy}");
        }
        // The count column in bold white, `err` in red, the dash faint;
        // the durations dim, a cancelled one fainter.
        assert_eq!(buf[(23, y0)].fg, pal.bright);
        assert!(buf[(23, y0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(23, y0 + 1)].fg, pal.deleted);
        assert_eq!(buf[(23, y0 + 2)].fg, pal.faint);
        assert_eq!(buf[(29, y0)].fg, pal.dim);
        assert_eq!(buf[(29, y0 + 2)].fg, pal.faint);

        // A long name is cut with an ellipsis clear of the count column.
        p.history[0].name = "AVeryLongQueryNameThatDoesNotFit.ql".into();
        let buf = draw(&mut p);
        let text = row(&buf, y0);
        assert!(text.contains("AVeryLongQuery…"), "{text}");
        assert!(text.contains("AVeryLongQuery…  17    4s │"), "{text}");
    }

    #[test]
    fn the_running_section_sweeps_the_gradient_with_the_elapsed_time() {
        use crate::codeql_query::RunStatus;
        let mut p = CodeqlPanel::new();
        p.history = vec![
            hist("TaintedPath.ql", RunStatus::Running, None, 0),
            hist("a.ql", RunStatus::Succeeded, Some(1), 3),
        ];
        p.running = Some(("TaintedPath.ql".into(), 77));
        let buf = draw(&mut p);
        // The history leaves the run in flight to the section.
        assert!(!p.lines().contains(&Line::History(0)));
        assert_eq!(p.count(Section::QueryHistory), Some(1));
        let pal = p.palette();
        let (_, y) = find(&buf, "RUNNING").expect("the section");
        assert!(row(&buf, y).ends_with("esc │"), "the key that cancels");
        let (_, hy) = find(&buf, "QUERY HISTORY").unwrap();
        assert!(hy > y + 2, "above the history");
        let (x, ny) = find(&buf, "◌ TaintedPath.ql").expect("what runs");
        assert_eq!((x, ny), (3, y + 1));
        assert_eq!(
            buf[(x, ny)].fg,
            crate::gradient::rgb_color(crate::gradient::GRAD_TR)
        );
        assert!(buf[(x + 2, ny)].modifier.contains(Modifier::BOLD));
        let by = ny + 1;
        assert!(row(&buf, by).ends_with("1:17 │"), "{}", row(&buf, by));
        assert_eq!(buf[(26, by)].fg, pal.ember);
        // The bar runs from column 5 to two short of the timer.
        let n = 26 - 2 - 5;
        let lit = sweep(n, 77);
        assert_eq!(lit, 10..16, "a third of the bar, mid-sweep");
        for c in 0..n {
            let cell = &buf[(5 + c, by)];
            if lit.contains(&c) {
                assert_eq!(cell.symbol(), "━", "cell {c}");
                let t = f32::from(c) / f32::from(n - 1);
                assert_eq!(
                    cell.fg,
                    crate::gradient::rgb_color(crate::gradient::lerp_rgb(
                        crate::gradient::GRAD_TL,
                        crate::gradient::GRAD_TR,
                        t
                    ))
                );
            } else {
                assert_eq!(cell.symbol(), "─", "cell {c}");
                assert_eq!(cell.fg, pal.faint);
            }
        }
        assert_eq!(buf[(5 + n, by)].symbol(), " ");
        // The sweep moves with the time, the same at the same second.
        assert_eq!(sweep(19, 0), 0..6);
        assert_eq!(sweep(19, 3), 6..12);
        assert_eq!(sweep(19, 9), 18..19, "leaving at the right");
        assert_eq!(sweep(19, 12), sweep(19, 0), "and starting over");
        assert_ne!(sweep(19, 77), sweep(19, 78));
        assert!((0..40).all(|t| !sweep(19, t).is_empty()), "never dark");

        // The section goes with the run, and the selection keeps its row.
        p.select_history(1);
        p.set_running(None);
        assert!(!p.lines().contains(&Line::Header(Section::Running)));
        assert!(p.lines().contains(&Line::History(0)), "back in the history");
        assert_eq!(p.selected_history(), Some(1));
        p.set_running(Some(("b.ql".into(), 1)));
        assert_eq!(p.selected_history(), Some(1));
    }

    #[test]
    fn a_narrow_row_never_lists_a_chip_line_it_cannot_paint() {
        use crate::codeql_query::RunStatus;
        let mut p = CodeqlPanel::new();
        p.history = vec![hist("UnsafeDeref", RunStatus::Succeeded, Some(17), 4)];
        for w in [12, 14, 16, 18, 22, 36] {
            let _ = draw_w(&mut p, w);
            p.select_history(0);
            if !p.is_expanded(0) {
                p.toggle_history(0);
            }
            let _ = draw_w(&mut p, w);
            for line in p.lines() {
                if matches!(line, Line::HistoryDetail(_, Detail::Chips(_))) {
                    assert!(!p.chips_on(&line).is_empty(), "an empty chip row at {w}");
                }
            }
        }
    }

    #[test]
    fn a_history_row_expands_into_details_and_chips_and_collapses() {
        use crate::codeql_query::RunStatus;
        let mut p = CodeqlPanel::new();
        p.history = vec![
            hist("UnsafeDeref", RunStatus::Succeeded, Some(17), 4),
            hist("b.ql", RunStatus::Succeeded, Some(1), 2),
        ];
        let _ = draw(&mut p);
        p.select_history(0);
        p.toggle_history(0);
        assert!(p.is_expanded(0) && !p.is_expanded(1));
        assert_eq!(p.selected_history(), Some(0), "the selection stays");
        let buf = draw(&mut p);
        let (_, y) = find(&buf, "✓ UnsafeDeref").unwrap();
        assert!(
            row(&buf, y + 1).contains("├ 17 alerts"),
            "{}",
            row(&buf, y + 1)
        );
        assert!(row(&buf, y + 1).ends_with("SARIF │"));
        assert!(
            row(&buf, y + 2).contains("├ app · "),
            "{}",
            row(&buf, y + 2)
        );
        assert!(
            row(&buf, y + 3).contains("└  ↗ Open   ↔ Compare "),
            "{}",
            row(&buf, y + 3)
        );
        assert!(
            row(&buf, y + 4).contains("   ↓ Export   ≡ Log "),
            "{}",
            row(&buf, y + 4)
        );
        let (x, cy) = find(&buf, "↗ Open").unwrap();
        assert_eq!(buf[(x, cy)].bg, crate::theme::Theme::BLACK.accent_chip_bg());
        assert!(row(&buf, y + 5).contains("✓ b.ql"), "the next row follows");
        // Tables are rows, in CSV.
        p.history[0].output = "/r/t/results.csv".into();
        p.expanded = Some(p.history[0].output.clone());
        let buf = draw(&mut p);
        assert!(row(&buf, y + 1).contains("├ 17 rows"));
        assert!(row(&buf, y + 1).ends_with("CSV │"));
        p.toggle_history(0);
        assert!(!p.is_expanded(0));
        assert!(
            !p.lines()
                .iter()
                .any(|l| matches!(l, Line::HistoryDetail(..)))
        );
    }

    #[test]
    fn a_click_on_a_chip_hits_the_chip_under_the_column() {
        use crate::codeql_query::RunStatus;
        let mut p = CodeqlPanel::new();
        p.databases = vec![db("croft-core", "rust")];
        p.current_db = Some(0);
        p.history = vec![hist("UnsafeDeref", RunStatus::Succeeded, Some(17), 4)];
        let _ = draw(&mut p);
        p.toggle_history(0);
        let buf = draw(&mut p);
        let lines = p.lines();
        let owner = lines.iter().position(|l| *l == Line::History(0)).unwrap();
        let (x, y) = find(&buf, "↓ Export").unwrap();
        assert_eq!(
            p.hit_at(x, y),
            Some((owner, Hit::Action(Action::ExportHistory(0)))),
            "the chip, selecting its history row"
        );
        assert_eq!(
            p.hit_at(x - 1, y).map(|(_, h)| h),
            Some(Hit::Action(Action::ExportHistory(0))),
            "the chip's padding"
        );
        assert_eq!(p.hit_at(x - 2, y), None, "left of every chip");
        let (x, _) = find(&buf, "≡ Log").unwrap();
        assert_eq!(
            p.hit_at(x, y).map(|(_, h)| h),
            Some(Hit::Action(Action::HistoryLog(0)))
        );
        let (x, y) = find(&buf, "↔ Compare").unwrap();
        assert_eq!(
            p.hit_at(x, y).map(|(_, h)| h),
            Some(Hit::Action(Action::CompareHistory(0)))
        );
        let (x, y) = find(&buf, "↗ Open").unwrap();
        assert_eq!(
            p.hit_at(x, y).map(|(_, h)| h),
            Some(Hit::Action(Action::OpenHistory(0)))
        );
        // The card's chips select its name row.
        let card = lines
            .iter()
            .position(|l| *l == Line::DbCard(DbCard::Name))
            .unwrap();
        let (x, y) = find(&buf, "AST").unwrap();
        assert_eq!(p.hit_at(x, y), Some((card, Hit::Action(Action::ViewAst))));
        let (x, y) = find(&buf, "» Quick").unwrap();
        assert_eq!(
            p.hit_at(x, y).map(|(_, h)| h),
            Some(Hit::Action(Action::QuickQuery))
        );
        let (x, y) = find(&buf, "▶ Run").unwrap();
        assert_eq!(
            p.hit_at(x, y).map(|(_, h)| h),
            Some(Hit::Action(Action::RunOpenQuery))
        );
        assert_eq!(p.hit_at(2, y), None, "the card's edge");
    }

    #[test]
    fn short_forms_of_durations_ages_and_sizes() {
        assert_eq!(duration(4), "4s");
        assert_eq!(duration(68), "1m08");
        assert_eq!(duration(160), "2m40");
        assert_eq!(duration(3900), "1h05");
        assert_eq!(short_age(30), "just now");
        assert_eq!(short_age(7230), "2h ago");
        assert_eq!(short_age(3 * 86_400), "3d ago");
        assert_eq!(short_size(512), "512 B");
        assert_eq!(short_size(214 * 1024 * 1024), "214 MB");
        assert_eq!(short_size(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert_eq!(clock(0, 5 * 86_400), "5d ago");
    }
}
