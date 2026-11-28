//! The workspace search engine behind the Search view, the search editor
//! and the log view: walking the workspace in parallel under its ignore
//! rules, matching a query as typed (case, whole word, regex), include and
//! exclude globs, the background worker that streams hits, highlighting a
//! line's matches, and Replace with $1, ${name} and Preserve Case. Nothing
//! here draws; the Search view is `widgets::search`.

use globset::{Glob, GlobSet, GlobSetBuilder};
use grep_matcher::{LineMatchKind, LineTerminator, Match, Matcher, NoCaptures, NoError};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use unicode_width::UnicodeWidthStr;

use crate::glob_list::split_globs;

mod highlight;
mod matcher;
mod path_filter;
mod replace;
mod walk;
mod worker;
pub use highlight::split_for_highlight;
pub(crate) use matcher::Pattern;
pub(crate) use matcher::query_error;
use matcher::*;
pub use path_filter::PathFilter;
pub(crate) use path_filter::invalid_globs;
pub use replace::preserve_case;
use replace::*;
pub(crate) use replace::{
    expand_replacement, replace_in_file, replace_in_text, trim_leading_context,
};
pub(crate) use walk::as_typed;
#[cfg(test)]
pub(crate) use walk::collect_matches_in_text;
pub use walk::line_may_match;
pub(crate) use walk::search_workspace;
pub use walk::search_workspace_streaming_filtered;
use walk::*;
pub use worker::SearchEvent;
pub use worker::SearchRequest;
pub use worker::search_worker_loop;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    /// 1-indexed line number, the way humans / editors talk about lines.
    pub line_no: usize,
    /// The matched line without its line break, length capped, and with
    /// any indent before the first match trimmed.
    pub line_text: String,
    /// How many times the query matches on this line, counted as Replace All
    /// replaces them. The header counts these, as VS Code does, not lines
    /// (#860).
    pub matches: usize,
}

/// Mode toggles that drive `search_workspace`. Mirror VS Code's three
/// search input toggles, in the same left-to-right order:
///   - `case_sensitive` (Aa)
///   - `whole_word` (ab with underline)
///   - `use_regex` (.*)
///   - `preserve_case` (AB, on the replace row): each replacement takes the
///     case of the text it replaces (#1612)
///
/// All-false matches the original case-insensitive substring behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SearchOpts {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub preserve_case: bool,
}

#[cfg(test)]
mod tests;
