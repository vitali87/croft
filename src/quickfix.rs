//! Turn a terminal `grep`/`rg` command into a croft Search.
//!
//! croft's Search sidebar already does find-in-files with multi-file
//! replace-all-on-disk. What it lacked was a way to *seed* it from a search
//! the user just ran in a terminal pane. `parse_search_command` reads the last
//! command's typed line (`rg -w "foo bar" src`, `grep -rn TODO`, `git grep -i
//! x`) and extracts the pattern plus the flags that map onto Search's toggles,
//! so the app can populate and run the Search panel — giving `:cdo`-style
//! "replace across every match" for free.
//
// ponytail: a heuristic arg parser, not a full clap model of every grep/rg
// flag. It covers the common invocations (pattern, `-i`/`-w`/`-F`/`-E`, `-e`,
// `-g`, value flags like `-C 3`); exotic combinations may misparse, but the
// seeded query is shown in the panel for the user to correct, never applied
// blindly. Upgrade to per-tool flag tables if that proves too coarse.

use std::path::{Path, PathBuf};

/// A terminal search command reduced to what the Search panel needs. The three
/// booleans map 1:1 onto `search::SearchOpts`; `include` onto the panel's
/// files-to-include glob. `paths`, `types` and `types_not` are the scope the
/// command searched, turned into globs by [`scope_filters`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCommand {
    pub pattern: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub include: Option<String>,
    pub exclude: Option<String>,
    /// The files and directories named after the pattern, as typed.
    pub paths: Vec<String>,
    /// rg `-t`/`--type` names.
    pub types: Vec<String>,
    /// rg `-T`/`--type-not` names.
    pub types_not: Vec<String>,
    /// `-v`/`--invert-match` or a files-without-match listing: the command
    /// printed what does NOT match the pattern.
    pub inverted: bool,
    /// Whether a named directory is searched through. rg, ag, ack and git
    /// grep always do; grep only with `-r`/`-R`/`--recursive`/`-d recurse`,
    /// and with no path at all it reads stdin.
    pub recursive: bool,
}

/// Consume `idx`'s token as a flag value (or take the inline `--flag=value`
/// form). Returns the value and advances `idx` past a consumed token.
fn take_value(inline: Option<String>, tokens: &[String], idx: &mut usize) -> Option<String> {
    if inline.is_some() {
        return inline;
    }
    let v = tokens.get(*idx).map(|s| s.to_string());
    if v.is_some() {
        *idx += 1;
    }
    v
}

