//! The debug session's own tests, moved out of session.rs (#2062).

/// #264: an adapter that refuses `launch` (delve rejecting a Go too old
/// for it, with its real payload) ends the session with the reason, on
/// the console and in `launch_error`, instead of leaving it Initializing.
#[cfg(unix)]
#[test]
fn a_refused_launch_ends_the_session_with_the_adapters_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = tmp.path().join("adapter.py");
    std::fs::write(
        &adapter,
        r#"import sys, json
def read():
    n = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line.strip() == b"":
            break
        if line.lower().startswith(b"content-length:"):
            n = int(line.split(b":")[1])
    return json.loads(sys.stdin.buffer.read(n))
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    sys.stdout.buffer.flush()
seq = 0
while True:
    req = read()
    seq += 1
    if req["command"] == "initialize":
        send({"seq": seq, "type": "response", "request_seq": req["seq"], "success": True, "command": "initialize", "body": {}})
    elif req["command"] == "launch":
        send({"seq": seq, "type": "response", "request_seq": req["seq"], "success": False, "command": "launch",
              "message": "Failed to launch /w",
              "body": {"error": {"id": 3000, "showUser": True,
                                 "format": "Failed to launch /w: Go version go1.24.7 is too old for this version of Delve (minimum supported version 1.25, suppress this error with --check-go-version=false)"}}})
    else:
        send({"seq": seq, "type": "response", "request_seq": req["seq"], "success": True, "command": req["command"]})
"#,
    )
    .unwrap();
    let launch =
        json!({"type": "request", "command": "launch", "arguments": {"request": "launch"}});
    let mut session = DapSession::launch_with(
        "python3",
        &[adapter.to_string_lossy().into_owned()],
        tmp.path(),
        launch,
        BTreeMap::new(),
    )
    .unwrap();
    let mut events = Vec::new();
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the adapter's launch response",
        || {
            events.extend(session.poll());
            session.phase == SessionPhase::Terminated
        },
    );
    let reason = session.launch_error.clone().expect("the reason is kept");
    assert!(reason.contains("go1.24.7 is too old"), "{reason}");
    assert!(events.iter().any(|e| matches!(e, DapEvent::Terminated)));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DapEvent::Output { category, text }
            if category == "stderr" && text.contains("minimum supported version 1.25"))),
        "{events:?}"
    );
    // The short `message` is the fallback when there is no `format`.
    let bare = json!({"type": "response", "command": "attach", "success": false, "message": "no such process"});
    assert_eq!(launch_failure_reason(&bare), "no such process");
}

/// #264: zero-config Go debugging runs the file's package; a `_test.go`
/// file runs the package's tests instead.
#[test]
fn delve_zero_config_runs_the_package_or_its_tests() {
    let main = delve_zero_config_request(Path::new("/w/cmd/app/main.go"));
    assert_eq!(main["arguments"]["mode"], "debug");
    assert_eq!(main["arguments"]["program"], "/w/cmd/app");
    let test = delve_zero_config_request(Path::new("/w/pkg/util_test.go"));
    assert_eq!(test["arguments"]["mode"], "test");
    assert_eq!(test["arguments"]["program"], "/w/pkg");
}

/// Every delve launch asks for its program's output as DAP `output`
/// events, whichever builder made it (#1205).
#[test]
fn delve_launches_forward_the_program_output() {
    let zero = with_remote_output(delve_zero_config_request(Path::new("/w/main.go")));
    assert_eq!(zero["arguments"]["outputMode"], "remote");
    let test = with_remote_output(delve_test_request(Path::new("/w"), &[]));
    assert_eq!(test["arguments"]["outputMode"], "remote");
}

/// Negative: a launch.json `outputMode` wins, and an attach (whose
/// process prints wherever it already does) gets none.
#[test]
fn an_explicit_output_mode_or_an_attach_is_left_alone() {
    let local = json!({"command": "launch", "arguments": {"outputMode": "local"}});
    assert_eq!(
        with_remote_output(local)["arguments"]["outputMode"],
        "local"
    );
    let attach = json!({"command": "attach", "arguments": {"processId": 7}});
    assert!(
        with_remote_output(attach)["arguments"]
            .get("outputMode")
            .is_none()
    );
}

/// End to end against a real delve: what the program prints to stdout
/// and stderr reaches the session as `output` events (#1205). Needs Go
/// and `dlv`; run it with `--ignored`.
#[test]
#[ignore]
fn delve_shows_the_program_output() {
    let dlv = std::env::var_os("CROFT_TEST_DLV")
        .map(PathBuf::from)
        .or_else(|| crate::dap::install::dlv_program().ok())
        .expect("dlv");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("go.mod"), "module t\n\ngo 1.21\n").unwrap();
    let main = tmp.path().join("main.go");
    std::fs::write(
        &main,
        "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() {\n\tfmt.Println(\"starting up\")\n\tfmt.Fprintln(os.Stderr, \"warning: done\")\n}\n",
    )
    .unwrap();
    let main = main.canonicalize().unwrap();
    let mut s = DapSession::launch_delve(
        &dlv,
        main.parent().unwrap(),
        delve_zero_config_request(&main),
        BTreeMap::new(),
    )
    .expect("delve starts");
    let mut output = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while s.phase != SessionPhase::Terminated && std::time::Instant::now() < deadline {
        for ev in s.poll() {
            if let DapEvent::Output { text, .. } = ev {
                output.push_str(&text);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(output.contains("starting up"), "stdout missing: {output:?}");
    assert!(
        output.contains("warning: done"),
        "stderr missing: {output:?}"
    );
}

/// End to end against a real delve: a breakpoint in a small Go program
/// binds, the program stops there, and the stop is reported at that line.
/// Needs Go and `dlv` (CROFT_TEST_DLV, or the usual discovery), so it is
/// ignored by default; run it with `--ignored`.
#[test]
#[ignore]
fn delve_stops_at_a_breakpoint_in_a_real_go_program() {
    let dlv = std::env::var_os("CROFT_TEST_DLV")
        .map(PathBuf::from)
        .or_else(|| crate::dap::install::dlv_program().ok())
        .expect("dlv");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("go.mod"), "module t\n\ngo 1.21\n").unwrap();
    let main = tmp.path().join("main.go");
    std::fs::write(
        &main,
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tx := 41\n\tx++\n\tfmt.Println(x)\n}\n",
    )
    .unwrap();
    let main = main.canonicalize().unwrap();
    let mut bps = BTreeMap::new();
    bps.insert(main.clone(), vec![SourceBreakpoint::plain(7)]);
    let mut s = DapSession::launch_delve(
        &dlv,
        main.parent().unwrap(),
        delve_zero_config_request(&main),
        bps,
    )
    .expect("delve starts");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while s.phase != SessionPhase::Stopped && std::time::Instant::now() < deadline {
        s.poll();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(s.phase, SessionPhase::Stopped, "delve never stopped");
    while s.current_location.is_none() && std::time::Instant::now() < deadline {
        s.poll();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let (file, line) = s.current_location.clone().expect("a stop location");
    assert_eq!(line, 7);
    assert_eq!(file.canonicalize().unwrap(), main);
    s.disconnect();
}

/// #264, against a real delve: whatever Go is installed, a launch never
/// sits in Initializing. A Go that delve supports stops at the
/// breakpoint; one too old for it (delve 1.27 wants 1.25+) ends the
/// session at once with delve's own reason. Needs Go and `dlv`.
#[test]
#[ignore]
fn delve_either_stops_or_says_why_it_cannot_start() {
    let dlv = std::env::var_os("CROFT_TEST_DLV")
        .map(PathBuf::from)
        .or_else(|| crate::dap::install::dlv_program().ok())
        .expect("dlv");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("go.mod"), "module t\n\ngo 1.21\n").unwrap();
    let main = tmp.path().join("main.go");
    std::fs::write(
        &main,
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tx := 41\n\tx++\n\tfmt.Println(x)\n}\n",
    )
    .unwrap();
    let main = main.canonicalize().unwrap();
    let mut bps = BTreeMap::new();
    bps.insert(main.clone(), vec![SourceBreakpoint::plain(7)]);
    let mut s = DapSession::launch_delve(
        &dlv,
        main.parent().unwrap(),
        delve_zero_config_request(&main),
        bps,
    )
    .expect("delve starts");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while !matches!(s.phase, SessionPhase::Stopped | SessionPhase::Terminated)
        && std::time::Instant::now() < deadline
    {
        s.poll();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    match s.phase {
        SessionPhase::Stopped => {}
        SessionPhase::Terminated => {
            let why = s.launch_error.clone().expect("a refused launch says why");
            assert!(why.contains("too old"), "{why}");
            eprintln!("delve refused, as expected for this Go: {why}");
        }
        other => panic!("still {other:?} after 90 s"),
    }
    s.disconnect();
}

/// Debug-a-test for Go (#264): delve's test mode scoped with
/// `-test.run` stops in the chosen test, never at a breakpoint in a
/// sibling test that runs first when unscoped. Needs Go and `dlv`, so
/// it is ignored by default; run it with `--ignored`.
#[test]
#[ignore]
fn delve_test_mode_runs_only_the_chosen_test() {
    let dlv = std::env::var_os("CROFT_TEST_DLV")
        .map(PathBuf::from)
        .or_else(|| crate::dap::install::dlv_program().ok())
        .expect("dlv");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("go.mod"), "module t\n\ngo 1.21\n").unwrap();
    let file = tmp.path().join("a_test.go");
    std::fs::write(
        &file,
        "package t\n\nimport \"testing\"\n\n\
             func TestA(t *testing.T) {\n\tt.Log(\"a\")\n}\n\n\
             func TestB(t *testing.T) {\n\tt.Log(\"b\")\n}\n",
    )
    .unwrap();
    let file = file.canonicalize().unwrap();
    let dir = file.parent().unwrap().to_path_buf();
    let mut bps = BTreeMap::new();
    bps.insert(
        file.clone(),
        vec![SourceBreakpoint::plain(6), SourceBreakpoint::plain(10)],
    );
    let request = delve_test_request(&dir, &crate::testing::gotest::test_binary_args(".::TestB"));
    let mut s = DapSession::launch_delve(&dlv, &dir, request, bps).expect("delve starts");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while s.current_location.is_none() && std::time::Instant::now() < deadline {
        s.poll();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let (at, line) = s.current_location.clone().expect("a stop location");
    assert_eq!(line, 10, "stopped in TestB, not TestA");
    assert_eq!(at.canonicalize().unwrap(), file);
    s.disconnect();
}

/// A Go program in its own module under a temp dir, for the delve
/// end-to-end tests: `(dir, canonical main.go)`.
fn go_program(src: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("go.mod"), "module t\n\ngo 1.21\n").unwrap();
    let main = tmp.path().join("main.go");
    std::fs::write(&main, src).unwrap();
    let main = main.canonicalize().unwrap();
    (tmp, main)
}

fn test_dlv() -> PathBuf {
    std::env::var_os("CROFT_TEST_DLV")
        .map(PathBuf::from)
        .or_else(|| crate::dap::install::dlv_program().ok())
        .expect("dlv")
}

