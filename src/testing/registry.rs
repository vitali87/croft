//! Data-driven test-runner registry.
//!
//! Mirrors `dap::registry`: which test tool the Testing view drives for a
//! workspace is read from `[[test_runners]]` blocks in the bundled + user
//! extension manifests, not a hardcoded detection ladder. A runner whose
//! contributing extension is disabled in the Extensions panel is skipped, so
//! the Testing view stays empty for that project type — the same
//! "disabled = opt-out" toggle the viewers, LSP servers, and debug adapters
//! use.
//!
//! The run *mechanisms* themselves (cargo libtest / pytest / vitest / jest —
//! their commands, parsers, and binary resolution) are heterogeneous and stay
//! native in `worker.rs`; the manifest's `kind` field only selects which of
//! those croft drives. So a third party can retarget an existing mechanism
//! onto new marker files from a manifest, but cannot introduce a brand-new
//! runner without Rust — exactly the boundary the debug adapters draw.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::worker::Runner;
use crate::lsp::manifest::{self, RunnerKindDecl, TestRunnerDecl};

/// Map a manifest's declared runner kind onto the run mechanism croft drives.
fn kind_of(decl: RunnerKindDecl) -> Runner {
    match decl {
        RunnerKindDecl::Cargo => Runner::Cargo,
        RunnerKindDecl::Pytest => Runner::Pytest,
        RunnerKindDecl::Vitest => Runner::Vitest,
        RunnerKindDecl::Jest => Runner::Jest,
        RunnerKindDecl::Go => Runner::Go,
        RunnerKindDecl::Codeql => Runner::Codeql,
    }
}

/// Whether `root`'s package.json names any of `deps` in its dependencies or
/// devDependencies. The dep check (not mere package.json presence) is the
/// signal: plenty of repos carry a package.json only for docs tooling.
fn package_declares(root: &Path, deps: &[String]) -> bool {
    if deps.is_empty() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(root.join("package.json")) else {
        return false;
    };
    let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    ["devDependencies", "dependencies"].iter().any(|section| {
        pkg.get(section)
            .and_then(|d| d.as_object())
            .is_some_and(|d| deps.iter().any(|dep| d.contains_key(dep)))
    })
}

/// Whether the workspace at `root` matches one runner declaration: any marker
/// file exists, or package.json names one of its dependency markers. CodeQL's
/// markers are pack files, and every CodeQL repo has those: only one that
/// declares a test pack, at the root or a few folders below, counts.
fn decl_matches(decl: &TestRunnerDecl, root: &Path) -> bool {
    if decl.kind == RunnerKindDecl::Codeql {
        return !super::codeqltest::test_packs(root, &decl.markers).is_empty();
    }
    decl.markers.iter().any(|m| root.join(m).is_file())
        || package_declares(root, &decl.package_deps)
}

/// The runner for `root` from `sources`, skipping any whose extension id is in
/// `disabled`. First enabled match wins (bundled order: cargo, then the JS
/// runners, then pytest — a mixed repo's root is usually the crate). Pure over
/// sources/prefs (testable against a tempdir without touching real prefs).
fn resolve(sources: &[String], disabled: &BTreeSet<String>, root: &Path) -> Option<Runner> {
    sources
        .iter()
        .filter_map(|s| manifest::parse(s).ok())
        .flat_map(|m| {
            let id = m.id;
            m.test_runners.into_iter().map(move |d| (id.clone(), d))
        })
        .filter(|(id, _)| !disabled.contains(id))
        .find(|(_, d)| decl_matches(d, root))
        .map(|(_, d)| kind_of(d.kind))
}

/// Bundled + user manifest sources, in load order.
fn all_sources() -> Vec<String> {
    let mut sources: Vec<String> = manifest::BUNDLED_MANIFESTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    sources.extend(manifest::read_extension_sources(
        &manifest::user_extensions_dir(),
    ));
    sources
}

/// The test runner for a workspace root from the *enabled* extensions, or
/// `None` when no enabled runner claims it (the Testing view stays empty
/// instead of shelling a tool that would error). Reads prefs + manifests fresh
/// on each call — detection runs on discover/run, never per keystroke, so a
/// panel toggle takes effect on the next discovery.
pub fn runner_for(root: &Path) -> Option<Runner> {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    resolve(&all_sources(), &disabled, root)
}

