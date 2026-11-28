//! What code outside the UI takes from the views (#2062, card "Non-UI code
//! out of `widgets/`", step 1f). `src/app/`, `src/widgets/` and `main.rs` are
//! the UI. Everything else names `crate::widgets` only for what [`ALLOWED`]
//! lists: parts of the few types that are both a model and the view that
//! draws it (the editor, the terminal's grid, the diff tab, the Testing
//! view), which stay whole on purpose. The list may only shrink: a new use
//! fails the test, and so does a listed use that has gone, so whoever ends
//! one also strikes it here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every `(file, path)` a file outside the UI may name under
/// `crate::widgets`, the path cut after its first type.
const ALLOWED: &[(&str, &str)] = &[
    ("src/archive.rs", "editor::Editor"),
    ("src/git/tests.rs", "diff::DiffData"),
    ("src/highlight.rs", "editor::SELECTABLE_LANGUAGES"),
    ("src/highlight.rs", "editor::language_scope_id"),
    ("src/import_vscode.rs", "editor::WhitespaceMode"),
    ("src/lsp/manager/tests.rs", "editor::Editor"),
    ("src/pane_host.rs", "terminal::VoidListener"),
    ("src/pane_host.rs", "terminal::ansi_rows_from"),
    ("src/pane_host.rs", "terminal::resize_term"),
    ("src/scrubber.rs", "editor::Editor"),
    ("src/scrubber.rs", "editor::split_into_lines"),
    ("src/testing/worker.rs", "testing::TestingPanel"),
    ("src/view_ipc.rs", "editor::Editor"),
];

/// The UI: the app, the views and the binary's root, which wires them up.
fn is_ui(rel: &str) -> bool {
    rel.starts_with("src/app/") || rel.starts_with("src/widgets/") || rel == "src/main.rs"
}

/// Every `(file, path)` that files under `root/src` outside the UI name
/// under `crate::widgets`, in code and in docs alike: a doc link into a view
/// is a dependency on it too.
fn uses(root: &Path) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for file in rust_files(&root.join("src")) {
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        // This file spells such paths out in its tests.
        if is_ui(&rel) || rel == "src/view_boundary.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let mut rest = text.as_str();
        while let Some(at) = rest.find("crate::widgets") {
            rest = &rest[at + "crate::widgets".len()..];
            if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            for path in use_paths(rest) {
                found.insert((rel.clone(), cut_after_type(&path)));
            }
        }
    }
    found
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files
}

/// The paths named by the text right after a `crate::widgets`: one for
/// `::editor::Editor`, one each for a group (`::{editor::Editor, diff}`),
/// and the empty path for the module itself (`;`, ` as w`), which no entry
/// allows, so an alias cannot hide what is named through it.
fn use_paths(after: &str) -> Vec<String> {
    match after.strip_prefix("::") {
        Some(tree) => use_tree(tree).0,
        None => vec![String::new()],
    }
}

/// The paths of the use tree at the start of `s`, and how many bytes it
/// takes: a name, a name followed by `::` and a tree, or a `{…}` group.
fn use_tree(s: &str) -> (Vec<String>, usize) {
    let trimmed = s.trim_start();
    let lead = s.len() - trimmed.len();
    if let Some(group) = trimmed.strip_prefix('{') {
        let mut paths = Vec::new();
        let mut at = 0;
        loop {
            let (mut sub, used) = use_tree(&group[at..]);
            paths.append(&mut sub);
            at += used;
            let next = group[at..].trim_start();
            at = group.len() - next.len();
            match next.chars().next() {
                Some(',') => at += 1,
                Some('}') => return (paths, lead + 1 + at + 1),
                _ => return (paths, lead + 1 + at),
            }
        }
    }
    let name_len = trimmed
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '*'))
        .unwrap_or(trimmed.len());
    if name_len == 0 {
        return (Vec::new(), lead);
    }
    let name = &trimmed[..name_len];
    match trimmed[name_len..].strip_prefix("::") {
        Some(tail) => {
            let (subs, used) = use_tree(tail);
            let paths = match subs.is_empty() {
                true => vec![name.to_string()],
                false => subs
                    .into_iter()
                    .map(|p| match p.as_str() {
                        "self" => name.to_string(),
                        _ => format!("{name}::{p}"),
                    })
                    .collect(),
            };
            (paths, lead + name_len + 2 + used)
        }
        None => (vec![name.to_string()], lead + name_len),
    }
}

