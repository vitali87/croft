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
pub(crate) fn config_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("croft");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("croft")
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
