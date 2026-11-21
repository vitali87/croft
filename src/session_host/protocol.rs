//! The wire format between the host and its clients: the frame tags, the control messages
//! and the roster entry they carry, encoding a frame, and the decoder that turns socket
//! chunks back into frames.

use super::*;

/// Frame type tag for raw PTY bytes (either direction).
pub const FRAME_BYTES: u8 = 0;

/// Frame type tag for a JSON [`Control`] message (either direction).
pub const FRAME_CONTROL: u8 = 1;

/// Control messages exchanged between host and clients. Serialized as
/// compact JSON with a `"t"` tag so the protocol stays greppable on the wire
/// and future fields stay backward-compatible (unknown fields are ignored).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Control {
    /// First message a client must send: identity and terminal size.
    /// `version` is the client's crate version (#53); pre-0.1.698 clients
    /// never send it (`default` keeps their Hello parsing) and pre-0.1.698
    /// servers ignore the unknown field (compact JSON tolerates extras).
    Hello {
        name: String,
        cols: u16,
        rows: u16,
        #[serde(default)]
        version: String,
        /// Stable identity of the attaching *client process*, so a
        /// reconnect can displace its own previous registration (#229).
        ///
        /// When an SSH transport dies the remote croft survives under the
        /// session host and the client reconnects, but the host cannot tell
        /// that the old socket is gone: a half-open SSH connection never
        /// returns EPIPE, it just stops draining. The stale entry lingers as
        /// a ghost participant — inflating the roster badge, pinning the
        /// shared winsize through `min_winsize`, and costing every broadcast
        /// its queue until it overflows.
        ///
        /// Empty (the `default`) from pre-0.1.701 clients and from any
        /// client that cannot determine one; an empty id never displaces
        /// anything, so those clients keep the old behavior exactly.
        #[serde(default)]
        client_id: String,
    },
    /// Client terminal was resized.
    Resize { cols: u16, rows: u16 },
    /// Host to clients: the current participant roster.
    Presence { participants: Vec<Participant> },
    /// A control-holding client grants write control to participant `id`.
    Grant { id: u64 },
    /// A control-holding client revokes write control from participant `id`.
    Revoke { id: u64 },
    /// Client detaches deliberately (the session keeps running).
    Detach,
    /// Host to clients: the inner croft exited with `code`; session over.
    Exit { code: i32 },
    /// First message of the inner croft's control channel instead of Hello:
    /// authenticates with the token the host put in CROFT_SESSION_TOKEN. A
    /// privileged channel is not a participant (no roster entry, no PTY
    /// broadcast) and may grant/revoke/kick unconditionally, because every
    /// keystroke reaching the inner croft's UI already came from a
    /// control-holding client (read-only input is dropped at the host).
    Inner { token: String },
    /// Disconnect participant `id` (their session keeps running; the client
    /// pump exits as on a detach). Honored from a control-holding client or
    /// the inner channel.
    Kick { id: u64 },
    /// A client asks for write control for itself, carrying no id: the host
    /// resolves the requester, so a client never needs to learn its own id.
    /// Granted only when NOBODY holds control (#235) — the claim exists so a
    /// roster that rests all-read-only (the #234 lockout, however a bug
    /// arrives at it) is recoverable from any attached client instead of
    /// being a one-way door. The pump sends it on seeing a vacant Presence;
    /// older hosts skip the unknown variant (the frame decoder drops
    /// malformed control payloads without poisoning the stream).
    Claim,
    /// Host to the registering client, directly after its Hello (#53):
    /// the server's crate version, so the client can detect skew against a
    /// long-lived server that survived binary upgrades. Pre-0.1.698
    /// servers never send it, which the client detects by timeout;
    /// pre-0.1.698 clients skip the unknown variant (the frame decoder
    /// drops malformed control payloads without poisoning the stream).
    ServerHello { version: String },
    /// Host to privileged channels only: participant `id` is now the one
    /// typing (sent when the writing client changes, not per keystroke).
    /// Ordered before that client's bytes reach the PTY, so the inner croft
    /// can attribute the keystrokes it is about to receive and switch to
    /// that participant's caret (docs/MULTIPLAYER.md, attributed carets).
    Typing { id: u64 },
    /// Host to clients: this host is about to replace its own process image
    /// with the updated binary on disk (#238). Accepted connections cannot
    /// ride through the exec, but the listening socket does - so the client
    /// should reconnect and re-register instead of treating the coming EOF
    /// as session end. Pre-swap clients skip the unknown variant and keep
    /// the old behavior (EOF = detach), same tolerance as [`Control::Claim`].
    HostSwap,
    /// Pane host to its client, after [`Control::ServerHello`] (#694): the
    /// pid of the shell it holds, for the cwd, foreground-process and port
    /// lookups a pane makes on its shell. Session hosts never send it.
    PaneInfo { pid: u32 },
    /// Client to pane host: end the shell (the pane is being closed, or
    /// croft is quitting). Disconnecting instead leaves it running.
    Kill,
}

/// One attached client as reported in [`Control::Presence`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    pub id: u64,
    pub name: String,
    pub cols: u16,
    pub rows: u16,
    /// Whether this client's keystrokes reach the PTY (write control).
    pub control: bool,
    /// The client's croft version from its Hello (#626), so the inner
    /// croft can say when a client is older than the session. Empty from
    /// clients that send none and in rosters written by older hosts.
    #[serde(default)]
    pub version: String,
}

/// A decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Bytes(Vec<u8>),
    Control(Control),
}

/// Encode raw PTY bytes as a frame.
pub fn encode_bytes_frame(data: &[u8]) -> Vec<u8> {
    encode_frame(FRAME_BYTES, data)
}

/// Encode a control message as a frame.
pub fn encode_control_frame(control: &Control) -> Vec<u8> {
    let json = serde_json::to_vec(control).unwrap_or_default();
    encode_frame(FRAME_CONTROL, &json)
}

pub(super) fn encode_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Incremental frame decoder: feed it whatever chunk sizes the socket
/// delivers; it yields every complete frame and buffers the remainder.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            if self.buf.len() < 5 {
                break;
            }
            let kind = self.buf[0];
            let len =
                u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
            if self.buf.len() < 5 + len {
                break;
            }
            let payload: Vec<u8> = self.buf.drain(..5 + len).skip(5).collect();
            match kind {
                FRAME_BYTES => frames.push(Frame::Bytes(payload)),
                FRAME_CONTROL => {
                    // A malformed control message is skipped rather than
                    // poisoning the stream: framing already resynchronized us.
                    if let Ok(control) = serde_json::from_slice::<Control>(&payload) {
                        frames.push(Frame::Control(control));
                    }
                }
                // Unknown frame type from a newer peer: ignore the payload.
                _ => {}
            }
        }
        frames
    }
}
