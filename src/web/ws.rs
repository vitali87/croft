//! The WebSocket protocol, as much of RFC 6455 as a session client needs:
//! the HTTP/1.1 upgrade, and framing for text, binary, ping, pong and
//! close messages. Vendored rather than a framework dependency, the same
//! way croft speaks LSP, DAP and MCP (#341).

use std::collections::HashMap;
use std::io::Read;

/// The GUID RFC 6455 appends to the client's key before hashing.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Longest request head accepted before the upgrade: a browser's is well
/// under 2 KiB.
const MAX_HEAD: usize = 16 * 1024;

/// Largest message accepted from a client. Keystrokes and control JSON are
/// tiny; this only bounds a hostile peer.
pub const MAX_MESSAGE: usize = 1 << 20;

/// An HTTP request head: its target (path and query) and headers, names
/// lower-cased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub target: String,
    pub headers: HashMap<String, String>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// A `?name=value` from the target.
    pub fn query(&self, name: &str) -> Option<&str> {
        let (_, query) = self.target.split_once('?')?;
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v)
    }

    /// A WebSocket upgrade request, with the key the reply must hash.
    pub fn websocket_key(&self) -> Option<&str> {
        let upgrade = self.header("upgrade")?.eq_ignore_ascii_case("websocket");
        let connection = self
            .header("connection")?
            .split(',')
            .any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
        (upgrade && connection).then(|| self.header("sec-websocket-key"))?
    }
}

/// Read one request head off `stream`, up to the blank line.
pub fn read_request(stream: &mut impl Read) -> std::io::Result<Request> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD || stream.read(&mut byte)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incomplete or oversized request",
            ));
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let target = lines
        .next()
        .and_then(|l| {
            let mut parts = l.split(' ');
            (parts.next() == Some("GET")).then(|| parts.next())?
        })
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a GET"))?
        .to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Ok(Request { target, headers })
}

/// `Sec-WebSocket-Accept` for a client key.
pub fn accept_key(key: &str) -> String {
    use base64::Engine as _;
    let digest = ring::digest::digest(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        format!("{key}{GUID}").as_bytes(),
    );
    base64::engine::general_purpose::STANDARD.encode(digest.as_ref())
}

/// The 101 reply that completes the upgrade.
pub fn upgrade_response(key: &str) -> String {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    )
}

/// A plain HTTP reply that refuses the request.
pub fn refusal(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// One message from the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// One server frame: final, unmasked (servers never mask).
pub fn encode(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= u16::MAX as usize => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

pub fn text(s: &str) -> Vec<u8> {
    encode(OP_TEXT, s.as_bytes())
}

pub fn binary(b: &[u8]) -> Vec<u8> {
    encode(OP_BINARY, b)
}

pub fn pong(b: &[u8]) -> Vec<u8> {
    encode(OP_PONG, b)
}

pub fn close() -> Vec<u8> {
    encode(OP_CLOSE, &[])
}

/// Turns the client's byte stream into messages. Client frames must be
/// masked (RFC 6455 5.1); an unmasked one, an oversized one, or bad UTF-8
/// in a text message is a protocol error and ends the connection.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// A fragmented message in progress: its opcode and data so far.
    partial: Option<(u8, Vec<u8>)>,
}

impl Decoder {
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Message>, &'static str> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 2 {
                break;
            }
            let fin = self.buf[0] & 0x80 != 0;
            let opcode = self.buf[0] & 0x0F;
            if self.buf[1] & 0x80 == 0 {
                return Err("client frame not masked");
            }
            let (len, mut at) = match self.buf[1] & 0x7F {
                126 if self.buf.len() >= 4 => {
                    (u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize, 4)
                }
                127 if self.buf.len() >= 10 => {
                    let mut n = [0u8; 8];
                    n.copy_from_slice(&self.buf[2..10]);
                    (u64::from_be_bytes(n) as usize, 10)
                }
                126 | 127 => break,
                n => (n as usize, 2),
            };
            if len > MAX_MESSAGE {
                return Err("message too large");
            }
            if self.buf.len() < at + 4 + len {
                break;
            }
            let mask = [
                self.buf[at],
                self.buf[at + 1],
                self.buf[at + 2],
                self.buf[at + 3],
            ];
            at += 4;
            let payload: Vec<u8> = self.buf[at..at + len]
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ mask[i % 4])
                .collect();
            self.buf.drain(..at + len);
            let whole = match opcode {
                OP_PING => {
                    out.push(Message::Ping(payload));
                    continue;
                }
                OP_PONG => {
                    out.push(Message::Pong);
                    continue;
                }
                OP_CLOSE => {
                    out.push(Message::Close);
                    continue;
                }
                OP_TEXT | OP_BINARY if self.partial.is_none() => {
                    if fin {
                        (opcode, payload)
                    } else {
                        self.partial = Some((opcode, payload));
                        continue;
                    }
                }
                OP_CONT => {
                    let Some((op, mut acc)) = self.partial.take() else {
                        return Err("continuation without a start");
                    };
                    acc.extend_from_slice(&payload);
                    if acc.len() > MAX_MESSAGE {
                        return Err("message too large");
                    }
                    if !fin {
                        self.partial = Some((op, acc));
                        continue;
                    }
                    (op, acc)
                }
                _ => return Err("unexpected opcode"),
            };
            out.push(match whole {
                (OP_TEXT, bytes) => {
                    Message::Text(String::from_utf8(bytes).map_err(|_| "text is not UTF-8")?)
                }
                (_, bytes) => Message::Binary(bytes),
            });
        }
        Ok(out)
    }
}

