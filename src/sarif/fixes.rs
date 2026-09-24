//! Applying a result's `fixes[]` (§3.55): every replacement of every
//! artifact change, computed against the text croft has for each file.
//!
//! Pure: the caller supplies each file's current text (an open buffer or the
//! file on disk) and applies the result, so a fix lands as one undoable edit
//! in the editor rather than a silent write to disk.

use super::model::{Fix, Run};
use super::region::{Lines, byte_span, column_kind, newline_sequences, text_range};
use super::resolve::Resolver;
use std::path::{Path, PathBuf};

/// One file's text after a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub before: String,
    pub after: String,
}

/// Compute every file a fix changes. Fails, changing nothing, when a file
/// cannot be found or read, a region does not resolve, replacements overlap,
/// or a replacement inserts binary content.
pub fn apply_fix(
    run: &Run,
    fix: &Fix,
    resolver: &Resolver,
    read: &mut dyn FnMut(&Path) -> Option<String>,
) -> Result<Vec<FileEdit>, String> {
    let kind = column_kind(run);
    let newlines = newline_sequences(run);
    let mut out = Vec::new();
    for change in &fix.artifact_changes {
        let uri = change
            .artifact_location
            .uri
            .clone()
            .unwrap_or_else(|| String::from("(unnamed file)"));
        let path = resolver
            .resolve(run, &change.artifact_location, &|p| p.is_file())
            .ok_or_else(|| format!("{uri} is not on this machine"))?;
        let before = read(&path).ok_or_else(|| format!("{} could not be read", path.display()))?;
        let lines = Lines::new(&before, &newlines);
        let mut spans: Vec<(usize, usize, String)> = Vec::new();
        for rep in &change.replacements {
            let inserted = match &rep.inserted_content {
                None => String::new(),
                Some(c) => match (&c.text, &c.binary) {
                    (Some(t), _) => t.clone(),
                    (None, Some(_)) => {
                        return Err(format!(
                            "{uri}: a binary replacement cannot be applied as text"
                        ));
                    }
                    (None, None) => String::new(),
                },
            };
            let range = text_range(&rep.deleted_region, &lines, kind)
                .ok_or_else(|| format!("{uri}: a replacement's region does not resolve"))?;
            let (a, b) = byte_span(&lines, &range);
            spans.push((a, b, inserted));
        }
        spans.sort_by_key(|s| (s.0, s.1));
        if spans.windows(2).any(|w| w[1].0 < w[0].1) {
            return Err(format!("{uri}: the fix's replacements overlap"));
        }
        let mut after = before.clone();
        for (a, b, text) in spans.iter().rev() {
            after.replace_range(*a..*b, text);
        }
        out.push(FileEdit {
            path,
            before,
            after,
        });
    }
    Ok(out)
}

