//! `croft web` (#341, #343): a WebSocket door into a workspace's session host.
//!
//! Each WebSocket connection becomes one ordinary client of the session
//! host's Unix socket, so a browser tab is a participant like any terminal:
//! it has its own output queue, attaches read-only unless it is first or is
//! granted control, and leaves the roster when the tab closes. Frames map
//! one to one: the host's byte frames are binary messages, its control
//! frames are JSON text messages, and the same two shapes go back. See
//! docs/WEB.md.
//!
//! The Unix socket is owner-only (0600); a TCP port is open to every local
//! user and to any web page the user visits. So a connection must present
//! the per-run token, as a WebSocket subprotocol rather than in the URL
//! where logs and history would keep it; a browser's `Origin` must be the
//! page's own; and the listener binds loopback unless `--bind` says
//! otherwise, which it warns about.

pub mod tls;
pub mod ws;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::session_host::{Control, Frame, FrameReader, encode_bytes_frame, encode_control_frame};

/// Where `croft web` listens unless `--bind` says otherwise.
pub const DEFAULT_PORT: u16 = 7681;

/// The subprotocol a client offers, and the one the server selects.
pub const PROTOCOL: &str = "croft";

/// The prefix of the subprotocol that carries the token. A browser cannot
/// set headers on a WebSocket, but it can list subprotocols, and those go
/// in a header: `new WebSocket(url, ["croft", "croft.token." + token])`.
pub const TOKEN_PROTOCOL_PREFIX: &str = "croft.token.";

/// How often a listener checks that its session is still running.
const HOST_POLL: Duration = Duration::from_secs(2);

/// What `croft web` was asked to do.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Options {
    /// Address to listen on; loopback port 7681 when unset.
    pub bind: Option<SocketAddr>,
    /// PEM certificate chain and private key to serve TLS with.
    pub tls: Option<(PathBuf, PathBuf)>,
    /// Stop the workspace's listener and start one with a fresh token.
    pub rotate_token: bool,
    /// Stop the workspace's listener and exit, leaving the session running.
    pub off: bool,
}

/// `croft web` for the session of `workspace`.
pub fn run(workspace: &Path, opts: Options) -> Result<()> {
    let root = workspace
        .canonicalize()
        .with_context(|| format!("{} is not a directory", workspace.display()))?;
    let socket = crate::session::mux_socket_path(&root);
    let record = record_path(&socket);

    if opts.off {
        match stop(&record) {
            Some(old) => println!("croft web: stopped the listener at {}", old.url),
            None => println!("croft web: no listener is serving {}", root.display()),
        }
        return Ok(());
    }
    let mut bind = opts.bind;
    if let Some(old) = live_record(&record) {
        anyhow::ensure!(
            opts.rotate_token,
            "already serving {} at {} (pid {}); `croft web --rotate-token` restarts it with a new token, `croft web --off` stops it",
            root.display(),
            old.url,
            old.pid
        );
        stop(&record);
        // The same address as before unless told otherwise, so rotating the
        // token does not move the listener.
        bind = bind.or(old.addr);
    }
    anyhow::ensure!(
        crate::session::is_alive(&socket),
        "no croft session is running for {}; start one with `croft attach`",
        root.display()
    );
    let addr = bind.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)));
    let tls = match &opts.tls {
        Some((cert, key)) => Some(tls::load(cert, key)?),
        None => None,
    };
    let token = token()?;
    let listener = TcpListener::bind(addr).with_context(|| format!("binding {addr}"))?;
    let local = listener.local_addr()?;
    let scheme = if tls.is_some() { "wss" } else { "ws" };
    let url = format!("{scheme}://{local}/");
    if !local.ip().is_loopback() {
        eprintln!("{}", exposure_warning(local, tls.is_some()));
    }
    write_record(
        &record,
        &Record {
            pid: std::process::id(),
            url: url.clone(),
            addr: Some(local),
        },
    )?;
    let page_scheme = if tls.is_some() { "https" } else { "http" };
    println!("croft web: serving {}", root.display());
    println!("  open {page_scheme}://{local}/#token={token}");
    println!(
        "  or connect to {url} offering the WebSocket subprotocols `{PROTOCOL}` and `{TOKEN_PROTOCOL_PREFIX}{token}`"
    );
    {
        let (socket, record) = (socket.clone(), record.clone());
        std::thread::spawn(move || follow_host(&socket, &record));
    }
    serve(
        listener,
        Server {
            socket,
            token,
            tls,
            loopback: local.ip().is_loopback(),
        },
    );
    Ok(())
}