/// Poll `s` until `done` holds or 90 s pass, keeping every output line.
fn poll_delve_until(
    s: &mut DapSession,
    output: &mut Vec<String>,
    done: impl Fn(&DapSession) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while !done(s) && std::time::Instant::now() < deadline {
        for ev in s.poll() {
            if let DapEvent::Output { text, .. } = ev {
                output.push(text);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// #264 against a real delve: a conditional breakpoint stops only when
/// its condition holds, and a logpoint prints its interpolated message
/// on every pass without stopping. Needs Go and `dlv`, so it is ignored
/// by default; run it with `--ignored`.
#[test]
#[ignore]
fn delve_honours_conditions_and_logpoints() {
    let (_tmp, main) = go_program(
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\ttotal := 0\n\
             \tfor i := 0; i < 10; i++ {\n\t\ttotal += i\n\t\tfmt.Println(total)\n\t}\n}\n",
    );
    let mut bps = BTreeMap::new();
    bps.insert(
        main.clone(),
        vec![
            SourceBreakpoint {
                line: 8,
                condition: None,
                log_message: Some(String::from("tick {i}")),
                hit_condition: None,
            },
            SourceBreakpoint {
                line: 9,
                condition: Some(String::from("i == 6")),
                log_message: None,
                hit_condition: None,
            },
        ],
    );
    let mut s = DapSession::launch_delve(
        &test_dlv(),
        main.parent().unwrap(),
        delve_zero_config_request(&main),
        bps,
    )
    .expect("delve starts");
    let mut output = Vec::new();
    poll_delve_until(&mut s, &mut output, |s| s.lookup_local("i").is_some());
    let (_, line) = s.current_location.clone().expect("a stop location");
    assert_eq!(line, 9, "stopped on the conditional line, not the logpoint");
    assert_eq!(
        s.lookup_local("i").map(|v| v.value.clone()).as_deref(),
        Some("6"),
        "the condition held at the stop"
    );
    let text = output.concat();
    for i in 0..=6 {
        assert!(
            text.contains(&format!("tick {i}")),
            "logpoint {i}: {text:?}"
        );
    }
    assert!(
        !text.contains("tick 7"),
        "nothing ran past the stop: {text:?}"
    );
    s.disconnect();
}

/// #264 against a real delve: at a stop the thread list is the program's
/// goroutines, and selecting a parked one loads its own stack. Needs Go
/// and `dlv`, so it is ignored by default; run it with `--ignored`.
#[test]
#[ignore]
fn delve_lists_goroutines_and_shows_one_s_stack() {
    let (_tmp, main) = go_program(
        "package main\n\nimport (\n\t\"fmt\"\n\t\"time\"\n)\n\n\
             func worker(c chan int) {\n\tv := <-c\n\tfmt.Println(v)\n}\n\n\
             func main() {\n\tc := make(chan int)\n\tgo worker(c)\n\
             \ttime.Sleep(200 * time.Millisecond)\n\tc <- 1\n\
             \ttime.Sleep(100 * time.Millisecond)\n}\n",
    );
    let mut bps = BTreeMap::new();
    bps.insert(main.clone(), vec![SourceBreakpoint::plain(17)]);
    let mut s = DapSession::launch_delve(
        &test_dlv(),
        main.parent().unwrap(),
        delve_zero_config_request(&main),
        bps,
    )
    .expect("delve starts");
    let mut output = Vec::new();
    poll_delve_until(&mut s, &mut output, |s| {
        s.current_location.is_some() && s.threads.len() > 1
    });
    let (_, line) = s.current_location.clone().expect("a stop location");
    assert_eq!(line, 17);
    let worker = s
        .threads
        .iter()
        .find(|(_, name)| name.contains("main.worker"))
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("the worker goroutine is listed: {:?}", s.threads));
    assert!(
        s.threads.iter().any(|(_, name)| name.contains("main.main")),
        "{:?}",
        s.threads
    );
    s.select_thread(worker);
    poll_delve_until(&mut s, &mut output, |s| {
        s.stack_frames
            .iter()
            .any(|f| f.name.contains("main.worker"))
    });
    let frame = s
        .stack_frames
        .iter()
        .find(|f| f.name.contains("main.worker"))
        .unwrap_or_else(|| panic!("the worker's own stack: {:?}", s.stack_frames));
    assert_eq!(frame.line, 9, "parked on its channel receive");
    assert!(
        // `main.main.gowrap1` is the `go` statement's wrapper, part of the
        // worker's stack; `main.main` itself is the other goroutine's.
        s.stack_frames.iter().all(|f| f.name != "main.main"),
        "the stack is the worker's, not main's: {:?}",
        s.stack_frames
    );
    s.disconnect();
}

/// #264 against a real delve, the first criterion's parity checks:
/// slice, map and struct locals expand to their elements, the debug
/// console evaluates Go, and step in / out / over move where Go does.
/// Needs Go and `dlv`, so it is ignored by default.
#[test]
#[ignore]
fn delve_expands_evaluates_and_steps() {
    let (_tmp, main) = go_program(
        "package main\n\nimport \"fmt\"\n\ntype point struct{ X, Y int }\n\n\
             func double(n int) int {\n\treturn n * 2\n}\n\n\
             func main() {\n\txs := []int{1, 2, 3}\n\tm := map[string]int{\"a\": 1}\n\
             \tp := point{X: 4, Y: 5}\n\td := double(p.X)\n\tfmt.Println(xs, m, p, d)\n}\n",
    );
    let mut bps = BTreeMap::new();
    bps.insert(main.clone(), vec![SourceBreakpoint::plain(15)]);
    let mut s = DapSession::launch_delve(
        &test_dlv(),
        main.parent().unwrap(),
        delve_zero_config_request(&main),
        bps,
    )
    .expect("delve starts");
    let mut out = Vec::new();
    poll_delve_until(&mut s, &mut out, |s| s.lookup_local("p").is_some());
    assert_eq!(s.current_location.clone().map(|l| l.1), Some(15));

    // Each composite local expands to its own children.
    let children = |s: &mut DapSession, out: &mut Vec<String>, name: &str| {
        let r = s.lookup_local(name).expect(name).variables_ref;
        assert!(r > 0, "{name} is expandable");
        s.expand_variable(r);
        poll_delve_until(s, out, |s| s.variables.contains_key(&r));
        s.variables[&r]
            .iter()
            .map(|v| (v.name.clone(), v.value.clone()))
            .collect::<Vec<_>>()
    };
    let xs = children(&mut s, &mut out, "xs");
    assert_eq!(
        xs.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(),
        ["1", "2", "3"],
        "{xs:?}"
    );
    let m = children(&mut s, &mut out, "m");
    assert!(m.iter().any(|(k, v)| k.contains("a") && v == "1"), "{m:?}");
    let p = children(&mut s, &mut out, "p");
    assert!(p.contains(&("X".into(), "4".into())), "{p:?}");
    assert!(p.contains(&("Y".into(), "5".into())), "{p:?}");

    // The debug console evaluates a Go expression in the stopped frame.
    s.evaluate("p.X + len(xs)", "repl");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut result = None;
    while result.is_none() && std::time::Instant::now() < deadline {
        for ev in s.poll() {
            if let DapEvent::Evaluated {
                result: r, success, ..
            } = ev
            {
                result = Some((r, success));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert_eq!(result, Some((String::from("7"), true)));

    // Step in lands in `double`, step out returns to the call, step over
    // finishes the assignment.
    let step = |s: &mut DapSession, out: &mut Vec<String>, cmd: &str| {
        s.step(cmd);
        poll_delve_until(s, out, |s| {
            s.phase == SessionPhase::Running || s.current_location.is_none()
        });
        poll_delve_until(s, out, |s| s.current_location.is_some());
        s.current_location.clone().map(|l| l.1)
    };
    // Delve's unoptimised build stops on the `func` line first.
    let into = step(&mut s, &mut out, "stepIn");
    assert!(matches!(into, Some(7) | Some(8)), "into double: {into:?}");
    poll_delve_until(&mut s, &mut out, |s| !s.stack_frames.is_empty());
    assert_eq!(
        s.stack_frames.first().map(|f| f.name.as_str()),
        Some("main.double")
    );
    let back = step(&mut s, &mut out, "stepOut");
    assert!(
        matches!(back, Some(15) | Some(16)),
        "back in main: {back:?}"
    );
    if back == Some(15) {
        assert_eq!(step(&mut s, &mut out, "next"), Some(16));
    }
    poll_delve_until(&mut s, &mut out, |s| s.lookup_local("d").is_some());
    assert_eq!(
        s.lookup_local("d").map(|v| v.value.clone()).as_deref(),
        Some("8")
    );
    s.disconnect();
}

#[test]
fn run_to_cursor_stops_even_on_a_logpoint_line() {
    let src = PathBuf::from("/w/app.py");
    let logpoint = SourceBreakpoint {
        line: 9,
        condition: None,
        log_message: Some(String::from("x")),
        hit_condition: None,
    };
    let target = (src.clone(), 9);
    let set = with_run_to(Some(&target), &src, std::slice::from_ref(&logpoint));
    assert_eq!(set, vec![SourceBreakpoint::plain(9)]);
    assert_eq!(
        with_run_to(
            Some(&target),
            Path::new("/w/other.py"),
            std::slice::from_ref(&logpoint)
        ),
        vec![logpoint]
    );
}

use super::*;

#[test]
fn stopping_an_attached_session_leaves_its_process_running() {
    let attach = json!({"type": "request", "command": "attach", "arguments": {"processId": 7}});
    let launch = json!({"type": "request", "command": "launch", "arguments": {}});
    assert!(is_attach(&attach));
    assert!(!is_attach(&launch));
    assert_eq!(
        disconnect_request(is_attach(&attach))["arguments"]["terminateDebuggee"],
        false
    );
    assert_eq!(
        disconnect_request(is_attach(&launch))["arguments"]["terminateDebuggee"],
        true
    );
}

#[test]
fn inline_locals_prefer_local_scopes_and_fall_back_to_the_first() {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert(
        1,
        vec![Variable {
            name: String::from("g"),
            value: String::from("0"),
            type_name: String::new(),
            variables_ref: 0,
            evaluate_name: None,
        }],
    );
    vars.insert(
        2,
        vec![Variable {
            name: String::from("x"),
            value: String::from("1"),
            type_name: String::new(),
            variables_ref: 0,
            evaluate_name: None,
        }],
    );
    let scopes = vec![
        Scope {
            name: String::from("Globals"),
            variables_ref: 1,
            expensive: false,
        },
        Scope {
            name: String::from("Locals"),
            variables_ref: 2,
            expensive: false,
        },
    ];
    assert_eq!(
        inline_locals(&scopes, &vars),
        vec![(String::from("x"), String::from("1"))],
        "a Locals scope excludes globals"
    );
    let unnamed = vec![Scope {
        name: String::from("Registers"),
        variables_ref: 1,
        expensive: false,
    }];
    assert_eq!(
        inline_locals(&unnamed, &vars),
        vec![(String::from("g"), String::from("0"))],
        "no local-named scope falls back to the first"
    );
}

#[test]
fn classifies_stopped_event_with_thread_and_reason() {
    let msg = json!({
        "type": "event", "event": "stopped",
        "body": { "threadId": 1, "reason": "breakpoint" }
    });
    assert_eq!(
        classify_event(&msg),
        Some(DapEvent::Stopped {
            thread_id: 1,
            reason: "breakpoint".into()
        })
    );
}

#[test]
fn classifies_initialized_and_terminated() {
    assert_eq!(
        classify_event(&json!({"type": "event", "event": "initialized"})),
        Some(DapEvent::Initialized)
    );
    assert_eq!(
        classify_event(&json!({"type": "event", "event": "terminated"})),
        Some(DapEvent::Terminated)
    );
    assert_eq!(
        classify_event(&json!({"type": "event", "event": "exited"})),
        Some(DapEvent::Terminated)
    );
}

#[test]
fn js_launch_request_is_pwa_node_with_source_maps() {
    let req = js_launch_request(Path::new("/proj/app.ts"), Path::new("/proj"), false);
    assert_eq!(req["command"], "launch");
    assert_eq!(req["arguments"]["type"], "pwa-node");
    assert_eq!(req["arguments"]["request"], "launch");
    assert_eq!(req["arguments"]["program"], "/proj/app.ts");
    assert_eq!(req["arguments"]["cwd"], "/proj");
    // #1414: js-debug's source-map defaults need the workspace folder.
    assert_eq!(req["arguments"]["__workspaceFolder"], "/proj");
    // TypeScript debugging rides on source maps (matches Zed's defaults).
    assert_eq!(req["arguments"]["sourceMaps"], true);
    assert_eq!(req["arguments"]["console"], "internalConsole");
}

#[test]
fn initialize_advertises_start_debugging_support() {
    // js-debug only sends the `startDebugging` reverse request to a client
    // that advertises support for it; without this it never spawns the
    // child session that actually binds breakpoints.
    let req = initialize_request(true);
    assert_eq!(req["arguments"]["supportsStartDebuggingRequest"], true);
    // A session with no child path claims no support (#1417).
    let req = initialize_request(false);
    assert_eq!(req["arguments"]["supportsStartDebuggingRequest"], false);
}

#[test]
fn start_debugging_reverse_request_is_recognised_and_yields_child_config() {
    // Shape captured from the live adapter (vscode-js-debug v1.117.0).
    let msg = json!({
        "type": "request", "seq": 8, "command": "startDebugging",
        "arguments": {
            "request": "launch",
            "configuration": {
                "type": "pwa-node",
                "name": "target.js [60277]",
                "__pendingTargetId": "dd35f91abc4e878814ccfee3"
            }
        }
    });
    let parsed = start_debugging_request(&msg).expect("must recognise startDebugging");
    assert_eq!(parsed.request_seq, 8);
    assert_eq!(
        parsed.config["__pendingTargetId"],
        "dd35f91abc4e878814ccfee3"
    );
    assert_eq!(parsed.config["type"], "pwa-node");
}

#[test]
fn js_child_request_follows_the_configurations_request_kind() {
    let launch_cfg = json!({ "type": "pwa-node", "__pendingTargetId": "x" });
    assert_eq!(
        js_child_launch_request(&launch_cfg, None)["command"],
        "launch"
    );
    let attach_cfg = json!({ "type": "pwa-node", "request": "attach", "__pendingTargetId": "x" });
    let req = js_child_launch_request(&attach_cfg, None);
    assert_eq!(req["command"], "attach");
    assert_eq!(req["arguments"]["__pendingTargetId"], "x");
    // The inner configuration may omit `request`; the reverse request's
    // outer field then decides, so attach children still attach.
    let bare_cfg = json!({ "type": "pwa-node", "__pendingTargetId": "x" });
    assert_eq!(
        js_child_launch_request(&bare_cfg, Some("attach"))["command"],
        "attach"
    );
    // An explicit inner request wins over the outer one.
    assert_eq!(
        js_child_launch_request(&attach_cfg, Some("launch"))["command"],
        "attach"
    );
}

#[test]
fn start_debugging_reverse_request_carries_the_outer_request_kind() {
    let msg = json!({
        "type": "request", "seq": 9, "command": "startDebugging",
        "arguments": {
            "request": "attach",
            "configuration": { "type": "pwa-node", "__pendingTargetId": "t" }
        }
    });
    let parsed = start_debugging_request(&msg).expect("recognised");
    assert_eq!(parsed.request.as_deref(), Some("attach"));
    // No outer request field parses to None rather than being rejected.
    let bare = json!({
        "type": "request", "seq": 10, "command": "startDebugging",
        "arguments": { "configuration": { "type": "pwa-node" } }
    });
    assert_eq!(start_debugging_request(&bare).unwrap().request, None);
}

#[test]
fn non_start_debugging_messages_are_not_mistaken_for_it() {
    assert!(start_debugging_request(&json!({"type": "event", "event": "stopped"})).is_none());
    assert!(
        start_debugging_request(&json!({"type": "request", "seq": 1, "command": "runInTerminal"}))
            .is_none()
    );
}

#[test]
fn lldb_launch_request_carries_binary_not_interpreter() {
    let req = lldb_launch_request(Path::new("/build/app"), false);
    assert_eq!(req["command"], "launch");
    assert_eq!(req["arguments"]["program"], "/build/app");
    // lldb-dap debugs a binary; there is no `python` interpreter field.
    assert!(req["arguments"].get("python").is_none());
}

#[test]
fn set_exception_breakpoints_request_lists_filters() {
    let req =
        set_exception_breakpoints_request(&[String::from("uncaught"), String::from("raised")]);
    assert_eq!(req["command"], "setExceptionBreakpoints");
    assert_eq!(req["arguments"]["filters"][0], "uncaught");
    assert_eq!(req["arguments"]["filters"][1], "raised");
}

#[test]
fn evaluate_request_carries_expression_frame_and_context() {
    let with_frame = evaluate_request("x + 1", Some(42), "repl");
    assert_eq!(with_frame["command"], "evaluate");
    assert_eq!(with_frame["arguments"]["expression"], "x + 1");
    assert_eq!(with_frame["arguments"]["frameId"], 42);
    assert_eq!(with_frame["arguments"]["context"], "repl");
    // No frame selected => no frameId key (global evaluation).
    let no_frame = evaluate_request("len(xs)", None, "watch");
    assert!(no_frame["arguments"].get("frameId").is_none());
}

#[test]
fn parse_evaluate_result_reads_body_result() {
    let resp = json!({ "body": { "result": "3", "type": "int" } });
    assert_eq!(parse_evaluate_result(&resp).as_deref(), Some("3"));
    assert_eq!(parse_evaluate_result(&json!({"body": {}})), None);
}

/// #864: what an exception stop threw, on one line.
#[test]
fn exception_summary_names_the_type_and_message() {
    let debugpy = json!({ "body": {
        "exceptionId": "ModuleNotFoundError",
        "description": "No module named 'requests'",
        "breakMode": "unhandled",
        "details": { "message": "No module named 'requests'", "stackTrace": "..." }
    }});
    assert_eq!(
        exception_summary(&debugpy).as_deref(),
        Some("ModuleNotFoundError: No module named 'requests'")
    );
    // What a real debugpy sends for an uncaught exception it pauses on
    // outside the user's own frames: the note is not part of the name.
    let noted = json!({ "body": {
        "exceptionId": "ValueError       (note: full exception trace is shown but \
                            execution is paused at: _run_module_as_main)",
        "description": "boom 42"
    }});
    assert_eq!(
        exception_summary(&noted).as_deref(),
        Some("ValueError: boom 42")
    );
    // A description that already names the type, followed by the stack
    // (js-debug's shape), is not prefixed twice or run onto one line.
    let js = json!({ "body": {
        "exceptionId": "Error",
        "description": "Error: boom\n    at main (/w/app.js:3:9)"
    }});
    assert_eq!(exception_summary(&js).as_deref(), Some("Error: boom"));
    let only_details = json!({ "body": {
        "exceptionId": "panic",
        "details": { "message": "index out of range" }
    }});
    assert_eq!(
        exception_summary(&only_details).as_deref(),
        Some("panic: index out of range")
    );
    let only_id = json!({ "body": { "exceptionId": "KeyError", "description": "" } });
    assert_eq!(exception_summary(&only_id).as_deref(), Some("KeyError"));
    assert_eq!(exception_summary(&json!({ "body": {} })), None);
    assert_eq!(exception_summary(&json!({ "success": false })), None);
}

#[test]
fn reverse_request_response_references_request_seq() {
    let r = reverse_request_response(42, "runInTerminal", false);
    assert_eq!(r["type"], "response");
    assert_eq!(r["request_seq"], 42);
    assert_eq!(r["command"], "runInTerminal");
    assert_eq!(r["success"], false);
    assert_eq!(r["message"], "unsupported by croft");
    // A success response carries no message.
    let ok = reverse_request_response(1, "startDebugging", true);
    assert_eq!(ok["success"], true);
    assert!(ok["message"].is_null());
}

#[test]
fn breakpoint_reports_parses_response_array() {
    let msg = json!({
        "type": "response", "command": "setBreakpoints",
        "body": { "breakpoints": [
            { "verified": true, "line": 3, "source": { "path": "/a.py" } },
            { "verified": false, "line": 7, "source": { "path": "/a.py" } },
        ]}
    });
    let reports = breakpoint_reports(&msg);
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[1].line, 7);
    assert!(!reports[1].verified);
}

#[test]
fn breakpoint_reports_parses_singular_event_and_skips_line_zero() {
    let event = json!({
        "type": "event", "event": "breakpoint",
        "body": { "reason": "changed",
            "breakpoint": { "verified": true, "line": 5, "source": { "path": "/b.py" } } }
    });
    let reports = breakpoint_reports(&event);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].path, PathBuf::from("/b.py"));

    // line: 0 (not yet resolved) is skipped, not reported as line 0.
    let unresolved = json!({
        "type": "response", "command": "setBreakpoints",
        "body": { "breakpoints": [
            { "verified": true, "line": 0, "source": { "path": "/c.py" } } ]}
    });
    assert!(breakpoint_reports(&unresolved).is_empty());
}

#[test]
fn a_breakpoint_event_updates_only_its_own_line() {
    let p = PathBuf::from("/a.py");
    let report = |line, verified| BreakpointReport {
        path: p.clone(),
        line,
        verified,
    };
    let mut map = BTreeMap::new();
    fold_breakpoint_reports(&mut map, &[report(3, false), report(7, false)]);
    // Line 3 verifies: line 7 stays unverified.
    assert!(update_breakpoint_lines(&mut map, &[report(3, true)]));
    assert_eq!(map[&p], [7].into_iter().collect());
    // A new unverified line joins the set rather than replacing it.
    assert!(update_breakpoint_lines(&mut map, &[report(9, false)]));
    assert_eq!(map[&p], [7, 9].into_iter().collect());
    assert!(!update_breakpoint_lines(&mut map, &[report(9, false)]));
    update_breakpoint_lines(&mut map, &[report(7, true), report(9, true)]);
    assert!(!map.contains_key(&p), "an empty set is dropped");
    // Verifying a line never tracked changes nothing and adds no entry.
    assert!(!update_breakpoint_lines(&mut map, &[report(1, true)]));
    assert!(map.is_empty());
}

#[test]
fn fold_breakpoint_reports_tracks_only_unverified() {
    let mut map = BTreeMap::new();
    let p = PathBuf::from("/a.py");
    // One verified, one not: only the unverified line is tracked.
    let changed = fold_breakpoint_reports(
        &mut map,
        &[
            BreakpointReport {
                path: p.clone(),
                line: 3,
                verified: true,
            },
            BreakpointReport {
                path: p.clone(),
                line: 7,
                verified: false,
            },
        ],
    );
    assert!(changed);
    assert_eq!(
        map.get(&p).unwrap().iter().copied().collect::<Vec<_>>(),
        vec![7]
    );
    // A later report verifying line 7 clears it (and the now-empty entry).
    let changed = fold_breakpoint_reports(
        &mut map,
        &[BreakpointReport {
            path: p.clone(),
            line: 7,
            verified: true,
        }],
    );
    assert!(changed);
    assert!(!map.contains_key(&p));
    // No reports => no change.
    assert!(!fold_breakpoint_reports(&mut map, &[]));
}

#[test]
fn telemetry_output_is_filtered_but_program_output_is_kept() {
    // debugpy's `ptvsd` / `debugpy` banner arrives as category `telemetry`.
    assert!(!output_is_user_visible("telemetry"));
    assert!(output_is_user_visible("stdout"));
    assert!(output_is_user_visible("stderr"));
    assert!(output_is_user_visible("console"));
}

#[test]
fn classifies_output_event() {
    let msg = json!({
        "type": "event", "event": "output",
        "body": { "category": "stdout", "output": "hello\n" }
    });
    assert_eq!(
        classify_event(&msg),
        Some(DapEvent::Output {
            category: "stdout".into(),
            text: "hello\n".into()
        })
    );
}

#[test]
fn ignores_responses_and_unknown_events() {
    assert_eq!(
        classify_event(&json!({"type": "response", "command": "initialize"})),
        None
    );
    assert_eq!(
        classify_event(&json!({"type": "event", "event": "module"})),
        None
    );
}

#[test]
fn set_breakpoints_request_maps_lines_to_objects() {
    let req = set_breakpoints_request(
        Path::new("/a/b.py"),
        &[
            SourceBreakpoint::plain(2),
            SourceBreakpoint {
                line: 7,
                condition: Some(String::from("i > 3")),
                log_message: None,
                hit_condition: None,
            },
        ],
    );
    assert_eq!(req["command"], "setBreakpoints");
    assert_eq!(req["arguments"]["source"]["path"], "/a/b.py");
    let bps = req["arguments"]["breakpoints"].as_array().unwrap();
    assert_eq!(bps.len(), 2);
    assert_eq!(bps[0]["line"], 2);
    assert!(bps[0].get("condition").is_none());
    assert_eq!(bps[1]["line"], 7);
    assert_eq!(bps[1]["condition"], "i > 3");
}

/// A fake debug adapter (#611): logs every request it receives, one
/// JSON object per line, to the path in argv[1]; advertises function and
/// data breakpoints; stops once on configurationDone and again on each
/// continue; answers dataBreakpointInfo with a data id.
const FAKE_DAP: &str = r#"
import json, sys
log = open(sys.argv[1], "a")
seq = [1000]
def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    return json.loads(sys.stdin.buffer.read(length))
def send(msg):
    seq[0] += 1
    msg["seq"] = seq[0]
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()
def reply(req, body=None):
    send({"type": "response", "request_seq": req["seq"], "success": True,
          "command": req["command"], "body": body or {}})
def stopped():
    send({"type": "event", "event": "stopped", "body": {"reason": "breakpoint", "threadId": 1}})
while True:
    req = read()
    if req is None:
        break
    log.write(json.dumps({"command": req["command"], "arguments": req.get("arguments")}) + "\n")
    log.flush()
    c = req["command"]
    if c == "initialize":
        reply(req, {"supportsFunctionBreakpoints": True, "supportsDataBreakpoints": True,
                    "supportsHitConditionalBreakpoints": True})
        send({"type": "event", "event": "initialized"})
    elif c == "configurationDone":
        reply(req)
        stopped()
    elif c == "continue":
        reply(req)
        stopped()
    elif c == "dataBreakpointInfo":
        reply(req, {"dataId": "var:" + req["arguments"]["name"], "description": "x"})
    elif c == "disconnect":
        reply(req)
        break
    else:
        reply(req)
"#;

fn requests(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn poll_until(session: &mut DapSession, log: &Path, done: impl Fn(&[Value]) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        session.poll();
        if done(&requests(log)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out; requests so far: {:?}", requests(log));
}

/// The fake adapter, answering `initialize` with `filters` (a Python
/// literal) as its `exceptionBreakpointFilters`, or with none when
/// `filters` is empty; polled until it has had `configurationDone`.
fn exception_filter_session(filters: &str) -> Option<(DapSession, tempfile::TempDir, PathBuf)> {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()?;
    let caps = if filters.is_empty() {
        String::from("{}")
    } else {
        format!("{{\"exceptionBreakpointFilters\": {filters}}}")
    };
    let script = FAKE_DAP.replace(
        r#"reply(req, {"supportsFunctionBreakpoints": True, "supportsDataBreakpoints": True,
                    "supportsHitConditionalBreakpoints": True})"#,
        &format!("reply(req, {caps})"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fake_dap.py");
    std::fs::write(&path, script).unwrap();
    let log = tmp.path().join("requests.jsonl");
    let mut session = DapSession::launch_with(
        "python3",
        &[path.display().to_string(), log.display().to_string()],
        tmp.path(),
        json!({"type": "request", "command": "launch", "arguments": {}}),
        BTreeMap::new(),
    )
    .expect("fake adapter spawns");
    poll_until(&mut session, &log, |r| {
        r.iter().any(|q| q["command"] == "configurationDone")
    });
    Some((session, tmp, log))
}

/// Every `setExceptionBreakpoints` filter list the adapter got, in order.
fn sent_exception_filters(log: &Path) -> Vec<Value> {
    requests(log)
        .into_iter()
        .filter(|q| q["command"] == "setExceptionBreakpoints")
        .map(|q| q["arguments"]["filters"].clone())
        .collect()
}

/// #1426: js-debug calls its caught-exceptions filter `all`, so the
/// raised-exceptions toggle sends `all` there, not debugpy's `raised`.
#[test]
fn the_raised_exceptions_toggle_uses_js_debugs_all_filter() {
    let Some((mut session, _tmp, log)) = exception_filter_session(
        r#"[{"filter": "all", "label": "Caught Exceptions", "default": False},
                {"filter": "uncaught", "label": "Uncaught Exceptions", "default": False}]"#,
    ) else {
        return;
    };
    assert_eq!(
        session.raised_exception_filters(),
        Some(vec![String::from("all")])
    );
    assert_eq!(session.toggle_raised_exceptions(), Some(true));
    poll_until(&mut session, &log, |_| {
        sent_exception_filters(&log).len() >= 2
    });
    assert_eq!(session.toggle_raised_exceptions(), Some(false));
    poll_until(&mut session, &log, |_| {
        sent_exception_filters(&log).len() >= 3
    });
    assert_eq!(
        sent_exception_filters(&log),
        vec![
            json!(["uncaught"]),
            json!(["uncaught", "all"]),
            json!(["uncaught"])
        ]
    );
}

/// #1426: lldb-dap knows neither `uncaught` nor `raised`. croft stops
/// sending ids it does not offer, keeps the ones it turns on by
/// default, and the toggle sends `cpp_throw`.
#[test]
fn lldb_dap_gets_its_own_filters_and_cpp_throw_for_raised() {
    let Some((mut session, _tmp, log)) = exception_filter_session(
        r#"[{"filter": "cpp_catch", "label": "C++ Catch", "default": False},
                {"filter": "cpp_throw", "label": "C++ Throw", "default": False},
                {"filter": "rust_panic", "label": "Rust Panic", "default": True}]"#,
    ) else {
        return;
    };
    assert_eq!(
        session.raised_exception_filters(),
        Some(vec![String::from("cpp_throw")])
    );
    assert_eq!(session.toggle_raised_exceptions(), Some(true));
    poll_until(&mut session, &log, |_| {
        sent_exception_filters(&log).len() >= 2
    });
    assert_eq!(
        sent_exception_filters(&log),
        vec![json!(["rust_panic"]), json!(["rust_panic", "cpp_throw"])]
    );
}

/// #1426: lldb-dap offers `cpp_throw` and `objc_throw` side by side; the
/// toggle turns both on so Objective-C(++) `@throw` stops too.
#[test]
fn lldb_dap_raised_toggle_covers_objective_c_throws() {
    let Some((mut session, _tmp, log)) = exception_filter_session(
        r#"[{"filter": "cpp_catch", "label": "C++ Catch", "default": False},
                {"filter": "cpp_throw", "label": "C++ Throw", "default": False},
                {"filter": "objc_catch", "label": "Objective-C Catch", "default": False},
                {"filter": "objc_throw", "label": "Objective-C Throw", "default": False}]"#,
    ) else {
        return;
    };
    assert_eq!(session.toggle_raised_exceptions(), Some(true));
    poll_until(&mut session, &log, |_| {
        sent_exception_filters(&log).len() >= 2
    });
    assert_eq!(session.toggle_raised_exceptions(), Some(false));
    poll_until(&mut session, &log, |_| {
        sent_exception_filters(&log).len() >= 3
    });
    assert_eq!(
        sent_exception_filters(&log),
        vec![json!([]), json!(["cpp_throw", "objc_throw"]), json!([])]
    );
}

/// #1426: an adapter with no exception filters (Delve) has nothing to
/// toggle; nothing is sent beyond the startup request, and debugpy's
/// own filter list still maps to `raised`.
#[test]
fn an_adapter_without_exception_filters_has_nothing_to_toggle() {
    let Some((mut session, _tmp, log)) = exception_filter_session("") else {
        return;
    };
    assert_eq!(session.raised_exception_filters(), None);
    assert_eq!(session.toggle_raised_exceptions(), None);
    session.poll();
    std::thread::sleep(std::time::Duration::from_millis(200));
    session.poll();
    assert_eq!(sent_exception_filters(&log), vec![json!(["uncaught"])]);

    let Some((session, _tmp, _log)) = exception_filter_session(
        r#"[{"filter": "raised", "label": "Raised Exceptions", "default": False},
                {"filter": "uncaught", "label": "Uncaught Exceptions", "default": True},
                {"filter": "userUnhandled", "label": "User Uncaught Exceptions", "default": False}]"#,
    ) else {
        return;
    };
    assert_eq!(
        session.raised_exception_filters(),
        Some(vec![String::from("raised")])
    );
    assert_eq!(session.exception_filters, vec![String::from("uncaught")]);
}

