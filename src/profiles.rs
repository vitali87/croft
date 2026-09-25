//! Profiles (#618): whole configurations to switch between.
//!
//! A profile is a directory holding its own copy of the files that shape how
//! croft behaves: settings, keybindings and snippets. History, macros,
//! sessions and the rest are about the user, not the setup, and stay shared.
//!
//! One profile is active at a time. A workspace can name its own in
//! `.croft/profile` (one line, the name), which wins over the global choice
//! kept in `<config>/profile`; with neither, croft reads its files from the
//! config directory exactly as it always has. The three path helpers
//! (`prefs::config_path`, `keymap::keybindings_path`,
//! `snippets::snippets_path`) resolve through [`file`], so every reader and
//! writer of those files follows the active profile without knowing about it.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// The files a profile owns.
pub const PROFILED_FILES: &[&str] = &["config.json", "keybindings.json", "snippets.json"];

/// The active profile's name, `None` for the default (no profile).
static ACTIVE: RwLock<Option<String>> = RwLock::new(None);

/// Where profiles live.
pub fn profiles_dir() -> PathBuf {
    crate::prefs::config_dir().join("profiles")
}

/// The global choice's file.
fn global_choice_path() -> PathBuf {
    crate::prefs::config_dir().join("profile")
}

/// A workspace's own choice's file.
pub fn workspace_choice_path(root: &Path) -> PathBuf {
    root.join(".croft").join("profile")
}

/// The path of profiled file `name` under the active profile, or under the
/// config directory when none is active.
pub fn file(name: &str) -> PathBuf {
    match active() {
        Some(profile) => profiles_dir().join(profile).join(name),
        None => crate::prefs::config_dir().join(name),
    }
}

pub fn active() -> Option<String> {
    ACTIVE.read().ok().and_then(|a| a.clone())
}

fn set_active(name: Option<String>) {
    if let Ok(mut a) = ACTIVE.write() {
        *a = name;
    }
}

/// A profile name croft will use as a directory: non-empty, no path
/// separators or dots at the front, nothing a shell would trip on.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || " -_".contains(c))
}

/// Every existing profile, sorted.
pub fn list() -> Vec<String> {
    list_in(&profiles_dir())
}

fn list_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_name(n))
        .collect();
    names.sort();
    names
}

/// Read the first line of a choice file as a profile name that exists.
fn read_choice(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let name = text.lines().next()?.trim().to_string();
    (valid_name(&name) && profiles_dir().join(&name).is_dir()).then_some(name)
}

/// Decide the active profile for a workspace at startup: its own choice,
/// else the global one, else none. Returns what it chose.
pub fn activate_for_workspace(root: &Path) -> Option<String> {
    let chosen =
        read_choice(&workspace_choice_path(root)).or_else(|| read_choice(&global_choice_path()));
    set_active(chosen.clone());
    chosen
}

/// Switch to `name` (or the default with `None`), remembering the choice
/// globally.
pub fn switch(name: Option<&str>) -> std::io::Result<()> {
    let path = global_choice_path();
    match name {
        Some(n) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, format!("{n}\n"))?;
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
    set_active(name.map(str::to_string));
    Ok(())
}

/// Create profile `name` as a copy of the files now in effect, so a new
/// profile starts from the current setup rather than from nothing.
pub fn create_from_current(name: &str) -> std::io::Result<()> {
    create_in(&profiles_dir(), name, file)
}

/// [`create_from_current`] against an explicit profiles directory, with
/// `current` naming where each file now in effect lives.
fn create_in(
    profiles: &Path,
    name: &str,
    current: impl Fn(&str) -> PathBuf,
) -> std::io::Result<()> {
    if !valid_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a profile name uses letters, digits, spaces, - and _",
        ));
    }
    let dir = profiles.join(name);
    if dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("profile {name} already exists"),
        ));
    }
    std::fs::create_dir_all(&dir)?;
    for f in PROFILED_FILES {
        let from = current(f);
        if from.is_file() {
            std::fs::copy(&from, dir.join(f))?;
        }
    }
    Ok(())
}

/// Make `name` this workspace's profile (or clear its choice with `None`).
pub fn set_workspace_default(root: &Path, name: Option<&str>) -> std::io::Result<()> {
    let path = workspace_choice_path(root);
    match name {
        Some(n) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, format!("{n}\n"))
        }
        None => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_could_escape_the_profiles_dir_are_refused() {
        assert!(valid_name("Python"));
        assert!(valid_name("writing mode_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("../etc"));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("a/b"));
    }

    #[test]
    fn a_new_profile_copies_the_files_in_effect_and_is_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let current = tmp.path().join("current");
        std::fs::create_dir(&current).unwrap();
        std::fs::write(current.join("keybindings.json"), "[]").unwrap();
        let profiles = tmp.path().join("profiles");
        create_in(&profiles, "Python", |f| current.join(f)).unwrap();
        assert_eq!(
            std::fs::read_to_string(profiles.join("Python").join("keybindings.json")).unwrap(),
            "[]"
        );
        assert!(
            !profiles.join("Python").join("config.json").exists(),
            "only what exists is copied"
        );
        assert!(
            create_in(&profiles, "Python", |f| current.join(f)).is_err(),
            "no overwrite"
        );
        assert!(create_in(&profiles, "../x", |f| current.join(f)).is_err());
        assert_eq!(list_in(&profiles), vec![String::from("Python")]);
    }

    #[test]
    fn a_workspace_default_is_written_and_cleared() {
        let tmp = tempfile::tempdir().unwrap();
        set_workspace_default(tmp.path(), Some("Python")).unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace_choice_path(tmp.path())).unwrap(),
            "Python\n"
        );
        set_workspace_default(tmp.path(), None).unwrap();
        assert!(!workspace_choice_path(tmp.path()).exists());
        set_workspace_default(tmp.path(), None).unwrap();
    }

    #[test]
    fn with_no_profile_the_files_live_in_the_config_dir() {
        // The default must be exactly croft's old layout: no profile, no
        // change to where anything is read or written.
        if active().is_none() {
            assert_eq!(
                file("config.json"),
                crate::prefs::config_dir().join("config.json")
            );
        }
    }
}
