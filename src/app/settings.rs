//! The Settings editor and hub, live and per-language settings, watching config files,
//! snippets, file associations and encodings, the Keyboard Shortcuts editor and the F1
//! overlay, the theme picker and profiles.

use super::*;

/// The config files the [`crate::config_sync::ConfigWatch`] checks for
/// changes made outside croft: the fixed ones, then every file of the
/// settings chain (#1435), so a `git pull` of `.croft/config.json`, an edit
/// in vim, or a dotfiles sync applies like a save from inside croft.
pub(super) fn config_watch_paths(settings_chain: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = vec![
        crate::keymap::keybindings_path(),
        crate::snippets::snippets_path(),
        crate::triggers::triggers_path(),
        crate::problem_matchers::matchers_path(),
        crate::macros::macros_path(),
        // The settings layer config sync brings from the connecting
        // machine (#262): applied on arrival like the files above.
        crate::config_layers::synced_config_path(),
    ];
    for path in settings_chain {
        if !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    paths
}

impl App {
    /// Replace the gear menu with the Color Theme picker: one row per theme,
    /// a check on the active one. Selecting a row switches + persists it.
    pub(super) fn open_theme_picker(&mut self) {
        let origin = self.settings_menu_origin();
        let active = self.theme;
        let items: Vec<(String, MenuAction)> = crate::theme::Theme::all()
            .iter()
            .map(|&t| {
                let mark = if t == active { "✔ " } else { "  " };
                (format!("{mark}{}", t.label()), MenuAction::SetTheme(t))
            })
            .collect();
        let selected = crate::theme::Theme::all()
            .iter()
            .position(|&t| t == active)
            .unwrap_or(0);
        self.context_menu = Some(ContextMenu {
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            origin,
            items: items.into_iter().map(MenuEntry::from).collect(),
            selected,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.workspace_root().to_path_buf(),
        });
        self.overlays.activity.mark_dirty();
    }

    /// Switch the IDE color theme: persist it, repaint the iTerm2 session
    /// background (so every `Color::Reset` surface follows), re-bake the
    /// OSC-1337 icon PNGs against the new background, and drop the lazily-baked
    /// image overlays + arm a full clear so nothing ghosts the old background.
    pub(super) fn apply_theme(&mut self, theme: crate::theme::Theme) {
        self.apply_theme_visuals(theme);
        // Persistence is best-effort: a write failure never blocks the
        // in-session switch, which has already taken effect above. Skipped
        // under test so the suite never clobbers the developer's real config.
        // The layered-settings reload path calls `apply_theme_visuals`
        // directly instead — a workspace-set theme must not be written back
        // into the user's own config.
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_theme(theme));
        }
    }

    /// Everything [`Self::apply_theme`] does except persisting the choice.
    pub(super) fn apply_theme_visuals(&mut self, theme: crate::theme::Theme) {
        self.theme = theme;
        // Push the theme's code palette into the highlighter and re-highlight
        // every open editor (across all split groups): cached spans carry baked
        // colors, so the syntax colors only follow the theme once recomputed.
        crate::highlight::set_syntax_palette(theme.syntax());
        // Assign the theme before re-highlighting: the rebuilds below (the
        // markdown preview in particular) read `ed.theme`, and the per-frame
        // push in render would hand it over one frame too late.
        for ed in &mut self.editor.editors {
            ed.theme = theme;
            ed.rehighlight_for_theme();
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in &mut group.editors {
                ed.theme = theme;
                ed.rehighlight_for_theme();
            }
        }
        // Refresh the per-pane gradient-border flag for the new theme so the
        // focused box switches its highlight style on the very next frame.
        self.sync_focus_flags();
        if !cfg!(test) {
            use std::io::Write;
            let mut out = stdout();
            let _ = out.write_all(theme_host_colors_seq(theme).as_bytes());
            let _ = out.flush();
        }
        self.rebake_inline_images();
        self.status = format!("Color Theme: {}", theme.label());
    }

    /// Bake every inline image again, for a new background or a new cell
    /// size.
    pub(super) fn rebake_inline_images(&mut self) {
        // Re-bake the fixed icon/badge/run-debug PNGs against the new bg.
        self.init_graphics();
        // The welcome wordmark, no-repo hero, SSH empty state, and editor
        // image previews are baked lazily and keyed on layout, not theme —
        // drop them so the next render re-bakes them on the new background.
        self.overlays.welcome.disable();
        self.overlays.hero.disable();
        self.overlays.ssh.disable();
        self.overlays.editor[0].disable();
        self.overlays.editor[1].disable();
        // Evict the stale activity-icon image layer and force a full repaint
        // (the clear latch the main loop folds into `terminal.clear()`), so
        // every cell — SGR and image alike — repaints on the new background.
        self.overlays.activity.mark_dirty();
        self.overlays.activity.request_clear();
    }

    /// Preferences: Open Settings — a searchable hub over the toggleable
    /// settings plus shortcuts to the raw JSON files and the theme picker. Each
    /// toggle row shows its current value; confirming a toggle flips it live and
    /// reopens the hub so the change is visible and more can be flipped.
    pub fn open_settings_view(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let on_off = |b: bool| if b { "on" } else { "off" };
        // Per-value provenance (#251): where a non-user layer set the backing
        // key, say so — the usual "I toggled it and nothing changed" is a
        // workspace layer winning the merge.
        let prov = |key: &str| {
            use crate::config_layers::LayerKind;
            match crate::config_layers::layer_of(&self.settings_provenance, key) {
                LayerKind::Default | LayerKind::User => String::new(),
                other => format!("  · {}", other.label()),
            }
        };
        let rows = vec![
            ListRow {
                id: String::from("toggle:auto_close_pairs"),
                label: format!(
                    "Auto Closing Pairs: {}{}",
                    on_off(self.auto_close_pairs),
                    prov("disable_auto_close_pairs")
                ),
            },
            ListRow {
                id: String::from("toggle:format_on_save"),
                label: format!(
                    "Format on Save: {}{}",
                    on_off(self.format_on_save),
                    prov("format_on_save")
                ),
            },
            ListRow {
                id: String::from("toggle:auto_save"),
                label: format!("Auto Save: {}{}", on_off(self.auto_save), prov("auto_save")),
            },
            ListRow {
                id: String::from("toggle:auto_save_on_focus_change"),
                label: format!(
                    "Auto Save on Focus Change: {}{}",
                    on_off(self.auto_save_on_focus_change),
                    prov("auto_save_on_focus_change")
                ),
            },
            ListRow {
                id: String::from("toggle:inline_blame"),
                label: format!(
                    "Git: Inline Blame: {}{}",
                    on_off(self.inline_blame_enabled),
                    prov("disable_inline_blame")
                ),
            },
            ListRow {
                id: String::from("toggle:indent_guides"),
                label: format!(
                    "Editor: Indent Guides: {}{}",
                    on_off(self.indent_guides_enabled),
                    prov("disable_indent_guides")
                ),
            },
            ListRow {
                id: String::from("toggle:bracket_colors"),
                label: format!(
                    "Editor: Bracket Pair Colorization: {}{}",
                    on_off(self.bracket_colors_enabled),
                    prov("disable_bracket_colors")
                ),
            },
            ListRow {
                id: String::from("toggle:render_whitespace"),
                label: format!(
                    "Editor: Render Whitespace: {}{}",
                    self.whitespace_mode.label(),
                    prov("render_whitespace")
                ),
            },
            ListRow {
                id: String::from("toggle:line_numbers"),
                label: format!(
                    "Editor: Line Numbers: {}{}",
                    self.line_numbers.pref_id(),
                    prov("line_numbers")
                ),
            },
            ListRow {
                id: String::from("toggle:vim_mode"),
                label: format!(
                    "Editor: Vim Mode: {}{}",
                    on_off(self.vim.enabled),
                    prov("vim_mode")
                ),
            },
            ListRow {
                id: String::from("toggle:inline_values"),
                label: format!(
                    "Debugger: Inline Values: {}{}",
                    on_off(self.inline_values_enabled),
                    prov("disable_inline_values")
                ),
            },
            ListRow {
                id: String::from("toggle:inlay_hints"),
                label: format!(
                    "Editor: Inlay Hints: {}{}",
                    on_off(self.inlay_hints_enabled),
                    prov("disable_inlay_hints")
                ),
            },
            ListRow {
                id: String::from("toggle:format_on_type"),
                label: format!(
                    "Editor: Format on Type: {}{}",
                    on_off(self.format_on_type),
                    prov("format_on_type")
                ),
            },
            ListRow {
                id: String::from("toggle:copy_on_select"),
                label: format!(
                    "Terminal: Copy on Selection: {}{}",
                    on_off(self.copy_on_select),
                    prov("copy_on_select")
                ),
            },
            ListRow {
                id: String::from("toggle:log_highlight"),
                label: format!(
                    "Log: Highlighting (tailspin): {}{}",
                    on_off(self.log_highlight),
                    prov("disable_log_highlight")
                ),
            },
            ListRow {
                id: String::from("toggle:secret_redaction"),
                label: format!(
                    "Terminal: Redact Secrets (keys, tokens): {}{}",
                    on_off(self.secret_redaction),
                    prov("disable_secret_redaction")
                ),
            },
            ListRow {
                id: String::from("cmd:color_theme"),
                label: format!("Color Theme: {}{}", self.theme.label(), prov("theme")),
            },
            ListRow {
                id: String::from("cmd:open_settings_json"),
                label: String::from("Open User Settings (JSON)"),
            },
            ListRow {
                id: String::from("cmd:open_user_settings_local_json"),
                label: String::from("Open User Settings — Local (JSON, this machine only)"),
            },
            ListRow {
                id: String::from("cmd:open_workspace_settings_json"),
                label: String::from("Open Workspace Settings (JSON)"),
            },
            ListRow {
                id: String::from("cmd:open_workspace_settings_local_json"),
                label: String::from("Open Workspace Settings — Local (JSON, gitignored)"),
            },
            ListRow {
                id: String::from("cmd:open_keybindings_json"),
                label: String::from("Open Keyboard Shortcuts (JSON)"),
            },
            ListRow {
                id: String::from("cmd:configure_snippets"),
                label: String::from("Configure User Snippets"),
            },
            ListRow {
                id: String::from("cmd:keyboard_shortcuts"),
                label: String::from(
                    "Keyboard Shortcuts\u{2026} (record a new chord for any command)",
                ),
            },
            ListRow {
                id: String::from("toggle:code_lens"),
                label: format!("Editor: CodeLens: {}", on_off(self.code_lens_enabled)),
            },
            ListRow {
                id: String::from("toggle:terminal_suggestions"),
                label: format!(
                    "Terminal: Command Suggestions: {}",
                    on_off(self.term_suggest_enabled)
                ),
            },
            ListRow {
                id: String::from("toggle:screen_reader"),
                label: format!(
                    "Accessibility: Screen Reader Mode: {}",
                    on_off(self.screen_reader)
                ),
            },
        ];
        self.open_list_picker(
            ListPicker::new(ListPurpose::Settings, "Settings", rows),
            "No settings",
        );
    }

    /// Preferences: Open Workspace Settings (#251) — the committed
    /// `.croft/config.json`, or with `local` the gitignored
    /// `.croft/config.local.json`. Creating either ensures
    /// `.croft/.gitignore` covers the local file first, so a personal
    /// override can never land in a commit by accident.
    pub fn open_workspace_settings(&mut self, local: bool) {
        let root = self.roots.primary().to_path_buf();
        if let Err(e) = crate::config_layers::ensure_workspace_local_ignored(&root) {
            self.status = format!("Could not prepare .croft/: {e}");
            return;
        }
        let path = if local {
            crate::config_layers::workspace_local_config_path(&root)
        } else {
            crate::config_layers::workspace_config_path(&root)
        };
        self.open_config_file_in_editor(path, ConfigFileSeed::SettingsLayer);
    }

    /// The directory of the user settings layer: the active profile's
    /// (#1587), as every other reader of that layer resolves it, or the
    /// config dir itself with no profile. Read through the bare config dir,
    /// the Settings editor showed and saved the Default profile's
    /// `config.json` while a profile's was in force.
    pub(super) fn user_settings_dir(&self) -> PathBuf {
        match &self.settings_profile {
            Some(profile) => self.config_dir.join("profiles").join(profile),
            None => self.config_dir.clone(),
        }
    }

    /// The merged settings as the Settings editor sees them: the user layers
    /// from [`Self::user_settings_dir`], the workspace layers from the
    /// primary root.
    pub(super) fn merged_settings_here(&self) -> crate::config_layers::MergedConfig {
        crate::config_layers::load_merged_from(
            &self.user_settings_dir(),
            Some(self.roots.primary()),
            crate::config_layers::current_platform(),
        )
    }

    /// Open the Settings editor (#612).
    pub(super) fn open_settings_editor(&mut self) {
        let merged = self.merged_settings_here();
        let mut editor = crate::widgets::settings_editor::SettingsEditor::new(
            crate::settings_editor::rows(&merged.prefs, &merged.provenance),
        );
        editor.profile = self.settings_profile.clone();
        self.settings_editor = Some(editor);
    }

    /// Write `key = value` into the Settings editor's target layer, re-merge
    /// and apply it live, and refresh the editor's rows.
    pub(super) fn write_setting(&mut self, key: &str, value: serde_json::Value) {
        let Some(path) = self.settings_target_file(key) else {
            return;
        };
        // A value croft can't read back as the setting is refused here,
        // not saved and then ignored (#1555).
        if let Err(e) =
            crate::settings_editor::check(&self.merged_settings_here().prefs, key, &value)
        {
            self.status = e;
            return;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let new = match crate::settings_editor::set_key(&text, key, value) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("{}: {e}", path.display());
                return;
            }
        };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, new));
        if let Err(e) = written {
            self.status = format!("{}: {e}", path.display());
            return;
        }
        // Applied just below, so the settings watch must not apply it again.
        self.config_watch.note(&path);
        let merged = self.merged_settings_here();
        for w in &merged.warnings {
            crate::output::push("Settings", crate::output::OutputLevel::Warn, w);
        }
        self.apply_merged_settings(&merged.prefs);
        self.settings_provenance = merged.provenance.clone();
        if let Some(ed) = self.settings_editor.as_mut() {
            ed.rows = crate::settings_editor::rows(&merged.prefs, &merged.provenance);
        }
        self.status = format!("{key} saved to {}", path.display());
    }

    /// The layer file the Settings editor writes `key` to: the user's
    /// `config.json`, or, with the workspace layer chosen, the workspace's,
    /// for the keys a workspace may set. None, with the status saying why,
    /// for any other key there.
    pub(super) fn settings_target_file(&mut self, key: &str) -> Option<std::path::PathBuf> {
        use crate::config_layers::LayerKind;
        let target = self.settings_editor.as_ref()?.target;
        match target {
            LayerKind::Workspace => {
                if !crate::config_layers::WORKSPACE_ALLOWED_KEYS.contains(&key) {
                    self.status =
                        format!("{key} can only be set in your user settings (Tab switches layer)");
                    return None;
                }
                Some(crate::config_layers::workspace_config_path(
                    self.roots.primary(),
                ))
            }
            _ => Some(self.user_settings_dir().join("config.json")),
        }
    }

    /// A setting the editor can't edit in place (an object or a list),
    /// opened in the chosen layer's file with the caret on its key
    /// (#1555): it used to open the user's file at line 1 whichever layer
    /// was chosen.
    pub(super) fn open_setting_in_json(&mut self, key: &str) {
        let Some(path) = self.settings_target_file(key) else {
            return;
        };
        let seed = if path == self.user_settings_dir().join("config.json") {
            ConfigFileSeed::Settings
        } else {
            ConfigFileSeed::SettingsLayer
        };
        self.settings_editor = None;
        self.open_config_file_in_editor(path.clone(), seed);
        if self.editor.path.as_deref() != Some(path.as_path()) {
            return;
        }
        let line = crate::settings_editor::key_line(&self.editor.lines.join("\n"), key);
        if let Some(line) = line {
            self.editor.goto_line(line + 1);
        }
        let name = key.replace('_', " ");
        self.status = match line {
            Some(_) => format!("{name} is edited in the JSON file"),
            None => format!("{name} is edited in the JSON file: add \"{key}\" here"),
        };
    }

    /// Reset Setting (#1655): remove `key` from the layer the Settings
    /// editor writes to, so the value falls back to the layers below it or
    /// croft's default, and re-merge. Once only a hand edit of `config.json`
    /// could undo a setting, and toggling one back left a redundant entry.
    pub(super) fn reset_setting(&mut self, key: &str) {
        use crate::config_layers::LayerKind;
        let Some(target) = self.settings_editor.as_ref().map(|e| e.target) else {
            return;
        };
        let (path, layer) = match target {
            LayerKind::Workspace => (
                crate::config_layers::workspace_config_path(self.roots.primary()),
                "workspace",
            ),
            _ => (self.user_settings_dir().join("config.json"), "user"),
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let new = match crate::settings_editor::remove_key(
            &text,
            key,
            crate::config_layers::current_platform(),
        ) {
            Ok(Some(t)) => t,
            Ok(None) => {
                self.status =
                    format!("{key} is not set in your {layer} settings (Tab switches layer)");
                return;
            }
            Err(e) => {
                self.status = format!("{}: {e}", path.display());
                return;
            }
        };
        if let Err(e) = std::fs::write(&path, new) {
            self.status = format!("{}: {e}", path.display());
            return;
        }
        let merged = self.merged_settings_here();
        for w in &merged.warnings {
            crate::output::push("Settings", crate::output::OutputLevel::Warn, w);
        }
        self.apply_merged_settings(&merged.prefs);
        self.settings_provenance = merged.provenance.clone();
        let rows = crate::settings_editor::rows(&merged.prefs, &merged.provenance);
        let now = rows.iter().find(|r| r.key == key).map(|r| {
            format!(
                "{} ({})",
                crate::settings_editor::display(&r.value),
                r.layer.label()
            )
        });
        if let Some(ed) = self.settings_editor.as_mut() {
            ed.rows = rows;
        }
        self.status = match now {
            Some(now) => format!("{key} reset in {}: now {now}", path.display()),
            None => format!("{key} reset in {}", path.display()),
        };
    }

    /// Keys while the Settings editor is open (#612): typing searches, Enter
    /// edits the selected setting, Del resets it (#1655), Tab switches
    /// between the user and the workspace layer.
    pub(super) fn handle_settings_editor_key(&mut self, key: KeyEvent) {
        use crate::config_layers::LayerKind;
        use crate::settings_editor::Kind;
        let Some(ed) = self.settings_editor.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.settings_editor = None,
            (KeyCode::Up, _) => ed.move_selection(-1),
            (KeyCode::Down, _) => ed.move_selection(1),
            (KeyCode::PageUp, _) => ed.move_selection(-10),
            (KeyCode::PageDown, _) => ed.move_selection(10),
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                ed.target = if ed.target == LayerKind::Workspace {
                    LayerKind::User
                } else {
                    LayerKind::Workspace
                };
            }
            (KeyCode::Enter, _) => {
                let Some(row) = ed.selected_row().cloned() else {
                    return;
                };
                match row.kind {
                    Kind::Bool | Kind::Choice(_) => {
                        if let Some(v) = crate::settings_editor::next_value(&row) {
                            self.write_setting(&row.key, v);
                        }
                    }
                    Kind::Number | Kind::Text => {
                        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                        // An unset optional setting, or an empty one,
                        // opens empty, not on the `(default)` label, which
                        // typing would extend and which, confirmed as is,
                        // would unset an explicitly empty `review_ai_prefix`.
                        let now = if row.value.is_null() || row.value == "" {
                            String::new()
                        } else {
                            crate::settings_editor::display(&row.value)
                        };
                        self.open_input_prompt(
                            InputPrompt::new(
                                InputPurpose::SettingValue {
                                    key: row.key.clone(),
                                },
                                row.key.replace('_', " "),
                                String::from("new value"),
                            )
                            .with_value(now),
                        );
                    }
                    Kind::Json => self.open_setting_in_json(&row.key),
                }
            }
            (KeyCode::Delete, _) => {
                if let Some(key) = ed.selected_row().map(|r| r.key.clone()) {
                    self.reset_setting(&key);
                }
            }
            (KeyCode::Backspace, _) => {
                let mut q = ed.query.clone();
                q.pop();
                ed.set_query(q);
            }
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                let mut q = ed.query.clone();
                q.push(c);
                ed.set_query(q);
            }
            _ => {}
        }
    }

    /// A typed value for setting `key` (#612): numbers must parse as one.
    pub(super) fn submit_setting_value(&mut self, key: &str, value: &str) {
        let Some(row) = self
            .settings_editor
            .as_ref()
            .and_then(|e| e.rows.iter().find(|r| r.key == key).cloned())
        else {
            return;
        };
        match crate::settings_editor::typed_value(&row, value) {
            Ok(parsed) => self.write_setting(key, parsed),
            Err(e) => self.status = e,
        }
    }

    pub(super) fn render_settings_editor(&mut self, frame: &mut ratatui::Frame) {
        let area = frame.area();
        let theme = self.theme;
        if let Some(ed) = self.settings_editor.as_mut() {
            crate::widgets::settings_editor::render(ed, area, frame.buffer_mut(), theme);
        }
    }

    pub(super) fn open_shortcuts_modal(&mut self) {
        if self.shortcuts_modal.is_none() {
            self.shortcuts_modal = Some(crate::widgets::shortcuts::ShortcutsModal::default());
            self.overlays.shortcuts_clear.request();
            self.status = String::from("Shortcuts: Esc to close");
        }
    }

    pub(super) fn close_shortcuts_modal(&mut self) {
        if self.shortcuts_modal.take().is_some() {
            self.overlays.shortcuts_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub fn consume_shortcuts_image_clear(&mut self) -> bool {
        self.overlays.shortcuts_clear.consume()
    }

    /// Open one of the JSON config files in the editor, creating it seeded with
    /// a helpful starter when it doesn't exist yet, then reload the in-memory
    /// copy so a subsequent save takes effect without a relaunch.
    pub(super) fn open_config_file_in_editor(
        &mut self,
        path: std::path::PathBuf,
        seed: ConfigFileSeed,
    ) {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let contents = match seed {
                ConfigFileSeed::Settings => SETTINGS_TEMPLATE.to_string(),
                ConfigFileSeed::SettingsLayer => SETTINGS_LAYER_TEMPLATE.to_string(),
                ConfigFileSeed::Keybindings => crate::keymap::TEMPLATE.to_string(),
                ConfigFileSeed::Snippets => crate::snippets::TEMPLATE.to_string(),
                ConfigFileSeed::Triggers => crate::triggers::TEMPLATE.to_string(),
                ConfigFileSeed::Agents => crate::agents::TEMPLATE.to_string(),
                ConfigFileSeed::Matchers => crate::problem_matchers::TEMPLATE.to_string(),
            };
            if let Err(e) = std::fs::write(&path, contents) {
                self.status = format!("Could not create {}: {e}", path.display());
                return;
            }
        }
        if let Some(idx) = self.editor.find_tab_with_path(&path) {
            self.editor.select(idx);
        } else if self.editor.open_pinned(&path).is_err() {
            self.status = format!("Could not open {}", path.display());
            return;
        }
        self.focus_pane(Pane::Editor);
        self.sync_open_file_poll_mtime();
        self.status = match seed {
            ConfigFileSeed::Settings => {
                String::from("Editing settings — save to apply (theme and editor toggles live)")
            }
            ConfigFileSeed::SettingsLayer => {
                String::from("Editing a settings layer — save to apply")
            }
            ConfigFileSeed::Keybindings => String::from(
                "Editing keybindings.json — save, then reload keybindings from the palette",
            ),
            ConfigFileSeed::Snippets => {
                String::from("Editing snippets.json — save to apply immediately")
            }
            ConfigFileSeed::Triggers => {
                String::from("Editing triggers.json — save to apply immediately")
            }
            ConfigFileSeed::Agents => {
                String::from("Editing agents.json — save to apply immediately")
            }
            ConfigFileSeed::Matchers => {
                String::from("Editing matchers.json — save to apply immediately")
            }
        };
    }

    pub(super) fn handle_shortcuts_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.shortcuts_modal.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::F(1), _) => self.close_shortcuts_modal(),
            (KeyCode::Char('q'), KeyModifiers::NONE) => self.close_shortcuts_modal(),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => modal.scroll_down(1),
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => modal.scroll_up(1),
            (KeyCode::PageDown, _) | (KeyCode::Char(' '), _) => modal.scroll_down(10),
            (KeyCode::PageUp, _) => modal.scroll_up(10),
            (KeyCode::Home, _) | (KeyCode::Char('g'), _) => modal.scroll_to_top(),
            (KeyCode::End, _) | (KeyCode::Char('G'), _) => modal.scroll_to_bottom(),
            _ => {}
        }
    }

    pub(super) fn handle_shortcuts_modal_mouse(&mut self, m: MouseEvent) {
        let Some(modal) = self.shortcuts_modal.as_mut() else {
            return;
        };
        let inside = rect_contains(modal.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => modal.scroll_down(3),
            MouseEventKind::ScrollUp => modal.scroll_up(3),
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_shortcuts_modal();
            }
            _ => {}
        }
    }

    pub(super) fn render_shortcuts_modal(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let Some(modal) = self.shortcuts_modal.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::shortcuts::render_shortcuts_modal(modal, area, frame.buffer_mut(), theme);
    }

    /// The overrides `"[<scope>]"` holds (#1440), if any.
    pub(super) fn language_prefs_for(&self, scope: &str) -> Option<&crate::prefs::LanguagePrefs> {
        self.language_prefs.get(scope)
    }

    /// Format on save for a file in language `scope` (#1440): its language
    /// block's value, else the global one.
    pub(super) fn format_on_save_for(&self, scope: &str) -> bool {
        self.language_prefs_for(scope)
            .and_then(|l| l.format_on_save)
            .unwrap_or(self.format_on_save)
    }

    /// Format on type for a file in language `scope` (#1440).
    pub(super) fn format_on_type_for(&self, scope: &str) -> bool {
        self.language_prefs_for(scope)
            .and_then(|l| l.format_on_type)
            .unwrap_or(self.format_on_type)
    }

    /// Auto-closing pairs for a file in language `scope` (#1440).
    pub(super) fn auto_close_pairs_for(&self, scope: &str) -> bool {
        self.language_prefs_for(scope)
            .and_then(|l| l.disable_auto_close_pairs)
            .map_or(self.auto_close_pairs, |off| !off)
    }

    /// Inlay hints for a file in language `scope` (#1440).
    pub(super) fn inlay_hints_enabled_for(&self, scope: &str) -> bool {
        inlay_hints_on_in(&self.language_prefs, self.inlay_hints_enabled, scope)
    }

    /// Whitespace rendering for a file in language `scope` (#1440).
    #[cfg(test)]
    pub(super) fn whitespace_mode_for(
        &self,
        scope: &str,
    ) -> crate::widgets::editor::WhitespaceMode {
        whitespace_mode_in(&self.language_prefs, self.whitespace_mode, scope)
    }

    /// Profiles: Switch Profile (#618): the default, every profile, and the
    /// actions to make a new one or pin one to this workspace.
    pub(super) fn open_profiles(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let active = crate::profiles::active();
        let mark = |on: bool| if on { "  \u{2713}" } else { "" };
        let mut rows = vec![ListRow {
            id: String::from("profile:"),
            label: format!("Default{}", mark(active.is_none())),
        }];
        for name in crate::profiles::list() {
            rows.push(ListRow {
                id: format!("profile:{name}"),
                label: format!("{name}{}", mark(active.as_deref() == Some(name.as_str()))),
            });
        }
        rows.push(ListRow {
            id: String::from("new"),
            label: String::from("New Profile from Current Setup\u{2026}"),
        });
        if active.is_some() {
            rows.push(ListRow {
                id: String::from("workspace"),
                label: String::from("Use the Active Profile for This Workspace"),
            });
        }
        if crate::profiles::workspace_choice_path(&self.tree.root).exists() {
            rows.push(ListRow {
                id: String::from("workspace-clear"),
                label: String::from("Stop Using a Profile for This Workspace"),
            });
        }
        self.open_list_picker(ListPicker::new(ListPurpose::Profiles, "Profiles", rows), "");
    }

    /// Make `name` (or the default) the active profile and apply what can be
    /// applied live: keybindings and snippets. Settings are read at startup.
    pub(super) fn switch_profile(&mut self, name: Option<String>) {
        if let Err(e) = crate::profiles::switch(name.as_deref()) {
            self.status = format!("Could not switch profile: {e}");
            return;
        }
        // The Settings editor reads and writes the new profile's settings
        // from now on, as every other settings writer does (#1587).
        self.settings_profile = name.clone();
        // Recorded shortcuts go to the active profile's file from now on.
        if !cfg!(test) {
            self.keybindings_file = crate::keymap::keybindings_path();
        }
        let (map, _) = crate::keymap::Keymap::load_with_warnings(&self.keybindings_file);
        self.keymap = map;
        self.snippets = load_all_snippets(self.roots.iter());
        self.status = format!(
            "Profile: {}; keybindings and snippets applied, settings apply at the next launch",
            name.as_deref().unwrap_or("Default")
        );
    }

    /// Preferences: Open Keyboard Shortcuts (#612, Cmd+K Cmd+S): every
    /// command with its shortcut, searchable; choosing one records a new one.
    pub(crate) fn open_keyboard_shortcuts(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows = crate::widgets::command_palette::ALL_COMMANDS
            .iter()
            .map(|&cmd| {
                let shown = match self.keymap.chord_for(cmd) {
                    Some(user) => format!("{user}  (yours)"),
                    // Its built-in chord now runs another command (#1482).
                    None if self.keymap.shadows_builtin(cmd) => String::from("\u{2014}"),
                    None if !cmd.keybinding_hint().is_empty() => {
                        crate::widgets::command_palette::platform_hint(cmd.keybinding_hint())
                            .into_owned()
                    }
                    None => String::from("\u{2014}"),
                };
                ListRow {
                    id: format!("kb:{}", cmd.id()),
                    label: format!("{}  \u{00b7}  {shown}", cmd.title()),
                }
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(ListPurpose::KeyboardShortcuts, "Keyboard Shortcuts", rows),
            "No commands",
        );
    }

    /// Bind `cmd` to the chord `key` in keybindings.json (#612) and reload.
    /// Plain typing cannot be a shortcut (it would stop being typing), so
    /// only function keys and Cmd/Ctrl/Alt chords are accepted, the same rule
    /// the keymap itself applies.
    pub(super) fn record_shortcut(
        &mut self,
        cmd: crate::widgets::command_palette::Command,
        key: KeyEvent,
    ) {
        if key.code == KeyCode::Esc {
            self.status = String::from("Shortcut unchanged");
            return;
        }
        if !is_rebindable_chord(key) {
            self.status = String::from(
                "A shortcut needs a function key or Cmd/Ctrl/Alt; press Keyboard Shortcuts again to retry",
            );
            return;
        }
        // The command the chord runs now: a user binding, else a built-in
        // shortcut no user binding has taken (#1482). Taking it from that
        // command waits for Enter, so a slip of the finger cannot silently
        // cost a shortcut like Toggle Line Comment's.
        let chord = crate::keymap::Chord::from_event(key);
        // A built-in chord can be shared (F7 is both Merge: Next Conflict
        // and Go to Next Change), and a user binding takes it from all of
        // them, so every other owner is named.
        let owners: Vec<_> = match self.keymap.command_for(key) {
            Some(bound) => vec![bound],
            None => crate::keymap::builtin_commands_on(chord),
        }
        .into_iter()
        .filter(|c| *c != cmd)
        .collect();
        if !owners.is_empty() {
            self.status = format!(
                "{} is {}'s shortcut: Enter to give it to \u{201c}{}\u{201d}, Esc to cancel",
                chord.to_config_string(),
                quoted_titles(&owners),
                cmd.title()
            );
            self.confirming_shortcut = Some((cmd, key, owners));
            return;
        }
        self.write_shortcut(cmd, key, &[]);
    }

    /// Write `key` as `cmd`'s shortcut in keybindings.json (#612) and
    /// reload; `previous` are the other commands the chord ran until now.
    pub(super) fn write_shortcut(
        &mut self,
        cmd: crate::widgets::command_palette::Command,
        key: KeyEvent,
        previous: &[crate::widgets::command_palette::Command],
    ) {
        let chord = crate::keymap::Chord::from_event(key).to_config_string();
        let path = self.keybindings_file.clone();
        // Only a missing file starts fresh: one that exists but cannot be
        // read would otherwise be overwritten with this single binding.
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                self.status = format!("Could not read {}: {e}", path.display());
                return;
            }
        };
        let Some(text) = crate::keymap::rebind_text(src.as_deref(), cmd.id(), &chord) else {
            self.status = format!(
                "{} does not parse; fix it before recording a shortcut",
                path.display()
            );
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, text) {
            self.status = format!("Could not write {}: {e}", path.display());
            return;
        }
        let (map, _) = crate::keymap::Keymap::load_with_warnings(&path);
        self.keymap = map;
        self.status = if previous.is_empty() {
            format!("{chord} now runs \u{201c}{}\u{201d}", cmd.title())
        } else {
            format!(
                "{chord} now runs \u{201c}{}\u{201d} (it ran {})",
                cmd.title(),
                quoted_titles(previous)
            )
        };
    }

    /// Apply croft's own config files that changed on disk outside croft
    /// (#262): a file that arrived by config sync, or was written by another
    /// tool. Returns true when something was reloaded, for a redraw.
    pub fn tick_config_watch(&mut self) -> bool {
        self.tick_config_watch_at(std::time::Instant::now())
    }

    pub(super) fn tick_config_watch_at(&mut self, now: std::time::Instant) -> bool {
        let changed = self.config_watch.poll(now);
        for path in &changed {
            self.reload_config_for_path(path);
        }
        !changed.is_empty()
    }

    /// Whether `path` is one of the workspace's project snippets files, or a
    /// folder's `.vscode` directory, whose change can add or remove one.
    pub(super) fn is_project_snippets_path(&self, path: &std::path::Path) -> bool {
        self.roots.iter().any(|root| {
            path == root.join(".vscode")
                || (path.starts_with(root) && crate::snippets::is_project_file(path))
        })
    }

    /// Reload every snippet (#1484) and watch any project snippets file
    /// that has appeared, so a later edit to it, by croft or by `git pull`,
    /// applies too.
    pub(super) fn reload_project_snippets(&mut self) {
        self.snippets = load_all_snippets(self.roots.iter());
        self.watch_project_snippets();
    }

    /// Put each workspace folder's `.vscode` directory and project snippets
    /// files under the config watch (#1484).
    pub(super) fn watch_project_snippets(&mut self) {
        for path in self.project_snippet_watch_paths() {
            self.config_watch.watch(path);
        }
    }

    /// What [`Self::watch_project_snippets`] watches: each workspace
    /// folder's `.vscode` directory, its `.croft/snippets.json` and its
    /// project snippets files.
    pub(super) fn project_snippet_watch_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for root in self.roots.iter() {
            paths.push(root.join(".vscode"));
            // Watched before it exists, so creating or pulling it applies.
            paths.push(root.join(".croft").join("snippets.json"));
            paths.extend(crate::snippets::project_files(root));
        }
        paths
    }

    /// After a save, if the file was one of croft's JSON configs, re-read it so
    /// the change applies without a relaunch (VS Code applies keybindings and
    /// snippets on save). settings.json fields are read at startup, so that one
    /// leaves its own "next launch" note rather than reloading here.
    pub(super) fn reload_config_if_needed(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        self.reload_config_for_path(&path);
        // The explicit-save path's other follow-up: a save of an agent's
        // proposal being edited approves it (#347).
        self.approve_saved_edit(&path);
    }

    /// Reload whichever of croft's own config files `path` is, if any.
    /// Shared by the explicit-save path (active tab) and the auto-save
    /// sweep (every written tab, background and inactive splits included).
    pub(super) fn reload_config_for_path(&mut self, path: &std::path::Path) {
        // Applied now, so the config watch does not reload it a second time.
        self.config_watch.note(path);
        if path == crate::keymap::keybindings_path() {
            // The non-reporting loader: this path needs the warnings itself
            // for the status summary below, and `load` would print each one to
            // OUTPUT a second time.
            let (map, warns) = crate::keymap::Keymap::load_with_warnings(path);
            self.keymap = map;
            // Refused rows used to vanish, which makes a mistyped binding
            // feel like croft is broken. Same treatment the settings loader
            // gives its warnings: OUTPUT for the detail, a status summary so
            // the user knows to look (#259).
            for w in &warns {
                crate::output::push("Keybindings", crate::output::OutputLevel::Warn, w);
            }
            self.status = keybindings_reload_status(&warns);
        } else if path == crate::snippets::snippets_path() {
            self.snippets = load_all_snippets(self.roots.iter());
            self.status =
                snippets_reload_status(self.snippets.is_broken(), self.snippets.warnings());
        } else if self.is_project_snippets_path(path) {
            // A project's snippets file, or its `.vscode` folder gaining or
            // losing one (#1484).
            // A `.vscode` change that touched no snippets (settings.json
            // saved) says nothing.
            let before = self.snippets.clone();
            self.reload_project_snippets();
            if self.snippets != before {
                self.status = format!(
                    "Project snippets reloaded{}",
                    match self.snippets.warnings().len() {
                        0 => String::new(),
                        n => format!(
                            " with {n} warning{} — see OUTPUT · Snippets",
                            if n == 1 { "" } else { "s" }
                        ),
                    }
                );
            }
        } else if path == crate::agents::agents_path() {
            self.agents = crate::agents::AgentTable::load(path);
            let dropped = self.agents.dropped_patterns();
            self.status = format!(
                "Agent lanes reloaded ({} agents known{})",
                self.agents.names().len(),
                if dropped > 0 {
                    format!(
                        ", {dropped} prompt pattern{} did not compile",
                        if dropped == 1 { "" } else { "s" }
                    )
                } else {
                    String::new()
                }
            );
        } else if path == self.macros_path {
            self.macro_registers = crate::macros::load(path);
            self.status = format!("Macros reloaded ({} registers)", self.macro_registers.len());
        } else if path == crate::triggers::triggers_path() {
            self.triggers = load_trigger_set(self.secret_redaction);
            self.status = match &self.triggers.problem {
                Some(problem) => format!(
                    "Triggers reloaded ({} of your rules active; {problem})",
                    self.triggers.user_rules
                ),
                None => format!(
                    "Triggers reloaded ({} of your rules active)",
                    self.triggers.user_rules
                ),
            };
        } else if path == crate::problem_matchers::matchers_path()
            || path == crate::problem_matchers::workspace_matchers_path(self.workspace_root())
        {
            self.reload_matchers();
        } else if path == crate::prefs::config_path()
            || self.settings_chain.iter().any(|p| p == path)
        {
            self.remerge_settings();
        }
    }

    /// Apply the project's `files_exclude` / `search_exclude` globs
    /// (#1345): the Explorer hides `files_exclude`, Search and Go to File
    /// leave out both. Re-lists the tree and re-runs a live search only
    /// when the globs changed.
    pub(super) fn apply_project_excludes(&mut self, p: &crate::prefs::Prefs) {
        let mut search = p.files_exclude.clone();
        search.extend(p.search_exclude.iter().cloned());
        if search != self.search.project_exclude {
            self.search.project_exclude = search;
            if !crate::widgets::search::as_typed(&self.search.query).is_empty() {
                self.submit_search_query();
            }
        }
        if p.files_exclude != self.files_exclude {
            self.files_exclude = p.files_exclude.clone();
            self.tree.exclude = crate::widgets::search::PathFilter::excluding(&self.files_exclude);
            self.tree.refresh_children(0);
        }
    }

    /// Apply `files_associations` (#1574): a file opens in the language a
    /// glob names for it. Open tabs still in the language croft picked for
    /// them follow a change; one whose language was picked by hand keeps it.
    pub(super) fn apply_file_associations(&mut self, p: &crate::prefs::Prefs) {
        self.install_file_associations(p.files_associations.clone());
    }

    /// Install `map` for every workspace root (#1574), clearing it from a
    /// root that left the workspace. Runs again when the roots change, so
    /// a folder added gets the associations too. An open file whose
    /// language server changes with them is closed on the old server; the
    /// next sync opens it on the new one.
    pub(super) fn install_file_associations(
        &mut self,
        map: std::collections::BTreeMap<String, String>,
    ) {
        let roots: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
        if map == self.files_associations && roots == self.files_associations_roots {
            return;
        }
        let picked = |e: &crate::widgets::editor::Editor| {
            let path = e.path.as_ref()?;
            (!e.has_non_text_view()
                && !e.language_picked_by_hand()
                && e.language() == crate::highlight::lang_for_path(path))
            .then(|| path.clone())
        };
        let following: Vec<PathBuf> = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter_map(picked)
            .collect();
        let servers_before: Vec<(PathBuf, Option<crate::lsp::Language>)> = self
            .lsp_last_seen
            .keys()
            .map(|p| (p.clone(), crate::lsp::manager::path_to_language(p)))
            .collect();
        let empty = std::collections::BTreeMap::new();
        for gone in self
            .files_associations_roots
            .iter()
            .filter(|r| !roots.contains(r))
        {
            crate::highlight::set_file_associations(gone, &empty);
        }
        for root in &roots {
            crate::highlight::set_file_associations(root, &map);
        }
        self.files_associations = map;
        self.files_associations_roots = roots;
        let mut dropped_any = false;
        for (path, before) in servers_before {
            if crate::lsp::manager::path_to_language(&path) == before {
                continue;
            }
            if let Some(lsp) = self.lsp.as_ref() {
                lsp.close_doc(path.clone());
            }
            self.lsp_last_seen.remove(&path);
            dropped_any |= self.lsp_diagnostics.remove(&path).is_some();
            self.code_lens_requested.remove(&path);
        }
        if dropped_any {
            self.rebuild_problems();
            self.refresh_problems_badge();
        }
        for e in std::iter::once(&mut self.editor)
            .chain(self.editor_layout.inactive_groups_mut())
            .flat_map(|g| g.editors.iter_mut())
        {
            let Some(path) = e.path.clone().filter(|p| following.contains(p)) else {
                continue;
            };
            let lang = crate::highlight::lang_for_path(&path);
            if lang != e.language() {
                e.set_language(lang);
            }
        }
    }

    /// Apply `files_encoding` (#1432) to every folder of the workspace: the
    /// next file opened there without a BOM decodes in it. Open tabs keep
    /// the encoding they were decoded with, so a settings change never
    /// re-reads a buffer from under unsaved edits.
    pub(super) fn apply_files_encoding(&mut self, p: &crate::prefs::Prefs) {
        let charset = crate::file_encoding::parse(&p.files_encoding);
        self.files_encoding = charset;
        for root in self.roots.iter() {
            crate::file_encoding::set_workspace_default(root, charset);
        }
    }

    /// Re-run the layered settings merge (#251) and apply everything that can
    /// apply live: theme, editor toggles, save behavior, host accents. Called
    /// when any file of the chain is saved in the editor or changes on disk
    /// outside croft (#1435); layout and other
    /// startup-read settings still say "next launch".
    pub(super) fn remerge_settings(&mut self) {
        let merged = crate::config_layers::load_merged(Some(self.roots.primary()));
        for w in &merged.warnings {
            crate::output::push("Settings", crate::output::OutputLevel::Warn, w);
        }
        self.settings_chain = merged.chain;
        // The chain follows the primary root and the `extends` targets, so
        // the watch follows the chain (#1435).
        // The project snippets stay watched too (#1484).
        let mut paths = config_watch_paths(&self.settings_chain);
        for path in self.project_snippet_watch_paths() {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        self.config_watch.set_paths(paths);
        self.settings_provenance = merged.provenance;
        self.apply_merged_settings(&merged.prefs);
        self.status = if let Some(error) = &merged.user_config_error {
            settings_not_loaded_status(error)
        } else if merged.warnings.is_empty() {
            String::from("Settings reloaded")
        } else {
            format!(
                "Settings reloaded with {} warning{} — see OUTPUT · Settings",
                merged.warnings.len(),
                if merged.warnings.len() == 1 { "" } else { "s" }
            )
        };
    }

    /// Say on the status line that a setting change was not saved (#923),
    /// most often because `config.json` does not parse and croft will not
    /// write defaults over it. The change still holds for this session; a
    /// confirmation such as "Inlay Hints: off" alone read as saved, and the
    /// setting was back after a restart. Call it after the confirmation.
    pub(super) fn report_unsaved_setting(&mut self, saved: anyhow::Result<()>) {
        if let Err(e) = saved {
            self.status = format!("Setting not saved: {e:#}");
        }
    }

    /// Apply a merged settings view to the live session. Mirrors what the
    /// individual toggles do, minus their persistence (the values already
    /// live in config files) and minus their status chatter.
    pub(super) fn apply_merged_settings(&mut self, p: &crate::prefs::Prefs) {
        self.notifier = crate::notifications::Notifier::new(&p.notifications);
        let theme = p.theme();
        if theme != self.theme {
            // Visuals only: a workspace-set theme must not be written back
            // into the user's config.
            self.apply_theme_visuals(theme);
        }
        self.format_on_save = p.format_on_save;
        self.default_formatter = p.default_formatter.clone();
        self.language_formatters = p.language_formatters.clone();
        self.format_on_type = p.format_on_type;
        let old_languages = std::mem::replace(&mut self.language_prefs, p.languages.clone());
        self.auto_save = p.auto_save;
        self.auto_save_on_focus_change = p.auto_save_on_focus_change;
        self.copy_on_select = p.copy_on_select;
        self.apply_project_excludes(p);
        self.tree.set_compact_folders(!p.disable_compact_folders);
        self.apply_file_associations(p);
        self.apply_files_encoding(p);
        // The ssh-pane offer's switches apply live like every other pref
        // here (#364); turning it off also takes down an offer on screen.
        self.remote_offer_disabled = p.disable_remote_offer;
        self.lane_agent = p.lane_agent.clone();
        if p.codeql_cli_path != self.codeql_cli_setting {
            self.codeql_cli_setting = p.codeql_cli_path.clone();
            self.resolve_codeql_cli();
        }
        // Live like every other pref here (#363): editing a group in
        // config.json takes effect on the next fleet run without a restart.
        self.fleet_groups = p.fleet_groups.clone();
        self.code_scanning = p.code_scanning;
        self.multiline_paste_warning = p.terminal_multiline_paste_warning;
        let was_excluded = std::mem::replace(
            &mut self.remote_offer_excluded,
            p.remote_offer_excluded_hosts.clone(),
        );
        // Sampling stops while the offer is off, so the per-pane memory stops
        // tracking reality; kept, it would make the first session seen after
        // a re-enable look like a continuation of the last one seen before
        // it, and that pane would never be offered that host again (#364).
        if self.remote_offer_disabled {
            self.ssh_offer_seen.clear();
        }
        // A host taken OFF the exclusion list: a pane already on it was seen
        // while excluded and would otherwise never be offered for the
        // session it is in. Forgetting what those panes were seen on makes
        // the next sample read as a session start, which is the moment the
        // user asked to be offered at.
        let freed: Vec<&String> = was_excluded
            .iter()
            .filter(|h| {
                !self
                    .remote_offer_excluded
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(h))
            })
            .collect();
        if !freed.is_empty() {
            self.ssh_offer_seen.retain(|_, seen| {
                !seen
                    .as_deref()
                    .is_some_and(|h| freed.iter().any(|f| f.eq_ignore_ascii_case(h)))
            });
        }
        if let Some(o) = self.ssh_offer.as_ref()
            && !crate::remote::offer_allowed(
                &o.host,
                self.remote_offer_disabled,
                &self.remote_offer_excluded,
                &self.remote_offer_refused,
                std::time::SystemTime::now(),
            )
        {
            self.dismiss_ssh_offer();
        }
        // Only on a real change: the startup remerge runs before any log is
        // open and the field already holds the preference, and writing the
        // process-wide default from here under test would race the tests
        // that read it (see `seed_log_highlight_default`).
        let want_highlight = !p.disable_log_highlight;
        if want_highlight != self.log_highlight {
            self.set_log_highlight(want_highlight);
        }
        self.build_column_unit = crate::build_matchers::ColumnUnit::from_pref(&p.build_column_unit);
        self.auto_close_pairs = !p.disable_auto_close_pairs;
        self.editor.auto_close_pairs = self.auto_close_pairs;
        self.paste_reindent = !p.disable_paste_reindent;

        self.save_cleanup = crate::widgets::editor::SaveCleanup::from_prefs(p);
        self.follow_vim_mode_pref(p.vim_mode);
        if !p.disable_inline_blame && !self.inline_blame_enabled {
            // Re-enabling refetches, like toggle_inline_blame.
            self.blame_fetched = None;
        }
        self.inline_blame_enabled = !p.disable_inline_blame;
        self.editor.blame_enabled = self.inline_blame_enabled;
        self.indent_guides_enabled = !p.disable_indent_guides;
        self.editor.show_indent_guides = self.indent_guides_enabled;
        self.home_walks_rows = p.home_walks_wrapped_rows;
        self.bracket_colors_enabled = !p.disable_bracket_colors;
        self.editor.show_bracket_colors = self.bracket_colors_enabled;
        self.whitespace_mode =
            crate::widgets::editor::WhitespaceMode::from_pref(&p.render_whitespace);
        self.editor.whitespace_mode = self.whitespace_mode;
        self.rulers = crate::widgets::editor::normalize_rulers(&p.rulers);
        // Straight onto every open editor: the focus sync that otherwise
        // carries them does not run per frame, so a reload would leave open
        // tabs on the old rulers and Home walk until an unrelated focus
        // change.
        for ed in self.editor.editors.iter_mut() {
            ed.rulers.clone_from(&self.rulers);
            ed.home_walks_rows = self.home_walks_rows;
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                ed.rulers.clone_from(&self.rulers);
                ed.home_walks_rows = self.home_walks_rows;
            }
        }
        self.line_numbers = crate::widgets::editor::LineNumbers::from_pref(&p.line_numbers);
        self.apply_line_numbers();
        self.word_suggestions =
            crate::word_suggest::WordSuggestions::from_pref(&p.word_based_suggestions);
        self.quick_suggestions = !p.disable_quick_suggestions;
        // A per-language `disable_inlay_hints` change (#1440) moves a file's
        // effective setting as surely as the global one does.
        let hint_overrides =
            |langs: &std::collections::BTreeMap<String, crate::prefs::LanguagePrefs>| {
                langs
                    .iter()
                    .filter_map(|(k, l)| l.disable_inlay_hints.map(|off| (k.clone(), off)))
                    .collect::<Vec<_>>()
            };
        let hints_were_on = self.inlay_hints_enabled;
        self.inlay_hints_enabled = !p.disable_inlay_hints;
        if hints_were_on != self.inlay_hints_enabled
            || hint_overrides(&old_languages) != hint_overrides(&self.language_prefs)
        {
            let (langs, global) = (&self.language_prefs, self.inlay_hints_enabled);
            if !inlay_hints_on_in(langs, global, editor_scope_id(&self.editor)) {
                self.editor.clear_inlay_hints();
            }
            for group in self.editor_layout.inactive_groups_mut() {
                if !inlay_hints_on_in(langs, global, editor_scope_id(group)) {
                    group.clear_inlay_hints();
                }
            }
            self.request_inlay_hints_for_open_editors();
        }
        if p.disable_inline_values && self.inline_values_enabled {
            self.inline_values_enabled = false;
            self.clear_inline_values();
        } else if !p.disable_inline_values && !self.inline_values_enabled {
            self.inline_values_enabled = true;
            self.refresh_inline_values();
        }
        self.save_before_debug = !p.disable_save_before_debug;
        self.explorer_views = ExplorerViewVisibility::from_prefs(p.explorer_views);
        self.set_host_accents(&p.host_accents);
    }
}

