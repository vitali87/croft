//! Starting and ending a session: spawning or reaching each adapter (debugpy, lldb-dap,
//! delve, vscode-js-debug, a server already listening), where delve builds the program,
//! the fresh session around its transport, and disconnecting.

use super::*;

impl DapSession {
    /// Spawn the adapter, send `initialize`, and stash the breakpoints to push
    /// when the `initialized` event arrives. `adapter_*` describe how to launch
    /// the DAP adapter (e.g. the debug-venv python with `-m debugpy.adapter`);
    /// `program`/`interpreter` describe the debuggee.
    #[allow(clippy::too_many_arguments)]
    pub fn launch(
        adapter_program: &str,
        adapter_args: &[String],
        cwd: &Path,
        program: &Path,
        interpreter: &Path,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
        stop_on_entry: bool,
    ) -> Result<DapSession> {
        Self::launch_with(
            adapter_program,
            adapter_args,
            cwd,
            launch_request(program, interpreter, stop_on_entry),
            breakpoints,
        )
    }

    /// Generic launch: spawn `adapter_program args...`, send `initialize`, then
    /// the caller-built `launch_request` (debugpy or lldb-dap shaped). The rest
    /// of the protocol (breakpoints, configurationDone, stepping, inspection) is
    /// adapter-agnostic, so every adapter shares this path.
    pub fn launch_with(
        adapter_program: &str,
        adapter_args: &[String],
        cwd: &Path,
        launch_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let transport = DapTransport::spawn(adapter_program, adapter_args, cwd)?;
        transport.send(initialize_request(false))?;
        let attached = is_attach(&launch_request);
        transport.send(launch_request)?;
        let mut session = Self::new_with_transport(transport, breakpoints, None);
        session.attached = attached;
        Ok(session)
    }

    /// Attach to a DAP server that is already listening at `host:port`
    /// (#1417): a `debugpy --listen` process, whose port is its own adapter.
    /// No adapter is spawned; `initialize` and `attach_request` go straight
    /// to the socket.
    pub fn attach_tcp(
        host: &str,
        port: u16,
        attach_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let transport = DapTransport::connect_tcp_once(host, port)?;
        transport.send(initialize_request(false))?;
        transport.send(attach_request)?;
        let mut session = Self::new_with_transport(transport, breakpoints, None);
        session.attached = true;
        Ok(session)
    }

    /// Launch a vscode-js-debug session: spawn `node dapDebugServer.js <port>
    /// <host>` as a TCP debug server, connect the parent socket, and drive the
    /// `pwa-node` launch. The parent will later issue a `startDebugging` reverse
    /// request (handled in [`DapSession::poll`]) that opens the child session
    /// which actually runs `program` and binds breakpoints.
    pub fn launch_js(
        node: &str,
        server_js: &Path,
        cwd: &Path,
        program: &Path,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
        stop_on_entry: bool,
    ) -> Result<DapSession> {
        Self::launch_js_with(
            node,
            server_js,
            cwd,
            js_launch_request(program, cwd, stop_on_entry),
            breakpoints,
        )
    }

    /// Like [`DapSession::launch_js`] but with a caller-built session-starting
    /// request (a launch.json config's `launch` or `attach`), mirroring what
    /// [`DapSession::launch_with`] is to the stdio adapters.
    pub fn launch_js_with(
        node: &str,
        server_js: &Path,
        cwd: &Path,
        start_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let host = "127.0.0.1";
        let port = crate::dap::transport::free_port()?;
        let args = vec![
            server_js.to_string_lossy().into_owned(),
            port.to_string(),
            host.to_string(),
        ];
        let node_dir = crate::dap::install::node_bin_dir(node);
        let transport =
            DapTransport::connect_tcp_server(node, &args, cwd, host, port, node_dir.as_deref())?;
        transport.send(initialize_request(true))?;
        transport.send(start_request)?;
        Ok(Self::new_with_transport(
            transport,
            breakpoints,
            Some((host.to_string(), port)),
        ))
    }

