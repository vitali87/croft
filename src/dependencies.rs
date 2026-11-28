//! The workspace's dependencies, read without drawing anything: which
//! ecosystems the root's manifests name ([`detect_ecosystems`]), and the
//! packages each one depends on ([`fetch_dependencies`]): `cargo metadata`
//! for Rust, and a direct read of `package.json`, `go.mod`, `pyproject.toml`
//! or `requirements.txt` for the rest. The Explorer's DEPENDENCIES section
//! (`widgets/dependencies.rs`) draws them; the update check, the update
//! watcher and the test runner use `cargo_binary` to find cargo.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// A package ecosystem croft can list dependencies for, identified by the
/// manifest file it leaves in a project root. The discriminant order is the
/// order dependencies group under a polyglot root's combined list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecosystem {
    Rust,
    Python,
    Node,
    Go,
}

/// One resolved dependency, tagged with the ecosystem it came from so a
/// polyglot root's list groups by language before name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub version: String,
    pub ecosystem: Ecosystem,
}

/// Detect which package ecosystems a workspace root belongs to, by the manifest
/// files present at its top level. Cheap (a handful of `exists` checks, no
/// subprocess), so it is safe to call on every re-root; the result drives both
/// the section's visibility and its header label. Order follows [`Ecosystem`]'s
/// discriminant order, de-duplicated (Python is listed once even with both a
/// `pyproject.toml` and a `requirements.txt`).
pub fn detect_ecosystems(root: &Path) -> Vec<Ecosystem> {
    let mut found = Vec::new();
    if root.join("Cargo.toml").is_file() {
        found.push(Ecosystem::Rust);
    }
    if root.join("pyproject.toml").is_file() || root.join("requirements.txt").is_file() {
        found.push(Ecosystem::Python);
    }
    if root.join("package.json").is_file() {
        found.push(Ecosystem::Node);
    }
    if root.join("go.mod").is_file() {
        found.push(Ecosystem::Go);
    }
    found
}

/// Resolve the workspace's dependency set off the render thread, across every
/// detected ecosystem. Each ecosystem contributes a sorted, de-duplicated block
/// and the blocks concatenate in [`Ecosystem`] order. An empty vec means the
/// manifests parsed to nothing (or the relevant toolchain was unavailable) — the
/// panel renders that as an empty state, never an error.
pub fn fetch_dependencies(root: &Path) -> Vec<Dependency> {
    let mut deps = Vec::new();
    for eco in detect_ecosystems(root) {
        let mut block = match eco {
            Ecosystem::Rust => fetch_rust(root),
            Ecosystem::Python => fetch_python(root),
            Ecosystem::Node => fetch_node(root),
            Ecosystem::Go => fetch_go(root),
        };
        block.sort_by(|a: &Dependency, b: &Dependency| {
            a.name.cmp(&b.name).then(a.version.cmp(&b.version))
        });
        block.dedup();
        deps.append(&mut block);
    }
    deps
}

// --- Rust: `cargo metadata` (resolved, transitive) --------------------------

/// Minimal `cargo metadata` shape — only the fields the dependency list needs.
#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<MetaPackage>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct MetaPackage {
    id: String,
    name: String,
    version: String,
}

/// Absolute path to the `cargo` binary. Prefers one on `PATH`, then the usual
/// rustup / Homebrew locations. A GUI-launched croft inherits the stripped
/// launchd `PATH` that omits `~/.cargo/bin`, so a bare `Command::new("cargo")`
/// silently fails to spawn there (the "No dependencies" bug) even though `git`
/// — which lives in `/usr/bin` — is found. Resolving by absolute path, like
/// croft already does for language servers, makes it launch-agnostic.
pub(crate) fn cargo_binary() -> PathBuf {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    let cargo_home = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo").join("bin"));
    let dirs = path_dirs
        .into_iter()
        .chain(cargo_home)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    // Last resort: let the OS resolve it and fail honestly into the empty state.
    crate::proc::find::first_in("cargo", dirs).unwrap_or_else(|| PathBuf::from("cargo"))
}

fn fetch_rust(root: &Path) -> Vec<Dependency> {
    let Some(path_str) = root.to_str() else {
        return Vec::new();
    };
    let cargo = cargo_binary();
    let mut cmd = Command::new(&cargo);
    cmd.args(["metadata", "--format-version", "1"])
        .current_dir(path_str);
    // Prepend cargo's own dir to the child PATH so the rustup shim can find its
    // sibling `rustc` even when croft launched with a stripped PATH.
    if let Some(dir) = cargo.parent() {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![dir.to_path_buf()];
        dirs.extend(std::env::split_paths(&existing));
        if let Ok(joined) = std::env::join_paths(dirs) {
            cmd.env("PATH", joined);
        }
    }
    let Ok(output) = cmd.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_cargo_metadata(&output.stdout)
}