/// Parse a terminal command line into a [`SearchCommand`], or `None` if it is
/// not a recognised search (`rg`/`ripgrep`/`grep`/`egrep`/`fgrep`/`git grep`/
/// `ag`/`ack`) or carries no pattern.
pub fn parse_search_command(cmdline: &str) -> Option<SearchCommand> {
    let tokens = shlex::split(cmdline)?;
    if tokens.is_empty() {
        return None;
    }
    let mut idx = 0;

    // Program name, path stripped (`/usr/bin/rg` -> `rg`). Sets the regex/case
    // defaults; flags below refine them.
    let prog_full = tokens[idx].as_str();
    let prog = prog_full.rsplit('/').next().unwrap_or(prog_full);
    idx += 1;
    // rg/ag/ack/egrep default to regex; plain grep's BRE is not Rust-regex, so
    // treat its pattern as literal (correct for the common "grep a string"),
    // upgraded by -E/-P. fgrep is always literal.
    let mut use_regex = match prog {
        "rg" | "ripgrep" | "ag" | "ack" | "egrep" => true,
        "grep" | "fgrep" => false,
        "git" => {
            if tokens.get(idx).map(|s| s.as_str()) != Some("grep") {
                return None;
            }
            idx += 1;
            false
        }
        _ => return None,
    };
    // `-s` means case-sensitive only in rg/ag; GNU grep and ack use it to
    // suppress error messages, and git grep has no such flag at all.
    let dash_s_is_case_sensitive = matches!(prog, "rg" | "ripgrep" | "ag");
    let mut case_sensitive = false; // rg smart-case & our default: case-insensitive
    let mut whole_word = false;
    let mut include: Option<String> = None;
    let mut exclude: Option<String> = None;
    let mut pattern: Option<String> = None;
    // A pattern given by `-e`/`--regexp` makes every positional a path.
    let mut positionals: Vec<String> = Vec::new();
    let mut types: Vec<String> = Vec::new();
    let mut types_not: Vec<String> = Vec::new();
    let mut inverted = false;
    let is_rg = matches!(prog, "rg" | "ripgrep");
    let mut recursive = !matches!(prog, "grep" | "egrep" | "fgrep");

    // Append a glob to one of the panel's comma-separated filter lists.
    fn push_glob(list: &mut Option<String>, glob: String) {
        match list {
            Some(s) => {
                s.push(',');
                s.push_str(&glob);
            }
            None => *list = Some(glob),
        }
    }
    // rg spells exclusion as a `!`-negated glob; the panel spells it as the
    // separate files-to-exclude list, which has no negation syntax.
    fn route_glob(inc: &mut Option<String>, exc: &mut Option<String>, glob: String) {
        match glob.strip_prefix('!') {
            Some(neg) => push_glob(exc, neg.to_string()),
            None => push_glob(inc, glob),
        }
    }

    // A standalone `--` ends option parsing: everything after it is
    // positional, so a dash-leading pattern (`rg -- -TODO`) stays a pattern.
    let mut flags_done = false;
    while idx < tokens.len() {
        let tok = tokens[idx].clone();
        idx += 1;
        if flags_done {
            positionals.push(tok);
            continue;
        }
        if tok == "--" {
            flags_done = true;
        } else if let Some(long) = tok.strip_prefix("--") {
            let (name, inline_val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "ignore-case" | "smart-case" => case_sensitive = false,
                "case-sensitive" => case_sensitive = true,
                "word-regexp" => whole_word = true,
                "invert-match" | "files-without-match" | "files-without-matches" => inverted = true,
                "fixed-strings" => use_regex = false,
                "recursive" | "dereference-recursive" => recursive = true,
                "directories" => {
                    if take_value(inline_val, &tokens, &mut idx).as_deref() == Some("recurse") {
                        recursive = true;
                    }
                }
                "extended-regexp" | "perl-regexp" => use_regex = true,
                "regexp" => {
                    let v = take_value(inline_val, &tokens, &mut idx);
                    if pattern.is_none() {
                        pattern = v;
                    }
                }
                "glob" | "iglob" => {
                    if let Some(v) = take_value(inline_val, &tokens, &mut idx) {
                        route_glob(&mut include, &mut exclude, v);
                    }
                }
                "include" => {
                    if let Some(v) = take_value(inline_val, &tokens, &mut idx) {
                        push_glob(&mut include, v);
                    }
                }
                // grep's exclusion flags: getopt_long takes the glob inline
                // (`--exclude=GLOB`) or spaced (`--exclude GLOB`).
                "exclude" | "exclude-dir" => {
                    if let Some(v) = take_value(inline_val, &tokens, &mut idx) {
                        push_glob(&mut exclude, v);
                    }
                }
                "type" | "type-not" if is_rg => {
                    if let Some(v) = take_value(inline_val, &tokens, &mut idx) {
                        if name == "type" {
                            types.push(v);
                        } else {
                            types_not.push(v);
                        }
                    }
                }
                // Long flags that take a value we don't use; drop the value so
                // it isn't mistaken for the pattern.
                "file" | "max-count" | "type" | "type-not" | "context" | "after-context"
                | "before-context" | "max-depth" | "threads" | "replace"
                    if inline_val.is_none() =>
                {
                    let _ = take_value(None, &tokens, &mut idx);
                }
                _ => {} // boolean long flag we don't care about
            }
        } else if tok.starts_with('-') && tok.len() > 1 {
            // Possibly-bundled short flags (`-rniw`, `-C3`, `-e pat`).
            let chars: Vec<char> = tok[1..].chars().collect();
            let mut ci = 0;
            while ci < chars.len() {
                match chars[ci] {
                    'i' | 'S' => case_sensitive = false,
                    's' if dash_s_is_case_sensitive => case_sensitive = true,
                    'w' => whole_word = true,
                    'v' => inverted = true,
                    // rg's `-L` follows symlinks; elsewhere it lists the
                    // files without a match.
                    'L' if !is_rg => inverted = true,
                    'F' => use_regex = false,
                    'r' | 'R' => recursive = true,
                    'd' => {
                        let rest: String = chars[ci + 1..].iter().collect();
                        let v = if rest.is_empty() {
                            take_value(None, &tokens, &mut idx)
                        } else {
                            Some(rest)
                        };
                        if v.as_deref() == Some("recurse") {
                            recursive = true;
                        }
                        break;
                    }
                    'E' | 'P' => use_regex = true,
                    'e' | 'g' => {
                        let rest: String = chars[ci + 1..].iter().collect();
                        let v = if rest.is_empty() {
                            take_value(None, &tokens, &mut idx)
                        } else {
                            Some(rest)
                        };
                        if chars[ci] == 'e' {
                            if pattern.is_none() {
                                pattern = v;
                            }
                        } else if let Some(v) = v {
                            route_glob(&mut include, &mut exclude, v);
                        }
                        break; // consumed the rest of this token
                    }
                    't' | 'T' if is_rg => {
                        let rest: String = chars[ci + 1..].iter().collect();
                        let v = if rest.is_empty() {
                            take_value(None, &tokens, &mut idx)
                        } else {
                            Some(rest)
                        };
                        if let Some(v) = v {
                            if chars[ci] == 't' {
                                types.push(v);
                            } else {
                                types_not.push(v);
                            }
                        }
                        break;
                    }
                    // Short flags that take a value we don't use.
                    'f' | 'm' | 'A' | 'B' | 'C' | 't' | 'T' => {
                        if chars[ci + 1..].is_empty() {
                            let _ = take_value(None, &tokens, &mut idx);
                        }
                        break;
                    }
                    _ => {} // boolean short flag (r, n, l, H, o, c, v, ...)
                }
                ci += 1;
            }
        } else {
            positionals.push(tok);
        }
    }

    // Without `-e`, the first positional is the pattern; the rest are the
    // files and directories searched.
    let mut positionals = positionals.into_iter();
    let pattern = match pattern {
        Some(p) => p,
        None => positionals.next()?,
    };
    let paths: Vec<String> = positionals.collect();
    if pattern.is_empty() {
        return None;
    }
    Some(SearchCommand {
        pattern,
        case_sensitive,
        whole_word,
        use_regex,
        include,
        exclude,
        paths,
        types,
        types_not,
        inverted,
        recursive,
    })
}

