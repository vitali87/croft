//! Watch expressions: adding and removing them, evaluating them at each stop, and what
//! they keep when a session ends.

use super::*;

impl App {
    /// Re-evaluate every watch expression against the current stop. The
    /// current values become the previous stop's baseline first, so the
    /// changed-value highlight compares stop-to-stop (#112).
    pub(in crate::app) fn refresh_watches(&mut self) {
        if self.watch_exprs.is_empty() {
            return;
        }
        if std::mem::take(&mut self.watch_baseline_pending) {
            self.watch_prev = self
                .watch_vals
                .drain()
                .filter(|(_, (_, ok))| *ok)
                .map(|(k, (v, _))| (k, v))
                .collect();
        }
        if let Some(session) = self
            .debug_sessions
            .focused_mut()
            .filter(|s| s.phase == crate::dap::session::SessionPhase::Stopped)
        {
            for expr in &self.watch_exprs {
                session.evaluate(expr, "watch");
            }
        }
    }

    /// Append a watch expression (palette or the panel's add row) and
    /// evaluate it immediately when a session is paused.
    pub(in crate::app) fn add_watch_expression(&mut self, expr: String) {
        let expr = expr.trim().to_string();
        if expr.is_empty() {
            self.status = String::from("Enter an expression to watch");
            return;
        }
        if self.watch_exprs.contains(&expr) {
            self.status = format!("Already watching {expr}");
            return;
        }
        self.watch_exprs.push(expr.clone());
        if let Some(session) = self
            .debug_sessions
            .focused_mut()
            .filter(|s| s.phase == crate::dap::session::SessionPhase::Stopped)
        {
            session.evaluate(&expr, "watch");
        }
        self.status = format!("Watching {expr}");
    }

    pub(in crate::app) fn remove_watch_expression(&mut self, idx: usize) {
        if idx < self.watch_exprs.len() {
            let expr = self.watch_exprs.remove(idx);
            self.watch_vals.remove(&expr);
            self.watch_prev.remove(&expr);
            self.status = format!("Stopped watching {expr}");
        }
    }

    /// The "+ Add Expression" affordance (panel row or palette): a single-line
    /// input popup, per croft's inputs-in-popups rule.
    pub(in crate::app) fn open_add_watch_prompt(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::AddWatch,
            "Add Watch Expression",
            "expression",
        ));
    }
}

impl App {
    /// A session ended: the expressions survive (they usually make sense for
    /// the next run too), but the values, the changed-value baseline, and the
    /// baseline latch are that session's — left in place they made every
    /// watch flash "changed" on a fresh session's first stop.
    pub(in crate::app) fn reset_watch_runtime(&mut self) {
        self.watch_vals.clear();
        self.watch_prev.clear();
        self.watch_baseline_pending = false;
    }
}
