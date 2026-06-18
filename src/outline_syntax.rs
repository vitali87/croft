//! Tree-sitter outline provider: the same data the OUTLINE panel shows, but
//! derived locally from the buffer's own syntax tree instead of waiting on a
//! language server's `textDocument/documentSymbol`. This is what paints the
//! outline *instantly* on file open, before a cold rust-analyzer/ty has
//! finished indexing; the LSP reply later supersedes it with richer kinds and
//! detail. The design mirrors Zed (whose outline is a tree-sitter query by
//! deliberate choice) and aerial.nvim (whose default backend order is
//! `treesitter` first, `lsp` second).
//!
//! The grammar crates we depend on do not bundle a `tags` query, so the
//! per-language outline queries below are hand-written. Each pattern captures
//! the whole definition node as `@item` (its line span feeds follow-cursor)
//! and the name node as `@name.<kind>` (its text is the label, its start the
//! jump target, and the `<kind>` suffix selects the [`OutlineKind`]). Nesting
//! is recovered from byte-range containment rather than from the query, so one
//! flat pattern set yields the indented tree the panel renders.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

use crate::highlight::LangKind;
use crate::lsp::manager::{OutlineKind, OutlineSymbol};

/// Extract the outline for `source` in `kind`'s grammar. Returns an empty vec
/// for languages without an outline query (the panel then falls back to the
/// LSP reply, exactly as before this provider existed) or when parsing fails.
pub fn symbols_for(kind: LangKind, source: &[u8]) -> Vec<OutlineSymbol> {
    let Some(compiled) = compiled_for(kind) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&compiled.lang).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let names = compiled.query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&compiled.query, tree.root_node(), source);
    let mut raw: Vec<Raw> = Vec::new();
    while let Some(m) = matches.next() {
        let mut name: Option<&str> = None;
        let mut kind_sym: Option<OutlineKind> = None;
        let mut sel: Option<(u32, u32)> = None;
        let mut full: Option<(usize, usize, u32, u32)> = None;
        for cap in m.captures {
            let cap_name = names[cap.index as usize];
            let node = cap.node;
            if cap_name == "item" {
                full = Some((
                    node.start_byte(),
                    node.end_byte(),
                    node.start_position().row as u32,
                    node.end_position().row as u32,
                ));
            } else if let Some(suffix) = cap_name.strip_prefix("name.") {
                name = node.utf8_text(source).ok();
                let start = node.start_position();
                sel = Some((start.row as u32, start.column as u32));
                kind_sym = Some(kind_from_suffix(suffix));
            }
        }
        if let (Some(name), Some(kind), Some((line, character)), Some((sb, eb, rsl, rel))) =
            (name, kind_sym, sel, full)
        {
            raw.push(Raw {
                name: name.to_string(),
                kind,
                line,
                character,
                start_byte: sb,
                end_byte: eb,
                range_start_line: rsl,
                range_end_line: rel,
            });
        }
    }

    // Document order, parents before children: ascending start, then the wider
    // span first so an enclosing node precedes the symbols it contains.
    raw.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then(b.end_byte.cmp(&a.end_byte))
    });

    // Depth = number of still-open ancestors. A stack of ancestor end-bytes is
    // popped until its top still encloses the current symbol's start.
    let mut stack: Vec<usize> = Vec::new();
    let mut out = Vec::with_capacity(raw.len());
    for r in raw {
        while stack.last().is_some_and(|&end| r.start_byte >= end) {
            stack.pop();
        }
        let depth = stack.len() as u16;
        stack.push(r.end_byte);
        out.push(OutlineSymbol {
            name: r.name,
            detail: None,
            kind: r.kind,
            depth,
            line: r.line,
            character: r.character,
            range_start_line: r.range_start_line,
            range_end_line: r.range_end_line,
        });
    }
    out
}

/// A symbol before depth has been resolved; byte ranges drive the containment
/// pass that assigns nesting.
struct Raw {
    name: String,
    kind: OutlineKind,
    line: u32,
    character: u32,
    start_byte: usize,
    end_byte: usize,
    range_start_line: u32,
    range_end_line: u32,
}

struct Compiled {
    lang: Language,
    query: Query,
}

