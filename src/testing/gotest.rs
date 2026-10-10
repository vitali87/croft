//! `go test` for the Testing view (#264). Everything Go-specific the worker
//! needs: the module path from `go.mod`, the test IDs (`./pkg::TestName`,
//! a subtest keeping Go's own `TestName/sub` leaf), the `go test -json`
//! event parsers, the `-run` selectors, and turning a Go cover profile into
//! the LCOV the coverage view reads.
//!
//! A package is named by its directory relative to the workspace root (`.`
//! for the root package, `./internal/db` below it), which is both what the
//! tree shows and what `go test` takes as a package argument. The root is a
//! module (`go.mod`) or, with no `go.mod`, a `go.work` whose `use`d modules
//! it lists (#1505). A package of a module `use`d from outside the root
//! (`use ../service`) is named by its path from the root
//! (`../service/inner`), which `go test` takes too. A package outside
//! every module keeps its import path, which `go test` also accepts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::model::{TestCase, TestStatus};
use super::regex_escape;

/// The `module` path `root/go.mod` declares, if any.
pub fn module_path(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("go.mod")).ok()?;
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("module")?;
        // `module` must be the whole keyword, not `modulefoo`.
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        // A trailing `// comment` is not part of the path.
        let m = rest.split("//").next().unwrap_or_default().trim();
        let m = m.trim_matches(|c| c == '"' || c == '`');
        (!m.is_empty()).then(|| m.to_string())
    })
}

/// The Go modules a workspace root holds: its own `go.mod`'s, or, when it
/// has none, each module a root `go.work` `use`s (#1505). Each is its
/// `module` path and its directory relative to the root (empty for the
/// root itself, `../service` for a module beside it, or an absolute
/// path).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Modules(Vec<(String, String)>);

impl Modules {
    pub fn of(root: &Path) -> Modules {
        if let Some(m) = module_path(root) {
            return Modules(vec![(m, String::new())]);
        }
        let Ok(work) = std::fs::read_to_string(root.join("go.work")) else {
            return Modules::default();
        };
        Modules(
            work_uses(&work)
                .iter()
                .filter_map(|dir| {
                    let dir = from_root(dir)?;
                    Some((module_path(&root.join(&dir))?, dir))
                })
                .collect(),
        )
    }

    /// `args` with each `./...` (every package) spelled so `go test` takes
    /// it at this root. A `go.work` root with no module of its own refuses
    /// `./...` ("directory prefix . does not contain modules listed in
    /// go.work"), so there it is one `<module dir>/...` per module
    /// (`./lib/...`, `../service/...`).
    pub fn expand<S: AsRef<str>>(&self, args: &[S]) -> Vec<String> {
        let whole_root = self.0.is_empty() || self.0.iter().any(|(_, dir)| dir.is_empty());
        args.iter()
            .flat_map(|a| match a.as_ref() {
                "./..." if !whole_root => self
                    .0
                    .iter()
                    .map(|(_, dir)| format!("{}/...", dir_arg(dir)))
                    .collect::<Vec<_>>(),
                a => vec![a.to_string()],
            })
            .collect()
    }

