//! The active editor's language and whether its server supports a feature (declaration,
//! type definition, implementations, references, call hierarchy, code actions): the gates
//! on the right-click menu's rows.

use super::*;

impl App {
    /// Whether the current file's language server implements call hierarchy;
    /// drives the right-click menu rows, like the other capability gates.
    pub(in crate::app) fn editor_language_supports_call_hierarchy(&self) -> bool {
        self.editor_language_supports(crate::lsp::LspManager::language_supports_call_hierarchy)
    }

    /// The language the current editor file's name or extension maps to, if
    /// any.
    pub(in crate::app) fn editor_language(&self) -> Option<crate::lsp::Language> {
        self.editor
            .path
            .as_deref()
            .and_then(crate::lsp::Language::from_path)
    }

    /// Whether the current editor file's language server reports support
    /// through `supports`, one of the `language_supports_*` getters on
    /// [`crate::lsp::LspManager`]. Unknown (server not yet spawned) or no
    /// server counts as unsupported, so a gated row only appears once support
    /// is confirmed.
    pub(in crate::app) fn editor_language_supports(
        &self,
        supports: fn(&crate::lsp::LspManager, crate::lsp::Language) -> Option<bool>,
    ) -> bool {
        let Some(lang) = self.editor_language() else {
            return false;
        };
        self.lsp
            .as_ref()
            .and_then(|lsp| supports(lsp, lang))
            .unwrap_or(false)
    }
}

impl App {
    /// Whether the current editor file's language server implements
    /// `textDocument/declaration`. Drives whether the right-click menu shows the
    /// "Go to Declaration" row. Unknown (server not yet spawned) or no server is
    /// treated as unsupported, so the row only appears once support is confirmed.
    pub(in crate::app) fn editor_language_supports_declaration(&self) -> bool {
        self.editor_language_supports(crate::lsp::LspManager::language_supports_declaration)
    }

    /// Whether the current editor file's language server implements
    /// `textDocument/typeDefinition`. Drives whether the right-click menu shows
    /// the "Go to Type Definition" row. Unknown (server not yet spawned) or no
    /// server is treated as unsupported, so the row only appears once support is
    /// confirmed.
    pub(in crate::app) fn editor_language_supports_type_definition(&self) -> bool {
        self.editor_language_supports(crate::lsp::LspManager::language_supports_type_definition)
    }

    /// Whether the current editor file's language server implements
    /// `textDocument/implementation`. Drives whether the right-click menu shows
    /// the "Go to Implementations" row. Unknown (server not yet spawned) or no
    /// server is treated as unsupported, so the row only appears once support is
    /// confirmed.
    pub(in crate::app) fn editor_language_supports_implementation(&self) -> bool {
        self.editor_language_supports(crate::lsp::LspManager::language_supports_implementation)
    }

    /// Whether the current editor file's language server implements
    /// `textDocument/references`. Drives whether the right-click menu shows the
    /// "Go to References" row. Unknown (server not yet spawned) or no server is
    /// treated as unsupported, so the row only appears once support is confirmed.
    pub(in crate::app) fn editor_language_supports_references(&self) -> bool {
        self.editor_language_supports(crate::lsp::LspManager::language_supports_references)
    }

    pub(in crate::app) fn editor_language_supports_code_action(&self) -> bool {
        self.editor_language_supports(crate::lsp::LspManager::language_supports_code_action)
    }
}
