//! Language and indentation settings: picking a buffer's language, detecting and setting
//! its indentation, whitespace display and rulers.

use super::*;

/// The buffer's indentation preference for newly typed indentation: the tab
/// width and whether Tab inserts spaces or a literal tab. Surfaced in the
/// status bar's "Spaces: N" / "Tab Size: N" pill, which the user can click to
/// change (VS Code's "Indent Using Spaces / Tabs" + "Change Tab Display Size").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndentStyle {
    pub width: u32,
    pub use_spaces: bool,
}

/// Detect the dominant indentation of a buffer's lines (VS Code's
/// `editor.detectIndentation`). Tabs vs spaces is a straight majority of
/// indented lines; the space width is the most common positive step
/// between consecutive indentation depths (2..=8), falling back to the
/// smallest leading run when the file never nests. `None` when nothing is
/// indented, so the caller keeps the language default. Scans at most the
/// first 10k lines: enough signal for any real file, bounded for huge ones.
pub fn detect_indentation(lines: &[String]) -> Option<IndentStyle> {
    let mut tab_lines = 0usize;
    let mut space_lines = 0usize;
    let mut steps: [usize; 9] = [0; 9];
    let mut prev_depth = 0usize;
    let mut min_run = usize::MAX;
    for line in lines.iter().take(10_000) {
        if line.starts_with('\t') {
            // Whitespace-only lines carry no intent, same as the space
            // branch below: a blank line with leftover tab indentation
            // must not vote.
            if line.chars().any(|c| !c.is_whitespace()) {
                tab_lines += 1;
            }
            continue;
        }
        let depth = line.chars().take_while(|&c| c == ' ').count();
        if depth == 0 {
            if !line.is_empty() {
                prev_depth = 0;
            }
            continue;
        }
        if line.chars().nth(depth).is_none() {
            // Whitespace-only lines carry no intent.
            continue;
        }
        space_lines += 1;
        min_run = min_run.min(depth);
        let step = depth.abs_diff(prev_depth);
        if (2..=8).contains(&step) {
            steps[step] += 1;
        }
        prev_depth = depth;
    }
    if tab_lines == 0 && space_lines == 0 {
        return None;
    }
    if tab_lines >= space_lines {
        return Some(IndentStyle {
            width: 4,
            use_spaces: false,
        });
    }
    let width = steps
        .iter()
        .enumerate()
        .skip(2)
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))
        .filter(|&(_, &n)| n > 0)
        .map(|(w, _)| w)
        .or(((2..=8).contains(&min_run)).then_some(min_run))?;
    Some(IndentStyle {
        width: width as u32,
        use_spaces: true,
    })
}

impl IndentStyle {
    /// The indentation unit as text: `width` spaces, or a single tab.
    pub fn unit(self) -> String {
        if self.use_spaces {
            " ".repeat(self.width as usize)
        } else {
            "\t".to_string()
        }
    }

    /// Status-bar label, matching VS Code ("Spaces: 4" / "Tab Size: 4").
    pub fn label(self) -> String {
        let kind = if self.use_spaces {
            "Spaces"
        } else {
            "Tab Size"
        };
        format!("{kind}: {}", self.width)
    }
}

impl Editor {
    /// The buffer's active indentation style, in precedence order: the
    /// status-bar override if set, then `.editorconfig`, then the style
    /// detected from the file's content on open, then the language default
    /// (2 spaces for YAML, 4 spaces otherwise).
    ///
    /// `.editorconfig` outranks detection because a project that STATES its
    /// style must beat a guess at it — including on a file whose current
    /// content is inconsistent with the rule, which is the case the property
    /// exists to correct. The two properties are resolved independently: a
    /// config naming only `indent_style` keeps the detected width.
    pub fn indent_style(&self) -> IndentStyle {
        if let Some(pinned) = self.indent_override {
            return pinned;
        }
        let base = self.detected_indent.unwrap_or(IndentStyle {
            width: indent_unit_for(self.lang).chars().count() as u32,
            use_spaces: true,
        });
        IndentStyle {
            width: self.editorconfig.indent_width.unwrap_or(base.width),
            use_spaces: self.editorconfig.use_spaces.unwrap_or(base.use_spaces),
        }
    }

