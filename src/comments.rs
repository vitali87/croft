//! Human comment boxes (#367): sticky notes a participant leaves on a line
//! for the others in a shared (`--solo`) session, or for themselves in a
//! plain one. They reuse the navigator's comment-box widget and are overlay
//! state like its notes: never buffer content, never saved into the file.
//!
//! One participant is the authority: the session owner in a shared session,
//! or the lone croft otherwise. It holds the canonical [`CommentStore`],
//! persists it per workspace, applies every [`CommentEdit`] (its own and
//! the guests') and broadcasts the whole store after each change. Guests
//! apply their own edits optimistically, send them to the owner, and adopt
//! each broadcast wholesale, so every participant converges on the owner's
//! copy and a guest that disconnects and reconnects simply asks for it
//! again.
//!
//! Anchors follow edits through [`CommentEdit::Shift`]: the participant
//! whose LOCAL edit moved a box's line sends the delta. Deltas commute, so
//! two peers inserting lines above one box at the same moment both land
//! (an absolute "now at line N" from each would keep only one of them).

use std::collections::HashMap;
use std::path::Path;

/// The top bit tags a human box's id. Navigator notes count up from 1 and
/// review threads carry GitHub comment ids (around 4e9), so neither ever
/// sets it; the shared comment-box widget and its hit test key on the bare
/// id, and this bit is how the App tells the three sources apart.
pub const HUMAN_ID_BIT: u64 = 1 << 63;

/// The id of the unsaved box a new comment is typed into. Reserved: real
/// ids come from [`new_id`], which never returns it.
pub const DRAFT_ID: u64 = HUMAN_ID_BIT;

/// Longest author name kept, matching the caret name tags.
const MAX_AUTHOR: usize = 24;
/// Longest body kept per entry: a box is a note, not a document, and a
/// peer's message must never grow a box past any screen.
const MAX_BODY: usize = 2000;

pub fn is_human_id(id: u64) -> bool {
    id & HUMAN_ID_BIT != 0
}

/// A fresh box id: unique across participants without coordination (the
/// author's name, the process, the clock and a counter all feed it), with
/// [`HUMAN_ID_BIT`] set and never equal to [`DRAFT_ID`].
pub fn new_id(author: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    author.hash(&mut h);
    std::process::id().hash(&mut h);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
        .hash(&mut h);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .hash(&mut h);
    let id = h.finish() | HUMAN_ID_BIT;
    if id == DRAFT_ID { id | 1 } else { id }
}

/// One message in a box's thread: the opening comment, then the replies.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub author: String,
    pub body: String,
}

impl Entry {
    /// Build an entry with the author and body capped (see [`MAX_AUTHOR`],
    /// [`MAX_BODY`]).
    pub fn new(author: &str, body: &str) -> Self {
        Self {
            author: author.chars().take(MAX_AUTHOR).collect(),
            body: body.chars().take(MAX_BODY).collect(),
        }
    }
}

/// One human comment box.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HumanBox {
    pub id: u64,
    /// The file's workspace-relative key (`collab_file_key`), the same key
    /// the collab ops use, so every participant names the file alike.
    pub file: String,
    /// 0-based buffer line the box hangs under.
    pub line: usize,
    /// The opening comment first, then replies in order. Never empty.
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub resolved: bool,
}

impl HumanBox {
    /// Who opened the thread (the title row and the accent color).
    pub fn author(&self) -> &str {
        self.entries.first().map_or("", |e| e.author.as_str())
    }

    /// The text the box body shows: the opening comment, then each reply
    /// on its own line as `author: body`.
    pub fn body_text(&self) -> String {
        let mut out = String::new();
        for (i, e) in self.entries.iter().enumerate() {
            if i == 0 {
                out.push_str(&e.body);
            } else {
                out.push('\n');
                out.push_str(&e.author);
                out.push_str(": ");
                out.push_str(&e.body);
            }
        }
        out
    }
}

/// One change to the store. The unit guests send to the owner.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CommentEdit {
    Add(HumanBox),
    Reply {
        id: u64,
        entry: Entry,
    },
    SetResolved {
        id: u64,
        resolved: bool,
    },
    /// Move a box by `delta` lines (see the module doc on why a delta).
    Shift {
        id: u64,
        delta: i64,
    },
    /// A file (or a folder: every key under `from/`) was renamed or moved.
    Rename {
        from: String,
        to: String,
    },
}

/// Every human box in the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommentStore {
    boxes: Vec<HumanBox>,
    /// Bumps on every change, so consumers (the Explorer badge) rebuild
    /// only when something moved.
    generation: u64,
}

