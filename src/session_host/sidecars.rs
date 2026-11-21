//! The files the host keeps beside its socket for the inner croft: the presence roster,
//! the stale-binary marker and the host's version, with their paths and readers.

use super::*;

/// Sidecar file the host keeps next to the socket with the current roster,
/// so the inner croft (which only has a PTY, not the socket) can poll who is
/// attached. Written atomically (tmp + rename).
pub fn presence_path(socket: &Path) -> PathBuf {
    let mut name = socket.file_name().unwrap_or_default().to_os_string();
    name.push(".presence.json");
    socket.with_file_name(name)
}

/// Sidecar the host writes when it noticed its binary was replaced on disk
/// but could NOT re-exec into it (#238): the normal path is a live handoff
/// (see [`swap_to_new_image`]) - the marker is the fallback when the exec
/// fails or the handoff fds are unavailable, so the host is genuinely stuck
/// on the old image. It lets the inner croft's status bar and `croft ls`
/// say so instead of leaving the stale host undetectable (2026-08-22: a
/// freshly shipped fix was resident nowhere until the host was killed by
/// hand).
pub fn stale_marker_path(socket: &Path) -> PathBuf {
    let mut name = socket.file_name().unwrap_or_default().to_os_string();
    name.push(".host-stale");
    socket.with_file_name(name)
}

/// Sidecar holding the croft version this host process runs (#652). A host
/// swap (#238) moves the host onto a new binary without touching the inner
/// croft, which keeps its version until it relaunches; this is how the inner
/// croft learns the version a reattaching client would get.
pub fn host_version_path(socket: &Path) -> PathBuf {
    let mut name = socket.file_name().unwrap_or_default().to_os_string();
    name.push(".host-version");
    socket.with_file_name(name)
}

/// Record this host's version next to its socket. Best-effort: without it
/// the inner croft falls back to comparing clients against itself.
pub(super) fn write_host_version(socket: &Path) {
    let _ = std::fs::write(host_version_path(socket), env!("CARGO_PKG_VERSION"));
}

/// The version the session host at `socket` runs, as its sidecar records it.
/// `None` when the host predates the sidecar or it cannot be read.
pub fn read_host_version(socket_sidecar: &Path) -> Option<String> {
    let v = std::fs::read_to_string(socket_sidecar).ok()?;
    let v = v.trim();
    parse_version(v).map(|_| v.to_string())
}

/// Parse a presence sidecar. None when the file is missing or torn (it is
/// written atomically, so torn in practice means "host gone").
pub fn read_presence(path: &Path) -> Option<Vec<Participant>> {
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}
