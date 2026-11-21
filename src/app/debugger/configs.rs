//! Launch configurations: Debug: Add Configuration's form, writing the draft to
//! `.croft/launch.json`, and Debug: Select and Start Debugging's picker.

use super::*;

impl App {
    /// Debug: Add Configuration… (#250): the native form for a launch.json
    /// entry, one step at a time, written to `.croft/launch.json` at the end.
    /// Starts with the adapter.
    pub fn open_add_debug_config(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        self.debug_config_draft = None;
        let rows = crate::dap::configs::DRAFT_TYPES
            .iter()
            .map(|(id, label)| ListRow {
                id: (*id).to_string(),
                label: (*label).to_string(),
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(
                ListPurpose::DebugConfigType,
                "Add Configuration: Debugger",
                rows,
            ),
            "",
        );
    }

    /// Ask for `field` of the draft, seeded with `retry` (a value just
    /// refused) or the field's default; `None` means the draft is complete,
    /// so write it.
    pub(in crate::app) fn prompt_debug_config_field(
        &mut self,
        field: Option<crate::dap::configs::DraftField>,
        retry: Option<&str>,
    ) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let Some(draft) = self.debug_config_draft.as_ref() else {
            return;
        };
        let Some(field) = field else {
            self.finish_debug_config_draft();
            return;
        };
        let (title, hint, seed) = draft.prompt(field);
        let mut prompt = InputPrompt::new(InputPurpose::DebugConfigField { field }, title, hint)
            .with_value(retry.map_or(seed, str::to_string));
        if crate::dap::configs::ConfigDraft::optional(field) {
            prompt = prompt.allowing_blank();
        }
        self.open_input_prompt(prompt);
    }

    /// Take a field's value: on to the next field, or back to this one with
    /// the reason when it does not fit.
    pub(in crate::app) fn submit_debug_config_field(
        &mut self,
        field: crate::dap::configs::DraftField,
        value: &str,
    ) {
        let Some(draft) = self.debug_config_draft.as_mut() else {
            return;
        };
        match draft.set(field, value) {
            Ok(()) => {
                let next = draft.next_field(Some(field));
                self.prompt_debug_config_field(next, None);
            }
            Err(why) => {
                self.status = why;
                self.prompt_debug_config_field(Some(field), Some(value));
            }
        }
    }

    /// Write the finished draft to `.croft/launch.json` and pick up the file
    /// again, so the new configuration is in the picker and F5 at once.
    pub(in crate::app) fn finish_debug_config_draft(&mut self) {
        let Some(draft) = self.debug_config_draft.take() else {
            return;
        };
        let root = self.active_workspace_root();
        match crate::dap::configs::add_to_croft_launch_json(&root, &draft) {
            Ok(_) => {
                self.debug_configs = crate::dap::configs::discover_configs(&root);
                self.selected_debug_config = Some(draft.name.clone());
                self.selected_debug_compound = None;
                self.run_debug.selected_config = Some(draft.name.clone());
                self.status = format!(
                    "Added \"{}\" to .croft/launch.json and selected it; F5 starts it",
                    draft.name
                );
            }
            Err(why) => self.status = why,
        }
    }

    /// Debug: Select and Start Debugging — the launch.json configurations
    /// plus the synthesized zero-config entry.
    pub fn open_debug_config_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let root = self.active_workspace_root();
        self.debug_configs = crate::dap::configs::discover_configs(&root);
        self.debug_compounds = crate::dap::configs::discover_compounds(&root);
        let mut rows = vec![ListRow {
            id: String::from("active"),
            label: String::from("Debug active file — no configuration"),
        }];
        // Configurations and compounds are ONE list to sort, which is what
        // VS Code does: a config and a compound can name the same
        // `presentation.group`, and sorting them separately would split that
        // group in two no matter what the file asked for (#318).
        //
        // Every id is built BEFORE the sort, from `enumerate` over the full
        // unsorted list, so `compound:{i}` and `{i}` keep naming the same
        // entries however the rows move. Indexing the sorted sequence would
        // launch a different configuration than the row says as soon as any
        // entry declares a `presentation`.
        let entries: Vec<(ListRow, Option<crate::dap::configs::Presentation>)> = self
            .debug_configs
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    ListRow {
                        id: i.to_string(),
                        label: format!("{} — {} · {}", c.name, c.type_name, c.source),
                    },
                    c.presentation.clone(),
                )
            })
            // Compounds are listed so a workspace's own launch.json is
            // reflected honestly. Selecting one LAUNCHES it when it names a
            // single configuration and carries no key croft cannot honour;
            // otherwise it reports what is missing (#310) rather than
            // launching a subset, which would debug something other than what
            // was asked for.
            .chain(self.debug_compounds.iter().enumerate().map(|(i, c)| {
                (
                    ListRow {
                        id: format!("compound:{i}"),
                        label: format!(
                            "{} — compound of {}",
                            c.name,
                            c.configurations.join(", ")
                        ),
                    },
                    c.presentation.clone(),
                )
            }))
            .collect();
        rows.extend(
            crate::dap::configs::visible_and_sorted(entries, |(_, p)| p.as_ref())
                .into_iter()
                .map(|(row, _)| row),
        );
        rows.push(ListRow {
            id: String::from("add"),
            label: String::from("Add Configuration…"),
        });
        self.open_list_picker(
            ListPicker::new(ListPurpose::DebugConfig, "Debug Configuration", rows),
            "No debug configurations (.croft/launch.json or .vscode/launch.json)",
        );
    }
}
