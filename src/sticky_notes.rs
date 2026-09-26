//! Human comment boxes (#367): sticky notes on lines, shared in a session.
//!
//! A note is overlay state, never buffer content: it hangs under a line of a
//! workspace file, carries its author, replies and a resolved flag, and in a
//! `--solo` session replicates to every participant over the collab relay
//! ([`crate::collab::CollabMsg::Note`]). Alone, the same notes are personal
//! sticky notes.
//!
//! Replication merges copies the same way on every peer: every change bumps
//! the note's `rev`, the higher-ranked copy's fields win, replies from both
//! copies are kept, and a deletion is a tombstone (`deleted`) that no older
//! copy can undo.
//!
//! A note remembers the text of its line. Lines inserted or removed above it
//! move that text, and [`Note::place`] finds it again near where it was, so
//! the box follows the code on every participant without tracking edits.
//!
//! The session owner (or a lone croft) keeps the notes in
//! `<cache>/notes/<workspace hash>.json`, so they outlive a disconnect and a
//! restart without writing into the repository.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub author: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Unique across participants: the author's name, their process id and
    /// a counter.
    pub id: String,
    /// Workspace-relative path, forward slashes.
    pub file: String,
    /// 0-based line when last placed.
    pub line: usize,
    /// That line's text, to find it again after edits above it.
    pub anchor: String,
    pub author: String,
    pub body: String,
    #[serde(default)]
    pub replies: Vec<Reply>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub deleted: bool,
    pub rev: u64,
}

impl Note {
    /// The line the note belongs under in `lines`: the nearest line holding
    /// its anchor text anywhere in the buffer (a block pasted far away still
    /// carries its note), else its last line clamped to the buffer.
    pub fn place(&self, lines: &[String]) -> usize {
        let last = lines.len().saturating_sub(1);
        let start = self.line.min(last);
        if lines
            .get(start)
            .is_some_and(|l| l.trim() == self.anchor.trim())
        {
            return start;
        }
        if self.anchor.trim().is_empty() {
            return start;
        }
        for d in 1..=last {
            for cand in [start.checked_sub(d), start.checked_add(d)] {
                if let Some(i) = cand.filter(|&i| i <= last)
                    && lines[i].trim() == self.anchor.trim()
                {
                    return i;
                }
            }
        }
        start
    }

    /// The box body: the note, then each reply.
    pub fn text(&self) -> String {
        let mut s = self.body.clone();
        for r in &self.replies {
            s.push_str(&format!("\n\n{}: {}", r.author, r.body));
        }
        s
    }

    /// The box title.
    pub fn title(&self) -> String {
        if self.resolved {
            format!("{} \u{b7} note \u{b7} resolved", self.author)
        } else {
            format!("{} \u{b7} note", self.author)
        }
    }

    /// A total order on copies of one note: revision first, then the
    /// content itself, so two peers holding different copies of the same
    /// revision pick the same winner.
    fn outranks(&self, other: &Note) -> bool {
        let key = |n: &Note| (n.rev, serde_json::to_string(n).unwrap_or_default());
        key(self) > key(other)
    }

    /// Combine two copies of one note the same way on every peer: fields
    /// from the higher-ranked copy, replies from both (nothing anyone wrote
    /// is lost to a concurrent edit), and a deletion from either sticks.
    fn joined(a: &Note, b: &Note) -> Note {
        let (win, lose) = if a.outranks(b) { (a, b) } else { (b, a) };
        let mut out = win.clone();
        for r in &lose.replies {
            if !out.replies.contains(r) {
                out.replies.push(r.clone());
            }
        }
        out.deleted = a.deleted || b.deleted;
        out
    }
}

/// Every note in a workspace, merged from local changes and peers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Notes {
    notes: Vec<Note>,
    next: u64,
    /// Bumped on every change, so a view can tell when to refresh.
    pub generation: u64,
}

impl Notes {
    /// Take in a copy (local or from a peer). Returns whether it changed
    /// anything.
    pub fn merge(&mut self, note: Note) -> bool {
        match self.notes.iter_mut().find(|n| n.id == note.id) {
            Some(existing) => {
                let joined = Note::joined(existing, &note);
                if &joined == existing {
                    return false;
                }
                *existing = joined;
                self.generation += 1;
                true
            }
            None => {
                self.notes.push(note);
                self.generation += 1;
                true
            }
        }
    }

