//! Right-click and dropdown menus: shortcut hints, ContextMenu methods, geometry, keys,
//! drawing, `dispatch_menu_action`, and the status-bar click menus.

use super::*;

/// Return the macOS-style keyboard shortcut hint to display on the right
/// side of a context-menu row, when one exists. Mirrors the bindings in
/// `handle_tree_key` / `handle_explorer_shortcut` so the menu is a
/// discovery surface for the same accelerators the keyboard accepts.
/// [`platform_shortcut_for`] spells it for the platform croft runs on.
pub(super) fn shortcut_for(action: &MenuAction) -> Option<&'static str> {
    match action {
        MenuAction::Create(CreateKind::File) => Some("⌘F"),
        MenuAction::Create(CreateKind::Folder) => Some("⌘⇧N"),
        MenuAction::Cut(_) => Some("⌘X"),
        MenuAction::Copy(_) => Some("⌘C"),
        MenuAction::Paste(_) => Some("⌘V"),
        MenuAction::Rename(_) => Some("⌘R"),
        MenuAction::MakeRoot(_) => Some("⌘/"),
        MenuAction::AddWorkspaceFolder => None,
        MenuAction::RemoveWorkspaceFolder(_) => None,
        MenuAction::RevealInFinder(_) => Some("⌥⌘R"),
        MenuAction::CopyTabPath(_) => Some("⌥⌘C"),
        MenuAction::CopyTabRelativePath(_) => Some("⌥⇧⌘C"),
        MenuAction::RevealInExplorer(_) => Some("⌘K E"),
        MenuAction::Delete { .. } => Some("⌫"),
        MenuAction::CloseTab(_) => Some("⌘W"),
        MenuAction::CloseOtherTabs(_) => Some("⌘K ⇧X"),
        MenuAction::CloseAllTabs => Some("⌘K W"),
        MenuAction::CloseSavedTabs => Some("⌘K U"),
        MenuAction::KeepTabOpen(_) => Some("⌘K ⇧P"),
        MenuAction::ToggleTabPin(_) => Some("⌘K P"),
        MenuAction::SplitEditor => Some("⌘\\"),
        MenuAction::SplitEditorUp => Some("⌘K ⌘\\"),
        MenuAction::SplitInGroup => Some("⌘K ⇧⌘\\"),
        MenuAction::FocusGroupLeft => Some("⌥⌘←"),
        MenuAction::FocusGroupRight => Some("⌥⌘→"),
        MenuAction::CopyIntoNewWindow(_) => Some("⌘K O"),
        MenuAction::MoveIntoNewWindow(_) => Some("⌘K ⇧O"),
        MenuAction::RenameSymbolAt { .. } => Some("F2"),
        MenuAction::ChangeAllOccurrencesAt { .. } => Some("⌘F2"),
        MenuAction::GoToDefinitionAt { .. } => Some("F12"),
        MenuAction::GoToReferencesAt { .. } => Some("⇧F12"),
        MenuAction::NavigateBack => Some("⌃-"),
        MenuAction::NavigateForward => Some("⌃⇧-"),
        MenuAction::QuickFixAt { .. } => Some("⌘."),
        MenuAction::GoToDeclarationAt { .. } => Some("⌃⇧F12"),
        MenuAction::GoToTypeDefinitionAt { .. } => Some("⌃F12"),
        MenuAction::GoToImplementationAt { .. } => Some("⌘F12"),
        MenuAction::ToggleBreakpointAt { .. } => Some("F9"),
        MenuAction::EditBreakpointConditionAt { .. } => Some("⇧F9"),
        MenuAction::EditLogpointAt { .. } => Some("⇧⌥F9"),
        MenuAction::ToggleBookmarkAt { .. } => Some("⌃⌥⇧K"),
        MenuAction::NextBookmark => Some("⌃⌥."),
        MenuAction::PreviousBookmark => Some("⌃⌥,"),
        MenuAction::AskNavigatorAt { .. } => Some("⌘K Q"),
        MenuAction::AskNavigatorSelection { .. } => Some("⌘K Q"),
        MenuAction::AskNavigatorAboutCapture => Some("n"),
        MenuAction::FixProblemWithNavigator { .. } => None,
        MenuAction::CopyDebugValue { .. }
        | MenuAction::CopyDebugExpression(_)
        | MenuAction::AddDebugWatch(_)
        | MenuAction::RemoveDebugWatch(_)
        | MenuAction::CopyCallStack => None,
        MenuAction::OpenSymbolTab { .. } => Some("⌘K ⇧V"),
        MenuAction::GoToOutlineSymbol { .. } => Some("Enter"),
        MenuAction::ShowCallsAt { incoming: true, .. } => Some("⌘K H"),
        MenuAction::ShowCallsAt {
            incoming: false, ..
        } => Some("⌘K ⇧H"),
        MenuAction::OpenThemePicker => Some("⌘K ⌘T"),
        MenuAction::ToggleSideBar => Some("⌘B"),
        MenuAction::ToggleSecondarySideBar => Some("⌥⌘B"),
        MenuAction::TogglePanel => Some("⌃J"),
        MenuAction::ToggleMinimap => Some("⌥⌘M"),
        MenuAction::ToggleZenMode => Some("⌘K Z"),
        MenuAction::CloseTabsToRight(_) => Some("⌘K →"),
        MenuAction::SelectForCompare(_) => Some("⌘K S"),
        MenuAction::CompareWithSelected { .. } => Some("⌘K C"),
        MenuAction::RenameTerminal(_) => Some("⌘K R"),
        MenuAction::ClearTerminal(_) => Some("⌘K K"),
        MenuAction::ToggleMaximizeTerminal(_) => Some("⌘K M"),
        // No hint. The label alternates "Collapse Terminal" / "Expand
        // Terminal", but `Cmd+K [` only ever COLLAPSES: it acts on
        // `active_terminal`, which the focus invariant guarantees is never a
        // folded pane, so the expand branch is unreachable from the chord. Its
        // sibling `ToggleMaximizeTerminal` can carry `⌘K M` honestly because
        // that chord really does toggle both ways. Advertising a shortcut
        // beside a label it cannot perform is worse than showing none.
        MenuAction::ToggleCollapseTerminal(_) => None,
        MenuAction::TerminalCopySelection => Some("⌘C"),
        MenuAction::TerminalPaste => Some("⌘V"),
        MenuAction::EditorCut => Some("⌘X"),
        MenuAction::EditorCopy => Some("⌘C"),
        MenuAction::EditorPaste => Some("⌘V"),
        MenuAction::ShowCommandPalette => Some("⌘⇧P"),
        MenuAction::TerminalQuickSelect => Some("⌃⇧Space"),
        MenuAction::TerminalCopyMode => Some("⌃⇧Y"),
        MenuAction::TerminalCommandHistory => Some("⌃⇧H"),
        MenuAction::OpenScrollbackInEditor => Some("⌘K D"),
        MenuAction::ToggleBroadcastInput => Some("⌘K I"),
        MenuAction::ToggleBroadcastExclusion => Some("⌘K ⇧I"),
        MenuAction::UndoCloseTerminal => Some("⌘K ⇧T"),
        MenuAction::SplitTerminal => Some("⌘T"),
        MenuAction::KillTerminal(_) => Some("⌘W"),
        MenuAction::CopyCommandOutput(..) => Some("⌘K ⇧C"),
        MenuAction::SelectCommandOutput(..) => Some("⌘K ⇧S"),
        MenuAction::RerunCommand(..) => Some("⌘K ⇧R"),
        // The status bar's indentation menu (#2643): the chords the palette's
        // Convert Indentation to Spaces / Tabs carry.
        MenuAction::ConvertIndentation(true) => Some("⌥⇧⌘S"),
        MenuAction::ConvertIndentation(false) => Some("⌥⇧⌘T"),
        _ => None,
    }
}

