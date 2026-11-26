//! Right-click menus (#2739): what a right-click lands on, and the menu
//! each of those things opens. [`App::right_click_target`] names the item
//! under the pointer; [`App::open_context_menu`] builds that item's menu at
//! a spot and gives its pane the keys. Kept apart so the keyboard can name
//! the item another way and open the very same menu.

use super::*;

/// The item a right-click opens a menu for, with what its menu is built
/// from.
pub(super) enum MenuTarget {
    /// The minimap strip: the Minimap toggle and its Side submenu.
    Minimap,
    /// A COMMITS graph row (#1627): the commit's actions.
    Commit(crate::git::GraphCommit),
    /// A PROBLEMS diagnostic row: Fix with Navigator (#374).
    Problem {
        path: PathBuf,
        item: crate::widgets::problems::ProblemItem,
    },
    /// A Run and Debug row, by its index (#1546). A row with no actions (a
    /// header, a scope) opens no menu, and nothing else is tried.
    DebugRow(usize),
    /// An OUTLINE row (#369): where its symbol is, and the symbol's name
    /// and first and last lines for its own tab.
    OutlineSymbol {
        path: PathBuf,
        line: u32,
        col: u32,
        name: String,
        first: usize,
        last: usize,
    },
    /// A CAPTURES row, by its index (#372).
    Capture(usize),
    /// An editor tab, by its index.
    Tab(usize),
    /// A terminal pane, or a folded pane's strip, by its index.
    Terminal(usize),
    /// The Explorer: a row's node (the sticky band's pinned folder where
    /// the band covers a row, #117), or `None` for the space below the rows.
    Explorer(Option<usize>),
    /// The editor gutter at a 0-based line: bookmarks and breakpoints.
    Gutter(usize),
    /// The editor's text at a buffer position: the symbol's actions, then
    /// the clipboard and the palette.
    Text { row: usize, col: usize },
}

