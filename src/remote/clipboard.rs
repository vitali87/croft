//! The relay's clipboard requests: this machine's clipboard for a remote paste, and a
//! remote croft's copied text put on this machine's clipboard.

use super::*;

pub(super) fn handle_clipboard_request(
    host: &str,
    socket: &Path,
    inbox_dir: &str,
    request_id: &str,
) {
    let dest_dir = format!("{inbox_dir}/{request_id}");
    let mkdir = format!("mkdir -p {}", shell_quote(&dest_dir));
    let mkdir_ok = crate::proc::command("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(&mkdir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !mkdir_ok {
        write_relay_err(host, socket, inbox_dir, request_id, "remote mkdir failed");
        return;
    }
    let payload = match crate::clipboard::read_string() {
        Some(s) => s,
        None => {
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                "local clipboard unavailable",
            );
            return;
        }
    };
    let remote_cmd = format!(
        "cat > {dst}",
        dst = shell_quote(&format!("{dest_dir}/clipboard.txt")),
    );
    let mut ssh_recv = match crate::proc::command("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(&remote_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                &format!("clipboard relay spawn failed: {e}"),
            );
            return;
        }
    };
    if let Some(mut stdin) = ssh_recv.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(payload.as_bytes());
    }
    let status = ssh_recv.wait();
    match status {
        Ok(s) if s.success() => {
            let touch = format!("touch {}/.ok", shell_quote(&dest_dir));
            let _ = crate::proc::command("ssh")
                .arg("-S")
                .arg(socket)
                .arg("-o")
                .arg("ControlMaster=no")
                .arg(host)
                .arg(&touch)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        Ok(s) => write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("clipboard relay ssh exited {s}"),
        ),
        Err(e) => write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("clipboard relay wait failed: {e}"),
        ),
    }
}

/// Bring a remote croft's copied text home and put it on this machine's
/// clipboard. The reverse of [`handle_clipboard_request`], and the reason a
/// copy on a remote box reaches the clipboard the user actually pastes from
/// rather than the remote box's own (#538).
///
/// The payload file is removed in the same command that reads it: a copy is
/// transient, and leaving them behind grows the relay dir for the life of the
/// session.
///
/// Only a copy that did NOT reach this clipboard is answered, with `.err` in
/// the request's inbox directory: this machine had no clipboard to write (a
/// Linux box with no clipboard tool), or the payload never arrived. The
/// remote croft then sends it by OSC 52, which reaches this machine's
/// terminal over the SSH session. A copy that landed costs no round trip.
pub(super) fn handle_copy_request(host: &str, socket: &Path, inbox_dir: &str, request_id: &str) {
    // The id indexes a file in the inbox this pump owns, and it is
    // interpolated into a remote shell command, so it is checked before use:
    // `shell_quote` stops injection but not a `../..` that walks out of the
    // relay. Every id croft mints is `<kind>-<pid>-<seq>`.
    if !copy_request_id_is_safe(request_id) {
        return;
    }
    let quoted = shell_quote(&format!("{inbox_dir}/{request_id}.txt"));
    // One byte past the cap, so "exactly the cap" and "there was more" are
    // distinguishable and an oversized payload is DROPPED rather than pasted
    // half-formed. `head` also bounds a FIFO or a character device, which
    // `cat` would have streamed into local memory without end.
    let limit = MAX_COPY_PAYLOAD_BYTES + 1;
    let remote_cmd = format!("head -c {limit} {quoted}; rm -f {quoted}");
    // Bounded by time as well as size: `output()` waits for EOF, and this
    // pump is single-threaded and serves every other relay verb, so one
    // payload that never ends would stop drops, opens and forwards too. The
    // local cap is `limit` as well, so it does not depend on the remote box
    // having a `head` that honours -c; a payload past it is dropped below.
    let ran = crate::proc::Bounded {
        stdout: crate::proc::Keep::All { cap: limit },
        stderr: crate::proc::Keep::Nothing,
        apart: crate::proc::Apart::No,
        limit: COPY_READ_TIMEOUT,
        retry_busy: false,
    }
    .run(
        crate::proc::command("ssh")
            .arg("-S")
            .arg(socket)
            .arg("-o")
            .arg("ControlMaster=no")
            .arg(host)
            .arg(&remote_cmd),
    );
    let payload = match ran {
        Ok(ran) => ran.stdout.into_bytes(),
        Err(crate::proc::RunError::TimedOut) => {
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                "copy payload timed out",
            );
            return;
        }
        Err(_) => return,
    };
    if payload.is_empty() || payload.len() > MAX_COPY_PAYLOAD_BYTES {
        return;
    }
    if !crate::clipboard::write_string(&String::from_utf8_lossy(&payload)) {
        write_relay_err(host, socket, inbox_dir, request_id, "no clipboard to write");
    }
}

/// A copy payload the pump will bring home, capped because it lands in THIS
/// machine's memory. Generous for any real selection of scrollback.
pub(crate) const MAX_COPY_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// How long the pump waits for one copy payload. A FIFO, a stalled link or a
/// wedged remote would otherwise block every other verb behind it.
pub(super) const COPY_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Whether `id` is one croft could have minted: `<kind>-<pid>-<seq>`. Used to
/// keep a hostile log line from walking out of the inbox with `..`.
pub(super) fn copy_request_id_is_safe(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