/// The command a menu row runs, when it is one (#1562), so the row can show
/// the chord a user bound to that command.
pub(super) fn menu_action_command(
    action: &MenuAction,
) -> Option<crate::widgets::command_palette::Command> {
    use crate::widgets::command_palette::Command as Cmd;
    Some(match action {
        MenuAction::ClearTerminal(_) => Cmd::TerminalClear,
        MenuAction::RenameTerminal(_) => Cmd::TerminalRename,
        MenuAction::TerminalQuickSelect => Cmd::TerminalQuickSelect,
        MenuAction::TerminalCopyMode => Cmd::TerminalCopyMode,
        MenuAction::TerminalCommandHistory => Cmd::TerminalCommandHistory,
        MenuAction::OpenScrollbackInEditor => Cmd::TerminalOpenScrollback,
        MenuAction::CloseOtherTabs(_) => Cmd::CloseOtherEditors,
        _ => return None,
    })
}

/// [`shortcut_for`] as a Mac (`macos`) or anything else spells it (#885):
/// the glyphs on a Mac, elsewhere `Ctrl`, `Alt` and `Super` as
/// [`glyph_hint_for_platform`](crate::widgets::command_palette::glyph_hint_for_platform)
/// writes them. Off macOS the terminal pane gives `Ctrl+C`, `Ctrl+V` and
/// `Ctrl+W` to the shell, so its Copy, Paste and Kill rows name the
/// `Ctrl+Shift` chords it takes instead (docs/LINUX.md).
pub(super) fn platform_shortcut_for(
    action: &MenuAction,
    macos: bool,
) -> Option<std::borrow::Cow<'static, str>> {
    let glyphs = shortcut_for(action)?;
    let in_the_terminal = match action {
        MenuAction::TerminalCopySelection => Some("Ctrl+Shift+C"),
        MenuAction::TerminalPaste => Some("Ctrl+Shift+V"),
        MenuAction::KillTerminal(_) => Some("Ctrl+Shift+W"),
        _ => None,
    };
    match in_the_terminal {
        Some(chord) if !macos => Some(std::borrow::Cow::Borrowed(chord)),
        _ => Some(crate::widgets::command_palette::glyph_hint_for_platform(
            glyphs, macos,
        )),
    }
}

/// The hint on a menu row's right: the user's own chord for the command the
/// row runs, else the built-in one as this platform spells it.
pub(super) fn menu_hint(action: &MenuAction, keymap: &crate::keymap::Keymap) -> Option<String> {
    menu_action_command(action)
        .and_then(|cmd| keymap.chord_for(cmd))
        .or_else(|| {
            platform_shortcut_for(action, cfg!(target_os = "macos")).map(|s| s.into_owned())
        })
}

/// Display width (in cells) a menu entry needs: its label plus the right-aligned
/// shortcut hint (items) or the `▸` submenu indicator. Dividers need none.
/// `keycaps` measures the hint as a `surfaces` theme draws it, each key in
/// a chip one cell of padding wider on each side.
pub(super) fn menu_entry_width(
    entry: &MenuEntry,
    keymap: &crate::keymap::Keymap,
    keycaps: bool,
) -> usize {
    match entry {
        MenuEntry::Item { label, action } => {
            let shortcut = menu_hint(action, keymap)
                .map(|s| {
                    let chips = if keycaps {
                        s.split(' ')
                            .map(|chord| crate::widgets::surface::chord_keys(chord).len())
                            .sum::<usize>()
                    } else {
                        0
                    };
                    s.chars().count() + 2 * chips + 2
                })
                .unwrap_or(0);
            label.chars().count() + shortcut
        }
        MenuEntry::Submenu { label, .. } => label.chars().count() + 2,
        MenuEntry::Header(label) => label.chars().count(),
        MenuEntry::Separator => 0,
    }
}

/// One row of a context menu: a clickable action, a parent that opens a nested
/// submenu, or a divider. Most menus are a flat list of `Item`s; the editor tab
/// menu uses a `Submenu` ("Split & Move") whose children include `Separator`s.
#[derive(Clone, Debug)]
pub(super) enum MenuEntry {
    Item {
        label: String,
        action: MenuAction,
    },
    Submenu {
        label: String,
        items: Vec<MenuEntry>,
    },
    Separator,
    /// A dimmed, non-selectable section label (e.g. "Side Bar Position" in the
    /// Customize Layout popup). The cursor skips it like a `Separator`.
    Header(String),
}

impl MenuEntry {
    pub(super) fn item(label: impl Into<String>, action: MenuAction) -> Self {
        Self::Item {
            label: label.into(),
            action,
        }
    }

    pub(super) fn header(label: impl Into<String>) -> Self {
        Self::Header(label.into())
    }

    /// The action a click on this entry dispatches, if it is a plain item.
    pub(super) fn action(&self) -> Option<&MenuAction> {
        match self {
            Self::Item { action, .. } => Some(action),
            _ => None,
        }
    }

    /// True for a row the cursor may land on (items and submenu parents, not
    /// dividers or section headers).
    pub(super) fn is_selectable(&self) -> bool {
        !matches!(self, Self::Separator | Self::Header(_))
    }
}

impl From<(String, MenuAction)> for MenuEntry {
    fn from((label, action): (String, MenuAction)) -> Self {
        Self::Item { label, action }
    }
}

impl ContextMenu {
    /// A flat menu from `(label, action)` tuples (the common case).
    pub(super) fn flat(
        origin: (u16, u16),
        items: Vec<(String, MenuAction)>,
        target_dir: PathBuf,
    ) -> Self {
        Self {
            origin,
            items: items.into_iter().map(MenuEntry::from).collect(),
            selected: 0,
            open_submenu: None,
            submenu_selected: 0,
            target_dir,
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
        }
    }

    /// Translate every label through the UI catalog (#621). Idempotent: a
    /// translated label is not itself a key.
    pub(super) fn localize(&mut self) {
        fn walk(items: &mut [MenuEntry]) {
            for e in items {
                match e {
                    MenuEntry::Item { label, .. } | MenuEntry::Header(label) => {
                        if let std::borrow::Cow::Owned(t) = crate::i18n::tr(label) {
                            *label = t;
                        }
                    }
                    MenuEntry::Submenu { label, items } => {
                        if let std::borrow::Cow::Owned(t) = crate::i18n::tr(label) {
                            *label = t;
                        }
                        walk(items);
                    }
                    MenuEntry::Separator => {}
                }
            }
        }
        if crate::i18n::active() {
            walk(&mut self.items);
        }
    }

    /// Move the cursor one selectable row up/down within the active level and
    /// report whether it actually moved. Unlike [`move_cursor`](Self::move_cursor)
    /// the caller learns the difference between a real step and hitting an end
    /// (or a run of non-selectable rows), so a wheel notch that cannot move
    /// the highlight can pan the view instead of dragging it.
    pub(super) fn step_cursor(&mut self, down: bool) -> bool {
        let n = self.level_entries().len();
        let mut i = self.level_selected();
        while let Some(next) = if down {
            (i + 1 < n).then_some(i + 1)
        } else {
            i.checked_sub(1)
        } {
            i = next;
            if self.level_entries()[i].is_selectable() {
                self.set_level_selected(i);
                return true;
            }
        }
        false
    }

    /// The rows the open submenu's panel can paint, given its height.
    pub(super) fn submenu_visible(&self, height: u16) -> usize {
        height.saturating_sub(2) as usize
    }

    /// Scroll the open submenu's panel by `notches` rows (positive = down),
    /// clamped so its last page is not scrolled past. The submenu paints from
    /// its own top offset, so it pans independently of the parent panel.
    pub(super) fn scroll_submenu(&mut self, notches: i32, visible: usize) -> bool {
        let Some(parent) = self.open_submenu else {
            return false;
        };
        let len = match self.items.get(parent) {
            Some(MenuEntry::Submenu { items, .. }) => items.len(),
            _ => return false,
        };
        let max = len.saturating_sub(visible.max(1));
        let cur = self.submenu_scroll.get().min(max);
        let next = if notches >= 0 {
            cur.saturating_add(notches.unsigned_abs() as usize)
        } else {
            cur.saturating_sub(notches.unsigned_abs() as usize)
        }
        .min(max);
        if next == cur {
            return false;
        }
        self.submenu_scroll.set(next);
        true
    }

