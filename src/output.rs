//! In-process OUTPUT bus: the data behind the panel group's OUTPUT tab, a
//! read-only VS Code-style log viewer with a channel dropdown. Log call sites
//! across croft push levelled lines into named channels (one per language
//! server, plus Debug Adapter, Git, and Server Provisioning); the OUTPUT widget
//! renders the selected channel's tail. Mirrors VS Code's
//! `window.createOutputChannel` model, where each producer owns a channel, and
//! Zed's per-language-server log view with its RPC-messages toggle.
//!
//! This is a sibling of the always-on `lsp::log_file` disk log: producers
//! mirror to both, so the on-disk log keeps working while the UI gets a live,
//! per-channel, in-memory view with no file polling.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// VS Code `LogOutputChannel` severity. Drives the row colour and the optional
/// minimum-level filter; ordered low to high so `level >= min` is a cheap
/// compare.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum OutputLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl OutputLevel {
    /// Short tag rendered before the message, matching VS Code's `[info]` style.
    pub fn label(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Well-known channel names for the non-LSP producers. Language servers each
/// own a channel named after the server (`ty`, `rust-analyzer`, ...), so they
/// need no constant here.
pub const CHANNEL_DAP: &str = "Debug Adapter";
pub const CHANNEL_GIT: &str = "Git";
pub const CHANNEL_PROVISION: &str = "Server Provisioning";
pub const CHANNEL_TESTS: &str = "Test Runner";
/// Fleet run results, one line per host plus the summary (#363).
pub const CHANNEL_FLEET: &str = "Fleet";
/// Notification-sink delivery failures (#358).
pub const CHANNEL_NOTIFICATIONS: &str = "Notifications";

/// One line in a channel: when it arrived, how severe, and the text.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputLine {
    pub ts: f64,
    pub level: OutputLevel,
    pub text: String,
}

/// Per-channel ring-buffer cap. Same order of magnitude as the terminal
/// scrollback; the oldest line drops off the front so a chatty server can't
/// grow the buffer without bound.
const MAX_LINES: usize = 5000;

struct Registry {
    channels: BTreeMap<String, VecDeque<OutputLine>>,
}

fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(|| {
        Mutex::new(Registry {
            channels: BTreeMap::new(),
        })
    })
}

/// Bumped on every push/clear so a renderer can skip re-pulling an unchanged
/// channel set: one relaxed load per frame instead of cloning every line.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Whether raw JSON-RPC tracing is active (the OUTPUT panel's "RPC" toggle,
/// Zed's RPC-messages equivalent). Off by default: tracing tees every LSP
/// frame, so the hot path loads this first and skips the push when off.
static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

pub fn trace_enabled() -> bool {
    TRACE_ENABLED.load(Ordering::Relaxed)
}

pub fn set_trace_enabled(on: bool) {
    TRACE_ENABLED.store(on, Ordering::Relaxed);
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Append one line to `channel`, creating the channel on first use. Drops the
/// oldest line once the channel passes `MAX_LINES`.
pub fn push(channel: &str, level: OutputLevel, text: &str) {
    let Ok(mut reg) = registry().lock() else {
        return;
    };
    let buf = reg.channels.entry(channel.to_string()).or_default();
    buf.push_back(OutputLine {
        ts: now(),
        level,
        text: text.to_string(),
    });
    while buf.len() > MAX_LINES {
        buf.pop_front();
    }
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The channel names, sorted (BTreeMap order) for a stable dropdown.
pub fn channel_names() -> Vec<String> {
    registry()
        .lock()
        .map(|r| r.channels.keys().cloned().collect())
        .unwrap_or_default()
}

/// Clone a channel's lines for rendering. `None` if the channel doesn't exist.
pub fn snapshot(channel: &str) -> Option<Vec<OutputLine>> {
    let reg = registry().lock().ok()?;
    reg.channels
        .get(channel)
        .map(|b| b.iter().cloned().collect())
}

/// Clear one channel's lines, keeping the channel in the list (VS Code's clear
/// action). Bumps the generation so the view refreshes.
pub fn clear(channel: &str) {
    if let Ok(mut reg) = registry().lock()
        && let Some(buf) = reg.channels.get_mut(channel)
    {
        buf.clear();
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_creates_a_channel_and_appends_in_order() {
        let ch = "test-create-append";
        push(ch, OutputLevel::Info, "first");
        push(ch, OutputLevel::Error, "second");
        let lines = snapshot(ch).expect("channel exists after push");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "first");
        assert_eq!(lines[1].text, "second");
        assert_eq!(lines[1].level, OutputLevel::Error);
        assert!(channel_names().iter().any(|c| c == ch));
    }

    #[test]
    fn push_bumps_the_generation() {
        let before = generation();
        push("test-generation", OutputLevel::Info, "x");
        assert!(generation() > before, "a push must bump the generation");
    }

    #[test]
    fn clear_empties_a_channel_but_keeps_it_listed() {
        let ch = "test-clear";
        push(ch, OutputLevel::Info, "a");
        push(ch, OutputLevel::Info, "b");
        clear(ch);
        assert_eq!(snapshot(ch).map(|l| l.len()), Some(0));
        assert!(
            channel_names().iter().any(|c| c == ch),
            "clear keeps the channel in the dropdown",
        );
    }

    #[test]
    fn levels_order_low_to_high_for_filtering() {
        assert!(OutputLevel::Error > OutputLevel::Warn);
        assert!(OutputLevel::Warn > OutputLevel::Info);
        assert!(OutputLevel::Info > OutputLevel::Debug);
        assert!(OutputLevel::Debug > OutputLevel::Trace);
    }

    #[test]
    fn trace_toggle_round_trips() {
        set_trace_enabled(true);
        assert!(trace_enabled());
        set_trace_enabled(false);
        assert!(!trace_enabled());
    }
}