/// #1421 against a fake adapter whose Global scope is `expensive`: a
/// stop fetches only the Local scope; opening Global fetches it, and it
/// stays open (and fetched) at the next stop until it is closed.
#[test]
fn an_expensive_scope_is_fetched_only_once_opened() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let script = FAKE_DAP.replace(
        "    elif c == \"disconnect\":",
        r#"    elif c == "stackTrace":
        reply(req, {"stackFrames": [{"id": 5, "name": "work", "line": 4,
                    "source": {"path": "/w/big.js"}}]})
    elif c == "scopes":
        reply(req, {"scopes": [
            {"name": "Local: work", "variablesReference": 10, "expensive": False},
            {"name": "Global", "variablesReference": 20, "expensive": True}]})
    elif c == "variables":
        ref = req["arguments"]["variablesReference"]
        reply(req, {"variables": [{"name": "v%d" % ref, "value": "1", "variablesReference": 0}]})
    elif c == "disconnect":"#,
    );
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fake_dap.py");
    std::fs::write(&path, script).unwrap();
    let log = tmp.path().join("requests.jsonl");
    let mut session = DapSession::launch_with(
        "python3",
        &[path.display().to_string(), log.display().to_string()],
        tmp.path(),
        json!({"type": "request", "command": "launch", "arguments": {}}),
        BTreeMap::new(),
    )
    .expect("fake adapter spawns");
    let fetched = |log: &Path| -> Vec<i64> {
        requests(log)
            .iter()
            .filter(|q| q["command"] == "variables")
            .filter_map(|q| q["arguments"]["variablesReference"].as_i64())
            .collect()
    };
    let scopes_asked = |log: &Path| {
        requests(log)
            .iter()
            .filter(|q| q["command"] == "scopes")
            .count()
    };
    poll_until(&mut session, &log, |_| fetched(&log).len() == 1);
    // Let any late request land before asserting it never went out.
    for _ in 0..20 {
        session.poll();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(fetched(&log), vec![10], "only the Local scope");
    let global = session
        .scopes
        .iter()
        .position(|s| s.name == "Global")
        .unwrap();
    assert!(session.scopes[global].expensive);
    assert!(!session.scope_open(global));

    session.toggle_expensive_scope(global);
    poll_until(&mut session, &log, |_| fetched(&log) == vec![10, 20]);
    assert!(session.scope_open(global));

    // The next stop fetches both: Global stays open.
    session.continue_execution();
    poll_until(&mut session, &log, |_| fetched(&log).len() == 4);
    assert_eq!(fetched(&log), vec![10, 20, 10, 20]);

    // Closed again, the stop after that skips it.
    session.toggle_expensive_scope(global);
    session.continue_execution();
    poll_until(&mut session, &log, |_| {
        scopes_asked(&log) == 3 && fetched(&log).len() == 5
    });
    for _ in 0..20 {
        session.poll();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(fetched(&log), vec![10, 20, 10, 20, 10]);
}

/// #264 against a fake adapter with two goroutines: a stop lists both,
/// selecting the other shows its own stack, and an attached session's
/// disconnect leaves the process running.
#[test]
fn goroutines_list_and_select_and_an_attach_disconnects_softly() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let script = FAKE_DAP.replace(
        "    elif c == \"disconnect\":",
        r#"    elif c == "threads":
        reply(req, {"threads": [{"id": 1, "name": "main"}, {"id": 7, "name": "worker"}]})
    elif c == "stackTrace":
        tid = req["arguments"]["threadId"]
        name = "main.run" if tid == 1 else "main.work"
        reply(req, {"stackFrames": [{"id": tid * 100, "name": name, "line": tid,
                    "source": {"path": "/w/main.go"}}]})
    elif c == "disconnect":"#,
    );
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fake_dap.py");
    std::fs::write(&path, script).unwrap();
    let log = tmp.path().join("requests.jsonl");
    let mut session = DapSession::launch_with(
        "python3",
        &[path.display().to_string(), log.display().to_string()],
        tmp.path(),
        json!({"type": "request", "command": "attach", "arguments": {"processId": 77}}),
        BTreeMap::new(),
    )
    .expect("fake adapter spawns");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while (session.threads.len() < 2 || session.stack_frames.is_empty())
        && std::time::Instant::now() < deadline
    {
        session.poll();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        session.threads,
        vec![(1, String::from("main")), (7, String::from("worker"))]
    );
    assert_eq!(session.stack_frames[0].name, "main.run");

    session.select_thread(7);
    assert_eq!(session.stopped_thread, Some(7));
    while session.stack_frames.is_empty() && std::time::Instant::now() < deadline {
        session.poll();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        session.stack_frames[0].name, "main.work",
        "the goroutine's own stack"
    );

    session.disconnect();
    poll_until(&mut session, &log, |r| {
        r.iter().any(|q| q["command"] == "disconnect")
    });
    let disc = requests(&log)
        .into_iter()
        .find(|q| q["command"] == "disconnect")
        .unwrap();
    assert_eq!(
        disc["arguments"]["terminateDebuggee"], false,
        "croft did not start it"
    );
}

