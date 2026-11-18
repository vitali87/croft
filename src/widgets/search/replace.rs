//! Replace: a replacement's $1 and ${name} references and Preserve Case, replacing in a
//! line, a text or a file, and the panel's Replace All on disk.

use super::*;

/// Compile the search query + `SearchOpts` into a `regex::Regex`, mirroring
/// the pattern construction used by `split_for_highlight_regex` so replace
/// matches exactly what the highlighter shows. In literal (non-regex) mode the
/// query is escaped so metacharacters are matched verbatim. Returns `None` for
/// an empty query or an uncompilable pattern.
pub(super) fn build_replace_regex(query: &str, opts: SearchOpts) -> Option<Pattern> {
    let q = as_typed(query);
    if q.is_empty() {
        return None;
    }
    let body = if opts.use_regex {
        q.to_string()
    } else {
        regex::escape(q)
    };
    let mut pattern = String::new();
    if !opts.case_sensitive {
        pattern.push_str("(?i)");
    }
    // Applied to one line at a time (see `replace_in_text`), as the search
    // matches, so `^` and `$` are that line's ends. Whole-word is checked on
    // each match by the search's own rule, not `\b`, which never matches
    // beside a query that starts or ends with punctuation (`$var`, `@x`).
    pattern.push_str(&body);
    Pattern::new(&pattern)
}

/// Replace the matches in ONE line (no terminator), returning the new text
/// and how many were replaced. Matches are found by the SAME grep matcher
/// the search used, so replace touches exactly the hits it listed: the
/// `regex` crate's leftmost-first choice missed a whole-word match that
/// only a longer alternative (`foo|foobar` on `foobar`) satisfies. `full`
/// (the pattern anchored to a whole match) only expands capture groups.
/// An empty match is replaced unless it touches the previous replacement,
/// as `Regex::replace_all` does, so `^` inserts at every line start.
pub(super) fn replace_in_line(
    line: &str,
    matcher: &SearchMatcher,
    full: Option<&Pattern>,
    replacement: &str,
    opts: SearchOpts,
) -> (String, usize) {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    let mut pos = 0;
    let mut count = 0;
    let mut prev_end: Option<usize> = None;
    while pos <= line.len() {
        let Ok(Some(m)) = matcher.find_at(line.as_bytes(), pos) else {
            break;
        };
        let (start, end) = (m.start(), m.end());
        if !line.is_char_boundary(start) || !line.is_char_boundary(end) {
            break;
        }
        let adjacent_empty = start == end && prev_end == Some(start);
        if !adjacent_empty {
            out.push_str(&line[last..start]);
            let expanded = full
                .filter(|_| opts.use_regex)
                .and_then(|re| re.expand_at(line, start, end, replacement))
                .unwrap_or_else(|| replacement.to_string());
            if opts.preserve_case {
                out.push_str(&preserve_case(&line[start..end], &expanded));
            } else {
                out.push_str(&expanded);
            }
            count += 1;
            last = end;
            prev_end = Some(end);
        }
        pos = if start == end {
            match line[end..].chars().next() {
                Some(c) => end + c.len_utf8(),
                None => break,
            }
        } else {
            end
        };
    }
    out.push_str(&line[last..]);
    (out, count)
}

/// How many matches ONE line holds, by exactly the rule `replace_in_line`
/// replaces them with, so the Search header's result count and the Replace
/// All confirmation always agree (#860).
pub(super) fn count_in_line(line: &str, matcher: &SearchMatcher) -> usize {
    replace_in_line(line, matcher, None, "", SearchOpts::default()).1
}

/// The leading context a result row keeps before its first match when that
/// match, and `need` cells with it (the match plus any replace preview), would
/// not fit in `avail` cells after `lead`; `None` when it fits, so a row whose
/// match shows is drawn exactly as its line reads (#860). The row then opens
/// on the last cells of `lead`, half the room the match leaves, so context
/// shows on both sides. Cut plainly, with no "…" marker: croft draws none.
pub(super) fn trim_leading_context(lead: &str, avail: usize, need: usize) -> Option<String> {
    use unicode_width::UnicodeWidthChar;
    if lead.width() + need <= avail {
        return None;
    }
    let keep = avail.saturating_sub(need) / 2;
    let mut width = 0;
    let mut start = lead.len();
    for (i, c) in lead.char_indices().rev() {
        width += c.width().unwrap_or(0);
        if width > keep {
            break;
        }
        start = i;
    }
    Some(lead[start..].to_string())
}

