//! Git for tests, shut off from the developer's own git setup (#2097, #2103).
//!
//! Every git command a test runs itself, to build a fixture repository or to
//! read one back, starts at [`test_git`] or [`test_git_command`], in any
//! module's tests. The one exception is a value the test compares with the
//! app's own git output: [`app_git`] reads that as the app does. A test that ran a plain `git` read the developer's
//! environment and global config instead: a `GIT_AUTHOR_NAME` set by a CI
//! runner or a sandbox authored the fixture's commits, and a hook's
//! `GIT_DIR` pointed git at another repository. The app's own git calls are
//! production code and never come here: they keep the user's settings.

use std::path::Path;
use std::process::Command;

/// The author and committer of every commit a test makes with
/// [`test_git`]. [`git_init_with_identity`] also writes it into the
/// repository's own config, for fixtures where the app commits; a fixture
/// that writes an identity of its own keeps that one for the app's commits.
pub(crate) const TEST_GIT_NAME: &str = "a";
pub(crate) const TEST_GIT_EMAIL: &str = "a@b";

/// `git`, shut off from the developer's own git setup, with no directory
/// set: for a command that names its own (`git clone <from> <to>`), or a
/// caller that adds its own `-C`. [`test_git`] is the same in a directory.
/// It inherits no `GIT_*` variable: no setting passed in the environment
/// (`GIT_CONFIG_COUNT` and `GIT_CONFIG_PARAMETERS` entries, a template dir,
/// a hash, dates, pathspec modes), and no `GIT_DIR`, `GIT_WORK_TREE` or
/// `GIT_INDEX_FILE` that would point it at another repository (git exports
/// those to a hook that runs the tests). It reads no global or system config
/// (so no default branch, signing, hooks, aliases or identity of theirs) and
/// no global ignore or attributes file. Commits are made as
/// [`TEST_GIT_NAME`] and no editor opens. All of it is set on the command,
/// never on the process.
pub(crate) fn test_git_command() -> Command {
    let mut git = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            git.env_remove(name);
        }
    }
    git.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // Where git finds the global `ignore` and `attributes` files, which
        // it reads whatever `GIT_CONFIG_GLOBAL` says.
        .env("XDG_CONFIG_HOME", "/dev/null")
        .env("GIT_AUTHOR_NAME", TEST_GIT_NAME)
        .env("GIT_AUTHOR_EMAIL", TEST_GIT_EMAIL)
        .env("GIT_COMMITTER_NAME", TEST_GIT_NAME)
        .env("GIT_COMMITTER_EMAIL", TEST_GIT_EMAIL)
        .env("GIT_EDITOR", "true");
    git
}

/// `git -C dir` through [`test_git_command`]: shut off from the developer's
/// own git setup.
pub(crate) fn test_git(dir: &Path) -> Command {
    let mut git = test_git_command();
    git.arg("-C").arg(dir);
    git
}

/// `git -C dir` run as the app runs git: in the developer's own environment
/// and config, like the production code. Only for reading back a value a
/// test then compares with what the app's own git call printed, so both
/// sides read the same settings (an abbreviation length, diff prefixes, a
/// log format) and agree whatever those are. Building a fixture never
/// starts here: that is [`test_git`].
pub(crate) fn app_git(dir: &Path) -> Command {
    let mut git = Command::new("git");
    git.arg("-C").arg(dir);
    git
}

/// `git init -q -b main` in `dir`: the first branch is `main` whatever the
/// developer's `init.defaultBranch` says.
pub(crate) fn git_init(dir: &Path) {
    git_ok(dir, &["init", "-q", "-b", "main"]);
}

/// [`git_init`], then [`TEST_GIT_NAME`] as the repository's own identity,
/// so the app's own commits there do not need the developer's.
pub(crate) fn git_init_with_identity(dir: &Path) {
    git_init(dir);
    git_ok(dir, &["config", "user.email", TEST_GIT_EMAIL]);
    git_ok(dir, &["config", "user.name", TEST_GIT_NAME]);
}

/// Runs [`test_git`] in `dir`, failing the test with git's error unless it
/// succeeds; its stdout, trimmed.
pub(crate) fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = test_git(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Runs [`test_git`] in `dir`; true when it succeeded. A git that cannot
/// start fails the test. A caller that ignores the answer tolerates a step
/// that fails.
pub(crate) fn git_succeeds(dir: &Path, args: &[&str]) -> bool {
    test_git(dir).args(args).output().unwrap().status.success()
}

/// Runs [`test_git`] in `dir`, succeeding or not; its stdout, trimmed.
pub(crate) fn git_stdout(dir: &Path, args: &[&str]) -> String {
    git_stdout_untrimmed(dir, args).trim().to_string()
}

/// [`git_stdout`] with git's trailing newline kept.
pub(crate) fn git_stdout_untrimmed(dir: &Path, args: &[&str]) -> String {
    let out = test_git(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2210 review: [`app_git`] is plain `git -C dir`, changing nothing in
    /// the environment, so what it reads matches what the app's own git
    /// calls read under any config the developer has.
    #[test]
    fn app_git_runs_git_as_the_app_does() {
        let git = app_git(Path::new("/some/repo"));
        assert_eq!(git.get_program(), "git");
        assert_eq!(git.get_envs().count(), 0, "no variable set or removed");
        let args: Vec<_> = git.get_args().collect();
        assert_eq!(args, ["-C", "/some/repo"]);
    }

    /// #2103: a commit made through [`test_git`] is authored by
    /// [`TEST_GIT_NAME`], whatever identity the environment names, and
    /// git reads no global config. Checked on the command's own
    /// environment, so the test changes nothing in the process that other
    /// tests share.
    #[test]
    fn test_git_drops_inherited_git_variables_and_global_config() {
        let git = test_git_command();
        let envs: Vec<_> = git.get_envs().collect();
        let value = |name: &str| {
            envs.iter()
                .find(|(k, _)| *k == name)
                .and_then(|(_, v)| *v)
                .map(|v| v.to_string_lossy().into_owned())
        };
        assert_eq!(value("GIT_AUTHOR_NAME").as_deref(), Some(TEST_GIT_NAME));
        assert_eq!(
            value("GIT_COMMITTER_EMAIL").as_deref(),
            Some(TEST_GIT_EMAIL)
        );
        assert_eq!(value("GIT_CONFIG_GLOBAL").as_deref(), Some("/dev/null"));
        assert_eq!(value("GIT_CONFIG_NOSYSTEM").as_deref(), Some("1"));
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy().into_owned();
            if name.starts_with("GIT_") {
                assert!(
                    envs.iter().any(|(k, _)| k.to_string_lossy() == name),
                    "{name} from the environment is neither removed nor replaced"
                );
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        std::fs::write(tmp.path().join("f"), "x").unwrap();
        git_ok(tmp.path(), &["add", "f"]);
        git_ok(tmp.path(), &["commit", "-q", "-m", "one"]);
        assert_eq!(
            git_ok(tmp.path(), &["log", "-1", "--format=%an <%ae> / %cn <%ce>"]),
            format!("{TEST_GIT_NAME} <{TEST_GIT_EMAIL}> / {TEST_GIT_NAME} <{TEST_GIT_EMAIL}>")
        );
        assert_eq!(
            git_stdout(tmp.path(), &["branch", "--show-current"]),
            "main"
        );
    }
}
