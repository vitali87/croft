//! Raw JSON-RPC trace tee for the OUTPUT panel (Phase 3 of the OUTPUT tab):
//! VS Code's `"<server>.trace.server": "verbose"` channel and Zed's
//! RPC-messages toggle. [`TraceRead`] / [`TraceWrite`] wrap a language server's
//! stdio; while [`crate::output::trace_enabled`] is on they reassemble the
//! `Content-Length`-framed messages flowing each way and push one line per
//! complete message into the server's `"<name> (trace)"` channel.
//!
//! Tracing is opt-in (the panel's RPC toggle) because reassembling every frame
//! is real work on the LSP hot path: when the toggle is off, `poll_read` /
//! `poll_write` do a single relaxed atomic load and skip the framer entirely.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::io::{AsyncRead, AsyncWrite};

use crate::output::OutputLevel;

/// Which way a framed message was travelling, for the trace line's prefix.
#[derive(Clone, Copy)]
enum Direction {
    /// Server → client (a response/notification croft received).
    Incoming,
    /// Client → server (a request/notification croft sent).
    Outgoing,
}

impl Direction {
    fn prefix(self) -> &'static str {
        match self {
            Self::Incoming => "<-",
            Self::Outgoing => "->",
        }
    }
}

/// Cap on buffered bytes before the framer gives up looking for a header and
/// resyncs. Bounds memory if tracing is toggled on mid-frame (so the buffer
/// starts with a body fragment that has no `Content-Length`).
const RESYNC_LIMIT: usize = 64 * 1024;

/// Incremental `Content-Length` frame splitter. Fed only while tracing is on,
/// it emits each complete JSON body to the server's trace channel and resyncs
/// at the next header if it was started mid-stream.
struct Framer {
    channel: String,
    dir: Direction,
    buf: Vec<u8>,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(headers).ok()?;
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            return v.trim().parse::<usize>().ok();
        }
    }
    None
}

impl Framer {
    fn new(server: &str, dir: Direction) -> Self {
        Self {
            channel: format!("{server} (trace)"),
            dir,
            buf: Vec::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        loop {
            // Resync to a frame boundary: drop anything before the next header.
            // In steady state the buffer already starts here (drains nothing);
            // after a mid-stream start it discards the stray body fragment.
            match find_subslice(&self.buf, b"Content-Length:") {
                Some(0) => {}
                Some(start) => {
                    self.buf.drain(..start);
                }
                None => {
                    if self.buf.len() > RESYNC_LIMIT {
                        self.buf.clear();
                    }
                    return;
                }
            }
            let Some(hdr_end) = find_subslice(&self.buf, b"\r\n\r\n") else {
                if self.buf.len() > RESYNC_LIMIT {
                    self.buf.clear();
                }
                return;
            };
            let Some(len) = parse_content_length(&self.buf[..hdr_end]) else {
                // A header block with no Content-Length isn't an LSP frame;
                // skip past it and keep scanning.
                self.buf.drain(..hdr_end + 4);
                continue;
            };
            let body_start = hdr_end + 4;
            if self.buf.len() < body_start + len {
                return; // body still arriving
            }
            let body = &self.buf[body_start..body_start + len];
            let text = String::from_utf8_lossy(body).into_owned();
            crate::output::push(
                &self.channel,
                OutputLevel::Trace,
                &format!("{} {text}", self.dir.prefix()),
            );
            self.buf.drain(..body_start + len);
        }
    }
}

/// Wraps a server's stdout, teeing incoming frames into its trace channel.
pub struct TraceRead<R> {
    inner: R,
    framer: Framer,
}

impl<R> TraceRead<R> {
    pub fn new(inner: R, server: String) -> Self {
        Self {
            inner,
            framer: Framer::new(&server, Direction::Incoming),
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for TraceRead<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let n = std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        if n > 0 && crate::output::trace_enabled() {
            this.framer.feed(&buf[..n]);
        }
        Poll::Ready(Ok(n))
    }
}

/// Wraps a server's stdin, teeing outgoing frames into its trace channel.
pub struct TraceWrite<W> {
    inner: W,
    framer: Framer,
}

impl<W> TraceWrite<W> {
    pub fn new(inner: W, server: String) -> Self {
        Self {
            inner,
            framer: Framer::new(&server, Direction::Outgoing),
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for TraceWrite<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let n = std::task::ready!(Pin::new(&mut this.inner).poll_write(cx, buf))?;
        if n > 0 && crate::output::trace_enabled() {
            this.framer.feed(&buf[..n]);
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_close(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(json: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{json}", json.len()).into_bytes()
    }

    #[test]
    fn framer_emits_one_line_per_complete_message() {
        crate::output::set_trace_enabled(true);
        let server = "trace-test-server";
        let mut f = Framer::new(server, Direction::Incoming);
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        // Feed the frame in two arbitrary chunks to exercise reassembly.
        let bytes = frame(body);
        let (a, b) = bytes.split_at(10);
        f.feed(a);
        f.feed(b);
        let lines = crate::output::snapshot(&format!("{server} (trace)")).unwrap_or_default();
        assert_eq!(lines.len(), 1, "one complete message → one line");
        assert!(lines[0].text.contains(body), "carries the JSON body");
        assert!(lines[0].text.starts_with("<-"), "tagged with the direction");
        crate::output::set_trace_enabled(false);
    }

    #[test]
    fn framer_resyncs_when_started_mid_body() {
        let server = "trace-test-resync";
        let mut f = Framer::new(server, Direction::Outgoing);
        // A leading body fragment with no header (toggled on mid-frame), then a
        // clean frame: only the clean frame should be emitted.
        f.feed(b"garbage-without-a-header-tail");
        let body = r#"{"jsonrpc":"2.0","method":"x"}"#;
        f.feed(&frame(body));
        let lines = crate::output::snapshot(&format!("{server} (trace)")).unwrap_or_default();
        assert_eq!(lines.len(), 1, "the pre-header junk is skipped");
        assert!(lines[0].text.contains(body));
    }
}
