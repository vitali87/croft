//! Inline values: the values of the variables on the paused line, shown after them in the
//! editor, cleared and toggled.

use super::*;

impl App {
    /// Rebuild the inline-value trailers (#135) from the current stop's
    /// already-fetched VARIABLES data, on every editor showing the stopped
    /// file (inactive split groups render too).
    pub(in crate::app) fn refresh_inline_values(&mut self) {
        use crate::dap::session::SessionPhase;
        if !self.inline_values_enabled {
            return;
        }
        let Some(session) = self.debug_sessions.focused() else {
            return;
        };
        if session.phase != SessionPhase::Stopped {
            return;
        }
        let Some((path, line)) = session.current_location.clone() else {
            return;
        };
        let locals = crate::dap::session::inline_locals(&session.scopes, &session.variables);
        for ed in self.editor.editors.iter_mut() {
            if ed.path.as_deref() == Some(path.as_path()) {
                ed.set_inline_values_from_locals(line, &locals);
            }
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                if ed.path.as_deref() == Some(path.as_path()) {
                    ed.set_inline_values_from_locals(line, &locals);
                }
            }
        }
    }

    /// Drop every inline-value trailer, in every editor of every group —
    /// called wherever the stop arrow clears (resume, step, terminate).
    pub(in crate::app) fn clear_inline_values(&mut self) {
        for ed in self.editor.editors.iter_mut() {
            ed.inline_values.clear();
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                ed.inline_values.clear();
            }
        }
    }

    pub(in crate::app) fn toggle_inline_values(&mut self) {
        self.inline_values_enabled = !self.inline_values_enabled;
        if self.inline_values_enabled {
            self.refresh_inline_values();
        } else {
            self.clear_inline_values();
        }
        self.status = format!(
            "Debug Inline Values: {}",
            if self.inline_values_enabled {
                "on"
            } else {
                "off"
            }
        );
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_inline_values(
                self.inline_values_enabled,
            ));
        }
    }
}
