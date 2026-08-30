//! What a VS Code extension corresponds to in croft (#352).
//!
//! croft cannot run VS Code extensions, but many of the popular ones name a
//! thing croft already has: a language server, a debug adapter, a test
//! runner, a theme, a viewer. `.vscode/extensions.json` and the user's
//! installed list (`code --list-extensions`) are the last part of a VS Code
//! profile, and "you have this, you do not need that, and this has no
//! equivalent" is a better answer than silence. The table is curated data;
//! the readers never download or execute anything.

use std::path::Path;

/// How a VS Code extension maps onto croft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// croft ships the equivalent; nothing to install.
    Builtin,
    /// A catalog entry croft can install on request (`croft` names its id).
    Installable,
    /// No equivalent. The `note` says what, if anything, is planned.
    None,
}

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    /// The marketplace id, `publisher.name`, lowercase.
    pub vscode: &'static str,
    /// The croft extension id, catalog id, or feature that covers it; empty
    /// for `Status::None`.
    pub croft: &'static str,
    pub status: Status,
    /// One line for the user: what covers it, or what does not.
    pub note: &'static str,
}

/// The curated table.
pub const TABLE: &[Mapping] = &[
    Mapping {
        vscode: "ms-python.python",
        croft: "lsp-python",
        status: Status::Builtin,
        note: "Python language support ships built in (pyright)",
    },
    Mapping {
        vscode: "ms-python.vscode-pylance",
        croft: "lsp-python",
        status: Status::Builtin,
        note: "pyright is the language server croft bundles",
    },
    Mapping {
        vscode: "ms-python.debugpy",
        croft: "dap-python",
        status: Status::Builtin,
        note: "debugpy is croft's bundled Python debug adapter",
    },
    Mapping {
        vscode: "ms-python.black-formatter",
        croft: "lsp-python",
        status: Status::Builtin,
        note: "formatting runs through the Python language server (format_on_save)",
    },
    Mapping {
        vscode: "ms-python.isort",
        croft: "lsp-python",
        status: Status::Builtin,
        note: "import sorting runs through the Python language server's code actions",
    },
    Mapping {
        vscode: "ms-toolsai.jupyter",
        croft: "notebook view",
        status: Status::Builtin,
        note: "notebooks render in place; running cells is tracked in #355",
    },
    Mapping {
        vscode: "rust-lang.rust-analyzer",
        croft: "lsp-rust",
        status: Status::Builtin,
        note: "rust-analyzer ships built in",
    },
    Mapping {
        vscode: "vadimcn.vscode-lldb",
        croft: "dap-lldb",
        status: Status::Builtin,
        note: "CodeLLDB's job is croft's bundled LLDB debug adapter",
    },
    Mapping {
        vscode: "swellaby.vscode-rust-test-adapter",
        croft: "test-cargo",
        status: Status::Builtin,
        note: "the Test Explorer runs cargo tests built in",
    },
    Mapping {
        vscode: "golang.go",
        croft: "lsp-go",
        status: Status::Builtin,
        note: "gopls ships built in; Go debugging is tracked in #264",
    },
    Mapping {
        vscode: "ms-vscode.cpptools",
        croft: "lsp-cpp",
        status: Status::Builtin,
        note: "clangd for C/C++ and LLDB for debugging ship built in",
    },
    Mapping {
        vscode: "ms-vscode.cpptools-extension-pack",
        croft: "lsp-cpp",
        status: Status::Builtin,
        note: "clangd and the LLDB adapter cover the pack",
    },
    Mapping {
        vscode: "llvm-vs-code-extensions.vscode-clangd",
        croft: "lsp-cpp",
        status: Status::Builtin,
        note: "clangd ships built in",
    },
    Mapping {
        vscode: "ms-vscode.cmake-tools",
        croft: "",
        status: Status::None,
        note: "no CMake integration; build through tasks.json or the terminal",
    },
    Mapping {
        vscode: "twxs.cmake",
        croft: "",
        status: Status::None,
        note: "no CMake syntax support yet",
    },
    Mapping {
        vscode: "ms-vscode.makefile-tools",
        croft: "",
        status: Status::None,
        note: "no Makefile integration; run make through tasks.json or the terminal",
    },
    Mapping {
        vscode: "ms-vscode.vscode-typescript-next",
        croft: "lsp-typescript",
        status: Status::Builtin,
        note: "typescript-language-server ships built in",
    },
    Mapping {
        vscode: "dbaeumer.vscode-eslint",
        croft: "",
        status: Status::None,
        note: "type diagnostics come from the TypeScript language server; eslint itself is not run",
    },
    Mapping {
        vscode: "esbenp.prettier-vscode",
        croft: "format_on_save",
        status: Status::Builtin,
        note: "formatting runs through each language server's formatter on save",
    },
    Mapping {
        vscode: "orta.vscode-jest",
        croft: "test-jest",
        status: Status::Builtin,
        note: "the Test Explorer runs Jest built in",
    },
    Mapping {
        vscode: "firsttris.vscode-jest-runner",
        croft: "test-jest",
        status: Status::Builtin,
        note: "the Test Explorer runs Jest built in",
    },
    Mapping {
        vscode: "vitest.explorer",
        croft: "test-vitest",
        status: Status::Builtin,
        note: "the Test Explorer runs Vitest built in",
    },
    Mapping {
        vscode: "ms-playwright.playwright",
        croft: "",
        status: Status::None,
        note: "no Playwright runner; run it from the terminal",
    },
    Mapping {
        vscode: "vue.volar",
        croft: "",
        status: Status::None,
        note: "no Vue language server bundled",
    },
    Mapping {
        vscode: "svelte.svelte-vscode",
        croft: "",
        status: Status::None,
        note: "no Svelte language server bundled",
    },
    Mapping {
        vscode: "bradlc.vscode-tailwindcss",
        croft: "",
        status: Status::None,
        note: "no Tailwind language server bundled",
    },
    Mapping {
        vscode: "dsznajder.es7-react-js-snippets",
        croft: "snippets",
        status: Status::Builtin,
        note: "croft has snippets; import-vscode brings yours across",
    },
    Mapping {
        vscode: "xabikos.javascriptsnippets",
        croft: "snippets",
        status: Status::Builtin,
        note: "croft has snippets; import-vscode brings yours across",
    },
    Mapping {
        vscode: "christian-kohler.path-intellisense",
        croft: "language servers",
        status: Status::Builtin,
        note: "path completion comes from the language servers",
    },
    Mapping {
        vscode: "redhat.vscode-yaml",
        croft: "lsp-yaml",
        status: Status::Builtin,
        note: "yaml-language-server ships built in",
    },
    Mapping {
        vscode: "tamasfe.even-better-toml",
        croft: "lsp-toml",
        status: Status::Builtin,
        note: "taplo ships built in",
    },
    Mapping {
        vscode: "bungcip.better-toml",
        croft: "lsp-toml",
        status: Status::Builtin,
        note: "taplo ships built in",
    },
    Mapping {
        vscode: "zainchen.json",
        croft: "lsp-json",
        status: Status::Builtin,
        note: "the JSON language server ships built in",
    },
    Mapping {
        vscode: "timonwong.shellcheck",
        croft: "lsp-bash",
        status: Status::Builtin,
        note: "bash-language-server runs shellcheck when it is installed",
    },
    Mapping {
        vscode: "foxundermoon.shell-format",
        croft: "lsp-bash",
        status: Status::Builtin,
        note: "bash-language-server formats through shfmt when it is installed",
    },
    Mapping {
        vscode: "mads-hartmann.bash-ide-vscode",
        croft: "lsp-bash",
        status: Status::Builtin,
        note: "bash-language-server ships built in",
    },
    Mapping {
        vscode: "ms-vscode.powershell",
        croft: "",
        status: Status::None,
        note: "no PowerShell support",
    },
    Mapping {
        vscode: "ms-dotnettools.csharp",
        croft: "",
        status: Status::None,
        note: "no C# language server bundled",
    },
    Mapping {
        vscode: "ms-dotnettools.csdevkit",
        croft: "",
        status: Status::None,
        note: "no C# language server bundled",
    },
    Mapping {
        vscode: "redhat.java",
        croft: "",
        status: Status::None,
        note: "no Java language server bundled",
    },
    Mapping {
        vscode: "vscjava.vscode-java-pack",
        croft: "",
        status: Status::None,
        note: "no Java language server bundled",
    },
    Mapping {
        vscode: "dart-code.dart-code",
        croft: "",
        status: Status::None,
        note: "no Dart support",
    },
    Mapping {
        vscode: "dart-code.flutter",
        croft: "",
        status: Status::None,
        note: "no Flutter support",
    },
    Mapping {
        vscode: "hashicorp.terraform",
        croft: "",
        status: Status::None,
        note: "no Terraform language server bundled",
    },
    Mapping {
        vscode: "ms-kubernetes-tools.vscode-kubernetes-tools",
        croft: "",
        status: Status::None,
        note: "no Kubernetes integration",
    },
    Mapping {
        vscode: "ms-azuretools.vscode-docker",
        croft: "",
        status: Status::None,
        note: "no Docker integration; the terminal is one keystroke away",
    },
    Mapping {
        vscode: "ms-vscode-remote.remote-ssh",
        croft: "croft remote",
        status: Status::Builtin,
        note: "croft remote opens a workspace over SSH with the same UI",
    },
    Mapping {
        vscode: "ms-vscode-remote.remote-containers",
        croft: "",
        status: Status::None,
        note: "no dev-container support",
    },
    Mapping {
        vscode: "ms-vscode-remote.remote-wsl",
        croft: "",
        status: Status::None,
        note: "no WSL integration; run croft inside WSL",
    },
    Mapping {
        vscode: "ms-vsliveshare.vsliveshare",
        croft: "croft pair",
        status: Status::Builtin,
        note: "croft pair shares a live session",
    },
    Mapping {
        vscode: "eamodio.gitlens",
        croft: "inline blame",
        status: Status::Builtin,
        note: "inline blame, the SCM view, and the Timeline ship built in",
    },
    Mapping {
        vscode: "donjayamanne.githistory",
        croft: "timeline",
        status: Status::Builtin,
        note: "the Timeline view shows a file's history",
    },
    Mapping {
        vscode: "mhutchie.git-graph",
        croft: "",
        status: Status::None,
        note: "no commit graph; the commit scrubber is tracked in #371",
    },
    Mapping {
        vscode: "github.vscode-pull-request-github",
        croft: "",
        status: Status::None,
        note: "PR review from the editor is tracked in #365 and #366",
    },
    Mapping {
        vscode: "github.copilot",
        croft: "navigator",
        status: Status::Builtin,
        note: "the Navigator (Cmd+I) covers code assistance; agent-aware panes are tracked in #344",
    },
    Mapping {
        vscode: "github.copilot-chat",
        croft: "navigator",
        status: Status::Builtin,
        note: "the Navigator (Cmd+I) is croft's chat",
    },
    Mapping {
        vscode: "continue.continue",
        croft: "navigator",
        status: Status::Builtin,
        note: "the Navigator (Cmd+I) covers it; agent-aware panes are tracked in #344",
    },
    Mapping {
        vscode: "saoudrizwan.claude-dev",
        croft: "navigator",
        status: Status::Builtin,
        note: "run Claude Code in a terminal pane; agent-aware panes are tracked in #344",
    },
    Mapping {
        vscode: "anthropic.claude-code",
        croft: "navigator",
        status: Status::Builtin,
        note: "run Claude Code in a terminal pane; agent-aware panes are tracked in #344",
    },
    Mapping {
        vscode: "tabnine.tabnine-vscode",
        croft: "",
        status: Status::None,
        note: "no inline completion provider beyond the language servers",
    },
    Mapping {
        vscode: "vscodevim.vim",
        croft: "vim",
        status: Status::Builtin,
        note: "Vim keybindings ship as a built-in extension",
    },
    Mapping {
        vscode: "github.github-vscode-theme",
        croft: "themes",
        status: Status::Builtin,
        note: "import the theme JSON with croft theme-import",
    },
    Mapping {
        vscode: "zhuangtongfa.material-theme",
        croft: "themes",
        status: Status::Builtin,
        note: "One Dark Pro ships built in; other variants import with croft theme-import",
    },
    Mapping {
        vscode: "dracula-theme.theme-dracula",
        croft: "themes",
        status: Status::Builtin,
        note: "Dracula ships built in",
    },
    Mapping {
        vscode: "enkia.tokyo-night",
        croft: "themes",
        status: Status::Builtin,
        note: "import the theme JSON with croft theme-import",
    },
    Mapping {
        vscode: "pkief.material-icon-theme",
        croft: "file icons",
        status: Status::Builtin,
        note: "file icons ship built in (Nerd Font glyphs)",
    },
    Mapping {
        vscode: "vscode-icons-team.vscode-icons",
        croft: "file icons",
        status: Status::Builtin,
        note: "file icons ship built in (Nerd Font glyphs)",
    },
    Mapping {
        vscode: "formulahendry.code-runner",
        croft: "tasks",
        status: Status::Builtin,
        note: "run through tasks.json, the terminal, or a runnable Markdown fence",
    },
    Mapping {
        vscode: "formulahendry.auto-rename-tag",
        croft: "lsp-html",
        status: Status::Builtin,
        note: "the HTML language server renames paired tags",
    },
    Mapping {
        vscode: "formulahendry.auto-close-tag",
        croft: "lsp-html",
        status: Status::Builtin,
        note: "the HTML language server closes tags",
    },
    Mapping {
        vscode: "streetsidesoftware.code-spell-checker",
        croft: "",
        status: Status::None,
        note: "no spell checker",
    },
    Mapping {
        vscode: "usernamehw.errorlens",
        croft: "PROBLEMS",
        status: Status::Builtin,
        note: "diagnostics paint inline and in PROBLEMS",
    },
    Mapping {
        vscode: "aaron-bond.better-comments",
        croft: "",
        status: Status::None,
        note: "no comment-keyword highlighting",
    },
    Mapping {
        vscode: "coenraads.bracket-pair-colorizer-2",
        croft: "bracket colours",
        status: Status::Builtin,
        note: "bracket pair colours ship built in (disable_bracket_colors)",
    },
    Mapping {
        vscode: "oderwat.indent-rainbow",
        croft: "indent guides",
        status: Status::Builtin,
        note: "indent guides ship built in (disable_indent_guides)",
    },
    Mapping {
        vscode: "naumovs.color-highlight",
        croft: "",
        status: Status::None,
        note: "no colour-swatch decorations",
    },
    Mapping {
        vscode: "ritwickdey.liveserver",
        croft: "",
        status: Status::None,
        note: "no live server; croft web is tracked in #378",
    },
    Mapping {
        vscode: "ms-vscode.live-server",
        croft: "",
        status: Status::None,
        note: "no live server; croft web is tracked in #378",
    },
    Mapping {
        vscode: "humao.rest-client",
        croft: "",
        status: Status::None,
        note: ".http files are tracked in #370",
    },
    Mapping {
        vscode: "yzhang.markdown-all-in-one",
        croft: "markdown preview",
        status: Status::Builtin,
        note: "the Markdown preview (Cmd+Shift+V) ships built in",
    },
    Mapping {
        vscode: "davidanson.vscode-markdownlint",
        croft: "",
        status: Status::None,
        note: "no Markdown linter",
    },
    Mapping {
        vscode: "bierner.markdown-mermaid",
        croft: "",
        status: Status::None,
        note: "the Markdown preview does not render Mermaid",
    },
    Mapping {
        vscode: "gruntfuggly.todo-tree",
        croft: "",
        status: Status::None,
        note: "no TODO tree; search (Cmd+Shift+F) finds them",
    },
    Mapping {
        vscode: "wayou.vscode-todo-highlight",
        croft: "",
        status: Status::None,
        note: "no TODO highlighting",
    },
    Mapping {
        vscode: "mechatroner.rainbow-csv",
        croft: "csv",
        status: Status::Builtin,
        note: "the CSV viewer ships built in",
    },
    Mapping {
        vscode: "tomoki1207.pdf",
        croft: "pdf",
        status: Status::Builtin,
        note: "the PDF viewer ships built in",
    },
    Mapping {
        vscode: "hediet.vscode-drawio",
        croft: "",
        status: Status::None,
        note: "no diagram editor",
    },
    Mapping {
        vscode: "mikestead.dotenv",
        croft: "core-languages",
        status: Status::Builtin,
        note: ".env files highlight through the core languages",
    },
    Mapping {
        vscode: "editorconfig.editorconfig",
        croft: "",
        status: Status::None,
        note: "no .editorconfig support",
    },
    Mapping {
        vscode: "sonarsource.sonarlint-vscode",
        croft: "",
        status: Status::None,
        note: "no SonarLint; diagnostics come from the language servers",
    },
    Mapping {
        vscode: "hbenl.vscode-test-explorer",
        croft: "test explorer",
        status: Status::Builtin,
        note: "the Test Explorer ships built in",
    },
];