/// Load the snippets file, writing what could not be read to OUTPUT ·
/// Snippets: one bad entry used to empty the whole set without a word (#1483).
pub(super) fn load_snippets(path: &std::path::Path) -> crate::snippets::SnippetSet {
    let set = crate::snippets::SnippetSet::load(path);
    for w in set.warnings() {
        crate::output::push("Snippets", crate::output::OutputLevel::Warn, w);
    }
    set
}

/// The user's snippets, then each workspace folder's project snippets
/// (#1484): `.vscode/*.code-snippets` and `.croft/snippets.json`. The user's
/// come first, so their own snippet wins a prefix both define. What a
/// project file could not load goes to OUTPUT · Snippets under its path.
pub(super) fn load_all_snippets<'a>(
    roots: impl IntoIterator<Item = &'a std::path::Path>,
) -> crate::snippets::SnippetSet {
    let mut set = load_snippets(&crate::snippets::snippets_path());
    for root in roots {
        for file in crate::snippets::project_files(root) {
            let project = crate::snippets::SnippetSet::load(&file);
            for w in project.warnings() {
                let w = w.replacen("snippets.json could not be read", "could not be read", 1);
                crate::output::push(
                    "Snippets",
                    crate::output::OutputLevel::Warn,
                    &format!("{}: {w}", file.display()),
                );
            }
            set.append(project);
        }
    }
    set
}

