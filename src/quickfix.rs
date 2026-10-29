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
// flag. It covers the common invocations (pattern, `-i`/`-w`/`-x`/`-F`/`-E`,
// `-e`, `-g`, value flags like `-C 3`); exotic combinations may misparse, but
// the seeded query is shown in the panel for the user to correct, never
// applied blindly. Upgrade to per-tool flag tables if that proves too coarse.

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
    /// Why the lines the command listed can't be matched by one Search
    /// query (patterns read from a file, NUL-split input, piped input).
    pub unseedable: Option<&'static str>,
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

/// How the command decides Match Case. Flags overwrite it in order, so the
/// last of `-i`/`-s`/`-S` wins, as in rg; `Smart` is resolved against the
/// pattern once parsing is done.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CaseRule {
    Sensitive,
    Insensitive,
    /// Ignore case unless the pattern has an uppercase letter: rg, ag and
    /// ack's `-S`/`--smart-case`, and ag's default.
    Smart,
}

/// Whether `prog`'s smart case matches `pattern` case-sensitively, read the
/// way that tool reads it; `rust_regex` says rg parsed it with its default
/// engine (no `-F` or `-P`). Where unsure it says yes: a seed narrower than
/// the terminal search can't make Replace All touch a line it never listed.
fn smart_case_is_sensitive(prog: &str, pattern: &str, rust_regex: bool) -> bool {
    let any_uppercase = || pattern.chars().any(char::is_uppercase);
    match prog {
        // rg's default engine looks for an uppercase literal in the parsed
        // regex, so `\W` and `\pL` don't count, and a pattern with no literal
        // keeps case. One it can't parse gets the check below.
        "rg" | "ripgrep" if rust_regex => {
            match regex_syntax::ast::parse::Parser::new().parse(pattern) {
                Ok(ast) => {
                    let Ok(lits) = regex_syntax::ast::visit(&ast, RgLiterals::default());
                    !lits.any || lits.uppercase
                }
                Err(_) => any_uppercase(),
            }
        }
        // ag checks every byte, escapes included, and a non-ASCII one counts.
        "ag" => pattern
            .chars()
            .any(|c| !c.is_ascii() || c.is_ascii_uppercase()),
        // rg -F checks every letter. rg -P skips the letter after a
        // backslash and ack drops a few escapes (`\W`, `\x4A`) first, so both
        // keep case only where this does too.
        _ => any_uppercase(),
    }
}

/// The literals of an rg regex, as its smart case counts them.
#[derive(Default)]
struct RgLiterals {
    any: bool,
    uppercase: bool,
}

impl RgLiterals {
    fn see(&mut self, c: char) {
        self.any = true;
        self.uppercase |= c.is_uppercase();
    }
}

impl regex_syntax::ast::Visitor for RgLiterals {
    type Output = Self;
    type Err = std::convert::Infallible;

    fn finish(self) -> Result<Self, Self::Err> {
        Ok(self)
    }

    fn visit_pre(&mut self, ast: &regex_syntax::ast::Ast) -> Result<(), Self::Err> {
        if let regex_syntax::ast::Ast::Literal(lit) = ast {
            self.see(lit.c);
        }
        Ok(())
    }

    fn visit_class_set_item_pre(
        &mut self,
        item: &regex_syntax::ast::ClassSetItem,
    ) -> Result<(), Self::Err> {
        use regex_syntax::ast::ClassSetItem;
        match item {
            ClassSetItem::Literal(lit) => self.see(lit.c),
            ClassSetItem::Range(range) => {
                self.see(range.start.c);
                self.see(range.end.c);
            }
            _ => {}
        }
        Ok(())
    }
}