    /// Keep the submenu's view offset in range and the highlighted child on
    /// screen, given the rows the panel can paint. Mirrors
    /// `scroll_for` for the top-level panel.
    pub(super) fn submenu_scroll_for(&self, visible: usize) -> usize {
        let mut top = self.submenu_scroll.get();
        if visible > 0 {
            if self.submenu_selected < top {
                top = self.submenu_selected;
            } else if self.submenu_selected >= top + visible {
                top = self.submenu_selected + 1 - visible;
            }
            let len = self
                .open_submenu
                .and_then(|p| match self.items.get(p) {
                    Some(MenuEntry::Submenu { items, .. }) => Some(items.len()),
                    _ => None,
                })
                .unwrap_or(0);
            top = top.min(len.saturating_sub(visible));
        }
        self.submenu_scroll.set(top);
        top
    }

    /// Scroll the active level with the wheel.
    ///
    /// The wheel walks the highlight through the level exactly like Up/Down
    /// do, so the selection always stays visible and Enter still fires what
    /// the user sees. When the highlight cannot step any further (both ends,
    /// or nothing selectable below), the view offset itself moves — so a
    /// clipped panel still pans instead of swallowing the notch. visible is
    /// the row count the caller can paint at the active level.
    pub(super) fn wheel_active(&mut self, notches: i32, visible: usize) {
        if notches == 0 {
            return;
        }
        let down = notches > 0;
        let steps = notches.unsigned_abs() as usize;
        for _ in 0..steps {
            if self.step_cursor(down) {
                continue;
            }
            if self.open_submenu.is_some() {
                self.scroll_submenu(if down { 1 } else { -1 }, visible);
            } else {
                let max = self.items.len().saturating_sub(visible.max(1));
                let cur = self.scroll.get().min(max);
                let next = if down {
                    cur.saturating_add(1)
                } else {
                    cur.saturating_sub(1)
                }
                .min(max);
                self.scroll.set(next);
            }
        }
    }

    /// Page the active level by pages screens (positive = down), one
    /// PgDn/PgUp. The highlight jumps a whole visible page at a time — as in
    /// VS Code's menus — skipping non-selectable rows, and the panel offset
    /// follows it via scroll_for / submenu_scroll_for. A page of a level
    /// that fits entirely just puts the cursor on the first / last entry.
    pub(super) fn page_active(&mut self, pages: i32, visible: usize) {
        if pages == 0 {
            return;
        }
        let down = pages > 0;
        let entries = self.level_entries();
        let n = entries.len();
        if n == 0 {
            return;
        }
        // A page is at least one row even in a one-row-tall panel.
        let step = visible.max(1);
        let cur = self.level_selected().min(n - 1);
        // Walk step selectable rows in the requested direction; if the
        // level runs out first, land on its last / first selectable row.
        let mut idx = cur;
        let mut moved = 0;
        let mut scan = cur;
        while moved < step {
            let next = if down {
                if scan + 1 >= n {
                    break;
                }
                scan + 1
            } else {
                match scan.checked_sub(1) {
                    Some(v) => v,
                    None => break,
                }
            };
            scan = next;
            if entries[next].is_selectable() {
                idx = next;
                moved += 1;
            }
        }
        if moved == 0 {
            return;
        }
        self.set_level_selected(idx);
        // Keep the view offset in step with the highlight so the panel pages
        // with it rather than leaving the cursor off screen.
        if self.open_submenu.is_some() {
            self.submenu_scroll_for(visible);
        } else {
            self.scroll_for(visible);
        }
    }

    /// Jump the active level's highlight to its first / last selectable row
    /// (Home / End), skipping headers and dividers. The panel offset follows
    /// through scroll_for / submenu_scroll_for.
    pub(super) fn jump_active(&mut self, to_end: bool, visible: usize) {
        let n = self.level_entries().len();
        if n == 0 {
            return;
        }
        let found = if to_end {
            (0..n)
                .rev()
                .find(|&i| self.level_entries()[i].is_selectable())
        } else {
            (0..n).find(|&i| self.level_entries()[i].is_selectable())
        };
        let Some(idx) = found else {
            return;
        };
        self.set_level_selected(idx);
        if self.open_submenu.is_some() {
            self.submenu_scroll_for(visible);
        } else {
            self.scroll_for(visible);
        }
    }

    /// The first top-level row shown when visible rows fit: the previous
    /// offset, moved only as far as keeps selected on screen.
    pub(super) fn scroll_for(&self, visible: usize) -> usize {
        let mut top = self.scroll.get();
        if visible > 0 {
            if self.selected < top {
                top = self.selected;
            } else if self.selected >= top + visible {
                top = self.selected + 1 - visible;
            }
            top = top.min(self.items.len().saturating_sub(visible));
        }
        self.scroll.set(top);
        top
    }

    /// The entries at the active level: the open submenu's children, or the
    /// top-level items.
    pub(super) fn level_entries(&self) -> &[MenuEntry] {
        match self.open_submenu {
            Some(i) => match &self.items[i] {
                MenuEntry::Submenu { items, .. } => items,
                _ => &self.items,
            },
            None => &self.items,
        }
    }

    pub(super) fn level_selected(&self) -> usize {
        if self.open_submenu.is_some() {
            self.submenu_selected
        } else {
            self.selected
        }
    }

    pub(super) fn set_level_selected(&mut self, idx: usize) {
        if self.open_submenu.is_some() {
            self.submenu_selected = idx;
        } else {
            self.selected = idx;
        }
    }

    /// Move the cursor up/down within the active level, skipping dividers and
    /// stopping at the ends.
    pub(super) fn move_cursor(&mut self, down: bool) {
        let entries = self.level_entries();
        let n = entries.len();
        let mut i = self.level_selected();
        loop {
            i = if down {
                if i + 1 >= n {
                    return;
                }
                i + 1
            } else {
                if i == 0 {
                    return;
                }
                i - 1
            };
            if entries[i].is_selectable() {
                self.set_level_selected(i);
                return;
            }
        }
    }

    /// The entry currently under the cursor at the active level.
    pub(super) fn cursor_entry(&self) -> Option<&MenuEntry> {
        self.level_entries().get(self.level_selected())
    }

    /// The action a cursor-on-item Enter/click dispatches (None on a submenu
    /// parent or divider).
    pub(super) fn cursor_action(&self) -> Option<MenuAction> {
        match self.cursor_entry() {
            Some(MenuEntry::Item { action, .. }) => Some(action.clone()),
            _ => None,
        }
    }

    /// If the top-level cursor is on a submenu parent (and none is open yet),
    /// open it, selecting its first selectable child. Returns true when opened.
    pub(super) fn open_cursor_submenu(&mut self) -> bool {
        if self.open_submenu.is_some() {
            return false;
        }
        if let Some(MenuEntry::Submenu { items, .. }) = self.items.get(self.selected) {
            self.submenu_selected = items.iter().position(MenuEntry::is_selectable).unwrap_or(0);
            self.open_submenu = Some(self.selected);
            return true;
        }
        false
    }

    /// Close any open submenu, returning to the top level. Returns true when a
    /// submenu was actually open.
    pub(super) fn close_submenu(&mut self) -> bool {
        if self.open_submenu.take().is_some() {
            self.submenu_selected = 0;
            return true;
        }
        false
    }
}