/// A short line diff of what a fix does, for the Fix tab.
pub fn preview(edits: &[FileEdit]) -> Vec<String> {
    use crate::widgets::diff::{DiffRow, build_diff_rows};
    let mut out = Vec::new();
    for e in edits {
        out.push(
            e.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| e.path.display().to_string()),
        );
        let old: Vec<String> = e.before.lines().map(str::to_string).collect();
        let new: Vec<String> = e.after.lines().map(str::to_string).collect();
        for row in build_diff_rows(&old, &new) {
            match row {
                DiffRow::Removed { left } => out.push(format!("- {}", old[left])),
                DiffRow::Added { right } => out.push(format!("+ {}", new[right])),
                DiffRow::Replaced { left, right } => {
                    out.push(format!("- {}", old[left]));
                    out.push(format!("+ {}", new[right]));
                }
                DiffRow::Equal { .. } => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sarif::load::parse_log;

    fn run_with_fix(fix_json: &str) -> (Run, Fix) {
        let log = parse_log(&format!(
            r#"{{"version":"2.1.0","runs":[{{"tool":{{"driver":{{"name":"t"}}}},"columnKind":"unicodeCodePoints",
                "results":[{{"message":{{"text":"m"}},"fixes":[{fix_json}]}}]}}]}}"#
        ))
        .unwrap();
        let run = log.runs.into_iter().next().unwrap();
        let fix = run.results.as_ref().unwrap()[0].fixes[0].clone();
        (run, fix)
    }

    fn resolver(root: &Path) -> Resolver {
        Resolver {
            roots: vec![root.to_path_buf()],
            ..Resolver::default()
        }
    }

    #[test]
    fn replacements_apply_back_to_front_in_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.rs");
        std::fs::write(&file, "let q = format!(\"{}\", id);\nrun(q);\n").unwrap();
        let (run, fix) = run_with_fix(
            r#"{"description":{"text":"sanitize"},"artifactChanges":[{"artifactLocation":{"uri":"a.rs"},"replacements":[
                {"deletedRegion":{"startLine":1,"startColumn":23,"endColumn":25},"insertedContent":{"text":"clean(id)"}},
                {"deletedRegion":{"startLine":2,"startColumn":1,"endColumn":4},"insertedContent":{"text":"execute"}}
            ]}]}"#,
        );
        let edits = apply_fix(&run, &fix, &resolver(tmp.path()), &mut |p| {
            std::fs::read_to_string(p).ok()
        })
        .unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].path, file);
        assert_eq!(
            edits[0].after,
            "let q = format!(\"{}\", clean(id));\nexecute(q);\n"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            edits[0].before,
            "nothing written"
        );
    }

    #[test]
    fn a_deletion_has_no_inserted_content() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("b.py"), "import os\nimport sys\n").unwrap();
        let (run, fix) = run_with_fix(
            r#"{"artifactChanges":[{"artifactLocation":{"uri":"b.py"},"replacements":[
                {"deletedRegion":{"startLine":1,"endLine":2,"endColumn":1}}]}]}"#,
        );
        let edits = apply_fix(&run, &fix, &resolver(tmp.path()), &mut |p| {
            std::fs::read_to_string(p).ok()
        })
        .unwrap();
        assert_eq!(edits[0].after, "import sys\n");
    }

    #[test]
    fn the_callers_text_is_used_not_the_disk() {
        // An open, unsaved buffer is what the fix applies to.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("c.txt"), "on disk\n").unwrap();
        let (run, fix) = run_with_fix(
            r#"{"artifactChanges":[{"artifactLocation":{"uri":"c.txt"},"replacements":[
                {"deletedRegion":{"startLine":1,"startColumn":1,"endColumn":3},"insertedContent":{"text":"IN"}}]}]}"#,
        );
        let edits = apply_fix(&run, &fix, &resolver(tmp.path()), &mut |_| {
            Some(String::from("in buffer\n"))
        })
        .unwrap();
        assert_eq!(edits[0].after, "IN buffer\n");
    }

    #[test]
    fn overlapping_replacements_and_missing_files_refuse() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("d.txt"), "abcdef\n").unwrap();
        let (run, fix) = run_with_fix(
            r#"{"artifactChanges":[{"artifactLocation":{"uri":"d.txt"},"replacements":[
                {"deletedRegion":{"startLine":1,"startColumn":1,"endColumn":4},"insertedContent":{"text":"x"}},
                {"deletedRegion":{"startLine":1,"startColumn":3,"endColumn":5},"insertedContent":{"text":"y"}}]}]}"#,
        );
        let err = apply_fix(&run, &fix, &resolver(tmp.path()), &mut |p| {
            std::fs::read_to_string(p).ok()
        })
        .unwrap_err();
        assert!(err.contains("overlap"), "{err}");
        let (run, fix) = run_with_fix(
            r#"{"artifactChanges":[{"artifactLocation":{"uri":"nowhere.txt"},"replacements":[
                {"deletedRegion":{"startLine":1},"insertedContent":{"text":"x"}}]}]}"#,
        );
        let err = apply_fix(&run, &fix, &resolver(tmp.path()), &mut |p| {
            std::fs::read_to_string(p).ok()
        })
        .unwrap_err();
        assert!(err.contains("nowhere.txt"), "{err}");
    }

    #[test]
    fn binary_insertions_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("e.bin"), "x\n").unwrap();
        let (run, fix) = run_with_fix(
            r#"{"artifactChanges":[{"artifactLocation":{"uri":"e.bin"},"replacements":[
                {"deletedRegion":{"startLine":1},"insertedContent":{"binary":"AAE="}}]}]}"#,
        );
        let err = apply_fix(&run, &fix, &resolver(tmp.path()), &mut |p| {
            std::fs::read_to_string(p).ok()
        })
        .unwrap_err();
        assert!(err.contains("binary"), "{err}");
    }

    #[test]
    fn the_preview_shows_changed_lines_per_file() {
        let edits = vec![FileEdit {
            path: PathBuf::from("/ws/a.rs"),
            before: "one\ntwo\nthree\n".into(),
            after: "one\n2\nthree\n".into(),
        }];
        let p = preview(&edits);
        assert_eq!(p, vec!["a.rs", "- two", "+ 2"]);
    }
}
