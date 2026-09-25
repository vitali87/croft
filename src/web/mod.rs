//! `croft web` (#341): a WebSocket door into a workspace's session host.
//!
//! Each WebSocket connection becomes one ordinary client of the session
//! host's Unix socket, so a browser tab is a participant like any terminal:
//! it has its own output queue, attaches read-only unless it is first or is
//! granted control, and leaves the roster when the tab closes. Frames map
//! one to one: the host's byte frames are binary messages, its control
//! frames are JSON text messages, and the same two shapes go back. See
//! docs/WEB.md.
//!
//! The Unix socket is owner-only (0600); a loopback TCP port is open to
//! every local user and to any web page the user visits. So a connection
//! must carry the per-run token printed at start-up, a browser's `Origin`
//! must be croft web's own, and only loopback addresses are served.

pub mod ws;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::session_host::{Control, Frame, FrameReader, encode_bytes_frame, encode_control_frame};

/// Where `croft web` listens unless `--bind` says otherwise.
pub const DEFAULT_PORT: u16 = 7681;

/// `croft web [--bind ADDR]` for the session of `workspace`.
pub fn run(workspace: &Path, bind: Option<SocketAddr>) -> Result<()> {
    let addr = bind.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)));
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "croft web serves loopback addresses only; {} is not one",
        addr.ip()
    );
    let root = workspace
        .canonicalize()
        .with_context(|| format!("{} is not a directory", workspace.display()))?;
    let socket = crate::session::mux_socket_path(&root);
    anyhow::ensure!(
        crate::session::is_alive(&socket),
        "no croft session is running for {}; start one with `croft attach`",
        root.display()
    );
    let token = token()?;
    let listener = TcpListener::bind(addr).with_context(|| format!("binding {addr}"))?;
    let local = listener.local_addr()?;
    println!(
        "croft web: the session for {} at ws://{local}/?token={token}",
        root.display()
    );
    serve(listener, socket, token);
    Ok(())
}

