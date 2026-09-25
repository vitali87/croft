//! The CodeQL side bar (#578): the view behind the QL activity-bar icon,
//! laid out as VS Code's CodeQL extension lays out its container. Sections
//! fold like VS Code's view panes; an empty section shows the same welcome
//! text and actions VS Code does, so the path from nothing to a first query
//! reads the same in both editors.

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
    MethodModeling,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Language,
        Section::Databases,
        Section::Queries,
        Section::VariantAnalysis,
        Section::QueryHistory,
        Section::AstViewer,
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
    SelectLanguage(usize),
    /// Make the listed database at this index the current one.
    SelectDatabase(usize),
    /// Open the results of the query history entry at this index.
    OpenHistory(usize),
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
    /// Query history labels, newest first (#578).
    pub history: Vec<String>,
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
                    for (i, db) in self.databases.iter().enumerate() {
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
                Section::Queries => {
                    out.push(Line::Text("We didn't find any CodeQL queries in"));
                    out.push(Line::Text("this workspace."));
                    out.push(Line::Action(
                        Action::CreateQuery,
                        "Create one to get started".to_string(),
                    ));
                }
                Section::VariantAnalysis => {
                    out.push(Line::Text("Set up a controller repository to start"));
                    out.push(Line::Text("using variant analysis."));
                    out.push(Line::Action(
                        Action::SetUpControllerRepository,
                        "Set up controller repository".to_string(),
                    ));
                }
                Section::QueryHistory if !self.history.is_empty() => {
                    for (i, label) in self.history.iter().enumerate() {
                        out.push(Line::Action(Action::OpenHistory(i), label.clone()));
                    }
                }
                Section::QueryHistory => {
                    out.push(Line::Text("You have no query history items at the"));
                    out.push(Line::Text("moment. Select a database to run a CodeQL"));
                    out.push(Line::Text("query and get your first results."));
                }
                Section::AstViewer => {
                    out.push(Line::Text("Run 'CodeQL: View AST' on an open source"));
                    out.push(Line::Text("file from a CodeQL database."));
                    out.push(Line::Action(Action::ViewAst, "View AST".to_string()));
                }
                Section::MethodModeling => {
                    out.push(Line::Text("Select a method in the model editor to"));
                    out.push(Line::Text("see and edit its model here."));
                }
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
            },
            crate::codeql_db::DbEntry {
                name: "b-db".into(),
                path: "/x/b-db".into(),
                language: Some("go".into()),
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
    }
}