    /// Pin the buffer's indentation style (status-bar "Indent Using …" /
    /// "Change Tab Display Size"). Affects newly typed indentation only; use
    /// [`Editor::convert_indentation`] to rewrite existing lines.
    pub fn set_indent_style(&mut self, style: IndentStyle) {
        self.indent_override = Some(style);
        self.hscroll_content_cols = None;
    }

    /// The indentation unit as text for the active style: N spaces or one tab.
    pub(super) fn indent_unit(&self) -> String {
        self.indent_style().unit()
    }

    /// The editor's indentation preference as the LSP `FormattingOptions`
    /// fields (`tab_size`, `insert_spaces`).
    pub fn indent_preference(&self) -> (u32, bool) {
        let s = self.indent_style();
        (s.width, s.use_spaces)
    }

    /// Human label for the buffer's language mode, for the status bar.
    pub fn language_label(&self) -> &'static str {
        if self.is_plain_log() {
            return "Log";
        }

        // SCSS, Less and Sass share the CSS grammar but are their own
        // languages to the reader and to the CSS server (#1532), so the
        // status bar names them the way VS Code does.
        if self.lang == Some(LangKind::Css) {
            let ext = self
                .path
                .as_deref()
                .and_then(|p| p.extension())
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            match ext.as_deref() {
                Some("scss") => return "SCSS",
                Some("less") => return "Less",
                Some("sass") => return "Sass",
                _ => {}
            }
        }
        language_label(self.lang)
    }

    /// The active language, for the status-bar "Change Language Mode" picker
    /// to pre-select the current entry.
    pub fn language(&self) -> Option<LangKind> {
        self.lang
    }

    /// Override the buffer's language mode (status-bar "Change Language Mode").
    /// Re-runs tree-sitter highlighting under the new grammar; the app re-syncs
    /// the LSP separately off the changed language.
    pub fn set_language(&mut self, lang: Option<LangKind>) {
        self.lang = lang;
        self.language_by_hand = false;
        self.recompute_highlights();
    }

    /// [`Self::set_language`] for a language the user picked (#1574): the
    /// tab keeps it when `files_associations` change, even when it is the
    /// language the file's name would give it anyway.
    pub fn pick_language(&mut self, lang: Option<LangKind>) {
        self.set_language(lang);
        self.language_by_hand = true;
    }

    /// Whether the language was picked by hand ([`Self::pick_language`]).
    pub fn language_picked_by_hand(&self) -> bool {
        self.language_by_hand
    }

    /// The language id (VS Code identifier) of this buffer, for snippet scoping.
    ///
    /// `.jsx` and `.jsonc` share a grammar with JavaScript and JSON but not
    /// VS Code's id (#1641): snippets imported from `javascriptreact.json`
    /// are scoped `javascriptreact`, and never applied while a `.jsx` file
    /// was called `javascript`.
    pub fn scope_id(&self) -> &'static str {
        let ext = self
            .path
            .as_deref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match (self.lang, ext.as_deref()) {
            (Some(LangKind::JavaScript), Some("jsx")) => "javascriptreact",
            (Some(LangKind::Json), Some("jsonc")) => "jsonc",
            _ => language_scope_id(self.lang),
        }
    }
}

/// Most rulers kept (#1524). A repository's `.croft/config.json` may set
/// `rulers`, and the painter walks them for every visible row, so the list
/// is bounded before it reaches an editor.
pub const MAX_RULERS: usize = 16;

/// The `rulers` setting as an editor keeps it: ascending, without
/// duplicates, and at most `MAX_RULERS` columns.
pub fn normalize_rulers(rulers: &[u16]) -> Vec<u16> {
    let mut out = rulers.to_vec();
    out.sort_unstable();
    out.dedup();
    out.truncate(MAX_RULERS);
    out
}