impl App {
    /// What a right-click at `(col, row)` opens a menu for, trying the
    /// spots in an order that lets the one drawn on top win. `in_tree`,
    /// `in_editor` and `terminal_hit` are the mouse handler's own reading
    /// of the spot.
    pub(super) fn right_click_target(
        &self,
        col: u16,
        row: u16,
        in_tree: bool,
        in_editor: bool,
        terminal_hit: Option<usize>,
    ) -> Option<MenuTarget> {
        // Minimap strip right-click: the VS Code-style options menu
        // (Minimap toggle + Side submenu). Checked first so the strip
        // wins over the editor pane it sits beside.
        if rect_contains(self.minimap_img_rect, col, row) {
            return Some(MenuTarget::Minimap);
        }
        // A COMMITS graph row (#1627): the commit's actions.
        if self.show_tree
            && self.sidebar_view == SidebarView::SourceControl
            && rect_contains(self.commit_graph.last_area, col, row)
            && !rect_contains(self.commit_graph.last_scrollbar, col, row)
            && let Some(commit) = self.commit_graph.commit_at(row)
        {
            return Some(MenuTarget::Commit(commit.clone()));
        }
        // A PROBLEMS diagnostic row: Fix with Navigator (#374).
        if self.bottom_panel_tab == BottomPanelTab::Problems
            && rect_contains(self.problems.last_area, col, row)
            && let Some((path, item)) = self.problems.diagnostic_at(row)
        {
            return Some(MenuTarget::Problem { path, item });
        }
        // A Run and Debug row: copy a variable's value or
        // expression, watch it, or copy the call stack (#1546).
        if in_tree
            && self.sidebar_view == SidebarView::RunDebug
            && let Some(idx) = self.run_debug.debug_row_at(row)
        {
            return Some(MenuTarget::DebugRow(idx));
        }
        // An OUTLINE row: go to the symbol, or open it as its own
        // tab (#369).
        if rect_contains(self.outline.last_area, col, row)
            && let Some(idx) = self.outline.row_at(row)
            && let Some((path, line, col)) = self.outline.jump_target(idx)
            && let Some(sym) = self.outline.symbols().get(idx)
        {
            return Some(MenuTarget::OutlineSymbol {
                path,
                line,
                col,
                name: sym.name.clone(),
                first: sym.range_start_line as usize,
                last: sym.range_end_line as usize,
            });
        }
        // A CAPTURES row (#372).
        if self.bottom_panel_tab == BottomPanelTab::Captures
            && rect_contains(self.captures.last_area, col, row)
            && let Some(idx) = self.captures.row_at(col, row)
        {
            return Some(MenuTarget::Capture(idx));
        }
        // Editor tab strip: right-click on a tab opens the
        // Close / Close Others / Close to the Right / Close All
        // menu. Routed here BEFORE the explorer branch so the
        // hit-test wins even when the editor pane shares its
        // column with the sidebar.
        if let Some(tab_idx) = self.editor.tab_at(col, row) {
            return Some(MenuTarget::Tab(tab_idx));
        }
        // A folded pane clears its `last_area`, so `terminal_at_pos`
        // can never name one and the menu's "Expand Terminal" branch
        // was unreachable while KEYBINDINGS.md advertised it (#313).
        // Fall back to the strips so a folded pane has a second
        // gesture rather than left-click alone.
        let terminal_hit = terminal_hit.or_else(|| {
            self.terminal_strip_rects
                .iter()
                .position(|r| rect_contains(*r, col, row))
        });
        if self.bottom_panel_tab == BottomPanelTab::Terminal
            && let Some(idx) = terminal_hit
        {
            return Some(MenuTarget::Terminal(idx));
        }
        if in_tree && self.sidebar_view == SidebarView::Explorer {
            // A right-click on the sticky band context-menus the
            // PINNED directory, not the covered row (#117): the band
            // hit wins the row resolution and the menu flow proceeds
            // for that node.
            let node_idx = self
                .tree
                .sticky_row_at(row)
                .or_else(|| self.tree.node_at_y(row));
            return Some(MenuTarget::Explorer(node_idx));
        }
        // Editor gutter: right-click in the glyph margin / line-number
        // column opens the breakpoint menu (Add / Remove Breakpoint,
        // Add Conditional / Edit Condition), mirroring VS Code's
        // glyph-margin context menu. Routed before the body branch so a
        // gutter click never falls through to the symbol menu. Diff /
        // sheet / image tabs have no debuggable source lines.
        if in_editor
            && self.editor.diff.is_none()
            && self.editor.sheet.is_none()
            && self.editor.image.is_none()
            && let Some(line0) = self.editor.gutter_line_at(col, row)
        {
            return Some(MenuTarget::Gutter(line0));
        }
        // Editor body: right-click over a text buffer opens the symbol
        // menu (Go to Definition / Rename Symbol / Change All
        // Occurrences). Read-only diff / sheet / image tabs have no
        // symbols, so they're excluded.
        if in_editor
            && self.editor.diff.is_none()
            && self.editor.sheet.is_none()
            && self.editor.image.is_none()
            && let Some((row, col)) = self.editor.buffer_pos_at(col, row)
        {
            return Some(MenuTarget::Text { row, col });
        }
        None
    }

