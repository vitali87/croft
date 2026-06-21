//! Data-driven debug-adapter registry.
//!
//! Mirrors `lsp::registry`: which debugger handles a given file type is read
//! from `[[debug_adapters]]` blocks in the bundled + user extension manifests,
//! not a hardcoded match. An adapter whose contributing extension is disabled
//! in the Extensions panel is skipped, so F5 then reports no debugger for that
//! file type — the same "disabled = opt-out" toggle the viewers and LSP servers
//! use.
//!
//! The launch *mechanisms* themselves (debugpy / lldb-dap / vscode-js-debug)
//! are heterogeneous and stay native in `session.rs` + the app; the manifest's
//! `kind` field only selects which of those croft drives. So a third party can
//! retarget the existing mechanisms onto new file types from a manifest, but
//! cannot introduce a brand-new launch flow without Rust — exactly the boundary
//! the PDF/CSV viewers draw.

use std::collections::BTreeSet;

use crate::dap::session::AdapterKind;
use crate::lsp::manifest::{self, AdapterKindDecl};

/// Map a manifest's declared adapter kind onto the launch mechanism croft drives.
fn kind_of(decl: AdapterKindDecl) -> AdapterKind {
    match decl {
        AdapterKindDecl::Debugpy => AdapterKind::Debugpy,
        AdapterKindDecl::Lldb => AdapterKind::LldbDap,
        AdapterKindDecl::JsDebug => AdapterKind::JsDebug,
    }
}

/// One adapter that claims `ext`: its launch mechanism and the id of the
/// extension that contributes it (the key the panel toggle / disabled set uses).
struct AdapterMatch {
    kind: AdapterKind,
    extension_id: String,
}

/// Every adapter across `sources` that lists `ext`, in source order (bundled
/// before user). Pure: no prefs, no filesystem.
fn matches(sources: &[String], ext: &str) -> Vec<AdapterMatch> {
    sources
        .iter()
        .filter_map(|s| manifest::parse(s).ok())
        .flat_map(|m| {
            let id = m.id;
            m.debug_adapters.into_iter().map(move |d| (id.clone(), d))
        })
        .filter(|(_, d)| d.extensions.iter().any(|e| e == ext))
        .map(|(extension_id, d)| AdapterMatch {
            kind: kind_of(d.kind),
            extension_id,
        })
        .collect()
}

/// The adapter for `ext` from `sources`, skipping any whose extension id is in
/// `disabled`. First enabled match wins (bundled order). Pure (testable without
/// touching real prefs).
fn resolve(sources: &[String], disabled: &BTreeSet<String>, ext: &str) -> Option<AdapterKind> {
    matches(sources, ext)
        .into_iter()
        .find(|m| !disabled.contains(&m.extension_id))
        .map(|m| m.kind)
}

/// Bundled + user manifest sources, in load order.
fn all_sources() -> Vec<String> {
    let mut sources: Vec<String> = manifest::BUNDLED_MANIFESTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    sources.extend(manifest::read_extension_sources(
        &manifest::user_extensions_dir(),
    ));
    sources
}

/// The debug adapter for a file extension (lowercased, dot-less) from the
/// *enabled* extensions, or `None` when no enabled adapter claims it. Reads
/// prefs + manifests fresh on each call — launching a debugger is a rare user
/// action, never a hot path, so a panel toggle takes effect on the next F5.
pub fn adapter_for_extension(ext: &str) -> Option<AdapterKind> {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    resolve(&all_sources(), &disabled, ext)
}

/// When no *enabled* adapter claims `ext` but a *disabled* one would have, the
/// disabled extension's id — so the launch path can say "enable it in
/// Extensions" instead of the misleading "no debugger for this file type".
/// `None` when an enabled adapter exists or nothing claims the extension.
pub fn disabled_extension_for(ext: &str) -> Option<String> {
    let disabled = crate::prefs::Prefs::load_or_default().disabled_extensions;
    let sources = all_sources();
    let claims = matches(&sources, ext);
    if claims.iter().any(|m| !disabled.contains(&m.extension_id)) {
        return None;
    }
    claims
        .into_iter()
        .map(|m| m.extension_id)
        .find(|id| disabled.contains(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled() -> Vec<String> {
        manifest::BUNDLED_MANIFESTS
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn bundled_manifests_map_file_extensions_to_adapters() {
        let s = bundled();
        let none = BTreeSet::new();
        for ext in ["py", "pyw"] {
            assert_eq!(resolve(&s, &none, ext), Some(AdapterKind::Debugpy), "{ext}");
        }
        for ext in ["rs", "c", "h", "cc", "cpp", "cxx", "hpp", "hxx", "m", "mm"] {
            assert_eq!(resolve(&s, &none, ext), Some(AdapterKind::LldbDap), "{ext}");
        }
        for ext in ["js", "mjs", "cjs", "jsx", "ts", "tsx", "mts", "cts"] {
            assert_eq!(resolve(&s, &none, ext), Some(AdapterKind::JsDebug), "{ext}");
        }
        assert_eq!(resolve(&s, &none, "txt"), None);
        assert_eq!(resolve(&s, &none, ""), None);
    }

    #[test]
    fn a_disabled_adapter_extension_stops_claiming_its_files() {
        let s = bundled();
        let mut disabled = BTreeSet::new();
        disabled.insert("dap-python".to_string());
        // Python no longer resolves...
        assert_eq!(resolve(&s, &disabled, "py"), None);
        // ...but the other adapters are unaffected.
        assert_eq!(resolve(&s, &disabled, "rs"), Some(AdapterKind::LldbDap));
        assert_eq!(resolve(&s, &disabled, "ts"), Some(AdapterKind::JsDebug));
    }

    #[test]
    fn a_user_manifest_can_retarget_a_built_in_mechanism_to_a_new_extension() {
        let mut s = bundled();
        // A third-party manifest pointing the debugpy mechanism at `.bzl`.
        s.push(
            r#"
id = "dap-starlark"
name = "Starlark Debugger"
api_version = 1
[[debug_adapters]]
id = "dap-starlark"
label = "debugpy (Starlark)"
kind = "debugpy"
extensions = ["bzl"]
"#
            .to_string(),
        );
        let none = BTreeSet::new();
        assert_eq!(resolve(&s, &none, "bzl"), Some(AdapterKind::Debugpy));
    }
}
