//! Applying a result's `fixes[]` (§3.55): every replacement of every
//! artifact change, computed against the text croft has for each file.
//!
//! Pure: the caller supplies each file's current text (an open buffer or the
//! file on disk) and applies the result, so a fix lands as one undoable edit
//! in the editor rather than a silent write to disk.

use super::model::{Fix, Run, SarifResult};
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

/// A result's fixes: its `fixes[]`, then a unified diff in its property bag
/// (`properties.diff`, #577), which some analyzers attach instead.
pub fn fix_count(result: &SarifResult) -> usize {
    result.fixes.len() + usize::from(property_diff(result).is_some())
}

/// Fix `index`'s description, for the Fix tab.
pub fn fix_description(result: &SarifResult, index: usize) -> Option<String> {
    match result.fixes.get(index) {
        Some(f) => Some(
            f.description
                .as_ref()
                .and_then(|m| m.text.clone())
                .unwrap_or_else(|| String::from("(no description)")),
        ),
        None if index == result.fixes.len() => {
            property_diff(result).map(|_| String::from("the patch in properties.diff"))
        }
        None => None,
    }
}

fn property_diff(result: &SarifResult) -> Option<&str> {
    result
        .properties
        .get("diff")
        .and_then(|d| d.as_str())
        .filter(|d| d.contains("@@"))
}

/// Compute fix `index` of `result` ([`fix_count`] orders them).
pub fn fix_for(
    run: &Run,
    result: &SarifResult,
    index: usize,
    resolver: &Resolver,
    read: &mut dyn FnMut(&Path) -> Option<String>,
) -> Result<Vec<FileEdit>, String> {
    if let Some(fix) = result.fixes.get(index) {
        return apply_fix(run, fix, resolver, read);
    }
    match property_diff(result) {
        Some(diff) if index == result.fixes.len() => apply_diff(run, diff, resolver, read),
        _ => Err(String::from("no such fix")),
    }
}

/// One file's part of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FilePatch {
    path: String,
    hunks: Vec<PatchHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchHunk {
    /// 1-based line the hunk starts at in the old file.
    old_start: usize,
    /// Context and removed lines, as the old file has them.
    old: Vec<String>,
    /// Context and added lines, as the new file has them.
    new: Vec<String>,
}

