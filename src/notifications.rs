//! Notification sinks (#358), the `notifications` config key: forward what croft already notices — a long
//! command finishing in a pane you are not watching, a red test run, a
//! terminal's OSC 9 notice — to a phone or a chat, so a remote session is
//! something you can walk away from.
//!
//! Sinks come from the `notifications` block in `config.json` (user layers
//! only: a cloned repo must not be able to make croft run a command or post
//! to a URL, so the key is not in `WORKSPACE_ALLOWED_KEYS`). Delivery runs
//! on a background thread with one retry and never on the render path; a
//! failure is a line in the **Notifications** OUTPUT channel, never a
//! blocked frame. A webhook's headers may carry a secret, so they belong in
//! `config.local.json` and are never logged.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::output::{self, CHANNEL_NOTIFICATIONS, OutputLevel};
use crate::prefs::NotificationSink;

/// The default ntfy server when a sink names only a topic.
pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";
/// A command shorter than this does not notify unless the sink says so.
pub const DEFAULT_MIN_DURATION: Duration = Duration::from_secs(10);
/// Connect and whole-request budgets for the HTTP sinks.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// One retry after this pause, then give up and log.
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// Something worth telling the user about, with what the sinks need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A command finished in a pane that was not focused.
    CommandFinished {
        pane: String,
        cmd: String,
        exit: Option<i32>,
        dur: Duration,
        cwd: Option<PathBuf>,
        host: Option<String>,
    },
    /// A Test Explorer run ended red (once per run). `reported` is false
    /// when no case reported at all — a compile error or a runner that
    /// died — so the counts are meaningless and the body must say so
    /// rather than "0 failed".
    TestsFailed {
        failed: usize,
        passed: usize,
        reported: bool,
    },
    /// A terminal sent an OSC 9 notification.
    Osc9 { pane: String, message: String },
    /// An agent in a pane is waiting for input. Declared so a sink's
    /// `events` filter can name it today; fired once #344 lands.
    // TODO(#344): construct this from the agent-lane sampler and drop the allow.
    #[allow(dead_code)]
    AgentWaiting { pane: String },
}

impl Event {
    /// The name a sink's `events` filter matches.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::CommandFinished { .. } => "command_finished",
            Event::TestsFailed { .. } => "tests_failed",
            Event::Osc9 { .. } => "osc9",
            Event::AgentWaiting { .. } => "agent_waiting",
        }
    }
}

/// What every sink receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub event: &'static str,
    pub title: String,
    pub body: String,
    pub workspace: String,
    pub host: String,
    /// `croft://attach?host=…&path=…`, for a phone-side consumer.
    pub link: String,
}

impl Notification {
    pub fn new(event: &Event, workspace: &Path, host: &str) -> Self {
        let name = workspace
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("croft");
        let body = match event {
            Event::CommandFinished {
                pane,
                cmd,
                exit,
                dur,
                host: cmd_host,
                ..
            } => {
                let code = exit.map_or_else(|| String::from("?"), |c| c.to_string());
                let mut s = format!(
                    "Command in {pane} finished: exit {code} in {}",
                    crate::widgets::terminal::human_duration(*dur)
                );
                if !cmd.trim().is_empty() {
                    s.push_str(" \u{2014} ");
                    s.push_str(cmd.trim());
                }
                if let Some(h) = cmd_host {
                    s.push_str(&format!(" (on {h})"));
                }
                s
            }
            Event::TestsFailed {
                failed,
                passed,
                reported,
            } => {
                if *reported {
                    format!("Tests: {failed} failed, {passed} passed")
                } else {
                    String::from(
                        "Test run failed before reporting any result (compile error, or the runner died)",
                    )
                }
            }
            Event::Osc9 { pane, message } => format!("{pane}: {message}"),
            Event::AgentWaiting { pane } => format!("Agent in {pane} is waiting for input"),
        };
        let path = workspace.display().to_string();
        Self {
            event: event.kind(),
            title: format!("croft \u{b7} {name}"),
            body,
            workspace: path.clone(),
            host: host.to_string(),
            link: format!(
                "croft://attach?host={}&path={}",
                encode(host),
                encode(&path)
            ),
        }
    }
}

/// Percent-encode for the deep link's query: everything but unreserved,
/// with `/` kept so a path stays readable (it is not a general encoder).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Whether `sink` wants `event`: its filter names the kind (an empty filter
/// takes everything), and a finished command lasted at least the sink's
/// threshold (default [`DEFAULT_MIN_DURATION`]).
pub fn wants(sink: &NotificationSink, event: &Event) -> bool {
    if !sink.events.is_empty() && !sink.events.iter().any(|e| e == event.kind()) {
        return false;
    }
    match event {
        Event::CommandFinished { dur, .. } => {
            let min = sink
                .min_duration_secs
                .map_or(DEFAULT_MIN_DURATION, Duration::from_secs);
            *dur >= min
        }
        _ => true,
    }
}