/// The simple commands of a shell line, each with whether a pipe feeds it:
/// the line is cut at the unquoted `|`, `|&`, `||`, `&&`, `;`, `&` and line
/// breaks, and redirections (`> out`, `2>/dev/null`, `<in`) are dropped. So
/// `clear; grep -rn x src | head 2>&1` is `clear`, `grep -rn x src` and a
/// piped-into `head`. An input redirection counts as a pipe: `rg x < f`
/// searched `f` on stdin, not the directory it ran in.
fn simple_commands(line: &str) -> Vec<(String, bool)> {
    // Push a quoted span, through its closing quote, onto `out`.
    /// Copy a quoted run through its closing `open` quote, honouring `\`
    /// escapes inside double quotes, so operators in it stay part of the word.
    fn quoted(open: char, chars: &mut std::iter::Peekable<std::str::Chars>, out: &mut String) {
        while let Some(c) = chars.next() {
            out.push(c);
            if c == open {
                return;
            }
            if c == '\\' && open == '"' {
                out.extend(chars.next());
            }
        }
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut piped = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                cur.extend(chars.next());
            }
            '\'' | '"' => {
                cur.push(c);
                quoted(c, &mut chars, &mut cur);
            }
            '>' | '<' => {
                // A redirection: its fd (`2>`), operator (`>>`, `>&`) and target.
                if c == '<' {
                    piped = true;
                }
                let before_fd = cur.trim_end_matches(|d: char| d.is_ascii_digit());
                if before_fd.is_empty() || before_fd.ends_with(char::is_whitespace) {
                    cur.truncate(before_fd.len());
                }
                while chars.next_if(|n| matches!(n, '>' | '<' | '&')).is_some() {}
                while chars.next_if(|n| *n == ' ' || *n == '\t').is_some() {}
                let mut target = String::new();
                while let Some(n) = chars.next_if(|n| !n.is_whitespace() && !"|&;<>".contains(*n)) {
                    match n {
                        '\'' | '"' => quoted(n, &mut chars, &mut target),
                        '\\' => target.extend(chars.next()),
                        _ => {}
                    }
                }
                cur.push(' ');
            }
            // `&>` redirects both streams; the `>` arm drops it.
            '&' if chars.peek() == Some(&'>') => {}
            '|' | '&' | ';' | '\n' => {
                let pipe = c == '|' && chars.next_if_eq(&'|').is_none();
                if c != ';' && c != '\n' {
                    chars.next_if_eq(&'&');
                }
                out.push((std::mem::take(&mut cur), piped));
                piped = pipe;
            }
            _ => cur.push(c),
        }
    }
    out.push((cur, piped));
    out
}

/// The search program `tokens` runs, path stripped (`/usr/bin/rg` -> `rg`,
/// `git grep` -> `git`), and the index of its first argument.
fn search_program(tokens: &[String]) -> Option<(&str, usize)> {
    let first = tokens.first()?;
    let prog = first.rsplit('/').next().unwrap_or(first);
    match prog {
        "rg" | "ripgrep" | "grep" | "egrep" | "fgrep" | "ag" | "ack" => Some((prog, 1)),
        "git" if tokens.get(1).is_some_and(|t| t == "grep") => Some((prog, 2)),
        _ => None,
    }
}

/// Why a grep over NUL-separated records can't seed: Search reads lines.
const NUL_DATA: &str = "the grep split its input at NUL bytes, not lines (-z)";

