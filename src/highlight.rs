use ratatui::style::{Color, Modifier, Style};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Order matters: the index assigned by `HighlightConfiguration::configure`
/// is the same as the index into this slice and into `HIGHLIGHT_STYLES`.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "module",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// JSX-specific captures appended onto the shared TypeScript/TSX
/// highlights query. The bundled query has none of these, so JSX
/// tags and attribute names land in default foreground without it.
const TSX_JSX_OVERLAY_QUERY: &str = r#"
(jsx_opening_element name: (_) @tag)
(jsx_closing_element name: (_) @tag)
(jsx_self_closing_element name: (_) @tag)
(jsx_attribute (property_identifier) @attribute)
"#;

/// Parameter captures appended onto the bundled Python highlights
/// query. tree-sitter-python emits no @variable.parameter, so every
/// function and lambda parameter falls through to the broad
/// (identifier) @variable rule and renders in default foreground.
/// Appended LAST so these win over that rule under
/// tree-sitter-highlight's last-match-wins rule. Covers plain,
/// typed, defaulted, and *args / **kwargs forms.
const PYTHON_PARAM_OVERLAY_QUERY: &str = r#"
(parameters (identifier) @variable.parameter)
(lambda_parameters (identifier) @variable.parameter)
(typed_parameter (identifier) @variable.parameter)
(default_parameter name: (identifier) @variable.parameter)
(typed_default_parameter name: (identifier) @variable.parameter)
(parameters (list_splat_pattern (identifier) @variable.parameter))
(parameters (dictionary_splat_pattern (identifier) @variable.parameter))
(lambda_parameters (list_splat_pattern (identifier) @variable.parameter))
(lambda_parameters (dictionary_splat_pattern (identifier) @variable.parameter))
(typed_parameter (list_splat_pattern (identifier) @variable.parameter))
(typed_parameter (dictionary_splat_pattern (identifier) @variable.parameter))
"#;

/// Namespace + class heuristics appended LAST onto the bundled Python
/// highlights query, so they win under tree-sitter-highlight's
/// last-match-wins rule. This is how Helix/Zed/Neovim colour Python without
/// the language server: the two biggest cold-open mismatches against ty are
/// imported class references and module names, and both are recoverable from
/// syntax alone.
///
/// 1. Module names in `import` / `from ... import` statements are coloured
///    `@module` (yellow), matching ty's `namespace`. The bundled query leaves
///    them as plain `@variable` (grey), which is the most visible part of the
///    grey-to-yellow snap on an import-heavy file.
/// 2. CamelCase identifiers (a capital followed somewhere by a lowercase) are
///    coloured `@type` (yellow), matching ty's `class`. The bundled query
///    already singles capitalised names out, but as `@constructor` (blue),
///    which disagrees with ty's yellow. Restricting to names that contain a
///    lowercase letter deliberately excludes ALL_CAPS names, which the bundled
///    `@constant` rule keeps orange (ty calls those plain variables, but
///    highlighting module-level constants is a deliberate choice and they are
///    few). PEP 8 makes CamelCase an almost perfect proxy for "class".
const PYTHON_NAMESPACE_TYPE_OVERLAY_QUERY: &str = r#"
(import_statement name: (dotted_name (identifier) @module))
(import_statement name: (aliased_import (dotted_name (identifier) @module)))
(import_from_statement module_name: (dotted_name (identifier) @module))
(import_from_statement module_name: (relative_import (dotted_name (identifier) @module)))