/// A request one HTTP sink would send: pure, so it can be tested without a
/// socket. Headers are `(name, value)` in send order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// ntfy: POST the body to `<server>/<topic>`, title and click link in
/// headers (ntfy's own scheme), tagged by event.
pub fn ntfy_request(sink: &NotificationSink, n: &Notification) -> Option<Request> {
    let topic = sink.topic.as_deref()?.trim();
    if topic.is_empty() {
        return None;
    }
    let server = sink
        .server
        .as_deref()
        .map(|s| s.trim_end_matches('/'))
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_NTFY_SERVER);
    Some(Request {
        url: format!("{server}/{topic}"),
        headers: vec![
            (String::from("Title"), n.title.clone()),
            (String::from("Click"), n.link.clone()),
            (String::from("Tags"), n.event.replace('_', "-")),
        ],
        body: n.body.clone(),
    })
}

/// Webhook: POST a JSON object with the whole notification; the sink's
/// headers are sent as given (that is where a bearer token lives).
pub fn webhook_request(sink: &NotificationSink, n: &Notification) -> Option<Request> {
    let url = sink.url.as_deref()?.trim();
    if url.is_empty() {
        return None;
    }
    let body = serde_json::json!({
        "event": n.event,
        "title": n.title,
        "body": n.body,
        "workspace": n.workspace,
        "host": n.host,
        "link": n.link,
    });
    let mut headers = vec![(
        String::from("content-type"),
        String::from("application/json"),
    )];
    headers.extend(sink.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
    Some(Request {
        url: url.to_string(),
        headers,
        body: body.to_string(),
    })
}

/// The argv a `command` or `termux` sink runs. The notification travels in
/// the environment (`CROFT_TITLE`, `CROFT_BODY`, `CROFT_LINK`,
/// `CROFT_EVENT`) rather than in argv, so a body with quotes needs no
/// escaping and does not show in `ps`.
pub fn command_argv(sink: &NotificationSink, n: &Notification) -> Option<Vec<String>> {
    match sink.kind.as_str() {
        "termux" => Some(vec![
            String::from("termux-notification"),
            String::from("--title"),
            n.title.clone(),
            String::from("--content"),
            n.body.clone(),
        ]),
        "command" if !sink.argv.is_empty() => Some(sink.argv.clone()),
        _ => None,
    }
}

/// Why a delivery failed, and whether trying again could help. A missing
/// topic or a program that is not there will not fix itself in two
/// seconds; a socket that timed out might.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub transient: bool,
}

impl Failure {
    fn fixed(message: String) -> Self {
        Self {
            message,
            transient: false,
        }
    }
    fn transient(message: String) -> Self {
        Self {
            message,
            transient: true,
        }
    }
}

/// How long a `command`/`termux` sink's program may run before it is
/// killed: a sink is a notifier, not a job, and a hung one must not pin the
/// delivery worker.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Send `n` through `sink` once. Errors name the sink and the failure, never
/// a header value.
pub fn deliver(sink: &NotificationSink, n: &Notification) -> Result<(), Failure> {
    match sink.kind.as_str() {
        "ntfy" | "webhook" => {
            let req = if sink.kind == "ntfy" {
                ntfy_request(sink, n)
            } else {
                webhook_request(sink, n)
            }
            .ok_or_else(|| {
                Failure::fixed(format!(
                    "{} sink is missing its {}",
                    sink.kind,
                    if sink.kind == "ntfy" { "topic" } else { "url" }
                ))
            })?;
            post(&req)
        }
        "termux" | "command" => {
            let argv = command_argv(sink, n)
                .ok_or_else(|| Failure::fixed(String::from("command sink has an empty argv")))?;
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..])
                .env("CROFT_TITLE", &n.title)
                .env("CROFT_BODY", &n.body)
                .env("CROFT_LINK", &n.link)
                .env("CROFT_EVENT", n.event)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped());
            run_bounded(cmd, &argv[0], COMMAND_TIMEOUT)
        }
        other => Err(Failure::fixed(format!("unknown sink kind {other:?}"))),
    }
}

