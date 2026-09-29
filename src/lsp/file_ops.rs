//! Which files a server wants to hear about when they are renamed (#610).
//!
//! A server that advertises `workspace.fileOperations.willRename` also says
//! which paths it cares about, as a list of glob filters. croft must honour
//! them: asking a TypeScript server about a renamed `.py` file wastes a round
//! trip at best, and at worst hands back an edit for files it was never
//! meant to touch. So the filters are compiled once at spawn and checked per
//! rename, before any request goes out.
//!
//! Glob semantics follow the LSP spec: `*` stays within one path segment,
//! `**` crosses segments, `{a,b}` and `[...]` work, and `ignoreCase` is
//! per-pattern. A filter can also be limited to files or folders.

use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};
use lsp_types::{FileOperationPatternKind, FileOperationRegistrationOptions};

/// One compiled filter.
#[derive(Debug, Clone)]
struct Filter {
    glob: GlobMatcher,
    kind: Option<FileOperationPatternKind>,
}

/// A server's compiled `willRename` / `didRename` filters.
#[derive(Debug, Clone, Default)]
pub struct FileOpFilters {
    filters: Vec<Filter>,
}

impl FileOpFilters {
    /// Compile a server's registration. Filters with a non-`file` scheme
    /// never match a local path, and a glob that does not compile is
    /// dropped rather than treated as "match everything".
    pub fn from_registration(reg: &FileOperationRegistrationOptions) -> Self {
        let filters = reg
            .filters
            .iter()
            .filter(|f| f.scheme.as_deref().is_none_or(|s| s == "file"))
            .filter_map(|f| {
                let ignore_case = f
                    .pattern
                    .options
                    .as_ref()
                    .and_then(|o| o.ignore_case)
                    .unwrap_or(false);
                let glob = GlobBuilder::new(&f.pattern.glob)
                    .literal_separator(true)
                    .case_insensitive(ignore_case)
                    .build()
                    .ok()?
                    .compile_matcher();
                Some(Filter {
                    glob,
                    kind: f.pattern.matches.clone(),
                })
            })
            .collect();
        FileOpFilters { filters }
    }

    /// Whether any filter accepts `path` (a folder when `is_dir`).
    pub fn matches(&self, path: &Path, is_dir: bool) -> bool {
        self.filters.iter().any(|f| {
            let kind_ok = match f.kind {
                Some(FileOperationPatternKind::File) => !is_dir,
                Some(FileOperationPatternKind::Folder) => is_dir,
                None => true,
            };
            kind_ok && f.glob.is_match(path)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{FileOperationFilter, FileOperationPattern, FileOperationPatternOptions};

    fn filters(specs: &[(&str, Option<FileOperationPatternKind>, bool)]) -> FileOpFilters {
        FileOpFilters::from_registration(&FileOperationRegistrationOptions {
            filters: specs
                .iter()
                .map(|(glob, kind, ignore_case)| FileOperationFilter {
                    scheme: Some(String::from("file")),
                    pattern: FileOperationPattern {
                        glob: glob.to_string(),
                        matches: kind.clone(),
                        options: Some(FileOperationPatternOptions {
                            ignore_case: Some(*ignore_case),
                        }),
                    },
                })
                .collect(),
        })
    }

    #[test]
    fn a_double_star_glob_matches_at_any_depth() {
        let f = filters(&[("**/*.{ts,tsx}", None, false)]);
        assert!(f.matches(Path::new("/p/src/a/b.ts"), false));
        assert!(f.matches(Path::new("/p/c.tsx"), false));
        assert!(!f.matches(Path::new("/p/src/b.py"), false));
    }

    #[test]
    fn a_single_star_stays_within_one_segment() {
        let f = filters(&[("/p/*.ts", None, false)]);
        assert!(f.matches(Path::new("/p/a.ts"), false));
        assert!(!f.matches(Path::new("/p/src/a.ts"), false));
    }

    #[test]
    fn a_kind_limits_the_filter_to_files_or_folders() {
        let f = filters(&[("**", Some(FileOperationPatternKind::Folder), false)]);
        assert!(f.matches(Path::new("/p/src"), true));
        assert!(!f.matches(Path::new("/p/src/a.ts"), false));
    }

    #[test]
    fn ignore_case_is_per_pattern() {
        let f = filters(&[("**/*.RS", None, true)]);
        assert!(f.matches(Path::new("/p/main.rs"), false));
        let strict = filters(&[("**/*.RS", None, false)]);
        assert!(!strict.matches(Path::new("/p/main.rs"), false));
    }

    #[test]
    fn a_foreign_scheme_never_matches_a_local_path() {
        let f = FileOpFilters::from_registration(&FileOperationRegistrationOptions {
            filters: vec![FileOperationFilter {
                scheme: Some(String::from("untitled")),
                pattern: FileOperationPattern {
                    glob: String::from("**"),
                    matches: None,
                    options: None,
                },
            }],
        });
        assert!(!f.matches(Path::new("/p/a.ts"), false));
    }
}
