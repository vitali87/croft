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
pub const DEFAULT_FILE: Icon = Icon { glyph: '\u{eae5}', color: rgb(0xcc, 0xcc, 0xcc) };
pub const CHEVRON_CLOSED: char = '▸';
pub const CHEVRON_OPEN: char = '▾';

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
        ".gitignore" | ".gitattributes" | ".gitmodules" => i('\u{efce}', 0xe8, 0x27, 0x4b),
        ".python-version" | "pyproject.toml" => i('\u{e235}', 0x35, 0x72, 0xa5),
        "uv.lock" | "poetry.lock" | "package-lock.json" => i('\u{ea75}', 0x51, 0x9a, 0xba),
        "package.json" => i('\u{ed0d}', 0xcb, 0xcb, 0x41),
        "tsconfig.json" => i('\u{ed0d}', 0x51, 0x9a, 0xba),
        "dockerfile" | ".dockerignore" => i('\u{ebc1}', 0x38, 0x4d, 0x54),
        "makefile" => i('\u{eb2c}', 0xcc, 0x3e, 0x44),
        "readme.md" => i('\u{f48a}', 0x51, 0x9a, 0xba),
        "license" => i('\u{eb12}', 0xcc, 0xcc, 0xcc),
        ".env" => i('\u{ea71}', 0xfa, 0xf7, 0x43),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ".sql" => i('\u{ebc1}', 0xda, 0xd8, 0xd8),
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
        ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp" | ".ico" => i('\u{eb1c}', 0xa0, 0x74, 0xc4),
        ".pdf" => i('\u{eaeb}', 0xb3, 0x0b, 0x00),
        ".zip" | ".tar" | ".gz" => i('\u{eaf1}', 0xcc, 0xa7, 0x00),
        ".lock" => i('\u{ea75}', 0x51, 0x9a, 0xba),
        ".log" => i('\u{eb1f}', 0xda, 0xd8, 0xd8),
        ".txt" => i('\u{ea7d}', 0xcc, 0xcc, 0xcc),
        ".csv" | ".tsv" => i('\u{eb6e}', 0x7c, 0xb3, 0x42),
        ".env" => i('\u{ea71}', 0xfa, 0xf7, 0x43),
        _ => return None,
    })
}