/// Run `cmd` to completion or `timeout`, whichever is first; a child that
/// outlives the bound is killed and reported as a (transient) failure.
fn run_bounded(
    mut cmd: std::process::Command,
    name: &str,
    timeout: Duration,
) -> Result<(), Failure> {
    let mut child = cmd
        .spawn()
        .map_err(|e| Failure::fixed(format!("{name} not runnable: {e}")))?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
                }
                return Err(Failure::transient(format!(
                    "{name} exited {status}: {}",
                    stderr.trim()
                )));
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::transient(format!(
                    "{name} did not finish within {timeout:?} and was killed"
                )));
            }
            Err(e) => return Err(Failure::fixed(format!("{name}: {e}"))),
        }
    }
}

/// POST with ureq 3: phased timeouts, redirects refused so a header
/// carrying a secret is never replayed to another host, and a non-2xx
/// reported with its status rather than its body (which could echo the
/// request).
fn post(req: &Request) -> Result<(), Failure> {
    let agent = ureq3::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut post = agent
        .post(&req.url)
        .config()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build();
    for (k, v) in &req.headers {
        post = post.header(k.as_str(), v.as_str());
    }
    let resp = post
        .send(req.body.as_str())
        .map_err(|e| Failure::transient(format!("{}: {e}", host_of(&req.url))))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        Ok(())
    } else if (500..600).contains(&status) || status == 429 {
        Err(Failure::transient(format!(
            "{}: HTTP {status}",
            host_of(&req.url)
        )))
    } else {
        Err(Failure::fixed(format!(
            "{}: HTTP {status}",
            host_of(&req.url)
        )))
    }
}

/// The host (and port) of a URL, for a log line that must not echo a
/// path, query, fragment, or userinfo that could carry a token.
fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .to_string()
}

/// One delivery worker for the whole process, fed by a bounded queue: a
/// burst of events (a terminal printing OSC 9 in a loop) costs at most
/// [`QUEUE_DEPTH`] pending jobs and one thread, never a thread per event.
/// Overflow drops the newest job and says so in the channel.
const QUEUE_DEPTH: usize = 64;

type Job = (Vec<NotificationSink>, Notification);

fn worker() -> &'static std::sync::mpsc::SyncSender<Job> {
    static TX: std::sync::OnceLock<std::sync::mpsc::SyncSender<Job>> = std::sync::OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(QUEUE_DEPTH);
        std::thread::spawn(move || {
            while let Ok((sinks, n)) = rx.recv() {
                deliver_all(&sinks, &n);
            }
        });
        tx
    })
}

/// The live sink list, rebuilt whenever config is (re)loaded.
#[derive(Debug, Clone, Default)]
pub struct Notifier {
    sinks: Vec<NotificationSink>,
}

impl Notifier {
    pub fn new(sinks: &[NotificationSink]) -> Self {
        let mut keep = Vec::new();
        for s in sinks {
            match s.kind.as_str() {
                "ntfy" | "webhook" | "termux" | "command" => keep.push(s.clone()),
                other => output::push(
                    CHANNEL_NOTIFICATIONS,
                    OutputLevel::Warn,
                    &format!("notifications: unknown sink kind {other:?} ignored"),
                ),
            }
        }
        Self { sinks: keep }
    }

    /// The sinks that take `event`.
    pub fn matching(&self, event: &Event) -> Vec<NotificationSink> {
        self.sinks
            .iter()
            .filter(|s| wants(s, event))
            .cloned()
            .collect()
    }

    /// Queue `event` for every sink that wants it. Nothing here blocks:
    /// the queue is bounded and a full one drops the job with a channel
    /// line rather than waiting.
    pub fn emit(&self, event: Event, workspace: &Path, host: &str) {
        let sinks = self.matching(&event);
        if sinks.is_empty() {
            return;
        }
        let n = Notification::new(&event, workspace, host);
        if let Err(std::sync::mpsc::TrySendError::Full(_)) = worker().try_send((sinks, n)) {
            output::push(
                CHANNEL_NOTIFICATIONS,
                OutputLevel::Warn,
                &format!(
                    "notifications: {} pending deliveries, dropped a {} event",
                    QUEUE_DEPTH,
                    event.kind()
                ),
            );
        }
    }
}