/// The row for `id` (`publisher.name`, any case), if it has one.
pub fn lookup(id: &str) -> Option<&'static Mapping> {
    TABLE
        .iter()
        .find(|m| m.vscode.eq_ignore_ascii_case(id.trim()))
}

/// Extension ids a workspace recommends in `<root>/.vscode/extensions.json`
/// (JSONC: comments and trailing commas are allowed there), in file order.
/// A missing or unreadable file is no recommendations.
pub fn workspace_recommendations(root: &Path) -> Vec<String> {
    let path = root.join(".vscode").join("extensions.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&crate::tasks::strip_jsonc(&raw))
    else {
        return Vec::new();
    };
    doc["recommendations"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Extension ids VS Code reports installed, via `code --list-extensions`.
/// Best effort: no `code` on PATH, or a failure, is an empty list. Runs a
/// process, so callers invoke it on an explicit user action, not per frame.
pub fn installed_via_code() -> Vec<String> {
    let Ok(out) = std::process::Command::new("code")
        .arg("--list-extensions")
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('.'))
        .map(str::to_string)
        .collect()
}

/// One extension the user has or is recommended, with its croft row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub id: String,
    pub mapping: Option<&'static Mapping>,
}

impl Comparison {
    /// The status to show: an unknown id reads as no equivalent.
    pub fn status(&self) -> Status {
        self.mapping.map_or(Status::None, |m| m.status)
    }
}

/// Look every id up, dropping duplicates (case-insensitively), ordered
/// built-in first, then installable, then the rest, each group by id.
pub fn compare<I: IntoIterator<Item = String>>(ids: I) -> Vec<Comparison> {
    let mut seen = std::collections::BTreeSet::new();
    let mut rows: Vec<Comparison> = ids
        .into_iter()
        .filter_map(|id| {
            let id = id.trim().to_lowercase();
            (!id.is_empty() && seen.insert(id.clone())).then(|| Comparison {
                mapping: lookup(&id),
                id,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.status().cmp(&b.status()).then_with(|| a.id.cmp(&b.id)));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_covers_the_popular_extensions_with_unique_lowercase_ids() {
        assert!(TABLE.len() >= 50, "{} rows", TABLE.len());
        let mut seen = std::collections::BTreeSet::new();
        for m in TABLE {
            assert_eq!(
                m.vscode,
                m.vscode.to_lowercase(),
                "{} is not lowercase",
                m.vscode
            );
            assert!(m.vscode.contains('.'), "{} is not publisher.name", m.vscode);
            assert!(seen.insert(m.vscode), "{} appears twice", m.vscode);
            assert!(!m.note.is_empty(), "{} has no note", m.vscode);
            match m.status {
                Status::None => assert!(
                    m.croft.is_empty(),
                    "{} maps to nothing but names {}",
                    m.vscode,
                    m.croft
                ),
                _ => assert!(
                    !m.croft.is_empty(),
                    "{} has a status but no croft target",
                    m.vscode
                ),
            }
        }
    }

    /// The acceptance triple from #352, and that a built-in target is a real
    /// bundled extension id or a feature, never a typo.
    #[test]
    fn the_acceptance_examples_resolve() {
        assert_eq!(
            lookup("rust-lang.rust-analyzer").map(|m| (m.status, m.croft)),
            Some((Status::Builtin, "lsp-rust"))
        );
        assert_eq!(
            lookup("ms-python.python").map(|m| (m.status, m.croft)),
            Some((Status::Builtin, "lsp-python"))
        );
        assert_eq!(
            lookup("esbenp.prettier-vscode").map(|m| m.status),
            Some(Status::Builtin)
        );
        assert_eq!(
            lookup("Rust-Lang.Rust-Analyzer").map(|m| m.vscode),
            Some("rust-lang.rust-analyzer"),
            "lookup is case-insensitive"
        );
        assert_eq!(lookup("nobody.nothing"), None);
        // Every built-in target that looks like an extension id is one croft
        // bundles, so the panel's "you have this" is true.
        let bundled: Vec<String> =
            crate::lsp::manifest::summaries(crate::lsp::manifest::BUNDLED_MANIFESTS)
                .into_iter()
                .map(|s| s.id)
                .collect();
        for m in TABLE.iter().filter(|m| m.status == Status::Builtin) {
            if m.croft.starts_with("lsp-")
                || m.croft.starts_with("dap-")
                || m.croft.starts_with("test-")
            {
                assert!(
                    bundled.contains(&m.croft.to_string()),
                    "{} names {} which croft does not bundle: {bundled:?}",
                    m.vscode,
                    m.croft
                );
            }
        }
        // And every installable target is a catalog entry.
        for m in TABLE.iter().filter(|m| m.status == Status::Installable) {
            assert!(
                crate::mcp::catalog::is_catalog_entry(m.croft),
                "{} names {} which the catalog does not have",
                m.vscode,
                m.croft
            );
        }
    }

    #[test]
    fn workspace_recommendations_read_jsonc_and_tolerate_absence() {
        let dir = tempfile::tempdir().unwrap();
        assert!(workspace_recommendations(dir.path()).is_empty());
        std::fs::create_dir(dir.path().join(".vscode")).unwrap();
        std::fs::write(
            dir.path().join(".vscode").join("extensions.json"),
            "{\n  // See https://go.microsoft.com/fwlink/?LinkId=827846\n  \"recommendations\": [\n    \"rust-lang.rust-analyzer\",\n    \"ms-python.python\",\n  ],\n  \"unwantedRecommendations\": [\"ms-vscode.cpptools\"],\n}\n",
        )
        .unwrap();
        assert_eq!(
            workspace_recommendations(dir.path()),
            vec![
                "rust-lang.rust-analyzer".to_string(),
                "ms-python.python".to_string()
            ]
        );
    }

    #[test]
    fn compare_dedups_and_orders_builtin_then_installable_then_none() {
        let rows = compare(vec![
            "nobody.nothing".to_string(),
            "MS-Python.Python".to_string(),
            "rust-lang.rust-analyzer".to_string(),
            "ms-python.python".to_string(),
        ]);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "ms-python.python",
                "rust-lang.rust-analyzer",
                "nobody.nothing"
            ]
        );
        assert_eq!(rows[0].status(), Status::Builtin);
        assert_eq!(rows[2].status(), Status::None);
        assert!(rows[2].mapping.is_none());
    }
}