/// The files and hunks of a unified diff (`git diff` or `diff -u`). A path
/// loses git's `a/`/`b/` prefix; `/dev/null` (a created or deleted file)
/// is refused, as is a hunk whose line counts do not add up.
fn parse_unified(diff: &str) -> Result<Vec<FilePatch>, String> {
    let mut files: Vec<FilePatch> = Vec::new();
    let mut lines = diff.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(target) = line.strip_prefix("+++ ") {
            let target = target.split('\t').next().unwrap_or(target).trim();
            if target == "/dev/null" {
                return Err(String::from(
                    "the patch deletes a file, which is not applied",
                ));
            }
            let path = target.strip_prefix("b/").unwrap_or(target).to_string();
            files.push(FilePatch {
                path,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(header) = line.strip_prefix("@@ ") else {
            continue;
        };
        let file = files
            .last_mut()
            .ok_or_else(|| String::from("a hunk comes before any file header"))?;
        let bad = || format!("unreadable hunk header: {line}");
        let old_spec = header.split_whitespace().next().ok_or_else(bad)?;
        let new_spec = header.split_whitespace().nth(1).ok_or_else(bad)?;
        let counts = |spec: &str, sign: char| -> Option<(usize, usize)> {
            let spec = spec.strip_prefix(sign)?;
            let (start, len) = spec.split_once(',').unwrap_or((spec, "1"));
            Some((start.parse().ok()?, len.parse().ok()?))
        };
        let (old_start, old_len) = counts(old_spec, '-').ok_or_else(bad)?;
        let (_, new_len) = counts(new_spec, '+').ok_or_else(bad)?;
        let mut hunk = PatchHunk {
            old_start,
            old: Vec::new(),
            new: Vec::new(),
        };
        while hunk.old.len() < old_len || hunk.new.len() < new_len {
            let Some(body) = lines.next() else { break };
            match body.chars().next() {
                Some('-') => hunk.old.push(body[1..].to_string()),
                Some('+') => hunk.new.push(body[1..].to_string()),
                Some('\\') => {}
                Some(' ') => {
                    hunk.old.push(body[1..].to_string());
                    hunk.new.push(body[1..].to_string());
                }
                // An empty context line whose leading space was trimmed.
                None => {
                    hunk.old.push(String::new());
                    hunk.new.push(String::new());
                }
                Some(_) => return Err(format!("unreadable hunk line: {body}")),
            }
        }
        if hunk.old.len() != old_len || hunk.new.len() != new_len {
            return Err(format!("a hunk's lines do not match its header: {line}"));
        }
        file.hunks.push(hunk);
    }
    if files.iter().all(|f| f.hunks.is_empty()) {
        return Err(String::from("the patch changes nothing"));
    }
    Ok(files)
}

/// How far from its stated line a hunk may have moved and still apply.
const DIFF_FUZZ_LINES: usize = 50;

/// Apply a unified diff (§3.55's alternative some analyzers put in
/// `properties.diff`) to the files it names, each resolved like an artifact
/// location. Every hunk's context and removed lines must match the file
/// exactly, at its stated line or the nearest place within
/// [`DIFF_FUZZ_LINES`]; otherwise nothing is changed.
pub fn apply_diff(
    run: &Run,
    diff: &str,
    resolver: &Resolver,
    read: &mut dyn FnMut(&Path) -> Option<String>,
) -> Result<Vec<FileEdit>, String> {
    let mut out = Vec::new();
    for patch in parse_unified(diff)? {
        let loc = super::model::ArtifactLocation {
            uri: Some(patch.path.clone()),
            ..Default::default()
        };
        let path = resolver
            .resolve(run, &loc, &|p| p.is_file())
            .ok_or_else(|| format!("{} is not on this machine", patch.path))?;
        let before = read(&path).ok_or_else(|| format!("{} could not be read", path.display()))?;
        let trailing_newline = before.ends_with('\n');
        let mut lines: Vec<String> = before.lines().map(str::to_string).collect();
        // Later hunks first, so earlier line numbers stay put.
        let mut hunks = patch.hunks.clone();
        hunks.sort_by_key(|h| std::cmp::Reverse(h.old_start));
        for h in &hunks {
            let stated = h.old_start.saturating_sub(1);
            let fits = |at: usize| {
                at + h.old.len() <= lines.len()
                    && lines[at..at + h.old.len()]
                        .iter()
                        .zip(&h.old)
                        .all(|(a, b)| a.trim_end_matches('\r') == b.trim_end_matches('\r'))
            };
            let at = (0..=DIFF_FUZZ_LINES)
                .flat_map(|d| [stated.checked_add(d), stated.checked_sub(d)])
                .flatten()
                .find(|&at| fits(at))
                .ok_or_else(|| {
                    format!(
                        "{}: the patch no longer matches the file near line {}",
                        patch.path, h.old_start
                    )
                })?;
            lines.splice(at..at + h.old.len(), h.new.iter().cloned());
        }
        let mut after = lines.join("\n");
        if trailing_newline && !after.is_empty() {
            after.push('\n');
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

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -2,3 +2,3 @@ fn main() {\n     let id = read();\n-    let q = format!(\"{}\", id);\n+    let q = escape(&id);\n     run(q);\n";

    fn diff_result(diff: &str) -> (Run, SarifResult) {
        let log = parse_log(&format!(
            r#"{{"version":"2.1.0","runs":[{{"tool":{{"driver":{{"name":"t"}}}},
                "results":[{{"message":{{"text":"m"}},"properties":{{"diff":{}}}}}]}}]}}"#,
            serde_json::to_string(diff).unwrap()
        ))
        .unwrap();
        let run = log.runs.into_iter().next().unwrap();
        let result = run.results.as_ref().unwrap()[0].clone();
        (run, result)
    }

    #[test]
    fn a_property_diff_is_a_fix_applied_where_its_context_matches() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        let file = tmp.path().join("src/a.rs");
        // Two lines were added above since the scan: the hunk moved.
        std::fs::write(
            &file,
            "// new\n// new\nfn main() {\n    let id = read();\n    let q = format!(\"{}\", id);\n    run(q);\n}\n",
        )
        .unwrap();
        let (run, result) = diff_result(DIFF);
        assert_eq!(fix_count(&result), 1);
        assert_eq!(
            fix_description(&result, 0).as_deref(),
            Some("the patch in properties.diff")
        );
        let edits = fix_for(&run, &result, 0, &resolver(tmp.path()), &mut |p| {
            std::fs::read_to_string(p).ok()
        })
        .unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].path, file);
        assert_eq!(
            edits[0].after,
            "// new\n// new\nfn main() {\n    let id = read();\n    let q = escape(&id);\n    run(q);\n}\n"
        );
        assert!(fix_for(&run, &result, 1, &resolver(tmp.path()), &mut |_| None).is_err());

        // A file that no longer matches is refused, changing nothing.
        std::fs::write(&file, "fn main() {\n    let q = other();\n}\n").unwrap();
        let err = fix_for(&run, &result, 0, &resolver(tmp.path()), &mut |p| {
            std::fs::read_to_string(p).ok()
        })
        .unwrap_err();
        assert!(
            err.contains("no longer matches the file near line 2"),
            "{err}"
        );
    }

    #[test]
    fn a_unified_diff_is_parsed_file_by_file_and_checked() {
        let two = format!("{DIFF}--- a/b.rs\n+++ b/b.rs\n@@ -1 +1,2 @@\n x\n+y\n");
        let files = parse_unified(&two).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/a.rs");
        assert_eq!(files[0].hunks[0].old_start, 2);
        assert_eq!(files[0].hunks[0].old.len(), 3);
        assert_eq!(files[1].path, "b.rs");
        assert_eq!(files[1].hunks[0].new, ["x", "y"]);
        assert!(parse_unified("--- a/x\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n").is_err());
        assert!(
            parse_unified("@@ -1 +1 @@\n-x\n+y\n").is_err(),
            "no file header"
        );
        assert!(
            parse_unified("--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-x\n+y\n").is_err(),
            "short hunk"
        );
        assert!(parse_unified("just text").is_err());
        // A result whose property is not a patch offers no fix.
        let (_, result) = diff_result("not a diff");
        assert_eq!(fix_count(&result), 0);
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