/// The status line shown after a snippets reload, tested the same way. A
/// file that loaded nothing says so, with the detail in OUTPUT (#1191), as
/// keybindings.json does; skipped entries are counted (#1483).
pub(super) fn snippets_reload_status(broken: bool, warns: &[String]) -> String {
    if broken {
        return String::from(
            "Snippets not loaded: the file does not parse — see OUTPUT · Snippets",
        );
    }
    match warns.len() {
        0 => String::from("Snippets reloaded"),
        n => format!(
            "Snippets reloaded with {n} warning{} — see OUTPUT · Snippets",
            if n == 1 { "" } else { "s" }
        ),
    }
}

/// The status line shown after a keybindings reload.
///
/// Split out of `reload_config_for_path` so it can be tested against a keymap
/// built in memory. The alternative — a test that writes the real
/// `~/.config/croft/keybindings.json` — would leak into every one of the
/// thousands of concurrent tests that construct an `App`, since `App::new`
/// loads that path unconditionally.
pub(super) fn keybindings_reload_status(warns: &[String]) -> String {
    if warns.is_empty() {
        String::from(
            "Keybindings reloaded (for new Cmd chords, re-run croft setup-iterm2 / setup-ghostty)",
        )
    } else {
        // One assignment: a second unconditional one below used to clobber
        // this, so the user never saw the warning count and a refused row
        // looked like croft ignoring them.
        format!(
            "Keybindings reloaded with {} warning{} — see OUTPUT · Keybindings",
            warns.len(),
            if warns.len() == 1 { "" } else { "s" }
        )
    }
}