    /// Launch a delve session (#264): spawn `dlv dap --listen=<host:port>`,
    /// connect over TCP, and send `start_request`. delve serves one session
    /// per connection, so unlike js-debug there is no child to open later.
    pub fn launch_delve(
        dlv: &Path,
        cwd: &Path,
        start_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let build_dir = delve_builds(&start_request)
            .then(DebugBuildDir::create)
            .and_then(std::io::Result::ok);
        let start_request = match &build_dir {
            Some(dir) => with_build_output(start_request, dir.path()),
            None => start_request,
        };
        let host = "127.0.0.1";
        let port = crate::dap::transport::free_port()?;
        let args = vec![String::from("dap"), format!("--listen={host}:{port}")];
        let transport =
            // delve runs `go build`: the Go croft found, put on its PATH
            // when croft's own does not reach it (#2807).
            DapTransport::connect_tcp_server(
                &dlv.to_string_lossy(),
                &args,
                cwd,
                host,
                port,
                crate::proc::find::go_dir_off_path().as_deref(),
            )?;
        transport.send(initialize_request(false))?;
        let attached = is_attach(&start_request);
        transport.send(with_remote_output(start_request))?;
        let mut session = Self::new_with_transport(transport, breakpoints, None);
        session.attached = attached;
        session.build_dir = build_dir;
        Ok(session)
    }

    /// Attach to a headless delve that already serves DAP at `host:port`
    /// (#1418, the Go extension's "Connect to server"): no dlv is started,
    /// `initialize` and the `mode: "remote"` attach go straight to the
    /// socket, and stopping disconnects with `terminateDebuggee: false`.
    /// Delve keeps the remote program running only when it was started
    /// with `--accept-multiclient`; a single-client server shuts down with
    /// its debuggee on disconnect.
    pub fn attach_delve_server(
        host: &str,
        port: u16,
        attach_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let transport = DapTransport::connect_tcp(host, port)?;
        transport.send(initialize_request(false))?;
        transport.send(attach_request)?;
        let mut session = Self::new_with_transport(transport, breakpoints, None);
        session.attached = true;
        Ok(session)
    }

    /// Assemble a fresh session around its (parent) `transport`. `js_server` is
    /// set only for vscode-js-debug, marking the session multi-session and
    /// carrying the address its child connection reuses.
    pub(super) fn new_with_transport(
        transport: DapTransport,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
        js_server: Option<(String, u16)>,
    ) -> DapSession {
        DapSession {
            transport,
            child: None,
            js_server,
            phase: SessionPhase::Initializing,
            launch_error: None,
            breakpoints,
            stopped_thread: None,
            threads: Vec::new(),
            current_location: None,
            unverified_breakpoints: BTreeMap::new(),
            stack_frames: Vec::new(),
            scopes: Vec::new(),
            open_expensive_scopes: std::collections::BTreeSet::new(),
            variables: BTreeMap::new(),
            selected_frame: None,
            pending_var_refs: std::collections::HashMap::new(),
            pending_evals: std::collections::HashMap::new(),
            exception_info_seq: None,
            step_in_targets_seq: None,
            exception_filters: vec![String::from("uncaught")],
            pending_raised: None,
            known_thread: None,
            capabilities: Value::Null,
            function_breakpoints: Vec::new(),
            run_to: None,
            data_breakpoints: Vec::new(),
            pending_data_info: std::collections::HashMap::new(),
            attached: false,
            build_dir: None,
        }
    }

    /// Ask the adapter to disconnect and terminate the debuggee. Sent to both the
    /// child (if any) and the parent so a js-debug server tears the whole tree
    /// down, not just the target. An attached session leaves its process
    /// running: croft did not start it, and delve and lldb-dap both honour
    /// `terminateDebuggee` for an attach.
    pub fn disconnect(&mut self) {
        let req = disconnect_request(self.attached);
        if let Some(child) = self.child.as_ref() {
            let _ = child.send(req.clone());
        }
        let _ = self.transport.send(req);
        self.phase = SessionPhase::Terminated;
        self.exception_info_seq = None;
    }
}

/// Whether delve builds the program for this request: a `launch` in
/// `debug`, `test` or `auto` mode (delve's default is `debug`) whose
/// launch.json names no `output`. An `exec`, a core dump or an attach
/// builds nothing (#1206).
pub fn delve_builds(request: &Value) -> bool {
    let args = &request["arguments"];
    request["command"] == "launch"
        && args.get("output").is_none()
        && matches!(
            args["mode"].as_str(),
            None | Some("debug" | "test" | "auto")
        )
}

/// The launch request with delve's `output` set to a `__debug_bin` in `dir`,
/// so the binary it builds lands outside the workspace. Left in the package
/// folder, it stayed behind whenever croft ended delve before delve could
/// delete it, showing in the Explorer and as untracked in git (#1206).
pub fn with_build_output(mut request: Value, dir: &Path) -> Value {
    let bin = dir.join(format!("__debug_bin{}", std::env::consts::EXE_SUFFIX));
    if let Some(args) = request.get_mut("arguments").and_then(Value::as_object_mut) {
        args.insert("output".into(), json!(bin.to_string_lossy()));
    }
    request
}
