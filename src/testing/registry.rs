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
use std::path::Path;

use super::worker::Runner;
use crate::lsp::manifest::{self, RunnerKindDecl, TestRunnerDecl};

/// Map a manifest's declared runner kind onto the run mechanism croft drives.
fn kind_of(decl: RunnerKindDecl) -> Runner {
    match decl {
        RunnerKindDecl::Cargo => Runner::Cargo,
        RunnerKindDecl::Pytest => Runner::Pytest,
        RunnerKindDecl::Vitest => Runner::Vitest,
        RunnerKindDecl::Jest => Runner::Jest,
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
/// file exists, or package.json names one of its dependency markers.
fn decl_matches(decl: &TestRunnerDecl, root: &Path) -> bool {
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
}
