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

/// A terminal search command reduced to what the Search panel needs. The three
/// booleans map 1:1 onto `search::SearchOpts`; `include` onto the panel's
/// files-to-include glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCommand {
    pub pattern: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub include: Option<String>,
    pub exclude: Option<String>,
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
    let mut case_sensitive = false; // rg smart-case & our default: case-insensitive
    let mut whole_word = false;
    let mut include: Option<String> = None;
    let mut exclude: Option<String> = None;
    let mut pattern: Option<String> = None;

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

    while idx < tokens.len() {
        let tok = tokens[idx].clone();
        idx += 1;
        if let Some(long) = tok.strip_prefix("--") {
            let (name, inline_val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "ignore-case" | "smart-case" => case_sensitive = false,
                "case-sensitive" => case_sensitive = true,
                "word-regexp" => whole_word = true,
                "fixed-strings" => use_regex = false,
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
                // grep's exclusion flags take `=GLOB` inline only; a bare
                // `--exclude` is a grep usage error, so don't eat a token.
                "exclude" | "exclude-dir" => {
                    if let Some(v) = inline_val {
                        push_glob(&mut exclude, v);
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
                    's' => case_sensitive = true,
                    'w' => whole_word = true,
                    'F' => use_regex = false,
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
                    // Short flags that take a value we don't use.
                    'f' | 'm' | 'A' | 'B' | 'C' | 't' | 'T' | 'd' => {
                        if chars[ci + 1..].is_empty() {
                            let _ = take_value(None, &tokens, &mut idx);
                        }
                        break;
                    }
                    _ => {} // boolean short flag (r, n, l, H, o, c, v, ...)
                }
                ci += 1;
            }
        } else if pattern.is_none() {
            // First positional token is the pattern; later positionals are
            // paths/files we ignore.
            pattern = Some(tok);
        }
    }

    let pattern = pattern?;
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
    })
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
}