    /// Open `target`'s menu with its top-left at `at`, giving the keys to
    /// the pane it is in and picking the item where a right-click picks it
    /// (an Explorer row, a capture, a terminal pane).
    pub(super) fn open_context_menu(&mut self, target: MenuTarget, at: (u16, u16)) {
        match target {
            MenuTarget::Minimap => self.open_minimap_menu(at.0, at.1),
            MenuTarget::Commit(commit) => {
                let menu = self.commit_graph_menu(at, &commit);
                self.focus_pane(Pane::Tree);
                self.context_menu = Some(menu);
            }
            // Offered whether or not a navigator is seated, like the
            // CAPTURES ask: the menu model has no disabled state, and a
            // refusal that says why beats a greyed entry.
            MenuTarget::Problem { path, item } => {
                self.context_menu = Some(ContextMenu::flat(
                    at,
                    vec![(
                        String::from("Fix with Navigator"),
                        MenuAction::FixProblemWithNavigator { path, item },
                    )],
                    self.tree.root.clone(),
                ));
            }
            MenuTarget::DebugRow(idx) => {
                let items = self.debug_row_menu(idx);
                if !items.is_empty() {
                    self.context_menu = Some(ContextMenu::flat(at, items, self.tree.root.clone()));
                }
            }
            MenuTarget::OutlineSymbol {
                path,
                line,
                col,
                name,
                first,
                last,
            } => {
                self.context_menu = Some(ContextMenu::flat(
                    at,
                    vec![
                        (
                            String::from("Go to Symbol"),
                            MenuAction::GoToOutlineSymbol {
                                path: path.clone(),
                                line,
                                col,
                            },
                        ),
                        (
                            String::from("Open Symbol in Its Own Tab"),
                            MenuAction::OpenSymbolTab {
                                path,
                                name,
                                first,
                                last,
                            },
                        ),
                    ],
                    self.tree.root.clone(),
                ));
            }
            // The ask is offered whether or not a navigator is seated, and
            // says so if none is (#372).
            MenuTarget::Capture(idx) => {
                self.captures.select_index(idx);
                self.context_menu = Some(ContextMenu::flat(
                    at,
                    vec![(
                        String::from("Ask Navigator About This Line"),
                        MenuAction::AskNavigatorAboutCapture,
                    )],
                    self.tree.root.clone(),
                ));
            }
            MenuTarget::Tab(tab_idx) => {
                self.focus_pane(Pane::Editor);
                let tab_path = self.editor.tab_path(tab_idx);
                let items = build_tab_context_menu_items(
                    tab_idx,
                    self.editor.tab_count(),
                    self.editor_layout.is_split(),
                    tab_path.as_deref(),
                    !self.is_remote && cfg!(target_os = "macos"),
                    self.editor.is_preview(tab_idx),
                    self.editor.is_pinned(tab_idx),
                );
                self.context_menu = Some(ContextMenu {
                    crumb: None,
                    scroll: Default::default(),
                    submenu_scroll: Default::default(),
                    look: MenuLook::Panel,
                    keyed: true,
                    origin: at,
                    items,
                    selected: 0,
                    open_submenu: None,
                    submenu_selected: 0,
                    target_dir: self.tree.root.clone(),
                });
            }
            MenuTarget::Terminal(idx) => self.open_terminal_menu(idx, at),
            MenuTarget::Explorer(node_idx) => self.open_explorer_menu(node_idx, at),
            MenuTarget::Gutter(line0) => self.open_gutter_menu(line0, at),
            MenuTarget::Text { row, col } => self.open_text_menu(row, col, at),
        }
    }