    /// The path below the root of import path `path` (a package, or a file
    /// in one), from the innermost module that holds it: `""` for a
    /// module at the root's own package. `None` outside every module.
    fn relative(&self, path: &str) -> Option<String> {
        self.0
            .iter()
            .filter_map(|(module, dir)| {
                let rest = if path == module {
                    ""
                } else {
                    path.strip_prefix(module.as_str())?.strip_prefix('/')?
                };
                Some((module.len(), dir, rest))
            })
            .max_by_key(|(len, _, _)| *len)
            .map(|(_, dir, rest)| {
                [dir.as_str(), rest]
                    .iter()
                    .filter(|p| !p.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join("/")
            })
    }
}

/// The directories a `go.work` `use`s: `use ./a` lines and `use ( … )`
/// blocks, comments dropped.
fn work_uses(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut block = false;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or_default().trim();
        if block {
            match line.strip_prefix(')') {
                Some(_) => block = false,
                None if line.is_empty() => {}
                None => out.push(line.trim_matches('"').to_string()),
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("use") else {
            continue;
        };
        if !rest.starts_with([' ', '\t', '(']) {
            continue;
        }
        let rest = rest.trim();
        if let Some(inner) = rest.strip_prefix('(') {
            match inner.strip_suffix(')') {
                Some(one_line) => out.extend(
                    one_line
                        .split_whitespace()
                        .map(|d| d.trim_matches('"').to_string()),
                ),
                None => {
                    block = true;
                    out.extend(
                        inner
                            .split_whitespace()
                            .map(|d| d.trim_matches('"').to_string()),
                    );
                }
            }
        } else if !rest.is_empty() {
            out.push(rest.trim_matches('"').to_string());
        }
    }
    out
}

/// A `use` directory as a plain path from the root: `./svc/` is `svc`,
/// `./a/../../service` is `../service`, an absolute path stays absolute.
fn from_root(dir: &str) -> Option<String> {
    let path = Path::new(dir);
    if path.is_absolute() {
        let s = path.to_str()?.trim_end_matches('/');
        return Some(if s.is_empty() {
            String::from("/")
        } else {
            s.to_string()
        });
    }
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            std::path::Component::Normal(p) => parts.push(p.to_str()?.to_string()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if parts.last().is_some_and(|p| p != "..") {
                    parts.pop();
                } else {
                    parts.push(String::from(".."));
                }
            }
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Whether a path from the root already reads as one to `go test` (it
/// leaves the root or is absolute), so it takes no `./` prefix.
fn outside_root(path: &str) -> bool {
    path == ".." || path.starts_with("../") || Path::new(path).is_absolute()
}

/// A path from the root spelled as a `go test` package argument.
fn dir_arg(path: &str) -> String {
    if outside_root(path) {
        path.to_string()
    } else {
        format!("./{path}")
    }
}

/// The tree's name for the package at import path `pkg`.
pub fn package_id(modules: &Modules, pkg: &str) -> String {
    match modules.relative(pkg) {
        Some(rel) if rel.is_empty() => String::from("."),
        Some(rel) => dir_arg(&rel),
        None => pkg.to_string(),
    }
}

/// The directory of a package id, when it is one of the workspace's own
/// (below the root, or a `go.work` module beside or outside it).
pub fn package_dir(root: &Path, package: &str) -> Option<PathBuf> {
    match package {
        "." => Some(root.to_path_buf()),
        p if outside_root(p) => Some(root.join(p)),
        p => p.strip_prefix("./").map(|rest| root.join(rest)),
    }
}

/// One `go test -json` event: only the fields croft reads.
#[derive(serde::Deserialize)]
struct Event {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Package", default)]
    package: String,
    #[serde(rename = "Test", default)]
    test: Option<String>,
    #[serde(rename = "Output", default)]
    output: Option<String>,
}

fn event(line: &str) -> Option<Event> {
    serde_json::from_str(line.trim()).ok()
}

/// A finished test from one `go test -json` line: `pass`, `fail` or `skip`
/// for a named test. Package-level results and output are not cases.
pub fn parse_event(modules: &Modules, line: &str) -> Option<TestCase> {
    let e = event(line)?;
    let status = match e.action.as_str() {
        "pass" => TestStatus::Passed,
        "fail" => TestStatus::Failed,
        "skip" => TestStatus::Skipped,
        _ => return None,
    };
    let test = e.test?;
    Some(TestCase {
        name: format!("{}::{test}", package_id(modules, &e.package)),
        status,
    })
}

/// A test `go test -json -list .` names: its output lines that are a bare
/// `Test…`, `Example…` or `Fuzz…` identifier. The `ok  pkg 0.1s` trailer
/// and benchmarks (which a plain run skips) are not.
pub fn parse_list_event(modules: &Modules, line: &str) -> Option<String> {
    let e = event(line)?;
    if e.action != "output" {
        return None;
    }
    let name = e.output?.trim_end().to_string();
    let runnable = ["Test", "Example", "Fuzz"]
        .iter()
        .any(|p| name.starts_with(p));
    let ident = name.chars().all(|c| c.is_alphanumeric() || c == '_');
    (runnable && ident).then(|| format!("{}::{name}", package_id(modules, &e.package)))
}

/// What the OUTPUT channel shows for a `go test -json` line: the text the
/// test printed, not the JSON around it. A line that is not an event (a
/// build error on stdout) shows as it is.
pub fn shown(line: &str) -> Option<String> {
    match event(line) {
        Some(e) => e.output.map(|o| o.trim_end_matches('\n').to_string()),
        None => Some(line.to_string()),
    }
}

/// `-run` for a test name: each `/`-separated level anchored and escaped,
/// the way `go test` matches subtests level by level.
fn run_pattern(test: &str) -> String {
    test.split('/')
        .map(|level| format!("^{}$", regex_escape(level)))
        .collect::<Vec<_>>()
        .join("/")
}

/// The package and `-run` arguments that select `id`: `./pkg::TestName`
/// runs that one test (and its subtests), a bare package id the whole
/// package.
fn select(id: &str) -> Vec<String> {
    match id.split_once("::") {
        Some((pkg, test)) => vec![pkg.to_string(), String::from("-run"), run_pattern(test)],
        None => vec![id.to_string()],
    }
}

/// Arguments after `go test -json` for one exact test.
pub fn one_args(id: &str) -> Vec<String> {
    select(id)
}

/// Arguments after `go test -json` for a filter run. A suite click passes
/// a package or `package::Test` id; run-at-cursor a bare function name,
/// which, like cargo's filter, may match in any package.
pub fn filter_args(pattern: &str, suite: bool) -> Vec<String> {
    if suite || pattern.contains("::") {
        select(pattern)
    } else {
        vec![
            String::from("./..."),
            String::from("-run"),
            regex_escape(pattern),
        ]
    }
}

/// The debuggee's argv for delve's `test` mode: the same selection as
/// [`one_args`], spelled as the test binary's own flag.
pub fn test_binary_args(id: &str) -> Vec<String> {
    match id.split_once("::") {
        Some((_, test)) => vec![String::from("-test.run"), run_pattern(test)],
        None => vec![String::from("-test.run"), regex_escape(id)],
    }
}

/// Turn a Go cover profile into LCOV `DA:` records, paths relative to the
/// workspace root. Each block's count applies to every line it spans; where
/// blocks share a line, the line keeps the highest count. Files outside
/// the module are left out: there is nothing in the workspace to paint.
pub fn cover_profile_to_lcov(profile: &str, modules: &Modules) -> String {
    let mut files: BTreeMap<String, BTreeMap<u32, u64>> = BTreeMap::new();
    for line in profile.lines().skip_while(|l| l.starts_with("mode:")) {
        // `import/path/file.go:3.24,4.13 1 1`
        let Some((file, rest)) = line.rsplit_once(':') else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let (Some(span), Some(_stmts), Some(Ok(count))) = (
            parts.next(),
            parts.next(),
            parts.next().map(str::parse::<u64>),
        ) else {
            continue;
        };
        let Some((start, end)) = span.split_once(',') else {
            continue;
        };
        let line_of = |pos: &str| pos.split('.').next().and_then(|l| l.parse::<u32>().ok());
        let (Some(from), Some(to)) = (line_of(start), line_of(end)) else {
            continue;
        };
        let Some(rel) = modules.relative(file).filter(|r| !r.is_empty()) else {
            continue;
        };
        let hits = files.entry(rel).or_default();
        for n in from..=to {
            let h = hits.entry(n).or_insert(0);
            *h = (*h).max(count);
        }
    }
    let mut out = String::new();
    for (file, hits) in files {
        out.push_str(&format!("SF:{file}\n"));
        for (n, h) in hits {
            out.push_str(&format!("DA:{n},{h}\n"));
        }
        out.push_str("end_of_record\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Modules {
        Modules(vec![(String::from("example.com/m"), String::new())])
    }

    #[test]
    fn go_json_events_become_cases_named_by_package_directory() {
        let pass = r#"{"Action":"pass","Package":"example.com/m/foo","Test":"TestAdd/small_one","Elapsed":0}"#;
        assert_eq!(
            parse_event(&m(), pass),
            Some(TestCase {
                name: String::from("./foo::TestAdd/small_one"),
                status: TestStatus::Passed,
            })
        );
        let fail = r#"{"Action":"fail","Package":"example.com/m","Test":"TestRoot"}"#;
        assert_eq!(
            parse_event(&m(), fail).map(|c| (c.name, c.status)),
            Some((String::from(".::TestRoot"), TestStatus::Failed))
        );
        let skip = r#"{"Action":"skip","Package":"example.com/m/foo","Test":"TestSkip"}"#;
        assert_eq!(parse_event(&m(), skip).unwrap().status, TestStatus::Skipped);
        // A package's own result, a run marker and output are not cases.
        for line in [
            r#"{"Action":"fail","Package":"example.com/m/foo","Elapsed":0.003}"#,
            r#"{"Action":"run","Package":"example.com/m/foo","Test":"TestAdd"}"#,
            r#"{"Action":"output","Package":"example.com/m/foo","Test":"TestAdd","Output":"--- PASS\n"}"#,
            "# example.com/m/foo [build failed]",
        ] {
            assert_eq!(parse_event(&m(), line), None, "{line}");
        }
        // Outside the module the import path stays.
        assert_eq!(
            parse_event(
                &m(),
                r#"{"Action":"pass","Package":"other.org/x","Test":"TestX"}"#
            )
            .unwrap()
            .name,
            "other.org/x::TestX"
        );
    }

    #[test]
    fn listing_keeps_the_runnable_names_and_drops_the_trailer() {
        let out = |s: &str| {
            format!(r#"{{"Action":"output","Package":"example.com/m/foo","Output":"{s}\n"}}"#)
        };
        assert_eq!(
            parse_list_event(&m(), &out("TestAdd")).as_deref(),
            Some("./foo::TestAdd")
        );
        assert_eq!(
            parse_list_event(&m(), &out("ExampleAdd")).as_deref(),
            Some("./foo::ExampleAdd")
        );
        assert_eq!(parse_list_event(&m(), &out("BenchmarkAdd")), None);
        assert_eq!(
            parse_list_event(&m(), &out("ok  \\texample.com/m/foo\\t0.002s")),
            None
        );
    }

    #[test]
    fn run_arguments_anchor_each_subtest_level() {
        assert_eq!(
            one_args("./foo::TestAdd/small_one"),
            ["./foo", "-run", "^TestAdd$/^small_one$"]
        );
        assert_eq!(filter_args("./foo", true), ["./foo"]);
        assert_eq!(
            filter_args(".::TestRoot", true),
            [".", "-run", "^TestRoot$"]
        );
        assert_eq!(filter_args("TestAdd", false), ["./...", "-run", "TestAdd"]);
        assert_eq!(
            test_binary_args("./foo::TestAdd"),
            ["-test.run", "^TestAdd$"]
        );
    }

    #[test]
    fn the_module_path_and_package_directories() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("go.mod"),
            "// c\nmodule \"example.com/m\"\n\ngo 1.21\n",
        )
        .unwrap();
        assert_eq!(module_path(d.path()).as_deref(), Some("example.com/m"));
        assert_eq!(package_dir(d.path(), "."), Some(d.path().to_path_buf()));
        assert_eq!(package_dir(d.path(), "./a/b"), Some(d.path().join("a/b")));
        assert_eq!(package_dir(d.path(), "other.org/x"), None);
    }

    /// A `go.work` root with no module of its own: `lib` and `svc` (with an
    /// `inner` package) below it, `tools` named on a `use` line of its own,
    /// and a module outside the root.
    fn go_work_root() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let module = |dir: &str, path: &str| {
            std::fs::create_dir_all(d.path().join(dir)).unwrap();
            std::fs::write(
                d.path().join(dir).join("go.mod"),
                format!("module {path}\n\ngo 1.24\n"),
            )
            .unwrap();
        };
        module("lib", "example.com/lib");
        module("svc", "\"example.com/svc\"");
        module("tools", "example.com/tools");
        std::fs::write(
            d.path().join("go.work"),
            "go 1.24\n\nuse (\n\t./lib // the library\n\t\"./svc/\"\n)\nuse ./tools\nuse ../elsewhere\n",
        )
        .unwrap();
        d
    }

    /// #1505: a `go.work` root's packages are named by their directory,
    /// as a module root's are, so the tree and `go test` agree.
    #[test]
    fn a_go_work_roots_packages_are_named_by_directory() {
        let d = go_work_root();
        let modules = Modules::of(d.path());
        assert_eq!(
            modules,
            Modules(vec![
                (String::from("example.com/lib"), String::from("lib")),
                (String::from("example.com/svc"), String::from("svc")),
                (String::from("example.com/tools"), String::from("tools")),
            ])
        );
        assert_eq!(package_id(&modules, "example.com/lib"), "./lib");
        assert_eq!(package_id(&modules, "example.com/svc/inner"), "./svc/inner");
        assert_eq!(package_id(&modules, "example.com/svcx"), "example.com/svcx");
        assert_eq!(package_id(&modules, "other.org/x"), "other.org/x");
        let fail = r#"{"Action":"fail","Package":"example.com/svc","Test":"TestSvc"}"#;
        assert_eq!(parse_event(&modules, fail).unwrap().name, "./svc::TestSvc");
        assert_eq!(
            package_dir(d.path(), "./svc/inner"),
            Some(d.path().join("svc/inner"))
        );
        let profile = "mode: set\nexample.com/svc/inner/in.go:3.1,4.2 1 1\n";
        assert_eq!(
            cover_profile_to_lcov(profile, &modules),
            "SF:svc/inner/in.go\nDA:3,1\nDA:4,1\nend_of_record\n"
        );
    }

    /// #1505: `go test ./...` fails at a `go.work` root with no module of
    /// its own, so "every package" is one pattern per module there.
    #[test]
    fn every_package_at_a_go_work_root_is_one_pattern_per_module() {
        let d = go_work_root();
        assert_eq!(
            Modules::of(d.path()).expand(&["-list", ".", "./..."]),
            ["-list", ".", "./lib/...", "./svc/...", "./tools/..."]
        );
        assert_eq!(
            Modules::of(d.path()).expand(&["./svc", "-run", "^TestSvc$"]),
            ["./svc", "-run", "^TestSvc$"],
            "a single package is left as it is"
        );
    }

    /// #1505, negative: a module root (with or without a `go.work` beside
    /// it) and a root with neither keep `./...` and their ids.
    #[test]
    fn a_module_root_keeps_dot_dot_dot() {
        let d = go_work_root();
        std::fs::write(d.path().join("go.mod"), "module example.com/m\n").unwrap();
        let modules = Modules::of(d.path());
        assert_eq!(modules.expand(&["./..."]), ["./..."]);
        assert_eq!(package_id(&modules, "example.com/m/foo"), "./foo");
        assert_eq!(package_id(&modules, "example.com/lib"), "example.com/lib");
        let none = tempfile::tempdir().unwrap();
        assert_eq!(Modules::of(none.path()), Modules::default());
        assert_eq!(Modules::of(none.path()).expand(&["./..."]), ["./..."]);
    }

    /// #1505: a `go.work` may `use` modules beside or outside its root
    /// (`use ../service`, an absolute path). They are kept, named by their
    /// path from the root, and "every package" reaches them too.
    #[test]
    fn go_work_modules_outside_the_root_are_kept() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("ws");
        let module = |dir: &Path, path: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("go.mod"), format!("module {path}\n")).unwrap();
        };
        module(&d.path().join("service"), "example.com/service");
        module(&d.path().join("abs"), "example.com/abs");
        let abs = d.path().join("abs");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("go.work"), "go 1.24\n\nuse ../service/\n").unwrap();
        // Every module outside the root: `./...` must not survive.
        let modules = Modules::of(&ws);
        assert_eq!(
            modules,
            Modules(vec![(
                String::from("example.com/service"),
                String::from("../service")
            )])
        );
        assert_eq!(modules.expand(&["./..."]), ["../service/..."]);
        assert_eq!(package_id(&modules, "example.com/service"), "../service");
        assert_eq!(
            package_id(&modules, "example.com/service/inner"),
            "../service/inner"
        );
        let pass = r#"{"Action":"pass","Package":"example.com/service/inner","Test":"TestIn"}"#;
        let id = parse_event(&modules, pass).unwrap().name;
        assert_eq!(id, "../service/inner::TestIn");
        assert_eq!(one_args(&id), ["../service/inner", "-run", "^TestIn$"]);
        assert_eq!(
            package_dir(&ws, "../service/inner"),
            Some(ws.join("../service/inner"))
        );
        let profile = "mode: set\nexample.com/service/inner/in.go:3.1,3.9 1 2\n";
        let lcov = cover_profile_to_lcov(profile, &modules);
        assert_eq!(lcov, "SF:../service/inner/in.go\nDA:3,2\nend_of_record\n");
        // Mixed with one below the root and one by absolute path.
        module(&ws.join("lib"), "example.com/lib");
        std::fs::write(
            ws.join("go.work"),
            format!(
                "go 1.24\n\nuse (\n\t./lib\n\t./lib/../../service\n\t\"{}\"\n)\n",
                abs.display()
            ),
        )
        .unwrap();
        let modules = Modules::of(&ws);
        let abs = abs.to_str().unwrap().to_string();
        assert_eq!(
            modules.expand(&["./..."]),
            [
                String::from("./lib/..."),
                String::from("../service/..."),
                format!("{abs}/..."),
            ]
        );
        assert_eq!(
            package_id(&modules, "example.com/abs/x"),
            format!("{abs}/x")
        );
        assert_eq!(
            package_dir(&ws, &format!("{abs}/x")),
            Some(Path::new(&abs).join("x"))
        );
    }

    /// A `module` line's trailing comment is not part of the path.
    #[test]
    fn a_commented_module_directive_keeps_only_the_path() {
        let d = tempfile::tempdir().unwrap();
        for text in [
            "module example.com/service // owner note\n",
            "module \"example.com/service\" // owner note\n",
            "module example.com/service//note\n",
        ] {
            std::fs::write(d.path().join("go.mod"), text).unwrap();
            assert_eq!(
                module_path(d.path()).as_deref(),
                Some("example.com/service"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_cover_profile_becomes_lcov_line_hits() {
        let profile = "mode: set\n\
            example.com/m/foo/foo.go:3.24,4.13 1 1\n\
            example.com/m/foo/foo.go:4.13,6.3 1 0\n\
            example.com/m/foo/foo.go:7.2,7.14 1 1\n\
            other.org/x/x.go:1.1,2.2 1 1\n";
        assert_eq!(
            cover_profile_to_lcov(profile, &m()),
            "SF:foo/foo.go\nDA:3,1\nDA:4,1\nDA:5,0\nDA:6,0\nDA:7,1\nend_of_record\n"
        );
    }
}