/// Parse `cargo metadata --format-version 1` JSON into the list of non-workspace
/// dependency crates.
pub fn parse_cargo_metadata(json: &[u8]) -> Vec<Dependency> {
    let Ok(meta) = serde_json::from_slice::<CargoMetadata>(json) else {
        return Vec::new();
    };
    let members: std::collections::HashSet<&str> =
        meta.workspace_members.iter().map(String::as_str).collect();
    meta.packages
        .iter()
        .filter(|p| !members.contains(p.id.as_str()))
        .map(|p| Dependency {
            name: p.name.clone(),
            version: p.version.clone(),
            ecosystem: Ecosystem::Rust,
        })
        .collect()
}

// --- Node: package.json `dependencies` + `devDependencies` ------------------

fn fetch_node(root: &Path) -> Vec<Dependency> {
    let Ok(text) = std::fs::read_to_string(root.join("package.json")) else {
        return Vec::new();
    };
    parse_package_json(&text)
}

/// Parse `package.json`'s `dependencies` and `devDependencies` maps into the
/// direct dependency list (the version is the manifest's range spec, e.g.
/// `^1.2.3`, shown verbatim).
pub fn parse_package_json(text: &str) -> Vec<Dependency> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut deps = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        let Some(map) = value.get(key).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, spec) in map {
            deps.push(Dependency {
                name: name.clone(),
                version: spec.as_str().unwrap_or_default().to_string(),
                ecosystem: Ecosystem::Node,
            });
        }
    }
    deps
}

// --- Go: go.mod `require` directives ----------------------------------------

fn fetch_go(root: &Path) -> Vec<Dependency> {
    let Ok(text) = std::fs::read_to_string(root.join("go.mod")) else {
        return Vec::new();
    };
    parse_go_mod(&text)
}

/// Parse `go.mod`'s `require` directives — both the single-line form
/// (`require module/path v1.2.3`) and the block form (`require ( … )`) — into
/// the module dependency list, dropping any trailing `// indirect` marker.
pub fn parse_go_mod(text: &str) -> Vec<Dependency> {
    let mut deps = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim();
        if in_block {
            if line == ")" {
                in_block = false;
            } else if let Some(dep) = parse_go_require_entry(line) {
                deps.push(dep);
            }
            continue;
        }
        if line == "require (" {
            in_block = true;
        } else if let Some(rest) = line.strip_prefix("require ")
            && let Some(dep) = parse_go_require_entry(rest.trim())
        {
            deps.push(dep);
        }
    }
    deps
}

/// One `module/path v1.2.3` entry from a go.mod require directive; the trailing
/// `// indirect` comment (if any) is dropped.
fn parse_go_require_entry(line: &str) -> Option<Dependency> {
    if line.is_empty() || line.starts_with("//") {
        return None;
    }
    let mut parts = line.split_whitespace();
    let name = parts.next()?.to_string();
    let version = parts.next().unwrap_or_default().to_string();
    Some(Dependency {
        name,
        version,
        ecosystem: Ecosystem::Go,
    })
}

// --- Python: pyproject.toml (PEP 621 / poetry) or requirements.txt ----------

fn fetch_python(root: &Path) -> Vec<Dependency> {
    if let Ok(text) = std::fs::read_to_string(root.join("pyproject.toml")) {
        let deps = parse_pyproject(&text);
        if !deps.is_empty() {
            return deps;
        }
    }
    if let Ok(text) = std::fs::read_to_string(root.join("requirements.txt")) {
        return parse_requirements_txt(&text);
    }
    Vec::new()
}

/// Parse `pyproject.toml` for direct dependencies. Handles both PEP 621
/// (`[project].dependencies`, a list of PEP 508 requirement strings) and poetry
/// (`[tool.poetry.dependencies]`, a name → version-spec table).
pub fn parse_pyproject(text: &str) -> Vec<Dependency> {
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut deps = Vec::new();

    if let Some(list) = table
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for item in list {
            if let Some(spec) = item.as_str() {
                deps.push(parse_pep508(spec));
            }
        }
    }

    if let Some(map) = table
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for (name, spec) in map {
            if name == "python" {
                continue;
            }
            deps.push(Dependency {
                name: name.clone(),
                version: poetry_version_spec(spec),
                ecosystem: Ecosystem::Python,
            });
        }
    }

    deps
}

/// A poetry dependency value is either a bare version string (`"^2.0"`) or an
/// inline table carrying a `version` key alongside extras/markers.
fn poetry_version_spec(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Table(t) => t
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Parse a `requirements.txt`, one requirement per line, skipping blanks,
/// comments, and option lines (`-r other.txt`, `--hash …`).
pub fn parse_requirements_txt(text: &str) -> Vec<Dependency> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .map(parse_pep508)
        .collect()
}

