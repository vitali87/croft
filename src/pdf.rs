use std::path::{Path, PathBuf};
use std::process::Command;

/// Rasteriser preference order:
/// 1. `pdftoppm` (poppler) — supports page selection, cross-platform when
///    poppler-utils is installed (`brew install poppler`, `apt install
///    poppler-utils`).
/// 2. `sips` — macOS built-in. Page 1 only; used as a fallback so the
///    feature works out of the box on every Mac without external deps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfBackend {
    PdftoppmCli,
    SipsCli,
}

pub fn detect_backend() -> Option<PdfBackend> {
    if which("pdftoppm").is_some() {
        return Some(PdfBackend::PdftoppmCli);
    }
    if cfg!(target_os = "macos") && which("sips").is_some() {
        return Some(PdfBackend::SipsCli);
    }
    None
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Total pages in `pdf`. Returns None when no detector is available so the
/// caller renders page 1 with no navigation rather than refusing the file.
pub fn detect_page_count(pdf: &Path) -> Option<u32> {
    if let Some(n) = page_count_via_pdfinfo(pdf) {
        return Some(n);
    }
    if cfg!(target_os = "macos")
        && let Some(n) = page_count_via_mdls(pdf)
    {
        return Some(n);
    }
    None
}

fn page_count_via_pdfinfo(pdf: &Path) -> Option<u32> {
    which("pdfinfo")?;
    let out = Command::new("pdfinfo").arg(pdf).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    parse_pdfinfo_pages(&s)
}

pub fn parse_pdfinfo_pages(out: &str) -> Option<u32> {
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("Pages:")
            && let Ok(n) = rest.trim().parse::<u32>()
        {
            return Some(n);
        }
    }
    None
}

fn page_count_via_mdls(pdf: &Path) -> Option<u32> {
    let out = Command::new("mdls")
        .args(["-raw", "-name", "kMDItemNumberOfPages"])
        .arg(pdf)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<u32>().ok()
}

/// Rasterise `page` (1-based) of `pdf` to PNG bytes at ~144 DPI. Returns
/// the encoded PNG. The caller is responsible for cleaning up no temp
/// files — every backend writes to a per-call temp path that we delete
/// before returning.
pub fn rasterize_page(pdf: &Path, page: u32, backend: PdfBackend) -> std::io::Result<Vec<u8>> {
    match backend {
        PdfBackend::PdftoppmCli => rasterize_with_pdftoppm(pdf, page),
        PdfBackend::SipsCli => {
            if page != 1 {
                return Err(std::io::Error::other("sips backend can only render page 1"));
            }
            rasterize_with_sips(pdf)
        }
    }
}

fn rasterize_with_pdftoppm(pdf: &Path, page: u32) -> std::io::Result<Vec<u8>> {
    let dir = unique_temp_dir("croft-pdf")?;
    let _guard = TempDirGuard(dir.clone());
    let prefix = dir.join("page");
    let status = Command::new("pdftoppm")
        .arg("-f")
        .arg(page.to_string())
        .arg("-l")
        .arg(page.to_string())
        .arg("-r")
        .arg("144")
        .args(["-png", "-singlefile"])
        .arg(pdf)
        .arg(&prefix)
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "pdftoppm exited with {status}"
        )));
    }
    let png_path = prefix.with_extension("png");
    std::fs::read(&png_path)
}

fn rasterize_with_sips(pdf: &Path) -> std::io::Result<Vec<u8>> {
    let dir = unique_temp_dir("croft-pdf")?;
    let _guard = TempDirGuard(dir.clone());
    let out = dir.join("page.png");
    let status = Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(pdf)
        .arg("--out")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!("sips exited with {status}")));
    }
    std::fs::read(&out)
}

fn unique_temp_dir(stem: &str) -> std::io::Result<PathBuf> {
    // pid + a process-wide counter, never a clock reading: two renders that
    // landed in the same nanosecond bucket used to pick the same directory,
    // and the second one's `remove_dir_all` below deleted the first one's
    // output from under `pdftoppm` - a page turn that silently did nothing.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("{stem}-{pid}-{seq}"));
    if path.exists() {
        std::fs::remove_dir_all(&path)?;
    }
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two renders in flight at once must never share a scratch directory:
    /// the loser's `remove_dir_all` used to delete the winner's PNG before it
    /// was read, and the page turn silently did nothing.
    #[test]
    fn concurrent_scratch_dirs_are_distinct() {
        let dirs: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..16)
                .map(|_| s.spawn(|| unique_temp_dir("croft-pdf-test").unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let unique: std::collections::HashSet<_> = dirs.iter().collect();
        assert_eq!(unique.len(), dirs.len(), "every render needs its own dir");
        for d in dirs {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn parses_page_count_from_pdfinfo_output() {
        let sample = "\
Title:          \nAuthor:         \nCreator:        \nProducer:       \n\
Pages:          12\nEncrypted:      no\nPage size:      612 x 792 pts\n";
        assert_eq!(parse_pdfinfo_pages(sample), Some(12));
    }

    #[test]
    fn pdfinfo_returns_none_when_pages_line_missing() {
        let sample = "Title:  Untitled\nAuthor:  Anon\n";
        assert!(parse_pdfinfo_pages(sample).is_none());
    }

    #[test]
    fn pdfinfo_handles_extra_whitespace() {
        let sample = "Pages:    7\n";
        assert_eq!(parse_pdfinfo_pages(sample), Some(7));
    }
}