/// The globs of the common rg file types (`rg --type-list`), so `rg -t py`
/// seeds a search over the same files. A type missing here refuses the seed
/// rather than widening it to every file.
fn rg_type_globs(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "c" => &["*.[chH]", "*.[chH].in", "*.cats"],
        "cpp" => &[
            "*.[ChH]",
            "*.[ChH].in",
            "*.[ch]pp",
            "*.[ch]pp.in",
            "*.[ch]xx",
            "*.[ch]xx.in",
            "*.cc",
            "*.cc.in",
            "*.hh",
            "*.hh.in",
            "*.inl",
        ],
        "cs" | "csharp" => &["*.cs"],
        "css" => &["*.css", "*.scss"],
        "csv" => &["*.csv"],
        "dart" => &["*.dart"],
        "docker" => &["*Dockerfile*"],
        "elixir" => &["*.eex", "*.ex", "*.exs", "*.heex", "*.leex", "*.livemd"],
        "go" => &["*.go"],
        "html" => &["*.ejs", "*.htm", "*.html"],
        "java" => &["*.java", "*.jsp", "*.jspx", "*.properties"],
        "js" => &["*.cjs", "*.js", "*.jsx", "*.mjs", "*.vue"],
        "json" => &["*.json", "*.sarif", "composer.lock"],
        "kotlin" => &["*.kt", "*.kts"],
        "lua" => &["*.lua"],
        "make" => &[
            "*.mak",
            "*.mk",
            "[Gg][Nn][Uu]makefile",
            "[Gg][Nn][Uu]makefile.am",
            "[Gg][Nn][Uu]makefile.in",
            "[Mm]akefile",
            "[Mm]akefile.am",
            "[Mm]akefile.in",
        ],
        "md" | "markdown" => &[
            "*.markdown",
            "*.md",
            "*.mdown",
            "*.mdwn",
            "*.mdx",
            "*.mkd",
            "*.mkdn",
        ],
        "php" => &[
            "*.php", "*.php3", "*.php4", "*.php5", "*.php7", "*.php8", "*.pht", "*.phtml",
        ],
        "py" => &["*.py", "*.pyi"],
        "ruby" => &[
            "*.gemspec",
            "*.rb",
            "*.rbw",
            ".irbrc",
            "Gemfile",
            "Rakefile",
            "config.ru",
        ],
        "rust" => &["*.rs"],
        "scala" => &["*.sbt", "*.scala"],
        "sh" => &["*.bash", "*.csh", "*.ksh", "*.sh", "*.tcsh", "*.zsh"],
        "sql" => &["*.psql", "*.sql"],
        "swift" => &["*.swift"],
        "toml" => &["*.toml", "Cargo.lock"],
        "ts" => &["*.cts", "*.mts", "*.ts", "*.tsx"],
        "txt" => &["*.txt"],
        "xml" => &[
            "*.dtd",
            "*.rng",
            "*.sch",
            "*.xhtml",
            "*.xjb",
            "*.xml",
            "*.xml.dist",
            "*.xsd",
            "*.xsl",
            "*.xslt",
        ],
        "yaml" => &["*.yaml", "*.yml"],
        "zig" => &["*.zig"],
        _ => return None,
    })
}

