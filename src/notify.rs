//! Notification sinks (#358): forward what croft already notices — a long
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
    /// A Test Explorer run ended with failures (once per run).
    TestsFailed { failed: usize, passed: usize },
    /// A terminal sent an OSC 9 notification.
    Osc9 { pane: String, message: String },
    /// An agent in a pane is waiting for input. Declared so a sink's
    /// `events` filter can name it today; fired once #344 lands.
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
            Event::TestsFailed { failed, passed } => {
                format!("Tests: {failed} failed, {passed} passed")
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

/// Percent-encode for the deep link's query: everything but unreserved.
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

/// Send `n` through `sink` once. Errors name the sink and the failure, never
/// a header value.
pub fn deliver(sink: &NotificationSink, n: &Notification) -> Result<(), String> {
    match sink.kind.as_str() {
        "ntfy" | "webhook" => {
            let req = if sink.kind == "ntfy" {
                ntfy_request(sink, n)
            } else {
                webhook_request(sink, n)
            }
            .ok_or_else(|| {
                format!(
                    "{} sink is missing its {}",
                    sink.kind,
                    if sink.kind == "ntfy" { "topic" } else { "url" }
                )
            })?;
            post(&req)
        }
        "termux" | "command" => {
            let argv = command_argv(sink, n)
                .ok_or_else(|| String::from("command sink has an empty argv"))?;
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..])
                .env("CROFT_TITLE", &n.title)
                .env("CROFT_BODY", &n.body)
                .env("CROFT_LINK", &n.link)
                .env("CROFT_EVENT", n.event)
                .stdin(std::process::Stdio::null());
            match cmd.output() {
                Ok(out) if out.status.success() => Ok(()),
                Ok(out) => Err(format!(
                    "{} exited {}: {}",
                    argv[0],
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                )),
                Err(e) => Err(format!("{} not runnable: {e}", argv[0])),
            }
        }
        other => Err(format!("unknown sink kind {other:?}")),
    }
}

/// POST with ureq 3: phased timeouts, redirects refused so a header
/// carrying a secret is never replayed to another host, and a non-2xx
/// reported with its status rather than its body (which could echo the
/// request).
fn post(req: &Request) -> Result<(), String> {
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
        .map_err(|e| format!("{}: {e}", host_of(&req.url)))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("{}: HTTP {status}", host_of(&req.url)))
    }
}

/// The host part of a URL, for a log line that must not echo a path or
/// query that could carry a token.
fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest).to_string()
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

    /// Deliver `event` to every sink that wants it, on a background thread:
    /// one attempt, one retry, then a line in the Notifications channel.
    /// Nothing here is awaited by the caller.
    pub fn emit(&self, event: Event, workspace: &Path, host: &str) {
        let sinks = self.matching(&event);
        if sinks.is_empty() {
            return;
        }
        let n = Notification::new(&event, workspace, host);
        std::thread::spawn(move || deliver_all(&sinks, &n));
    }
}

/// Deliver to each sink with one retry, logging outcomes. Synchronous, so a
/// test can drive it without a thread.
pub fn deliver_all(sinks: &[NotificationSink], n: &Notification) {
    for sink in sinks {
        if deliver(sink, n).is_ok() {
            continue;
        }
        std::thread::sleep(RETRY_AFTER);
        let Err(err) = deliver(sink, n) else {
            continue;
        };
        output::push(
            CHANNEL_NOTIFICATIONS,
            OutputLevel::Warn,
            &format!("{} sink failed twice for {}: {err}", sink.kind, n.event),
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
                passed: 9
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
            },
            Path::new("/w"),
            "h",
        );
        assert_eq!(n.body, "Tests: 2 failed, 40 passed");
        assert_eq!(n.event, "tests_failed");
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
        assert!(err.contains("exited") && err.contains("nope"), "{err}");
        c.argv = vec!["/definitely/not/here".into()];
        assert!(deliver(&c, &n).unwrap_err().contains("not runnable"));
    }

    #[test]
    fn a_log_line_names_the_host_never_the_path() {
        assert_eq!(
            host_of("https://hooks.example.org/T0K3N/abc"),
            "hooks.example.org"
        );
        assert_eq!(host_of("http://10.0.0.5:8080/x?token=1"), "10.0.0.5:8080");
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
                passed: 0
            }),
            vec![ntfy]
        );
    }
}
