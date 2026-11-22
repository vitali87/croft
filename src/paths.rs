//! Where croft keeps its files on disk: the config directory (settings,
//! keybindings, user extensions), the state directory (data that is the
//! user's own and must outlive a cache clean) and the cache directory (what
//! can be rebuilt).
//!
//! These are leaf helpers with no dependency on the rest of croft, so any
//! module can find its files without reaching into `prefs` (and through it,
//! half the crate). See docs/EXTENSIONS.md, phase 1.

use std::path::PathBuf;

/// croft's config directory: `$XDG_CONFIG_HOME/croft` when that is set and
/// non-empty, otherwise `~/.config/croft`.
#[cfg(not(test))]
pub(crate) fn config_dir() -> PathBuf {
    config_dir_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Test builds' config directory: one of the test process's own, never the
/// developer's (#2062). `App::new` reads the user's macros, snippets,
/// triggers and agents from here, so a file in the real one used to steer
/// every test that built an app. It is made fresh under the temp directory,
/// with a random name, the first time it is asked for, so it starts empty as
/// on CI: a name from the process id alone could find what an earlier run
/// with the same id left there. The settings layers stay in a directory of
/// their own that nothing creates (`config_layers::user_layers_dir`, #624), so
/// a test that writes a file here never changes the settings every app test
/// reads.
#[cfg(test)]
pub(crate) fn config_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("croft-test-config-dir-")
            .tempdir()
            .expect("a fresh temp dir for the test build's config dir")
            .keep()
    })
    .clone()
}

/// The config directory a release build would use in this environment: in a
/// test build, the developer's own. Tests check they never reach it.
#[cfg(test)]
pub(crate) fn real_config_dir() -> PathBuf {
    config_dir_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// [`config_dir`] from the given `XDG_CONFIG_HOME` and `HOME`.
fn config_dir_from(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(xdg) = xdg_config_home.filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("croft");
    }
    home.map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("croft")
}

/// Where croft keeps data that is the user's own and must outlive a cache
/// clean (#1578): hot-exit backups of unsaved edits and sticky notes. XDG's
/// state directory, `$XDG_STATE_HOME/croft`, by default
/// `~/.local/state/croft`; the cache directory is for what can be rebuilt,
/// and cleaners and `rm -rf ~/.cache` delete it.
pub(crate) fn state_dir() -> PathBuf {
    state_dir_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// [`state_dir`] from the given `XDG_STATE_HOME` and `HOME`. The spec
/// counts a relative value (an empty one included) as invalid, so it falls
/// back to the default rather than landing under the working directory.
fn state_dir_from(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(xdg) = xdg_state_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return xdg.join("croft");
    }
    home.map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("state")
        .join("croft")
}

/// Test-only redirect for [`croft_cache_dir`], so the relay tests steer the
/// relay rendezvous without repointing the process-global `$HOME` (#37):
/// every concurrent test thread resolves `$HOME` at call time (the PDF
/// rasteriser's scratch dir, the session paths), and a substituted value
/// leaking across threads made unrelated tests fail inside the fake home.
#[cfg(test)]
pub static CACHE_DIR_OVERRIDE_FOR_TEST: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);

/// croft's cache directory, `~/.cache/croft`: downloads, scratch files and
/// anything else that can be rebuilt, so a cache clean costs nothing but time.
pub(crate) fn croft_cache_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap().clone() {
        return dir;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("croft")
}

#[cfg(test)]
mod tests {
    /// #2062: a test build never reads or writes the developer's own config
    /// directory, the one croft itself would use with this environment.
    #[test]
    fn a_test_build_has_a_config_dir_of_its_own() {
        let dir = super::config_dir();
        assert!(!dir.starts_with(super::real_config_dir()), "{dir:?}");
        // Made fresh for this run, not named from the process id alone,
        // which a later run can be given again (review on #2547).
        assert!(dir.is_dir(), "{dir:?}");
        assert_ne!(
            dir.file_name().unwrap(),
            format!("croft-test-config-dir-{}", std::process::id()).as_str()
        );
        assert!(dir.starts_with(std::env::temp_dir()), "{dir:?}");
        assert_eq!(dir, super::config_dir(), "one directory for the whole run");
    }

    /// The config directory is `$XDG_CONFIG_HOME/croft`, or `~/.config/croft`
    /// when that is unset or empty.
    #[test]
    fn the_config_dir_follows_xdg_config_home_then_home() {
        use std::path::PathBuf;
        assert_eq!(
            super::config_dir_from(Some("/x/config".into()), Some("/home/u".into())),
            PathBuf::from("/x/config/croft")
        );
        for unset in [None, Some("".into())] {
            assert_eq!(
                super::config_dir_from(unset, Some("/home/u".into())),
                PathBuf::from("/home/u/.config/croft")
            );
        }
    }

    /// #1578: hot-exit backups and sticky notes are the user's own text, so
    /// they live in XDG's state directory, not the cache that cleaners and
    /// `rm -rf ~/.cache` are free to delete.
    #[test]
    fn user_data_lives_in_the_xdg_state_directory() {
        use std::path::PathBuf;
        assert_eq!(
            super::state_dir_from(Some("/x/state".into()), Some("/home/u".into())),
            PathBuf::from("/x/state/croft")
        );
        assert_eq!(
            super::state_dir_from(None, Some("/home/u".into())),
            PathBuf::from("/home/u/.local/state/croft")
        );
    }

    /// Negative: an empty or relative `XDG_STATE_HOME` is invalid under the
    /// XDG spec and is ignored, never joined onto the working directory.
    #[test]
    fn an_empty_or_relative_xdg_state_home_is_ignored() {
        use std::path::PathBuf;
        for bad in ["", "rel/state"] {
            assert_eq!(
                super::state_dir_from(Some(bad.into()), Some("/home/u".into())),
                PathBuf::from("/home/u/.local/state/croft"),
                "{bad:?}"
            );
        }
    }
}
