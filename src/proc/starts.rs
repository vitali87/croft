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
            let is_cfg_test = line.starts_with("#[cfg(") && line.contains("test");
            let Some(next) = lines[i + 1..].iter().find(|l| !l.is_empty()) else {
                continue;
            };
            let Some(name) = next
                .strip_prefix("pub(crate) mod ")
                .or_else(|| next.strip_prefix("pub mod "))
                .or_else(|| next.strip_prefix("mod "))
                .and_then(|rest| rest.strip_suffix(';'))
            else {
                continue;
            };
            if !is_cfg_test {
                continue;
            }
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

/// `text` up to an inline `#[cfg(test)] mod … {`: the tests at a file's end.
fn shipped_part(text: &str) -> &str {
    let mut offset = 0;
    let mut lines = text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[cfg(") && trimmed.contains("test") {
            let next = lines.peek().map(|l| l.trim()).unwrap_or("");
            if next.starts_with("mod ") && next.ends_with('{') {
                return &text[..offset];
            }
        }
        offset += line.len();
    }
    text
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
    assert_eq!(shipped_part(text), "fn a() { Command::new(\"x\"); }\n");
    assert!(is_test_file(Path::new("app/tests/debug.rs")));
    assert!(is_test_file(Path::new("remote/tests.rs")));
    assert!(!is_test_file(Path::new("remote/relay.rs")));
}
