//! How a query matches text: the matcher built from the query and its Aa / ab / .*
//! toggles, the pattern the editor's Find bar shares, and the error an invalid query shows.

use super::*;

pub(super) fn build_matcher(query: &str, opts: SearchOpts) -> Option<SearchMatcher> {
    if query.is_empty() {
        return None;
    }
    compile_matcher(query, opts).ok()
}

/// The search's matcher: ripgrep's own engine, or, for a pattern it can't
/// compile because it needs look-around or a backreference, fancy-regex
/// (#1552), as ripgrep's `--engine auto` falls back to PCRE2 and so VS Code
/// runs `\d+ (?!USD)`, `(?<=@)\w+` and `(\w+) \1`. A pattern neither engine
/// takes keeps ripgrep's error (#1496).
pub(super) fn compile_matcher(
    query: &str,
    opts: SearchOpts,
) -> Result<SearchMatcher, grep_regex::Error> {
    let mut b = RegexMatcherBuilder::new();
    b.case_insensitive(!opts.case_sensitive);
    b.word(opts.whole_word);
    b.fixed_strings(!opts.use_regex);
    match b.build(query) {
        Ok(m) => Ok(SearchMatcher::Fast(m)),
        Err(err) => {
            if !opts.use_regex {
                return Err(err);
            }
            let case = if opts.case_sensitive { "" } else { "(?i)" };
            let pattern = if opts.whole_word {
                format!(r"{case}(?<!\w)(?:{query})(?!\w)")
            } else {
                format!("{case}{query}")
            };
            fancy_regex::Regex::new(&pattern)
                .map(|re| SearchMatcher::Fancy(Arc::new(re)))
                .map_err(|_| err)
        }
    }
}

/// See [`compile_matcher`].
#[derive(Clone, Debug)]
pub(super) enum SearchMatcher {
    Fast(RegexMatcher),
    /// Run line by line: it reports no line terminator, so the searcher
    /// hands it one line at a time, which also keeps a `\s` or `[^x]` from
    /// matching across lines, as with the fast engine.
    Fancy(Arc<fancy_regex::Regex>),
}

impl Matcher for SearchMatcher {
    type Captures = NoCaptures;

    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<Match>, NoError> {
        match self {
            Self::Fast(m) => m.find_at(haystack, at),
            // Text that isn't UTF-8 can't be searched by a `str` engine;
            // it is a line with no match rather than an error. A pattern
            // that runs past its backtracking limit is the same.
            Self::Fancy(re) => Ok(std::str::from_utf8(haystack)
                .ok()
                .filter(|text| text.is_char_boundary(at))
                .and_then(|text| re.find_from_pos(text, at).ok().flatten())
                .map(|m| Match::new(m.start(), m.end()))),
        }
    }

    fn new_captures(&self) -> Result<NoCaptures, NoError> {
        Ok(NoCaptures::new())
    }

    fn shortest_match_at(&self, haystack: &[u8], at: usize) -> Result<Option<usize>, NoError> {
        match self {
            Self::Fast(m) => m.shortest_match_at(haystack, at),
            Self::Fancy(_) => Ok(self.find_at(haystack, at)?.map(|m| m.end())),
        }
    }

    fn is_match_at(&self, haystack: &[u8], at: usize) -> Result<bool, NoError> {
        match self {
            Self::Fast(m) => m.is_match_at(haystack, at),
            Self::Fancy(_) => Ok(self.find_at(haystack, at)?.is_some()),
        }
    }

    fn non_matching_bytes(&self) -> Option<&grep_matcher::ByteSet> {
        match self {
            Self::Fast(m) => m.non_matching_bytes(),
            Self::Fancy(_) => None,
        }
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        match self {
            Self::Fast(m) => m.line_terminator(),
            Self::Fancy(_) => None,
        }
    }

    fn find_candidate_line(&self, haystack: &[u8]) -> Result<Option<LineMatchKind>, NoError> {
        match self {
            Self::Fast(m) => m.find_candidate_line(haystack),
            Self::Fancy(_) => Ok(self.shortest_match(haystack)?.map(LineMatchKind::Confirmed)),
        }
    }
}

/// A pattern for what the matcher can't do itself, highlighting a row's
/// matches and expanding `$1` in a replacement: the `regex` crate's, or
/// fancy-regex's for a pattern that needs it (#1552), as [`compile_matcher`].
pub(crate) enum Pattern {
    Plain(regex::Regex),
    Fancy(fancy_regex::Regex),
}