/// Split a PEP 508 requirement string (`requests>=2.0,<3 ; python_version>"3"`)
/// into its package name and the remaining version/marker spec. The name runs
/// up to the first version operator, bracket (extras), or marker separator.
fn parse_pep508(spec: &str) -> Dependency {
    let spec = spec.trim();
    let end = spec
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(spec.len());
    let name = spec[..end].to_string();
    let version = spec[end..].trim().to_string();
    Dependency {
        name,
        version,
        ecosystem: Ecosystem::Python,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_SAMPLE: &str = r#"{
        "packages": [
            {"id": "ws 0.1.0 (path+file:///repo)", "name": "ws", "version": "0.1.0"},
            {"id": "serde 1.0.0 (registry+x)", "name": "serde", "version": "1.0.0"},
            {"id": "anyhow 1.0.5 (registry+x)", "name": "anyhow", "version": "1.0.5"}
        ],
        "workspace_members": ["ws 0.1.0 (path+file:///repo)"]
    }"#;

    #[test]
    fn cargo_metadata_excludes_workspace_members() {
        let deps = parse_cargo_metadata(CARGO_SAMPLE.as_bytes());
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"serde") && names.contains(&"anyhow"));
        assert!(
            !names.contains(&"ws"),
            "the workspace member is never listed"
        );
        assert!(deps.iter().all(|d| d.ecosystem == Ecosystem::Rust));
    }

    #[test]
    fn cargo_metadata_garbage_yields_empty() {
        assert!(parse_cargo_metadata(b"not json").is_empty());
    }

    #[test]
    fn package_json_reads_both_dependency_maps() {
        let json = r#"{
            "dependencies": {"react": "^18.0.0"},
            "devDependencies": {"typescript": "5.4.0"}
        }"#;
        let deps = parse_package_json(json);
        let react = deps.iter().find(|d| d.name == "react").expect("react");
        assert_eq!(react.version, "^18.0.0");
        assert_eq!(react.ecosystem, Ecosystem::Node);
        assert!(
            deps.iter()
                .any(|d| d.name == "typescript" && d.version == "5.4.0")
        );
    }

    #[test]
    fn go_mod_reads_block_and_strips_indirect() {
        let go = "module example.com/x\n\ngo 1.22\n\nrequire (\n\tgithub.com/pkg/errors v0.9.1\n\tgolang.org/x/sys v0.1.0 // indirect\n)\n";
        let deps = parse_go_mod(go);
        let errs = deps
            .iter()
            .find(|d| d.name == "github.com/pkg/errors")
            .expect("errors");
        assert_eq!(errs.version, "v0.9.1");
        assert_eq!(errs.ecosystem, Ecosystem::Go);
        let sys = deps
            .iter()
            .find(|d| d.name == "golang.org/x/sys")
            .expect("sys");
        assert_eq!(sys.version, "v0.1.0", "the // indirect comment is dropped");
    }

    #[test]
    fn go_mod_reads_single_line_require() {
        let deps = parse_go_mod("require github.com/spf13/cobra v1.8.0\n");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "github.com/spf13/cobra");
        assert_eq!(deps[0].version, "v1.8.0");
    }

    #[test]
    fn pyproject_pep621_dependencies() {
        let py =
            "[project]\nname = \"x\"\ndependencies = [\n  \"requests>=2.0\",\n  \"loguru\",\n]\n";
        let deps = parse_pyproject(py);
        let req = deps
            .iter()
            .find(|d| d.name == "requests")
            .expect("requests");
        assert_eq!(req.version, ">=2.0");
        assert_eq!(req.ecosystem, Ecosystem::Python);
        let loguru = deps.iter().find(|d| d.name == "loguru").expect("loguru");
        assert_eq!(loguru.version, "", "a bare requirement has no version spec");
    }

    #[test]
    fn pyproject_poetry_table_skips_python() {
        let py = "[tool.poetry.dependencies]\npython = \"^3.11\"\nrequests = \"^2.0\"\nrich = { version = \"^13.0\", optional = true }\n";
        let deps = parse_pyproject(py);
        assert!(
            !deps.iter().any(|d| d.name == "python"),
            "the python pin is not a dependency"
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "requests" && d.version == "^2.0")
        );
        let rich = deps.iter().find(|d| d.name == "rich").expect("rich");
        assert_eq!(rich.version, "^13.0", "inline-table version is unwrapped");
    }

    #[test]
    fn requirements_txt_skips_comments_and_options() {
        let txt = "# top\nrequests==2.31.0\n\n-r other.txt\nloguru\n--hash=sha256:abc\n";
        let deps = parse_requirements_txt(txt);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["requests", "loguru"]);
        assert_eq!(deps[0].version, "==2.31.0");
    }

    #[test]
    fn detect_ecosystems_lists_each_manifest_once() {
        let dir = std::env::temp_dir().join(format!("croft-deps-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("cargo");
        std::fs::write(dir.join("pyproject.toml"), "[project]\n").expect("py");
        std::fs::write(dir.join("requirements.txt"), "loguru\n").expect("reqs");
        let ecos = detect_ecosystems(&dir);
        assert_eq!(
            ecos,
            vec![Ecosystem::Rust, Ecosystem::Python],
            "Python listed once despite two manifests"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_ecosystems_empty_for_plain_folder() {
        let dir = std::env::temp_dir().join(format!("croft-deps-plain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        assert!(detect_ecosystems(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
