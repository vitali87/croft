//! `croft view` (#362): a pane asks the croft that hosts it to open a file.
//!
//! Over SSH `imgcat file.png` works because the escape sequences travel
//! through the terminal, but croft's richer previews (PDF pages, sheets,
//! SQLite) are editor tabs, and a shell prompt had no way to ask for one.
//! This module is the channel: croft listens on a per-process Unix socket,
//! exports its path into every pane it spawns, and `croft view <path>`
//! connects to it.
//!
//! # Direction, and why it is not the drop relay
//!
//! The drop relay ([`crate::app`]'s `relay_dir`) runs remote -> local: a
//! remote croft asks the user's local pump to fetch something. This runs the
//! other way and never crosses a machine: the pane's shell is a CHILD of the
//! croft it is talking to, so both ends are always the same host and the same
//! uid, whether that host is the laptop or the box behind `croft remote`.
//! That is what makes a plain `0600` socket sufficient here.
//!
//! # Why the client resolves the path
//!
//! A relative `croft view report.pdf` means "relative to the shell's cwd",
//! and the server cannot know that: the pane's cwd is the client's own, it
//! changes with every `cd`, and croft's notion of the pane cwd is a scraped
//! approximation refreshed on a timer. The client holds the truth, so it
//! resolves before sending and the server only ever sees absolute paths.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Env var naming the host croft's socket, exported into every pane.
pub const SOCK_ENV: &str = "CROFT_VIEW_SOCK";

/// One request, one JSON line.
///
/// The path travels as raw BYTES rather than a `String` because a filename
/// on Linux is bytes, not UTF-8: `to_string_lossy` would replace an invalid
/// sequence with U+FFFD and the server would open the wrong path (or none).
/// A JSON array of integers is unlovely on the wire and exactly lossless,
/// which is the trade worth making for something the user never reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRequest {
    pub path: Vec<u8>,
}

/// The server's answer. `Err` carries a message the client prints verbatim,
/// so the reason a view failed is decided by the side that knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ViewReply {
    Ok,
    Err { message: String },
}

impl ViewRequest {
    pub fn new(path: &Path) -> Self {
        use std::os::unix::ffi::OsStrExt;
        Self {
            path: path.as_os_str().as_bytes().to_vec(),
        }
    }

    pub fn to_path(&self) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(&self.path))
    }
}

/// Where the croft with this pid listens.
///
/// Keyed by pid, not by workspace: two crofts can share a workspace, and a
/// pane must reach the one that spawned it rather than whichever bound
/// first. The name is kept short because an `AF_UNIX` path is capped near
/// 104 bytes on macOS and the cache dir already eats most of that.
pub fn socket_path(cache_dir: &Path, pid: u32) -> PathBuf {
    cache_dir.join(format!("view-{pid}.sock"))
}

/// Resolve the user's argument against the client's cwd.
///
/// `~` is deliberately NOT expanded: every shell croft spawns expands it
/// before `croft view` is ever exec'd, so a literal `~` reaching here means
/// the user quoted it and meant a file of that name.
pub fn resolve(cwd: &Path, arg: &Path) -> PathBuf {
    if arg.is_absolute() {
        arg.to_path_buf()
    } else {
        cwd.join(arg)
    }
}

/// Send one request and wait for the reply.
///
/// Both timeouts are set because a croft wedged mid-frame would otherwise
/// hang the user's shell forever, and a shell that never returns is a worse
/// failure than a view that does not open.
pub fn send(socket: &Path, req: &ViewRequest) -> std::io::Result<ViewReply> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut line = serde_json::to_string(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    if reply.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "croft accepted the request but closed without replying",
        ));
    }
    serde_json::from_str(reply.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Read one request from an accepted connection.
pub fn read_request(stream: &std::os::unix::net::UnixStream) -> std::io::Result<ViewRequest> {
    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    reader.read_line(&mut line)?;
    serde_json::from_str(line.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn write_reply(
    stream: &mut std::os::unix::net::UnixStream,
    reply: &ViewReply,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(reply)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}