impl CommentStore {
    pub fn boxes(&self) -> &[HumanBox] {
        &self.boxes
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Replace everything (a guest adopting the owner's broadcast, or the
    /// authority loading the persisted store). Ids arriving from a peer
    /// without the human tag are dropped: they would collide with a
    /// navigator note or a review thread in the shared widget.
    pub fn set_all(&mut self, boxes: Vec<HumanBox>) {
        self.boxes = boxes
            .into_iter()
            .filter(|b| is_human_id(b.id) && b.id != DRAFT_ID && !b.entries.is_empty())
            .collect();
        self.generation += 1;
    }

    pub fn get(&self, id: u64) -> Option<&HumanBox> {
        self.boxes.iter().find(|b| b.id == id)
    }

    /// The boxes anchored in `file`.
    pub fn in_file<'a>(&'a self, file: &'a str) -> impl Iterator<Item = &'a HumanBox> + 'a {
        self.boxes.iter().filter(move |b| b.file == file)
    }

    /// Unresolved boxes per file key (the Explorer badge).
    pub fn unresolved_counts(&self) -> HashMap<String, usize> {
        let mut out = HashMap::new();
        for b in self.boxes.iter().filter(|b| !b.resolved) {
            *out.entry(b.file.clone()).or_insert(0) += 1;
        }
        out
    }

    /// Apply one edit. Returns whether anything changed; an edit naming a
    /// box this store does not hold is a no-op (a guest's view may lag).
    pub fn apply(&mut self, edit: &CommentEdit) -> bool {
        let changed = self.apply_inner(edit);
        if changed {
            self.generation += 1;
        }
        changed
    }

    fn apply_inner(&mut self, edit: &CommentEdit) -> bool {
        match edit {
            CommentEdit::Add(b) => {
                if !is_human_id(b.id)
                    || b.id == DRAFT_ID
                    || b.entries.is_empty()
                    || self.get(b.id).is_some()
                {
                    return false;
                }
                let mut b = b.clone();
                b.entries = b
                    .entries
                    .iter()
                    .map(|e| Entry::new(&e.author, &e.body))
                    .collect();
                self.boxes.push(b);
                true
            }
            CommentEdit::Reply { id, entry } => match self.get_mut(*id) {
                Some(b) => {
                    b.entries.push(Entry::new(&entry.author, &entry.body));
                    true
                }
                None => false,
            },
            CommentEdit::SetResolved { id, resolved } => match self.get_mut(*id) {
                Some(b) if b.resolved != *resolved => {
                    b.resolved = *resolved;
                    true
                }
                _ => false,
            },
            CommentEdit::Shift { id, delta } => match self.get_mut(*id) {
                Some(b) if *delta != 0 => {
                    let line = (b.line as i64).saturating_add(*delta).max(0);
                    b.line = usize::try_from(line).unwrap_or(0);
                    true
                }
                _ => false,
            },
            CommentEdit::Rename { from, to } => {
                let prefix = format!("{from}/");
                let mut changed = false;
                for b in &mut self.boxes {
                    if b.file == *from {
                        b.file = to.clone();
                        changed = true;
                    } else if let Some(rest) = b.file.strip_prefix(&prefix) {
                        b.file = format!("{to}/{rest}");
                        changed = true;
                    }
                }
                changed
            }
        }
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut HumanBox> {
        self.boxes.iter_mut().find(|b| b.id == id)
    }
}

/// Where each of `lines` (0-based rows of `old`) lands in `new`, as a
/// signed delta. A row that survived maps to where it went; a row that was
/// deleted or rewritten maps to where the change landed, so a box on a
/// deleted line stays near its code instead of vanishing (a note outlives
/// the line it was about; the bookmark mapping drops such rows).
pub fn line_deltas(old: &[String], new: &[String], lines: &[usize]) -> Vec<i64> {
    use similar::DiffOp;
    let common = old.len().min(new.len());
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(a, b)| a == b)
        .count()
        .min(common);
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(common - prefix);
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    // A deadline bounds a pathological middle; past it the diff is coarser
    // but still a valid diff, so rows still land on real lines.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
    let ops = similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &old[prefix..old_end],
        &new[prefix..new_end],
        Some(deadline),
    );
    let last = new.len().saturating_sub(1);
    let map_middle = |r: usize| -> usize {
        for op in &ops {
            match *op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } if (old_index..old_index + len).contains(&r) => {
                    return new_index + (r - old_index);
                }
                DiffOp::Delete {
                    old_index,
                    old_len,
                    new_index,
                } if (old_index..old_index + old_len).contains(&r) => return new_index,
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } if (old_index..old_index + old_len).contains(&r) => {
                    return new_index + (r - old_index).min(new_len - 1);
                }
                _ => {}
            }
        }
        r
    };
    lines
        .iter()
        .map(|&row| {
            let mapped = if row < prefix {
                row
            } else if row >= old_end {
                row - old_end + new_end
            } else {
                prefix + map_middle(row - prefix)
            };
            let mapped = mapped.min(last);
            mapped as i64 - row as i64
        })
        .collect()
}

/// The persisted store: workspace root → its boxes.
pub fn load(path: &Path, root: &str) -> Vec<HumanBox> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<HashMap<String, Vec<HumanBox>>>(&raw).ok())
        .and_then(|mut map| map.remove(root))
        .unwrap_or_default()
}

