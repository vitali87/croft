//! The BREAKPOINTS list in the Run and Debug view: its rows, keeping it current, a click
//! on one, and removing a breakpoint from it.

use super::*;

/// The BREAKPOINTS section of the debug tree (#250): a header and one row per
/// breakpoint, or nothing when there are none.
pub(in crate::app) fn breakpoint_section_rows(
    items: &[crate::widgets::run_debug::BreakpointItem],
) -> Vec<crate::widgets::run_debug::DebugRow> {
    use crate::widgets::run_debug::{DebugRow, DebugRowKind};
    if items.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![DebugRow {
        indent: 0,
        kind: DebugRowKind::Header {
            title: String::from("BREAKPOINTS"),
        },
    }];
    rows.extend(items.iter().enumerate().map(|(index, item)| DebugRow {
        indent: 1,
        kind: DebugRowKind::Breakpoint {
            index,
            path: item.path.clone(),
            line: item.line,
        },
    }));
    rows
}

impl App {
    /// Every breakpoint for the BREAKPOINTS list (#250), by file then line,
    /// with its condition or log message.
    pub(in crate::app) fn breakpoint_items(
        &self,
    ) -> Vec<crate::widgets::run_debug::BreakpointItem> {
        let mut paths: Vec<&PathBuf> = self.editor.breakpoints.keys().collect();
        paths.sort();
        let mut items = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            for &line in &self.editor.breakpoints[path] {
                let condition = self
                    .editor
                    .breakpoint_conditions
                    .get(path)
                    .and_then(|m| m.get(&line));
                let log = self
                    .editor
                    .breakpoint_logs
                    .get(path)
                    .and_then(|m| m.get(&line));
                let detail = match (log, condition) {
                    (Some(msg), _) => Some(format!("log {msg}")),
                    (None, Some(cond)) => Some(format!("if {cond}")),
                    (None, None) => None,
                };
                items.push(crate::widgets::run_debug::BreakpointItem {
                    path: path.clone(),
                    line,
                    label: format!("{name}:{line}"),
                    detail,
                });
            }
        }
        items
    }

    /// Bring the panel's BREAKPOINTS list up to date, and the tree's section
    /// with it while a session is shown. A no-op when nothing changed.
    pub(in crate::app) fn sync_breakpoint_list(&mut self) {
        use crate::widgets::run_debug::DebugRowKind;
        let items = self.breakpoint_items();
        if items == self.run_debug.breakpoints {
            return;
        }
        let section = breakpoint_section_rows(&items);
        self.run_debug.breakpoints = items;
        if self.run_debug.debug_active {
            // Through `replace_rows`, so the keyboard's row stays on its own
            // breakpoint when one above it goes (#2748).
            let mut rows = self.run_debug.debug_rows.clone();
            if let Some(pos) = rows.iter().position(
                |r| matches!(&r.kind, DebugRowKind::Header { title } if title == "BREAKPOINTS"),
            ) {
                rows.truncate(pos);
            }
            rows.extend(section);
            self.run_debug.replace_rows(rows);
        }
    }

    /// A click in the BREAKPOINTS list: jump to the breakpoint, or remove it
    /// when the click landed on its `✕`.
    pub(in crate::app) fn breakpoint_list_click(&mut self, index: usize, remove: bool) {
        let Some(bp) = self.run_debug.breakpoints.get(index).cloned() else {
            return;
        };
        if remove {
            self.remove_breakpoint_at(&bp.path, bp.line);
        } else if let Err(e) = self.open_at(&bp.path, bp.line.saturating_sub(1), 0) {
            self.status = format!("Could not open {}: {e}", bp.path.display());
        }
        self.sync_breakpoint_list();
    }

    /// Remove the breakpoint at `path:line` (1-based) with its condition or
    /// log message, and push the file's breakpoints to every live session so
    /// it stops binding mid-run too, as F9 does.
    pub(in crate::app) fn remove_breakpoint_at(&mut self, path: &Path, line: usize) {
        if let Some(lines) = self.editor.breakpoints.get_mut(path) {
            lines.remove(&line);
            if lines.is_empty() {
                self.editor.breakpoints.remove(path);
            }
        }
        if let Some(m) = self.editor.breakpoint_conditions.get_mut(path) {
            m.remove(&line);
        }
        if let Some(m) = self.editor.breakpoint_logs.get_mut(path) {
            m.remove(&line);
        }
        let specs = self
            .editor
            .breakpoints
            .get(path)
            .map(|lines| self.editor.source_breakpoints(path, lines))
            .unwrap_or_default();
        let owned = path.to_path_buf();
        for session in self.debug_sessions.iter_mut() {
            session.update_breakpoints(&owned, &specs);
        }
        self.status = format!(
            "Breakpoint removed from {}:{line}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
    }
}