/// Printed when the listener is reachable from other machines.
fn exposure_warning(local: SocketAddr, tls: bool) -> String {
    let mut s = format!(
        "croft web: WARNING: {local} is not a loopback address, so other machines can reach this listener. Anyone holding the token can watch the session, and type into it if they are given control."
    );
    if !tls {
        s.push_str(
            " Without --tls the token and everything on screen cross the network in plain text.",
        );
    }
    s
}

/// The listener lives as long as its session: when the host goes, or when
/// another `croft web` takes the record over (`--off`, `--rotate-token`),
/// this one exits.
fn follow_host(socket: &Path, record: &Path) {
    loop {
        std::thread::sleep(HOST_POLL);
        if !keep_serving(socket, record, std::process::id()) {
            std::process::exit(0);
        }
    }
}

/// Whether listener `pid` should keep serving: its record still names it
/// and its session is still running. A listener whose session ended clears
/// its record on the way out.
fn keep_serving(socket: &Path, record: &Path, pid: u32) -> bool {
    if !read_record(record).is_some_and(|r| r.pid == pid) {
        return false;
    }
    if crate::session::is_alive(socket) {
        return true;
    }
    let _ = std::fs::remove_file(record);
    false
}

fn token() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("RNG failed: {e:?}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Where a workspace's web listener is recorded, beside its session socket.
/// `croft ls` reads it to show which sessions are exposed and where.
pub fn record_path(socket: &Path) -> PathBuf {
    let mut name = socket.file_name().unwrap_or_default().to_os_string();
    name.push(".web");
    socket.with_file_name(name)
}

/// A running `croft web` listener. Never holds the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub pid: u32,
    pub url: String,
    #[serde(default)]
    pub addr: Option<SocketAddr>,
}

fn write_record(path: &Path, record: &Record) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let json = serde_json::to_string(record)?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(json.as_bytes())?;
    Ok(())
}

fn read_record(path: &Path) -> Option<Record> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The workspace's listener, if its process is still running. A record
/// left by a listener that was killed is removed.
pub fn live_record(path: &Path) -> Option<Record> {
    let record = read_record(path)?;
    if pid_alive(record.pid) {
        Some(record)
    } else {
        let _ = std::fs::remove_file(path);
        None
    }
}

fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // Signal 0 checks for existence without sending anything. EPERM means
    // the process exists but belongs to someone else.
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// Stop the workspace's listener, returning what it was.
fn stop(path: &Path) -> Option<Record> {
    let record = live_record(path)?;
    // Removing the record first means the listener also exits on its own
    // next poll, should the signal not reach it.
    let _ = std::fs::remove_file(path);
    if let Ok(pid) = libc::pid_t::try_from(record.pid) {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    let end = std::time::Instant::now() + Duration::from_secs(3);
    while pid_alive(record.pid) && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    Some(record)
}

/// What every connection is checked against.
pub struct Server {
    pub socket: PathBuf,
    pub token: String,
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// Bound to a loopback address: then a browser page must be on one too.
    pub loopback: bool,
}

/// Accept connections forever, one thread each.
pub fn serve(listener: TcpListener, server: Server) {
    let server = Arc::new(server);
    for stream in listener.incoming().flatten() {
        let server = Arc::clone(&server);
        std::thread::spawn(move || {
            let _ = match &server.tls {
                Some(config) => match tls::terminate(stream, Arc::clone(config)) {
                    Ok(plain) => handle(plain, &server, "https"),
                    Err(_) => Ok(()),
                },
                None => handle(stream, &server, "http"),
            };
        });
    }
}

/// Either transport a connection arrives on: TCP, or the plaintext side of
/// a TLS terminator.
trait Conn: Read + Write + Send + Sized + 'static {
    fn duplicate(&self) -> std::io::Result<Self>;
    fn close(&self);
    fn read_timeout(&self, t: Option<Duration>) -> std::io::Result<()>;
}

