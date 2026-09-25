//! Profiles (#618): named sets of settings and keybindings under
//! `~/.config/croft/profiles/<name>/`, switched from the palette and
//! optionally picked per workspace.
//!
//! A profile's `config.json` is a settings layer that sits over the user's
//! own ([`crate::config_layers`] places it); its `keybindings.json`, when it
//! has one, is used instead of the user's. The active profile is the
//! `profile` setting, which a workspace may set as its default.

use std::path::{Path, PathBuf};

/// Whether `name` can name a profile: 1-64 letters, digits, spaces, `-`,
/// `_` or `.`, not starting with `.` or a space. Anything else (a path
/// separator above all) is refused, because a workspace can set the name.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && !name.starts_with(['.', ' '])
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

/// The directory holding profile `name`.
pub fn dir(config_dir: &Path, name: &str) -> PathBuf {
    config_dir.join("profiles").join(name)
}

/// The profiles that exist, sorted: every valid-named directory under
/// `profiles/`.
pub fn list(config_dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(config_dir.join("profiles")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_name(n))
        .collect();
    out.sort();
    out
}

/// Create profile `name` with an empty settings file. An error for an
/// invalid name or one that exists.
pub fn create(config_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!(
            "\"{name}\" is not a profile name: use letters, digits, spaces, - _ ."
        ));
    }
    let d = dir(config_dir, name);
    if d.exists() {
        return Err(format!("A profile named \"{name}\" already exists"));
    }
    std::fs::create_dir_all(&d)
        .and_then(|()| std::fs::write(d.join("config.json"), "{}\n"))
        .map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

/// The keybindings file in force: the active profile's when it has one,
/// else the user's.
pub fn keybindings_file(config_dir: &Path, profile: &str) -> PathBuf {
    if valid_name(profile) {
        let own = dir(config_dir, profile).join("keybindings.json");
        if own.is_file() {
            return own;
        }
    }
    config_dir.join("keybindings.json")
}

/// A settings file's `text` (a JSON object, or empty) with `profile` set
/// to `name`, or removed when `name` is empty.
pub fn set_profile_in(text: &str, name: &str) -> Result<String, String> {
    let mut map = if text.trim().is_empty() {
        serde_json::Map::new()
    } else {
        match serde_json::from_str::<serde_json::Value>(&crate::tasks::strip_jsonc(text)) {
            Ok(serde_json::Value::Object(m)) => m,
            Ok(_) => return Err(String::from("the settings file is not a JSON object")),
            Err(e) => return Err(format!("the settings file does not parse: {e}")),
        }
    };
    if name.is_empty() {
        map.remove("profile");
    } else {
        map.insert(String::from("profile"), serde_json::Value::from(name));
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(map))
        .map(|s| s + "\n")
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_name_is_a_plain_directory_name() {
        for ok in [
            "Python",
            "writing",
            "rust-dev",
            "my_profile.v2",
            "Deep Work",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            " lead",
            ".hidden",
            "..",
            "a/b",
            "a\\b",
            "../x",
            &"x".repeat(65),
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn profiles_live_under_the_config_dir_and_are_listed_sorted() {
        let cfg = tempfile::tempdir().unwrap();
        assert!(list(cfg.path()).is_empty(), "none yet, and no folder");
        let python = create(cfg.path(), "Python").unwrap();
        assert_eq!(python, cfg.path().join("profiles/Python"));
        assert_eq!(
            std::fs::read_to_string(python.join("config.json")).unwrap(),
            "{}\n"
        );
        create(cfg.path(), "Writing").unwrap();
        std::fs::create_dir_all(cfg.path().join("profiles/.cache")).unwrap();
        std::fs::write(cfg.path().join("profiles/stray.json"), "{}").unwrap();
        assert_eq!(list(cfg.path()), ["Python", "Writing"]);
        assert!(create(cfg.path(), "Python").is_err(), "exists");
        assert!(create(cfg.path(), "../evil").is_err(), "invalid");
        assert_eq!(
            dir(cfg.path(), "Writing"),
            cfg.path().join("profiles/Writing")
        );
    }

    #[test]
    fn a_profiles_own_keybindings_replace_the_users() {
        let cfg = tempfile::tempdir().unwrap();
        create(cfg.path(), "Python").unwrap();
        let user = cfg.path().join("keybindings.json");
        assert_eq!(keybindings_file(cfg.path(), ""), user);
        assert_eq!(
            keybindings_file(cfg.path(), "Python"),
            user,
            "no file of its own"
        );
        std::fs::write(cfg.path().join("profiles/Python/keybindings.json"), "[]").unwrap();
        assert_eq!(
            keybindings_file(cfg.path(), "Python"),
            cfg.path().join("profiles/Python/keybindings.json")
        );
        assert_eq!(
            keybindings_file(cfg.path(), "../x"),
            user,
            "an invalid name is ignored"
        );
    }

    #[test]
    fn the_profile_key_is_set_or_removed_and_other_keys_stay() {
        let out = set_profile_in("{\"theme\": \"dark\"}", "Python").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"theme": "dark", "profile": "Python"}));
        let out = set_profile_in(&out, "").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!({"theme": "dark"}));
        let v: serde_json::Value =
            serde_json::from_str(&set_profile_in("", "Python").unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"profile": "Python"}));
        assert!(set_profile_in("[1]", "Python").is_err());
    }
}
