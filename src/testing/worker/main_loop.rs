//! The worker thread's loop: taking each request in turn and handing it to its run or
//! discovery.

use super::*;

pub(super) fn worker_loop(
    mut root: PathBuf,
    rx: Receiver<TestRequest>,
    tx: Sender<(u64, TestResponse)>,
    control: &RunControl,
) {
    let mut epoch = 0u64;
    // Found on PATH, as the CodeQL side bar finds it, until the app says
    // otherwise.
    let mut codeql = PathBuf::from("codeql");
    let mut extensions = crate::lsp::manifest::user_extensions_dir();
    let mut request = 0u64;
    let owners = RefCell::new(BTreeMap::new());
    while let Ok(req) = rx.recv() {
        request += 1;
        let etx = EpochTx {
            tx: &tx,
            epoch,
            codeql: &codeql,
            extensions: &extensions,
            control,
            request,
            owners: Some(&owners),
            tag: None,
        };
        match req {
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
                epoch += 1;
                owners.borrow_mut().clear();
            }
            TestRequest::SetCodeqlProgram(p) => codeql = p,
            TestRequest::SetExtensionsDir(p) => extensions = p,
        }
    }
}