/// Write one workspace's boxes; an empty list removes its key so plain
/// workspaces never accumulate entries. Shares the locked
/// read-modify-write every per-workspace store uses, so a corrupt store is
/// refused rather than replaced.
pub fn save(path: &Path, root: &str, boxes: &[HumanBox]) -> Result<(), String> {
    crate::workspace::update_json_store::<Vec<HumanBox>, _>(path, |map| {
        if boxes.is_empty() {
            map.remove(root);
        } else {
            map.insert(root.to_string(), boxes.to_vec());
        }
    })
}

/// The real store path, `~/.config/croft/comment_boxes.json`. The App
/// keeps it in a field that is None under test, so no test ever reads or
/// writes the user's store (a test that wants persistence sets a tempdir).
pub fn path() -> std::path::PathBuf {
    crate::prefs::config_dir().join("comment_boxes.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|l| l.to_string()).collect()
    }

    fn human(id: u64, file: &str, line: usize) -> HumanBox {
        HumanBox {
            id: id | HUMAN_ID_BIT,
            file: file.into(),
            line,
            entries: vec![Entry::new("ana", "look here")],
            resolved: false,
        }
    }

    #[test]
    fn a_box_follows_lines_inserted_and_removed_above_it() {
        let old = lines(&["a", "b", "c", "d"]);
        let new = lines(&["x", "y", "a", "b", "c", "d"]);
        assert_eq!(line_deltas(&old, &new, &[2, 0]), vec![2, 2]);
        let new = lines(&["a", "c", "d"]);
        assert_eq!(line_deltas(&old, &new, &[3, 0]), vec![-1, 0]);
    }

    #[test]
    fn a_box_on_a_deleted_line_stays_where_the_line_was() {
        let old = lines(&["a", "b", "c", "d"]);
        let new = lines(&["a", "d"]);
        // "c" is gone: its box stays at the gap (row 1, where "d" now is),
        // instead of vanishing with the line.
        assert_eq!(line_deltas(&old, &new, &[2]), vec![-1]);
        // Everything deleted: the box parks on the one line left.
        assert_eq!(line_deltas(&old, &lines(&[""]), &[3]), vec![-3]);
    }

    #[test]
    fn concurrent_shifts_commute() {
        let mut a = CommentStore::default();
        a.set_all(vec![human(1, "f.rs", 10)]);
        let mut b = a.clone();
        let id = 1 | HUMAN_ID_BIT;
        let one = CommentEdit::Shift { id, delta: 1 };
        let two = CommentEdit::Shift { id, delta: 3 };
        a.apply(&one);
        a.apply(&two);
        b.apply(&two);
        b.apply(&one);
        assert_eq!(a, b);
        assert_eq!(a.get(id).unwrap().line, 14);
    }

    #[test]
    fn replies_resolve_and_renames_apply() {
        let mut s = CommentStore::default();
        let id = 7 | HUMAN_ID_BIT;
        assert!(s.apply(&CommentEdit::Add(human(7, "src/a.rs", 3))));
        assert!(
            !s.apply(&CommentEdit::Add(human(7, "src/a.rs", 3))),
            "ids are unique"
        );
        s.apply(&CommentEdit::Reply {
            id,
            entry: Entry::new("bo", "agreed"),
        });
        assert_eq!(s.get(id).unwrap().body_text(), "look here\nbo: agreed");
        assert!(s.apply(&CommentEdit::SetResolved { id, resolved: true }));
        assert!(!s.apply(&CommentEdit::SetResolved { id, resolved: true }));
        assert!(s.unresolved_counts().is_empty());
        s.apply(&CommentEdit::Rename {
            from: "src".into(),
            to: "lib".into(),
        });
        assert_eq!(s.get(id).unwrap().file, "lib/a.rs");
        // A sibling whose name merely starts with the folder's is not in it.
        s.apply(&CommentEdit::Add(human(8, "libx.rs", 0)));
        s.apply(&CommentEdit::Rename {
            from: "lib".into(),
            to: "src".into(),
        });
        assert_eq!(s.get(8 | HUMAN_ID_BIT).unwrap().file, "libx.rs");
    }

    #[test]
    fn untagged_ids_from_a_peer_are_refused() {
        let mut s = CommentStore::default();
        let mut b = human(3, "f", 0);
        b.id = 3;
        assert!(!s.apply(&CommentEdit::Add(b.clone())));
        s.set_all(vec![b]);
        assert!(s.boxes().is_empty());
        assert!(is_human_id(new_id("ana")));
        assert_ne!(new_id("ana"), DRAFT_ID);
    }

    #[test]
    fn the_store_round_trips_per_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("boxes.json");
        save(&path, "/w/a", &[human(1, "f", 2)]).unwrap();
        save(&path, "/w/b", &[human(2, "g", 0)]).unwrap();
        assert_eq!(load(&path, "/w/a"), vec![human(1, "f", 2)]);
        save(&path, "/w/a", &[]).unwrap();
        assert!(load(&path, "/w/a").is_empty());
        assert_eq!(load(&path, "/w/b").len(), 1);
    }
}