((identifier) @type
 (#match? @type "^[A-Z].*[a-z]"))
"#;

/// Local-scope query for Python, passed as the locals slot of
/// `HighlightConfiguration::new`. tree-sitter-python ships none, so without
/// this a parameter is coloured only at its `def` site (by
/// `PYTHON_PARAM_OVERLAY_QUERY`); its references in the body fall through to
/// `(identifier) @variable` (grey) and stay grey until ty's semantic-token
/// overlay lands, which on a cold, heavy-import file is 1-2s after open. The
/// locals query lets tree-sitter link each body reference to its in-scope
/// definition and inherit the definition's colour instantly, the same way
/// Helix and Zed colour locals without waiting on the language server.
///
/// The Rust `tree-sitter-highlight` crate matches the BARE capture names
/// `local.scope` / `local.definition` / `local.reference` exactly (it does not
/// understand nvim-treesitter's dotted `@local.definition.parameter`
/// subtypes), so every capture below uses the bare form. Definition patterns
/// are listed before the broad `(identifier) @local.reference` so a node that
/// is both a definition and an identifier is recorded as a definition first
/// and not re-resolved as a reference.
const PYTHON_LOCALS_QUERY: &str = r#"
[
  (module)
  (function_definition)
  (lambda)
  (class_definition)
  (list_comprehension)
  (set_comprehension)
  (dictionary_comprehension)
  (generator_expression)
] @local.scope

(parameters (identifier) @local.definition)
(lambda_parameters (identifier) @local.definition)
(typed_parameter (identifier) @local.definition)
(default_parameter name: (identifier) @local.definition)
(typed_default_parameter name: (identifier) @local.definition)
(parameters (list_splat_pattern (identifier) @local.definition))
(parameters (dictionary_splat_pattern (identifier) @local.definition))
(lambda_parameters (list_splat_pattern (identifier) @local.definition))
(lambda_parameters (dictionary_splat_pattern (identifier) @local.definition))

(assignment left: (identifier) @local.definition)
(assignment left: (pattern_list (identifier) @local.definition))
(assignment left: (tuple_pattern (identifier) @local.definition))
(for_statement left: (identifier) @local.definition)
(for_statement left: (pattern_list (identifier) @local.definition))
(for_statement left: (tuple_pattern (identifier) @local.definition))
(for_in_clause left: (identifier) @local.definition)
(for_in_clause left: (pattern_list (identifier) @local.definition))
(for_in_clause left: (tuple_pattern (identifier) @local.definition))
(named_expression (identifier) @local.definition)
(as_pattern alias: (as_pattern_target) @local.definition)

; Imported module names bind a name in scope, so body references to them
; (`pydantic` in `pydantic.BaseModel`) resolve here and inherit the def's
; `@module` colour instantly, the same way parameters do. The `.` anchor binds
; only the first segment of a dotted import (`import a.b.c` binds `a`); an
; aliased import binds its alias.
(import_statement name: (dotted_name . (identifier) @local.definition))
(import_statement name: (aliased_import alias: (identifier) @local.definition))

(identifier) @local.reference
"#;

/// Locals overlay appended to the bundled `tree-sitter-typescript`
/// locals query (used for both TypeScript and TSX). The bundled query
/// captures `required_parameter` / `optional_parameter` as
/// `@local.definition` but ships NO `@local.scope` and NO
/// `@local.reference`, so without this overlay a parameter is coloured
/// only at its declaration and its body references stay default-coloured
/// until the language server's semantic-token overlay arrives. Adds the
/// missing scopes, single-identifier arrow parameters, `const`/`let`/`var`
/// declarators, and the broad reference capture so tree-sitter links body
/// references to their in-scope definition and colours them instantly.
/// Bundled definition patterns precede this overlay's `@local.reference`,
/// so a definition node is recorded as a definition, not re-resolved as a
/// reference. Bare capture names only (the Rust highlighter matches them
/// exactly).
const TS_LOCALS_OVERLAY_QUERY: &str = r#"
[
  (statement_block)
  (function_declaration)
  (function_expression)
  (arrow_function)
  (method_definition)
] @local.scope

(arrow_function parameter: (identifier) @local.definition)
(variable_declarator name: (identifier) @local.definition)

(identifier) @local.reference
"#;

/// Locals overlay PREPENDED to the bundled `tree-sitter-javascript`
/// locals query. That bundled query already has scopes, `variable_declarator`
/// definitions, and a broad `(identifier) @local.reference`, but it captures
/// only `(pattern/identifier)` as a definition, which a plain
/// `(formal_parameters (identifier))` parameter is NOT, so function
/// parameters were never recorded and their body references stayed
/// default-coloured until the language server replied. Prepending these
/// parameter-definition patterns (so they precede the bundled reference
/// capture) records parameters as definitions, letting body references
/// inherit the parameter colour instantly. Bare capture names only.
const JS_PARAM_LOCALS_OVERLAY_QUERY: &str = r#"
(formal_parameters (identifier) @local.definition)
(arrow_function parameter: (identifier) @local.definition)
(rest_pattern (identifier) @local.definition)
"#;

/// Highlights overlay appended to the bundled `tree-sitter-javascript`
/// highlights query. That query captures no `@variable.parameter` at all
/// (function parameters fall through to the broad `(identifier) @variable`
/// and render in default foreground), so unlike Python and TypeScript a JS
/// parameter was not even coloured at its declaration. Appended LAST so it
/// wins over the broad `@variable` rule under tree-sitter-highlight's
/// last-match-wins rule, matching the Python parameter overlay and giving
/// the locals query a parameter colour to propagate to body references.
const JS_PARAM_HIGHLIGHTS_OVERLAY_QUERY: &str = r#"
(formal_parameters (identifier) @variable.parameter)
(arrow_function parameter: (identifier) @variable.parameter)
(rest_pattern (identifier) @variable.parameter)
"#;

/// Broad identifier capture PREPENDED onto the bundled Rust highlights query.
/// tree-sitter-rust's highlights query has no catch-all `(identifier)` rule
/// (only specific ones: `@function` calls, `@constant`/`@constructor` by
/// regex, `@variable.parameter` at declarations, ...), so a parameter's body
/// reference matches no highlight capture at all. That breaks the locals
/// machinery: `tree-sitter-highlight` resolves the reference to its
/// definition's colour but only EMITS that colour when the reference node ALSO
/// carries a highlights capture (otherwise it skips emission entirely). This
/// catch-all gives every identifier a carrier capture. Prepended FIRST so the
/// bundled specific rules follow and win under last-match-wins; only
/// otherwise-unmatched identifiers take `@variable`, whose grey equals the
/// previous default foreground, so non-parameter identifiers look unchanged.
/// Mirrors `JS_PARAM_HIGHLIGHTS_OVERLAY_QUERY`.
const RUST_IDENTIFIER_OVERLAY_QUERY: &str = r#"(identifier) @variable"#;

/// Appended AFTER the bundled Rust highlights so it wins under last-match-wins.
/// Colours the two most pervasive tokens that otherwise recolour when
/// rust-analyzer's semantic overlay lands 1-3s after open (RA cold crate-graph
/// analysis, unfixable at the LSP layer):
///   - module/namespace path segments (`std::collections::`) which RA paints
///     `namespace` (yellow) but tree-sitter leaves grey @variable. Restricted
///     to lowercase leading segments via `#match?` so capitalized type paths
///     keep the bundled `@type` rule. Mirrors the bundled capitalized-`@type`
///     scoped rules but for lowercase -> `@module`.
///   - enum-variant DECLARATIONS (`enum E { A, B }`) which RA paints
///     `enumMember` (orange) but tree-sitter colours @constructor (blue-grey).
/// Variant USES (`E::A`) are deliberately NOT matched: syntactically they are
/// indistinguishable from a type path or associated const, so colouring them
/// would mis-paint real types. That residual recolour is inherent to the
/// combined model (VS Code has it too) and only the LSP can resolve it.
const RUST_MODULE_OVERLAY_QUERY: &str = r#"
((scoped_identifier path: (identifier) @module)
 (#match? @module "^[a-z]"))
((scoped_identifier path: (scoped_identifier name: (identifier) @module))
 (#match? @module "^[a-z]"))
((scoped_type_identifier path: (identifier) @module)
 (#match? @module "^[a-z]"))
((scoped_type_identifier path: (scoped_identifier name: (identifier) @module))
 (#match? @module "^[a-z]"))
(enum_variant name: (identifier) @constant)
"#;

/// Appended AFTER `RUST_MODULE_OVERLAY_QUERY` so it wins under last-match-wins.
/// Colours `use` declarations the way rust-analyzer resolves them, killing the
/// most visible cold-open snap: the whole import block at the top of a file,
/// which the bundled query paints blue/grey and RA repaints yellow ~1-3s later
/// once crate-graph analysis lands.
///
/// This is the Rust counterpart of Python's `PYTHON_NAMESPACE_TYPE_OVERLAY_QUERY`,
/// but restricted to `use` position rather than a blanket CamelCase rule. In
/// Rust a blanket `(identifier) @type (#match? "^[A-Z]...")` would collide with
/// enum variants: `Some` / `None` / `Ok` / `Err` and custom variants are
/// CamelCase bare identifiers too, yet RA paints them `enumMember` (orange), not
/// type-yellow, and syntax cannot tell a variant from a type in expression
/// position. Inside a `use` path there is no such ambiguity: `Some`/`None`/etc.
/// are prelude items never written in `use`, so every capitalized import name is
/// a type/trait/struct (yellow) and every lowercase path segment is a module
/// (yellow `@module`). Empirically (token diff against RA's cached tokens for
/// the real src/remote.rs) imports are the single biggest type-coloured snap.
///
///   - Imported type/trait/struct names -> `@type`. `^[A-Z].*[a-z]` (a capital
///     plus a lowercase) excludes ALL_CAPS const imports, which RA paints as
///     `const` (orange) and the bundled `@constant` rule already handles.
///   - Brace-list module heads (`anyhow` in `anyhow::{...}`, `thread` in
///     `std::thread::{...}`) which the `scoped_identifier` module overlay does
///     not reach because a `scoped_use_list` path is not a `scoped_identifier`.
const RUST_USE_OVERLAY_QUERY: &str = r#"
((use_declaration argument: (scoped_identifier name: (identifier) @type))
 (#match? @type "^[A-Z].*[a-z]"))
((use_list (identifier) @type)
 (#match? @type "^[A-Z].*[a-z]"))
((use_as_clause alias: (identifier) @type)
 (#match? @type "^[A-Z].*[a-z]"))

((scoped_use_list path: (identifier) @module)
 (#match? @module "^[a-z]"))
((scoped_use_list path: (scoped_identifier name: (identifier) @module))
 (#match? @module "^[a-z]"))
"#;

/// Local-scope query for Rust, passed as the locals slot of
/// `HighlightConfiguration::new`. tree-sitter-rust ships no locals query, so
/// without this a parameter is coloured only at its declaration site (by the
/// bundled `(parameter (identifier) @variable.parameter)` rule); its
/// references in the body match no highlights capture (the Rust query has no
/// broad `(identifier) @variable` rule) and fall through to default grey until
/// rust-analyzer's semantic-token overlay lands, which on a cold crate is
/// ~25s after open. The locals query lets tree-sitter link each body
/// reference to its in-scope definition and inherit the definition's colour
/// instantly, the same way Helix and Zed colour locals without waiting on the
/// language server.
///
/// The Rust `tree-sitter-highlight` crate matches the BARE capture names
/// `local.scope` / `local.definition` / `local.reference` exactly. Definition
/// patterns are listed before the broad `(identifier) @local.reference` so a
/// node that is both a definition and an identifier is recorded as a
/// definition first and not re-resolved as a reference. A body reference only
/// inherits a colour when its definition node carries a highlights-query
/// capture: parameters do (`@variable.parameter`, orange), so their references
/// turn orange; plain `let` locals do not, so they stay default grey exactly
/// as before, matching rust-analyzer's own "parameters coloured, locals plain"
/// scheme. `self` is a dedicated `(self)` node, not an `(identifier)`, so it is
/// untouched and keeps its `@variable.builtin` colour.
const RUST_LOCALS_QUERY: &str = r#"
[
  (source_file)
  (function_item)
  (closure_expression)
  (block)
] @local.scope

(parameter (identifier) @local.definition)
(closure_parameters (identifier) @local.definition)
(let_declaration pattern: (identifier) @local.definition)
(for_expression pattern: (identifier) @local.definition)

(identifier) @local.reference
"#;

/// Base16-Ocean-Dark inspired palette, indexed by HIGHLIGHT_NAMES position.
fn style_for(idx: usize) -> Style {
    style_for_name(HIGHLIGHT_NAMES.get(idx).copied().unwrap_or(""))
}

/// The palette keyed by capture name. Shared by tree-sitter highlighting
/// (via `style_for`) and by the LSP semantic-token overlay (via
/// `semantic_style_for`) so both layers paint the same colors.
fn style_for_name(name: &str) -> Style {
    match name {
        "comment" => Style::default()
            .fg(rgb(0x65, 0x73, 0x7e))
            .add_modifier(Modifier::ITALIC),
        "keyword" | "label" => Style::default()
            .fg(rgb(0xb4, 0x8e, 0xad))
            .add_modifier(Modifier::BOLD),
        "string" | "string.escape" | "string.special" => Style::default().fg(rgb(0xa3, 0xbe, 0x8c)),
        "number" | "boolean" | "constant" | "constant.builtin" => {
            Style::default().fg(rgb(0xd0, 0x87, 0x70))
        }
        "function" | "function.builtin" | "function.macro" | "function.method" | "constructor" => {
            Style::default().fg(rgb(0x8f, 0xa1, 0xb3))
        }
        "type" | "type.builtin" => Style::default().fg(rgb(0xeb, 0xcb, 0x8b)),
        "attribute" | "tag" => Style::default().fg(rgb(0xbf, 0x61, 0x6a)),
        "property" => Style::default().fg(rgb(0x8f, 0xa1, 0xb3)),
        "variable.builtin" => Style::default().fg(rgb(0xbf, 0x61, 0x6a)),
        "variable.parameter" => Style::default().fg(rgb(0xd0, 0x87, 0x70)),
        "module" => Style::default().fg(rgb(0xeb, 0xcb, 0x8b)),
        "operator"
        | "punctuation"
        | "punctuation.bracket"
        | "punctuation.delimiter"
        | "punctuation.special"
        | "variable" => Style::default().fg(rgb(0xc0, 0xc5, 0xce)),
        _ => Style::default().fg(rgb(0xc0, 0xc5, 0xce)),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LangKind {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Json,
    Toml,
    Yaml,
    Markdown,
    Go,
    Html,
    Css,
    Bash,
    C,
    Cpp,
}

pub fn lang_for_extension(ext: &str) -> Option<LangKind> {
    Some(match ext {
        "rs" => LangKind::Rust,
        "py" | "pyi" | "pyw" => LangKind::Python,
        "js" | "mjs" | "cjs" | "jsx" => LangKind::JavaScript,
        "ts" => LangKind::TypeScript,
        "tsx" => LangKind::Tsx,
        "json" | "jsonc" => LangKind::Json,
        "toml" => LangKind::Toml,
        "yaml" | "yml" => LangKind::Yaml,
        "md" | "markdown" => LangKind::Markdown,
        "go" => LangKind::Go,
        "html" | "htm" => LangKind::Html,
        "css" | "scss" | "sass" => LangKind::Css,
        "sh" | "bash" | "zsh" => LangKind::Bash,
        "c" | "h" => LangKind::C,
        "cpp" | "cc" | "cxx" | "c++" | "C" | "hpp" | "hxx" | "h++" => LangKind::Cpp,
        _ => return None,
    })
}

fn build_config(kind: LangKind) -> Option<HighlightConfiguration> {
    let mut cfg = match kind {
        LangKind::Rust => {
            // Prepend a broad `(identifier) @variable` carrier so the locals
            // query can emit a parameter reference's resolved colour (the
            // tree-sitter-highlight crate only emits a resolved reference when
            // its node also carries a highlights capture). Bundled specific
            // rules follow and win under last-match-wins.
            let highlights = format!(
                "{}\n{}\n{}\n{}",
                RUST_IDENTIFIER_OVERLAY_QUERY,
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                RUST_MODULE_OVERLAY_QUERY,
                RUST_USE_OVERLAY_QUERY,
            );
            HighlightConfiguration::new(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                &highlights,
                tree_sitter_rust::INJECTIONS_QUERY,
                RUST_LOCALS_QUERY,
            )
            .ok()?
        }
        LangKind::Python => {
            // The bundled Python highlights.scm emits no
            // @variable.parameter, so parameters render as plain
            // @variable. Append the overlay LAST so its parameter
            // captures override the broad (identifier) @variable rule
            // under tree-sitter-highlight's last-match-wins rule.
            let combined = format!(
                "{}\n{}\n{}",
                tree_sitter_python::HIGHLIGHTS_QUERY,
                PYTHON_PARAM_OVERLAY_QUERY,
                PYTHON_NAMESPACE_TYPE_OVERLAY_QUERY,
            );
            HighlightConfiguration::new(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                &combined,
                "",
                PYTHON_LOCALS_QUERY,
            )
            .ok()?
        }
        LangKind::JavaScript => {
            // Prepend parameter-definition captures so plain
            // `(formal_parameters (identifier))` params (which the bundled
            // query's `(pattern/identifier)` does not match) are recorded as
            // definitions and their body references inherit the parameter
            // colour instantly.
            let locals = format!(
                "{}\n{}",
                JS_PARAM_LOCALS_OVERLAY_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
            );
            // Append the parameter highlights overlay so JS parameters are
            // coloured `@variable.parameter` (the bundled query has none); the
            // locals query then propagates that colour to body references.
            let highlights = format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                JS_PARAM_HIGHLIGHTS_OVERLAY_QUERY,
            );
            HighlightConfiguration::new(
                tree_sitter_javascript::LANGUAGE.into(),
                "javascript",
                &highlights,
                tree_sitter_javascript::INJECTIONS_QUERY,
                &locals,
            )
            .ok()?
        }
        LangKind::TypeScript => {
            // The bundled TypeScript highlights.scm captures only @type,
            // @type.builtin, @variable.parameter, @punctuation.bracket,
            // and @keyword. All the @function / @function.method /
            // @property / @constructor / @constant captures live in the
            // tree-sitter-javascript highlights.scm; the TypeScript
            // grammar inherits from the JavaScript one but the Rust
            // tree-sitter-highlight crate does not auto-resolve the
            // `; inherits:` directive, so the queries have to be
            // concatenated by hand. JS first so the TS file overrides
            // it where they overlap (capitalized identifiers as @type
            // rather than @constructor, etc.). Tree-sitter-highlight
            // applies last-matching capture on overlapping ranges.
            let combined = format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            );
            // The bundled TS locals query has only parameter definitions; the
            // overlay adds the scopes and reference capture it lacks so body
            // references colour instantly.
            let locals = format!(
                "{}\n{}",
                tree_sitter_typescript::LOCALS_QUERY,
                TS_LOCALS_OVERLAY_QUERY,
            );
            HighlightConfiguration::new(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                &combined,
                "",
                &locals,
            )
            .ok()?
        }
        LangKind::Tsx => {
            // Same fix as TypeScript above (JS query prepended for
            // @function / @property / etc.), plus a JSX overlay
            // appended LAST so its (jsx_attribute (property_identifier)
            // @attribute) capture wins over the JS query's broad
            // (property_identifier) @property capture under
            // tree-sitter-highlight's last-match-wins rule.
            let combined = format!(
                "{}\n{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                TSX_JSX_OVERLAY_QUERY,
            );
            // Same locals overlay as TypeScript: bundled param definitions
            // plus the scopes and reference capture they lack.
            let locals = format!(
                "{}\n{}",
                tree_sitter_typescript::LOCALS_QUERY,
                TS_LOCALS_OVERLAY_QUERY,
            );
            HighlightConfiguration::new(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                "tsx",
                &combined,
                "",
                &locals,
            )
            .ok()?
        }
        LangKind::Json => HighlightConfiguration::new(
            tree_sitter_json::LANGUAGE.into(),
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        )
        .ok()?,
        LangKind::Toml => HighlightConfiguration::new(
            tree_sitter_toml_ng::LANGUAGE.into(),
            "toml",
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            "",
            "",
        )
        .ok()?,
        LangKind::Yaml => HighlightConfiguration::new(
            tree_sitter_yaml::LANGUAGE.into(),
            "yaml",
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            "",
            "",
        )
        .ok()?,
        LangKind::Markdown => HighlightConfiguration::new(
            tree_sitter_md::LANGUAGE.into(),
            "markdown",
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            tree_sitter_md::INJECTION_QUERY_BLOCK,
            "",
        )
        .ok()?,
        LangKind::Go => HighlightConfiguration::new(
            tree_sitter_go::LANGUAGE.into(),
            "go",
            tree_sitter_go::HIGHLIGHTS_QUERY,
            "",
            "",
        )
        .ok()?,
        LangKind::Html => HighlightConfiguration::new(
            tree_sitter_html::LANGUAGE.into(),
            "html",
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            "",
        )
        .ok()?,
        LangKind::Css => HighlightConfiguration::new(
            tree_sitter_css::LANGUAGE.into(),
            "css",
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            "",
        )
        .ok()?,
        LangKind::Bash => HighlightConfiguration::new(
            tree_sitter_bash::LANGUAGE.into(),
            "bash",
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            "",
        )
        .ok()?,
        LangKind::C => HighlightConfiguration::new(
            tree_sitter_c::LANGUAGE.into(),
            "c",
            tree_sitter_c::HIGHLIGHT_QUERY,
            "",
            "",
        )
        .ok()?,
        LangKind::Cpp => HighlightConfiguration::new(
            tree_sitter_cpp::LANGUAGE.into(),
            "cpp",
            tree_sitter_cpp::HIGHLIGHT_QUERY,
            "",
            "",
        )
        .ok()?,
    };
    cfg.configure(HIGHLIGHT_NAMES);
    Some(cfg)
}

/// One styled byte-range inside a line.
#[derive(Clone, Copy, Debug)]
pub struct HiSpan {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

thread_local! {
    // One built `HighlightConfiguration` per language, shared by every editor
    // on the UI thread. Compiling the (overlay-augmented) tree-sitter queries
    // costs tens of milliseconds; without sharing, each new tab rebuilt them on
    // its first highlight, blocking the first paint. The config is immutable
    // after `build_config` calls `configure`, so an `Rc` is safe to hand out.
    static SHARED_CONFIGS: RefCell<HashMap<LangKind, Rc<HighlightConfiguration>>> =
        RefCell::new(HashMap::new());
}

/// Build once per thread and return the shared highlight configuration for
/// `kind`, or `None` if the language has no tree-sitter config.
fn shared_config(kind: LangKind) -> Option<Rc<HighlightConfiguration>> {
    SHARED_CONFIGS.with(|c| {
        if let Some(cfg) = c.borrow().get(&kind) {
            return Some(Rc::clone(cfg));
        }
        let built = Rc::new(build_config(kind)?);
        c.borrow_mut().insert(kind, Rc::clone(&built));
        Some(built)
    })
}

/// Pre-build the tree-sitter highlight configurations on the calling thread so
/// the first file open does not pay the query-compilation cost on the paint
/// path. Call once at startup on the UI thread. Idempotent.
pub fn prewarm_configs() {
    for kind in [
        LangKind::Rust,
        LangKind::Python,
        LangKind::TypeScript,
        LangKind::Tsx,
        LangKind::JavaScript,
    ] {
        let _ = shared_config(kind);
    }
}

pub struct LangRegistry {
    _private: (),
}

impl Default for LangRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LangRegistry {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Return the shared configuration for `kind`, building it once per thread.
    /// Takes `&mut self` for call-site compatibility, but state lives in the
    /// thread-local `SHARED_CONFIGS` so all editors reuse one build.
    pub fn get(&mut self, kind: LangKind) -> Option<Rc<HighlightConfiguration>> {
        shared_config(kind)
    }
}

/// Highlight `text` and project the events onto per-line span lists.
/// `line_starts[i]` is the byte offset of the start of line i in `text`.
pub fn highlight_text(
    registry: &mut LangRegistry,
    kind: LangKind,
    text: &[u8],
    line_starts: &[usize],
) -> Vec<Vec<HiSpan>> {
    let mut per_line: Vec<Vec<HiSpan>> = vec![Vec::new(); line_starts.len()];
    let cfg = match registry.get(kind) {
        Some(c) => c,
        None => return per_line,
    };
    let mut hl = Highlighter::new();
    let events = match hl.highlight(cfg.as_ref(), text, None, |_| None) {
        Ok(e) => e,
        Err(_) => return per_line,
    };

    let mut stack: Vec<usize> = Vec::new();
    for ev in events {
        match ev {
            Ok(HighlightEvent::HighlightStart(h)) => stack.push(h.0),
            Ok(HighlightEvent::HighlightEnd) => {
                stack.pop();
            }
            Ok(HighlightEvent::Source { start, end }) => {
                let style = match stack.last() {
                    Some(idx) => style_for(*idx),
                    None => Style::default().fg(rgb(0xc0, 0xc5, 0xce)),
                };
                project_range(start, end, style, line_starts, &mut per_line);
            }
            Err(_) => {}
        }
    }
    per_line
}

fn project_range(
    mut start: usize,
    end: usize,
    style: Style,
    line_starts: &[usize],
    per_line: &mut [Vec<HiSpan>],
) {
    if start >= end || line_starts.is_empty() {
        return;
    }
    // Find the line containing `start`.
    let mut line_idx = match line_starts.binary_search(&start) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    while start < end && line_idx < line_starts.len() {
        let line_start = line_starts[line_idx];
        let next_line_start = line_starts.get(line_idx + 1).copied().unwrap_or(usize::MAX);
        let chunk_end = end.min(next_line_start);
        // If we ran into the next-line boundary, the last byte before it is the '\n'
        // separator, which is not part of the line string. Trim it from the span.
        let on_boundary = chunk_end == next_line_start && line_idx + 1 < line_starts.len();
        let span_end_abs = if on_boundary && chunk_end > line_start {
            chunk_end - 1
        } else {
            chunk_end
        };
        if span_end_abs > start {
            per_line[line_idx].push(HiSpan {
                start: start - line_start,
                end: span_end_abs - line_start,
                style,
            });
        }
        start = chunk_end;
        line_idx += 1;
    }
}

/// Map a standard LSP semantic-token type name onto a croft capture
/// name, then to its `Style`. Returns `None` for token types we do not
/// recolor, so the underlying tree-sitter highlight shows through, which
/// is the VS Code / Zed "combined" fallback model: semantic wins only
/// where it has an opinion, otherwise syntax prevails.
fn semantic_style_for(token_type: &str) -> Option<Style> {
    let capture = match token_type {
        // `selfParameter`/`clsParameter` are ty's split of `self`/`cls` out of
        // the generic `parameter` type; color them like any other parameter.
        "parameter" | "selfParameter" | "clsParameter" => "variable.parameter",
        "variable" => "variable",
        "property" => "property",
        "function" | "method" => "function",
        "macro" => "function.macro",
        "namespace" => "module",
        "type" | "class" | "enum" | "interface" | "struct" | "typeParameter" => "type",
        // `builtinConstant` is ty's type for `True`/`False`/`None`/`...`.
        "enumMember" | "builtinConstant" => "constant",
        "keyword" | "modifier" => "keyword",
        "comment" => "comment",
        "string" => "string",
        "number" => "number",
        "regexp" => "string.special",
        "operator" => "operator",
        "decorator" => "attribute",
        "event" | "label" => "label",
        _ => return None,
    };
    Some(style_for_name(capture))
}

/// Decode an LSP `textDocument/semanticTokens/full` `data` array into
/// per-line byte-offset spans, ready to overlay on the tree-sitter
/// highlights. The array is flat groups of 5 u32:
/// `[deltaLine, deltaStartChar, length, tokenType, tokenModifiers]`,
/// each field relative to the previous token. `token_type_names` is the
/// server's legend in index order. LSP positions are UTF-16 code units;
/// we convert to byte offsets against `text`. Tokens whose type maps to
/// no croft style are skipped (tree-sitter base shows through).
pub fn decode_semantic_tokens(
    data: &[u32],
    token_type_names: &[String],
    text: &[u8],
    line_starts: &[usize],
) -> Vec<Vec<HiSpan>> {
    let mut per_line: Vec<Vec<HiSpan>> = vec![Vec::new(); line_starts.len()];
    let src = match std::str::from_utf8(text) {
        Ok(s) => s,
        Err(_) => return per_line,
    };
    let mut line: usize = 0;
    let mut col_u16: u32 = 0;
    for group in data.chunks_exact(5) {
        let (delta_line, delta_start, length, ttype) = (group[0], group[1], group[2], group[3]);
        if delta_line > 0 {
            line += delta_line as usize;
            col_u16 = delta_start;
        } else {
            col_u16 += delta_start;
        }
        if line >= line_starts.len() {
            continue;
        }
        let name = match token_type_names.get(ttype as usize) {
            Some(n) => n.as_str(),
            None => continue,
        };
        let style = match semantic_style_for(name) {
            Some(s) => s,
            None => continue,
        };
        let line_start = line_starts[line];
        let content_end = match line_starts.get(line + 1) {
            Some(&next) => next.saturating_sub(1).max(line_start),
            None => src.len(),
        };
        let line_str = &src[line_start..content_end];
        if let Some((b0, b1)) = utf16_span_to_bytes(line_str, col_u16, length)
            && b1 > b0
        {
            per_line[line].push(HiSpan {
                start: b0,
                end: b1,
                style,
            });
        }
    }
    per_line
}

/// Convert a UTF-16 `[start, start+len)` column range within a single
/// line into a byte `[start, end)` range. Returns `None` if the range
/// falls outside the line. Positions landing exactly at end-of-line are
/// clamped to the line length.
fn utf16_span_to_bytes(line: &str, start_u16: u32, len_u16: u32) -> Option<(usize, usize)> {
    let end_u16 = start_u16.checked_add(len_u16)?;
    let mut u16_count: u32 = 0;
    let mut byte = 0usize;
    let mut b_start: Option<usize> = None;
    let mut b_end: Option<usize> = None;
    for ch in line.chars() {
        if u16_count == start_u16 {
            b_start = Some(byte);
        }
        if u16_count == end_u16 {
            b_end = Some(byte);
            break;
        }
        u16_count += ch.len_utf16() as u32;
        byte += ch.len_utf8();
    }
    if u16_count == start_u16 {
        b_start.get_or_insert(byte);
    }
    if u16_count == end_u16 {
        b_end.get_or_insert(byte);
    }
    Some((b_start?, b_end?))
}

/// Build a `line_starts` table from raw bytes.
pub fn compute_line_starts(text: &[u8]) -> Vec<usize> {
    let mut out = vec![0usize];
    for (i, &b) in text.iter().enumerate() {
        if b == b'\n' {
            out.push(i + 1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Color of the first occurrence of `needle` on the line at `row`.
    fn py_color(src: &str, row: usize, needle: &str) -> Option<Color> {
        let mut reg = LangRegistry::new();
        let ls = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Python, src.as_bytes(), &ls);
        let line = src.split('\n').nth(row)?;
        let col = line.find(needle)?;
        h[row]
            .iter()
            .find(|s| s.start <= col && col < s.end)
            .map(|s| s.style.fg)
            .flatten()
    }

    #[test]
    fn highlight_python_colors_imported_classes_and_modules_to_match_ty() {
        // The cold-open snap on an import-heavy file was tree-sitter painting
        // imported names grey/blue while ty paints them yellow. The namespace +
        // CamelCase overlay must make the base agree with ty INSTANTLY: module
        // names in imports and CamelCase class references are both `@module` /
        // `@type` (yellow #ebcb8b), matching ty's `namespace` / `class`.
        let yellow = rgb(0xeb, 0xcb, 0x8b);
        let src = "import pydantic\n\
from anterior.lib_tasks.schema import Config\n\
\n\
def f() -> Config:\n\
    return pydantic.BaseModel()\n";

        // Module name in `import pydantic`.
        assert_eq!(
            py_color(src, 0, "pydantic"),
            Some(yellow),
            "import module name"
        );
        // Dotted module path segments in a from-import.
        assert_eq!(
            py_color(src, 1, "anterior"),
            Some(yellow),
            "from-import module head"
        );
        assert_eq!(
            py_color(src, 1, "schema"),
            Some(yellow),
            "from-import module tail"
        );
        // CamelCase imported class, both at import and as a return annotation.
        assert_eq!(
            py_color(src, 1, "Config"),
            Some(yellow),
            "imported class name"
        );
        assert_eq!(
            py_color(src, 3, "Config"),
            Some(yellow),
            "class as annotation"
        );
        // Module reference in the BODY resolves through the locals query and
        // inherits the namespace colour (this is the part syntax-position rules
        // alone can't reach).
        assert_eq!(
            py_color(src, 4, "pydantic"),
            Some(yellow),
            "module base in body"
        );
        // The class accessed off the module is a CamelCase reference -> type.
        assert_eq!(
            py_color(src, 4, "BaseModel"),
            Some(yellow),
            "CamelCase attr ref"
        );
    }

    #[test]
    fn highlight_python_camelcase_overlay_excludes_all_caps_constants() {
        // The CamelCase rule requires a lowercase letter, so an ALL_CAPS name
        // is NOT recoloured to type-yellow; it stays on the bundled @constant
        // rule (orange). This keeps module-level constants highlighted and
        // avoids turning them yellow.
        let orange = rgb(0xd0, 0x87, 0x70);
        let src = "MAX_SIZE = 300\n";
        assert_eq!(py_color(src, 0, "MAX_SIZE"), Some(orange));
    }

    #[test]
    fn highlight_c_produces_spans() {
        let mut reg = LangRegistry::new();
        let src = "int main() {\n    return 0;\n}\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::C, src.as_bytes(), &line_starts);
        assert!(
            !h[0].is_empty(),
            "line 0 of C source should have highlight spans"
        );
    }

    #[test]
    fn highlight_cpp_produces_spans() {
        let mut reg = LangRegistry::new();
        let src = "int main() {\n    return 0;\n}\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Cpp, src.as_bytes(), &line_starts);
        assert!(
            !h[0].is_empty(),
            "line 0 of C++ source should have highlight spans"
        );
    }

    #[test]
    fn lang_for_extension_known() {
        assert_eq!(lang_for_extension("rs"), Some(LangKind::Rust));
        assert_eq!(lang_for_extension("py"), Some(LangKind::Python));
        assert_eq!(lang_for_extension("ts"), Some(LangKind::TypeScript));
        assert_eq!(lang_for_extension("tsx"), Some(LangKind::Tsx));
        assert_eq!(lang_for_extension("js"), Some(LangKind::JavaScript));
        assert_eq!(lang_for_extension("jsx"), Some(LangKind::JavaScript));
        assert_eq!(lang_for_extension("json"), Some(LangKind::Json));
        assert_eq!(lang_for_extension("toml"), Some(LangKind::Toml));
        assert_eq!(lang_for_extension("yaml"), Some(LangKind::Yaml));
        assert_eq!(lang_for_extension("yml"), Some(LangKind::Yaml));
        assert_eq!(lang_for_extension("md"), Some(LangKind::Markdown));
        assert_eq!(lang_for_extension("go"), Some(LangKind::Go));
        assert_eq!(lang_for_extension("html"), Some(LangKind::Html));
        assert_eq!(lang_for_extension("htm"), Some(LangKind::Html));
        assert_eq!(lang_for_extension("css"), Some(LangKind::Css));
        assert_eq!(lang_for_extension("scss"), Some(LangKind::Css));
        assert_eq!(lang_for_extension("sh"), Some(LangKind::Bash));
        assert_eq!(lang_for_extension("c"), Some(LangKind::C));
        assert_eq!(lang_for_extension("h"), Some(LangKind::C));
        assert_eq!(lang_for_extension("cpp"), Some(LangKind::Cpp));
        assert_eq!(lang_for_extension("cc"), Some(LangKind::Cpp));
        assert_eq!(lang_for_extension("cxx"), Some(LangKind::Cpp));
        assert_eq!(lang_for_extension("hpp"), Some(LangKind::Cpp));
        assert_eq!(lang_for_extension("hxx"), Some(LangKind::Cpp));
    }

    #[test]
    fn lang_for_extension_unknown() {
        assert_eq!(lang_for_extension("xyz"), None);
        assert_eq!(lang_for_extension(""), None);
    }

    #[test]
    fn line_starts_empty_input() {
        assert_eq!(compute_line_starts(b""), vec![0]);
    }

    #[test]
    fn line_starts_single_line_no_newline() {
        assert_eq!(compute_line_starts(b"hello"), vec![0]);
    }

    #[test]
    fn line_starts_multiple_lines() {
        // "a\nbb\nccc"  → starts at 0, 2, 5
        assert_eq!(compute_line_starts(b"a\nbb\nccc"), vec![0, 2, 5]);
    }

    #[test]
    fn line_starts_trailing_newline() {
        // "a\nb\n" → starts at 0, 2, 4 (an empty trailing line)
        assert_eq!(compute_line_starts(b"a\nb\n"), vec![0, 2, 4]);
    }

    #[test]
    fn line_starts_only_newlines() {
        assert_eq!(compute_line_starts(b"\n\n\n"), vec![0, 1, 2, 3]);
    }

    #[test]
    fn registry_caches_configs() {
        let mut reg = LangRegistry::new();
        // First call should build and insert.
        assert!(reg.get(LangKind::Rust).is_some());
        // Second call should hit the cache and still succeed.
        assert!(reg.get(LangKind::Rust).is_some());
    }

    #[test]
    fn highlight_configs_are_shared_across_editors() {
        // The query-compilation cost must be paid once per process, not once
        // per editor tab: a new tab's first paint should never rebuild the
        // config. Two independent registries must hand out the same `Rc`.
        prewarm_configs();
        let mut a = LangRegistry::new();
        let mut b = LangRegistry::new();
        let ca = a.get(LangKind::Rust).expect("rust config");
        let cb = b.get(LangKind::Rust).expect("rust config");
        assert!(
            Rc::ptr_eq(&ca, &cb),
            "every editor must share one built highlight config"
        );
    }

    #[test]
    fn highlight_rust_produces_per_line_spans() {
        let mut reg = LangRegistry::new();
        let src = "fn main() {\n    let x = 1;\n}\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let highlights = highlight_text(&mut reg, LangKind::Rust, src.as_bytes(), &line_starts);
        // 4 line entries (last "" after trailing newline)
        assert_eq!(highlights.len(), 4);
        // Line 0 has "fn" as a keyword — at least one span should land there.
        assert!(!highlights[0].is_empty(), "line 0 should have spans");
        // Spans on line 0 should be within line 0's length.
        let line0_len = "fn main() {".len();
        for sp in &highlights[0] {
            assert!(
                sp.start <= line0_len,
                "span start {} > line len {}",
                sp.start,
                line0_len
            );
            assert!(
                sp.end <= line0_len,
                "span end {} > line len {}",
                sp.end,
                line0_len
            );
            assert!(sp.start <= sp.end);
        }
    }

    #[test]
    fn semantic_tokens_color_parameter_in_body() {
        // The whole point of the LSP overlay: a parameter referenced in
        // the function body (where tree-sitter sees only a plain
        // identifier) must get the parameter color, matching VS Code.
        let src = "def f(x):\n    return x\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let legend = vec!["parameter".to_string(), "variable".to_string()];
        // Two `parameter` tokens: `x` in the signature (line 0, col 6)
        // and `x` in the body (line 1, col 11). Relative-encoded.
        let data: Vec<u32> = vec![
            0, 6, 1, 0, 0, // line 0, char 6, len 1, type 0 (parameter)
            1, 11, 1, 0, 0, // +1 line, char 11, len 1, type 0 (parameter)
        ];
        let spans = decode_semantic_tokens(&data, &legend, src.as_bytes(), &line_starts);

        let param_fg = Some(rgb(0xd0, 0x87, 0x70));
        // Body line: byte col 11 is the `x` in "    return x".
        let body = "    return x";
        let col = body.rfind('x').unwrap();
        let span = spans[1]
            .iter()
            .find(|s| s.start <= col && col < s.end)
            .expect("a semantic span should cover the body parameter `x`");
        assert_eq!(
            span.style.fg, param_fg,
            "parameter referenced in the body should carry the parameter color"
        );
    }

    #[test]
    fn semantic_tokens_skip_unmapped_and_handle_unicode() {
        // Unknown token types are skipped (tree-sitter shows through),
        // and UTF-16 columns past a multi-byte char convert correctly.
        let src = "x = \"é\" + ab\n"; // 'é' is 2 bytes / 1 UTF-16 unit
        let line_starts = compute_line_starts(src.as_bytes());
        let legend = vec!["bogusType".to_string(), "variable".to_string()];
        // `ab` starts after: x(1) space(1) =(1) space(1) "(1) é(1 u16) "(1) space(1) +(1) space(1) = col 10
        let data: Vec<u32> = vec![
            0, 0, 1, 0, 0, // type 0 = bogus -> skipped
            0, 10, 2, 1, 0, // type 1 = variable, len 2 -> `ab`
        ];
        let spans = decode_semantic_tokens(&data, &legend, src.as_bytes(), &line_starts);
        assert_eq!(spans[0].len(), 1, "only the mapped token survives");
        let byte_col = src.find("ab").unwrap();
        let s = &spans[0][0];
        assert_eq!(s.start, byte_col, "UTF-16 col converted to the right byte");
        assert_eq!(s.end, byte_col + 2);
    }

    #[test]
    fn highlight_python_colors_parameters() {
        // The bundled Python query emits no @variable.parameter; the
        // appended overlay should color def/lambda parameters with the
        // parameter style rather than leaving them as default @variable.
        let mut reg = LangRegistry::new();
        let src = "def f(text, n=1, *args, **kw):\n    return text\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Python, src.as_bytes(), &line_starts);

        let param_fg = Some(rgb(0xd0, 0x87, 0x70));
        let default_fg = Some(rgb(0xc0, 0xc5, 0xce));
        let line0 = "def f(text, n=1, *args, **kw):";

        // Each parameter name on line 0 should carry the parameter color.
        for name in ["text", "n", "args", "kw"] {
            let col = line0.find(name).expect("param in source");
            let span = h[0]
                .iter()
                .find(|s| s.start <= col && col < s.end)
                .unwrap_or_else(|| panic!("no span covers parameter `{name}`"));
            assert_eq!(
                span.style.fg, param_fg,
                "parameter `{name}` should use the parameter color, not {:?}",
                span.style.fg
            );
            assert_ne!(span.style.fg, default_fg);
        }
    }

    #[test]
    fn highlight_python_colors_parameter_references_in_body_via_locals() {
        // Tree-sitter alone (no LSP) must colour a parameter's REFERENCES in
        // the function body, not just at the `def` site. Without a locals
        // query, body references fall through to `(identifier) @variable`
        // (grey) and only ty's semantic-token overlay recolours them, seconds
        // later on a cold file. The Python locals query links each body
        // reference to its in-scope parameter definition so the parameter
        // colour appears instantly, the way Helix/Zed do it.
        let mut reg = LangRegistry::new();
        let src = "def f(text):\n    return text + text\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Python, src.as_bytes(), &line_starts);

        let param_fg = Some(rgb(0xd0, 0x87, 0x70));
        let default_fg = Some(rgb(0xc0, 0xc5, 0xce));
        let line1 = "    return text + text";

        // Both body references of `text` on line 1 must carry the parameter
        // colour, not the default @variable grey.
        let first = line1.find("text").expect("first body ref");
        let second = line1[first + 4..].find("text").expect("second body ref") + first + 4;
        for col in [first, second] {
            let span = h[1]
                .iter()
                .find(|s| s.start <= col && col < s.end)
                .unwrap_or_else(|| panic!("no span covers body reference at col {col}"));
            assert_eq!(
                span.style.fg, param_fg,
                "body parameter reference at col {col} should use the parameter colour, not {:?}",
                span.style.fg
            );
            assert_ne!(span.style.fg, default_fg);
        }
    }

    fn assert_body_param_ref_is_param_colored(lang: LangKind, src: &str, line1: &str) {
        let mut reg = LangRegistry::new();
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, lang, src.as_bytes(), &line_starts);
        let param_fg = Some(rgb(0xd0, 0x87, 0x70));
        let default_fg = Some(rgb(0xc0, 0xc5, 0xce));
        let first = line1.find("text").expect("first body ref");
        let second = line1[first + 4..].find("text").expect("second body ref") + first + 4;
        for col in [first, second] {
            let span = h[1]
                .iter()
                .find(|s| s.start <= col && col < s.end)
                .unwrap_or_else(|| panic!("{lang:?}: no span covers body reference at col {col}"));
            assert_eq!(
                span.style.fg, param_fg,
                "{lang:?}: body parameter reference at col {col} should use the parameter colour, not {:?}",
                span.style.fg
            );
            assert_ne!(span.style.fg, default_fg);
        }
    }

    #[test]
    fn highlight_typescript_colors_parameter_references_in_body_via_locals() {
        assert_body_param_ref_is_param_colored(
            LangKind::TypeScript,
            "function f(text: string) {\n    return text + text;\n}\n",
            "    return text + text;",
        );
    }

    #[test]
    fn highlight_tsx_colors_parameter_references_in_body_via_locals() {
        assert_body_param_ref_is_param_colored(
            LangKind::Tsx,
            "function f(text: string) {\n    return text + text;\n}\n",
            "    return text + text;",
        );
    }

    #[test]
    fn highlight_javascript_colors_parameter_references_in_body_via_locals() {
        assert_body_param_ref_is_param_colored(
            LangKind::JavaScript,
            "function f(text) {\n    return text + text;\n}\n",
            "    return text + text;",
        );
    }

    #[test]
    fn highlight_rust_colors_parameter_references_in_body_via_locals() {
        // tree-sitter-rust colours `(parameter (identifier))` orange at the
        // declaration but ships no locals query, so a parameter's body
        // references match no highlights capture and fall through to default
        // grey until rust-analyzer's semantic-token overlay lands, which on a
        // cold crate is ~25s after open. The Rust locals query links each body
        // reference to its in-scope parameter definition so the parameter
        // colour appears instantly, the way Helix/Zed do it.
        assert_body_param_ref_is_param_colored(
            LangKind::Rust,
            "fn f(text: i32) -> i32 {\n    text + text\n}\n",
            "    text + text",
        );
    }

    #[test]
    fn highlight_rust_keeps_specific_captures_over_broad_identifier_overlay() {
        // The prepended `(identifier) @variable` carrier (needed so the locals
        // query can emit a resolved parameter reference) must not swallow the
        // bundled specific captures under last-match-wins: a function call and
        // a type name must keep their own colours, not turn @variable grey.
        let mut reg = LangRegistry::new();
        let src = "fn run() {\n    let v = compute(x);\n    let p: Parser = v;\n}\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Rust, src.as_bytes(), &line_starts);

        let function = Some(rgb(0x8f, 0xa1, 0xb3));
        let type_c = Some(rgb(0xeb, 0xcb, 0x8b));
        let default_fg = Some(rgb(0xc0, 0xc5, 0xce));

        let line1 = "    let v = compute(x);";
        let call = span_at(&h[1], line1, "compute").expect("call name span");
        assert_eq!(
            call.style.fg, function,
            "call name must keep @function colour"
        );
        assert_ne!(call.style.fg, default_fg);

        let line2 = "    let p: Parser = v;";
        let ty = span_at(&h[2], line2, "Parser").expect("type name span");
        assert_eq!(ty.style.fg, type_c, "type name must keep @type colour");
        assert_ne!(ty.style.fg, default_fg);
    }

    #[test]
    fn highlight_rust_colors_module_paths_and_enum_variant_defs_to_match_lsp() {
        // rust-analyzer paints module path segments (`std`, `collections`) as
        // `namespace` (yellow) and enum-variant declarations (`Light`, `Dark`)
        // as `enumMember` (orange/constant). tree-sitter-rust leaves the path
        // segments grey @variable and colours capitalized scoped names
        // @constructor (blue-grey), so when RA's overlay lands ~1-3s after open
        // those tokens visibly recolour. Colour them in the tree-sitter base so
        // the overlay is a no-op for these (the most pervasive recolour, since
        // every `use`/path has module segments).
        let mut reg = LangRegistry::new();
        let src = "use std::collections::HashMap;\nenum Shade { Light, Dark }\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Rust, src.as_bytes(), &line_starts);

        let module_c = Some(rgb(0xeb, 0xcb, 0x8b)); // @module / @type (yellow)
        let constant_c = Some(rgb(0xd0, 0x87, 0x70)); // @constant (orange)

        let l0 = "use std::collections::HashMap;";
        let std_span = span_at(&h[0], l0, "std").expect("std path span");
        assert_eq!(std_span.style.fg, module_c, "`std` must be module-coloured");
        let coll_span = span_at(&h[0], l0, "collections").expect("collections span");
        assert_eq!(
            coll_span.style.fg, module_c,
            "`collections` must be module-coloured"
        );

        let l1 = "enum Shade { Light, Dark }";
        let light = span_at(&h[1], l1, "Light").expect("Light variant span");
        assert_eq!(
            light.style.fg, constant_c,
            "enum variant declaration must be constant-coloured"
        );
        let dark = span_at(&h[1], l1, "Dark").expect("Dark variant span");
        assert_eq!(dark.style.fg, constant_c);
    }

    #[test]
    fn highlight_rust_colors_use_imports_to_match_lsp() {
        // rust-analyzer paints `use` paths by resolved kind: module segments
        // (`std`, `collections`, `anyhow`) as `namespace` (yellow) and imported
        // type/trait/struct names (`BTreeSet`, `Context`, `Result`) as
        // `struct`/`interface`/`typeAlias` (yellow). The bundled
        // tree-sitter-rust query colours capitalized use-list items
        // `@constructor` (blue) and a brace-list module head plain @variable
        // (grey), so the whole import block snaps blue/grey -> yellow ~1-3s
        // after open when RA's semantic overlay lands. Colour them in the base
        // so imports never snap. This is the UNAMBIGUOUS slice of
        // "CamelCase == type": `Some`/`None`/`Ok`/`Err` never appear in `use`,
        // so unlike a blanket CamelCase rule there is no enum-variant collision.
        let mut reg = LangRegistry::new();
        let src = "use std::collections::BTreeSet;\nuse anyhow::{Context, Result};\n";
        let ls = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Rust, src.as_bytes(), &ls);
        let yellow = Some(rgb(0xeb, 0xcb, 0x8b));

        let l0 = "use std::collections::BTreeSet;";
        for (needle, label) in [
            ("std", "module head `std`"),
            ("collections", "module segment `collections`"),
            ("BTreeSet", "imported struct `BTreeSet`"),
        ] {
            let sp = span_at(&h[0], l0, needle).unwrap_or_else(|| panic!("no span: {label}"));
            assert_eq!(sp.style.fg, yellow, "{label}");
        }

        let l1 = "use anyhow::{Context, Result};";
        for (needle, label) in [
            ("anyhow", "brace-list module head `anyhow`"),
            ("Context", "imported trait `Context` in list"),
            ("Result", "imported type alias `Result` in list"),
        ] {
            let sp = span_at(&h[1], l1, needle).unwrap_or_else(|| panic!("no span: {label}"));
            assert_eq!(sp.style.fg, yellow, "{label}");
        }
    }

    #[test]
    fn highlight_rust_colors_capitalized_scoped_path_heads_as_types() {
        // Regression guard for existing behaviour (the bundled
        // `scoped_identifier path -> @type` rule, kept winning over the
        // prepended `(identifier) @variable` carrier and the use overlay): the
        // capitalized head of a `::` path is a type, and rust-analyzer paints it
        // `struct`/`enum` (yellow). This already holds for real (non-macro)
        // scoped calls like `String::from(..)`; the empirically-observed blue
        // `String` tokens that still snap are inside macro token trees
        // (`vec![..]`, `format!(..)`), where tree-sitter sees only unparsed
        // identifier tokens and no syntactic rule can recover the type. Those
        // are left to the LSP/disk cache, matching the 0.1.315 doctrine.
        let mut reg = LangRegistry::new();
        let src = "fn g() {\n    let _ = String::from(\"x\");\n    let _ = Command::new(\"y\");\n    let _ = Shade::Dark;\n}\n";
        let ls = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Rust, src.as_bytes(), &ls);
        let yellow = Some(rgb(0xeb, 0xcb, 0x8b));
        for (row, line, head) in [
            (1usize, "    let _ = String::from(\"x\");", "String"),
            (2, "    let _ = Command::new(\"y\");", "Command"),
            (3, "    let _ = Shade::Dark;", "Shade"),
        ] {
            let sp = span_at(&h[row], line, head).unwrap_or_else(|| panic!("no span: {head}"));
            assert_eq!(
                sp.style.fg, yellow,
                "scoped path head `{head}` must be type-yellow"
            );
        }
    }

    #[test]
    fn highlight_rust_use_overlay_does_not_yellow_enum_variants_in_body() {
        // The use-import type rule must NOT leak to expression position: `Some`
        // referenced in a body is an enum member, which RA paints orange, not
        // type-yellow. The tree-sitter base cannot disambiguate variant-vs-type
        // in expression position (the LSP/cache covers that residual), so it
        // must keep the bundled colour here rather than be turned yellow.
        // Guards against regressing to a blanket CamelCase->type rule.
        let mut reg = LangRegistry::new();
        let src = "fn f() {\n    let _ = Some(1);\n}\n";
        let ls = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Rust, src.as_bytes(), &ls);
        let yellow = Some(rgb(0xeb, 0xcb, 0x8b));
        let l1 = "    let _ = Some(1);";
        let some = span_at(&h[1], l1, "Some").expect("a span covers `Some`");
        assert_ne!(
            some.style.fg, yellow,
            "an enum variant in expression position must not be coloured type-yellow"
        );
    }

    #[test]
    fn project_range_splits_across_lines() {
        // "abc\ndef" → line_starts [0, 4]; range 1..6 covers "bc" on line 0 and "de" on line 1
        let line_starts = vec![0usize, 4];
        let mut per_line: Vec<Vec<HiSpan>> = vec![Vec::new(); 2];
        let style = Style::default();
        project_range(1, 6, style, &line_starts, &mut per_line);
        assert_eq!(per_line[0].len(), 1);
        assert_eq!(per_line[0][0].start, 1);
        // Line content "abc" has byte length 3; the '\n' at byte 3 is excluded.
        assert_eq!(per_line[0][0].end, 3);
        assert_eq!(per_line[1].len(), 1);
        assert_eq!(per_line[1][0].start, 0);
        assert_eq!(per_line[1][0].end, 2);
    }

    #[test]
    fn project_range_excludes_newline_at_end_of_line() {
        // "abc\n" with the next-line entry set: the span 0..4 must end at 3, not 4.
        let line_starts = vec![0usize, 4];
        let mut per_line: Vec<Vec<HiSpan>> = vec![Vec::new(); 2];
        project_range(0, 4, Style::default(), &line_starts, &mut per_line);
        assert_eq!(per_line[0].len(), 1);
        assert_eq!(per_line[0][0].end, 3);
    }

    #[test]
    fn project_range_within_single_line() {
        let line_starts = vec![0usize, 5];
        let mut per_line: Vec<Vec<HiSpan>> = vec![Vec::new(); 2];
        project_range(1, 4, Style::default(), &line_starts, &mut per_line);
        assert_eq!(per_line[0].len(), 1);
        assert_eq!(per_line[0][0].start, 1);
        assert_eq!(per_line[0][0].end, 4);
        assert!(per_line[1].is_empty());
    }

    fn span_at<'a>(spans: &'a [HiSpan], line: &str, needle: &str) -> Option<&'a HiSpan> {
        let off = line.find(needle)?;
        let end = off + needle.len();
        spans.iter().find(|sp| sp.start <= off && sp.end >= end)
    }

    const TAG_COLOR: Color = Color::Rgb(0xbf, 0x61, 0x6a);
    const ATTRIBUTE_COLOR: Color = Color::Rgb(0xbf, 0x61, 0x6a);
    const FUNCTION_COLOR: Color = Color::Rgb(0x8f, 0xa1, 0xb3);
    const PROPERTY_COLOR: Color = Color::Rgb(0x8f, 0xa1, 0xb3);

    #[test]
    fn tsx_jsx_opening_tag_identifier_gets_tag_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = <div className=\"y\">hi</div>;\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Tsx, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        let div_span = span_at(&h[0], line0, "div").expect(
            "the 'div' tag identifier in <div ...> must produce at least one highlight span",
        );
        assert_eq!(
            div_span.style.fg,
            Some(TAG_COLOR),
            "JSX opening-tag identifier must render in the @tag colour ({TAG_COLOR:?}); a missing or default-foreground span means the TSX query has no jsx_opening_element capture"
        );
    }

    #[test]
    fn tsx_jsx_closing_tag_identifier_gets_tag_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = <div>hi</div>;\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Tsx, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        // Two occurrences of "div" on the line: opening and closing. The
        // closing tag starts after "</", so find the second one.
        let first = line0.find("div").unwrap();
        let second_off = line0[first + 3..].find("div").unwrap() + first + 3;
        let span = h[0]
            .iter()
            .find(|sp| sp.start <= second_off && sp.end >= second_off + 3)
            .expect("the closing-tag 'div' in </div> must produce a highlight span");
        assert_eq!(span.style.fg, Some(TAG_COLOR));
    }

    #[test]
    fn tsx_jsx_self_closing_tag_identifier_gets_tag_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = <Icon name=\"plus\" />;\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Tsx, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        let span = span_at(&h[0], line0, "Icon")
            .expect("self-closing <Icon ... /> must produce a tag-name highlight span");
        assert_eq!(span.style.fg, Some(TAG_COLOR));
    }

    #[test]
    fn tsx_jsx_attribute_name_gets_attribute_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = <div className=\"y\" onClick={f}>hi</div>;\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Tsx, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        for name in ["className", "onClick"] {
            let span = span_at(&h[0], line0, name)
                .unwrap_or_else(|| panic!("JSX attribute '{name}' must produce a highlight span"));
            assert_eq!(
                span.style.fg,
                Some(ATTRIBUTE_COLOR),
                "JSX attribute name '{name}' must render in the @attribute colour"
            );
        }
    }

    #[test]
    fn typescript_call_expression_function_name_gets_function_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = useMemo(() => 1);\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::TypeScript, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        let span = span_at(&h[0], line0, "useMemo")
            .expect("call_expression function name 'useMemo' must produce a highlight span");
        assert_eq!(
            span.style.fg,
            Some(FUNCTION_COLOR),
            "the bundled TS highlights.scm has no @function capture; without concatenating the JS query, call sites like 'useMemo(...)' render in default foreground"
        );
    }

    #[test]
    fn typescript_member_expression_property_gets_property_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = styles.sectionHeader;\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::TypeScript, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        let span = span_at(&h[0], line0, "sectionHeader")
            .expect("property_identifier in member_expression must produce a highlight span");
        assert_eq!(span.style.fg, Some(PROPERTY_COLOR));
    }

    #[test]
    fn typescript_method_call_property_gets_function_method_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = arr.map(f);\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::TypeScript, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        let span = span_at(&h[0], line0, "map")
            .expect("method call 'arr.map' must produce a highlight span on 'map'");
        assert_eq!(
            span.style.fg,
            Some(FUNCTION_COLOR),
            "method-call property must take @function.method (which shares the function colour), not the bare @property colour"
        );
    }

    #[test]
    fn tsx_call_expression_function_name_gets_function_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = useMemo(() => 1);\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Tsx, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        let span = span_at(&h[0], line0, "useMemo")
            .expect("TSX call_expression function name 'useMemo' must produce a highlight span");
        assert_eq!(span.style.fg, Some(FUNCTION_COLOR));
    }

    #[test]
    fn tsx_member_expression_property_gets_property_color() {
        let mut reg = LangRegistry::new();
        let src = "const x = styles.sectionHeader;\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Tsx, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        let span = span_at(&h[0], line0, "sectionHeader")
            .expect("TSX property_identifier in member_expression must produce a highlight span");
        assert_eq!(span.style.fg, Some(PROPERTY_COLOR));
    }

    #[test]
    fn tsx_jsx_attribute_capture_still_wins_over_generic_property_capture() {
        // Regression guard for the query-ordering choice: the JS query
        // (now concatenated before the TS query and JSX overlay) has a
        // broad `(property_identifier) @property` rule. The JSX overlay
        // must come AFTER so that on a `jsx_attribute`'s property_identifier
        // the @attribute capture wins over @property — otherwise JSX
        // attribute names would silently drift from orange-red to the
        // slate-blue @property colour.
        let mut reg = LangRegistry::new();
        let src = "const x = <div className=\"y\" />;\n";
        let line_starts = compute_line_starts(src.as_bytes());
        let h = highlight_text(&mut reg, LangKind::Tsx, src.as_bytes(), &line_starts);
        let line0 = src.lines().next().unwrap();
        let span = span_at(&h[0], line0, "className")
            .expect("'className' must still produce a highlight span");
        assert_eq!(
            span.style.fg,
            Some(ATTRIBUTE_COLOR),
            "JSX attribute name must keep the @attribute colour; if it renders in @property colour, the overlay is ordered before the JS query and needs to move after"
        );
    }

    #[test]
    fn project_range_zero_length_noop() {
        let line_starts = vec![0usize];
        let mut per_line: Vec<Vec<HiSpan>> = vec![Vec::new(); 1];
        project_range(3, 3, Style::default(), &line_starts, &mut per_line);
        assert!(per_line[0].is_empty());
    }
}