fn token() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("RNG failed: {e:?}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Accept connections forever, one thread each.
pub fn serve(listener: TcpListener, socket: PathBuf, token: String) {
    let local = listener.local_addr().ok();
    for stream in listener.incoming().flatten() {
        let (socket, token) = (socket.clone(), token.clone());
        std::thread::spawn(move || {
            let _ = handle(stream, &socket, &token, local);
        });
    }
}

/// A browser's `Origin` must be croft web's own page; a request with none
/// is not from a browser page at all (a script or test client) and is
/// judged by its token alone.
fn origin_allowed(origin: Option<&str>, local: Option<SocketAddr>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some(local) = local else {
        return false;
    };
    origin == format!("http://{local}") || origin == format!("http://localhost:{}", local.port())
}

fn handle(
    mut tcp: TcpStream,
    socket: &Path,
    token: &str,
    local: Option<SocketAddr>,
) -> std::io::Result<()> {
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let request = ws::read_request(&mut tcp)?;
    tcp.set_read_timeout(None)?;
    let Some(key) = request.websocket_key().map(str::to_string) else {
        return tcp.write_all(
            ws::refusal("426 Upgrade Required", "croft web speaks WebSocket\n").as_bytes(),
        );
    };
    if request.query("token") != Some(token) {
        return tcp.write_all(ws::refusal("403 Forbidden", "wrong or missing token\n").as_bytes());
    }
    if !origin_allowed(request.header("origin"), local) {
        return tcp.write_all(ws::refusal("403 Forbidden", "foreign origin\n").as_bytes());
    }
    let unix = match UnixStream::connect(socket) {
        Ok(u) => u,
        Err(_) => {
            return tcp.write_all(
                ws::refusal("503 Service Unavailable", "the session has ended\n").as_bytes(),
            );
        }
    };
    tcp.write_all(ws::upgrade_response(&key).as_bytes())?;
    bridge(tcp, unix)
}

/// Relay between one WebSocket and one session-host connection until either
/// side goes. Both directions write to the socket, so the writer is shared
/// behind a lock and frames never interleave.
fn bridge(tcp: TcpStream, unix: UnixStream) -> std::io::Result<()> {
    let writer = Arc::new(Mutex::new(tcp.try_clone()?));
    let host_to_browser = {
        let writer = Arc::clone(&writer);
        let mut from_host = unix.try_clone()?;
        std::thread::spawn(move || {
            let mut frames = FrameReader::new();
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = from_host.read(&mut buf) {
                if n == 0 {
                    break;
                }
                for frame in frames.push(&buf[..n]) {
                    let message = match frame {
                        Frame::Bytes(bytes) => ws::binary(&bytes),
                        Frame::Control(control) => match serde_json::to_string(&control) {
                            Ok(json) => ws::text(&json),
                            Err(_) => continue,
                        },
                    };
                    if writer.lock().unwrap().write_all(&message).is_err() {
                        return;
                    }
                }
            }
            let w = writer.lock().unwrap();
            let _ = (&*w).write_all(&ws::close());
            let _ = w.shutdown(std::net::Shutdown::Both);
        })
    };
    let mut from_browser = tcp;
    let mut to_host = unix;
    let mut decoder = ws::Decoder::default();
    let mut buf = vec![0u8; 16 * 1024];
    'read: while let Ok(n) = from_browser.read(&mut buf) {
        if n == 0 {
            break;
        }
        let Ok(messages) = decoder.push(&buf[..n]) else {
            break;
        };
        for message in messages {
            let sent = match message {
                ws::Message::Binary(bytes) => to_host.write_all(&encode_bytes_frame(&bytes)),
                // A control message the host would not understand is
                // dropped here, as the host itself drops malformed ones.
                ws::Message::Text(json) => match serde_json::from_str::<Control>(&json) {
                    Ok(control) => to_host.write_all(&encode_control_frame(&control)),
                    Err(_) => Ok(()),
                },
                ws::Message::Ping(data) => writer.lock().unwrap().write_all(&ws::pong(&data)),
                ws::Message::Pong => Ok(()),
                ws::Message::Close => {
                    let _ = writer.lock().unwrap().write_all(&ws::close());
                    break 'read;
                }
            };
            if sent.is_err() {
                break 'read;
            }
        }
    }
    // The host sees the connection close and takes the participant off the
    // roster, exactly as for a terminal client that detached.
    let _ = to_host.shutdown(std::net::Shutdown::Both);
    let _ = host_to_browser.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn only_croft_webs_own_page_may_connect_from_a_browser() {
        let local = Some(SocketAddr::from(([127, 0, 0, 1], 7681)));
        assert!(
            origin_allowed(None, local),
            "not a browser page: the token decides"
        );
        assert!(origin_allowed(Some("http://127.0.0.1:7681"), local));
        assert!(origin_allowed(Some("http://localhost:7681"), local));
        assert!(!origin_allowed(Some("https://evil.example"), local));
        assert!(!origin_allowed(Some("http://127.0.0.1:9999"), local));
    }

    #[test]
    fn a_non_loopback_address_is_refused_before_anything_binds() {
        let err = run(Path::new("/"), Some(SocketAddr::from(([0, 0, 0, 0], 0)))).unwrap_err();
        assert!(err.to_string().contains("loopback"), "{err}");
    }

    /// A minimal WebSocket client over the bridge.
    struct Client {
        tcp: TcpStream,
        decoder: ServerDecoder,
    }

    /// Decodes the server's (unmasked) frames.
    #[derive(Default)]
    struct ServerDecoder {
        buf: Vec<u8>,
    }

    impl ServerDecoder {
        fn push(&mut self, data: &[u8]) -> Vec<(u8, Vec<u8>)> {
            self.buf.extend_from_slice(data);
            let mut out = Vec::new();
            loop {
                if self.buf.len() < 2 {
                    break;
                }
                let op = self.buf[0] & 0x0F;
                let (len, at) = match self.buf[1] {
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
                if self.buf.len() < at + len {
                    break;
                }
                out.push((op, self.buf[at..at + len].to_vec()));
                self.buf.drain(..at + len);
            }
            out
        }
    }

    impl Client {
        fn connect(addr: SocketAddr, target: &str, origin: Option<&str>) -> (TcpStream, String) {
            let mut tcp = TcpStream::connect(addr).unwrap();
            let origin = origin
                .map(|o| format!("Origin: {o}\r\n"))
                .unwrap_or_default();
            write!(
                tcp,
                "GET {target} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{origin}\r\n"
            )
            .unwrap();
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && tcp.read(&mut b).unwrap() == 1 {
                head.push(b[0]);
            }
            (tcp, String::from_utf8_lossy(&head).into_owned())
        }

        fn open(addr: SocketAddr, token: &str) -> Self {
            let (tcp, head) = Self::connect(addr, &format!("/?token={token}"), None);
            assert!(head.starts_with("HTTP/1.1 101"), "{head}");
            assert!(head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
            tcp.set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            Self {
                tcp,
                decoder: ServerDecoder::default(),
            }
        }

        fn send(&mut self, opcode: u8, payload: &[u8]) {
            self.tcp
                .write_all(&ws::client_frame(opcode, true, payload))
                .unwrap();
        }

        /// Read messages until `done` says so, within five seconds.
        fn until(&mut self, done: impl Fn(&[(u8, Vec<u8>)]) -> bool) -> Vec<(u8, Vec<u8>)> {
            let end = Instant::now() + Duration::from_secs(5);
            let mut seen = Vec::new();
            let mut buf = [0u8; 8192];
            while !done(&seen) {
                assert!(
                    Instant::now() < end,
                    "timed out; saw {} messages",
                    seen.len()
                );
                if let Ok(n) = self.tcp.read(&mut buf) {
                    seen.extend(self.decoder.push(&buf[..n]));
                }
            }
            seen
        }
    }

    fn texts(seen: &[(u8, Vec<u8>)]) -> String {
        seen.iter()
            .filter(|(op, _)| *op == 0x1)
            .map(|(_, p)| String::from_utf8_lossy(p).into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn bytes(seen: &[(u8, Vec<u8>)]) -> Vec<u8> {
        seen.iter()
            .filter(|(op, _)| *op == 0x2)
            .flat_map(|(_, p)| p.clone())
            .collect()
    }

    /// The acceptance run: a real session host (`cat` inside), a browser
    /// client through the bridge and a terminal client on the Unix socket.
    /// The browser appears in the roster, its keystrokes reach the PTY, both
    /// clients get the same bytes, and closing the socket takes the browser
    /// off the roster.
    #[test]
    fn a_websocket_client_is_a_participant_like_any_other() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        {
            let socket = socket.clone();
            std::thread::spawn(move || {
                crate::session_host::serve_with_token(&socket, None, &[String::from("cat")], "t")
            });
        }
        let end = Instant::now() + Duration::from_secs(5);
        while !crate::session::is_alive(&socket) {
            assert!(Instant::now() < end, "the host never came up");
            std::thread::sleep(Duration::from_millis(20));
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        {
            let socket = socket.clone();
            std::thread::spawn(move || serve(listener, socket, String::from("secret")));
        }

        // Refusals first: no upgrade, a wrong token, a foreign page.
        let (_t, head) = Client::connect(addr, "/?token=nope", None);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        let (_t, head) = Client::connect(addr, "/?token=secret", Some("https://evil.example"));
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");

        let mut browser = Client::open(addr, "secret");
        browser.send(
            0x1,
            br#"{"t":"hello","name":"browser","cols":80,"rows":24}"#,
        );
        let seen = browser.until(|s| texts(s).contains("\"browser\""));
        assert!(texts(&seen).contains("presence"), "{}", texts(&seen));

        // A terminal client on the Unix socket, attached alongside.
        let mut term = UnixStream::connect(&socket).unwrap();
        term.write_all(&encode_control_frame(&Control::Hello {
            name: String::from("term"),
            cols: 80,
            rows: 24,
            version: String::new(),
            client_id: String::new(),
        }))
        .unwrap();
        term.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut term_frames = FrameReader::new();
        let mut term_bytes = Vec::new();
        let mut term_roster = String::new();
        let mut read_term = |term: &mut UnixStream, bytes: &mut Vec<u8>, roster: &mut String| {
            let mut buf = [0u8; 8192];
            if let Ok(n) = term.read(&mut buf) {
                for f in term_frames.push(&buf[..n]) {
                    match f {
                        Frame::Bytes(b) => bytes.extend(b),
                        Frame::Control(c) => *roster = serde_json::to_string(&c).unwrap(),
                    }
                }
            }
        };

        // The host broadcasts output only to clients it has registered, so
        // the browser types only once the roster names the terminal client.
        // Typing on its heels raced the terminal's hello, and the echo went
        // out before the terminal was there to receive it.
        browser.until(|s| texts(s).contains("\"term\""));

        // The browser attached first, so it holds control: its keystrokes
        // reach `cat`, and both clients see the echo.
        browser.send(0x2, b"from-the-browser\r");
        let seen =
            browser.until(|s| String::from_utf8_lossy(&bytes(s)).contains("from-the-browser"));
        assert!(String::from_utf8_lossy(&bytes(&seen)).contains("from-the-browser"));
        let end = Instant::now() + Duration::from_secs(5);
        while !String::from_utf8_lossy(&term_bytes).contains("from-the-browser") {
            assert!(
                Instant::now() < end,
                "the terminal client never saw the echo"
            );
            read_term(&mut term, &mut term_bytes, &mut term_roster);
        }

        // Closing the tab: the host drops the browser from the roster.
        browser.send(0x8, b"");
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            read_term(&mut term, &mut term_bytes, &mut term_roster);
            if term_roster.contains("presence") && !term_roster.contains("\"browser\"") {
                break;
            }
            assert!(
                Instant::now() < end,
                "the browser stayed on the roster: {term_roster}"
            );
        }
    }
}
