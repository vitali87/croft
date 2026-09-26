//! Screen reader mode (#621).
//!
//! A terminal screen reader (VoiceOver, Orca, NVDA over WSL) follows the
//! terminal's real cursor and reads text that changes around it. croft
//! normally blinks its caret in software and draws everything else as
//! styled cells, which gives a reader little to follow. With the mode on,
//! the caret is steady and always sits on the focused text, and each frame
//! croft compares a [`Snapshot`] of what the user is looking at with the
//! previous one and writes a one-line description of the change
//! ([`announce`]) to the status bar, where the cursor is parked when no text
//! has focus. `screen_reader_command` (for example `spd-say` or `say`)
//! additionally speaks each line through a speech program.

/// What the user is focused on, reduced to the parts worth announcing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The focused area: "Editor main.rs", "Explorer", "Terminal 1", or
    /// the name of an open picker.
    pub focus: String,
    /// Editor caret line (1-based) and its text.
    pub line: Option<(usize, String)>,
    /// Diagnostic under the caret.
    pub diagnostic: Option<String>,
    /// Highlighted completion, palette entry, list row or tree entry.
    pub item: Option<String>,
    /// The transient status message.
    pub status: String,
}

/// The longest line text read out; beyond this a reader drones.
const MAX_LINE: usize = 160;

fn clip(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= MAX_LINE {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_LINE).collect();
    out.push('…');
    out
}

/// A description of what changed from `prev` to `cur`, most important
/// first, or `None` when nothing worth saying did.
pub fn announce(prev: &Snapshot, cur: &Snapshot) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let focus_changed = prev.focus != cur.focus;
    if focus_changed && !cur.focus.is_empty() {
        parts.push(cur.focus.clone());
    }
    let line_moved = cur.line.as_ref().map(|(n, _)| n) != prev.line.as_ref().map(|(n, _)| n);
    if let Some(item) = cur
        .item
        .as_ref()
        .filter(|_| cur.item != prev.item || focus_changed)
    {
        parts.push(clip(item));
    } else if let Some((n, text)) = &cur.line
        && (focus_changed || line_moved || (prev.item.is_some() && cur.item.is_none()))
    {
        let text = clip(text);
        let line = crate::i18n::tr("Line");
        parts.push(if text.is_empty() {
            format!("{line} {n}, {}", crate::i18n::tr("blank"))
        } else {
            format!("{line} {n}: {text}")
        });
    }
    if cur.diagnostic != prev.diagnostic
        && let Some(d) = &cur.diagnostic
    {
        parts.push(clip(d));
    }
    if cur.status != prev.status && !cur.status.is_empty() {
        parts.push(clip(&cur.status));
    }
    (!parts.is_empty()).then(|| parts.join(". "))
}

/// Holds the last snapshot and the line on show, and speaks new lines.
#[derive(Default)]
pub struct Announcer {
    last: Snapshot,
    pub line: String,
    child: Option<std::process::Child>,
}

impl Announcer {
    /// Take in this frame's snapshot. Returns true when the line changed.
    pub fn update(&mut self, cur: Snapshot, speak: Option<&str>) -> bool {
        let said = announce(&self.last, &cur);
        self.last = cur;
        let Some(line) = said else {
            return false;
        };
        if let Some(cmd) = speak {
            self.speak(cmd, &line);
        }
        self.line = line;
        true
    }

    /// Run the speech command with `line` as its last argument, cutting off
    /// whatever it was still saying so speech keeps up with the keys.
    fn speak(&mut self, cmd: &str, line: &str) {
        if let Some(mut old) = self.child.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
        let mut words = cmd.split_whitespace();
        let Some(program) = words.next() else {
            return;
        };
        // A line starting with `-` (a file named `--output-file=x`) would
        // read as the speech program's own option; a leading space is
        // silent and keeps it text.
        let line = if line.starts_with('-') {
            format!(" {line}")
        } else {
            line.to_string()
        };
        self.child = std::process::Command::new(program)
            .args(words)
            .arg(line)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
    }

    /// Forget the previous state, so the next snapshot is announced in full.
    pub fn reset(&mut self) {
        self.last = Snapshot::default();
        self.line.clear();
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(line: usize, text: &str) -> Snapshot {
        Snapshot {
            focus: "Editor main.rs".into(),
            line: Some((line, text.into())),
            ..Default::default()
        }
    }

    #[test]
    fn moving_to_another_line_reads_it_and_staying_says_nothing() {
        let a = editor(3, "fn main() {");
        assert_eq!(
            announce(&Snapshot::default(), &a).as_deref(),
            Some("Editor main.rs. Line 3: fn main() {")
        );
        let b = editor(4, "    ");
        assert_eq!(announce(&a, &b).as_deref(), Some("Line 4, blank"));
        let mut typed = b.clone();
        typed.line = Some((4, "    let".into()));
        assert_eq!(
            announce(&b, &typed),
            None,
            "typing on a line is not re-read"
        );
    }

    #[test]
    fn a_diagnostic_and_a_status_are_added_when_they_appear() {
        let a = editor(3, "x");
        let mut b = a.clone();
        b.diagnostic = Some("error: cannot find value `x`".into());
        b.status = "Saved main.rs".into();
        assert_eq!(
            announce(&a, &b).as_deref(),
            Some("error: cannot find value `x`. Saved main.rs")
        );
    }

    #[test]
    fn a_highlighted_item_is_read_instead_of_the_line() {
        let a = editor(3, "pri");
        let mut b = a.clone();
        b.item = Some("println!".into());
        assert_eq!(announce(&a, &b).as_deref(), Some("println!"));
        let mut back = b.clone();
        back.item = None;
        assert_eq!(
            announce(&b, &back).as_deref(),
            Some("Line 3: pri"),
            "closing a popup re-reads the line"
        );
        let mut c = b.clone();
        c.focus = "Command Palette".into();
        c.item = Some("File: Save".into());
        assert_eq!(
            announce(&b, &c).as_deref(),
            Some("Command Palette. File: Save")
        );
    }

    #[test]
    fn long_lines_are_clipped() {
        let long = "x".repeat(500);
        let said = announce(&Snapshot::default(), &editor(1, &long)).unwrap();
        assert!(said.chars().count() < 200);
        assert!(said.ends_with('…'));
    }

    #[test]
    fn the_announcer_keeps_the_last_line_until_something_changes() {
        let mut a = Announcer::default();
        assert!(a.update(editor(1, "a"), None));
        assert_eq!(a.line, "Editor main.rs. Line 1: a");
        assert!(!a.update(editor(1, "ab"), None));
        assert_eq!(a.line, "Editor main.rs. Line 1: a");
    }
}
