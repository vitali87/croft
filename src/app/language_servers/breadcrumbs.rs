//! The breadcrumbs bar and sticky scroll: the crumbs for the caret, each crumb's picker of
//! folders, files or sibling symbols and its filter, View: Focus and Select Breadcrumbs,
//! and the scope headers pinned above the editor.

use super::*;

impl App {
    /// The breadcrumb segments for the active editor: the workspace-relative
    /// path folders and file name, then the enclosing symbol chain at the
    /// caret. Empty for the welcome screen, preview tabs, and buffers with no
    /// path (so the bar hides). Symbol crumbs carry a jump target; path crumbs
    /// are informational.
    pub(in crate::app) fn build_breadcrumbs(&self) -> Vec<crate::widgets::editor::Crumb> {
        use crate::widgets::editor::Crumb;
        let Some(path) = self.editor.path.as_deref() else {
            return Vec::new();
        };
        if self.editor.has_non_text_view() {
            return Vec::new();
        }
        let mut crumbs = Vec::new();
        // Strip against the OWNING root (#147): a secondary-root file
        // crumbs from its own root (folder-relative, like status_path),
        // never the primary — which yielded the full absolute chain.
        let rel = self
            .roots
            .owning_root(path)
            .and_then(|r| path.strip_prefix(r).ok())
            .unwrap_or(path);
        for comp in rel.components() {
            if let std::path::Component::Normal(os) = comp {
                crumbs.push(Crumb {
                    label: os.to_string_lossy().into_owned(),
                    target: None,
                    icon: None,
                });
            }
        }
        // The file crumb wears its Explorer icon (drawn on a `surfaces`
        // theme only); the folders before it stay plain.
        if let Some(file) = crumbs.last_mut() {
            let suffix = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let icon = crate::icons::for_path(&file.label, &suffix);
            file.icon = Some((icon.glyph, icon.color));
        }
        // Symbol scope chain, only when the outline belongs to this file so a
        // stale outline from the previous tab never leaks into the bar.
        if self.outline.path() == Some(path) {
            // While scrubbing (#371) the outline is the commit's, so the
            // chain follows the historical view's caret.
            let line = self
                .scrub_view
                .as_ref()
                .map_or(self.editor.cursor_row, |v| v.cursor_row) as u32;
            let symbols = self.outline.symbols();
            let col = self
                .scrub_view
                .as_ref()
                .map_or(self.editor.cursor_col, |v| v.cursor_col) as u32;
            for i in breadcrumb_symbol_chain(symbols, line, col) {
                let s = &symbols[i];
                let icon = crate::icons::for_outline_kind(s.kind);
                crumbs.push(Crumb {
                    label: s.name.clone(),
                    target: Some((s.line, s.character)),
                    icon: Some((icon.glyph, icon.color)),
                });
            }
        }
        crumbs
    }