    /// Live (not deleted) notes on `file`.
    pub fn on_file<'a>(&'a self, file: &'a str) -> impl Iterator<Item = &'a Note> + 'a {
        self.notes
            .iter()
            .filter(move |n| n.file == file && !n.deleted)
    }

    /// Every live note.
    pub fn live(&self) -> impl Iterator<Item = &Note> {
        self.notes.iter().filter(|n| !n.deleted)
    }

    /// Every note including tombstones, for syncing a joiner.
    pub fn all(&self) -> &[Note] {
        &self.notes
    }

    #[cfg(test)]
    pub fn get(&self, id: &str) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id && !n.deleted)
    }

    /// A new note by `author`, merged and returned for broadcasting.
    pub fn add(&mut self, author: &str, file: &str, line: usize, anchor: &str, body: &str) -> Note {
        self.next += 1;
        let note = Note {
            // The clock too: a pid alone recurs across restarts, and a
            // reused id would merge a new note into an old tombstone.
            id: format!(
                "{author}-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
                self.next
            ),
            file: file.to_string(),
            line,
            anchor: anchor.to_string(),
            author: author.to_string(),
            body: body.to_string(),
            replies: Vec::new(),
            resolved: false,
            deleted: false,
            rev: 1,
        };
        self.merge(note.clone());
        note
    }

    /// Apply `change` to note `id` as a new revision; returns the new copy
    /// for broadcasting.
    pub fn update(&mut self, id: &str, change: impl FnOnce(&mut Note)) -> Option<Note> {
        let n = self.notes.iter_mut().find(|n| n.id == id)?;
        change(n);
        n.rev += 1;
        self.generation += 1;
        Some(n.clone())
    }

    /// Where a workspace's notes are kept.
    pub fn store_path(cache: &Path, workspace: &Path) -> PathBuf {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in workspace.to_string_lossy().bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        cache.join("notes").join(format!("{h:016x}.json"))
    }

    pub fn load(path: &Path) -> Self {
        let notes: Vec<Note> = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            notes,
            next: 0,
            generation: 1,
        }
    }

    /// Write every note, tombstones included: a guest that reconnects after
    /// a restart still holds its copy of a deleted note, and only the
    /// tombstone stops that copy bringing the note back.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Written aside and renamed in, so a crash mid-write never leaves a
        // truncated file that `load` reads as no notes at all.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string(&self.notes).unwrap_or_default())?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn a_note_follows_its_line_when_lines_are_added_above() {
        let mut notes = Notes::default();
        let n = notes.add("ada", "a.rs", 1, "fn b() {}", "why?");
        assert_eq!(n.place(&lines(&["fn a() {}", "fn b() {}"])), 1);
        assert_eq!(
            n.place(&lines(&["// new", "// more", "fn a() {}", "fn b() {}"])),
            3
        );
        assert_eq!(n.place(&lines(&["fn b() {}"])), 0, "and when lines go");
        assert_eq!(
            n.place(&lines(&["changed", "also changed"])),
            1,
            "an edited line keeps the old number"
        );
    }

    #[test]
    fn a_note_finds_its_anchor_anywhere_in_the_buffer() {
        let mut notes = Notes::default();
        let n = notes.add("ada", "a.rs", 0, "fn moved() {}", "why?");
        let mut buf = vec![String::from("// filler"); 1000];
        buf.push(String::from("fn moved() {}"));
        assert_eq!(n.place(&buf), 1000);
        let again = notes.add("ada", "a.rs", 0, "x", "y");
        assert_ne!(n.id, again.id);
    }

    #[test]
    fn the_newest_revision_wins_and_a_deletion_sticks() {
        let mut a = Notes::default();
        let n = a.add("ada", "a.rs", 0, "x", "note");
        let mut b = Notes::default();
        assert!(b.merge(n.clone()));
        let resolved = a.update(&n.id, |n| n.resolved = true).unwrap();
        let replied = b
            .update(&n.id, |n| {
                n.replies.push(Reply {
                    author: "bob".into(),
                    body: "ok".into(),
                })
            })
            .unwrap();
        // Same revision from two sides: both converge, and neither the
        // reply nor anything else is lost.
        a.merge(replied.clone());
        b.merge(resolved.clone());
        assert_eq!(a.get(&n.id), b.get(&n.id));
        assert_eq!(a.get(&n.id).unwrap().replies.len(), 1);
        let gone = a.update(&n.id, |n| n.deleted = true).unwrap();
        b.merge(gone);
        assert!(!b.merge(n.clone()), "an old copy can't resurrect it");
        assert!(b.get(&n.id).is_none());
    }

    #[test]
    fn a_deleted_note_stays_deleted_across_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Notes::store_path(tmp.path(), Path::new("/w"));
        let mut n = Notes::default();
        let keep = n.add("ada", "a.rs", 0, "x", "keep");
        let drop = n.add("ada", "a.rs", 1, "y", "drop");
        n.update(&drop.id, |d| d.deleted = true);
        n.save(&path).unwrap();
        let mut back = Notes::load(&path);
        assert_eq!(back.live().count(), 1);
        assert_eq!(back.get(&keep.id).map(|k| k.body.as_str()), Some("keep"));
        // A guest's stale copy from before the delete, arriving after the
        // restart, must not bring it back.
        back.merge(drop.clone());
        assert!(back.get(&drop.id).is_none());
    }

    #[test]
    fn the_box_reads_title_body_and_replies() {
        let mut notes = Notes::default();
        let n = notes.add("ada", "a.rs", 0, "x", "why?");
        let n = notes
            .update(&n.id, |n| {
                n.replies.push(Reply {
                    author: "bob".into(),
                    body: "because".into(),
                });
                n.resolved = true;
            })
            .unwrap();
        assert_eq!(n.title(), "ada \u{b7} note \u{b7} resolved");
        assert_eq!(n.text(), "why?\n\nbob: because");
    }
}
