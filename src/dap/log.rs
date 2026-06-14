//! Optional DAP wire log.
//!
//! Mirrors [`crate::lsp::log_file`]: append-only `~/.croft/dap.log`, one line per
//! event with a unix-epoch timestamp. Writing is gated behind the `CROFT_DAP_LOG`
//! environment variable so the hot path pays nothing unless a debug session is
//! being diagnosed; when unset, [`log`] returns before touching the filesystem.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CROFT_DAP_LOG").is_some())
}

fn default_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".croft").join("dap.log");
    }
    std::env::temp_dir().join("croft-dap.log")
}

fn handle() -> Option<&'static Mutex<File>> {
    static LOG: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = default_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map(Mutex::new)
            .ok()
    })
    .as_ref()
}

/// Append one timestamped line to the wire log, if `CROFT_DAP_LOG` is set.
pub fn log(line: &str) {
    if !enabled() {
        return;
    }
    let Some(m) = handle() else {
        return;
    };
    let Ok(mut f) = m.lock() else {
        return;
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let _ = writeln!(f, "{ts:.3} {line}");
}