/// The package folders a JS monorepo at `root` declares (#1520): the root
/// `package.json`'s `workspaces` (a list, or `{ "packages": [...] }`), else
/// `pnpm-workspace.yaml`'s `packages:` list, else `packages/*` and `apps/*`.
/// A pattern is a folder or a folder ending in `/*`; `node_modules` and
/// negated (`!`) patterns are skipped. Sorted, existing folders only.
fn workspace_packages(root: &Path) -> Vec<PathBuf> {
    let mut patterns: Vec<String> = std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|pkg| {
            let ws = pkg.get("workspaces")?;
            let list = ws.as_array().or_else(|| ws.get("packages")?.as_array())?;
            Some(
                list.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
            )
        })
        .unwrap_or_default();
    if patterns.is_empty()
        && let Ok(text) = std::fs::read_to_string(root.join("pnpm-workspace.yaml"))
    {
        let mut in_packages = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if !line.starts_with([' ', '\t', '-']) {
                in_packages = trimmed == "packages:";
                continue;
            }
            if in_packages && let Some(item) = trimmed.strip_prefix('-') {
                patterns.push(item.trim().trim_matches(['\'', '"']).to_owned());
            }
        }
    }
    if patterns.is_empty() {
        patterns = vec![String::from("packages/*"), String::from("apps/*")];
    }
    let mut dirs = BTreeSet::new();
    for pattern in &patterns {
        let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
        if pattern.starts_with('!') || pattern.split('/').any(|c| c == "node_modules") {
            continue;
        }
        match pattern.strip_suffix("/*") {
            Some(parent) if !parent.contains('*') => {
                let Ok(entries) = std::fs::read_dir(root.join(parent)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && entry.file_name() != "node_modules" {
                        dirs.insert(path);
                    }
                }
            }
            None if !pattern.contains('*') => {
                let path = root.join(pattern);
                if path.is_dir() {
                    dirs.insert(path);
                }
            }
            _ => {}
        }
    }
    dirs.into_iter().collect()
}

/// The folder the Testing view runs in for workspace folder `root` with
/// `file` focused (#1520). `root` itself when a runner claims it, as
/// always. Otherwise, as in a monorepo whose packages each declare their
/// own vitest or jest: the nearest folder between `file` and `root` that a
/// runner claims, else the first workspace package one claims, else `root`
/// (which then reports no runner). Pure over `sources` and `disabled`.
fn resolve_test_root(
    sources: &[String],
    disabled: &BTreeSet<String>,
    root: &Path,
    file: Option<&Path>,
) -> PathBuf {
    if resolve(sources, disabled, root).is_some() {
        return root.to_path_buf();
    }
    let claimed = |dir: &Path| resolve(sources, disabled, dir).is_some();
    let in_package = file
        .and_then(Path::parent)
        .filter(|dir| dir.starts_with(root))
        .and_then(|dir| {
            dir.ancestors()
                .take_while(|d| *d != root)
                .filter(|d| {
                    d.strip_prefix(root)
                        .is_ok_and(|rel| !rel.components().any(|c| c.as_os_str() == "node_modules"))
                })
                .find(|d| claimed(d))
                .map(Path::to_path_buf)
        });
    in_package
        .or_else(|| workspace_packages(root).into_iter().find(|d| claimed(d)))
        .unwrap_or_else(|| root.to_path_buf())
}

