use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
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

/// Base16-Ocean-Dark inspired palette, indexed by HIGHLIGHT_NAMES position.
fn style_for(idx: usize) -> Style {
    let name = HIGHLIGHT_NAMES.get(idx).copied().unwrap_or("");
    match name {
        "comment" => Style::default().fg(rgb(0x65, 0x73, 0x7e)).add_modifier(Modifier::ITALIC),
        "keyword" | "label" => Style::default().fg(rgb(0xb4, 0x8e, 0xad)).add_modifier(Modifier::BOLD),
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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
        _ => return None,
    })
}

fn build_config(kind: LangKind) -> Option<HighlightConfiguration> {
    let mut cfg = match kind {
        LangKind::Rust => HighlightConfiguration::new(
            tree_sitter_rust::LANGUAGE.into(),
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        )
        .ok()?,
        LangKind::Python => HighlightConfiguration::new(
            tree_sitter_python::LANGUAGE.into(),
            "python",
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            "",
        )
        .ok()?,
        LangKind::JavaScript => HighlightConfiguration::new(
            tree_sitter_javascript::LANGUAGE.into(),
            "javascript",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        )
        .ok()?,
        LangKind::TypeScript => HighlightConfiguration::new(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "typescript",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        )
        .ok()?,
        LangKind::Tsx => HighlightConfiguration::new(
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "tsx",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        )
        .ok()?,
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

pub struct LangRegistry {
    cache: HashMap<LangKind, HighlightConfiguration>,
}

impl LangRegistry {
    pub fn new() -> Self {
        Self { cache: HashMap::new() }
    }

    pub fn get(&mut self, kind: LangKind) -> Option<&HighlightConfiguration> {
        if !self.cache.contains_key(&kind) {
            if let Some(cfg) = build_config(kind) {
                self.cache.insert(kind, cfg);
            } else {
                return None;
            }
        }
        self.cache.get(&kind)
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
    let events = match hl.highlight(cfg, text, None, |_| None) {
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
        let line_end = line_starts.get(line_idx + 1).copied().unwrap_or(usize::MAX);
        let chunk_end = end.min(line_end);
        if chunk_end > start {
            per_line[line_idx].push(HiSpan {
                start: start - line_start,
                end: chunk_end - line_start,
                style,
            });
        }
        start = chunk_end;
        line_idx += 1;
    }
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