/// Whitespace rendering (#133, VS Code `editor.renderWhitespace`): which
/// spaces/tabs paint a visible glyph (`·` / `→`). `Selection` — VS Code's
/// default — shows them only inside selected text; `All` across the whole
/// buffer; `None` never. The palette command cycles Selection → All → None.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhitespaceMode {
    None,
    #[default]
    Selection,
    All,
}

impl WhitespaceMode {
    /// The persisted config id (`render_whitespace` in config.json).
    pub fn pref_id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selection => "selection",
            Self::All => "all",
        }
    }

    /// Parse the persisted id; anything unrecognised (including the empty
    /// string an older config deserializes to) is the VS Code default.
    pub fn from_pref(id: &str) -> Self {
        match id {
            "none" => Self::None,
            "all" => Self::All,
            _ => Self::Selection,
        }
    }

    /// Settings/status label.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selection => "selection",
            Self::All => "all",
        }
    }

    /// The palette toggle's cycle order (VS Code default first).
    pub fn next(self) -> Self {
        match self {
            Self::Selection => Self::All,
            Self::All => Self::None,
            Self::None => Self::Selection,
        }
    }
}

pub(super) fn indent_unit_for(lang: Option<LangKind>) -> &'static str {
    match lang {
        Some(LangKind::Yaml) => "  ",
        _ => "    ",
    }
}

/// The VS Code language id for `lang`, used to scope user snippets (their
/// `scope` field names these). Lowercase, matching VS Code's identifiers so a
/// config copied from there just works.
pub fn language_scope_id(lang: Option<LangKind>) -> &'static str {
    match lang {
        None => "plaintext",
        Some(LangKind::Rust) => "rust",
        Some(LangKind::Python) => "python",
        Some(LangKind::JavaScript) => "javascript",
        Some(LangKind::TypeScript) => "typescript",
        Some(LangKind::Tsx) => "typescriptreact",
        Some(LangKind::Json) => "json",
        Some(LangKind::Xml) => "xml",
        Some(LangKind::Toml) => "toml",
        Some(LangKind::Yaml) => "yaml",
        Some(LangKind::Markdown) => "markdown",
        Some(LangKind::Go) => "go",
        Some(LangKind::Html) => "html",
        Some(LangKind::Css) => "css",
        Some(LangKind::Bash) => "shellscript",
        Some(LangKind::C) => "c",
        Some(LangKind::Cpp) => "cpp",
        Some(LangKind::Lua) => "lua",
        Some(LangKind::Ql) => "ql",
        Some(LangKind::Dbscheme) => "dbscheme",
        Some(LangKind::Java) => "java",
        Some(LangKind::Ruby) => "ruby",
        Some(LangKind::CSharp) => "csharp",
        Some(LangKind::Sql) => "sql",
        Some(LangKind::Make) => "makefile",
        Some(LangKind::Dockerfile) => "dockerfile",
        Some(LangKind::Vue) => "vue",
        Some(LangKind::Svelte) => "svelte",
    }
}

/// Every selectable language mode, in the order the status-bar picker lists
/// them. `None` (Plain Text) is offered separately by the picker.
pub const SELECTABLE_LANGUAGES: &[LangKind] = &[
    LangKind::Rust,
    LangKind::Python,
    LangKind::JavaScript,
    LangKind::TypeScript,
    LangKind::Tsx,
    LangKind::Json,
    LangKind::Toml,
    LangKind::Yaml,
    LangKind::Markdown,
    LangKind::Go,
    LangKind::Html,
    LangKind::Css,
    LangKind::Bash,
    LangKind::C,
    LangKind::Cpp,
    LangKind::Lua,
    LangKind::Ql,
    LangKind::Dbscheme,
    LangKind::Java,
    LangKind::Ruby,
    LangKind::CSharp,
    LangKind::Sql,
    LangKind::Make,
    LangKind::Dockerfile,
    LangKind::Vue,
    LangKind::Svelte,
];
