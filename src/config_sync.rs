//! Config sync (#262): the user's croft configuration follows them to a
//! remote over the SSH connection they already have.
//!
//! Connect to a fresh box today and you get default keybindings, no
//! snippets, no triggers — the binary follows you but the configuration
//! does not. This module names *what* may travel; [`crate::remote`] does
//! the travelling.
//!
//! # Why an allow-list, not a directory walk
//!
//! Every syncable file is named explicitly. Walking `config_dir()` and
//! excluding known-bad names would be shorter and is the wrong shape: a
//! file added later travels by default, and the failure is silent and
//! security-relevant. The same reasoning already governs
//! [`crate::config_layers::WORKSPACE_ALLOWED_KEYS`] and the remote install
//! stamp, whose deny-list ancestor once read a 98 GB `target.noindex`.
//!
//! Adding an entry here is a deliberate review decision: it means "this
//! file is safe to place on every machine the user connects to".

use std::path::PathBuf;

/// A file that may travel to a remote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Syncable {
    /// Name under `~/.config/croft/`, and the name it lands under remotely.
    pub name: &'static str,
    /// Whether croft reloads this file when it is saved THROUGH croft on the
    /// machine that owns it (`reload_config_for_path` has an arm for it).
    ///
    /// This says nothing about a file that ARRIVES by sync. Nothing watches
    /// `~/.config/croft`: the reload path is driven by the editor's own save,
    /// so a file rsynced in from elsewhere is not noticed until the remote
    /// next launches, whatever this flag says. Kept because it records a real
    /// property worth pinning, but the OUTPUT message deliberately does not
    /// use it to promise a live apply.
    pub hot_reloads: bool,
}

/// Files that travel, in the order the OUTPUT channel lists them.
///
/// `config.json` is deliberately ABSENT. It carries MCP consent
/// (`mcp_consented`), trust-on-first-use tool fingerprints
/// (`mcp_tool_fingerprints`), and `disabled_extensions` in the same
/// document as the appearance settings a user would want synced, so it
/// cannot travel as a file: consent granted on the laptop would silently
/// become consent on every box they connect to, and the push would clobber
/// consent granted on the remote. Resolving that is tracked on #262 and
/// needs either a filtered projection or the trust fields moved out.
pub const SYNCABLE: &[Syncable] = &[
    Syncable {
        name: "keybindings.json",
        hot_reloads: true,
    },
    Syncable {
        name: "snippets.json",
        hot_reloads: true,
    },
    Syncable {
        name: "triggers.json",
        hot_reloads: true,
    },
    Syncable {
        name: "matchers.json",
        hot_reloads: true,
    },
    // Has a path function but no arm in `reload_config_for_path`, so it
    // takes effect on the remote's next launch rather than on arrival.
    Syncable {
        name: "macros.json",
        hot_reloads: false,
    },
];

/// Names that must never travel, with the reason, so a future edit to
/// [`SYNCABLE`] has to argue with something rather than merely compile.
///
/// Checked by a test rather than at runtime: the allow-list is already
/// deny-by-default, and this exists to make the *intent* explicit and to
/// fail loudly if someone adds one of these to it. That makes it test-only
/// by construction, not dead code awaiting a caller.
#[cfg(test)]
pub const NEVER_SYNC: &[(&str, &str)] = &[
    (
        "config.json",
        "carries MCP consent and trust-on-first-use fingerprints; see #262",
    ),
    (
        "config.local.json",
        "the machine-local layer is machine-local by definition",
    ),
    (
        "history",
        "local history snapshots are per-machine working state",
    ),
    (
        "command_history.json",
        "command history is per-machine working state",
    ),
];

/// Local paths of the syncable files that actually exist.
///
/// A missing file is skipped rather than erroring: a user with no snippets
/// is the common case, not a fault, and pushing a zero-byte file would
/// blank whatever the remote had.
pub fn local_files() -> Vec<(Syncable, PathBuf)> {
    let dir = crate::prefs::config_dir();
    SYNCABLE
        .iter()
        .filter_map(|s| {
            let p = dir.join(s.name);
            p.is_file().then_some((*s, p))
        })
        .collect()
}

/// The rsync destination for `name` on `host`, as an rsync remote spec.
///
/// `.config/croft` rather than `$XDG_CONFIG_HOME`: rsync gets no shell on
/// the remote side to expand a variable, and the remote's own
/// `config_dir()` honours XDG. A remote that sets XDG_CONFIG_HOME would
/// read from elsewhere, which is a known gap rather than a silent one.
pub fn remote_dest(host: &str, name: &str) -> String {
    format!("{host}:.config/croft/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trust_carrying_config_never_becomes_syncable() {
        // The whole point of the module. `config.json` holds mcp_consented
        // and mcp_tool_fingerprints, so syncing it as a file would grant on
        // every remote what the user granted once locally.
        for (never, why) in NEVER_SYNC {
            assert!(
                !SYNCABLE.iter().any(|s| s.name == *never),
                "{never} must never sync: {why}"
            );
        }
    }

    #[test]
    fn every_syncable_name_is_a_bare_file_name() {
        // A name with a separator would let an entry escape the config dir
        // on either end — `../` locally, or an absolute path remotely.
        for s in SYNCABLE {
            assert!(
                !s.name.contains('/') && !s.name.contains('\\') && !s.name.contains(".."),
                "{} must be a bare file name, not a path",
                s.name
            );
            assert!(!s.name.is_empty(), "an empty name would sync the dir");
        }
    }

    #[test]
    fn the_destination_stays_inside_the_remote_config_dir() {
        assert_eq!(
            remote_dest("box", "keybindings.json"),
            "box:.config/croft/keybindings.json"
        );
        // Every syncable name lands under the config dir and nowhere else.
        for s in SYNCABLE {
            let dest = remote_dest("h", s.name);
            assert!(
                dest.starts_with("h:.config/croft/"),
                "{dest} escaped the remote config dir"
            );
        }
    }

    #[test]
    fn a_missing_file_is_skipped_rather_than_pushed_empty() {
        // `local_files` filters on `is_file`, so a user with no snippets
        // pushes nothing for it instead of blanking the remote's copy.
        let listed = local_files();
        for (_, path) in &listed {
            assert!(
                path.is_file(),
                "{} was listed but is not a file",
                path.display()
            );
        }
    }

    #[test]
    fn the_hot_reload_flag_matches_what_croft_actually_reloads_on_save() {
        // `reload_config_for_path` has arms for keybindings, snippets,
        // triggers and matchers, but none for macros. The flag records that
        // and nothing more: a SYNCED file is not reloaded either way, because
        // nothing watches the config directory.
        let hot: Vec<&str> = SYNCABLE
            .iter()
            .filter(|s| s.hot_reloads)
            .map(|s| s.name)
            .collect();
        assert_eq!(
            hot,
            vec![
                "keybindings.json",
                "snippets.json",
                "triggers.json",
                "matchers.json"
            ]
        );
        assert!(
            SYNCABLE
                .iter()
                .any(|s| s.name == "macros.json" && !s.hot_reloads),
            "macros.json has no reload arm, so it must not claim one"
        );
    }
}
