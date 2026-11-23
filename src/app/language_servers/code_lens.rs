//! CodeLens: asking for the active file's lenses once its edits settle, landing them on
//! every tab showing the file, running one by click or from the keyboard, and Editor:
//! Toggle CodeLens.

use super::*;

impl App {
    /// Ask for the active file's code lenses once its edits have settled
    /// (#608). Lenses cost a server real work (rust-analyzer resolves each
    /// one), so this waits out [`CODE_LENS_SETTLE`] rather than riding every
    /// edit the way inlay hints do.
    pub fn tick_code_lens(&mut self) {
        if !self.code_lens_enabled {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(&seq) = self.lsp_last_seen.get(&path) else {
            return;
        };
        if self.code_lens_requested.get(&path) == Some(&seq)
            || self
                .editor
                .last_edit_at
                .is_some_and(|t| t.elapsed() < CODE_LENS_SETTLE)
        {
            return;
        }
        if let Some(lsp) = self.lsp.as_ref() {
            lsp.request_code_lens(path.clone(), seq);
            self.code_lens_requested.insert(path, seq);
        }
    }

    /// Land code-lens replies on every tab showing the file (#608). A reply
    /// for an older seq is dropped; the tab keeps its lenses, each shown only
    /// while its own line is unchanged.
    pub fn drain_lsp_code_lens(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_code_lens() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            if !self.code_lens_enabled || self.lsp_last_seen.get(&u.path) != Some(&u.seq) {
                continue;
            }
            self.for_each_tab_of(&u.path, |tab| {
                tab.code_lenses = u
                    .lenses
                    .iter()
                    .filter_map(|l| {
                        let line = l.line as usize;
                        Some(crate::code_lens::EditorLens {
                            line,
                            line_text: tab.lines.get(line)?.clone(),
                            title: l.title.clone(),
                            command: l.command.clone(),
                            arguments: l.arguments.clone(),
                            server_side: l.server_side,
                        })
                    })
                    .collect();
            });
            changed = true;
        }
        changed
    }

    /// Run the active editor's code lens at `idx` (#608).
    pub(crate) fn run_code_lens(&mut self, idx: usize) {
        let Some(lens) = self.editor.code_lenses.get(idx).cloned() else {
            return;
        };
        match crate::code_lens::action_for(&lens) {
            crate::code_lens::LensAction::ShowLocations(targets) => {
                if targets.len() == 1 {
                    let (path, line, col) = targets.into_iter().next().expect("len 1");
                    self.go_to_definition(path, line, col);
                } else {
                    self.open_location_picker(targets, "references");
                }
            }
            crate::code_lens::LensAction::RunInTerminal { label, command } => {
                self.start_project_task(crate::tasks::Task {
                    label,
                    command,
                    source: String::from("code lens"),
                    is_build: false,
                    is_default: false,
                    problem_matcher: None,
                    is_background: false,
                    vscode: None,
                });
            }
            crate::code_lens::LensAction::DebugAt(line) => {
                self.editor.cursor_row = line.min(self.editor.lines.len().saturating_sub(1));
                self.editor.cursor_col = 0;
                self.debug_test_at_cursor();
            }
            crate::code_lens::LensAction::ServerCommand { command, arguments } => {
                if let (Some(lsp), Some(path)) = (self.lsp.as_mut(), self.editor.path.clone()) {
                    lsp.execute_command(path, command, arguments);
                    self.status = format!("Ran \u{201c}{}\u{201d}", lens.title);
                }
            }
            crate::code_lens::LensAction::Unsupported(why) => {
                self.status = format!("Code lens: {why}");
            }
        }
    }

    /// The caret line's code lenses from the keyboard (Cmd+K Shift+E): one
    /// runs at once, several open a menu to pick from (#608).
    pub(in crate::app) fn run_code_lens_at_cursor(&mut self) {
        let lenses = self.editor.lenses_on_line(self.editor.cursor_row);
        match lenses.as_slice() {
            [] => self.status = String::from("No code lens on this line"),
            [one] => self.run_code_lens(*one),
            many => {
                let items: Vec<(String, MenuAction)> = many
                    .iter()
                    .map(|&i| {
                        (
                            self.editor.code_lenses[i].title.clone(),
                            MenuAction::RunCodeLens(i),
                        )
                    })
                    .collect();
                let origin = self.editor.cursor_screen_pos().unwrap_or((
                    self.editor.last_full_area.x + 1,
                    self.editor.last_full_area.y + 1,
                ));
                let root = self.tree.root.clone();
                self.context_menu = Some(ContextMenu::flat(origin, items, root));
            }
        }
    }

    /// Editor: Toggle CodeLens (#608).
    pub(in crate::app) fn toggle_code_lens(&mut self) {
        self.code_lens_enabled = !self.code_lens_enabled;
        self.code_lens_requested.clear();
        if !self.code_lens_enabled {
            for tab in self.editor.editors.iter_mut() {
                tab.code_lenses.clear();
            }
            for group in self.editor_layout.inactive_groups_mut() {
                for tab in group.editors.iter_mut() {
                    tab.code_lenses.clear();
                }
            }
        }
        self.status = format!(
            "CodeLens: {}",
            if self.code_lens_enabled { "on" } else { "off" }
        );
    }
}
