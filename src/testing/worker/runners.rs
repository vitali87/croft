//! Which runner runs a workspace's tests: the extensions' `[[test_runners]]`, every
//! runner that claims a folder, and the one a file's or a test's language picks.

use super::*;

/// Detect the workspace's test runner from the enabled extensions'
/// `[[test_runners]]` declarations. `None` means no recognised test project,
/// so the Testing view stays empty instead of shelling a tool that would error.
pub fn runner_for(extensions_dir: &Path, root: &Path) -> Option<Runner> {
    crate::testing::registry::runner_for(extensions_dir, root)
}

/// Every runner that claims `root` (#1408): a folder with two languages'
/// tests has a runner for each, and the Testing view lists both.
pub fn runners_for(extensions_dir: &Path, root: &Path) -> Vec<Runner> {
    crate::testing::registry::runners_for(extensions_dir, root)
}

/// The runners whose tests a file of this kind holds, by its extension.
pub(super) fn runners_of_file(file: &Path) -> &'static [Runner] {
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rs" => &[Runner::Cargo],
        "py" => &[Runner::Pytest],
        "go" => &[Runner::Go],
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => {
            &[Runner::Vitest, Runner::Jest]
        }
        "ql" | "qlref" | "expected" => &[Runner::Codeql],
        _ => &[],
    }
}

/// The runner of `runners` that runs the tests in `file` (#1408): the one
/// for the file's language, else the folder's first. A pytest file in a
/// maturin crate is pytest's, not Cargo's.
pub fn runner_for_file(runners: &[Runner], file: Option<&Path>) -> Option<Runner> {
    file.map(runners_of_file)
        .unwrap_or(&[])
        .iter()
        .copied()
        .find(|r| runners.contains(r))
        .or_else(|| runners.first().copied())
}

/// The runner a run of `name` goes to (#1408): the file's language when the
/// run came from a file, else the runner that listed `name` (or, for a
/// suite, a test under it), else the folder's first. Where the file's
/// language has two runners (Vitest and Jest), the one that listed `name`
/// or the file's tests decides.
pub(super) fn choose_runner(
    runners: &[Runner],
    owners: &BTreeMap<String, Runner>,
    name: &str,
    file: Option<&Path>,
) -> Option<Runner> {
    let of_file: Vec<Runner> = file
        .map(runners_of_file)
        .unwrap_or(&[])
        .iter()
        .copied()
        .filter(|r| runners.contains(r))
        .collect();
    // A suite owns what sits under it at the `::` separator, not every
    // name that starts with its text: Rust's `tests` is not pytest's
    // `tests/test_x.py`.
    let suite = crate::testing::suite_pattern(name);
    let by_name = owners
        .get(name)
        .or_else(|| {
            owners
                .iter()
                .find(|(listed, _)| listed.starts_with(&suite))
                .map(|(_, r)| r)
        })
        .copied()
        .filter(|r| runners.contains(r));
    let by_file = match of_file.as_slice() {
        [] => None,
        [only] => Some(*only),
        several => by_name
            .filter(|r| several.contains(r))
            .or_else(|| file.and_then(|f| file_owner(owners, f, several)))
            .or_else(|| several.first().copied()),
    };
    by_file.or(by_name).or_else(|| runners.first().copied())
}

/// The runner among `candidates` that listed a test of `file`: JS test
/// names start with the file's path relative to the root.
pub(super) fn file_owner(
    owners: &BTreeMap<String, Runner>,
    file: &Path,
    candidates: &[Runner],
) -> Option<Runner> {
    owners
        .iter()
        .find(|(listed, r)| {
            candidates.contains(r)
                && listed
                    .split_once("::")
                    .is_some_and(|(path, _)| !path.is_empty() && file.ends_with(path))
        })
        .map(|(_, r)| *r)
}