/// #611 end to end against a fake adapter: function breakpoints and hit
/// counts go out on `initialized`, Run to Cursor adds a one-stop
/// breakpoint and withdraws it at the next stop, and Break When Value
/// Changes turns a variable into a data breakpoint.
#[test]
fn breakpoint_kinds_reach_the_adapter() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("fake_dap.py");
    std::fs::write(&script, FAKE_DAP).unwrap();
    let log = tmp.path().join("requests.jsonl");
    let src = tmp.path().join("app.py");
    let mut bps = BTreeMap::new();
    bps.insert(
        src.clone(),
        vec![SourceBreakpoint {
            line: 3,
            condition: None,
            log_message: None,
            hit_condition: Some(String::from("5")),
        }],
    );
    let mut session = DapSession::launch_with(
        "python3",
        &[script.display().to_string(), log.display().to_string()],
        tmp.path(),
        json!({"type": "request", "command": "launch", "arguments": {}}),
        bps,
    )
    .expect("fake adapter spawns");
    session.set_function_breakpoints(vec![String::from("main")]);
    poll_until(&mut session, &log, |r| {
        r.iter().any(|q| q["command"] == "configurationDone")
    });
    let reqs = requests(&log);
    let fbp = reqs
        .iter()
        .find(|q| q["command"] == "setFunctionBreakpoints")
        .expect("sent");
    assert_eq!(fbp["arguments"]["breakpoints"][0]["name"], "main");
    let sbp = reqs
        .iter()
        .find(|q| q["command"] == "setBreakpoints")
        .expect("sent");
    assert_eq!(sbp["arguments"]["breakpoints"][0]["hitCondition"], "5");
    assert!(session.supports("supportsDataBreakpoints"));
    assert!(!session.supports("supportsStepBack"));

    // Run to Cursor: line 9 joins the file's set, then continue.
    while session.stopped_thread.is_none() {
        session.poll();
    }
    session.run_to_cursor(&src, 9);
    poll_until(&mut session, &log, |r| {
        r.iter()
            .filter(|q| q["command"] == "setBreakpoints")
            .count()
            >= 3
    });
    let sets: Vec<Value> = requests(&log)
        .into_iter()
        .filter(|q| q["command"] == "setBreakpoints")
        .collect();
    let lines = |q: &Value| -> Vec<u64> {
        q["arguments"]["breakpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["line"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(
        lines(&sets[1]),
        vec![3, 9],
        "the one-stop breakpoint is added"
    );
    assert_eq!(lines(&sets[2]), vec![3], "and withdrawn at the stop");

    // Break When Value Changes.
    session.break_on_change(42, "total");
    poll_until(&mut session, &log, |r| {
        r.iter().any(|q| q["command"] == "setDataBreakpoints")
    });
    let dbp = requests(&log)
        .into_iter()
        .find(|q| q["command"] == "setDataBreakpoints")
        .unwrap();
    assert_eq!(dbp["arguments"]["breakpoints"][0]["dataId"], "var:total");
    assert_eq!(
        session.data_breakpoints,
        vec![(String::from("var:total"), String::from("total"))]
    );
    session.disconnect();
}