    /// Terminal pane `idx`'s menu: the `…`-overflow equivalent. Rename and
    /// Clear (the gaps vs VS Code's toolbar); split/close already have
    /// buttons and chords.
    fn open_terminal_menu(&mut self, idx: usize, at: (u16, u16)) {
        self.active_terminal = idx;
        self.focus_pane(Pane::Terminal);
        // A folded pane cannot BE the active one: `focus_pane`
        // runs `sync_focus_flags`, whose invariant walks focus to
        // the nearest expanded neighbour rather than leave it on
        // a pane with nothing painted. Entries carrying `idx` are
        // unaffected, but the ones that read `active_terminal`
        // would act on that neighbour while this menu names THIS
        // pane - and the broadcast entry read its label from
        // `idx` while toggling the neighbour, so it could say
        // "Exclude from Broadcast" and include a different pane.
        // Each of them is also a gesture on a grid the user
        // cannot see while the pane is a strip.
        let folded = self.terminals.get(idx).is_some_and(|t| t.collapsed);
        let mut items: Vec<MenuEntry> = Vec::new();
        // Copy and Paste lead the menu, as they do in VS Code's
        // terminal. Both act on the ACTIVE pane, so a folded one
        // offers neither: focus has already walked to an expanded
        // neighbour, and pasting into a grid the user cannot see
        // is worse than not offering the entry at all.
        if !folded {
            // Copy appears only while there is something to copy.
            // The menu model has no disabled state, and an entry
            // that silently copies nothing is worse than no entry.
            if self.terminals[idx]
                .selection()
                .is_some_and(|s| s.has_area())
            {
                items.push(MenuEntry::Item {
                    label: String::from("Copy"),
                    action: MenuAction::TerminalCopySelection,
                });
            }
            items.push(MenuEntry::Item {
                label: String::from("Paste"),
                action: MenuAction::TerminalPaste,
            });
        }
        items.push(MenuEntry::Item {
            label: String::from("Rename Terminal"),
            action: MenuAction::RenameTerminal(idx),
        });
        items.push(MenuEntry::Item {
            label: String::from("Clear"),
            action: MenuAction::ClearTerminal(idx),
        });
        // Split and Kill, as in VS Code's terminal menu (#1598):
        // GNOME Terminal and Konsole keep Ctrl+Shift+T / W for
        // their own New Tab and Close Tab, so off the kitty
        // protocol this menu is the keyboard-free route. Split
        // opens beside the ACTIVE pane, so a folded one offers
        // only Kill, which carries its own index; a lone pane
        // cannot be closed, so it offers no Kill.
        if !folded {
            items.push(MenuEntry::Item {
                label: String::from("Split Terminal"),
                action: MenuAction::SplitTerminal,
            });
        }
        if self.terminals.len() > 1 {
            items.push(MenuEntry::Item {
                label: String::from("Kill Terminal"),
                action: MenuAction::KillTerminal(idx),
            });
        }
        if !folded {
            items.extend([
                MenuEntry::Item {
                    label: String::from("Quick Select"),
                    action: MenuAction::TerminalQuickSelect,
                },
                MenuEntry::Item {
                    label: String::from("Copy Mode"),
                    action: MenuAction::TerminalCopyMode,
                },
                MenuEntry::Item {
                    label: String::from("Command History"),
                    action: MenuAction::TerminalCommandHistory,
                },
                MenuEntry::Item {
                    label: String::from("Open Scrollback in Editor"),
                    action: MenuAction::OpenScrollbackInEditor,
                },
            ]);
        }
        // Undo close only appears while a parked pane is alive
        // to restore (the grace window).
        if !self.closed_terminals.is_empty() {
            items.push(MenuEntry::Item {
                label: String::from("Reopen Closed Terminal"),
                action: MenuAction::UndoCloseTerminal,
            });
        }
        // Maximize and broadcast both need a second pane, so a
        // lone terminal doesn't offer them.
        if self.terminals.len() > 1 {
            items.push(MenuEntry::Item {
                label: String::from(if self.terminal_pane_maximized {
                    "Restore Terminal Split"
                } else {
                    "Maximize Terminal"
                }),
                action: MenuAction::ToggleMaximizeTerminal(idx),
            });
            // Collapse is meaningless while maximize owns the
            // whole panel and the flags are ignored, so the entry
            // is not offered there.
            if !self.terminal_pane_maximized {
                items.push(MenuEntry::Item {
                    label: String::from(if self.terminals[idx].collapsed {
                        "Expand Terminal"
                    } else {
                        "Collapse Terminal"
                    }),
                    action: MenuAction::ToggleCollapseTerminal(idx),
                });
            }
            items.push(MenuEntry::Item {
                label: String::from(if self.broadcast_input {
                    "Stop Broadcast Input"
                } else {
                    "Broadcast Input"
                }),
                action: MenuAction::ToggleBroadcastInput,
            });
            if self.broadcast_input && !folded {
                items.push(MenuEntry::Item {
                    label: String::from(if self.terminals[idx].broadcast_excluded {
                        "Include in Broadcast"
                    } else {
                        "Exclude from Broadcast"
                    }),
                    action: MenuAction::ToggleBroadcastExclusion,
                });
            }
        }
        self.context_menu = Some(ContextMenu {
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            look: MenuLook::Panel,
            keyed: true,
            origin: at,
            items,
            selected: 0,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.tree.root.clone(),
        });
    }

