//! Debugging a Rust test: the harness executables `cargo test --no-run` builds, and the
//! one that holds the test.

use super::*;

/// One test-harness executable out of `cargo test --no-run`'s JSON: its
/// path, the target that built it, the target's root source file, and
/// whether that target is an integration test (kind `test`, one harness per
/// `tests/*.rs` — or per explicit `[[test]]` entry, which can rename the
/// target and point it anywhere; `src_path` is the only reliable link back
/// to the file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestBinary {
    pub path: PathBuf,
    pub target: String,
    pub src_path: PathBuf,
    pub integration: bool,
}

/// The test-profile executables from `cargo test --no-run
/// --message-format=json` output: compiler-artifact lines whose profile is
/// `test` and whose `executable` is set. Ranked lib first (unit tests
/// overwhelmingly live in the lib), then bin, then integration `test`
/// targets, preserving cargo's order within a rank. A src/lib.rs +
/// src/main.rs crate emits both a lib and a bin harness; the old last-wins
/// pick handed a lib test to the bin harness, which filters everything out
/// and exits before a breakpoint can bind.
pub fn test_binary_candidates(output: &str) -> Vec<TestBinary> {
    let mut ranked: Vec<(u8, TestBinary)> = Vec::new();
    for line in output.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact")
            || v.get("profile")
                .and_then(|p| p.get("test"))
                .and_then(|t| t.as_bool())
                != Some(true)
        {
            continue;
        }
        let Some(exe) = v.get("executable").and_then(|e| e.as_str()) else {
            continue;
        };
        let target = v.get("target");
        let has_kind = |want: &str| {
            target
                .and_then(|t| t.get("kind"))
                .and_then(|k| k.as_array())
                .is_some_and(|kinds| kinds.iter().filter_map(|k| k.as_str()).any(|k| k == want))
        };
        let kind_rank = if has_kind("lib") {
            0
        } else if has_kind("bin") {
            1
        } else {
            2
        };
        ranked.push((
            kind_rank,
            TestBinary {
                path: PathBuf::from(exe),
                target: target
                    .and_then(|t| t.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                src_path: PathBuf::from(
                    target
                        .and_then(|t| t.get("src_path"))
                        .and_then(|s| s.as_str())
                        .unwrap_or(""),
                ),
                integration: has_kind("test"),
            },
        ));
    }
    ranked.sort_by_key(|(r, _)| *r); // stable: cargo order kept within a rank
    ranked.into_iter().map(|(_, c)| c).collect()
}

/// Which candidate harness actually contains `name`: each is asked to
/// `--list` with the name as libtest's filter (fast — nothing runs), and the
/// first listing a test whose final `::` segment IS the name wins. The
/// filter alone is a substring match, so `lib_side_test` would otherwise
/// claim a harness that only knows `lib_side_test_extra`. cargo's JSON does
/// not say which target defines a test.
pub fn binary_containing_test(candidates: &[PathBuf], name: &str) -> Option<PathBuf> {
    let qualified = format!("::{name}");
    for exe in candidates {
        let Ok(out) = Command::new(exe)
            .args([name, "--list"])
            .stdin(Stdio::null())
            .output()
        else {
            continue;
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let listed = stdout.lines().any(|l| {
            l.trim_end()
                .strip_suffix(": test")
                .is_some_and(|t| t == name || t.ends_with(&qualified))
        });
        if listed {
            return Some(exe.clone());
        }
    }
    None
}

/// Build (or reuse) the workspace's test binaries and pick the harness that
/// contains `name`: `cargo test --no-run --message-format=json`, then a
/// `--list` probe when more than one harness could own the test. A nonzero
/// cargo exit is a build failure even if some targets' executables were
/// already emitted — launching a partial build would debug stale code
/// instead of surfacing the compile error. `source` (the file the debug
/// gesture happened in) narrows the harnesses first: a file that IS a
/// target's `src_path` owns that target outright (exact even for a renamed
/// `[[test]] path = ...` target the file stem cannot predict); a module
/// file narrows to its side of the build (`tests/` → integration harnesses,
/// anything else → lib/bin) and never widens further — a lib-first probe
/// over every harness is how a unit test used to steal an integration
/// test's bare name. Blocking — the app runs this on a background thread
/// and launches from the drain.
pub fn build_test_binary(
    root: &Path,
    name: &str,
    source: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let out = cargo_cmd(root, &["test", "--no-run", "--message-format=json"]).output()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "cargo test --no-run failed: {}",
            stderr
                .lines()
                .rev()
                .find(|l| l.starts_with("error"))
                .or_else(|| stderr.lines().last())
                .unwrap_or("")
                .trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let all = test_binary_candidates(&stdout);
    if all.is_empty() {
        return Err(std::io::Error::other(format!(
            "no test binary in cargo's build output: {}",
            stderr.lines().last().unwrap_or("")
        )));
    }
    let pool: Vec<&TestBinary> = match source {
        Some(src) => {
            // Canonicalize both sides: cargo reports resolved paths
            // (/private/var/... on macOS) while the editor may hold the
            // symlinked spelling of the same file.
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let src = canon(src);
            let exact: Vec<&TestBinary> =
                all.iter().filter(|c| canon(&c.src_path) == src).collect();
            if exact.is_empty() {
                let is_integration_src = src.strip_prefix(canon(root)).is_ok_and(|rel| {
                    rel.components()
                        .next()
                        .is_some_and(|c| c.as_os_str() == "tests")
                });
                let side: Vec<&TestBinary> = all
                    .iter()
                    .filter(|c| c.integration == is_integration_src)
                    .collect();
                if side.is_empty() {
                    return Err(std::io::Error::other(format!(
                        "no {} harness in the build owns {}",
                        if is_integration_src {
                            "integration-test"
                        } else {
                            "unit-test"
                        },
                        src.display()
                    )));
                }
                side
            } else {
                exact
            }
        }
        None => all.iter().collect(),
    };
    match pool.as_slice() {
        [only] => Ok(only.path.clone()),
        _ => {
            let paths: Vec<PathBuf> = pool.iter().map(|c| c.path.clone()).collect();
            binary_containing_test(&paths, name).ok_or_else(|| {
                std::io::Error::other(format!(
                    "none of the {} test harnesses lists a test named {name}",
                    paths.len()
                ))
            })
        }
    }
}