impl Conn for TcpStream {
    fn duplicate(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
    fn close(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
    fn read_timeout(&self, t: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(t)
    }
}

impl Conn for UnixStream {
    fn duplicate(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
    fn close(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
    fn read_timeout(&self, t: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(t)
    }
}

/// The token a client offered, if it also offered `croft` for the server
/// to select. A browser fails a connection whose reply selects none of the
/// subprotocols it listed, so the reply names `croft` and never the token.
fn offered_token(protocols: Option<&str>) -> Option<&str> {
    let offered: Vec<&str> = protocols?.split(',').map(str::trim).collect();
    if !offered.contains(&PROTOCOL) {
        return None;
    }
    offered
        .iter()
        .find_map(|p| p.strip_prefix(TOKEN_PROTOCOL_PREFIX))
}

/// Compare without leaking how much of the token matched through timing.
fn token_matches(offered: Option<&str>, token: &str) -> bool {
    let Some(offered) = offered else {
        return false;
    };
    offered.len() == token.len()
        && offered
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// A browser's `Origin` must be the page's own: the scheme it arrived on
/// and the `Host` it asked for. A request with no `Origin` is not from a
/// browser page (a script or test client) and is judged by its token alone.
/// On a loopback listener the host must be a loopback name as well, which
/// closes DNS rebinding: a page at a hostile name that resolves to 127.0.0.1
/// has an `Origin` and a `Host` that agree with each other, but not with
/// loopback.
fn origin_allowed(origin: Option<&str>, host: Option<&str>, scheme: &str, loopback: bool) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some(host) = host else {
        return false;
    };
    origin.eq_ignore_ascii_case(&format!("{scheme}://{host}"))
        && (!loopback || host_is_loopback(host))
}

fn host_is_loopback(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        host.rsplit_once(':').map_or(host, |(name, _)| name)
    };
    name.eq_ignore_ascii_case("localhost")
        || name
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn handle<C: Conn>(mut conn: C, server: &Server, scheme: &str) -> std::io::Result<()> {
    conn.read_timeout(Some(Duration::from_secs(10)))?;
    let request = ws::read_request(&mut conn)?;
    conn.read_timeout(None)?;
    let Some(key) = request.websocket_key().map(str::to_string) else {
        // The page is static and holds no secret, so it is served to anyone
        // who can reach the port; the session behind it needs the token.
        return match page(&request.target) {
            Some((content_type, body)) => {
                conn.write_all(&page_response(content_type, body, request.header("host")))
            }
            None => conn.write_all(ws::refusal("404 Not Found", "not found\n").as_bytes()),
        };
    };
    let offered = offered_token(request.header("sec-websocket-protocol"));
    if !token_matches(offered, &server.token) {
        return conn.write_all(ws::refusal("403 Forbidden", "wrong or missing token\n").as_bytes());
    }
    if !origin_allowed(
        request.header("origin"),
        request.header("host"),
        scheme,
        server.loopback,
    ) {
        return conn.write_all(ws::refusal("403 Forbidden", "foreign origin\n").as_bytes());
    }
    let unix = match UnixStream::connect(&server.socket) {
        Ok(u) => u,
        Err(_) => {
            return conn.write_all(
                ws::refusal("503 Service Unavailable", "the session has ended\n").as_bytes(),
            );
        }
    };
    conn.write_all(ws::upgrade_response(&key, Some(PROTOCOL)).as_bytes())?;
    bridge(conn, unix)
}

/// The page (#342): markup, the terminal model and the glue, embedded so
/// `croft web` needs nothing beside the binary.
const INDEX_HTML: &str = include_str!("page/index.html");
const TERM_JS: &str = include_str!("page/term.js");
const APP_JS: &str = include_str!("page/app.js");
/// The Nerd Font glyphs croft's icons use, subset by
/// scripts/web_icon_font.py so a browser needs no font installed.
const ICONS_WOFF2: &[u8] = include_bytes!("page/icons.woff2");

/// What the page serves at `target`, as (content type, body).
fn page(target: &str) -> Option<(&'static str, &'static [u8])> {
    let path = target.split(['?', '#']).next().unwrap_or_default();
    match path {
        "/" | "/index.html" => Some(("text/html; charset=utf-8", INDEX_HTML.as_bytes())),
        "/term.js" => Some(("text/javascript; charset=utf-8", TERM_JS.as_bytes())),
        "/app.js" => Some(("text/javascript; charset=utf-8", APP_JS.as_bytes())),
        "/icons.woff2" => Some(("font/woff2", ICONS_WOFF2)),
        _ => None,
    }
}

/// A page response. The Content-Security-Policy allows only the page's own
/// scripts, inline images decoded from the session, and a WebSocket back to
/// the host it was loaded from; no other network request can be made.
fn page_response(content_type: &str, body: &[u8], host: Option<&str>) -> Vec<u8> {
    // Only a plain host[:port] goes into the policy, so a crafted Host
    // header cannot add directives to it.
    let host = host.filter(|h| {
        !h.is_empty()
            && h.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
    });
    let sockets = host
        .map(|h| format!(" ws://{h} wss://{h}"))
        .unwrap_or_default();
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; img-src data:; font-src 'self'; connect-src 'self'{sockets}; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// Relay between one WebSocket and one session-host connection until either
/// side goes. Both directions write to the socket, so the writer is shared
/// behind a lock and frames never interleave.
fn bridge<C: Conn>(conn: C, unix: UnixStream) -> std::io::Result<()> {
    let writer = Arc::new(Mutex::new(conn.duplicate()?));
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
            let mut w = writer.lock().unwrap();
            let _ = w.write_all(&ws::close());
            w.close();
        })
    };
    let mut from_browser = conn;
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
    fn a_browser_page_must_be_the_one_it_asked_for() {
        assert!(
            origin_allowed(None, Some("127.0.0.1:7681"), "http", true),
            "not a browser page: the token decides"
        );
        assert!(origin_allowed(
            Some("http://127.0.0.1:7681"),
            Some("127.0.0.1:7681"),
            "http",
            true
        ));
        assert!(origin_allowed(
            Some("http://localhost:7681"),
            Some("localhost:7681"),
            "http",
            true
        ));
        assert!(origin_allowed(
            Some("https://[::1]:7681"),
            Some("[::1]:7681"),
            "https",
            true
        ));
        // Another page, another port, or the wrong scheme.
        let host = Some("127.0.0.1:7681");
        assert!(!origin_allowed(
            Some("https://evil.example"),
            host,
            "http",
            true
        ));
        assert!(!origin_allowed(
            Some("http://127.0.0.1:9999"),
            host,
            "http",
            true
        ));
        assert!(!origin_allowed(
            Some("http://127.0.0.1:7681"),
            host,
            "https",
            true
        ));
        assert!(!origin_allowed(
            Some("http://127.0.0.1:7681"),
            None,
            "http",
            true
        ));
        // DNS rebinding: a hostile name resolving to loopback agrees with
        // its own Host header, and loopback still refuses it.
        let rebound = Some("evil.example:7681");
        assert!(!origin_allowed(
            Some("http://evil.example:7681"),
            rebound,
            "http",
            true
        ));
        // Bound to a routable address, a real hostname is the page's own.
        assert!(origin_allowed(
            Some("https://devbox.tail1234.ts.net:7681"),
            Some("devbox.tail1234.ts.net:7681"),
            "https",
            false
        ));
    }

    #[test]
    fn the_token_travels_as_a_subprotocol_beside_croft() {
        assert_eq!(offered_token(Some("croft, croft.token.abc")), Some("abc"));
        assert_eq!(offered_token(Some("croft.token.abc,croft")), Some("abc"));
        // Without `croft` a browser would fail the reply, which cannot name
        // the token; without the token there is nothing to check.
        assert_eq!(offered_token(Some("croft.token.abc")), None);
        assert_eq!(offered_token(Some("croft")), None);
        assert_eq!(offered_token(None), None);
        assert!(token_matches(Some("abc"), "abc"));
        assert!(!token_matches(Some("abd"), "abc"));
        assert!(!token_matches(Some("ab"), "abc"));
        assert!(!token_matches(None, "abc"));
    }

    #[test]
    fn the_reply_selects_croft_and_never_echoes_the_token() {
        let reply = ws::upgrade_response("dGhlIHNhbXBsZSBub25jZQ==", Some(PROTOCOL));
        assert!(
            reply.contains("\r\nSec-WebSocket-Protocol: croft\r\n"),
            "{reply}"
        );
        assert!(!reply.contains("token"), "{reply}");
    }

    #[test]
    fn a_routable_address_is_served_with_a_warning() {
        let addr = SocketAddr::from(([192, 0, 2, 7], 7681));
        let plain = exposure_warning(addr, false);
        assert!(
            plain.contains("192.0.2.7:7681") && plain.contains("plain text"),
            "{plain}"
        );
        let tls = exposure_warning(addr, true);
        assert!(!tls.contains("plain text"), "{tls}");
    }

    #[test]
    fn a_record_outlives_its_listener_only_until_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("abc.mux.sock");
        let path = record_path(&socket);
        assert_eq!(path, dir.path().join("abc.mux.sock.web"));
        let live = Record {
            pid: std::process::id(),
            url: String::from("ws://127.0.0.1:7681/"),
            addr: Some(SocketAddr::from(([127, 0, 0, 1], 7681))),
        };
        write_record(&path, &live).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(live_record(&path), Some(live));
        // A listener that was killed leaves its record behind; reading it
        // clears it. A child that has exited and been reaped is a pid
        // nothing holds.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        write_record(
            &path,
            &Record {
                pid: dead,
                url: String::from("ws://127.0.0.1:7681/"),
                addr: None,
            },
        )
        .unwrap();
        assert_eq!(live_record(&path), None);
        assert!(!path.exists());
    }