impl App {
    pub(super) fn render_context_menu(&self, frame: &mut ratatui::Frame) {
        let Some(menu) = &self.context_menu else {
            return;
        };
        // `menu_rect` already clamps against `last_frame_area`, so the
        // rect we draw here is the same rect `menu_item_at` hit-tests
        // against. Keeping the two in lock-step is what prevents the
        // off-by-N row dispatch when the menu has to shift up to fit.
        let Some(clipped) = self.menu_rect() else {
            return;
        };
        let grad = self.popup_gradient();
        // The top-level panel; the open submenu's parent row stays highlighted.
        let top = menu.scroll_for(clipped.height.saturating_sub(2) as usize);
        let tones = crate::widgets::surface::tones(self.theme);
        if let Some(t) = tones {
            // The keyboard is in the submenu while one is open, so its
            // parent row keeps an unfocused band without the accent bar.
            let focused = menu.open_submenu.is_none();
            self.render_menu_card(frame, clipped, &menu.items, menu.selected, top, focused, t);
        } else {
            self.render_menu_panel(frame, clipped, &menu.items, menu.selected, top, grad);
        }
        // A breadcrumb picker shows what has been typed on its top border.
        if menu.crumb.is_some() && !self.crumb_filter.0.is_empty() {
            let text = if menu.items.is_empty() {
                format!(" No match: {} ", self.crumb_filter.0)
            } else {
                format!(" Filter: {} ", self.crumb_filter.0)
            };
            let room = clipped.width.saturating_sub(4) as usize;
            let shown: String = text.chars().take(room).collect();
            let style = match tones {
                Some(t) => Style::default().fg(t.text()).bg(t.raised()),
                None => Style::default()
                    .fg(self.theme.ui(Color::White))
                    .bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e)))
                    .add_modifier(Modifier::BOLD),
            };
            frame
                .buffer_mut()
                .set_string(clipped.x + 2, clipped.y, shown, style);
        }
        // Float the open submenu's panel beside its parent.
        if let Some(sub_rect) = self.submenu_rect()
            && let Some(MenuEntry::Submenu { items, .. }) =
                menu.open_submenu.and_then(|p| menu.items.get(p))
        {
            let sub_top = menu.submenu_scroll_for(menu.submenu_visible(sub_rect.height));
            if let Some(t) = tones {
                let sel = menu.submenu_selected;
                self.render_menu_card(frame, sub_rect, items, sel, sub_top, true, t);
            } else {
                self.render_menu_panel(
                    frame,
                    sub_rect,
                    items,
                    menu.submenu_selected,
                    sub_top,
                    grad,
                );
            }
        }
    }

    /// One menu panel as a `surfaces` theme draws it, in the same rect and
    /// rows as [`Self::render_menu_panel`] so the hit tests are unchanged: a
    /// raised card with the accent lip, rows in the body ink, the selected
    /// row as the band with the accent bar on the card's edge (a plain band
    /// while `focused` is false), section headers muted, separators a rule
    /// across the whole card, and shortcuts as keycaps against the right
    /// edge (`menu_entry_width` sizes the panel for them).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_menu_card(
        &self,
        frame: &mut ratatui::Frame,
        rect: Rect,
        entries: &[MenuEntry],
        selected: usize,
        top: usize,
        focused: bool,
        t: crate::widgets::surface::Tones,
    ) {
        use crate::widgets::surface;
        let rect = rect.intersection(frame.area());
        if rect.width < 3 || rect.height < 3 {
            return;
        }
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(t.rim()))
            .style(Style::default().bg(t.raised()));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let buf = frame.buffer_mut();
        surface::paint_overlay_frame(buf, rect, self.theme);
        let inner = Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            width: rect.width - 2,
            height: rect.height - 2,
        };
        for (i, entry) in entries.iter().enumerate().skip(top) {
            if (i - top) as u16 >= inner.height {
                break;
            }
            let y = inner.y + (i - top) as u16;
            let (label, hint) = match entry {
                MenuEntry::Separator => {
                    // Edge to edge, so the rule meets the rim on both sides.
                    let rule = "\u{2500}".repeat(rect.width as usize);
                    buf.set_string(rect.x, y, &rule, Style::default().fg(t.rim()));
                    continue;
                }
                MenuEntry::Header(label) => {
                    buf.set_stringn(
                        inner.x,
                        y,
                        format!(" {label}"),
                        inner.width as usize,
                        Style::default().fg(t.muted()),
                    );
                    continue;
                }
                MenuEntry::Item { label, action } => {
                    (label.as_str(), menu_hint(action, &self.keymap))
                }
                MenuEntry::Submenu { label, .. } => (label.as_str(), None),
            };
            let is_sel = i == selected;
            let row_bg = if is_sel {
                let band = Rect::new(rect.x, y, rect.width - 1, 1);
                surface::paint_selected_row(buf, band, t, t.raised, focused);
                buf[(rect.x + 1, y)].bg
            } else {
                t.raised()
            };
            let ink = if is_sel { t.strong() } else { t.text() };
            buf.set_stringn(
                inner.x,
                y,
                format!(" {label}"),
                inner.width as usize,
                Style::default().fg(ink),
            );
            let trail: Vec<ratatui::text::Span> = match (entry, hint) {
                (MenuEntry::Submenu { .. }, _) => vec![ratatui::text::Span::styled(
                    "\u{25b8}",
                    Style::default().fg(if is_sel { t.text() } else { t.muted() }),
                )],
                (_, Some(hint)) => surface::keycap_spans(&hint, t, row_bg, is_sel),
                (_, None) => Vec::new(),
            };
            let trail_w: u16 = trail.iter().map(|s| s.width() as u16).sum();
            if trail_w > 0 && trail_w + 2 <= inner.width {
                let x = inner.right() - trail_w - 1;
                buf.set_line(x, y, &ratatui::text::Line::from(trail), trail_w);
            }
        }
    }

    /// Paint one bordered menu panel of `entries` into `rect`, from entry
    /// `top` on, highlighting row `selected`. Items show a right-aligned shortcut hint, submenu parents a
    /// `▸`, and separators a divider rule. Shared by the main menu and the
    /// floating submenu.
    pub(super) fn render_menu_panel(
        &self,
        frame: &mut ratatui::Frame,
        rect: Rect,
        entries: &[MenuEntry],
        selected: usize,
        top: usize,
        grad: bool,
    ) {
        // Black theme: orange→green gradient border + muted dark-teal
        // selection. Croft Dark keeps the legacy bright-blue accent so the
        // menu stays coherent with that theme's blue focus border.
        let border_blue = self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff));
        // Clipped to the frame (a submenu flipped at a narrow edge can still
        // reach past it).
        let rect = rect.intersection(frame.area());
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(border_blue))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        if self.theme.framed_overlays() {
            crate::widgets::surface::paint_overlay_frame(frame.buffer_mut(), rect, self.theme);
        }
        let sel_bg = if grad {
            rgb_color(POPUP_SEL_BG)
        } else {
            border_blue
        };
        let sel_fg = if grad { Color::White } else { Color::Black };
        let inner = Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            width: rect.width.saturating_sub(2),
            height: rect.height.saturating_sub(2),
        };
        for (i, entry) in entries.iter().enumerate().skip(top) {
            if (i - top) as u16 >= inner.height {
                break;
            }
            let row = Rect {
                x: inner.x,
                y: inner.y + (i - top) as u16,
                width: inner.width,
                height: 1,
            };
            if let MenuEntry::Separator = entry {
                let rule = "\u{2500}".repeat(inner.width as usize);
                frame.buffer_mut().set_string(
                    inner.x,
                    row.y,
                    &rule,
                    Style::default().fg(self.theme.ui(Color::Rgb(0x45, 0x45, 0x45))),
                );
                continue;
            }
            if let MenuEntry::Header(label) = entry {
                // Dimmed, non-selectable section label (VS Code's group titles
                // like "Side Bar Position").
                frame.buffer_mut().set_string(
                    inner.x,
                    row.y,
                    format!(" {label}"),
                    Style::default()
                        .fg(self.theme.ui(Color::Rgb(0x7a, 0x82, 0x90)))
                        .add_modifier(Modifier::BOLD),
                );
                continue;
            }
            let is_sel = i == selected;
            let row_style = if is_sel {
                Style::default().fg(sel_fg).bg(sel_bg)
            } else {
                Style::default().fg(self.theme.ui(Color::White))
            };
            let (label, right_hint) = match entry {
                MenuEntry::Item { label, action } => {
                    (label.as_str(), menu_hint(action, &self.keymap))
                }
                // A submenu parent shows the ▸ where a shortcut would sit.
                MenuEntry::Submenu { label, .. } => {
                    (label.as_str(), Some(String::from("\u{25b8}")))
                }
                MenuEntry::Separator | MenuEntry::Header(_) => {
                    unreachable!("handled above")
                }
            };
            let line = ratatui::text::Line::from(format!(" {label}"));
            frame.render_widget(ratatui::widgets::Paragraph::new(line).style(row_style), row);
            if let Some(hint) = right_hint {
                let hint_w = hint.chars().count() as u16;
                if hint_w + 2 <= inner.width {
                    let hint_x = inner.x + inner.width - hint_w - 1;
                    let hint_style = if is_sel {
                        Style::default().fg(sel_fg).bg(sel_bg)
                    } else {
                        Style::default().fg(self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)))
                    };
                    frame
                        .buffer_mut()
                        .set_string(hint_x, row.y, &hint, hint_style);
                }
            }
        }
    }

    /// Whether an open context menu (or its submenu panel) overlaps `rect`
    /// on a protocol with no image z-layer (iTerm2 OSC-1337, Sixel), where
    /// the image blit would overpaint the menu. Kitty layers images below
    /// text, so it never needs this.
    pub(super) fn context_menu_covers_rect(&self, rect: Rect) -> bool {
        match self.inline_protocol {
            crate::iterm2_inline::InlineImageProtocol::ITerm2
            | crate::iterm2_inline::InlineImageProtocol::Sixel => {}
            _ => return false,
        }
        self.menu_rect()
            .into_iter()
            .chain(self.submenu_rect())
            .any(|m| rects_intersect(m, rect))
    }

    /// Compute the menu's bounding rect from current state.
    pub(super) fn menu_rect(&self) -> Option<Rect> {
        let menu = self.context_menu.as_ref()?;
        // Use char count (not byte len) so multi-byte glyphs in menu
        // labels (e.g. the "" in "Rename", the arrow in "Compare with
        // Selected") don't inflate the menu width past what's needed.
        // Width must also fit the shortcut hint on the right side, with
        // at least 2 cells of gap between label and shortcut.
        let widest = menu
            .items
            .iter()
            .map(|e| menu_entry_width(e, &self.keymap, self.theme.surfaces().is_some()))
            .max()
            .unwrap_or(0);
        let area = self.last_frame_area;
        // Never past the frame: a long menu on a short terminal (or any menu
        // under 18 columns) built a rect outside the buffer, which panics.
        let fit = |v: u16, max: u16| if max > 0 { v.min(max) } else { v };
        let width = fit((widest + 4).max(18) as u16, area.width);
        let height = fit((menu.items.len() + 2) as u16, area.height);
        // Clamp identically to `render_context_menu` so hit-testing maps
        // clicks to the same row the user actually sees. Without this, a
        // menu that has to shift up to fit (right-click low on screen)
        // dispatches the row above the one the user clicked.
        let x = if area.width > 0 {
            menu.origin.0.min(area.width.saturating_sub(width))
        } else {
            menu.origin.0
        };
        let y = if area.height > 0 {
            menu.origin.1.min(area.height.saturating_sub(height))
        } else {
            menu.origin.1
        };
        Some(Rect {
            x,
            y,
            width,
            height,
        })
    }

    /// Rect of the open submenu's panel: floated to the RIGHT of its parent
    /// row, flipped to the left when it would overflow the screen, with its
    /// first row aligned to the parent row. `None` when no submenu is open.
    pub(super) fn submenu_rect(&self) -> Option<Rect> {
        let menu = self.context_menu.as_ref()?;
        let parent = menu.open_submenu?;
        let MenuEntry::Submenu { items, .. } = menu.items.get(parent)? else {
            return None;
        };
        let main = self.menu_rect()?;
        let widest = items
            .iter()
            .map(|e| menu_entry_width(e, &self.keymap, self.theme.surfaces().is_some()))
            .max()
            .unwrap_or(0);
        let area = self.last_frame_area;
        let fit = |v: u16, max: u16| if max > 0 { v.min(max) } else { v };
        let width = fit((widest + 4).max(18) as u16, area.width);
        let height = fit((items.len() + 2) as u16, area.height);
        // Float right of the main panel; flip left if it would overflow.
        let mut x = main.x + main.width;
        if area.width > 0 && x + width > area.width {
            x = main.x.saturating_sub(width);
        }
        // Align the submenu's top border so its first row sits on the parent row.
        let shown = parent.saturating_sub(menu.scroll.get());
        let y = (main.y + 1 + shown as u16).saturating_sub(1);
        let y = if area.height > 0 {
            y.min(area.height.saturating_sub(height))
        } else {
            y
        };
        Some(Rect {
            x,
            y,
            width,
            height,
        })
    }

    /// Rows the active menu level can paint: the open submenu's panel when
    /// one is open, else the top-level panel. Shared by the wheel, PgUp/PgDn
    /// and Home/End so every one of them pages against what is on screen.
    pub(super) fn menu_visible_rows(&self) -> usize {
        let rect = if self
            .context_menu
            .as_ref()
            .is_some_and(|m| m.open_submenu.is_some())
        {
            self.submenu_rect()
        } else {
            self.menu_rect()
        };
        rect.map(|r| r.height.saturating_sub(2) as usize)
            .unwrap_or(0)
    }

    /// If (x, y) hits a top-level menu row, return its index.
    pub(super) fn menu_item_at(&self, x: u16, y: u16) -> Option<usize> {
        let r = self.menu_rect()?;
        if !rect_contains(r, x, y) {
            return None;
        }
        // Items live inside the 1-cell border.
        let inner_y = y.checked_sub(r.y + 1)?;
        let menu = self.context_menu.as_ref()?;
        let top = menu.scroll_for(r.height.saturating_sub(2) as usize);
        let idx = top + inner_y as usize;
        (idx < menu.items.len()).then_some(idx)
    }

    /// If (x, y) hits a row of the open submenu panel, return that child index.
    pub(super) fn submenu_item_at(&self, x: u16, y: u16) -> Option<usize> {
        let r = self.submenu_rect()?;
        if !rect_contains(r, x, y) {
            return None;
        }
        let inner_y = y.checked_sub(r.y + 1)?;
        let menu = self.context_menu.as_ref()?;
        let parent = menu.open_submenu?;
        let MenuEntry::Submenu { items, .. } = menu.items.get(parent)? else {
            return None;
        };
        // The panel may be scrolled: the visible row maps to an entry offset
        // by the same view offset the renderer painted from.
        let top = menu.submenu_scroll_for(menu.submenu_visible(r.height));
        let idx = top + inner_y as usize;
        (idx < items.len()).then_some(idx)
    }

    pub(super) fn handle_menu_key(&mut self, key: KeyEvent) {
        // A breadcrumb picker filters as you type (the rows are a long list
        // of symbols, files or folders): characters narrow it, Backspace
        // widens it, Esc clears the text before it dismisses the picker.
        if self
            .context_menu
            .as_ref()
            .is_some_and(|m| m.crumb.is_some() && m.open_submenu.is_none())
        {
            match key.code {
                KeyCode::Char(c)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    self.crumb_filter.0.push(c);
                    self.apply_crumb_filter();
                    return;
                }
                KeyCode::Backspace if !self.crumb_filter.0.is_empty() => {
                    self.crumb_filter.0.pop();
                    self.apply_crumb_filter();
                    return;
                }
                KeyCode::Esc if !self.crumb_filter.0.is_empty() => {
                    self.crumb_filter.0.clear();
                    self.apply_crumb_filter();
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => {
                // Esc backs out of an open submenu first; at the top level it
                // dismisses the whole menu.
                let closed = self
                    .context_menu
                    .as_mut()
                    .is_some_and(|menu| menu.close_submenu());
                if !closed {
                    self.context_menu = None;
                }
            }
            KeyCode::Up => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.move_cursor(false);
                }
            }
            KeyCode::Down => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.move_cursor(true);
                }
            }
            KeyCode::Home | KeyCode::End => {
                let to_end = key.code == KeyCode::End;
                let visible = self.menu_visible_rows();
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.jump_active(to_end, visible);
                }
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                // Page through the active level (the open submenu when the
                // cursor is in it, else the top-level panel).
                let down = key.code == KeyCode::PageDown;
                let visible = self.menu_visible_rows();
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.page_active(if down { 1 } else { -1 }, visible);
                }
            }
            KeyCode::Left | KeyCode::Right
                if self
                    .context_menu
                    .as_ref()
                    .is_some_and(|m| m.crumb.is_some() && m.open_submenu.is_none()) =>
            {
                // A breadcrumb picker steps to the neighbouring crumb's
                // (#1608), VS Code's Left / Right in the focused bar.
                let Some(i) = self.context_menu.as_ref().and_then(|m| m.crumb) else {
                    return;
                };
                let next = if key.code == KeyCode::Left {
                    i.checked_sub(1)
                } else {
                    Some(i + 1).filter(|&n| n < self.editor.breadcrumbs.len())
                };
                if let Some(n) = next {
                    self.open_breadcrumb_picker(n);
                }
            }
            KeyCode::Left => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.close_submenu();
                }
            }
            KeyCode::Right => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.open_cursor_submenu();
                }
            }
            KeyCode::Enter => {
                // On a submenu parent, Enter opens it; on an item it dispatches.
                let opened = self
                    .context_menu
                    .as_mut()
                    .is_some_and(|menu| menu.open_cursor_submenu());
                if opened {
                    return;
                }
                if let Some(menu) = self.context_menu.as_ref()
                    && let Some(action) = menu.cursor_action()
                {
                    let dir = menu.target_dir.clone();
                    self.context_menu = None;
                    self.dispatch_menu_action(action, dir);
                }
            }
            _ => {}
        }
    }

    pub(super) fn dispatch_menu_action(&mut self, action: MenuAction, target_dir: PathBuf) {
        match action {
            MenuAction::Create(kind) => self.open_create_prompt(kind, target_dir),
            MenuAction::Delete { paths } => self.delete_paths(paths),
            MenuAction::Rename(path) => self.open_rename_prompt(path),
            MenuAction::Cut(paths) => {
                let n = paths.len();
                self.tree_clipboard = Some(ExplorerClipboard {
                    mode: ExplorerClipMode::Cut,
                    paths,
                });
                self.status = if n == 1 {
                    String::from("Cut 1 item")
                } else {
                    format!("Cut {n} items")
                };
            }
            MenuAction::Copy(paths) => {
                let n = paths.len();
                self.tree_clipboard = Some(ExplorerClipboard {
                    mode: ExplorerClipMode::Copy,
                    paths,
                });
                self.status = if n == 1 {
                    String::from("Copied 1 item")
                } else {
                    format!("Copied {n} items")
                };
            }
            MenuAction::Paste(dest) => self.paste_into(dest),
            MenuAction::SelectForCompare(path) => {
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.compare_anchor = Some(path);
                self.status = format!("Selected {label} for compare");
            }
            MenuAction::AddWorkspaceFolder => self.open_add_folder_picker(),
            MenuAction::RemoveWorkspaceFolder(path) => self.remove_workspace_folder(path),
            MenuAction::MakeRoot(folder) => {
                self.change_workspace_root(folder);
            }
            MenuAction::RevealInFinder(path) => self.reveal_in_finder(path),
            MenuAction::OpenInViewer(id, path) => {
                // The row was built when the menu opened; an extension
                // removed since then must not make the click a silent no-op.
                match crate::mcp::registry::viewer_by_id_in_dir(
                    &self.config_dir,
                    &self.disabled_extensions,
                    &id,
                ) {
                    Some(viewer) => self.open_in_viewer(&viewer, &path),
                    None => self.status = format!("{id} is no longer available"),
                }
            }
            MenuAction::CopyTabPath(path) => self.copy_path_to_clipboard(path),
            MenuAction::CopyTabRelativePath(path) => self.copy_relative_path_to_clipboard(path),
            MenuAction::RevealInExplorer(path) => self.reveal_in_explorer(path),
            MenuAction::CloseTab(idx) => self.request_close_tab(idx),
            MenuAction::CloseOtherTabs(keep_idx) => self.close_other_tabs(keep_idx),
            MenuAction::CloseTabsToRight(from_idx) => self.close_tabs_to_right(from_idx),
            MenuAction::CloseAllTabs => self.close_all_tabs(),
            MenuAction::CloseSavedTabs => self.close_saved_tabs(),
            MenuAction::KeepTabOpen(idx) => self.keep_tab_open(idx),
            MenuAction::ToggleTabPin(idx) => self.toggle_tab_pin(idx),
            MenuAction::SplitEditor => self.split_editor(),
            MenuAction::SplitEditorLeft => {
                self.split_editor_dir(editor_layout::SplitDir::Horizontal, false)
            }
            MenuAction::SplitEditorUp => {
                self.split_editor_dir(editor_layout::SplitDir::Vertical, false)
            }
            MenuAction::SplitEditorDown => {
                self.split_editor_dir(editor_layout::SplitDir::Vertical, true)
            }
            MenuAction::MoveEditorUp => self.move_active_editor(editor_layout::Dir::Up),
            MenuAction::MoveEditorDown => self.move_active_editor(editor_layout::Dir::Down),
            MenuAction::MoveEditorLeft => self.move_active_editor(editor_layout::Dir::Left),
            MenuAction::MoveEditorRight => self.move_active_editor(editor_layout::Dir::Right),
            MenuAction::SplitInGroup => self.split_in_group(),
            MenuAction::FocusGroupLeft => self.focus_editor_group(true),
            MenuAction::FocusGroupRight => self.focus_editor_group(false),
            MenuAction::MoveIntoNewWindow(idx) => self.move_into_new_window(idx),
            MenuAction::CopyIntoNewWindow(idx) => self.copy_into_new_window(idx),
            MenuAction::CompareWithSelected { anchor, other } => {
                match self.open_diff_across_groups(&anchor, &other) {
                    Ok(()) => {
                        self.focus_pane(Pane::Editor);
                        self.compare_anchor = None;
                        self.sync_open_file_poll_mtime();
                        let l = anchor
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| anchor.display().to_string());
                        let r = other
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| other.display().to_string());
                        self.status = format!("Diff: {l} \u{2194} {r}");
                    }
                    Err(e) => {
                        self.status = format!("Diff failed: {e}");
                    }
                }
            }
            MenuAction::RenameSymbolAt { row, col } => {
                self.editor.collapse_carets();
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.start_rename_symbol();
            }
            MenuAction::CopyPermalink => self.share_link(ShareLink::CopyPermalink),
            MenuAction::OpenOnRemote => self.share_link(ShareLink::OpenOnRemote),
            MenuAction::ChangeAllOccurrencesAt { row, col } => {
                self.editor.collapse_carets();
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.start_change_all_occurrences();
            }
            MenuAction::NavigateBack => self.nav_back(),
            MenuAction::NavigateForward => self.nav_forward(),
            MenuAction::GoToDefinitionAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_definition_at_cursor();
            }
            MenuAction::GoToDeclarationAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_declaration_at_cursor();
            }
            MenuAction::GoToTypeDefinitionAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_type_definition_at_cursor();
            }
            MenuAction::GoToImplementationAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_implementation_at_cursor();
            }
            MenuAction::QuickFixAt { row, col } => {
                self.editor.collapse_carets();
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.start_code_action();
            }
            MenuAction::GoToReferencesAt { row, col } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_references_at_cursor();
            }
            MenuAction::OpenCrumbFile(path) => self.open_crumb_file(&path),
            MenuAction::BrowseCrumbFolder { dir, crumb } => {
                self.open_crumb_folder_picker(&dir, crumb, None);
            }
            MenuAction::GoToLocation { path, line, col } => {
                self.go_to_definition(path, line, col);
            }
            MenuAction::ToggleBreakpointAt { line } => {
                self.focus_pane(Pane::Editor);
                self.debug_toggle_breakpoint_line(line);
            }
            MenuAction::EditBreakpointConditionAt { line } => {
                self.focus_pane(Pane::Editor);
                self.debug_edit_condition_line(line);
            }
            MenuAction::EditLogpointAt { line } => {
                self.focus_pane(Pane::Editor);
                self.debug_edit_logpoint_line(line);
            }
            MenuAction::ToggleBookmarkAt { line } => {
                self.focus_pane(Pane::Editor);
                // The clicked line (1-based), with the caret, selection and
                // scroll left alone, as the gutter's own toggle does: moving
                // the caret there kept the selection, which the next key
                // then replaced off screen.
                self.toggle_bookmark_at(line);
            }
            MenuAction::NextBookmark => {
                self.focus_pane(Pane::Editor);
                self.goto_bookmark(true);
            }
            MenuAction::PreviousBookmark => {
                self.focus_pane(Pane::Editor);
                self.goto_bookmark(false);
            }
            MenuAction::AskNavigatorAt { line } => {
                self.focus_pane(Pane::Editor);
                self.open_ask_navigator((line, line), String::new());
            }
            MenuAction::AskNavigatorSelection { start, end } => {
                self.focus_pane(Pane::Editor);
                let selection = self.editor.selection_text();
                self.open_ask_navigator((start, end), selection);
            }
            MenuAction::AskNavigatorAboutCapture => self.ask_navigator_about_capture(),
            MenuAction::CopyDebugValue { expression, value } => {
                self.copy_debug_value(expression, value)
            }
            MenuAction::CopyDebugExpression(expr) => {
                self.status = self.copy_where_pasted(&expr).status("expression");
            }
            MenuAction::AddDebugWatch(expr) => {
                self.add_watch_expression(expr);
                self.refresh_debug_panel();
            }
            MenuAction::RemoveDebugWatch(index) => {
                self.remove_watch_expression(index);
                self.refresh_debug_panel();
            }
            MenuAction::CopyCallStack => self.copy_call_stack(),
            MenuAction::FixProblemWithNavigator { path, item } => {
                self.fix_problem_with_navigator(path, item, Vec::new());
            }
            MenuAction::OpenSymbolTab {
                path,
                name,
                first,
                last,
            } => {
                // Go to the symbol first: the tab opens from the file's own
                // tab, which may not be the one showing.
                self.go_to_definition(path.clone(), first as u32, 0);
                if self.editor.path.as_deref() == Some(path.as_path()) {
                    self.open_symbol_tab_for(name, first, last);
                }
            }
            MenuAction::GoToOutlineSymbol { path, line, col } => {
                self.go_to_definition(path, line, col);
            }
            MenuAction::ShowCallsAt { row, col, incoming } => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.focus_pane(Pane::Editor);
                self.request_call_hierarchy_at_cursor(incoming);
            }
            MenuAction::OpenThemePicker => self.open_theme_picker(),
            MenuAction::SetTheme(theme) => self.apply_theme(theme),
            MenuAction::ToggleExplorerView(view) => self.toggle_explorer_view(view),
            MenuAction::OpenCustomizeLayout => self.open_customize_layout_menu(),
            MenuAction::ToggleActivityBar => {
                self.toggle_activity_bar();
                self.open_customize_layout_menu_on(&MenuAction::ToggleActivityBar);
            }
            MenuAction::ToggleSideBar => {
                self.toggle_side_bar();
                self.after_chrome_visibility_change();
                self.open_customize_layout_menu_on(&MenuAction::ToggleSideBar);
            }
            MenuAction::ToggleAutoHideSideBar => {
                self.toggle_sidebar_auto_hide();
                self.open_customize_layout_menu_on(&MenuAction::ToggleAutoHideSideBar);
            }
            MenuAction::ToggleSecondarySideBar => {
                self.toggle_secondary_side_bar();
                self.open_customize_layout_menu_on(&MenuAction::ToggleSecondarySideBar);
            }
            MenuAction::TogglePanel => {
                self.toggle_terminal();
                self.open_customize_layout_menu_on(&MenuAction::TogglePanel);
            }
            MenuAction::ToggleStatusBar => {
                self.toggle_status_bar();
                self.open_customize_layout_menu_on(&MenuAction::ToggleStatusBar);
            }
            MenuAction::SetSideBarPosition(pos) => {
                self.set_side_bar_position(pos);
                self.open_customize_layout_menu_on(&MenuAction::SetSideBarPosition(pos));
            }
            MenuAction::SetPanelAlignment(al) => {
                self.set_panel_alignment(al);
                self.open_customize_layout_menu_on(&MenuAction::SetPanelAlignment(al));
            }
            MenuAction::SetQuickInputPosition(pos) => {
                self.set_quick_input_position(pos);
                self.open_customize_layout_menu_on(&MenuAction::SetQuickInputPosition(pos));
            }
            MenuAction::ToggleZenMode => self.toggle_zen_mode(),
            MenuAction::ToggleCenteredLayout => {
                self.toggle_centered_layout();
                self.open_customize_layout_menu_on(&MenuAction::ToggleCenteredLayout);
            }
            MenuAction::RunCodeLens(idx) => self.run_code_lens(idx),
            MenuAction::RenameTerminal(idx) => self.begin_rename_terminal(idx),
            MenuAction::ClearTerminal(idx) => self.clear_terminal_at(idx),
            MenuAction::TerminalCopySelection => self.copy_terminal_selection(),
            MenuAction::TerminalPaste => self.paste_clipboard_into_terminal(),
            MenuAction::EditorCut => self.cut_editor_selection(),
            MenuAction::EditorCopy => self.copy_editor_selection(),
            MenuAction::EditorPaste => {
                let text = self.clipboard_text();
                self.paste_clipboard_into_editor(text.as_deref());
            }
            MenuAction::ShowCommandPalette => self.open_command_palette(),
            MenuAction::TerminalQuickSelect => self.open_terminal_quick_select(),
            MenuAction::TerminalCopyMode => self.open_terminal_copy_mode(),
            MenuAction::TerminalCommandHistory => self.open_command_history(),
            MenuAction::OpenScrollbackInEditor => self.open_scrollback_in_editor(),
            MenuAction::ToggleBroadcastInput => self.toggle_broadcast_input(),
            MenuAction::ToggleBroadcastExclusion => self.toggle_broadcast_exclusion(),
            MenuAction::UndoCloseTerminal => self.undo_close_terminal(),
            MenuAction::SplitTerminal => self.split_terminal_reporting(),
            MenuAction::KillTerminal(idx) => self.close_terminal_reporting(idx),
            MenuAction::CopyCommandOutput(idx, d) => self.copy_command_output(idx, d),
            MenuAction::SelectCommandOutput(idx, d) => self.select_command_output(idx, d),
            MenuAction::RerunCommand(idx, d) => self.rerun_command(idx, d),
            MenuAction::ToggleMaximizeTerminal(idx) => {
                if !self.terminal_pane_maximized {
                    self.active_terminal = idx;
                }
                self.toggle_terminal_pane_maximize();
            }
            MenuAction::ToggleCollapseTerminal(idx) => self.toggle_terminal_collapse(idx),
            MenuAction::NewTerminalWithProfile(shell) => {
                let label = std::path::Path::new(&shell)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&shell)
                    .to_string();
                match self.split_terminal_with_shell(&shell) {
                    Ok(()) => self.status = format!("Opened {label} terminal"),
                    Err(e) => self.status = format!("New terminal failed: {e}"),
                }
            }
            MenuAction::ToggleMinimap => self.toggle_minimap(),
            MenuAction::SetMinimapSide(side) => {
                self.minimap_side = side;
                // The strip jumps columns: evict the old placement so it
                // doesn't ghost, and force a rebake at the new position.
                self.disable_minimap_image();
            }
            MenuAction::SetLanguage(lang) => {
                self.editor.pick_language(lang);
                self.status = format!("Language Mode: {}", self.editor.language_label());
            }
            MenuAction::SetLineEnding(eol) => {
                if self.editor.eol != eol {
                    self.editor.eol = eol;
                    // The content is unchanged but the on-disk bytes will differ:
                    // mark dirty so the user saves to apply the new EOL.
                    self.editor.mark_dirty();
                }
                self.status = format!("End of Line: {}", eol.label());
            }
            MenuAction::SetEncoding(enc) => {
                // Reopening re-reads the file over the buffer and clears both
                // history stacks, so unsaved work would be gone with no way
                // back through Cmd+Z. The encoding segment sits a few cells
                // from the Ln/Col readout in the status bar, and picking the
                // encoding already in use looks like a no-op, so this is easy
                // to hit by accident. Never discard on its behalf.
                if self.editor.dirty {
                    // Deliberately NOT "save first": the buffer may be showing
                    // mojibake precisely because it was decoded wrongly, and
                    // saving writes those replacement characters over the
                    // original bytes. Undoing is the one route that loses
                    // nothing on either side.
                    self.status = String::from(
                        "Reopen with Encoding replaces the buffer from disk. Undo your unsaved changes first (Cmd+Z), or copy them elsewhere.",
                    );
                } else {
                    match self.editor.reopen_with_encoding(enc) {
                        Ok(()) => self.status = format!("Reopened with {}", enc.name()),
                        Err(e) => self.status = format!("Reopen failed: {e}"),
                    }
                }
            }
            MenuAction::SetIndentStyle(style) => {
                self.editor.set_indent_style(style);
                self.status = style.label();
            }
            MenuAction::ConvertIndentation(to_spaces) => {
                if to_spaces {
                    self.editor.indentation_to_spaces();
                    self.status = String::from("Converted indentation to spaces");
                } else {
                    self.editor.indentation_to_tabs();
                    self.status = String::from("Converted indentation to tabs");
                }
            }
            MenuAction::CommitAction {
                action,
                hash,
                short,
            } => self.run_commit_action(action, &hash, &short),
        }
    }

    /// Build a status-bar popup menu at `(col, row)`. `menu_rect` clamps it up
    /// to fit, so opening at the bottom status row renders above the bar.
    pub(super) fn open_status_menu(&mut self, col: u16, row: u16, items: Vec<MenuEntry>) {
        self.context_menu = Some(ContextMenu {
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            origin: (col, row),
            items,
            selected: 0,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.tree.root.clone(),
        });
    }

    /// Status-bar "Change Language Mode" picker. Editor-level only: it re-runs
    /// tree-sitter highlighting / indentation / comment rules under the chosen
    /// grammar. The LSP stays routed by file extension.
    pub(super) fn open_status_language_menu(&mut self, col: u16, row: u16) {
        let check = |on: bool| if on { "\u{2713} " } else { "  " };
        let current = self.editor.language();
        let mut items = vec![MenuEntry::Item {
            label: format!("{}Plain Text", check(current.is_none())),
            action: MenuAction::SetLanguage(None),
        }];
        for &lang in crate::widgets::editor::SELECTABLE_LANGUAGES {
            items.push(MenuEntry::Item {
                label: format!(
                    "{}{}",
                    check(current == Some(lang)),
                    crate::highlight::language_label(Some(lang))
                ),
                action: MenuAction::SetLanguage(Some(lang)),
            });
        }
        self.open_status_menu(col, row, items);
    }

    /// Status-bar EOL picker (LF / CRLF). Converts on the next save.
    pub(super) fn open_status_eol_menu(&mut self, col: u16, row: u16) {
        use crate::widgets::editor::LineEnding;
        let check = |on: bool| if on { "\u{2713} " } else { "  " };
        let cur = self.editor.eol;
        let items = vec![
            MenuEntry::Item {
                label: format!("{}LF", check(cur == LineEnding::Lf)),
                action: MenuAction::SetLineEnding(LineEnding::Lf),
            },
            MenuEntry::Item {
                label: format!("{}CRLF", check(cur == LineEnding::Crlf)),
                action: MenuAction::SetLineEnding(LineEnding::Crlf),
            },
        ];
        self.open_status_menu(col, row, items);
    }

    /// Status-bar "Reopen with Encoding" picker over a common set; selecting one
    /// re-decodes the file (and re-encodes it on the next save).
    pub(super) fn open_status_encoding_menu(&mut self, col: u16, row: u16) {
        use encoding_rs::{
            BIG5, EUC_JP, EUC_KR, GBK, KOI8_R, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251,
            WINDOWS_1252,
        };
        let check = |on: bool| if on { "\u{2713} " } else { "  " };
        let cur = self.editor.encoding;
        let encs: [&'static encoding_rs::Encoding; 11] = [
            UTF_8,
            UTF_16LE,
            UTF_16BE,
            WINDOWS_1252,
            WINDOWS_1251,
            SHIFT_JIS,
            EUC_JP,
            GBK,
            BIG5,
            EUC_KR,
            KOI8_R,
        ];
        let items = encs
            .iter()
            .map(|&enc| MenuEntry::Item {
                label: format!("{}{}", check(std::ptr::eq(enc, cur)), enc.name()),
                action: MenuAction::SetEncoding(enc),
            })
            .collect();
        self.open_status_menu(col, row, items);
    }

    /// Go to Line, opened by clicking the status bar's `Ln X, Col Y` readout.
    /// The field is prefilled with the caret's current line, selected, so the
    /// user can type over it or press Enter to stay put.
    pub(super) fn open_go_to_line_prompt(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let line = self.editor.cursor_row + 1;
        self.open_input_prompt(
            InputPrompt::new(InputPurpose::GoToLine, "Go to Line", "line number")
                .with_value(line.to_string()),
        );
    }

    /// Status-bar indentation picker, mirroring VS Code's "Select Indentation"
    /// quick pick: switch spaces-vs-tabs for future typing, change the width,
    /// and convert the buffer's existing indentation.
    pub(super) fn open_status_indent_menu(&mut self, col: u16, row: u16) {
        use crate::widgets::editor::IndentStyle;
        let check = |on: bool| if on { "\u{2713} " } else { "  " };
        let cur = self.editor.indent_style();
        let mut items = vec![
            MenuEntry::Item {
                label: format!("{}Indent Using Spaces", check(cur.use_spaces)),
                action: MenuAction::SetIndentStyle(IndentStyle {
                    width: cur.width,
                    use_spaces: true,
                }),
            },
            MenuEntry::Item {
                label: format!("{}Indent Using Tabs", check(!cur.use_spaces)),
                action: MenuAction::SetIndentStyle(IndentStyle {
                    width: cur.width,
                    use_spaces: false,
                }),
            },
            MenuEntry::Separator,
        ];
        for width in [2u32, 4, 8] {
            items.push(MenuEntry::Item {
                label: format!("{}Tab Size: {width}", check(cur.width == width)),
                action: MenuAction::SetIndentStyle(IndentStyle {
                    width,
                    use_spaces: cur.use_spaces,
                }),
            });
        }
        items.push(MenuEntry::Separator);
        items.push(MenuEntry::Item {
            label: String::from("  Convert Indentation to Spaces"),
            action: MenuAction::ConvertIndentation(true),
        });
        items.push(MenuEntry::Item {
            label: String::from("  Convert Indentation to Tabs"),
            action: MenuAction::ConvertIndentation(false),
        });
        self.open_status_menu(col, row, items);
    }
}