/// Compile (once) and cache the outline query for `kind`. `None` is cached too,
/// so an unsupported language is not re-probed every keystroke-batch.
fn compiled_for(kind: LangKind) -> Option<Arc<Compiled>> {
    static CACHE: OnceLock<Mutex<HashMap<LangKind, Option<Arc<Compiled>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap();
    if let Some(entry) = guard.get(&kind) {
        return entry.clone();
    }
    let (lang, src) = match lang_and_query(kind) {
        Some(pair) => pair,
        None => {
            guard.insert(kind, None);
            return None;
        }
    };
    let built = match Query::new(&lang, src) {
        Ok(query) => Some(Arc::new(Compiled { lang, query })),
        Err(e) => {
            crate::lsp::log_file::log(&format!(
                "outline_syntax: outline query for {kind:?} failed to compile: {e}"
            ));
            None
        }
    };
    guard.insert(kind, built.clone());
    built
}

fn lang_and_query(kind: LangKind) -> Option<(Language, &'static str)> {
    Some(match kind {
        LangKind::Rust => (tree_sitter_rust::LANGUAGE.into(), RUST_QUERY),
        LangKind::Python => (tree_sitter_python::LANGUAGE.into(), PYTHON_QUERY),
        LangKind::JavaScript => (tree_sitter_javascript::LANGUAGE.into(), JS_QUERY),
        LangKind::TypeScript => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), TS_QUERY),
        LangKind::Tsx => (tree_sitter_typescript::LANGUAGE_TSX.into(), TS_QUERY),
        LangKind::Go => (tree_sitter_go::LANGUAGE.into(), GO_QUERY),
        // Other languages either lack a useful symbol structure (json/toml/
        // yaml/css/html/bash) or are not yet covered; they fall through to the
        // LSP outline. Add a query + arm here to light one up.
        _ => return None,
    })
}

fn kind_from_suffix(suffix: &str) -> OutlineKind {
    match suffix {
        "function" => OutlineKind::Function,
        "method" => OutlineKind::Method,
        "struct" => OutlineKind::Struct,
        "enum" => OutlineKind::Enum,
        "enum_member" => OutlineKind::EnumMember,
        "interface" => OutlineKind::Interface,
        "class" => OutlineKind::Class,
        "constant" => OutlineKind::Constant,
        "field" => OutlineKind::Field,
        "module" => OutlineKind::Module,
        "constructor" => OutlineKind::Constructor,
        "property" => OutlineKind::Property,
        "variable" => OutlineKind::Variable,
        _ => OutlineKind::Field,
    }
}

const RUST_QUERY: &str = r#"
(function_item name: (identifier) @name.function) @item
(struct_item name: (type_identifier) @name.struct) @item
(union_item name: (type_identifier) @name.struct) @item
(enum_item name: (type_identifier) @name.enum) @item
(enum_variant name: (identifier) @name.enum_member) @item
(trait_item name: (type_identifier) @name.interface) @item
(type_item name: (type_identifier) @name.interface) @item
(mod_item name: (identifier) @name.module) @item
(const_item name: (identifier) @name.constant) @item
(static_item name: (identifier) @name.constant) @item
(macro_definition name: (identifier) @name.function) @item
(impl_item type: (type_identifier) @name.class) @item
(field_declaration name: (field_identifier) @name.field) @item
"#;

const PYTHON_QUERY: &str = r#"
(function_definition name: (identifier) @name.function) @item
(class_definition name: (identifier) @name.class) @item
"#;

const JS_QUERY: &str = r#"
(function_declaration name: (identifier) @name.function) @item
(generator_function_declaration name: (identifier) @name.function) @item
(class_declaration name: (identifier) @name.class) @item
(method_definition name: (property_identifier) @name.method) @item
"#;

const TS_QUERY: &str = r#"
(function_declaration name: (identifier) @name.function) @item
(class_declaration name: (type_identifier) @name.class) @item
(abstract_class_declaration name: (type_identifier) @name.class) @item
(method_definition name: (property_identifier) @name.method) @item
(interface_declaration name: (type_identifier) @name.interface) @item
(type_alias_declaration name: (type_identifier) @name.interface) @item
(enum_declaration name: (identifier) @name.enum) @item
"#;