/// Parse a terminal command line into a [`SearchCommand`], or `None` if it
/// runs no recognised search (`rg`/`ripgrep`/`grep`/`egrep`/`fgrep`/`git
/// grep`/`ag`/`ack`) or that search carries no pattern. In a pipeline or a
/// list (`clear; rg x | head`) the last search is the one read.
pub fn parse_search_command(cmdline: &str) -> Option<SearchCommand> {
    let (tokens, piped) = simple_commands(cmdline)
        .into_iter()
        .rev()
        .find_map(|(cmd, piped)| {
            let tokens = shlex::split(&cmd)?;
            search_program(&tokens).is_some().then_some((tokens, piped))
        })?;
    // Program name; sets the regex/case defaults, flags below refine them.
    let (prog, mut idx) = search_program(&tokens)?;
    // rg/ag/ack/egrep default to regex; plain grep's BRE is not Rust-regex, so
    // treat its pattern as literal (correct for the common "grep a string"),
    // upgraded by -E/-P. fgrep and git grep are literal too.
    let mut use_regex = matches!(prog, "rg" | "ripgrep" | "ag" | "ack" | "egrep");
    // `-s` means case-sensitive only in rg/ag; GNU grep and ack use it to
    // suppress error messages, and git grep has no such flag at all.
    let dash_s_is_case_sensitive = matches!(prog, "rg" | "ripgrep" | "ag");
    // `-S` is smart case in rg/ag/ack; BSD grep follows symlinks with it.
    let dash_s_upper_is_smart_case = matches!(prog, "rg" | "ripgrep" | "ag" | "ack");
    // grep, git grep, rg and ack match case unless told otherwise; only ag
    // defaults to smart case. Seeding ignore-case instead made Replace All
    // rewrite `COLOR_DEFAULT` after an `rg color` that never listed it (#922).
    let mut case = if prog == "ag" {
        CaseRule::Smart
    } else {
        CaseRule::Sensitive
    };
    let mut whole_word = false;
    let mut include: Option<String> = None;
    let mut exclude: Option<String> = None;
    // Patterns given by `-e`/`--regexp` (or `-f`) make every positional a
    // path.
    let mut patterns: Vec<String> = Vec::new();
    let mut patterns_from_file = false;
    let mut positionals: Vec<String> = Vec::new();
    let mut types: Vec<String> = Vec::new();
    let mut types_not: Vec<String> = Vec::new();
    let mut inverted = false;
    // `-x`/`--line-regexp`: the pattern must match the whole line.
    let mut line_regexp = false;
    let mut unseedable: Option<&'static str> = None;
    let is_rg = matches!(prog, "rg" | "ripgrep");
    let is_grep = matches!(prog, "grep" | "egrep" | "fgrep");
    // rg's `-P`/`--pcre2` engine reads smart case its own way.
    let mut pcre2 = false;
    let mut recursive = !is_grep;

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
                "ignore-case" => case = CaseRule::Insensitive,
                "case-sensitive" | "no-ignore-case" => case = CaseRule::Sensitive,
                "smart-case" => case = CaseRule::Smart,
                "word-regexp" => whole_word = true,
                "line-regexp" => line_regexp = true,
                "invert-match" | "files-without-match" | "files-without-matches" => inverted = true,
                "fixed-strings" => use_regex = false,
                "recursive" | "dereference-recursive" => recursive = true,
                "directories" => {
                    if take_value(inline_val, &tokens, &mut idx).as_deref() == Some("recurse") {
                        recursive = true;
                    }
                }
                "extended-regexp" | "perl-regexp" => use_regex = true,
                "pcre2" => {
                    use_regex = true;
                    pcre2 = true;
                }
                "regexp" => patterns.extend(take_value(inline_val, &tokens, &mut idx)),
                "file" => {
                    let _ = take_value(inline_val, &tokens, &mut idx);
                    patterns_from_file = true;
                }
                "null-data" => unseedable = Some(NUL_DATA),
                // git grep's pattern algebra; `--or` is the default.
                "and" | "not" | "all-match" => {
                    unseedable = Some("git grep combined its patterns with --and / --not")
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
                "max-count" | "type" | "type-not" | "context" | "after-context"
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
                    'i' => case = CaseRule::Insensitive,
                    's' if dash_s_is_case_sensitive => case = CaseRule::Sensitive,
                    'S' if dash_s_upper_is_smart_case => case = CaseRule::Smart,
                    'w' => whole_word = true,
                    'x' => line_regexp = true,
                    'v' => inverted = true,
                    // rg's `-z` searches compressed files; grep's splits
                    // the input at NUL bytes instead of line breaks.
                    'z' if is_grep => unseedable = Some(NUL_DATA),
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
                    'E' => use_regex = true,
                    'P' => {
                        use_regex = true;
                        pcre2 = true;
                    }
                    'e' | 'g' => {
                        let rest: String = chars[ci + 1..].iter().collect();
                        let v = if rest.is_empty() {
                            take_value(None, &tokens, &mut idx)
                        } else {
                            Some(rest)
                        };
                        if chars[ci] == 'e' {
                            patterns.extend(v);
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
                    // ag's `-f` follows symlinks; elsewhere it names a
                    // file of patterns.
                    'f' if prog != "ag" => {
                        if chars[ci + 1..].is_empty() {
                            let _ = take_value(None, &tokens, &mut idx);
                        }
                        patterns_from_file = true;
                        break;
                    }
                    // Short flags that take a value we don't use.
                    'm' | 'A' | 'B' | 'C' | 't' | 'T' => {
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

    // Without `-e` or `-f`, the first positional is the pattern; the rest
    // are the files and directories searched.
    let mut positionals = positionals.into_iter();
    if patterns.is_empty() && !patterns_from_file {
        patterns.push(positionals.next()?);
    }
    let paths: Vec<String> = positionals.collect();
    if patterns.iter().any(String::is_empty) {
        return None;
    }
    if patterns_from_file {
        unseedable = Some("the grep's patterns come from a file (-f); search for them by hand");
    }
    // A search fed by a pipe and given no path reads the pipe (git grep
    // ignores it).
    if piped && paths.is_empty() && prog != "git" {
        unseedable.get_or_insert("the grep searched what was piped into it, not files");
    }
    // Each pattern is read on its own: where one keeps case the seed does
    // too, which can only list fewer lines.
    let case_sensitive = match case {
        CaseRule::Sensitive => true,
        CaseRule::Insensitive => false,
        CaseRule::Smart => patterns
            .iter()
            .any(|p| smart_case_is_sensitive(prog, p, use_regex && !pcre2)),
    };
    // Several patterns list a line holding any of them, and `-x` only a line
    // that is all pattern: both need a regex, literal patterns escaped. `-x`
    // overrides `-w`, as in grep and rg.
    let pattern = if patterns.len() > 1 || line_regexp {
        let as_regex = |p: &String| {
            if use_regex {
                p.clone()
            } else {
                regex_syntax::escape(p)
            }
        };
        let body = match patterns.as_slice() {
            [one] => as_regex(one),
            many => many
                .iter()
                .map(|p| format!("(?:{})", as_regex(p)))
                .collect::<Vec<_>>()
                .join("|"),
        };
        use_regex = true;
        if line_regexp {
            whole_word = false;
            format!("^(?:{body})$")
        } else {
            body
        }
    } else {
        patterns.pop().unwrap_or_default()
    };
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
        unseedable,
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
    if let Some(why) = sc.unseedable {
        return Err(why.to_string());
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
        assert!(c.case_sensitive, "rg matches case unless told otherwise");
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
        // rg's case-sensitive flag after `-i` would seed a search that skips
        // differently-cased matches the terminal command actually found.
        assert!(!parse("grep -is Error .").unwrap().case_sensitive);
        assert!(!parse("grep -ris Error .").unwrap().case_sensitive);
        assert!(!parse("git grep -i -s Error").unwrap().case_sensitive);
        assert!(!parse("ack -is Error").unwrap().case_sensitive);
    }

    /// #922: grep, rg, git grep and ack match case unless told otherwise, so
    /// a seed that ignored case listed (and Replace All rewrote)
    /// `COLOR_DEFAULT` after `rg color`. Only ag defaults to smart case.
    #[test]
    fn each_tool_seeds_its_own_default_case() {
        for cmd in [
            "rg color",
            "grep -rn color .",
            "egrep -r color .",
            "git grep color",
            "ack color",
            "ag Color",
        ] {
            assert!(parse(cmd).unwrap().case_sensitive, "{cmd}");
        }
        assert!(!parse("ag color").unwrap().case_sensitive);
    }

    /// `-i` ignores case and `-S` is smart case (ignore it only for an
    /// all-lowercase pattern); the last of `-i`/`-s`/`-S` wins, as in rg.
    #[test]
    fn case_flags_override_the_default_and_the_last_one_wins() {
        for cmd in [
            "rg -i color",
            "rg --ignore-case Color",
            "grep -rni Color .",
            "git grep --ignore-case Color",
            "rg -S color",
            "rg --smart-case color",
            "ack -S color",
            "rg -s -i Color",
            "ag -i Color",
        ] {
            assert!(!parse(cmd).unwrap().case_sensitive, "{cmd}");
        }
        for cmd in [
            "rg -S Color",
            "rg -i -s color",
            "rg -i -S Color",
            "grep -i --no-ignore-case color .",
            // Negative: grep has no smart case; BSD grep's -S follows links.
            "grep -rS color .",
        ] {
            assert!(parse(cmd).unwrap().case_sensitive, "{cmd}");
        }
    }

    /// rg's smart case looks for an uppercase literal in the parsed regex, so
    /// `\W` and `\pL` don't count but `[A-Z]` and `\x52` do, and a pattern
    /// with no literal keeps case. ag counts every byte, escapes included.
    #[test]
    fn smart_case_reads_the_pattern_the_way_the_tool_does() {
        for cmd in [
            r"rg -S '\bcolor\W'",
            r"rg -S '\pL+color'",
            r"rg -S '\S+_color'",
        ] {
            assert!(!parse(cmd).unwrap().case_sensitive, "{cmd}");
        }
        for cmd in [
            r"rg -S 'colo[A-Z]'",
            r"rg -S 'colo\x52'",
            r"rg -S '[[:upper:]]+'",
            r"rg -SF 'color\W'",
            // rg -P skips only the letter after a backslash.
            r"rg -SP '\pLcolor'",
            r"ag 'color\W'",
            r"ag 'café'",
        ] {
            assert!(parse(cmd).unwrap().case_sensitive, "{cmd}");
        }
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

    /// The three greps of #1201's follow-up: `-x` matches whole lines, every
    /// `-e` is a pattern, and `-f` reads them from a file croft can't seed.
    #[test]
    fn line_regexp_and_several_patterns_seed_one_regex() {
        let c = parse("grep -rnx timeout src").unwrap();
        assert_eq!(c.pattern, "^(?:timeout)$");
        assert!(c.use_regex);
        assert_eq!(c.paths, ["src"]);
        let c = parse("grep -rn -e connect_timeout -e retry_timeout src").unwrap();
        assert_eq!(c.pattern, "(?:connect_timeout)|(?:retry_timeout)");
        assert!(c.use_regex);
        assert_eq!(c.paths, ["src"]);
        // Literal patterns are escaped; `-x` overrides `-w`.
        let c = parse("rg -xwF -e 'a.b' --regexp=c").unwrap();
        assert_eq!(c.pattern, r"^(?:(?:a\.b)|(?:c))$");
        assert!(!c.whole_word);
        // One pattern with an uppercase letter keeps case for all of them.
        assert!(parse("rg -S -e color -e Hue").unwrap().case_sensitive);
        assert!(!parse("rg -S -e color -e hue").unwrap().case_sensitive);
        // Negative: one pattern without -x is seeded as typed.
        let c = parse("grep -rn -e 'a.b' src").unwrap();
        assert_eq!(c.pattern, "a.b");
        assert!(!c.use_regex);
    }

    /// Patterns read from a file, NUL-split input and git grep's pattern
    /// algebra can't be one Search query, so they're refused with a reason.
    #[test]
    fn patterns_from_a_file_or_nul_split_input_are_unseedable() {
        let c = parse("grep -rn -f words.txt src").unwrap();
        assert!(c.unseedable.unwrap().contains("from a file"));
        assert_eq!(c.paths, ["src"]);
        for cmd in [
            "rg --file=words.txt src",
            "grep -rnz color src",
            "rg --null-data color",
            "git grep -e a --and -e b",
        ] {
            assert!(parse(cmd).unwrap().unseedable.is_some(), "{cmd}");
        }
        // Negative: ag's -f follows links and rg's -z reads compressed files.
        let c = parse("ag -f color src").unwrap();
        assert_eq!((c.pattern.as_str(), c.unseedable), ("color", None));
        assert_eq!(parse("rg -z color").unwrap().unseedable, None);
    }

    /// The search read is the last one of a pipeline or list, redirections
    /// dropped, and one fed by a pipe or a redirected file is refused.
    #[test]
    fn the_last_search_of_a_pipeline_or_list_is_read() {
        for cmd in [
            "grep -rn x src | head -5",
            "clear; grep -rn x src",
            "cd . && grep -rn x src 2>/dev/null >out.txt",
            "grep -rn x src 2>&1 |& less",
            "true || grep -rn x src &",
        ] {
            let c = parse(cmd).unwrap();
            assert_eq!((c.pattern.as_str(), c.unseedable), ("x", None), "{cmd}");
            assert_eq!(c.paths, ["src"], "{cmd}");
        }
        // Quoted or escaped operators are part of the pattern.
        assert_eq!(parse("rg 'a|b; c>d' src").unwrap().pattern, "a|b; c>d");
        assert_eq!(parse(r"grep -rE a\|b src").unwrap().pattern, "a|b");
        // A search fed by a pipe reads it, not files; the last one wins.
        assert!(parse("cat f | grep x").unwrap().unseedable.is_some());
        assert!(parse("rg x src | rg y").unwrap().unseedable.is_some());
        // So is one whose stdin is a redirected file, unless it names paths.
        assert!(parse("rg timeout < one.txt").unwrap().unseedable.is_some());
        assert!(parse("grep -rn x src <<< y").unwrap().unseedable.is_none());
        assert!(parse("rg x src | grep -v y").unwrap().inverted);
        assert!(parse("ls | wc -l").is_none());
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