/// The Search panel's (files-to-include, files-to-exclude) lists for `sc`,
/// covering exactly what the terminal command searched: its paths (resolved
/// against `cwd`, the pane shell's directory, or `cwd` itself when none was
/// named), its `-g`/`--include` globs and its rg `-t`/`-T` types. Scopes are
/// anchored to `root` with VS Code's `./` prefix. Anything that can't be
/// mapped exactly is an `Err` naming why: a Replace All seeded from a
/// terminal search must never cover more files than that search did (#1201).
pub fn scope_filters(
    sc: &SearchCommand,
    cwd: &Path,
    root: &Path,
) -> Result<(String, String), String> {
    if sc.inverted {
        return Err(String::from("the grep lists what does not match (-v / -L)"));
    }
    if !sc.recursive && sc.paths.is_empty() {
        return Err(String::from("the grep read stdin, not files (no -r)"));
    }
    let mut exclude: Vec<String> = sc
        .exclude
        .as_deref()
        .map(|e| {
            crate::widgets::search::split_globs(e)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    for t in &sc.types_not {
        let globs =
            rg_type_globs(t).ok_or_else(|| format!("croft doesn't know rg's type '{t}'"))?;
        exclude.extend(globs.iter().map(|g| g.to_string()));
    }
    let includes: Vec<String> = sc
        .include
        .as_deref()
        .map(|i| {
            crate::widgets::search::split_globs(i)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut type_globs: Vec<String> = Vec::new();
    for t in &sc.types {
        let globs =
            rg_type_globs(t).ok_or_else(|| format!("croft doesn't know rg's type '{t}'"))?;
        type_globs.extend(globs.iter().map(|g| g.to_string()));
    }
    if !includes.is_empty() && !type_globs.is_empty() {
        return Err(String::from("the grep combines -g globs with -t types"));
    }
    let names = if includes.is_empty() {
        type_globs
    } else {
        includes
    };

    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let named: Vec<PathBuf> = if sc.paths.is_empty() {
        vec![cwd.to_path_buf()]
    } else {
        sc.paths
            .iter()
            .map(|p| match p.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(rest))
                    .unwrap_or_else(|| PathBuf::from(p)),
                None => cwd.join(p),
            })
            .collect()
    };
    // (path relative to root, is a directory); an empty path is the root.
    let mut scopes: Vec<(String, bool)> = Vec::new();
    for (abs, typed) in named.iter().zip(
        sc.paths
            .iter()
            .map(String::as_str)
            .chain(std::iter::repeat(".")),
    ) {
        let canon = abs
            .canonicalize()
            .map_err(|_| format!("{typed} doesn't exist"))?;
        let rel = canon
            .strip_prefix(&root)
            .map_err(|_| format!("{typed} is outside the workspace"))?;
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if rel.contains(',') {
            return Err(format!("{typed} has a comma in its path"));
        }
        if !sc.recursive && canon.is_dir() {
            return Err(format!("grep without -r skips the directory {typed}"));
        }
        scopes.push((globset_escape(&rel), canon.is_dir()));
    }

    let include = if scopes.iter().any(|(rel, _)| rel.is_empty()) {
        // The whole workspace: only the name filters narrow it.
        names
    } else {
        let mut out = Vec::new();
        for (rel, is_dir) in &scopes {
            if !is_dir {
                // rg searches a file it is handed whatever its globs say.
                out.push(format!("./{rel}"));
            } else if names.is_empty() {
                out.push(format!("./{rel}/**"));
            } else {
                for g in &names {
                    if g.contains('/') {
                        return Err(format!(
                            "the grep's glob {g} has a / and it also names a path"
                        ));
                    }
                    out.push(format!("./{rel}/**/{g}"));
                }
            }
        }
        out
    };
    Ok((include.join(","), exclude.join(",")))
}

/// `s` with glob metacharacters escaped, so a file named `a[1].txt` matches
/// only itself.
fn globset_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '*' | '?' | '[' | ']' | '{' | '}' => {
                out.push('[');
                out.push(c);
                out.push(']');
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Option<SearchCommand> {
        parse_search_command(s)
    }

    #[test]
    fn plain_rg_is_a_regex_search() {
        let c = parse("rg foo").unwrap();
        assert_eq!(c.pattern, "foo");
        assert!(c.use_regex);
        assert!(!c.case_sensitive);
        assert!(!c.whole_word);
        assert_eq!(c.include, None);
    }

    #[test]
    fn short_flags_bundle_and_set_toggles() {
        let c = parse("rg -iw Bar").unwrap();
        assert_eq!(c.pattern, "Bar");
        assert!(c.whole_word);
        assert!(!c.case_sensitive);
    }

    #[test]
    fn fixed_strings_disables_regex_and_keeps_quoted_pattern() {
        let c = parse(r#"rg -F "a.b()""#).unwrap();
        assert_eq!(c.pattern, "a.b()");
        assert!(!c.use_regex);
    }

    #[test]
    fn plain_grep_pattern_is_literal_but_dash_e_upgrades() {
        assert!(!parse("grep -rn hello src/").unwrap().use_regex);
        assert!(parse("grep -E 'a|b' .").unwrap().use_regex);
    }

    #[test]
    fn grep_pattern_comes_before_the_path() {
        let c = parse("grep -rn hello src/").unwrap();
        assert_eq!(c.pattern, "hello");
    }

    #[test]
    fn value_flag_is_not_mistaken_for_pattern() {
        // -C 3 (context) must eat the 3, leaving `needle` as the pattern.
        assert_eq!(parse("rg -C 3 needle").unwrap().pattern, "needle");
        assert_eq!(parse("rg -C3 needle").unwrap().pattern, "needle");
        assert_eq!(
            parse("grep --max-count 5 needle .").unwrap().pattern,
            "needle"
        );
    }

    #[test]
    fn dash_e_supplies_the_pattern() {
        assert_eq!(parse("grep -rn -e pat").unwrap().pattern, "pat");
        assert_eq!(parse("rg --regexp=pat").unwrap().pattern, "pat");
    }

    #[test]
    fn glob_becomes_the_include_filter() {
        let c = parse("rg -g '*.rs' TODO").unwrap();
        assert_eq!(c.pattern, "TODO");
        assert_eq!(c.include.as_deref(), Some("*.rs"));
    }

    #[test]
    fn git_grep_is_recognised() {
        let c = parse("git grep -w thing").unwrap();
        assert_eq!(c.pattern, "thing");
        assert!(c.whole_word);
    }

    #[test]
    fn long_flags_map_to_toggles() {
        let c = parse("rg --ignore-case --word-regexp Foo").unwrap();
        assert_eq!(c.pattern, "Foo");
        assert!(c.whole_word);
        assert!(!c.case_sensitive);
    }

    #[test]
    fn non_search_commands_are_rejected() {
        assert!(parse("ls -la").is_none());
        assert!(parse("echo hi").is_none());
        assert!(parse("cargo build").is_none());
        assert!(parse("").is_none());
        assert!(parse("rg").is_none()); // no pattern
    }

    #[test]
    fn path_prefixed_program_is_recognised() {
        assert_eq!(parse("/usr/bin/rg needle").unwrap().pattern, "needle");
    }

    #[test]
    fn rg_dash_s_forces_case_sensitivity() {
        // `-s` is rg's explicit case-sensitive flag; dropping it would seed a
        // case-insensitive search whose Replace All rewrites identifiers the
        // terminal command never matched.
        assert!(parse("rg -s Error src/").unwrap().case_sensitive);
        assert!(parse("rg -sw Error").unwrap().case_sensitive);
        assert!(parse("rg --case-sensitive Error").unwrap().case_sensitive);
        // ag spells case-sensitive the same way.
        assert!(parse("ag -s Error").unwrap().case_sensitive);
    }

    #[test]
    fn grep_dash_s_is_not_case_sensitivity() {
        // In GNU grep (and ack) `-s` suppresses error messages; reading it as
        // rg's case-sensitive flag would seed a search that skips
        // differently-cased matches the terminal command actually found.
        assert!(!parse("grep -s Error .").unwrap().case_sensitive);
        assert!(!parse("grep -rs Error .").unwrap().case_sensitive);
        assert!(!parse("git grep -s Error").unwrap().case_sensitive);
    }

    #[test]
    fn negated_globs_become_the_exclude_filter() {
        // `rg -g '!node_modules'` is rg's exclude idiom. Copied verbatim into
        // the include filter it matches nothing (the panel's globs have no
        // negation), silently emptying the seeded search.
        let c = parse("rg -g '!node_modules' TODO").unwrap();
        assert_eq!(c.include, None);
        assert_eq!(c.exclude.as_deref(), Some("node_modules"));
    }

    #[test]
    fn multiple_globs_accumulate_into_the_comma_lists() {
        let c = parse("rg -g '*.rs' -g '*.md' -g '!target' TODO").unwrap();
        assert_eq!(c.include.as_deref(), Some("*.rs,*.md"));
        assert_eq!(c.exclude.as_deref(), Some("target"));
    }

    #[test]
    fn grep_exclude_flags_seed_the_exclude_filter() {
        let c = parse("grep -rn --exclude=*.min.js --exclude-dir=dist TODO .").unwrap();
        assert_eq!(c.pattern, "TODO");
        assert_eq!(c.exclude.as_deref(), Some("*.min.js,dist"));
    }

    #[test]
    fn end_of_options_marker_lets_a_dash_leading_pattern_through() {
        // `rg -- -TODO src` searches for the literal `-TODO`; without
        // honoring `--`, the parser ate `-TODO` as flags and seeded the
        // search with `src` — running replace against unintended text.
        let c = parse("rg -g '*.rs' -- -TODO src").unwrap();
        assert_eq!(c.pattern, "-TODO");
        assert_eq!(c.include.as_deref(), Some("*.rs"));
        assert_eq!(parse("grep -rn -- --help .").unwrap().pattern, "--help");
    }

    #[test]
    fn spaced_grep_exclusion_values_are_not_the_pattern() {
        // getopt_long takes required arguments spaced too: `--exclude GLOB`.
        // Not consuming the value made the glob the seeded search pattern.
        let c = parse("grep -r --exclude *.min.js needle .").unwrap();
        assert_eq!(c.pattern, "needle");
        assert_eq!(c.exclude.as_deref(), Some("*.min.js"));
        let c = parse("grep -r --exclude-dir dist TODO .").unwrap();
        assert_eq!(c.pattern, "TODO");
        assert_eq!(c.exclude.as_deref(), Some("dist"));
    }

    #[test]
    fn positionals_after_the_pattern_are_the_searched_paths() {
        let c = parse("rg -n color src/ docs/a.md").unwrap();
        assert_eq!(c.pattern, "color");
        assert_eq!(c.paths, ["src/", "docs/a.md"]);
        // With -e every positional is a path, the first one included.
        let c = parse("grep -rn -e color src").unwrap();
        assert_eq!(c.pattern, "color");
        assert_eq!(c.paths, ["src"]);
        let c = parse("rg -- -TODO src").unwrap();
        assert_eq!(c.paths, ["src"]);
    }

    #[test]
    fn rg_types_are_kept_and_other_tools_dash_t_is_not_a_type() {
        let c = parse("rg -tpy --type=rust -T md color").unwrap();
        assert_eq!(c.types, ["py", "rust"]);
        assert_eq!(c.types_not, ["md"]);
        // grep's -T is --initial-tab; it names no type.
        let c = parse("grep -rT color .").unwrap();
        assert!(c.types.is_empty() && c.types_not.is_empty());
    }

    #[test]
    fn inverted_listings_are_flagged_but_rg_dash_l_follows_links() {
        assert!(parse("rg -v color").unwrap().inverted);
        assert!(parse("grep --invert-match color .").unwrap().inverted);
        assert!(parse("grep -rL color .").unwrap().inverted);
        assert!(parse("rg --files-without-match color").unwrap().inverted);
        assert!(!parse("rg -L color").unwrap().inverted);
        assert!(!parse("rg -l color").unwrap().inverted);
    }

    #[test]
    fn scope_filters_anchor_dirs_files_and_name_globs_to_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/a[1]")).unwrap();
        std::fs::write(root.join("main.rs"), "").unwrap();
        let f = |cmd: &str, cwd: &Path| scope_filters(&parse(cmd).unwrap(), cwd, root);
        assert_eq!(f("rg x src", root).unwrap().0, "./src/**");
        assert_eq!(f("rg x main.rs", root).unwrap().0, "./main.rs");
        assert_eq!(f("rg -g '*.rs' x src", root).unwrap().0, "./src/**/*.rs");
        assert_eq!(f("rg x", &root.join("src")).unwrap().0, "./src/**");
        assert_eq!(
            f("rg x .", &root.join("src/a[1]")).unwrap().0,
            "./src/a[[]1[]]/**"
        );
        assert_eq!(
            f("rg -t rust -T md x", root).unwrap(),
            (
                "*.rs".into(),
                "*.markdown,*.md,*.mdown,*.mdwn,*.mdx,*.mkd,*.mkdn".into()
            )
        );
        assert!(f("rg -g '*.rs' -t py x", root).is_err());
        assert!(f("rg -g 'src/*.rs' x src", root).is_err());
    }

    /// grep without `-r` reads stdin when given no path and skips a named
    /// directory, so neither seeds a scope; a named file still does.
    #[test]
    fn a_grep_without_recursion_seeds_only_the_files_it_named() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/style.py"), "").unwrap();
        let f = |cmd: &str| scope_filters(&parse(cmd).unwrap(), root, root);
        assert!(f("grep color").unwrap_err().contains("stdin"));
        assert!(
            f("grep color src")
                .unwrap_err()
                .contains("skips the directory src")
        );
        assert_eq!(f("grep color src/style.py").unwrap().0, "./src/style.py");
        for cmd in [
            "grep -r color src",
            "grep -Rn color src",
            "grep --recursive color src",
            "grep -d recurse color src",
            "grep --directories=recurse color src",
            "egrep -rn color src",
        ] {
            assert_eq!(f(cmd).unwrap().0, "./src/**", "{cmd}");
        }
        // Negative: the tools that recurse on their own need no flag.
        for cmd in [
            "rg color src",
            "git grep color src",
            "ag color src",
            "ack color src",
        ] {
            assert_eq!(f(cmd).unwrap().0, "./src/**", "{cmd}");
        }
        assert_eq!(parse("grep -d skip color src").unwrap().pattern, "color");
    }
}
