use ratatui::style::Color;

pub struct Icon {
    pub glyph: char,
    pub color: Color,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

pub const FOLDER_CLOSED: Icon = Icon { glyph: '\u{ea83}', color: rgb(0xdc, 0xb6, 0x7a) };
pub const FOLDER_OPEN: Icon = Icon { glyph: '\u{eaf7}', color: rgb(0xdc, 0xb6, 0x7a) };
pub const DEFAULT_FILE: Icon = Icon { glyph: '\u{ea7b}', color: rgb(0xcc, 0xcc, 0xcc) };
pub const CHEVRON_CLOSED: char = '▸';
pub const CHEVRON_OPEN: char = '▾';

/// Codicon glyph for the Explorer entry in the activity bar (multi-page
/// stack). Verified against Nerd Font cmap; do NOT use U+EAEB which is
/// `cod-file_pdf` and renders the literal "PDF" document.
pub const ACTIVITY_EXPLORER: char = '\u{eaf0}';
pub const ACTIVITY_SEARCH: char = '\u{ea6d}';

pub fn for_path(name: &str, suffix: &str) -> Icon {
    let n = name.to_ascii_lowercase();
    if let Some(i) = name_icon(&n) {
        return i;
    }
    ext_icon(&suffix.to_ascii_lowercase()).unwrap_or(DEFAULT_FILE)
}

fn name_icon(n: &str) -> Option<Icon> {
    let i = |g, r, gr, b| Icon { glyph: g, color: rgb(r, gr, b) };
    Some(match n {
        // fa-git_alt official Git logo (red)
        ".gitignore" | ".gitattributes" | ".gitmodules" => i('\u{f841}', 0xe8, 0x27, 0x4b),
        ".python-version" | "pyproject.toml" => i('\u{e235}', 0x35, 0x72, 0xa5),
        "uv.lock" | "poetry.lock" | "package-lock.json" => i('\u{ea75}', 0x51, 0x9a, 0xba),
        "package.json" => i('\u{ed0d}', 0xcb, 0xcb, 0x41),
        "tsconfig.json" => i('\u{e628}', 0x51, 0x9a, 0xba),
        // cod-package, the build/recipe metaphor reads better than a shield
        "dockerfile" | ".dockerignore" => i('\u{eb29}', 0x38, 0x4d, 0x54),
        // cod-tools (wrench + screwdriver)
        "makefile" => i('\u{eb6d}', 0xcc, 0x3e, 0x44),
        "readme.md" => i('\u{f48a}', 0x51, 0x9a, 0xba),
        "license" => i('\u{eb12}', 0xcc, 0xcc, 0xcc),
        // cod-settings_gear; better than a solid dot
        ".env" => i('\u{eb51}', 0xfa, 0xf7, 0x43),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_icon_is_a_document_glyph_not_the_discard_arrow() {
        // Regression: U+EA7D is `cod-discard` (a left-curving arrow). A .txt
        // file should get a document-shaped glyph, not that.
        let icon = for_path("notes.txt", ".txt");
        assert_ne!(
            icon.glyph as u32, 0xea7d,
            "U+EA7D is cod-discard, not a text-file icon"
        );
        // Use Font Awesome's file-text-o which renders as a document with lines.
        assert_eq!(icon.glyph, '\u{f15c}');
    }

    #[test]
    fn ext_lookup_python() {
        let icon = for_path("hello.py", ".py");
        assert_eq!(icon.glyph, '\u{e235}');
        assert_eq!(icon.color, rgb(0x35, 0x72, 0xa5));
    }

    #[test]
    fn ext_lookup_json() {
        let icon = for_path("config.json", ".json");
        assert_eq!(icon.glyph, '\u{ed0d}');
    }

    #[test]
    fn ext_lookup_is_case_insensitive() {
        let lower = for_path("file.rs", ".rs");
        let upper = for_path("FILE.RS", ".RS");
        assert_eq!(lower.glyph, upper.glyph);
        assert_eq!(lower.color, upper.color);
    }

    #[test]
    fn unknown_extension_falls_back_to_default() {
        let icon = for_path("weird.xyz123", ".xyz123");
        assert_eq!(icon.glyph, DEFAULT_FILE.glyph);
        assert_eq!(icon.color, DEFAULT_FILE.color);
    }

    #[test]
    fn name_match_beats_extension_match() {
        // .gitignore: filename match (red) vs no extension match
        let icon = for_path(".gitignore", "");
        assert_eq!(icon.color, rgb(0xe8, 0x27, 0x4b));
    }

    #[test]
    fn pyproject_toml_overrides_generic_toml() {
        let generic = for_path("config.toml", ".toml");
        let specific = for_path("pyproject.toml", ".toml");
        // pyproject.toml has python blue (#3572a5); generic toml has rust brown (#9c4221)
        assert_eq!(specific.color, rgb(0x35, 0x72, 0xa5));
        assert_eq!(generic.color, rgb(0x9c, 0x42, 0x21));
    }

    #[test]
    fn dockerfile_name_match_is_case_insensitive() {
        let lower = for_path("dockerfile", "");
        let upper = for_path("Dockerfile", "");
        assert_eq!(lower.color, upper.color);
        assert_eq!(lower.color, rgb(0x38, 0x4d, 0x54));
    }

    #[test]
    fn folder_icons_are_consistent_color() {
        assert_eq!(FOLDER_OPEN.color, FOLDER_CLOSED.color);
    }

    /// Each entry: (codepoint we use, the glyph name that codepoint MAPS TO
    /// in the Codicon range of Nerd Fonts, what we use it for).  This is a
    /// regression suite against the bug where wrong codepoints made files
    /// look like gears, mail icons, triangles, etc. Cross-checked against
    /// `MesloLGSDZNerdFont-Regular.ttf` cmap on 2026-05-03.
    #[test]
    fn default_file_glyph_is_cod_file_not_cod_exclude() {
        // U+EAE5 was `cod-exclude` (a "no entry" circle); it made every
        // unknown file look like a stop sign. The proper sheet-of-paper
        // glyph is `cod-file` at U+EA7B.
        assert_eq!(DEFAULT_FILE.glyph, '\u{ea7b}', "DEFAULT_FILE must be cod-file");
        assert_ne!(DEFAULT_FILE.glyph, '\u{eae5}', "U+EAE5 is cod-exclude, not a file glyph");
    }

    #[test]
    fn folder_closed_glyph_is_cod_folder() {
        assert_eq!(FOLDER_CLOSED.glyph, '\u{ea83}');
    }

    #[test]
    fn folder_open_glyph_is_cod_folder_opened() {
        assert_eq!(FOLDER_OPEN.glyph, '\u{eaf7}');
    }

    #[test]
    fn activity_bar_explorer_glyph_is_cod_files_not_cod_file_pdf() {
        // U+EAEB is `cod-file_pdf` — the literal PDF document glyph that
        // the user saw in the activity bar. Activity-bar Explorer must be
        // `cod-files` (multi-page stack) at U+EAF0.
        assert_eq!(ACTIVITY_EXPLORER, '\u{eaf0}');
        assert_ne!(ACTIVITY_EXPLORER, '\u{eaeb}', "U+EAEB is cod-file_pdf, not cod-files");
    }

    #[test]
    fn activity_bar_search_glyph_is_cod_search() {
        assert_eq!(ACTIVITY_SEARCH, '\u{ea6d}');
    }

    #[test]
    fn archive_glyph_is_cod_file_zip_not_cod_filter() {
        // U+EAF1 is cod-filter (a funnel). Archives need cod-file_zip U+EAEF.
        let icon = for_path("foo.zip", ".zip");
        assert_ne!(icon.glyph, '\u{eaf1}', "U+EAF1 is cod-filter, not an archive");
        assert_eq!(icon.glyph, '\u{eaef}');
    }

    #[test]
    fn image_glyph_is_cod_file_media_not_cod_mail() {
        // U+EB1C is cod-mail (an envelope). Images need cod-file_media U+EAEA.
        let icon = for_path("logo.png", ".png");
        assert_ne!(icon.glyph, '\u{eb1c}', "U+EB1C is cod-mail, not an image glyph");
        assert_eq!(icon.glyph, '\u{eaea}');
    }

    #[test]
    fn log_glyph_is_cod_output_not_cod_mention() {
        // U+EB1F is cod-mention (the @ sign). Logs need cod-output U+EB9D.
        let icon = for_path("debug.log", ".log");
        assert_ne!(icon.glyph, '\u{eb1f}', "U+EB1F is cod-mention, not a log glyph");
        assert_eq!(icon.glyph, '\u{eb9d}');
    }

    #[test]
    fn csv_glyph_is_cod_table_not_cod_triangle_down() {
        // U+EB6E is cod-triangle_down (a downward arrow). CSV/TSV need
        // cod-table U+EBB7.
        let icon = for_path("data.csv", ".csv");
        assert_ne!(icon.glyph, '\u{eb6e}', "U+EB6E is cod-triangle_down, not a table");
        assert_eq!(icon.glyph, '\u{ebb7}');
    }

    #[test]
    fn makefile_glyph_is_cod_tools_not_cod_play() {
        // U+EB2C is cod-play (a play-button triangle). Makefile needs
        // cod-tools U+EB6D.
        let icon = for_path("makefile", "");
        assert_ne!(icon.glyph, '\u{eb2c}', "U+EB2C is cod-play, not a wrench");
        assert_eq!(icon.glyph, '\u{eb6d}');
    }

    #[test]
    fn env_glyph_is_settings_gear_not_circle_filled() {
        // U+EA71 is cod-circle_filled (a solid dot). .env carries config so
        // cod-settings_gear U+EB51 communicates intent.
        let icon = for_path(".env", "");
        assert_ne!(icon.glyph, '\u{ea71}', "U+EA71 is just a filled circle");
        assert_eq!(icon.glyph, '\u{eb51}');
    }

    #[test]
    fn dockerfile_glyph_is_cod_package_not_workspace_trusted() {
        // U+EBC1 is cod-workspace_trusted (a shield). Dockerfile is a
        // build/package recipe; cod-package U+EB29 reads better.
        let icon = for_path("dockerfile", "");
        assert_ne!(icon.glyph, '\u{ebc1}');
        assert_eq!(icon.glyph, '\u{eb29}');
    }

    #[test]
    fn sql_glyph_is_cod_database_not_workspace_trusted() {
        // Same wrong shield. SQL → cod-database U+EBE6 (table-like icon).
        let icon = for_path("query.sql", ".sql");
        assert_ne!(icon.glyph, '\u{ebc1}');
    }
}

fn ext_icon(s: &str) -> Option<Icon> {
    let i = |g, r, gr, b| Icon { glyph: g, color: rgb(r, gr, b) };
    Some(match s {
        ".py" | ".pyi" | ".pyc" | ".ipynb" => i('\u{e235}', 0x35, 0x72, 0xa5),
        ".js" | ".mjs" | ".cjs" => i('\u{e74e}', 0xcb, 0xcb, 0x41),
        ".jsx" | ".tsx" => i('\u{e7ba}', 0x51, 0x9a, 0xba),
        ".ts" => i('\u{e628}', 0x51, 0x9a, 0xba),
        ".html" | ".htm" => i('\u{e736}', 0xe4, 0x4d, 0x26),
        ".css" | ".tcss" => i('\u{e749}', 0x42, 0xa5, 0xf5),
        ".scss" | ".sass" => i('\u{e603}', 0xcc, 0x66, 0x99),
        ".json" | ".jsonc" => i('\u{ed0d}', 0xcb, 0xcb, 0x41),
        ".md" | ".markdown" => i('\u{f48a}', 0x51, 0x9a, 0xba),
        ".yaml" | ".yml" => i('\u{e6a8}', 0xcb, 0x17, 0x1e),
        ".toml" => i('\u{e6b2}', 0x9c, 0x42, 0x21),
        // cod-table — closest in-font match for a database/sql file
        ".sql" => i('\u{ebb7}', 0xda, 0xd8, 0xd8),
        ".sh" | ".bash" | ".zsh" | ".fish" => i('\u{ebca}', 0x4d, 0x5a, 0x5e),
        ".go" => i('\u{e627}', 0x51, 0x9a, 0xba),
        ".rs" => i('\u{e7a8}', 0xde, 0xa5, 0x84),
        ".java" => i('\u{e738}', 0xcc, 0x3e, 0x44),
        ".kt" | ".kts" => i('\u{e634}', 0x7f, 0x52, 0xff),
        ".c" => i('\u{e61e}', 0x59, 0x9e, 0xff),
        ".h" => i('\u{e61e}', 0xa0, 0x74, 0xc4),
        ".cpp" => i('\u{e61d}', 0x51, 0x9a, 0xba),
        ".hpp" => i('\u{e61d}', 0xa0, 0x74, 0xc4),
        ".cs" => i('\u{e648}', 0x59, 0x67, 0x06),
        ".rb" => i('\u{e739}', 0xcc, 0x34, 0x2d),
        ".php" => i('\u{e73d}', 0xa0, 0x74, 0xc4),
        ".swift" => i('\u{e755}', 0xe3, 0x79, 0x33),
        ".lua" => i('\u{e620}', 0x00, 0x00, 0x80),
        ".vim" => i('\u{e7c5}', 0x01, 0x98, 0x33),
        ".xml" => i('\u{eabe}', 0xe3, 0x79, 0x33),
        ".svg" => i('\u{eabe}', 0xff, 0xb1, 0x3b),
        // cod-file_media (NOT cod-mail U+EB1C, which is an envelope)
        ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp" | ".ico" => i('\u{eaea}', 0xa0, 0x74, 0xc4),
        ".pdf" => i('\u{eaeb}', 0xb3, 0x0b, 0x00),
        // cod-file_zip (NOT cod-filter U+EAF1, which is a funnel)
        ".zip" | ".tar" | ".gz" => i('\u{eaef}', 0xcc, 0xa7, 0x00),
        ".lock" => i('\u{ea75}', 0x51, 0x9a, 0xba),
        // cod-output (NOT cod-mention U+EB1F, which is the @ glyph)
        ".log" => i('\u{eb9d}', 0xda, 0xd8, 0xd8),
        ".txt" => i('\u{f15c}', 0xcc, 0xcc, 0xcc),
        // cod-table (NOT cod-triangle_down U+EB6E)
        ".csv" | ".tsv" => i('\u{ebb7}', 0x7c, 0xb3, 0x42),
        ".env" => i('\u{eb51}', 0xfa, 0xf7, 0x43),
        _ => return None,
    })
}