impl Pattern {
    pub(super) fn new(pattern: &str) -> Option<Self> {
        match regex::Regex::new(pattern) {
            Ok(re) => Some(Self::Plain(re)),
            Err(_) => fancy_regex::Regex::new(pattern).ok().map(Self::Fancy),
        }
    }

    /// The regex `body` with the case and whole-word toggles, as the row
    /// highlighter and the editor Find bar match it. A fancy-only pattern
    /// takes whole-word by [`compile_matcher`]'s rule, `(?<!\w)` / `(?!\w)`,
    /// so a match that starts or ends with punctuation (`-(?= )`) is shown
    /// and replaced wherever the search counts it.
    pub(crate) fn for_find(body: &str, opts: SearchOpts) -> Option<Self> {
        let case = if opts.case_sensitive { "" } else { "(?i)" };
        if !opts.whole_word {
            return Self::new(&format!("{case}{body}"));
        }
        match regex::Regex::new(&format!(r"{case}\b(?:{body})\b")) {
            Ok(re) => Some(Self::Plain(re)),
            Err(_) => fancy_regex::Regex::new(&format!(r"{case}(?<!\w)(?:{body})(?!\w)"))
                .ok()
                .map(Self::Fancy),
        }
    }

    pub(super) fn has_group(&self, name: &str) -> bool {
        match self {
            Self::Plain(re) => re.capture_names().flatten().any(|n| n == name),
            Self::Fancy(re) => re.capture_names().flatten().any(|n| n == name),
        }
    }

    /// The number of capture groups, the whole match (group 0) included.
    pub(super) fn captures_len(&self) -> usize {
        match self {
            Self::Plain(re) => re.captures_len(),
            Self::Fancy(re) => re.captures_len(),
        }
    }

    /// Every match in `line`, as byte spans.
    pub(crate) fn spans(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            Self::Plain(re) => re.find_iter(line).map(|m| (m.start(), m.end())).collect(),
            Self::Fancy(re) => re
                .find_iter(line)
                .map_while(Result::ok)
                .map(|m| (m.start(), m.end()))
                .collect(),
        }
    }

    /// `replacement` expanded with the captures of the match spanning
    /// exactly `start..end` of `line`. Captured from the whole line at the
    /// match, so assertions like `\b` see the text around it (on the bare
    /// substring `\b(\.rs)` no longer matched and `$1` came out literally);
    /// the substring alone only as a fallback. None when neither holds a
    /// match of exactly that span.
    pub(crate) fn expand_at(
        &self,
        line: &str,
        start: usize,
        end: usize,
        replacement: &str,
    ) -> Option<String> {
        let template = brace_refs(replacement, self.captures_len(), |name| {
            self.has_group(name)
        });
        let mut out = String::new();
        match self {
            Self::Plain(re) => {
                let caps = re
                    .captures_at(line, start)
                    .filter(|c| {
                        c.get(0)
                            .is_some_and(|m| (m.start(), m.end()) == (start, end))
                    })
                    .or_else(|| {
                        re.captures(&line[start..end])
                            .filter(|c| c.get(0).is_some_and(|m| m.range() == (0..end - start)))
                    })?;
                caps.expand(&template, &mut out);
            }
            Self::Fancy(re) => {
                let caps = re
                    .captures_from_pos(line, start)
                    .ok()
                    .flatten()
                    .filter(|c| {
                        c.get(0)
                            .is_some_and(|m| (m.start(), m.end()) == (start, end))
                    })
                    .or_else(|| {
                        re.captures(&line[start..end])
                            .ok()
                            .flatten()
                            .filter(|c| c.get(0).is_some_and(|m| m.range() == (0..end - start)))
                    })?;
                caps.expand(&template, &mut out);
            }
        }
        Some(out)
    }
}

/// Why the query can't run, for the sidebar to show where the result count
/// would be (#1496): every search path drops a pattern that fails to compile,
/// so without this an unclosed `(` or a look-ahead read as "0 results".
/// `None` for an empty query or one that compiles. The parser's multi-line
/// report (pattern, caret, `error: ...`) is cut to its `error:` line.
pub fn query_error(query: &str, opts: SearchOpts) -> Option<String> {
    let q = as_typed(query);
    if q.is_empty() {
        return None;
    }
    let err = compile_matcher(q, opts).err()?.to_string();
    let reason = err
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("error: "))
        .unwrap_or_else(|| err.lines().next().unwrap_or("").trim());
    Some(format!("Invalid regex: {reason}"))
}

pub(super) fn build_searcher() -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .memory_map(MmapChoice::never())
        .build()
}