/// A masked client frame, for tests and test clients.
#[cfg(test)]
pub fn client_frame(opcode: u8, fin: bool, payload: &[u8]) -> Vec<u8> {
    let mask = [0x12u8, 0x34, 0x56, 0x78];
    let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
    match payload.len() {
        n if n < 126 => out.push(0x80 | n as u8),
        n => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_accept_key_matches_rfc_6455s_example() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn an_upgrade_request_is_parsed_with_its_key_and_query() {
        let head = "GET /ws?token=abc&x=1 HTTP/1.1\r\nHost: 127.0.0.1:7681\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: k==\r\nOrigin: http://127.0.0.1:7681\r\n\r\n";
        let req = read_request(&mut head.as_bytes()).unwrap();
        assert_eq!(req.target, "/ws?token=abc&x=1");
        assert_eq!(req.websocket_key(), Some("k=="));
        assert_eq!(req.query("token"), Some("abc"));
        assert_eq!(req.header("origin"), Some("http://127.0.0.1:7681"));
        let plain = read_request(&mut "GET / HTTP/1.1\r\nHost: x\r\n\r\n".as_bytes()).unwrap();
        assert_eq!(plain.websocket_key(), None);
        assert!(read_request(&mut "POST / HTTP/1.1\r\n\r\n".as_bytes()).is_err());
        assert!(
            read_request(&mut "GET / HTTP/1.1\r\n".as_bytes()).is_err(),
            "no blank line"
        );
    }

    #[test]
    fn server_frames_use_the_three_length_forms() {
        assert_eq!(text("hi"), vec![0x81, 2, b'h', b'i']);
        let mid = binary(&[7u8; 300]);
        assert_eq!(&mid[..4], &[0x82, 126, 1, 44]);
        let big = binary(&vec![0u8; 70_000]);
        assert_eq!(big[1], 127);
        assert_eq!(&big[2..10], &70_000u64.to_be_bytes());
    }

    #[test]
    fn client_frames_decode_across_reads_and_fragments() {
        let mut d = Decoder::default();
        let mut stream = client_frame(OP_TEXT, true, b"hello");
        stream.extend(client_frame(OP_BINARY, false, b"ab"));
        stream.extend(client_frame(OP_PING, true, b"p"));
        stream.extend(client_frame(OP_CONT, true, b"cd"));
        stream.extend(client_frame(OP_CLOSE, true, b""));
        // Fed one byte at a time: framing survives any split.
        let mut got = Vec::new();
        for b in &stream {
            got.extend(d.push(std::slice::from_ref(b)).unwrap());
        }
        assert_eq!(
            got,
            vec![
                Message::Text("hello".into()),
                Message::Ping(b"p".to_vec()),
                Message::Binary(b"abcd".to_vec()),
                Message::Close,
            ]
        );
    }

    #[test]
    fn protocol_errors_end_the_connection() {
        assert!(
            Decoder::default().push(&[0x81, 0x02, b'h', b'i']).is_err(),
            "unmasked"
        );
        assert!(
            Decoder::default()
                .push(&client_frame(OP_CONT, true, b"x"))
                .is_err()
        );
        assert!(
            Decoder::default()
                .push(&client_frame(OP_TEXT, true, &[0xff, 0xfe]))
                .is_err()
        );
        let mut huge = vec![0x82, 0x80 | 127];
        huge.extend_from_slice(&((MAX_MESSAGE + 1) as u64).to_be_bytes());
        assert!(Decoder::default().push(&huge).is_err());
    }
}