/// #864 against a fake adapter: an `exception` stop asks the adapter
/// what was thrown, and the answer comes back as one event.
#[test]
fn an_exception_stop_reports_what_was_thrown() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let script = FAKE_DAP
        .replace(r#""reason": "breakpoint""#, r#""reason": "exception""#)
        .replace(
            r#""supportsHitConditionalBreakpoints": True"#,
            r#""supportsHitConditionalBreakpoints": True, "supportsExceptionInfoRequest": True"#,
        )
        .replace(
            "    elif c == \"disconnect\":",
            r#"    elif c == "exceptionInfo":
        reply(req, {"exceptionId": "ModuleNotFoundError",
                    "description": "No module named 'requests'", "breakMode": "unhandled"})
    elif c == "disconnect":"#,
        );
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fake_dap.py");
    std::fs::write(&path, script).unwrap();
    let log = tmp.path().join("requests.jsonl");
    let mut session = DapSession::launch_with(
        "python3",
        &[path.display().to_string(), log.display().to_string()],
        tmp.path(),
        json!({"type": "request", "command": "launch", "arguments": {}}),
        BTreeMap::new(),
    )
    .expect("fake adapter spawns");
    let mut summary = None;
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the fake adapter to answer exceptionInfo",
        || {
            for ev in session.poll() {
                if let DapEvent::ExceptionInfo { summary: s } = ev {
                    summary = Some(s);
                }
            }
            summary.is_some()
        },
    );
    let asked = requests(&log)
        .into_iter()
        .find(|q| q["command"] == "exceptionInfo")
        .expect("exceptionInfo was requested");
    assert_eq!(asked["arguments"]["threadId"], 1);
    assert_eq!(
        summary.as_deref(),
        Some("ModuleNotFoundError: No module named 'requests'")
    );
    session.disconnect();
}

/// Run FAKE_DAP to one stop for `reason`, its `initialize` advertising
/// `supportsExceptionInfoRequest` or not, then disconnect. Returns every
/// request the adapter saw and every `ExceptionInfo` the session raised.
fn one_stop_then_disconnect(
    reason: &str,
    supports_exception_info: bool,
) -> (Vec<Value>, Vec<String>) {
    let mut script = FAKE_DAP
        .replace(
            r#""reason": "breakpoint""#,
            &format!(r#""reason": "{reason}""#),
        )
        .replace(
            "    elif c == \"disconnect\":",
            r#"    elif c == "exceptionInfo":
        reply(req, {"exceptionId": "KeyError", "description": "'x'"})
    elif c == "disconnect":"#,
        );
    if supports_exception_info {
        script = script.replace(
            r#""supportsHitConditionalBreakpoints": True"#,
            r#""supportsHitConditionalBreakpoints": True, "supportsExceptionInfoRequest": True"#,
        );
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fake_dap.py");
    std::fs::write(&path, script).unwrap();
    let log = tmp.path().join("requests.jsonl");
    let mut session = DapSession::launch_with(
        "python3",
        &[path.display().to_string(), log.display().to_string()],
        tmp.path(),
        json!({"type": "request", "command": "launch", "arguments": {}}),
        BTreeMap::new(),
    )
    .expect("fake adapter spawns");
    let mut summaries = Vec::new();
    let mut drain = |session: &mut DapSession| {
        for ev in session.poll() {
            if let DapEvent::ExceptionInfo { summary } = ev {
                summaries.push(summary);
            }
        }
    };
    // The stop's follow-ups all go out together, `threads` among them.
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the fake adapter to stop",
        || {
            drain(&mut session);
            requests(&log).iter().any(|q| q["command"] == "threads")
        },
    );
    // Anything the stop sent is logged before the disconnect that follows.
    session.disconnect();
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the fake adapter to log the disconnect",
        || {
            drain(&mut session);
            requests(&log).iter().any(|q| q["command"] == "disconnect")
        },
    );
    (requests(&log), summaries)
}

/// #864 guard: only an `exception` stop asks what was thrown. A
/// breakpoint stop, from an adapter that does support `exceptionInfo`,
/// sends no such request and reports no exception.
#[test]
fn a_breakpoint_stop_does_not_ask_what_was_thrown() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let (reqs, summaries) = one_stop_then_disconnect("breakpoint", true);
    assert!(
        !reqs.iter().any(|q| q["command"] == "exceptionInfo"),
        "{reqs:?}"
    );
    assert!(summaries.is_empty(), "{summaries:?}");
}

/// #864 guard: an adapter that does not advertise
/// `supportsExceptionInfoRequest` is not sent one on an exception stop,
/// which it would answer with an error.
#[test]
fn an_exception_stop_skips_exception_info_the_adapter_lacks() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let (reqs, summaries) = one_stop_then_disconnect("exception", false);
    assert!(
        reqs.iter().any(|q| q["command"] == "stackTrace"),
        "the stop was handled: {reqs:?}"
    );
    assert!(
        !reqs.iter().any(|q| q["command"] == "exceptionInfo"),
        "{reqs:?}"
    );
    assert!(summaries.is_empty(), "{summaries:?}");
}

