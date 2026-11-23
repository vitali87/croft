//! The CodeQL side bar: databases, query history and logs, running queries, suites and
//! packs, result viewers, AST and CFG, the model editor, the run queue, variant analysis,
//! and downloading and updating the CLI.

use super::*;

mod cli_install;
mod contextual;
mod database_sources;
mod databases;
mod history;
mod logs;
mod model_editor;
mod packs;
mod queries;
mod queue;
mod results;
mod run;
mod side_bar;
mod test_runs;
mod variant_repos;
mod variant_runs;

/// What a CodeQL run evaluates (#578): the whole query, a quick
/// evaluation of part of it, or a published pack named by its reference.
pub(super) enum CodeqlRunMode {
    Whole,
    Quick(crate::codeql_qs::QuickEval),
    Pack,
}

/// What "CodeQL: Check for CLI Updates" learns (#578): the latest release,
/// or why it could not be read, and the version in use when it runs.
pub(super) type CodeqlCliCheck = (Result<String, String>, Option<String>);

/// Query help being rendered (#578): where the outcome arrives, the
/// Markdown file to preview, and the query's name.
pub(super) type CodeqlQueryHelp = (
    std::sync::mpsc::Receiver<Result<(), String>>,
    PathBuf,
    String,
);

/// A `codeql pack` install or download in flight (#578): where its outcome
/// arrives, and the status lines for success and for failure.
pub(super) type CodeqlPackJob = (
    std::sync::mpsc::Receiver<Result<(), String>>,
    String,
    String,
);

/// A CodeQL document being made (#578): the file it writes, or why not,
/// and the status lines for success and for failure.
pub(super) type CodeqlDocJob = (
    std::sync::mpsc::Receiver<Result<PathBuf, String>>,
    String,
    String,
);

/// An evaluator log being read for its viewer (#578): the predicates, or
/// why not, and the query whose log it is.
pub(super) type CodeqlLogViewerJob = (
    std::sync::mpsc::Receiver<Result<Vec<crate::codeql_evallog::Predicate>, String>>,
    String,
);

/// The rows of the Add CodeQL Database picker and the side-bar action each
/// runs.
pub(super) const CODEQL_DB_SOURCES: [(crate::widgets::codeql::Action, &str); 4] = [
    (
        crate::widgets::codeql::Action::AddDatabaseFromFolder,
        "From a folder",
    ),
    (
        crate::widgets::codeql::Action::AddDatabaseFromArchive,
        "From an archive",
    ),
    (
        crate::widgets::codeql::Action::AddDatabaseFromUrl,
        "From a URL (as a zip file)",
    ),
    (
        crate::widgets::codeql::Action::AddDatabaseFromGithub,
        "From GitHub",
    ),
];

/// The Model Editor's endpoints being read (#578): the endpoints, or why
/// not, and the database and language they are for.
pub(super) type CodeqlModelJob = (
    std::sync::mpsc::Receiver<Result<Vec<crate::codeql_model::Endpoint>, String>>,
    String,
    String,
);

/// A CodeQL database upgrade or cache cleanup in flight (#578): where its
/// outcome arrives, which job it is, and the database's name and folder.
pub(super) type CodeqlUpgrade = (
    std::sync::mpsc::Receiver<Result<(), String>>,
    crate::codeql_query::DbJob,
    String,
    PathBuf,
);

pub(super) type VariantResults = Result<VariantResultFiles, String>;

/// One unfinished variant analysis and what reading its progress gave.
pub(super) type CodeqlRunPoll = (
    crate::codeql_submit::Submitted,
    Result<crate::codeql_submit::RunProgress, String>,
);

/// How often unfinished variant analyses are read again (#578).
pub(super) const CODEQL_VARIANT_POLL: std::time::Duration = std::time::Duration::from_secs(30);

/// How long after submitting a variant analysis croft follows it (#578).
pub(super) const CODEQL_VARIANT_FOLLOW: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 3600);

/// A GitHub Code Search in flight: the variant analysis list its
/// repositories go into, and its outcome: the repositories found, and why
/// a page after the first failed, cutting the results short.
pub(super) type CodeqlCodeSearch = (
    String,
    std::sync::mpsc::Receiver<Result<(Vec<String>, Option<String>), String>>,
);
