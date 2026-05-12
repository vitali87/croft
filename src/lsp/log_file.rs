use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn default_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".croft").join("lsp.log");
    }
    std::env::temp_dir().join("croft-lsp.log")
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

pub fn log(line: &str) {
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

pub fn path() -> PathBuf {
    default_path()
}