/// FAKE_DAP stopping on an exception and advertising `exceptionInfo`,
/// with `arms` (Python `elif` arms) in place of its `continue` arm: they
/// decide when each held `exceptionInfo` request is answered, through
/// `answer(req, name, text)`, and send `marker()`, an output line after
/// which every earlier reply has been read.
fn held_exception_info_session(arms: &str) -> (tempfile::TempDir, PathBuf, DapSession) {
    let script = FAKE_DAP
        .replace(r#""reason": "breakpoint""#, r#""reason": "exception""#)
        .replace(
            r#""supportsHitConditionalBreakpoints": True"#,
            r#""supportsHitConditionalBreakpoints": True, "supportsExceptionInfoRequest": True"#,
        )
        .replace(
            "def stopped():",
            r#"held = []
def answer(req, name, text):
    reply(req, {"exceptionId": name, "description": text})
def marker():
    send({"type": "event", "event": "output", "body": {"category": "stdout", "output": "MARKER\n"}})
def stopped():"#,
        )
        .replace(
            "    elif c == \"continue\":\n        reply(req)\n        stopped()\n",
            arms,
        );
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fake_dap.py");
    std::fs::write(&path, script).unwrap();
    let log = tmp.path().join("requests.jsonl");
    let session = DapSession::launch_with(
        "python3",
        &[path.display().to_string(), log.display().to_string()],
        tmp.path(),
        json!({"type": "request", "command": "launch", "arguments": {}}),
        BTreeMap::new(),
    )
    .expect("fake adapter spawns");
    (tmp, log, session)
}

/// A fake adapter paused at frame 7 of thread 1 on a line with three
/// calls (#1543), advertising `supportsStepInTargetsRequest` or not.
fn step_in_targets_session(supported: bool) -> (tempfile::TempDir, PathBuf, DapSession) {
    let mut script = FAKE_DAP.replace(
        "    elif c == \"dataBreakpointInfo\":",
        r#"    elif c == "stackTrace":
        reply(req, {"stackFrames": [{"id": 7, "name": "<module>", "line": 13, "column": 1, "source": {"path": "/tmp/app.py"}}], "totalFrames": 1})
    elif c == "stepInTargets":
        reply(req, {"targets": [{"id": 1, "label": "load('data.csv')"}, {"id": 2, "label": "scale(2)"}, {"id": 3, "label": "transform(...)"}]})
    elif c == "dataBreakpointInfo":"#,
    );
    if supported {
        script = script.replace(
            r#""supportsHitConditionalBreakpoints": True"#,
            r#""supportsHitConditionalBreakpoints": True, "supportsStepInTargetsRequest": True"#,
        );
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fake_dap.py");
    std::fs::write(&path, script).unwrap();
    let log = tmp.path().join("requests.jsonl");
    let mut session = DapSession::launch_with(
        "python3",
        &[path.display().to_string(), log.display().to_string()],
        tmp.path(),
        json!({"type": "request", "command": "launch", "arguments": {}}),
        BTreeMap::new(),
    )
    .expect("fake adapter spawns");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !(session.stopped_thread.is_some() && !session.stack_frames.is_empty()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the fake never paused"
        );
        session.poll();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    (tmp, log, session)
}

fn python3_missing() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
}

/// #1543: Step Into Target asks for the paused frame's calls, surfaces
/// them in the adapter's order, and steps into the chosen one by its
/// `targetId` rather than into the first call to run.
#[test]
fn step_into_target_lists_the_lines_calls_and_enters_the_chosen_one() {
    if python3_missing() {
        return;
    }
    let (_tmp, log, mut session) = step_in_targets_session(true);
    assert_eq!(session.request_step_in_targets(), StepInTargetsAsk::Sent);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let targets = loop {
        assert!(std::time::Instant::now() < deadline, "no StepInTargets");
        if let Some(DapEvent::StepInTargets { targets }) = session
            .poll()
            .into_iter()
            .find(|e| matches!(e, DapEvent::StepInTargets { .. }))
        {
            break targets;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(
        targets,
        [
            (1, String::from("load('data.csv')")),
            (2, String::from("scale(2)")),
            (3, String::from("transform(...)")),
        ]
    );
    let asked = requests(&log);
    let ask = asked
        .iter()
        .find(|q| q["command"] == "stepInTargets")
        .expect("stepInTargets sent");
    assert_eq!(ask["arguments"]["frameId"], 7, "for the paused frame");
    session.step_in_target(2);
    poll_until(&mut session, &log, |qs| {
        qs.iter().any(|q| {
            q["command"] == "stepIn"
                && q["arguments"]["targetId"] == 2
                && q["arguments"]["threadId"] == 1
        })
    });
    session.disconnect();
}

/// #1543 negative: an adapter that does not advertise step-in targets is
/// never asked, and the caller falls back to a plain step in.
#[test]
fn an_adapter_without_step_in_targets_is_not_asked() {
    if python3_missing() {
        return;
    }
    let (_tmp, log, mut session) = step_in_targets_session(false);
    assert_eq!(
        session.request_step_in_targets(),
        StepInTargetsAsk::Unsupported
    );
    session.step("next");
    poll_until(&mut session, &log, |qs| {
        qs.iter().any(|q| q["command"] == "next")
    });
    assert!(
        !requests(&log)
            .iter()
            .any(|q| q["command"] == "stepInTargets"),
        "{:?}",
        requests(&log)
    );
    session.disconnect();
}

/// #1543: asked before the paused frame's stack trace has arrived, a
/// supporting adapter reports `NoFrame` rather than `Unsupported`, so the
/// caller does not resume the program with a plain step in.
#[test]
fn step_in_targets_before_the_frame_arrives_is_not_unsupported() {
    if python3_missing() {
        return;
    }
    let (_tmp, log, mut session) = step_in_targets_session(true);
    session.stack_frames.clear();
    session.selected_frame = None;
    assert_eq!(session.request_step_in_targets(), StepInTargetsAsk::NoFrame);
    assert!(
        !requests(&log)
            .iter()
            .any(|q| q["command"] == "stepInTargets" || q["command"] == "stepIn"),
        "{:?}",
        requests(&log)
    );
    session.disconnect();
}

#[test]
fn step_in_targets_parse_and_skip_malformed_entries() {
    let msg = json!({"type": "response", "command": "stepInTargets", "body": {"targets": [
        {"id": 4, "label": "f(x)"},
        {"label": "no id"},
        {"id": 5},
        {"id": 6, "label": "g()"}
    ]}});
    assert_eq!(
        parse_step_in_targets(&msg),
        [(4, String::from("f(x)")), (6, String::from("g()"))]
    );
    assert!(parse_step_in_targets(&json!({"type": "response"})).is_empty());
    assert_eq!(
        step_in_target_request(1, 6)["arguments"],
        json!({"threadId": 1, "targetId": 6})
    );
}

/// Poll `session` to its exception stop, with the `exceptionInfo` request
/// sent, run `then`, and poll to the fake's marker. Returns every
/// `ExceptionInfo` the session raised after `then`, in order.
fn exception_info_after(
    session: &mut DapSession,
    log: &Path,
    then: impl FnOnce(&mut DapSession),
) -> Vec<String> {
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "an exception stop that asks what was thrown",
        || {
            session.poll();
            session.stopped_thread.is_some()
                && requests(log)
                    .iter()
                    .any(|q| q["command"] == "exceptionInfo")
        },
    );
    then(session);
    let mut summaries = Vec::new();
    let mut marked = false;
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the fake adapter's marker",
        || {
            for ev in session.poll() {
                match ev {
                    DapEvent::ExceptionInfo { summary } => summaries.push(summary),
                    DapEvent::Output { text, .. } if text.contains("MARKER") => marked = true,
                    _ => {}
                }
            }
            marked
        },
    );
    summaries
}

/// #864: an `exceptionInfo` reply that arrives after the program moved
/// on describes a stop that is over, and is dropped: after Continue,
/// after a Step, and after the session was ended. The adapter holds the
/// reply until that request, so the order is fixed.
#[test]
fn an_exception_reply_that_arrives_after_the_stop_ended_is_dropped() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let arms = r#"    elif c == "exceptionInfo":
        held.append(req)
    elif c in ("continue", "next", "disconnect"):
        reply(req)
        for h in held:
            answer(h, "KeyError", "'x'")
        marker()
"#;
    type Move = fn(&mut DapSession);
    let moves: [(&str, Move); 3] = [
        ("continue", DapSession::continue_execution),
        ("step", |s| s.step("next")),
        ("disconnect", DapSession::disconnect),
    ];
    let late: Vec<(&str, Vec<String>)> = moves
        .into_iter()
        .map(|(name, then)| {
            let (_tmp, log, mut session) = held_exception_info_session(arms);
            let late = exception_info_after(&mut session, &log, then);
            session.disconnect();
            (name, late)
        })
        .filter(|(_, late)| !late.is_empty())
        .collect();
    assert!(late.is_empty(), "reported after the stop ended: {late:?}");
}

/// #864 guard: a reply for an older stop that lands after a newer
/// exception stop's own does not overwrite it; the newer stop keeps its
/// summary.
#[test]
fn an_older_stops_exception_reply_does_not_overwrite_the_newer_ones() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let arms = r#"    elif c == "exceptionInfo":
        held.append(req)
        if len(held) == 2:
            answer(held[1], "ValueError", "new")
            answer(held[0], "KeyError", "'old'")
            marker()
    elif c == "continue":
        reply(req)
        stopped()
"#;
    let (_tmp, log, mut session) = held_exception_info_session(arms);
    let seen = exception_info_after(&mut session, &log, DapSession::continue_execution);
    session.disconnect();
    assert_eq!(seen, vec![String::from("ValueError: new")]);
}

#[test]
fn set_breakpoints_request_carries_log_messages() {
    // A logpoint is a breakpoint with a `logMessage`: the adapter prints
    // the interpolated text instead of pausing. Condition and logMessage
    // are independent fields and may coexist on one breakpoint.
    let req = set_breakpoints_request(
        Path::new("/a/b.py"),
        &[
            SourceBreakpoint {
                line: 4,
                condition: None,
                log_message: Some(String::from("x is {x}")),
                hit_condition: None,
            },
            SourceBreakpoint {
                line: 9,
                condition: Some(String::from("n > 0")),
                log_message: Some(String::from("n={n}")),
                hit_condition: None,
            },
            SourceBreakpoint::plain(11),
        ],
    );
    let bps = req["arguments"]["breakpoints"].as_array().unwrap();
    assert_eq!(bps[0]["logMessage"], "x is {x}");
    assert!(bps[0].get("condition").is_none());
    assert_eq!(bps[1]["condition"], "n > 0");
    assert_eq!(bps[1]["logMessage"], "n={n}");
    assert!(bps[2].get("logMessage").is_none());
}

#[test]
fn pytest_debug_launch_request_launches_the_pytest_module() {
    // Debug-a-test (Python): debugpy launches the `pytest` MODULE under
    // the PROJECT's interpreter (where pytest is importable) with the
    // test name as a `-k` filter; the adapter injects debugpy into the
    // debuggee itself, so the project venv needs no debugpy install.
    let req = pytest_debug_launch_request(
        Path::new("/proj/.venv/bin/python"),
        Path::new("/proj"),
        "test_addition",
    );
    assert_eq!(req["command"], "launch");
    assert_eq!(req["arguments"]["module"], "pytest");
    assert!(
        req["arguments"].get("program").is_none(),
        "a module launch must not also name a program"
    );
    let args = req["arguments"]["args"].as_array().unwrap();
    assert_eq!(args[0], "-v");
    assert_eq!(args[1], "-k");
    assert_eq!(args[2], "test_addition");
    assert_eq!(req["arguments"]["python"][0], "/proj/.venv/bin/python");
    assert_eq!(req["arguments"]["cwd"], "/proj");
}

/// #1519: every Python session croft started sent `justMyCode: false`,
/// so F11 on `json.dumps(...)` stepped into the standard library and the
/// call stack filled with `runpy` frames. VS Code and debugpy default to
/// stepping only through the user's own code; F5 and Debug Test now do.
#[test]
fn python_launches_step_through_the_users_code_only() {
    let f5 = launch_request(Path::new("/w/main.py"), Path::new("/v/bin/python"), false);
    assert_eq!(f5["arguments"]["justMyCode"], true, "F5 on the active file");
    let test = pytest_debug_launch_request(
        Path::new("/proj/.venv/bin/python"),
        Path::new("/proj"),
        "test_addition",
    );
    assert_eq!(test["arguments"]["justMyCode"], true, "Debug Test");
}

