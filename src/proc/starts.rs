//! How many places outside `proc` start a program directly (#2062, card 12
//! step 12): with `Command::new`, which no stand-in, record or UI-turn check
//! can see. Each move onto a builder lowers the count, and it may never
//! rise. Test code is not counted.

use std::path::{Path, PathBuf};

/// The `Command::new(` calls outside `proc` and test code, as `file:line`.
fn direct_starts(src: &Path) -> Vec<String> {
    let files = rust_files(src);
    let test_only = test_only_modules(&files);
    let mut sites = Vec::new();
    for file in &files {
        let rel = file.strip_prefix(src).unwrap();
        if rel == Path::new("proc.rs")
            || rel.starts_with("proc")
            || test_only.contains(file)
            || is_test_file(rel)
        {
            continue;
        }
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in shipped_part(&text).lines().enumerate() {
            if line.contains("Command::new(") && !line.trim_start().starts_with("//") {
                sites.push(format!("src/{}:{}", rel.display(), n + 1));
            }
        }
    }
    sites.sort();
    sites
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

/// A test file by its name or folder: `tests.rs`, `*_tests.rs`, or under a
/// `tests/` folder.
fn is_test_file(rel: &Path) -> bool {
    let name = rel.file_name().unwrap().to_string_lossy();
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || rel.components().any(|c| c.as_os_str() == "tests")
}

/// The files of modules their parent declares behind `#[cfg(test)]`
/// (`#[cfg(test)] mod test_git;`), and everything under them.
fn test_only_modules(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        for (i, line) in lines.iter().enumerate() {
            if !is_cfg_test(line) {
                continue;
            }
            let Some(name) = item_after(&lines, i)
                .map(|m| lines[m])
                .filter(|item| item.ends_with(';'))
                .and_then(module_name)
            else {
                continue;
            };
            let stem = file.file_stem().unwrap();
            let dir = if stem == "main" || stem == "mod" || stem == "lib" {
                file.parent().unwrap().to_path_buf()
            } else {
                file.with_extension("")
            };
            out.push(dir.join(format!("{name}.rs")));
            let folder = dir.join(name);
            out.extend(files.iter().filter(|f| f.starts_with(&folder)).cloned());
        }
    }
    out
}

/// `text` with each inline `#[cfg(test)] mod … { … }` blanked line for
/// line, so what is left is the shipped code at its own line numbers: items
/// after a test module count too. The module ends at the first `}` at its
/// own indent, which rustfmt guarantees.
fn shipped_part(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let trimmed: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let module = is_cfg_test(trimmed[i])
            .then(|| item_after(&trimmed, i))
            .flatten()
            .filter(|&m| trimmed[m].ends_with('{') && module_name(trimmed[m]).is_some());
        if let Some(m) = module {
            let indent = &lines[m][..lines[m].len() - lines[m].trim_start().len()];
            let close = format!("{indent}}}");
            let end = (m + 1..lines.len())
                .find(|&j| lines[j] == close)
                .unwrap_or(lines.len() - 1);
            out.extend(std::iter::repeat_n("", end + 1 - i));
            i = end + 1;
        } else {
            out.push(lines[i]);
            i += 1;
        }
    }
    out.join("\n")
}

/// Whether `line` is an attribute that compiles what follows for tests
/// alone: `#[cfg(test)]`, `#[cfg(all(test, unix))]`, never
/// `#[cfg(not(test))]` or `#[cfg(any(test, …))]`.
fn is_cfg_test(line: &str) -> bool {
    line.starts_with("#[cfg(")
        && line.contains("test")
        && !line.contains("not(")
        && !line.contains("any(")
}

/// The line of the item the attribute at `lines[at]` applies to: the next
/// one that is not blank, a comment or another attribute (one spanning
/// lines too). `lines` are trimmed.
fn item_after(lines: &[&str], at: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, line) in lines.iter().enumerate().skip(at + 1) {
        if depth > 0 || line.starts_with("#[") {
            depth += line.matches('[').count() as i32 - line.matches(']').count() as i32;
            continue;
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        return Some(i);
    }
    None
}

/// The name of the module `item` declares, if it declares one.
fn module_name(item: &str) -> Option<&str> {
    let rest = ["pub(crate) ", "pub(super) ", "pub "]
        .iter()
        .find_map(|vis| item.strip_prefix(vis))
        .unwrap_or(item);
    let name = rest
        .strip_prefix("mod ")?
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()?;
    (!name.is_empty()).then_some(name)
}

/// The direct starts may only fall in number: a new one is a program no
/// stand-in, record or UI-turn check can see, which belongs on a builder
/// (`proc::command`, `proc::Bounded`, the git, gh and ssh builders).
#[test]
fn direct_program_starts_only_fall() {
    const MOST: usize = 113;
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sites = direct_starts(&src);
    assert!(
        sites.len() <= MOST,
        "{} places start a program directly, more than the {MOST} allowed; start the new one \
         through proc instead:\n{}",
        sites.len(),
        sites.join("\n")
    );
    assert_eq!(
        sites.len(),
        MOST,
        "fewer places start a program directly now: lower MOST to {} so the count holds",
        sites.len()
    );
}

/// The count reads only shipped code: a test module at a file's end, a
/// test file and a module declared for tests alone are left out.
#[test]
fn test_code_is_not_counted() {
    let text = "fn a() { Command::new(\"x\"); }\n#[cfg(test)]\nmod tests {\n    fn b() { Command::new(\"y\"); }\n}\n";
    assert_eq!(
        shipped_part(text),
        "fn a() { Command::new(\"x\"); }\n\n\n\n"
    );
    // Another attribute, or a comment, may sit between the two; an item
    // after the module is shipped and keeps its line number.
    let text = "#[cfg(test)]\n#[allow(\n    clippy::items_after_test_module,\n)]\n// the tests\nmod tests {\n    fn b() { Command::new(\"y\"); }\n}\n\nfn c() { Command::new(\"z\"); }\n";
    let shipped = shipped_part(text);
    assert!(!shipped.contains("\"y\""), "{shipped}");
    assert_eq!(
        shipped.lines().position(|l| l.contains("\"z\"")),
        Some(9),
        "{shipped}"
    );
    // Code compiled only outside tests is shipped.
    let text = "#[cfg(not(test))]\nmod real {\n    fn d() { Command::new(\"w\"); }\n}\n";
    assert_eq!(shipped_part(text), text.trim_end());
    assert_eq!(module_name("pub(crate) mod test_git;"), Some("test_git"));
    assert_eq!(module_name("fn modest() {"), None);
    assert!(is_test_file(Path::new("app/tests/debug.rs")));
    assert!(is_test_file(Path::new("remote/tests.rs")));
    assert!(!is_test_file(Path::new("remote/relay.rs")));
}
