//! The CodeQL side bar (#578): the view behind the QL activity-bar icon,
//! laid out as VS Code's CodeQL extension lays out its container. Sections
//! fold like VS Code's view panes; an empty section shows the same welcome
//! text and actions VS Code does, so the path from nothing to a first query
//! reads the same in both editors.

// The activity bar and side bar that use this land in the next change.
#![allow(dead_code)]

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

/// VS Code's `ql-container` views, in its order. The Evaluator Log Viewer is
/// canary-only there and is left out here for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Language,
    Databases,
    Queries,
    VariantAnalysis,
    QueryHistory,
    AstViewer,
    EvaluatorLog,
    MethodModeling,
}

impl Section {
    pub const ALL: [Section; 8] = [
        Section::Language,
        Section::Databases,
        Section::Queries,
        Section::VariantAnalysis,
        Section::QueryHistory,
        Section::AstViewer,
        Section::EvaluatorLog,
        Section::MethodModeling,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Language => "LANGUAGE",
            Section::Databases => "DATABASES",
            Section::Queries => "QUERIES",
            Section::VariantAnalysis => "VARIANT ANALYSIS REPOSITORIES",
            Section::QueryHistory => "QUERY HISTORY",
            Section::AstViewer => "AST VIEWER",
            Section::EvaluatorLog => "EVALUATOR LOG VIEWER",
            Section::MethodModeling => "METHOD MODELING",
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
    /// Query history labels, in the store's order (#578).
    pub history: Vec<String>,
    /// The order the store keeps the history in, shown on the sort row.
    pub history_sort: crate::codeql_query::HistSort,
    /// The workspace's queries by pack, from the last discovery.
    pub queries: Vec<crate::codeql_query::QueryPack>,
    /// Folded packs, by folder, so a fold survives rediscovery.
    pub folded_packs: std::collections::HashSet<std::path::PathBuf>,
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

    /// The store index of the database whose row is selected.
    pub fn selected_database(&self) -> Option<usize> {
        match self.selected_hit() {
            Some(Hit::Action(Action::SelectDatabase(i))) => Some(i),
            _ => None,
        }
    }

    /// Put the selection on database `index`'s row, when it shows.
    pub fn select_database(&mut self, index: usize) {
        if let Some(n) = self
            .lines()
            .iter()
            .position(|l| matches!(l, Line::Action(Action::SelectDatabase(i), _) if *i == index))
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
        if let Some(n) = self
            .lines()
            .iter()
            .position(|l| matches!(l, Line::Action(Action::OpenHistory(i), _) if *i == index))
        {
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
        if let Some(n) = self
            .lines()
            .iter()
            .position(|l| matches!(l, Line::Action(a, _) if *a == action))
        {
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
            out.push(Line::Text("The variant analysis config file could"));
            out.push(Line::Text("not be read. Fix or remove it to go on."));
            out.push(Line::Action(
                Action::OpenVariantConfig,
                "Open config file".to_string(),
            ));
            return;
        }
        let v = &self.variant;
        match &v.controller_repo {
            None => {
                out.push(Line::Text("Set up a controller repository to start"));
                out.push(Line::Text("using variant analysis."));
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

    /// Every line the panel paints, top to bottom.
    pub fn lines(&self) -> Vec<Line> {
        let mut out = Vec::new();
        for s in Section::ALL {
            out.push(Line::Header(s));
            if self.collapsed.contains(&s) {
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
                    if !self.databases.is_empty() {
                        let by = self.db_sort.map_or("date added", |b| b.label());
                        out.push(Line::Action(
                            Action::SortDatabases,
                            format!("Sort by: {by}"),
                        ));
                    }
                    let mut shown = 0;
                    for (i, db) in self.databases.iter().enumerate() {
                        if !self.db_matches_language(db) {
                            continue;
                        }
                        shown += 1;
                        let mark = if self.current_db == Some(i) {
                            "●"
                        } else {
                            "○"
                        };
                        let lang = db
                            .language
                            .as_deref()
                            .map(|l| format!(" ({l})"))
                            .unwrap_or_default();
                        out.push(Line::Action(
                            Action::SelectDatabase(i),
                            format!("{mark} {}{lang}", db.name),
                        ));
                    }
                    if shown == 0 && !self.databases.is_empty() {
                        out.push(Line::Text("No databases in this language."));
                    }
                    out.push(Line::Text("Add a CodeQL database:"));
                    for (a, label) in [
                        (Action::AddDatabaseFromFolder, "From a folder"),
                        (Action::AddDatabaseFromArchive, "From an archive"),
                        (Action::AddDatabaseFromUrl, "From a URL (as a zip file)"),
                        (Action::AddDatabaseFromGithub, "From GitHub"),
                    ] {
                        out.push(Line::Action(a, label.to_string()));
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
                Section::Queries => {
                    out.push(Line::Text("We didn't find any CodeQL queries in"));
                    out.push(Line::Text("this workspace."));
                    out.push(Line::Action(
                        Action::CreateQuery,
                        "Create one to get started".to_string(),
                    ));
                }
                Section::VariantAnalysis => self.variant_lines(&mut out),
                Section::QueryHistory if !self.history.is_empty() => {
                    out.push(Line::Action(
                        Action::SortHistory,
                        format!("Sort by: {}", self.history_sort.label()),
                    ));
                    for (i, label) in self.history.iter().enumerate() {
                        out.push(Line::Action(Action::OpenHistory(i), label.clone()));
                    }
                }
                Section::QueryHistory => {
                    out.push(Line::Text("You have no query history items at the"));
                    out.push(Line::Text("moment. Select a database to run a CodeQL"));
                    out.push(Line::Text("query and get your first results."));
                }
                Section::AstViewer => match &self.ast {
                    Some(view) => {
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
                    None => {
                        out.push(Line::Text("Run 'CodeQL: View AST' on an open source"));
                        out.push(Line::Text("file from a CodeQL database."));
                        out.push(Line::Action(Action::ViewAst, "View AST".to_string()));
                    }
                },
                Section::EvaluatorLog => match &self.evallog {
                    Some(view) => {
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
                    None => {
                        out.push(Line::Text("Run 'Show Evaluator Log (Viewer)' on a"));
                        out.push(Line::Text("query history item."));
                    }
                },
                Section::MethodModeling => match &self.model {
                    Some(view) => {
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
                    None => {
                        out.push(Line::Text("Model the library methods of the"));
                        out.push(Line::Text("current database as sources, sinks"));
                        out.push(Line::Text("or summaries."));
                        out.push(Line::Action(
                            Action::OpenModelEditor,
                            "Open Model Editor".to_string(),
                        ));
                    }
                },
            }
        }
        out
    }

    fn selectable(line: &Line) -> Option<Hit> {
        match line {
            Line::Header(s) => Some(Hit::Header(*s)),
            Line::Action(a, _) => Some(Hit::Action(*a)),
            Line::Text(_) => None,
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

    pub fn selected_hit(&self) -> Option<Hit> {
        self.lines().get(self.selected).and_then(Self::selectable)
    }

    /// Fold or unfold a section, keeping the selection on its header.
    pub fn toggle(&mut self, section: Section) {
        if !self.collapsed.remove(&section) {
            self.collapsed.insert(section);
        }
        if let Some(n) = self
            .lines()
            .iter()
            .position(|l| *l == Line::Header(section))
        {
            self.selected = n;
        }
    }

    /// The row under a screen position, from the last painted frame.
    pub fn hit_at(&self, _x: u16, y: u16) -> Option<(usize, Hit)> {
        let a = self.last_area;
        if a.height == 0 || y <= a.y || y >= a.y + a.height {
            return None;
        }
        let idx = self.scroll + (y - a.y - 1) as usize;
        let lines = self.lines();
        lines.get(idx).and_then(Self::selectable).map(|h| (idx, h))
    }

    pub fn scroll_down(&mut self, n: usize) {
        let max = self.lines().len().saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }
}

impl Widget for &mut CodeqlPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        if area.height < 2 || area.width < 8 {
            return;
        }
        let theme = self.theme;
        let fg = theme.ui(Color::Rgb(0xcc, 0xcc, 0xcc));
        let dim = Color::Rgb(0x8b, 0x94, 0x9e);
        let link = theme.accent();
        let w = area.width as usize;
        buf.set_stringn(
            area.x + 1,
            area.y,
            "CODEQL",
            w.saturating_sub(1),
            Style::default().fg(fg).add_modifier(Modifier::BOLD),
        );
        let lines = self.lines();
        let rows = (area.height - 1) as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        for (r, line) in lines.iter().enumerate().skip(self.scroll).take(rows) {
            let y = area.y + 1 + (r - self.scroll) as u16;
            let selected = self.focused && r == self.selected;
            let base = if selected {
                Style::default()
                    .fg(theme.accent_contrast_fg())
                    .bg(theme.accent())
            } else {
                Style::default()
            };
            let (text, style) = match line {
                Line::Header(s) => {
                    let chevron = if self.collapsed.contains(s) {
                        crate::icons::CHEVRON_CLOSED
                    } else {
                        crate::icons::CHEVRON_OPEN
                    };
                    let title = match s {
                        Section::Language => format!(
                            "{} · {}",
                            s.title(),
                            self.language.map_or("All", |i| LANGUAGES[i])
                        ),
                        _ => s.title().to_string(),
                    };
                    (
                        format!("{chevron} {title}"),
                        base.fg(if selected { base.fg.unwrap_or(fg) } else { fg })
                            .add_modifier(Modifier::BOLD),
                    )
                }
                Line::Text(t) => (format!("  {t}"), base.fg(dim)),
                Line::Action(_, label) => (
                    format!("  {label}"),
                    if selected {
                        base
                    } else {
                        base.fg(link).add_modifier(Modifier::UNDERLINED)
                    },
                ),
            };
            buf.set_stringn(area.x, y, &text, w, style);
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
        for s in Section::ALL {
            assert!(lines.contains(&Line::Header(s)), "{s:?}");
        }
        assert!(lines.contains(&Line::Action(
            Action::AddDatabaseFromGithub,
            "From GitHub".into()
        )));
    }

    #[test]
    fn selection_skips_text_and_folding_keeps_it_on_the_header() {
        let mut p = CodeqlPanel::new();
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
        assert_eq!(p.selected_hit(), Some(Hit::Header(Section::Databases)));
        p.move_selection(true);
        assert_eq!(
            p.selected_hit(),
            Some(Hit::Action(Action::AddDatabaseFromFolder)),
            "the 'Add a CodeQL database:' text line is skipped"
        );
        p.move_selection(false);
        p.move_selection(false);
        p.move_selection(false);
        assert_eq!(
            p.selected_hit(),
            Some(Hit::Header(Section::Language)),
            "clamps at the top"
        );
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
        assert!(lines.contains(&Line::Action(
            Action::SelectDatabase(0),
            "○ a-db (python)".into()
        )));
        assert!(lines.contains(&Line::Action(
            Action::SelectDatabase(1),
            "● b-db (go)".into()
        )));
        assert!(
            lines.contains(&Line::Action(
                Action::AddDatabaseFromGithub,
                "From GitHub".into()
            )),
            "adding more stays on offer"
        );
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
        // JavaScript / TypeScript (index 5) hides the Go database.
        p.language = Some(5);
        let dbs: Vec<Action> = p
            .lines()
            .into_iter()
            .filter_map(|l| match l {
                Line::Action(a @ Action::SelectDatabase(_), _) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(
            dbs,
            [Action::SelectDatabase(1), Action::SelectDatabase(2)],
            "store indices, and the unknown language shows under any"
        );
        p.select_database(1);
        assert_eq!(p.selected_database(), Some(1));
        // Swift: nothing but the one of unknown language.
        p.databases.pop();
        p.language = Some(9);
        assert!(
            p.lines()
                .contains(&Line::Text("No databases in this language."))
        );
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
        assert!(
            p.lines()
                .contains(&Line::Text("You have no query history items at the"))
        );
        p.history = vec![
            String::from("\u{2713} a.ql \u{b7} app \u{b7} 3s"),
            String::from("\u{2717} b.ql \u{b7} app \u{b7} failed: x"),
        ];
        let lines = p.lines();
        assert!(!lines.contains(&Line::Text("You have no query history items at the")));
        assert!(lines.contains(&Line::Action(
            Action::OpenHistory(0),
            "\u{2713} a.ql \u{b7} app \u{b7} 3s".into()
        )));
        assert!(lines.contains(&Line::Action(
            Action::OpenHistory(1),
            "\u{2717} b.ql \u{b7} app \u{b7} failed: x".into()
        )));
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
        assert!(
            p.lines()
                .contains(&Line::Text("We didn't find any CodeQL queries in"))
        );
        p.queries = vec![
            pack("acme/go", Some("go"), "/w/go", &["a.ql", "sub/b.ql"]),
            pack("acme/py", Some("python"), "/w/py", &["c.ql"]),
        ];
        let lines = p.lines();
        assert!(!lines.contains(&Line::Text("We didn't find any CodeQL queries in")));
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
                .contains(&Line::Text("We didn't find any CodeQL queries in"))
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
}