/// Which starter contents to seed when a JSON config file is opened for the
/// first time from the palette.
#[derive(Debug, Clone, Copy)]
pub(super) enum ConfigFileSeed {
    Settings,
    /// A settings overlay layer (#251): `.croft/config.json`, its gitignored
    /// `.local` sibling, or the machine-local user overlay. Seeded with a
    /// commented template rather than a full prefs dump — overlays should
    /// carry only the keys they actually set.
    SettingsLayer,
    Keybindings,
    Snippets,
    Triggers,
    Matchers,
    Agents,
}

/// Starter contents for a fresh settings overlay layer.
pub(super) const SETTINGS_LAYER_TEMPLATE: &str = r#"{
  // A croft settings overlay: later layers win key-by-key over the user
  // config (see docs/SETTINGS.md for the chain, "extends", and platform
  // blocks). Workspace files may set appearance and editor/terminal
  // behavior keys only; trust-related settings are user-config only and
  // warn in OUTPUT · Settings.
}
"#;

/// What Open Settings (JSON) creates when there is no `config.json` yet: an
/// object with no keys, like VS Code's settings.json. Seeding every default
/// made each one a user setting, over an `extends` base and over defaults a
/// later release changes (#1617).
pub(super) const SETTINGS_TEMPLATE: &str = r#"{
  // Your croft settings: add only the keys you want to change, and every
  // other setting keeps its default. docs/SETTINGS.md lists the keys and
  // explains "extends" and platform blocks.
}
"#;