    fn write_upgrade(w: &mut impl Write, addr: SocketAddr, protocols: &str, origin: Option<&str>) {
        let origin = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        write!(
            w,
            "GET / HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {protocols}\r\n{origin}\r\n"
        )
        .unwrap();
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
        fn connect(addr: SocketAddr, protocols: &str, origin: Option<&str>) -> (TcpStream, String) {
            let mut tcp = TcpStream::connect(addr).unwrap();
            write_upgrade(&mut tcp, addr, protocols, origin);
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && tcp.read(&mut b).unwrap() == 1 {
                head.push(b[0]);
            }
            (tcp, String::from_utf8_lossy(&head).into_owned())
        }

        fn open(addr: SocketAddr, token: &str) -> Self {
            let (tcp, head) = Self::connect(addr, &format!("croft, croft.token.{token}"), None);
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
            std::thread::spawn(move || {
                serve(
                    listener,
                    Server {
                        socket,
                        token: String::from("secret"),
                        tls: None,
                        loopback: true,
                    },
                )
            });
        }

        // A plain GET is the page, with no token needed.
        let mut plain = TcpStream::connect(addr).unwrap();
        write!(plain, "GET / HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
        let mut got = String::new();
        plain.read_to_string(&mut got).unwrap();
        assert!(got.starts_with("HTTP/1.1 200 OK"), "{got}");
        assert!(got.contains("<canvas id=\"grid\">"), "{got}");

        // Refusals: a wrong token, no `croft`, a foreign page.
        let (_t, head) = Client::connect(addr, "croft, croft.token.nope", None);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        let (_t, head) = Client::connect(addr, "croft.token.secret", None);
        assert!(
            head.starts_with("HTTP/1.1 403"),
            "no `croft` offered: {head}"
        );
        let (_t, head) = Client::connect(
            addr,
            "croft, croft.token.secret",
            Some("https://evil.example"),
        );
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

    #[test]
    fn the_page_is_served_with_a_policy_that_allows_nothing_else() {
        let (ty, _) = page("/").unwrap();
        assert!(ty.starts_with("text/html"));
        assert!(INDEX_HTML.contains("term.js") && INDEX_HTML.contains("app.js"));
        assert!(INDEX_HTML.contains("icons.woff2"));
        assert_eq!(page("/?x=1").map(|p| p.1), Some(INDEX_HTML.as_bytes()));
        assert_eq!(page("/term.js").map(|p| p.1), Some(TERM_JS.as_bytes()));
        assert_eq!(page("/app.js").map(|p| p.1), Some(APP_JS.as_bytes()));
        assert_eq!(page("/icons.woff2"), Some(("font/woff2", ICONS_WOFF2)));
        assert!(ICONS_WOFF2.starts_with(b"wOF2"));
        assert_eq!(page("/../etc/passwd"), None);
        assert_eq!(page("/favicon.ico"), None);

        let reply =
            String::from_utf8(page_response("text/html", b"hi", Some("127.0.0.1:7681"))).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
        assert!(reply.contains("Content-Length: 2\r\n"), "{reply}");
        assert!(reply.contains("default-src 'none'"), "{reply}");
        assert!(
            reply.contains("connect-src 'self' ws://127.0.0.1:7681 wss://127.0.0.1:7681;"),
            "{reply}"
        );
        assert!(reply.ends_with("\r\n\r\nhi"));
        // A Host header cannot write into the policy.
        let reply =
            String::from_utf8(page_response("text/html", b"", Some("x; script-src *"))).unwrap();
        assert!(!reply.contains("script-src *"), "{reply}");
        assert!(reply.contains("connect-src 'self'; base-uri"), "{reply}");
    }

    /// #342's budget: the whole page, icon font included, under 300 KB, with
    /// no external request anywhere in it.
    #[test]
    fn the_page_stays_small_and_self_contained() {
        let total = INDEX_HTML.len() + TERM_JS.len() + APP_JS.len() + ICONS_WOFF2.len();
        assert!(total < 300 * 1024, "the page is {total} bytes");
        for (name, text) in [
            ("index.html", INDEX_HTML),
            ("term.js", TERM_JS),
            ("app.js", APP_JS),
        ] {
            for scheme in ["http://", "https://", "//cdn", "@import"] {
                assert!(!text.contains(scheme), "{name} references {scheme}");
            }
        }
    }

    #[test]
    fn a_listener_stops_with_its_session_or_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let record = record_path(&socket);
        let host = crate::session::bind_socket_0600(&socket).unwrap();
        let me = Record {
            pid: 42,
            url: String::from("ws://127.0.0.1:7681/"),
            addr: None,
        };
        write_record(&record, &me).unwrap();
        assert!(keep_serving(&socket, &record, 42));
        // `--off` or `--rotate-token` took the record over.
        assert!(!keep_serving(&socket, &record, 43));
        // The session ended: stop, and leave no record for `croft ls`.
        drop(host);
        let _ = std::fs::remove_file(&socket);
        assert!(!keep_serving(&socket, &record, 42));
        assert!(!record.exists());
    }

    /// `--tls`: the upgrade happens inside TLS, with the same token and
    /// origin rules, and a client that skips the handshake gets nothing.
    /// The certificate in testdata/ is a self-signed one for `localhost`
    /// made for this test and nothing else.
    #[test]
    fn the_upgrade_is_served_inside_tls() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/web/testdata");
        let config = tls::load(&here.join("cert.pem"), &here.join("key.pem")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("gone.mux.sock");
        std::thread::spawn(move || {
            serve(
                listener,
                Server {
                    socket,
                    token: String::from("secret"),
                    tls: Some(config),
                    loopback: true,
                },
            )
        });

        use rustls::pki_types::pem::PemObject;
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls::pki_types::CertificateDer::pem_file_iter(here.join("cert.pem")).unwrap()
        {
            roots.add(cert.unwrap()).unwrap();
        }
        let client = Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
        );
        let reply = |protocols: &str, origin: Option<&str>| {
            let tcp = TcpStream::connect(addr).unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let conn =
                rustls::ClientConnection::new(Arc::clone(&client), "localhost".try_into().unwrap())
                    .unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            write_upgrade(&mut tls, addr, protocols, origin);
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && tls.read(&mut b).unwrap_or(0) == 1 {
                head.push(b[0]);
            }
            String::from_utf8_lossy(&head).into_owned()
        };
        // Past the token and origin checks, only the ended session refuses.
        let head = reply("croft, croft.token.secret", None);
        assert!(head.starts_with("HTTP/1.1 503"), "{head}");
        let head = reply("croft, croft.token.nope", None);
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        // An `http://` page is not the `https://` page this listener serves.
        let head = reply("croft, croft.token.secret", Some(&format!("http://{addr}")));
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");

        // Plain HTTP at a TLS listener is never answered in plain text.
        let mut plain = TcpStream::connect(addr).unwrap();
        plain
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write_upgrade(&mut plain, addr, "croft, croft.token.secret", None);
        let mut got = Vec::new();
        let _ = plain.read_to_end(&mut got);
        assert!(
            !String::from_utf8_lossy(&got).contains("HTTP/1.1"),
            "{got:?}"
        );
    }
}