    /// The Explorer's menu for `node_idx`'s row, or the workspace's for
    /// `None`. A right-click on a row that is not marked picks it alone; on
    /// a marked one it keeps the marks, so the menu acts on all of them.
    fn open_explorer_menu(&mut self, node_idx: Option<usize>, at: (u16, u16)) {
        self.focus_pane(Pane::Tree);
        if let Some(idx) = node_idx {
            let path_clicked = self.tree.nodes[idx].path.clone();
            let already_marked = self.tree.marked.contains(&path_clicked);
            if already_marked {
                self.tree.selected = idx;
            } else {
                self.tree.select_replace(idx);
            }
        }
        let node = node_idx.and_then(|i| self.tree.nodes.get(i));
        let target_dir = crate::widgets::file_tree::create_target_dir_for(node, &self.tree.root);
        let selection = self.tree.action_paths();
        let mut items = self.tree_context_menu_items(node, &selection, &target_dir);
        let clicked = node.map(|n| n.path.clone());
        maybe_add_reveal_in_finder(
            &mut items,
            clicked.as_deref(),
            !self.is_remote && cfg!(target_os = "macos"),
        );
        self.context_menu = Some(ContextMenu::flat(at, items, target_dir));
    }

    /// The gutter's menu for 0-based line `line0`: bookmarks, then the
    /// line's breakpoint, condition and logpoint, then the navigator's ask.
    fn open_gutter_menu(&mut self, line0: usize, at: (u16, u16)) {
        self.focus_pane(Pane::Editor);
        let line = line0 + 1; // 1-based, as breakpoints are stored
        let (has_bp, has_cond, has_log) = self
            .editor
            .path
            .as_ref()
            .map(|p| {
                let has_bp = self
                    .editor
                    .breakpoints
                    .get(p)
                    .is_some_and(|s| s.contains(&line));
                let has_cond = self
                    .editor
                    .breakpoint_conditions
                    .get(p)
                    .is_some_and(|c| c.contains_key(&line));
                let has_log = self
                    .editor
                    .breakpoint_logs
                    .get(p)
                    .is_some_and(|m| m.contains_key(&line));
                (has_bp, has_cond, has_log)
            })
            .unwrap_or((false, false, false));
        let toggle_label = if has_bp {
            String::from("Remove Breakpoint")
        } else {
            String::from("Add Breakpoint")
        };
        let cond_label = if has_cond {
            String::from("Edit Condition\u{2026}")
        } else {
            String::from("Add Conditional Breakpoint\u{2026}")
        };
        let log_label = if has_log {
            String::from("Edit Logpoint")
        } else {
            String::from("Add Logpoint")
        };
        let mut items = vec![
            // Bookmarks sit above the breakpoint group: same gutter,
            // different tool, and the Bookmarks extension's own
            // labels so the chords read the same as in VS Code.
            (
                String::from("Toggle Bookmark"),
                MenuAction::ToggleBookmarkAt { line },
            ),
            (String::from("Next Bookmark"), MenuAction::NextBookmark),
            (
                String::from("Previous Bookmark"),
                MenuAction::PreviousBookmark,
            ),
            (toggle_label, MenuAction::ToggleBreakpointAt { line }),
            (cond_label, MenuAction::EditBreakpointConditionAt { line }),
            (log_label, MenuAction::EditLogpointAt { line }),
        ];
        // The resident navigator's line-scoped ask, only while
        // one is seated (`line` here is 1-based, the box 0-based).
        if self.pair_host.is_some() {
            items.push((
                String::from("Ask Navigator"),
                MenuAction::AskNavigatorAt {
                    line: line.saturating_sub(1),
                },
            ));
        }
        self.context_menu = Some(ContextMenu::flat(at, items, self.tree.root.clone()));
    }