/// `replacement` made safe for `Captures::expand`, read the way
/// VS Code reads it. A numbered reference is braced (`$1_old` to
/// `${1}_old`): the regex crate reads `$1_old` as a group NAMED `1_old`,
/// which expands to nothing and deletes the match, where VS Code reads group
/// 1 then `_old`. `$&` is the whole match and `$$` a literal dollar. A
/// `$NAME` or `${NAME}` expands only when `is_group` knows that name;
/// any other `$` is text (#1399), so a shell, PHP or Perl variable such as
/// `$PREFIX` survives instead of expanding to nothing. A `$N` or `${N}`
/// with no group N is text too (#1636); `groups` counts them, group 0
/// included.
pub(super) fn brace_refs(
    replacement: &str,
    groups: usize,
    is_group: impl Fn(&str) -> bool,
) -> String {
    let is_name = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
    let mut out = String::with_capacity(replacement.len());
    let mut rest = replacement;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let name_len = after
            .chars()
            .take_while(is_name)
            .map(char::len_utf8)
            .sum::<usize>();
        let braced = after
            .strip_prefix('{')
            .and_then(|b| b.find('}').map(|end| &b[..end]));
        let (expansion, used) = if after.starts_with('$') {
            (String::from("$$"), 1)
        } else if after.starts_with('&') {
            (String::from("${0}"), 1)
        } else if digits > 0 {
            numbered_ref(&after[..digits], groups)
        } else if let Some(name) = braced {
            let group = if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()) {
                name.parse::<usize>().is_ok_and(|g| g < groups)
            } else {
                is_group(name)
            };
            if group {
                (format!("${{{name}}}"), name.len() + 2)
            } else {
                (String::from("$$"), 0)
            }
        } else if name_len > 0 && is_group(&after[..name_len]) {
            (format!("${{{}}}", &after[..name_len]), name_len)
        } else {
            (String::from("$$"), 0)
        };
        out.push_str(&expansion);
        rest = &after[used..];
    }
    out.push_str(rest);
    out
}

/// `$` then `digits`, read the way VS Code reads it: the longest prefix of
/// the digits that numbers a group of the pattern, the rest left as text, so
/// `$10` with one group is group 1 then `0`. With no such prefix the `$` is
/// text (#1636): a price like `$5` or a shell `$2` survives instead of
/// expanding to nothing. Returns the expansion and the digits it used.
pub(super) fn numbered_ref(digits: &str, groups: usize) -> (String, usize) {
    (1..=digits.len())
        .rev()
        .find(|&n| digits[..n].parse::<usize>().is_ok_and(|g| g < groups))
        .map_or((String::from("$$"), 0), |n| {
            (format!("${{{}}}", &digits[..n]), n)
        })
}

/// Replace every match of `query` (honouring `opts`) in `content` with
/// `replacement`, returning `(new_content, count)`. In regex mode the
/// replacement honours `$1` / `${name}` capture references, like VS Code; in
/// literal mode the replacement is inserted verbatim (a `$` in the replacement
/// is escaped so it is not treated as a capture reference). Returns `None` when
/// the query is empty or the pattern can't compile, so callers leave the file
/// untouched.
pub fn replace_in_text(
    content: &str,
    query: &str,
    replacement: &str,
    opts: SearchOpts,
) -> Option<(String, usize)> {
    let q = as_typed(query);
    let matcher = build_matcher(q, opts)?;
    // The pattern itself, for `$1` expansion only. Not wrapped in anything:
    // a `(?x)` query's trailing `# comment` swallowed a closing `)$`.
    let full = if opts.use_regex {
        let case = if opts.case_sensitive { "" } else { "(?i)" };
        Some(Pattern::new(&format!("{case}{q}"))?)
    } else {
        None
    };
    // Line by line, exactly as the search matched: over the whole file,
    // `[^;]*`, `\s` or `\W` ran across line breaks, so Replace All deleted
    // lines the search had never listed.
    let mut out = String::with_capacity(content.len());
    let mut count = 0;
    for piece in content.split_inclusive('\n') {
        let body = piece
            .strip_suffix("\r\n")
            .or_else(|| piece.strip_suffix('\n'))
            .unwrap_or(piece);
        let (line, n) = replace_in_line(body, &matcher, full.as_ref(), replacement, opts);
        out.push_str(&line);
        out.push_str(&piece[body.len()..]);
        count += n;
    }
    Some((out, count))
}

/// `replacement` in the case of `matched`, VS Code's Preserve Case rules
/// (`buildReplaceStringWithCasePreserved`): an all-upper match gives an
/// all-upper replacement, all-lower gives all-lower, and a capitalised one a
/// capitalised one. When both split into the same number of `-` (or `_`)
/// parts, each part is shaped by its own, so `User-Name` over `account-id`
/// gives `Account-Id`. Anything else goes in as typed (#1612).
pub fn preserve_case(matched: &str, replacement: &str) -> String {
    if matched.is_empty() {
        return replacement.to_string();
    }
    let splits = |sep: char| {
        matched.contains(sep)
            && replacement.contains(sep)
            && matched.split(sep).count() == replacement.split(sep).count()
    };
    let (hyphens, underscores) = (splits('-'), splits('_'));
    if hyphens != underscores {
        let sep = if hyphens { '-' } else { '_' };
        return matched
            .split(sep)
            .zip(replacement.split(sep))
            .map(|(m, r)| preserve_case(m, r))
            .collect::<Vec<_>>()
            .join(&sep.to_string());
    }
    if matched.to_uppercase() == matched {
        return replacement.to_uppercase();
    }
    if matched.to_lowercase() == matched {
        return replacement.to_lowercase();
    }
    let (Some(first), Some(lead)) = (matched.chars().next(), replacement.chars().next()) else {
        return replacement.to_string();
    };
    let rest = &replacement[lead.len_utf8()..];
    if first.is_uppercase() {
        lead.to_uppercase().chain(rest.chars()).collect()
    } else if first.is_lowercase() {
        lead.to_lowercase().chain(rest.chars()).collect()
    } else {
        replacement.to_string()
    }
}

