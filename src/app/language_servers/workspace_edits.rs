//! Applying a server's workspace edit: open buffers in any split edited in memory as one
//! undo step, closed files written on disk, and refusing an edit to a buffer typed into
//! since the request.

use super::*;

impl App {
    /// The first file of a workspace edit whose open buffer changed after
    /// `baseline` was taken, named for the status bar (#1151).
    pub(in crate::app) fn stale_edit_target(
        &self,
        baseline: &[(PathBuf, u64)],
        files: &[(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)],
    ) -> Option<String> {
        files
            .iter()
            .filter(|(_, edits)| !edits.is_empty())
            .find(|(path, _)| {
                self.buffer_group(path)
                    .is_some_and(|group| group.edited_since(baseline, path))
            })
            .map(|(path, _)| self.status_path(path))
    }

    /// Every open tab's file and `edit_seq`, in every split group: the
    /// baseline a workspace edit is checked against when it lands (#1151).
    pub(in crate::app) fn edit_seqs_in_every_group(&self) -> Vec<(PathBuf, u64)> {
        std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|group| group.edit_seqs())
            .collect()
    }

    /// The editor group whose tab holds `path`'s text, the active group
    /// first: the one [`Self::apply_edits_to_open_buffer`] edits. A split
    /// may show only a diff of the file while another holds the file
    /// itself (#1113).
    pub(in crate::app) fn buffer_group(&self, path: &Path) -> Option<&EditorTabs> {
        std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .find(|group| group.find_buffer_tab(path).is_some())
    }

    /// Apply workspace `edits` to the tab holding `path`'s text in any
    /// split group, the active group first. `None` when no group holds it,
    /// so the caller edits the file on disk.
    pub(in crate::app) fn apply_edits_to_open_buffer(
        &mut self,
        path: &Path,
        edits: &[crate::widgets::editor::TextSpanEdit],
    ) -> Option<usize> {
        if let Some(n) = self.editor.apply_rename_to_open_tab(path, edits) {
            return Some(n);
        }
        self.editor_layout
            .inactive_groups_mut()
            .into_iter()
            .find_map(|group| group.apply_rename_to_open_tab(path, edits))
    }

    /// Apply an LSP rename `WorkspaceEdit` (already normalised to per-file
    /// char-indexed spans). Open tabs, in any split group, are edited
    /// in-memory as one undo step and left dirty; closed files are rewritten
    /// on disk (the fs watcher refreshes the tree) and the servers are told
    /// at once (#908), so the next Go to Definition sees the new name.
    /// Returns (files touched, occurrences replaced).
    pub(in crate::app) fn apply_rename_edits(
        &mut self,
        files: &[(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)],
    ) -> Result<(usize, usize)> {
        let mut file_count = 0;
        let mut occ_count = 0;
        let mut written = std::collections::BTreeSet::new();
        for (path, edits) in files {
            if edits.is_empty() {
                continue;
            }
            if let Some(n) = self.apply_edits_to_open_buffer(path, edits) {
                occ_count += n;
            } else if self.is_collab_guest() && collab_file_key(&self.tree.root, path).is_some() {
                // Closed-file rename edits are raw disk writes; a collab
                // guest never writes shared files (open tabs above are fine:
                // those edits flow to the owner as ops).
                continue;
            } else {
                let content = std::fs::read_to_string(path)?;
                let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
                occ_count += crate::widgets::editor::apply_span_edits_to_lines(&mut lines, edits);
                std::fs::write(path, lines.join("\n"))?;
                written.insert(path.clone());
            }
            file_count += 1;
        }
        self.forward_disk_changes_to_lsp(&fs_watch::FsDrain {
            changed_files: written,
            ..Default::default()
        });
        Ok((file_count, occ_count))
    }
}