/// `path` cut after its first segment that starts with a capital (a type, a
/// constant), so `editor::Editor::new` is `editor::Editor`.
fn cut_after_type(path: &str) -> String {
    let segments: Vec<&str> = path.split("::").collect();
    let end = segments
        .iter()
        .position(|s| s.starts_with(|c: char| c.is_uppercase()))
        .map_or(segments.len(), |i| i + 1);
    segments[..end].join("::")
}

#[test]
fn outside_the_ui_only_the_listed_parts_come_from_the_views() {
    let found = uses(Path::new(env!("CARGO_MANIFEST_DIR")));
    let allowed: BTreeSet<(String, String)> = ALLOWED
        .iter()
        .map(|(file, path)| (file.to_string(), path.to_string()))
        .collect();
    let new: Vec<_> = found.difference(&allowed).collect();
    let gone: Vec<_> = allowed.difference(&found).collect();
    assert!(
        new.is_empty(),
        "code outside the UI names a view; move what it needs out of `src/widgets/` \
         next to its users (an empty path is `crate::widgets` itself): {new:?}"
    );
    assert!(
        gone.is_empty(),
        "these listed uses of a view are gone; strike them from ALLOWED, which \
         may only shrink: {gone:?}"
    );
}

#[test]
fn a_grouped_nested_or_aliased_use_is_read_in_full() {
    assert_eq!(
        use_paths("::editor::Editor::new(x)"),
        ["editor::Editor::new"]
    );
    assert_eq!(
        use_paths("::{editor::{self, Editor}, terminal::VoidListener};"),
        ["editor", "editor::Editor", "terminal::VoidListener"]
    );
    assert_eq!(
        use_paths("::{\n    diff::DiffData,\n    testing,\n};"),
        ["diff::DiffData", "testing"]
    );
    assert_eq!(use_paths(";"), [""]);
    assert_eq!(use_paths(" as views;"), [""]);
    assert_eq!(cut_after_type("editor::Editor::new"), "editor::Editor");
    assert_eq!(
        cut_after_type("terminal::resize_term"),
        "terminal::resize_term"
    );
    assert_eq!(
        cut_after_type("editor::SELECTABLE_LANGUAGES"),
        "editor::SELECTABLE_LANGUAGES"
    );
}

#[test]
fn a_new_use_outside_the_ui_is_caught_and_the_ui_is_not_read() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(src.join("widgets")).unwrap();
    std::fs::create_dir_all(src.join("app")).unwrap();
    std::fs::write(
        src.join("engine.rs"),
        "use crate::widgets::search::SearchPanel;\n/// See [`crate::widgets::diff`].\n",
    )
    .unwrap();
    std::fs::write(
        src.join("app/mod.rs"),
        "use crate::widgets::editor::Editor;\n",
    )
    .unwrap();
    std::fs::write(src.join("widgets/a.rs"), "use crate::widgets::b::B;\n").unwrap();
    std::fs::write(src.join("main.rs"), "use crate::widgets::editor::Editor;\n").unwrap();
    std::fs::write(src.join("other.rs"), "use crate::widgets_extra::X;\n").unwrap();
    let found: Vec<_> = uses(root.path()).into_iter().collect();
    assert_eq!(
        found,
        [
            ("src/engine.rs".to_string(), "diff".to_string()),
            (
                "src/engine.rs".to_string(),
                "search::SearchPanel".to_string()
            ),
        ]
    );
}