/// Expand the replacement for ONE matched slice — regex capture references
/// (`$1`, `${name}`) honoured in regex mode, verbatim otherwise, then shaped
/// to the match's case under Preserve Case (#1612). Drives the result rows'
/// inline old→new preview (#123).
pub fn expand_replacement(
    matched: &str,
    query: &str,
    replacement: &str,
    opts: SearchOpts,
) -> String {
    let expanded = match build_replace_regex(query, opts) {
        Some(re) if opts.use_regex => re
            .spans(matched)
            .first()
            .and_then(|&(start, end)| {
                let expanded = re.expand_at(matched, start, end, replacement)?;
                Some(format!(
                    "{}{expanded}{}",
                    &matched[..start],
                    &matched[end..]
                ))
            })
            .unwrap_or_else(|| matched.to_string()),
        _ => replacement.to_string(),
    };
    if opts.preserve_case {
        preserve_case(matched, &expanded)
    } else {
        expanded
    }
}

/// Replace every match in the file at `path` on disk, writing the result back
/// only when at least one replacement was made. Returns the number of matches
/// replaced, or `None` when the file can't be read, the pattern can't compile,
/// or the write fails. Preserves files with no matches untouched (returns 0).
pub fn replace_in_file(
    path: &Path,
    query: &str,
    replacement: &str,
    opts: SearchOpts,
) -> Option<usize> {
    let content = std::fs::read_to_string(path).ok()?;
    let (new_content, count) = replace_in_text(&content, query, replacement, opts)?;
    if count == 0 {
        return Some(0);
    }
    // Written aside and renamed in, keeping the file's mode: written in
    // place, a failed write (a full disk) left the file truncated.
    crate::prefs::replace_file_contents(path, new_content.as_bytes()).ok()?;
    Some(count)
}

pub(super) fn is_whole_word_match(haystack: &str, start: usize, end: usize) -> bool {
    let prev_ok = start == 0
        || haystack[..start]
            .chars()
            .last()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
    let next_ok = end >= haystack.len()
        || haystack[end..]
            .chars()
            .next()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
    prev_ok && next_ok
}

pub(super) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl SearchPanel {
    /// Replace every match in every distinct file among the current hits with
    /// `self.replace`, honouring the active `SearchOpts`. Returns the total
    /// number of matches replaced across all files. The caller is responsible
    /// for re-running the query afterwards so the (now stale) hit list updates.
    pub fn replace_all_on_disk(&self) -> usize {
        let needle = as_typed(&self.query);
        if needle.is_empty() {
            return 0;
        }
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut total = 0usize;
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if let Some(n) = replace_in_file(&hit.path, needle, &self.replace, self.opts) {
                total += n;
            }
        }
        total
    }

    /// Count what Replace All WOULD do — `(occurrences, files)` — without
    /// writing anything. Feeds the confirmation modal (#123).
    pub fn count_replacements(&self, skip: &HashSet<PathBuf>) -> (usize, usize) {
        let needle = as_typed(&self.query);
        if needle.is_empty() {
            return (0, 0);
        }
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut occurrences = 0usize;
        let mut files = 0usize;
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if skip.contains(&hit.path) {
                // The confirmed action will not touch these; counting them
                // made the modal overpromise (#124 review).
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&hit.path) else {
                continue;
            };
            if let Some((_, n)) = replace_in_text(&content, needle, &self.replace, self.opts)
                && n > 0
            {
                occurrences += n;
                files += 1;
            }
        }
        (occurrences, files)
    }

    /// Replace All, skipping `skip` (files open with unsaved edits — a disk
    /// write under a dirty buffer forces the tab into conflict divergence).
    /// Returns `(occurrences replaced, the skipped hit files, the files that
    /// could not be rewritten)`. The last are named rather than dropped: a
    /// file that is not UTF-8 still shows hits (the search reads bytes) but
    /// is never replaced, and "replaced N" alone hid that.
    pub fn replace_all_skipping(
        &self,
        skip: &HashSet<PathBuf>,
    ) -> (usize, Vec<PathBuf>, Vec<PathBuf>) {
        let needle = as_typed(&self.query);
        if needle.is_empty() {
            return (0, Vec::new(), Vec::new());
        }
        let mut failed = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut total = 0usize;
        let mut skipped = Vec::new();
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if skip.contains(&hit.path) {
                skipped.push(hit.path.clone());
                continue;
            }
            match replace_in_file(&hit.path, needle, &self.replace, self.opts) {
                Some(n) => total += n,
                None => failed.push(hit.path.clone()),
            }
        }
        (total, skipped, failed)
    }
}
