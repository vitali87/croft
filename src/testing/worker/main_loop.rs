//! The worker thread's loop: taking each request in turn and handing it to its run or
//! discovery.

use super::*;

pub(super) fn worker_loop(
    mut root: PathBuf,
    rx: Receiver<Sent>,
    tx: Sender<(u64, TestResponse)>,
    control: &RunControl,
) {
    // Found on PATH, as the CodeQL side bar finds it, until the app says
    // otherwise.
    let mut codeql = PathBuf::from("codeql");
    let mut extensions = crate::lsp::manifest::user_extensions_dir();
    let owners = RefCell::new(BTreeMap::new());
    while let Ok(Sent {
        number,
        epoch,
        request,
    }) = rx.recv()
    {
        let etx = EpochTx {
            tx: &tx,
            epoch,
            codeql: &codeql,
            extensions: &extensions,
            control,
            request: number,
            owners: Some(&owners),
            tag: None,
        };
        match request {
            TestRequest::RunAll => run_all(&root, &etx),
            TestRequest::RunCoverage(scope) => run_coverage(&root, &etx, scope.as_ref()),
            TestRequest::RunOne(name) => run_one(&root, &etx, &name),
            TestRequest::RunMany(names) => run_many(&root, &etx, &names),
            TestRequest::RunFilter(pattern, file) => {
                run_filter_in(&root, &etx, &pattern, false, file.as_deref())
            }
            TestRequest::RunSuite(suite) => run_filter(&root, &etx, &suite, true),
            TestRequest::Discover { quiet } => discover(&root, &etx, quiet),
            TestRequest::SetRoot(p) => {
                root = p;
                owners.borrow_mut().clear();
            }
            TestRequest::SetCodeqlProgram(p) => codeql = p,
            TestRequest::SetExtensionsDir(p) => extensions = p,
        }
    }
}