    /// What each crumb [`build_breadcrumbs`](Self::build_breadcrumbs) makes
    /// stands for, in the same order (#1608).
    pub(in crate::app) fn breadcrumb_targets(&self) -> Vec<CrumbTarget> {
        let Some(path) = self.editor.path.as_deref() else {
            return Vec::new();
        };
        if self.editor.has_non_text_view() {
            return Vec::new();
        }
        let root = self.roots.owning_root(path);
        let rel = root.and_then(|r| path.strip_prefix(r).ok()).unwrap_or(path);
        let mut at = root.map(Path::to_path_buf).unwrap_or_default();
        let names: Vec<_> = rel
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(os) => Some(os.to_os_string()),
                _ => None,
            })
            .collect();
        if root.is_none() {
            // An absolute path outside every root: its crumbs start at `/`.
            at = path
                .ancestors()
                .last()
                .map(Path::to_path_buf)
                .unwrap_or_default();
        }
        let mut targets = Vec::new();
        for (k, name) in names.iter().enumerate() {
            at.push(name);
            targets.push(if k + 1 == names.len() {
                CrumbTarget::File(at.clone())
            } else {
                CrumbTarget::Folder(at.clone())
            });
        }
        if self.outline.path() == Some(path) {
            let line = self
                .scrub_view
                .as_ref()
                .map_or(self.editor.cursor_row, |v| v.cursor_row) as u32;
            let col = self
                .scrub_view
                .as_ref()
                .map_or(self.editor.cursor_col, |v| v.cursor_col) as u32;
            targets.extend(
                breadcrumb_symbol_chain(self.outline.symbols(), line, col)
                    .into_iter()
                    .map(CrumbTarget::Symbol),
            );
        }
        targets
    }

    /// Open crumb `i`'s picker under it (#1608): a folder crumb lists the
    /// folder, the file crumb lists its folder with the file selected, and a
    /// symbol crumb lists the symbols beside it with it selected.
    pub(in crate::app) fn open_breadcrumb_picker(&mut self, i: usize) {
        let Some(target) = self.breadcrumb_targets().into_iter().nth(i) else {
            return;
        };
        match target {
            CrumbTarget::Folder(dir) => self.open_crumb_folder_picker(&dir, i, None),
            CrumbTarget::File(path) => {
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
                self.open_crumb_folder_picker(&dir, i, Some(&path));
            }
            CrumbTarget::Symbol(s) => {
                let Some(path) = self.editor.path.clone() else {
                    return;
                };
                let symbols = self.outline.symbols();
                let siblings = sibling_symbols(symbols, s);
                let selected = siblings.iter().position(|&k| k == s).unwrap_or(0);
                let items = siblings
                    .into_iter()
                    .map(|k| {
                        let sym = &symbols[k];
                        (
                            sym.name.clone(),
                            MenuAction::GoToLocation {
                                path: path.clone(),
                                line: sym.line,
                                col: sym.character,
                            },
                        )
                    })
                    .collect();
                self.show_crumb_picker(i, items, selected);
            }
        }
    }

    /// The picker of crumb `i` listing `dir`: folders first, then files,
    /// each by name; `select` is the row to start on.
    pub(in crate::app) fn open_crumb_folder_picker(
        &mut self,
        dir: &Path,
        i: usize,
        select: Option<&Path>,
    ) {
        let mut entries: Vec<(bool, String, PathBuf)> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter(|e| e.file_name() != ".git")
                    .map(|e| {
                        let path = e.path();
                        (
                            path.is_dir(),
                            e.file_name().to_string_lossy().into_owned(),
                            path,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        if entries.is_empty() {
            self.status = format!("{} is empty", dir.display());
            return;
        }
        let selected = select
            .and_then(|p| entries.iter().position(|e| e.2 == p))
            .unwrap_or(0);
        let items = entries
            .into_iter()
            .map(|(is_dir, name, path)| {
                if is_dir {
                    (
                        format!("{name}/"),
                        MenuAction::BrowseCrumbFolder {
                            dir: path,
                            crumb: i,
                        },
                    )
                } else {
                    (name, MenuAction::OpenCrumbFile(path))
                }
            })
            .collect();
        self.show_crumb_picker(i, items, selected);
    }

    pub(in crate::app) fn show_crumb_picker(
        &mut self,
        i: usize,
        items: Vec<(String, MenuAction)>,
        selected: usize,
    ) {
        let Some(origin) = self.editor.breadcrumb_anchor(i) else {
            return;
        };
        let mut menu = ContextMenu::flat(origin, items, self.tree.root.clone());
        menu.selected = selected;
        menu.crumb = Some(i);
        self.crumb_filter = (String::new(), menu.items.clone());
        self.context_menu = Some(menu);
    }

    /// Re-derive a breadcrumb picker's rows from `crumb_filter`: the rows
    /// whose label contains the typed text (case-insensitive), keeping the
    /// highlighted row when it survives and otherwise starting from the top.
    pub(in crate::app) fn apply_crumb_filter(&mut self) {
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        let (text, all) = &self.crumb_filter;
        let kept = menu.items.get(menu.selected).and_then(|e| match e {
            MenuEntry::Item { label, .. } => Some(label.clone()),
            _ => None,
        });
        let needle = text.trim().to_lowercase();
        menu.items = all
            .iter()
            .filter(|e| match e {
                MenuEntry::Item { label, .. } => {
                    needle.is_empty() || label.to_lowercase().contains(&needle)
                }
                _ => needle.is_empty(),
            })
            .cloned()
            .collect();
        menu.selected = kept
            .and_then(|k| {
                menu.items
                    .iter()
                    .position(|e| matches!(e, MenuEntry::Item { label, .. } if *label == k))
            })
            .unwrap_or(0);
        menu.scroll.set(0);
    }

    /// A file picked from a breadcrumb picker opens as a Quick Open pick does.
    pub(in crate::app) fn open_crumb_file(&mut self, path: &Path) {
        match self.editor.open_preview(path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.tree.reveal_path(path);
                self.focus_pane(Pane::Editor);
                self.status = format!("Opened {}", self.status_path(path));
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
    }

    /// View: Focus and Select Breadcrumbs (#1608): the last crumb's picker,
    /// from which Left / Right walk the bar.
    pub(in crate::app) fn focus_breadcrumbs(&mut self) {
        // A row too narrow for the whole chain focuses the last crumb that
        // fit on it.
        let n = self.editor.breadcrumbs.len();
        let Some(last) = (0..n)
            .rev()
            .find(|&i| self.editor.breadcrumb_anchor(i).is_some())
        else {
            self.status = String::from("No breadcrumbs to focus");
            return;
        };
        self.focus_pane(Pane::Editor);
        self.open_breadcrumb_picker(last);
    }

    /// The scope-header lines to pin as sticky scroll: the enclosing symbols of
    /// the top visible line whose header has scrolled above the viewport,
    /// outermost first, capped. Empty when the feature does not apply (welcome
    /// screen, preview tab, no outline, or nothing scrolled off). The header
    /// is the symbol's name line (its `selectionRange`), not where its range
    /// starts: a decorated Python `def` or `class` starts at `@route(...)`,
    /// which names nothing and could be pinned over the `class` line (#940).
    pub(in crate::app) fn build_sticky_lines(&self) -> Vec<u32> {
        let Some(path) = self.editor.path.as_deref() else {
            return Vec::new();
        };
        if self.editor.has_non_text_view() {
            return Vec::new();
        }
        if self.outline.path() != Some(path) {
            return Vec::new();
        }
        let top = self.editor.scroll as u32;
        let symbols = self.outline.symbols();
        breadcrumb_symbol_chain(symbols, top, 0)
            .into_iter()
            .map(|i| symbols[i].line)
            .filter(|&l| l < top)
            .take(STICKY_SCROLL_MAX)
            .collect()
    }
}

/// The breadcrumb scope chain for the caret at (`line`, `col`): the indices
/// of every outline symbol whose range encloses that position, ordered
/// outermost first (the enclosing class before the method inside it).
/// Columns count (#1221): symbols sharing a line, such as a one-line
/// interface's fields or a minified bundle's functions, are siblings, and
/// only the one the caret is in belongs to the chain. Enclosing ranges nest,
/// so the earliest start (then the latest end) is the outermost.
pub(in crate::app) fn breadcrumb_symbol_chain(
    symbols: &[crate::lsp::manager::OutlineSymbol],
    line: u32,
    col: u32,
) -> Vec<usize> {
    let start =
        |s: &crate::lsp::manager::OutlineSymbol| (s.range_start_line, s.range_start_character);
    let end = |s: &crate::lsp::manager::OutlineSymbol| (s.range_end_line, s.range_end_character);
    let mut hits: Vec<usize> = symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| start(s) <= (line, col) && (line, col) <= end(s))
        .map(|(i, _)| i)
        .collect();
    hits.sort_by(|&a, &b| {
        start(&symbols[a])
            .cmp(&start(&symbols[b]))
            .then(end(&symbols[b]).cmp(&end(&symbols[a])))
    });
    hits
}

/// The symbols at `i`'s level under its parent, `i` among them, in outline
/// order (#1608): the outline is a pre-order walk, so they are the symbols
/// of `i`'s depth between the parent and the next symbol shallower than `i`.
pub(in crate::app) fn sibling_symbols(
    symbols: &[crate::lsp::manager::OutlineSymbol],
    i: usize,
) -> Vec<usize> {
    let Some(depth) = symbols.get(i).map(|s| s.depth) else {
        return Vec::new();
    };
    let start = (0..i)
        .rev()
        .find(|&k| symbols[k].depth < depth)
        .map_or(0, |parent| parent + 1);
    let end = (i + 1..symbols.len())
        .find(|&k| symbols[k].depth < depth)
        .unwrap_or(symbols.len());
    (start..end)
        .filter(|&k| symbols[k].depth == depth)
        .collect()
}
