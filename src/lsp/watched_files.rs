//! Which files a server wants `workspace/didChangeWatchedFiles` for (#908).
//!
//! Servers such as ty, gopls and rust-analyzer leave file watching to the
//! client: once croft declares the capability, each registers the globs it
//! cares about through `client/registerCapability`, and croft forwards its
//! own watcher's events for the files they cover. Without it, a file changed
//! on disk outside a tab (a Rename's write, a `git checkout`, an agent in a
//! terminal) stayed stale in the server until it was opened.
//!
//! Globs follow the LSP spec: `*` stays within one path segment, `**`
//! crosses segments, and `{a,b}` and `[...]` work. A string pattern is
//! matched against the absolute path, a relative pattern against the path
//! below its base.

use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileChangeType, FileEvent, FileSystemWatcher,
    GlobPattern, OneOf, Registration, Unregistration, Url, WatchKind,
};

const METHOD: &str = "workspace/didChangeWatchedFiles";

/// One compiled watcher.
#[derive(Debug)]
struct Watcher {
    /// A relative pattern's base; `None` for a string pattern.
    base: Option<PathBuf>,
    glob: GlobMatcher,
    kind: WatchKind,
}

impl Watcher {
    /// `None` for a pattern that does not compile or a base that is not a
    /// local folder, rather than a watcher that matches everything.
    fn compile(w: &FileSystemWatcher) -> Option<Self> {
        let (base, pattern) = match &w.glob_pattern {
            GlobPattern::String(p) => (None, p),
            GlobPattern::Relative(r) => {
                let uri = match &r.base_uri {
                    OneOf::Left(folder) => &folder.uri,
                    OneOf::Right(uri) => uri,
                };
                (Some(uri.to_file_path().ok()?), &r.pattern)
            }
        };
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .ok()?
            .compile_matcher();
        let all = WatchKind::Create | WatchKind::Change | WatchKind::Delete;
        Some(Self {
            base,
            glob,
            kind: w.kind.unwrap_or(all),
        })
    }

    fn covers(&self, path: &Path, typ: FileChangeType) -> bool {
        let wanted = match typ {
            FileChangeType::CREATED => WatchKind::Create,
            FileChangeType::DELETED => WatchKind::Delete,
            _ => WatchKind::Change,
        };
        if !self.kind.contains(wanted) {
            return false;
        }
        match &self.base {
            Some(base) => path
                .strip_prefix(base)
                .is_ok_and(|rel| self.glob.is_match(rel)),
            None => self.glob.is_match(path),
        }
    }
}

/// A server's registered watchers, by registration id.
#[derive(Debug, Default)]
pub struct WatchedFiles {
    registrations: Vec<(String, Vec<Watcher>)>,
}

impl WatchedFiles {
    /// Store the `workspace/didChangeWatchedFiles` registrations among
    /// `registrations`. False when there were none, so the caller declines a
    /// registration croft does not support, as it always has.
    pub fn register(&mut self, registrations: &[Registration]) -> bool {
        let mut stored = false;
        for reg in registrations.iter().filter(|r| r.method == METHOD) {
            let Some(options) = reg.register_options.clone().and_then(|o| {
                serde_json::from_value::<DidChangeWatchedFilesRegistrationOptions>(o).ok()
            }) else {
                continue;
            };
            let watchers = options.watchers.iter().filter_map(Watcher::compile);
            self.registrations.retain(|(id, _)| *id != reg.id);
            self.registrations
                .push((reg.id.clone(), watchers.collect()));
            stored = true;
        }
        stored
    }

    pub fn unregister(&mut self, unregistrations: &[Unregistration]) {
        self.registrations.retain(|(id, _)| {
            !unregistrations
                .iter()
                .any(|u| u.method == METHOD && u.id == *id)
        });
    }

    /// The events among `changes` that some watcher covers.
    pub fn events_for(&self, changes: &[(PathBuf, FileChangeType)]) -> Vec<FileEvent> {
        changes
            .iter()
            .filter(|(path, typ)| {
                self.registrations
                    .iter()
                    .flat_map(|(_, watchers)| watchers)
                    .any(|w| w.covers(path, *typ))
            })
            .filter_map(|(path, typ)| Some(FileEvent::new(Url::from_file_path(path).ok()?, *typ)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registration(id: &str, watchers: serde_json::Value) -> Registration {
        let options = json!({ "watchers": watchers });
        serde_json::from_value(json!({ "id": id, "method": METHOD, "registerOptions": options }))
            .expect("registration")
    }

    /// The paths among `changes` that `w` reports.
    fn reported(w: &WatchedFiles, changes: &[(&str, FileChangeType)]) -> Vec<String> {
        let changes: Vec<_> = changes
            .iter()
            .map(|(p, t)| (PathBuf::from(p), *t))
            .collect();
        let events = w.events_for(&changes).into_iter();
        events.map(|e| e.uri.path().to_string()).collect()
    }

    #[test]
    fn watchers_match_by_glob_base_and_kind() {
        let (created, changed, deleted) = (
            FileChangeType::CREATED,
            FileChangeType::CHANGED,
            FileChangeType::DELETED,
        );
        let mut w = WatchedFiles::default();
        assert!(w.register(&[registration(
            "a",
            json!([
                { "globPattern": "**/*.py" },
                { "globPattern": { "baseUri": "file:///p/a", "pattern": "**/*.rs" } },
                // 5 = Create | Delete: no Change events.
                { "globPattern": "**/*.go", "kind": 5 },
            ])
        )]));
        let got = reported(
            &w,
            &[
                ("/p/pkg/lib.py", changed),
                ("/p/notes.md", changed),
                ("/p/a/src/lib.rs", created),
                ("/p/b/src/lib.rs", created),
                ("/p/new.go", created),
                ("/p/edited.go", changed),
                ("/p/gone.go", deleted),
            ],
        );
        assert_eq!(
            got,
            [
                "/p/pkg/lib.py",
                "/p/a/src/lib.rs",
                "/p/new.go",
                "/p/gone.go"
            ]
        );
    }

    #[test]
    fn other_registrations_are_declined_and_unregister_drops_its_own() {
        let mut w = WatchedFiles::default();
        let mut other = registration("s", json!([]));
        other.method = String::from("textDocument/didSave");
        assert!(
            !w.register(&[other]),
            "only watched files can be registered"
        );
        assert!(w.register(&[
            registration("a", json!([{ "globPattern": "**/*.py" }])),
            registration("b", json!([{ "globPattern": "**/*.toml" }])),
        ]));
        let a = Unregistration {
            id: String::from("a"),
            method: METHOD.to_string(),
        };
        w.unregister(&[a]);
        let changed = FileChangeType::CHANGED;
        let got = reported(&w, &[("/p/a.py", changed), ("/p/pyproject.toml", changed)]);
        assert_eq!(got, ["/p/pyproject.toml"]);
    }
}
