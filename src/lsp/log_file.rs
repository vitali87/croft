use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Size at which the log starts over, keeping the previous one as
/// `lsp.log.1`. Every diagnostics publish is logged, so a long session
/// grew the file without limit.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

fn default_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".croft").join("lsp.log");
    }
    std::env::temp_dir().join("croft-lsp.log")
}

/// An append-only log that rotates to `<path>.1` past `max` bytes.
struct RotatingLog {
    path: PathBuf,
    max: u64,
    file: File,
    len: u64,
}

impl RotatingLog {
    fn open(path: &Path, max: u64) -> Option<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        let len = file.metadata().map_or(0, |m| m.len());
        let mut log = RotatingLog {
            path: path.to_path_buf(),
            max,
            file,
            len,
        };
        // A log already past the cap from an earlier session starts over too.
        if log.len >= max {
            log.rotate();
        }
        Some(log)
    }

    fn rotate(&mut self) {
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        let _ = std::fs::rename(&self.path, &old);
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            self.file = file;
            self.len = 0;
        }
    }

    fn write_line(&mut self, line: &str) {
        if self.len >= self.max {
            self.rotate();
        }
        if writeln!(self.file, "{line}").is_ok() {
            self.len += line.len() as u64 + 1;
        }
    }
}

fn handle() -> Option<&'static Mutex<RotatingLog>> {
    static LOG: OnceLock<Option<Mutex<RotatingLog>>> = OnceLock::new();
    LOG.get_or_init(|| RotatingLog::open(&default_path(), MAX_LOG_BYTES).map(Mutex::new))
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
    f.write_line(&format!("{ts:.3} {line}"));
}

pub fn path() -> PathBuf {
    default_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_rotates_past_its_cap_and_keeps_one_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lsp.log");
        let mut log = RotatingLog::open(&path, 64).unwrap();
        for i in 0..20 {
            log.write_line(&format!("line {i:02} xxxxxxxx"));
        }
        let current = std::fs::metadata(&path).unwrap().len();
        let previous = std::fs::metadata(dir.path().join("lsp.log.1"))
            .unwrap()
            .len();
        assert!(current <= 64 + 20, "current {current}");
        assert!(previous <= 64 + 20, "previous {previous}");
        assert!(std::fs::read_to_string(&path).unwrap().contains("line 19"));
        // Only one previous file is kept.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_log_left_over_the_cap_starts_over_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lsp.log");
        std::fs::write(&path, vec![b'x'; 200]).unwrap();
        let mut log = RotatingLog::open(&path, 64).unwrap();
        log.write_line("fresh");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
        assert_eq!(
            std::fs::metadata(dir.path().join("lsp.log.1"))
                .unwrap()
                .len(),
            200
        );
    }
}
