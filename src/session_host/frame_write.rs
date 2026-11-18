//! Writing one frame to a socket within a deadline, so a stopped or wedged peer can never
//! hang the writer.

use super::*;

/// Total wall-clock ceiling for one frame write. The peer is another
/// process: stopped or wedged mid-write it drains nothing, and an unbounded
/// retry loop here — often on the UI thread — froze croft with no escape.
/// A peer whose buffers stay full this long is gone for practical purposes;
/// reporting failure lets the caller treat the connection as dead.
pub(crate) const WRITE_FRAME_DEADLINE: Duration = Duration::from_secs(5);

/// Write a whole frame to a socket that may be in non-blocking mode. `write_all`
/// aborts on the first `WouldBlock` even after a partial write, which would leave
/// a torn `[type][len][payload]` on the wire and desync the peer's FrameReader
/// for every later control frame. Loop until the frame is fully committed or
/// the deadline passes ([`WRITE_FRAME_DEADLINE`]): false either way means the
/// connection is unusable.
pub(crate) fn write_frame_blocking(stream: &mut UnixStream, frame: &[u8]) -> bool {
    write_frame_blocking_with_deadline(
        stream,
        frame,
        std::time::Instant::now() + WRITE_FRAME_DEADLINE,
    )
}

/// Write a whole frame to a BLOCKING socket without ever entering an
/// unbounded kernel write (#53): [`write_frame_blocking`] only bounds a
/// socket that is already non-blocking — on the server's blocking client
/// streams the first `write` against a full buffer parks in the kernel and
/// the deadline is unreachable. Waiting for writability with `poll(2)`
/// bounds the stall without flipping `O_NONBLOCK`, which is shared with the
/// client thread's blocking reads on the same file description. False means
/// the peer is unusable (dead, or draining nothing for the whole deadline).
pub(super) fn write_frame_bounded(
    stream: &mut UnixStream,
    frame: &[u8],
    deadline: Instant,
) -> bool {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut written = 0;
    while written < frame.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if r == 0 {
            // Writability never arrived inside the deadline: the peer is
            // wedged.
            return false;
        }
        if pfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return false;
        }
        match stream.write(&frame[written..]) {
            Ok(0) => return false,
            Ok(n) => written += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return false,
        }
    }
    true
}

pub(super) fn write_frame_blocking_with_deadline(
    stream: &mut impl Write,
    frame: &[u8],
    deadline: std::time::Instant,
) -> bool {
    let mut written = 0;
    while written < frame.len() {
        match stream.write(&frame[written..]) {
            Ok(0) => return false,
            Ok(n) => written += n,
            // Interrupted retries immediately but still against the
            // deadline: a signal storm must not spin past the bound this
            // function exists to guarantee.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if std::time::Instant::now() > deadline {
                    return false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return false,
        }
    }
    true
}
