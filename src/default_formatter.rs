//! VS Code's `editor.defaultFormatter` against the formatters croft runs
//! (#1499).
//!
//! A project that commits `editor.formatOnSave` almost always names its
//! formatter next to it: Biome, Prettier, Ruff. croft formats through the
//! file's language server, so when the project names a formatter croft
//! does not run, a save must not be formatted by something else: that
//! rewrites lines the project's own formatter laid out. VS Code does the
//! same and says the configured formatter is missing.

use std::collections::BTreeMap;

/// VS Code formatter extensions that croft's bundled language servers stand
/// in for, with the language ids each formats.
const SERVED: &[(&str, &[&str])] = &[
    (
        "vscode.typescript-language-features",
        &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
    ),
    ("rust-lang.rust-analyzer", &["rust"]),
    ("golang.go", &["go"]),
    ("charliermarsh.ruff", &["python"]),
    ("llvm-vs-code-extensions.vscode-clangd", &["c", "cpp"]),
    ("vscode.json-language-features", &["json", "jsonc"]),
    ("vscode.css-language-features", &["css", "scss", "less"]),
    ("vscode.html-language-features", &["html"]),
    ("redhat.vscode-yaml", &["yaml"]),
    ("tamasfe.even-better-toml", &["toml"]),
    ("sumneko.lua", &["lua"]),
    ("mads-hartmann.bash-ide-vscode", &["shellscript"]),
];

/// The formatter named for a file in language `lang`: its language's entry,
/// else the one for every file.
pub fn named<'a>(
    global: Option<&'a str>,
    per_language: &'a BTreeMap<String, String>,
    lang: Option<&str>,
) -> Option<&'a str> {
    lang.and_then(|l| per_language.get(l))
        .map(String::as_str)
        .or(global)
}

/// Whether croft's own language server for `lang` is the formatter
/// `formatter` names. Extension ids compare without case, as in VS Code.
pub fn croft_runs(formatter: &str, lang: Option<&str>) -> bool {
    let Some(lang) = lang else {
        return false;
    };
    SERVED
        .iter()
        .any(|(id, langs)| id.eq_ignore_ascii_case(formatter) && langs.contains(&lang))
}

/// The formatter a file in `lang` names that croft does not run, if any:
/// format on save leaves such a file as typed.
pub fn foreign<'a>(
    global: Option<&'a str>,
    per_language: &'a BTreeMap<String, String>,
    lang: Option<&str>,
) -> Option<&'a str> {
    named(global, per_language, lang).filter(|f| !croft_runs(f, lang))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn langs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(l, f)| (l.to_string(), f.to_string()))
            .collect()
    }

    /// #1499: zod names Biome for every file; croft runs no Biome, so a
    /// TypeScript save is not formatted by tsserver instead.
    #[test]
    fn a_formatter_croft_does_not_run_is_foreign() {
        let none = BTreeMap::new();
        assert_eq!(
            foreign(Some("biomejs.biome"), &none, Some("typescript")),
            Some("biomejs.biome")
        );
        assert_eq!(
            foreign(Some("esbenp.prettier-vscode"), &none, Some("javascript")),
            Some("esbenp.prettier-vscode")
        );
        let per = langs(&[("typescript", "biomejs.biome")]);
        assert_eq!(
            foreign(None, &per, Some("typescript")),
            Some("biomejs.biome")
        );
    }

    /// #1499, negative: no formatter named, or the one named is the
    /// language server croft already formats with, keeps format on save.
    #[test]
    fn no_formatter_or_croft_own_server_is_not_foreign() {
        let none = BTreeMap::new();
        assert_eq!(foreign(None, &none, Some("typescript")), None);
        assert_eq!(
            foreign(
                Some("vscode.typescript-language-features"),
                &none,
                Some("typescriptreact")
            ),
            None
        );
        assert_eq!(
            foreign(Some("Charliermarsh.Ruff"), &none, Some("python")),
            None,
            "extension ids ignore case"
        );
        assert_eq!(
            foreign(Some("charliermarsh.ruff"), &none, Some("rust")),
            Some("charliermarsh.ruff"),
            "ruff does not format Rust"
        );
    }

    /// #1499: a language's own entry wins over the one for every file,
    /// either way round.
    #[test]
    fn a_language_entry_wins_over_the_global_one() {
        let per = langs(&[
            ("python", "charliermarsh.ruff"),
            ("typescript", "esbenp.prettier-vscode"),
        ]);
        assert_eq!(foreign(Some("biomejs.biome"), &per, Some("python")), None);
        assert_eq!(
            foreign(Some("biomejs.biome"), &per, Some("go")),
            Some("biomejs.biome")
        );
        assert_eq!(
            foreign(
                Some("vscode.typescript-language-features"),
                &per,
                Some("typescript")
            ),
            Some("esbenp.prettier-vscode")
        );
        assert_eq!(named(None, &per, None), None);
    }
}