const GO_QUERY: &str = r#"
(function_declaration name: (identifier) @name.function) @item
(method_declaration name: (field_identifier) @name.method) @item
(type_spec name: (type_identifier) @name.struct) @item
(const_spec name: (identifier) @name.constant) @item
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn names_kinds_depths(syms: &[OutlineSymbol]) -> Vec<(String, OutlineKind, u16)> {
        syms.iter()
            .map(|s| (s.name.clone(), s.kind, s.depth))
            .collect()
    }

    #[test]
    fn rust_methods_nest_under_their_impl() {
        let src = b"struct Foo { x: i32 }\n\nimpl Foo {\n    fn new() -> Self { Foo { x: 0 } }\n    fn get(&self) -> i32 { self.x }\n}\n\nfn free() {}\n";
        let syms = symbols_for(LangKind::Rust, src);
        let got = names_kinds_depths(&syms);
        // Document order: struct (and its field) first, then the impl with its
        // two methods nested, then the free function.
        assert_eq!(got[0], ("Foo".into(), OutlineKind::Struct, 0));
        assert_eq!(got[1], ("x".into(), OutlineKind::Field, 1));
        assert_eq!(got[2], ("Foo".into(), OutlineKind::Class, 0));
        assert_eq!(got[3], ("new".into(), OutlineKind::Function, 1));
        assert_eq!(got[4], ("get".into(), OutlineKind::Function, 1));
        assert_eq!(got[5], ("free".into(), OutlineKind::Function, 0));
    }

    #[test]
    fn rust_enum_variants_nest_under_enum() {
        let src = b"enum Color {\n    Red,\n    Green,\n}\n";
        let syms = symbols_for(LangKind::Rust, src);
        let got = names_kinds_depths(&syms);
        assert_eq!(got[0], ("Color".into(), OutlineKind::Enum, 0));
        assert_eq!(got[1], ("Red".into(), OutlineKind::EnumMember, 1));
        assert_eq!(got[2], ("Green".into(), OutlineKind::EnumMember, 1));
    }

    #[test]
    fn python_methods_nest_under_class() {
        let src = b"def top():\n    pass\n\nclass Greeter:\n    def __init__(self):\n        pass\n    def hi(self):\n        pass\n";
        let syms = symbols_for(LangKind::Python, src);
        let got = names_kinds_depths(&syms);
        assert_eq!(got[0], ("top".into(), OutlineKind::Function, 0));
        assert_eq!(got[1], ("Greeter".into(), OutlineKind::Class, 0));
        assert_eq!(got[2], ("__init__".into(), OutlineKind::Function, 1));
        assert_eq!(got[3], ("hi".into(), OutlineKind::Function, 1));
    }

    #[test]
    fn typescript_surfaces_interfaces_and_classes() {
        let src = b"interface Shape { area(): number }\n\nclass Circle {\n  radius(): number { return 1; }\n}\n";
        let syms = symbols_for(LangKind::TypeScript, src);
        let got = names_kinds_depths(&syms);
        assert!(got.contains(&("Shape".into(), OutlineKind::Interface, 0)));
        assert!(got.contains(&("Circle".into(), OutlineKind::Class, 0)));
        assert!(got.contains(&("radius".into(), OutlineKind::Method, 1)));
    }

    #[test]
    fn go_methods_and_types_are_extracted() {
        let src = b"package main\n\ntype Point struct {\n\tX int\n}\n\nfunc (p Point) Norm() int {\n\treturn p.X\n}\n\nfunc Free() {}\n";
        let syms = symbols_for(LangKind::Go, src);
        let got = names_kinds_depths(&syms);
        assert!(got.contains(&("Point".into(), OutlineKind::Struct, 0)));
        assert!(got.contains(&("Norm".into(), OutlineKind::Method, 0)));
        assert!(got.contains(&("Free".into(), OutlineKind::Function, 0)));
    }

    #[test]
    fn unsupported_language_returns_empty() {
        assert!(symbols_for(LangKind::Json, b"{\"a\": 1}").is_empty());
    }

    #[test]
    fn selection_point_targets_the_name_not_the_keyword() {
        let src = b"fn alpha() {}\n";
        let syms = symbols_for(LangKind::Rust, src);
        assert_eq!(syms[0].name, "alpha");
        // `fn ` is three columns; the name starts at column 3.
        assert_eq!((syms[0].line, syms[0].character), (0, 3));
    }
}