/// [`resolve_test_root`] over the enabled extensions.
pub fn test_root_for(root: &Path, file: Option<&Path>) -> PathBuf {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    resolve_test_root(&all_sources(), &disabled, root, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled() -> Vec<String> {
        manifest::BUNDLED_MANIFESTS
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn bundled_manifests_prefer_cargo_then_python_markers() {
        let s = bundled();
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve(&s, &none(), tmp.path()),
            None,
            "no manifest, no runner"
        );
        std::fs::write(tmp.path().join("pyproject.toml"), "[project]\n").unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Pytest));
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Cargo));

        let py = tempfile::tempdir().unwrap();
        std::fs::write(py.path().join("pytest.ini"), "[pytest]\n").unwrap();
        assert_eq!(resolve(&s, &none(), py.path()), Some(Runner::Pytest));
    }

    #[test]
    fn bundled_manifests_identify_js_runners_from_package_json() {
        let s = bundled();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"devDependencies":{"vitest":"^3.0.0"}}"#,
        )
        .unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Vitest));
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"devDependencies":{"jest":"^30.0.0"}}"#,
        )
        .unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Jest));
        // A package.json naming neither runner detects nothing (docs tooling,
        // a plain library) instead of shelling a tool that isn't there.
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"dependencies":{"react":"^19.0.0"}}"#,
        )
        .unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), None);
        // A config file marks the runner when the dep is hoisted away.
        std::fs::write(tmp.path().join("vitest.config.ts"), "").unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Vitest));
        // Cargo still outranks JS at a mixed root.
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Cargo));

        let jest_cfg = tempfile::tempdir().unwrap();
        std::fs::write(jest_cfg.path().join("package.json"), "{}").unwrap();
        std::fs::write(jest_cfg.path().join("jest.config.js"), "").unwrap();
        assert_eq!(resolve(&s, &none(), jest_cfg.path()), Some(Runner::Jest));
    }

    #[test]
    fn a_go_module_routes_to_go_test() {
        let s = bundled();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("go.mod"), "module example.com/m\n").unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Go));
        let off: BTreeSet<String> = [String::from("test-go")].into();
        assert_eq!(resolve(&s, &off, tmp.path()), None);
    }

    #[test]
    fn a_codeql_test_pack_routes_to_codeql_but_a_query_pack_does_not() {
        let s = bundled();
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        // A plain query pack is not a test project.
        write(
            "src/qlpack.yml",
            "name: acme/queries\ndependencies:\n  codeql/go-all: '*'\n",
        );
        write("src/Find.ql", "select 1");
        assert_eq!(resolve(&s, &none(), tmp.path()), None);
        // A test pack below the root is.
        write(
            "test/qlpack.yml",
            "name: acme/tests\nextractor: go\ntests: .\n",
        );
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Codeql));
        let off: BTreeSet<String> = [String::from("test-codeql")].into();
        assert_eq!(resolve(&s, &off, tmp.path()), None);
        // Another project's marker at the root still wins.
        write("go.mod", "module example.com/m\n");
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Go));
    }

    #[test]
    fn a_disabled_runner_extension_stops_claiming_its_projects() {
        let s = bundled();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(tmp.path().join("pyproject.toml"), "[project]\n").unwrap();
        let mut disabled = BTreeSet::new();
        disabled.insert("test-cargo".to_string());
        // Cargo no longer claims the root, so the next enabled match wins...
        assert_eq!(resolve(&s, &disabled, tmp.path()), Some(Runner::Pytest));
        // ...and with pytest off too, nothing does.
        disabled.insert("test-pytest".to_string());
        assert_eq!(resolve(&s, &disabled, tmp.path()), None);
    }

    #[test]
    fn a_user_manifest_can_retarget_a_built_in_mechanism_to_new_markers() {
        let mut s = bundled();
        // A third-party manifest pointing the pytest mechanism at nox projects.
        s.push(
            r#"
id = "test-nox"
name = "Nox Test Runner"
api_version = 1
[[test_runners]]
id = "test-nox"
label = "pytest (nox)"
kind = "pytest"
markers = ["noxfile.py"]
"#
            .to_string(),
        );
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("noxfile.py"), "").unwrap();
        assert_eq!(resolve(&s, &none(), tmp.path()), Some(Runner::Pytest));
    }

    /// The issue's monorepo: vitest only in `packages/math`.
    fn vitest_monorepo(root: &Path) -> PathBuf {
        let math = root.join("packages/math");
        std::fs::create_dir_all(math.join("src")).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"vmono","private":true,"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        std::fs::write(
            math.join("package.json"),
            r#"{"name":"math","type":"module","devDependencies":{"vitest":"^5.0.3"}}"#,
        )
        .unwrap();
        std::fs::write(math.join("vitest.config.ts"), "export default {};\n").unwrap();
        std::fs::write(math.join("src/add.test.ts"), "test('adds', () => {});\n").unwrap();
        std::fs::create_dir_all(root.join("packages/docs")).unwrap();
        std::fs::write(
            root.join("packages/docs/package.json"),
            r#"{"name":"docs"}"#,
        )
        .unwrap();
        math
    }

    /// #1520: a monorepo whose root declares no runner tests in the package
    /// that declares one: the focused file's, else the first such package.
    #[test]
    fn a_package_that_declares_its_own_runner_is_the_test_root() {
        let s = bundled();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let math = vitest_monorepo(root);
        assert_eq!(resolve(&s, &none(), root), None);
        let test = math.join("src/add.test.ts");
        assert_eq!(resolve_test_root(&s, &none(), root, Some(&test)), math);
        assert_eq!(resolve_test_root(&s, &none(), root, None), math);
        let readme = root.join("README.md");
        assert_eq!(resolve_test_root(&s, &none(), root, Some(&readme)), math);
    }

    /// #1520: jest in a pnpm workspace's `apps/web`, and the focused file
    /// picks between two packages on different runners.
    #[test]
    fn pnpm_packages_and_the_focused_package_pick_the_runner() {
        let s = bundled();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let math = vitest_monorepo(root);
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"vmono","private":true}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - 'apps/*'\n  - \"packages/*\"\n  - '!**/test/**'\n",
        )
        .unwrap();
        let web = root.join("apps/web");
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("jest.config.js"), "module.exports = {};\n").unwrap();
        assert_eq!(
            workspace_packages(root),
            vec![web.clone(), root.join("packages/docs"), math.clone()]
        );
        assert_eq!(resolve_test_root(&s, &none(), root, None), web);
        assert_eq!(resolve(&s, &none(), &web), Some(Runner::Jest));
        let test = math.join("src/add.test.ts");
        assert_eq!(resolve_test_root(&s, &none(), root, Some(&test)), math);
        assert_eq!(resolve(&s, &none(), &math), Some(Runner::Vitest));
    }

    /// #1520, negative: a root a runner claims stays the test root; a
    /// package under `node_modules`, a disabled runner, a file outside the
    /// folder and a repo with no runner anywhere all leave the root.
    #[test]
    fn the_root_stays_the_test_root_unless_only_a_package_has_a_runner() {
        let s = bundled();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let math = vitest_monorepo(root);
        let test = math.join("src/add.test.ts");

        let disabled: BTreeSet<String> = s
            .iter()
            .filter_map(|m| manifest::parse(m).ok())
            .filter(|m| {
                m.test_runners
                    .iter()
                    .any(|d| d.kind == RunnerKindDecl::Vitest)
            })
            .map(|m| m.id)
            .collect();
        assert!(!disabled.is_empty());
        assert_eq!(resolve_test_root(&s, &disabled, root, Some(&test)), root);

        let other = tempfile::tempdir().unwrap();
        let dep = other.path().join("node_modules/lib");
        std::fs::create_dir_all(&dep).unwrap();
        std::fs::write(dep.join("vitest.config.ts"), "export default {};\n").unwrap();
        std::fs::write(dep.join("x.test.ts"), "").unwrap();
        assert_eq!(
            resolve_test_root(&s, &none(), other.path(), Some(&dep.join("x.test.ts"))),
            other.path()
        );
        assert_eq!(
            resolve_test_root(&s, &none(), other.path(), Some(&test)),
            other.path(),
            "a file in another folder is not this folder's package"
        );

        std::fs::write(
            root.join("package.json"),
            r#"{"private":true,"workspaces":["packages/*"],"devDependencies":{"jest":"29"}}"#,
        )
        .unwrap();
        assert_eq!(resolve(&s, &none(), root), Some(Runner::Jest));
        assert_eq!(resolve_test_root(&s, &none(), root, Some(&test)), root);
    }

    /// #1520: `workspaces` in its object form, and patterns croft does not
    /// expand, are read without guessing.
    #[test]
    fn workspace_packages_reads_the_object_form_and_skips_what_it_cannot_expand() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in ["libs/a", "libs/b", "tools", "deep/x/y"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(
            root.join("package.json"),
            r#"{"workspaces":{"packages":["libs/*","./tools/","deep/**","!libs/b","node_modules/*"]}}"#,
        )
        .unwrap();
        assert_eq!(
            workspace_packages(root),
            vec![root.join("libs/a"), root.join("libs/b"), root.join("tools")]
        );
        let bare = tempfile::tempdir().unwrap();
        assert!(workspace_packages(bare.path()).is_empty());
    }
}