/// Deliver to each sink, retrying a transient failure once after a pause,
/// and logging what finally failed. Synchronous, so a test can drive it.
pub fn deliver_all(sinks: &[NotificationSink], n: &Notification) {
    for sink in sinks {
        let first = match deliver(sink, n) {
            Ok(()) => continue,
            Err(f) => f,
        };
        let last = if first.transient {
            std::thread::sleep(RETRY_AFTER);
            match deliver(sink, n) {
                Ok(()) => continue,
                Err(f) => f,
            }
        } else {
            first
        };
        output::push(
            CHANNEL_NOTIFICATIONS,
            OutputLevel::Warn,
            &format!(
                "{} sink failed for {}{}: {}",
                sink.kind,
                n.event,
                if last.transient { " (twice)" } else { "" },
                last.message
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sink(kind: &str) -> NotificationSink {
        NotificationSink {
            kind: kind.to_string(),
            ..Default::default()
        }
    }

    fn finished(secs: u64) -> Event {
        Event::CommandFinished {
            pane: "Terminal 2".into(),
            cmd: "cargo build".into(),
            exit: Some(0),
            dur: Duration::from_secs(secs),
            cwd: None,
            host: None,
        }
    }

    #[test]
    fn a_sink_filters_by_event_kind_and_a_command_by_duration() {
        let mut s = sink("ntfy");
        assert!(wants(&s, &finished(30)), "an empty filter takes everything");
        assert!(!wants(&s, &finished(3)), "under the default 10s threshold");
        s.min_duration_secs = Some(2);
        assert!(wants(&s, &finished(3)));
        s.events = vec![String::from("tests_failed")];
        assert!(!wants(&s, &finished(30)));
        assert!(wants(
            &s,
            &Event::TestsFailed {
                failed: 1,
                passed: 9,
                reported: true
            }
        ));
        s.events = vec![String::from("command_finished"), String::from("osc9")];
        assert!(wants(
            &s,
            &Event::Osc9 {
                pane: "T1".into(),
                message: "done".into()
            }
        ));
        assert!(!wants(&s, &Event::AgentWaiting { pane: "T1".into() }));
    }

    #[test]
    fn the_payload_names_the_workspace_host_and_a_deep_link() {
        let n = Notification::new(&finished(75), Path::new("/home/t/my proj"), "box.local");
        assert_eq!(n.title, "croft \u{b7} my proj");
        assert!(
            n.body
                .starts_with("Command in Terminal 2 finished: exit 0 in "),
            "{}",
            n.body
        );
        assert!(n.body.ends_with("\u{2014} cargo build"), "{}", n.body);
        assert_eq!(n.host, "box.local");
        assert_eq!(
            n.link,
            "croft://attach?host=box.local&path=/home/t/my%20proj"
        );
        let n = Notification::new(
            &Event::TestsFailed {
                failed: 2,
                passed: 40,
                reported: true,
            },
            Path::new("/w"),
            "h",
        );
        assert_eq!(n.body, "Tests: 2 failed, 40 passed");
        assert_eq!(n.event, "tests_failed");
        // A compile error reports nothing; "0 failed, 0 passed" would be a
        // lie, so the body says what happened instead.
        let n = Notification::new(
            &Event::TestsFailed {
                failed: 0,
                passed: 0,
                reported: false,
            },
            Path::new("/w"),
            "h",
        );
        assert!(
            n.body.starts_with("Test run failed before reporting"),
            "{}",
            n.body
        );
    }

    #[test]
    fn ntfy_posts_to_the_topic_with_title_and_click_headers() {
        let n = Notification::new(&finished(30), Path::new("/w"), "h");
        let mut s = sink("ntfy");
        assert_eq!(ntfy_request(&s, &n), None, "no topic, no request");
        s.topic = Some("croft-alerts".into());
        let r = ntfy_request(&s, &n).unwrap();
        assert_eq!(r.url, "https://ntfy.sh/croft-alerts");
        assert_eq!(r.body, n.body);
        assert!(
            r.headers
                .contains(&(String::from("Title"), n.title.clone()))
        );
        assert!(r.headers.contains(&(String::from("Click"), n.link.clone())));
        assert!(
            r.headers
                .contains(&(String::from("Tags"), String::from("command-finished")))
        );
        s.server = Some("https://ntfy.example.org/".into());
        assert_eq!(
            ntfy_request(&s, &n).unwrap().url,
            "https://ntfy.example.org/croft-alerts"
        );
    }

    #[test]
    fn a_webhook_posts_json_with_the_configured_headers() {
        let n = Notification::new(&finished(30), Path::new("/w"), "h");
        let mut s = sink("webhook");
        assert_eq!(webhook_request(&s, &n), None);
        s.url = Some("https://hooks.example.org/x".into());
        s.headers =
            BTreeMap::from([(String::from("authorization"), String::from("Bearer s3cret"))]);
        let r = webhook_request(&s, &n).unwrap();
        assert_eq!(r.url, "https://hooks.example.org/x");
        assert!(
            r.headers
                .contains(&(String::from("authorization"), String::from("Bearer s3cret")))
        );
        assert_eq!(
            r.headers[0],
            (
                String::from("content-type"),
                String::from("application/json")
            )
        );
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["event"], "command_finished");
        assert_eq!(v["link"], n.link);
        assert_eq!(v["body"], n.body);
    }

    #[test]
    fn termux_and_command_sinks_build_their_argv() {
        let n = Notification::new(&finished(30), Path::new("/w"), "h");
        let t = command_argv(&sink("termux"), &n).unwrap();
        assert_eq!(t[0], "termux-notification");
        assert_eq!(t[1..3], [String::from("--title"), n.title.clone()]);
        assert_eq!(t[3..5], [String::from("--content"), n.body.clone()]);
        assert_eq!(
            command_argv(&sink("command"), &n),
            None,
            "an empty argv is no sink"
        );
        let mut c = sink("command");
        c.argv = vec!["/bin/sh".into(), "-c".into(), "true".into()];
        assert_eq!(command_argv(&c, &n).unwrap(), c.argv);
    }

    /// The command sink is the network-free path: it proves delivery runs
    /// the program with the notification in its environment, and that a
    /// failing program is an error (which `deliver_all` retries and logs)
    /// rather than a panic.
    #[test]
    fn a_command_sink_runs_with_the_notification_in_its_environment() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("got.txt");
        let n = Notification::new(&finished(30), dir.path(), "box");
        let mut c = sink("command");
        c.argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "printf '%s|%s|%s' \"$CROFT_EVENT\" \"$CROFT_TITLE\" \"$CROFT_LINK\" > '{}'",
                out.display()
            ),
        ];
        deliver(&c, &n).expect("the program ran");
        let got = std::fs::read_to_string(&out).unwrap();
        let parts: Vec<&str> = got.split('|').collect();
        assert_eq!(parts[0], "command_finished");
        assert_eq!(parts[1], n.title);
        assert_eq!(parts[2], n.link);

        c.argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "echo nope >&2; exit 3".into(),
        ];
        let err = deliver(&c, &n).unwrap_err();
        assert!(
            err.message.contains("exited") && err.message.contains("nope"),
            "{err:?}"
        );
        assert!(err.transient, "a failing program may pass next time");
        c.argv = vec!["/definitely/not/here".into()];
        let err = deliver(&c, &n).unwrap_err();
        assert!(err.message.contains("not runnable"));
        assert!(
            !err.transient,
            "a missing program will not appear in two seconds"
        );
    }

    /// A sink's program is a notifier, not a job: one that hangs is killed
    /// at the bound rather than pinning the delivery worker.
    #[test]
    fn a_hung_command_sink_is_killed_at_the_bound() {
        let started = std::time::Instant::now();
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30"]);
        let err = run_bounded(cmd, "/bin/sh", Duration::from_millis(300)).unwrap_err();
        assert!(err.message.contains("was killed"), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_log_line_names_the_host_never_the_path_query_or_userinfo() {
        assert_eq!(
            host_of("https://hooks.example.org/T0K3N/abc"),
            "hooks.example.org"
        );
        assert_eq!(host_of("http://10.0.0.5:8080/x?token=1"), "10.0.0.5:8080");
        assert_eq!(host_of("https://user:pass@host.io/x"), "host.io");
        assert_eq!(host_of("https://h.io?token=zz"), "h.io");
        assert_eq!(host_of("https://h.io#frag"), "h.io");
        assert_eq!(host_of("https://u:p@h.io"), "h.io");
    }

    /// The queue delivers: an event with a command sink lands in the file
    /// the program writes, from the worker thread, without the caller
    /// waiting on anything.
    #[test]
    fn emit_delivers_through_the_worker_queue() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("hit.txt");
        let mut c = sink("command");
        c.min_duration_secs = Some(0);
        c.argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("printf '%s' \"$CROFT_EVENT\" > '{}'", out.display()),
        ];
        let notifier = Notifier::new(&[c]);
        notifier.emit(finished(1), dir.path(), "h");
        let mut waited = 0;
        while !out.exists() && waited < 8000 {
            std::thread::sleep(Duration::from_millis(40));
            waited += 40;
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "command_finished");
    }

    #[test]
    fn an_unknown_sink_kind_is_dropped_and_only_matching_sinks_are_returned() {
        let mut ntfy = sink("ntfy");
        ntfy.topic = Some("t".into());
        ntfy.events = vec![String::from("tests_failed")];
        let notifier = Notifier::new(&[ntfy.clone(), sink("pigeon")]);
        assert_eq!(
            notifier.matching(&finished(30)),
            Vec::<NotificationSink>::new()
        );
        assert_eq!(
            notifier.matching(&Event::TestsFailed {
                failed: 1,
                passed: 0,
                reported: true
            }),
            vec![ntfy]
        );
    }
}