    /// The text's menu at buffer position `(row, col)`.
    fn open_text_menu(&mut self, row: usize, col: usize, at: (u16, u16)) {
        self.focus_pane(Pane::Editor);
        let mut items = vec![(
            String::from("Go to Definition"),
            MenuAction::GoToDefinitionAt { row, col },
        )];
        // Only offer "Go to Declaration" when this file's language
        // server actually implements `textDocument/declaration`.
        // TypeScript's vtsls advertises `declarationProvider: false`,
        // so the row is hidden there, exactly as VS Code does.
        if self.editor_language_supports_declaration() {
            items.push((
                String::from("Go to Declaration"),
                MenuAction::GoToDeclarationAt { row, col },
            ));
        }
        // "Go to Type Definition" is offered only when the server
        // implements `textDocument/typeDefinition` (vtsls,
        // basedpyright, rust-analyzer and gopls all do), mirroring
        // VS Code's ordering right after Go to Declaration.
        if self.editor_language_supports_type_definition() {
            items.push((
                String::from("Go to Type Definition"),
                MenuAction::GoToTypeDefinitionAt { row, col },
            ));
        }
        // "Go to Implementations" is offered only when the server
        // implements `textDocument/implementation` (rust-analyzer,
        // gopls, vtsls do), placed right after Type Definition to
        // mirror VS Code's Go To submenu order.
        if self.editor_language_supports_implementation() {
            items.push((
                String::from("Go to Implementations"),
                MenuAction::GoToImplementationAt { row, col },
            ));
        }
        // "Go to References" is offered only when the server
        // implements `textDocument/references` (every server croft
        // ships does), placed last in the Go To group to mirror VS
        // Code's submenu order.
        if self.editor_language_supports_references() {
            items.push((
                String::from("Go to References"),
                MenuAction::GoToReferencesAt { row, col },
            ));
        }
        // Call hierarchy, one level per invocation: who calls
        // this (incoming) and what does this call (outgoing).
        if self.editor_language_supports_call_hierarchy() {
            items.push((
                String::from("Show Incoming Calls"),
                MenuAction::ShowCallsAt {
                    row,
                    col,
                    incoming: true,
                },
            ));
            items.push((
                String::from("Show Outgoing Calls"),
                MenuAction::ShowCallsAt {
                    row,
                    col,
                    incoming: false,
                },
            ));
        }
        items.push((String::from("Go Back"), MenuAction::NavigateBack));
        items.push((String::from("Go Forward"), MenuAction::NavigateForward));
        items.push((
            String::from("Rename Symbol"),
            MenuAction::RenameSymbolAt { row, col },
        ));
        items.push((
            String::from("Change All Occurrences"),
            MenuAction::ChangeAllOccurrencesAt { row, col },
        ));
        // Sharing the lines (#1610), for a file on disk; why a
        // link cannot be made is said when it is asked for.
        if self.editor.path.is_some() {
            items.push((String::from("Copy Permalink"), MenuAction::CopyPermalink));
            items.push((String::from("Open on Remote"), MenuAction::OpenOnRemote));
        }
        if self.editor_language_supports_code_action() {
            items.push((
                String::from("Quick Fix"),
                MenuAction::QuickFixAt { row, col },
            ));
        }
        // The navigator's selection-scoped ask (GitHub-review
        // style), only while one is seated and lines are selected.
        if self.pair_host.is_some()
            && let Some((start, end)) = self.editor.selection_rows()
        {
            items.push((
                String::from("Ask Navigator About Selection"),
                MenuAction::AskNavigatorSelection { start, end },
            ));
        }
        let mut menu = ContextMenu::flat(at, items, self.tree.root.clone());
        // Then the clipboard and the palette, as VS Code's editor
        // menu ends (#1117): the way to copy where the keys are
        // awkward (Linux terminals, the `croft web` canvas).
        // Cut and Paste edit the buffer, so only an editable text
        // view offers them: a rendered preview hides its source
        // and swallows those keys too. Copy takes the selection
        // the view shows.
        let editable = self.editor_is_text();
        let mut clipboard = Vec::new();
        if editable && self.editor.selection.is_some_and(|s| s.has_area()) {
            clipboard.push(MenuEntry::item("Cut", MenuAction::EditorCut));
        }
        if self.editor_view_has_selection() {
            clipboard.push(MenuEntry::item("Copy", MenuAction::EditorCopy));
        }
        if editable {
            clipboard.push(MenuEntry::item("Paste", MenuAction::EditorPaste));
        }
        if !clipboard.is_empty() {
            menu.items.push(MenuEntry::Separator);
            menu.items.extend(clipboard);
        }
        menu.items.push(MenuEntry::Separator);
        menu.items.push(MenuEntry::item(
            "Command Palette",
            MenuAction::ShowCommandPalette,
        ));
        self.context_menu = Some(menu);
    }
}