#[test]
fn lldb_test_launch_request_carries_the_filter_args() {
    // Debug-a-test (Rust): lldb-dap launches the cargo TEST BINARY with
    // libtest's substring filter as its argv, so only the picked test runs.
    let req = lldb_test_launch_request(
        Path::new("/proj/target/debug/deps/croft-abc123"),
        &[String::from("my_case"), String::from("--nocapture")],
    );
    assert_eq!(req["command"], "launch");
    assert_eq!(
        req["arguments"]["program"],
        "/proj/target/debug/deps/croft-abc123"
    );
    let args = req["arguments"]["args"].as_array().unwrap();
    assert_eq!(args[0], "my_case");
    assert_eq!(args[1], "--nocapture");
}

#[test]
fn launch_request_carries_program_and_interpreter() {
    let req = launch_request(Path::new("/w/app.py"), Path::new("/v/bin/python"), false);
    assert_eq!(req["command"], "launch");
    assert_eq!(req["arguments"]["program"], "/w/app.py");
    assert_eq!(req["arguments"]["python"][0], "/v/bin/python");
    assert_eq!(req["arguments"]["request"], "launch");
}

#[test]
fn thread_request_carries_thread_id() {
    let req = thread_request("continue", 3);
    assert_eq!(req["command"], "continue");
    assert_eq!(req["arguments"]["threadId"], 3);
}

#[test]
fn stack_trace_request_targets_full_stack() {
    let req = stack_trace_request(5);
    assert_eq!(req["command"], "stackTrace");
    assert_eq!(req["arguments"]["threadId"], 5);
    assert_eq!(req["arguments"]["startFrame"], 0);
    // No `levels` cap: the adapter returns every frame.
    assert!(req["arguments"].get("levels").is_none());
}

#[test]
fn parse_stack_frames_reads_all_frames_with_ids() {
    let resp = json!({
        "type": "response", "command": "stackTrace",
        "body": { "stackFrames": [
            { "id": 1000, "name": "isValid", "source": { "path": "/w/app.py" }, "line": 12 },
            { "id": 1001, "name": "<module>", "source": { "path": "/w/app.py" }, "line": 24 }
        ]}
    });
    let frames = parse_stack_frames(&resp);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].id, 1000);
    assert_eq!(frames[0].name, "isValid");
    assert_eq!(frames[0].path, Some(PathBuf::from("/w/app.py")));
    assert_eq!(frames[0].line, 12);
    assert_eq!(frames[1].id, 1001);
}

#[test]
fn parse_stack_frames_empty_when_malformed() {
    assert!(parse_stack_frames(&json!({"body": {}})).is_empty());
    assert!(parse_stack_frames(&json!({})).is_empty());
}

#[test]
fn parse_scopes_and_variables() {
    let scopes = parse_scopes(&json!({
        "body": { "scopes": [
            { "name": "Locals", "variablesReference": 7 },
            { "name": "Globals", "variablesReference": 8 }
        ]}
    }));
    assert_eq!(scopes.len(), 2);
    assert_eq!(scopes[0].name, "Locals");
    assert_eq!(scopes[0].variables_ref, 7);
    assert!(!scopes[0].expensive && !scopes[1].expensive);
    // #1421: js-debug marks its Global scope `expensive`.
    let scopes = parse_scopes(&json!({
        "body": { "scopes": [
            { "name": "Local: work", "variablesReference": 7, "expensive": false },
            { "name": "Global", "variablesReference": 8, "expensive": true }
        ]}
    }));
    assert_eq!(
        scopes.iter().map(|s| s.expensive).collect::<Vec<_>>(),
        vec![false, true]
    );

    let vars = parse_variables(&json!({
        "body": { "variables": [
            { "name": "x", "value": "1", "type": "int", "variablesReference": 0 },
            { "name": "obj", "value": "<Foo>", "type": "Foo", "variablesReference": 9 }
        ]}
    }));
    assert_eq!(vars.len(), 2);
    assert_eq!(vars[0].name, "x");
    assert_eq!(vars[0].value, "1");
    assert_eq!(vars[0].type_name, "int");
    assert_eq!(vars[0].variables_ref, 0);
    assert_eq!(
        vars[1].variables_ref, 9,
        "expandable variable keeps its ref"
    );
}

/// #1546: a variable keeps the adapter's `evaluateName`, the expression
/// Copy as Expression and Add to Watch use; one without it has none.
#[test]
fn parse_variables_keeps_the_evaluate_name() {
    let vars = parse_variables(&json!({
        "body": { "variables": [
            { "name": "[0]", "value": "1", "variablesReference": 0,
              "evaluateName": "obj.items[0]" },
            { "name": "special variables", "value": "", "variablesReference": 3 }
        ]}
    }));
    assert_eq!(vars[0].evaluate_name.as_deref(), Some("obj.items[0]"));
    assert_eq!(vars[1].evaluate_name, None, "no evaluateName, none made up");
}

