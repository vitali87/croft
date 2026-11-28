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

/// CodeQL's own state outside its view: the CLI it runs and where that came
/// from, the jobs it has running, the queue behind them, and what the last
/// ones reported. The view, `App::codeql`, draws from it.
pub(super) struct CodeqlJobs {
    /// Background folder-size scans for the CodeQL database card (#578): a
    /// large database is many files, so the walk stays off the input path.
    pub(super) size_rx: std::sync::mpsc::Receiver<(PathBuf, Option<u64>)>,
    pub(super) size_tx: std::sync::mpsc::Sender<(PathBuf, Option<u64>)>,
    /// Databases whose size scan is in flight, so a refresh does not start
    /// a second walk of the same folder.
    pub(super) size_pending: std::collections::HashSet<PathBuf>,
    /// The `codeql` executable; tests point it at a stand-in (#578).
    pub(super) program: PathBuf,
    /// The query run in flight: its outcome arrives here, and when it
    /// started (#578).
    pub(super) run: Option<(
        std::sync::mpsc::Receiver<crate::codeql_query::RunStatus>,
        std::time::Instant,
    )>,
    /// Raised to stop the query run in flight: its `codeql` is killed and
    /// the run recorded as cancelled (#578).
    pub(super) cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The file name of the query in flight, for its status-bar chip.
    pub(super) run_name: String,
    /// Queries "Run Queries in Pack" has yet to start, in order (#578).
    pub(super) run_queue: std::collections::VecDeque<crate::codeql_query::QueuedRun>,
    /// "Run Query on Multiple Databases" being set up (#578): the query,
    /// its text, and the databases checked so far.
    pub(super) multi_db: Option<(PathBuf, String, Vec<PathBuf>)>,
    /// How many queries the pack run holds and how many have failed so
    /// far, for the status line; the total is 0 when no pack run is on.
    pub(super) batch: (usize, usize),
    /// The database upgrade or cache cleanup in flight (#578).
    pub(super) upgrade: Option<CodeqlUpgrade>,
    /// The `codeql version` call behind "CodeQL: Copy Version Information"
    /// (#578): its bare version number, or why it could not be read.
    pub(super) version: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// The evaluator log summary being generated (#578): where the outcome
    /// arrives, and the summary file to open once it lands.
    pub(super) log_summary: Option<(std::sync::mpsc::Receiver<Result<(), String>>, PathBuf)>,
    /// The evaluator log viewer being prepared (#578): where the rendered
    /// tree (or why there is none) arrives, and the tab label to show it in.
    pub(super) log_viewer: Option<CodeqlLogViewerJob>,
    /// The Model Editor's endpoints being read (#578): the endpoints or why
    /// not, and the database and language they are for.
    pub(super) model_job: Option<CodeqlModelJob>,
    /// The performance comparison being prepared (#578): where the rendered
    /// table (or why there is none) arrives, and the tab label to show it in.
    pub(super) perf_compare: Option<(std::sync::mpsc::Receiver<Result<String, String>>, String)>,
    /// The query help being rendered (#578): where the outcome arrives,
    /// the Markdown file to preview once it lands, and the query's name.
    pub(super) query_help: Option<CodeqlQueryHelp>,
    /// The pack install or download in flight (#578): where its outcome
    /// arrives, and the status lines for success and failure.
    pub(super) pack_job: Option<CodeqlPackJob>,
    /// A database being downloaded or extracted (#1155): what is being
    /// added, for the status line, and where its folder arrives.
    pub(super) db_job: Option<(String, std::sync::mpsc::Receiver<Result<PathBuf, String>>)>,
    /// A CodeQL document being made (#578: View CFG, the alert and result
    /// views): the file it writes or why not, and the status lines for
    /// each.
    pub(super) doc_job: Option<CodeqlDocJob>,
    /// The CodeQL results table last walked (#578), so the walk goes on
    /// once the code it opened has the focus.
    pub(super) results_walk: Option<PathBuf>,
    /// The AST being read for the AST Viewer (#578): the tree or why not,
    /// and the view it fills in.
    pub(super) ast_job: Option<(
        std::sync::mpsc::Receiver<Result<crate::codeql_ast::AstTree, String>>,
        crate::codeql_ast::AstView,
    )>,
    /// `codeql_cli_path` from settings (#578), kept to notice a reload
    /// changing it.
    pub(super) cli_setting: Option<String>,
    /// Where [`Self::program`] came from (#578).
    pub(super) cli_source: crate::codeql_cli::Source,
    /// The newest CLI release "CodeQL: Check for CLI Updates" found, which
    /// "CodeQL: Download CLI" then installs (#578).
    pub(super) cli_latest: Option<String>,
    /// The CLI download in flight (#578).
    pub(super) cli_download: Option<std::sync::mpsc::Receiver<crate::codeql_cli::JobUpdate>>,
    /// The CLI update check in flight (#578): the latest release and the
    /// version in use, if it could be read.
    pub(super) cli_check: Option<std::sync::mpsc::Receiver<CodeqlCliCheck>>,
    /// The network calls behind those two, replaced in tests.
    pub(super) cli_net: crate::codeql_cli::Net,
    /// The GitHub Code Search in flight: the list its repositories go
    /// into, and its outcome (#578).
    pub(super) code_search: Option<CodeqlCodeSearch>,
    /// The variant analysis being bundled and submitted (#578).
    pub(super) variant_submit:
        Option<std::sync::mpsc::Receiver<Result<crate::codeql_submit::Submitted, String>>>,
    /// The read of unfinished variant analyses' progress in flight, and
    /// when the last one started (#578).
    pub(super) variant_poll: Option<std::sync::mpsc::Receiver<Vec<CodeqlRunPoll>>>,
    pub(super) variant_polled: Option<std::time::Instant>,
    /// The fetch of a variant analysis's results in flight (#578).
    pub(super) variant_results: Option<std::sync::mpsc::Receiver<VariantResults>>,
    /// The export of a variant analysis's results in flight (#578): where
    /// it went (a folder's summary file or a gist's URL), how many
    /// repositories it holds, and those whose results failed.
    pub(super) variant_export: Option<std::sync::mpsc::Receiver<Result<VariantExport, String>>>,
    /// The walk for the workspace's queries in flight (#840): opening the
    /// CodeQL view starts it off the UI thread and the packs land here. It is
    /// tagged with the root it walks, so a root the workspace has left since
    /// never lists its queries.
    pub(super) queries_job: Option<(
        PathBuf,
        std::sync::mpsc::Receiver<Vec<crate::codeql_query::QueryPack>>,
    )>,
}

impl CodeqlJobs {
    /// Nothing running and nothing learned yet; `cli_setting` is the
    /// `codeql_cli_path` setting the CLI is looked up from (#578).
    pub(super) fn new(cli_setting: Option<String>) -> Self {
        let (size_tx, size_rx) = std::sync::mpsc::channel();
        Self {
            size_rx,
            size_tx,
            size_pending: std::collections::HashSet::new(),
            program: PathBuf::from("codeql"),
            run: None,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            run_name: String::new(),
            run_queue: std::collections::VecDeque::new(),
            batch: (0, 0),
            multi_db: None,
            upgrade: None,
            version: None,
            log_summary: None,
            log_viewer: None,
            model_job: None,
            perf_compare: None,
            query_help: None,
            pack_job: None,
            db_job: None,
            ast_job: None,
            doc_job: None,
            results_walk: None,
            cli_setting,
            cli_source: crate::codeql_cli::Source::Fallback,
            cli_latest: None,
            cli_download: None,
            cli_check: None,
            cli_net: crate::codeql_cli::Net::default(),
            code_search: None,
            variant_submit: None,
            variant_poll: None,
            variant_polled: None,
            variant_results: None,
            variant_export: None,
            queries_job: None,
        }
    }
}

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