/// End-to-end against a real debugpy adapter: launch a script with a
/// breakpoint and confirm the session reaches `Stopped` at that line.
/// Ignored by default — needs `~/.croft/debug-venv` (uv + debugpy on 3.14);
/// run with `cargo test --bin croft -- --ignored launches_debugpy`.
#[test]
#[ignore = "requires ~/.croft/debug-venv (uv + debugpy on CPython 3.14)"]
fn launches_debugpy_and_stops_at_breakpoint() {
    use std::io::Write;
    let py = crate::dap::install::ensure_debug_venv().expect("provision debug venv");
    let mut f = tempfile::Builder::new().suffix(".py").tempfile().unwrap();
    writeln!(f, "x = 1\ny = 2\nz = x + y\nprint(z)").unwrap();
    // Canonicalize so our breakpoint path matches debugpy's (macOS resolves
    // /var -> /private/var; an unresolved path would never bind).
    let path = f.path().canonicalize().unwrap();

    let mut bps = BTreeMap::new();
    bps.insert(path.clone(), vec![SourceBreakpoint::plain(3)]); // break on `z = x + y`
    let mut sess = DapSession::launch(
        &py.to_string_lossy(),
        &["-m".to_string(), "debugpy.adapter".to_string()],
        path.parent().unwrap(),
        &path,
        &py,
        bps,
        false,
    )
    .unwrap();

    let mut stopped = false;
    for _ in 0..100 {
        for ev in sess.poll() {
            if matches!(ev, DapEvent::Stopped { .. }) {
                stopped = true;
            }
        }
        if sess.current_location.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    sess.disconnect();

    assert!(stopped, "expected a `stopped` event from debugpy");
    let (_loc, line) = sess.current_location.clone().expect("a paused location");
    assert_eq!(line, 3, "should pause on the breakpoint line");
}

/// End-to-end against the real vscode-js-debug server: launch a Node script
/// with a breakpoint and confirm the multi-session handshake (parent ->
/// `startDebugging` -> child) reaches `Stopped` at that line. Ignored by
/// default — needs `node` + `~/.croft/js-debug`; run with
/// `cargo test --bin croft -- --ignored launches_js_debug`.
#[test]
#[ignore = "requires node + ~/.croft/js-debug (vscode-js-debug server)"]
fn launches_js_debug_and_stops_at_breakpoint() {
    use std::io::Write;
    let node = crate::dap::install::node_program().expect("node on PATH");
    let server = crate::dap::install::ensure_js_debug().expect("provision js-debug");
    let mut f = tempfile::Builder::new().suffix(".js").tempfile().unwrap();
    writeln!(
        f,
        "const a = 1;\nconst b = 2;\nconst c = a + b;\nconsole.log(c);"
    )
    .unwrap();
    let path = f.path().canonicalize().unwrap();

    let mut bps = BTreeMap::new();
    bps.insert(path.clone(), vec![SourceBreakpoint::plain(3)]); // break on `const c = a + b`
    let mut sess =
        DapSession::launch_js(&node, &server, path.parent().unwrap(), &path, bps, false).unwrap();

    let mut stopped = false;
    for _ in 0..200 {
        for ev in sess.poll() {
            if matches!(ev, DapEvent::Stopped { .. }) {
                stopped = true;
            }
        }
        if sess.current_location.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    sess.disconnect();

    assert!(
        stopped,
        "expected a `stopped` event from js-debug's child session"
    );
    let (_loc, line) = sess.current_location.clone().expect("a paused location");
    assert_eq!(line, 3, "should pause on the breakpoint line");
}

/// End-to-end: at a breakpoint, the inspection chain (stackTrace -> scopes ->
/// variables) populates the call stack and the locals. Break on line 3 of
/// `x=1; y=2; z=x+y; print(z)`, where `x` and `y` are already bound.
#[test]
#[ignore = "requires ~/.croft/debug-venv (uv + debugpy on CPython 3.14)"]
fn inspection_chain_populates_stack_and_locals() {
    use std::io::Write;
    let py = crate::dap::install::ensure_debug_venv().expect("provision debug venv");
    let mut f = tempfile::Builder::new().suffix(".py").tempfile().unwrap();
    writeln!(f, "x = 1\ny = 2\nz = x + y\nprint(z)").unwrap();
    let path = f.path().canonicalize().unwrap();
    let mut bps = BTreeMap::new();
    bps.insert(path.clone(), vec![SourceBreakpoint::plain(3)]);
    let mut sess = DapSession::launch(
        &py.to_string_lossy(),
        &["-m".to_string(), "debugpy.adapter".to_string()],
        path.parent().unwrap(),
        &path,
        &py,
        bps,
        false,
    )
    .unwrap();

    // Poll until the variables for some scope have arrived (the chain runs
    // across several polls: stackTrace -> scopes -> variables).
    let mut have_locals = false;
    for _ in 0..200 {
        sess.poll();
        let names: Vec<&str> = sess
            .variables
            .values()
            .flatten()
            .map(|v| v.name.as_str())
            .collect();
        if names.contains(&"x") && names.contains(&"y") {
            have_locals = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let frames = sess.stack_frames.clone();
    let xy: Vec<(String, String)> = sess
        .variables
        .values()
        .flatten()
        .filter(|v| v.name == "x" || v.name == "y")
        .map(|v| (v.name.clone(), v.value.clone()))
        .collect();
    sess.disconnect();

    assert!(
        !frames.is_empty(),
        "call stack must be populated at the stop"
    );
    assert!(have_locals, "locals x and y must arrive via the chain");
    assert!(xy.contains(&("x".to_string(), "1".to_string())));
    assert!(xy.contains(&("y".to_string(), "2".to_string())));
    // Hover-to-evaluate lookup resolves a loaded local by name.
    let x = sess.lookup_local("x").expect("lookup_local should find x");
    assert_eq!(x.value, "1");
    assert!(sess.lookup_local("nonexistent").is_none());
}

/// End-to-end through croft's shared session machinery with a SECOND adapter
/// (lldb-dap), proving `launch_with` is adapter-agnostic: compile a tiny C
/// program, launch it under lldb-dap, and confirm the session progresses to
/// termination. (Breakpoint binding needs a permitted macOS `debugserver`,
/// so this asserts the launch/run/teardown flow, not a stop.)
#[test]
#[ignore = "requires lldb-dap (Xcode Command Line Tools) + cc"]
fn lldb_dap_session_launches_via_shared_machinery() {
    use std::io::Write;
    let adapter = "/Library/Developer/CommandLineTools/usr/bin/lldb-dap";
    if !std::path::Path::new(adapter).is_file() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("m.c");
    let mut f = std::fs::File::create(&src).unwrap();
    writeln!(f, "#include <stdio.h>\nint main(){{\n  int x=41;\n  x++;\n  printf(\"%d\\n\", x);\n  return 0;\n}}").unwrap();
    let bin = dir.path().join("m.dbg");
    let ok = std::process::Command::new("cc")
        .args([
            "-g",
            "-O0",
            src.to_str().unwrap(),
            "-o",
            bin.to_str().unwrap(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "cc must compile the test program");

    let mut sess = DapSession::launch_with(
        adapter,
        &[],
        dir.path(),
        lldb_launch_request(&bin, false),
        BTreeMap::new(),
    )
    .unwrap();

    let mut terminated = false;
    for _ in 0..200 {
        for ev in sess.poll() {
            if matches!(ev, DapEvent::Terminated) {
                terminated = true;
            }
        }
        if terminated {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    sess.disconnect();
    assert!(
        terminated,
        "lldb-dap session must launch and run to completion via launch_with"
    );
}

/// Regression: a breakpoint set via a NON-canonical (symlinked) path must
/// still bind, and debugpy must echo back the SAME path we launched with
/// (not the canonical `realpath`). This is why croft does NOT canonicalize:
/// debugpy keys breakpoints and reports `stackTrace` source paths by the
/// path it was launched with, so `current_location` matches the editor's
/// open path and the gutter triangle renders. Canonicalizing the launch path
/// would make debugpy report the canonical path and break that match.
/// (macOS `/tmp` is itself a symlink to `/private/tmp`, exercising the
/// `/var`-style case too.)
#[test]
#[ignore = "requires ~/.croft/debug-venv (uv + debugpy on CPython 3.14)"]
fn binds_breakpoint_set_via_symlinked_path() {
    use std::io::Write;
    let py = crate::dap::install::ensure_debug_venv().expect("provision debug venv");
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let mut f = std::fs::File::create(real.join("m.py")).unwrap();
    writeln!(f, "a = 1\nb = 2\nc = a + b\nprint(c)").unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    // Open the file through the symlinked directory: NOT canonical.
    let via_link = link.join("m.py");
    assert_ne!(
        via_link,
        via_link.canonicalize().unwrap(),
        "test setup: path must be non-canonical"
    );

    let mut bps = BTreeMap::new();
    bps.insert(via_link.clone(), vec![SourceBreakpoint::plain(3)]);
    let mut sess = DapSession::launch(
        &py.to_string_lossy(),
        &["-m".to_string(), "debugpy.adapter".to_string()],
        &link,
        &via_link,
        &py,
        bps,
        false,
    )
    .unwrap();

    let mut stopped = false;
    for _ in 0..100 {
        for ev in sess.poll() {
            if matches!(ev, DapEvent::Stopped { .. }) {
                stopped = true;
            }
        }
        if sess.current_location.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    sess.disconnect();
    assert!(
        stopped,
        "breakpoint via symlinked path must still bind+stop"
    );
    let (loc, line) = sess.current_location.clone().expect("a paused location");
    assert_eq!(line, 3);
    // debugpy echoes the launched (symlinked) path, NOT the canonical one.
    assert_eq!(loc, via_link, "debugpy must report the path as launched");
}

/// A delve launch builds into croft's own temp folder, never the
/// package folder, whether it debugs the package or its tests (#1206).
#[test]
fn a_delve_launch_builds_outside_the_workspace() {
    let dir = Path::new("/tmp/croft-dlv-x");
    for request in [
        delve_zero_config_request(Path::new("/w/main.go")),
        delve_zero_config_request(Path::new("/w/main_test.go")),
        json!({"command": "launch", "arguments": {"program": "/w"}}),
    ] {
        assert!(delve_builds(&request), "{request}");
        let out = with_build_output(request, dir);
        let bin = out["arguments"]["output"].as_str().unwrap();
        assert!(bin.starts_with("/tmp/croft-dlv-x/__debug_bin"), "{bin}");
    }
}

/// Negative: a launch.json `output`, an `exec` of a built binary, a core
/// dump and an attach keep what they asked for; none gets croft's folder.
#[test]
fn an_explicit_output_or_a_launch_that_builds_nothing_is_left_alone() {
    for request in [
        json!({"command": "launch", "arguments": {"mode": "debug", "output": "./bin/app"}}),
        json!({"command": "launch", "arguments": {"mode": "exec", "program": "/w/app"}}),
        json!({"command": "launch", "arguments": {"mode": "core", "program": "/w/app"}}),
        json!({"command": "attach", "arguments": {"processId": 7}}),
    ] {
        assert!(!delve_builds(&request), "{request}");
    }
}

/// The build folder is private and goes away with the session, the
/// binary in it included; two sessions never share one.
#[test]
fn the_build_folder_is_removed_with_the_session() {
    let a = DebugBuildDir::create().unwrap();
    let b = DebugBuildDir::create().unwrap();
    assert_ne!(a.path(), b.path());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(a.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    let bin = a.path().join("__debug_bin");
    std::fs::write(&bin, "x").unwrap();
    let path = a.path().to_path_buf();
    drop(a);
    assert!(!path.exists());
    assert!(b.path().is_dir());
}

/// End to end against a real delve: stopping a session at a breakpoint,
/// or letting the program run to its end, leaves no `__debug_bin` in the
/// package folder (#1206). Needs Go and `dlv`; run it with `--ignored`.
#[test]
#[ignore]
fn delve_leaves_no_debug_binary_in_the_package() {
    let dlv = std::env::var_os("CROFT_TEST_DLV")
        .map(PathBuf::from)
        .or_else(|| crate::dap::install::dlv_program().ok())
        .expect("dlv");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("go.mod"), "module t\n\ngo 1.21\n").unwrap();
    let main = tmp.path().join("main.go");
    std::fs::write(
        &main,
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tx := 41\n\tx++\n\tfmt.Println(x)\n}\n",
    )
    .unwrap();
    let main = main.canonicalize().unwrap();
    let leftovers = || {
        std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("__debug_bin"))
            .collect::<Vec<_>>()
    };
    for stop_at_breakpoint in [true, false] {
        let mut bps = BTreeMap::new();
        if stop_at_breakpoint {
            bps.insert(main.clone(), vec![SourceBreakpoint::plain(7)]);
        }
        let mut s = DapSession::launch_delve(
            &dlv,
            main.parent().unwrap(),
            delve_zero_config_request(&main),
            bps,
        )
        .expect("delve starts");
        let want = if stop_at_breakpoint {
            SessionPhase::Stopped
        } else {
            SessionPhase::Terminated
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while s.phase != want && std::time::Instant::now() < deadline {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(s.phase, want);
        s.disconnect();
        drop(s);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(leftovers().is_empty(), "left behind: {:?}", leftovers());
    }
}

/// Accept the one client a test's stand-in DAP server expects, within a
/// load-scaled budget (#1417). A client that never connects (a routing
/// regression sending the session somewhere else) then fails the test
/// with a message, where a blocking `accept` would hang the server
/// thread, and the test's `join` on it, forever.
fn accept_within(listener: &std::net::TcpListener) -> std::net::TcpStream {
    let budget = crate::test_budget::spawn_budget(std::time::Duration::from_secs(2));
    let started = std::time::Instant::now();
    listener.set_nonblocking(true).unwrap();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .unwrap();
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && started.elapsed() < budget => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => {
                panic!("no client connected to the stand-in DAP server in {budget:?}: {e}")
            }
        }
    }
}

/// The next `Content-Length`-framed DAP message on `reader`, or `None`
/// once the client hangs up or goes quiet past the read timeout.
fn read_dap_message(reader: &mut impl std::io::BufRead) -> Option<Value> {
    let mut len = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return None;
        }
        if let Some(v) = line.trim().strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
        if line.trim().is_empty() {
            break;
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// The first `n` DAP messages a client sends to a socket listening on a
/// free port (#1417): a stand-in for `debugpy --listen`.
pub(crate) fn listening_adapter(n: usize) -> (u16, std::thread::JoinHandle<Vec<Value>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(accept_within(&listener));
        std::iter::from_fn(|| read_dap_message(&mut reader))
            .take(n)
            .collect()
    });
    (port, handle)
}

/// #1417: a Remote Attach talks DAP to the listening port itself: the
/// `initialize` and the `attach` arrive there, from croft, with no
/// adapter process in between.
#[test]
fn attach_tcp_talks_to_the_listening_adapter_directly() {
    let (port, server) = listening_adapter(2);
    let attach = serde_json::json!({
        "type": "request", "command": "attach",
        "arguments": { "request": "attach", "connect": { "host": "127.0.0.1", "port": port } }
    });
    let session =
        DapSession::attach_tcp("127.0.0.1", port, attach, BTreeMap::new()).expect("connects");
    let got = server.join().unwrap();
    let commands: Vec<_> = got.iter().map(|m| m["command"].as_str().unwrap()).collect();
    assert_eq!(commands, ["initialize", "attach"]);
    assert_eq!(got[1]["arguments"]["connect"]["port"], port);
    assert!(session.attached);
    drop(session);
}

/// #1417: a session with no child path (anything but js-debug, here a
/// direct debugpy attach) offers no `startDebugging`, and declines one
/// an adapter sends anyway. debugpy asks for one per subprocess of the
/// program when offered; acknowledging it promised a subprocess session,
/// with its breakpoints, that never opened.
#[test]
fn a_session_without_a_child_path_declines_start_debugging() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let stream = accept_within(&listener);
        let mut writer = stream.try_clone().unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let init = read_dap_message(&mut reader);
        let _attach = read_dap_message(&mut reader);
        let ask = json!({"type": "request", "seq": 5, "command": "startDebugging",
            "arguments": {"request": "attach",
                "configuration": {"name": "Subprocess 4242", "subProcessId": 4242}}});
        writer
            .write_all(&crate::dap::transport::encode(&ask))
            .unwrap();
        (init, read_dap_message(&mut reader))
    });
    let attach = json!({"type": "request", "command": "attach",
        "arguments": {"connect": {"host": "127.0.0.1", "port": port}}});
    let mut session =
        DapSession::attach_tcp("127.0.0.1", port, attach, BTreeMap::new()).expect("connects");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "croft's reply to the startDebugging request",
        || {
            session.poll();
            server.is_finished()
        },
    );
    let (init, reply) = server.join().unwrap();
    let init = init.expect("croft's initialize");
    assert_eq!(init["arguments"]["supportsStartDebuggingRequest"], false);
    let reply = reply.expect("croft answers the reverse request");
    assert_eq!(reply["type"], "response");
    assert_eq!(reply["command"], "startDebugging");
    assert_eq!(reply["request_seq"], 5);
    assert_eq!(reply["success"], false);
    assert!(session.child.is_none());
}

/// The first `n` DAP messages a client sends to a socket listening on a
/// free port (#1418): a stand-in for `dlv --headless --listen`.
pub(crate) fn headless_delve(n: usize) -> (u16, std::thread::JoinHandle<Vec<Value>>) {
    listening_adapter(n)
}

/// #1418: a remote Go attach connects to the headless delve's own port
/// and sends it `initialize` and the `mode: "remote"` attach; no dlv is
/// started, and the session is an attach (stopping sends
/// `terminateDebuggee: false`).
#[test]
fn attach_delve_server_talks_to_the_headless_delve_directly() {
    let (port, server) = headless_delve(2);
    let attach = json!({"type": "request", "command": "attach",
        "arguments": {"mode": "remote"}});
    let session = DapSession::attach_delve_server("127.0.0.1", port, attach, BTreeMap::new())
        .expect("connects");
    let got = server.join().unwrap();
    assert_eq!(got[0]["command"], "initialize");
    assert_eq!(got[1]["command"], "attach");
    assert_eq!(got[1]["arguments"]["mode"], "remote");
    assert!(session.attached);
}
