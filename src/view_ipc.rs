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

/// Map sniffed content to the extension the editor's routing keys on.
///
/// The editor picks a viewer by EXTENSION (`crate::sheet::is_sheet`,
/// `Editor::open`'s arms), so staging stdin under a bare name would land
/// every piped byte in the text fallback however recognisable it was. This
/// hands the staged file a name the existing routing already understands,
/// rather than teaching the editor a second way to decide.
pub fn extension_for(magic: crate::magic::Magic) -> &'static str {
    use crate::magic::Magic;
    match magic {
        Magic::Png => "png",
        Magic::Jpeg => "jpg",
        Magic::Gif => "gif",
        Magic::WebP => "webp",
        Magic::Bmp => "bmp",
        Magic::Pdf => "pdf",
        // A zip is most often an xlsx/docx here; the sheet and docx openers
        // both fail soft to the archive view, so this is the useful guess.
        Magic::Zip => "zip",
        Magic::Gzip => "gz",
        Magic::Tar => "tar",
        Magic::Sqlite => "sqlite",
    }
}

/// Accept only a plain lowercase alphanumeric extension from `--as`.
///
/// The value becomes part of a filename croft then opens, so anything
/// carrying a separator, a dot or a NUL is refused rather than sanitised:
/// silently rewriting a user's input to something that "works" is how a
/// staged file ends up somewhere the user did not name.
pub fn sanitize_extension(raw: &str) -> Option<String> {
    let e = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    if e.is_empty() || e.len() > 16 {
        return None;
    }
    e.chars().all(|c| c.is_ascii_alphanumeric()).then_some(e)
}

/// Does this look like delimiter-separated data rather than prose?
///
/// Needed because CSV has no magic bytes: `cat data.csv | croft view -` is
/// in the issue's acceptance criteria, and by content a CSV is just text.
/// The test is deliberately strict — every one of the first lines must
/// carry the SAME non-zero count of the delimiter — because the cost of a
/// false positive (prose opening in a grid) is worse than the cost of a
/// false negative (a `--as csv` away).
pub fn looks_delimited(bytes: &[u8]) -> Option<&'static str> {
    if bytes.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(8)
        .collect();
    // One line is a sentence, not a table: a header alone tells us nothing.
    if lines.len() < 2 {
        return None;
    }
    for (delim, ext) in [(',', "csv"), ('\t', "tsv")] {
        let first = lines[0].matches(delim).count();
        if first > 0 && lines.iter().all(|l| l.matches(delim).count() == first) {
            return Some(ext);
        }
    }
    None
}

/// The extension to stage piped bytes under, if any.
///
/// `None` means "no extension": the file opens as text, which is the
/// issue's stated fallback and the right answer for a log or a diff.
pub fn stdin_extension(bytes: &[u8], hint: Option<&str>) -> Option<String> {
    if let Some(h) = hint {
        return sanitize_extension(h);
    }
    if let Some(m) = crate::magic::sniff(bytes) {
        return Some(extension_for(m).to_string());
    }
    looks_delimited(bytes).map(str::to_string)
}

/// Stage piped bytes as a file the editor can route on.
///
/// Kept in croft's cache dir rather than `/tmp` so the file survives a
/// tmpreaper mid-session and sits with the rest of croft's scratch state;
/// named by pid and a counter so two pipes in the same pane cannot collide.
pub fn stage_stdin(cache_dir: &Path, bytes: &[u8], hint: Option<&str>) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = cache_dir.join("view-stdin");
    std::fs::create_dir_all(&dir)?;
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let stem = format!("stdin-{}-{n}", std::process::id());
    let name = match stdin_extension(bytes, hint) {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem,
    };
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// `croft view <path>` / `croft view -`.
pub fn run(target: &str, as_hint: Option<&str>, cache_dir: &Path) -> anyhow::Result<()> {
    let socket = match std::env::var_os(SOCK_ENV).filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => anyhow::bail!(
            "croft view needs a croft to view in: run it from a pane inside croft ({SOCK_ENV} is unset)"
        ),
    };

    let path = if target == "-" {
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        if buf.is_empty() {
            anyhow::bail!("croft view -: nothing arrived on stdin");
        }
        stage_stdin(cache_dir, &buf, as_hint)?
    } else {
        let cwd = std::env::current_dir()?;
        let path = resolve(&cwd, Path::new(target));
        // Checked here rather than at the server so the message names the
        // path the USER typed, resolved against the cwd they typed it in.
        if !path.exists() {
            anyhow::bail!("croft view: no such file: {}", path.display());
        }
        path
    };

    match send(&socket, &ViewRequest::new(&path)) {
        Ok(ViewReply::Ok) => Ok(()),
        Ok(ViewReply::Err { message }) => anyhow::bail!("croft view: {message}"),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            // The env var outlives the croft that set it: a dtach session
            // reattached to a new croft, or a pane that survived its parent.
            anyhow::bail!(
                "croft view: the croft that opened this pane is gone (socket {})",
                socket.display()
            )
        }
        Err(e) => anyhow::bail!("croft view: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_argument_resolves_against_the_clients_cwd_not_the_servers() {
        // The whole reason the client resolves: croft's idea of a pane's cwd
        // is a scraped approximation, and `cd` moves the truth every time.
        assert_eq!(
            resolve(Path::new("/home/u/proj"), Path::new("report.pdf")),
            PathBuf::from("/home/u/proj/report.pdf")
        );
        assert_eq!(
            resolve(Path::new("/home/u/proj"), Path::new("../out/a.png")),
            PathBuf::from("/home/u/proj/../out/a.png")
        );
    }

    #[test]
    fn an_absolute_argument_is_left_exactly_alone() {
        assert_eq!(
            resolve(Path::new("/home/u"), Path::new("/etc/hosts")),
            PathBuf::from("/etc/hosts")
        );
    }

    #[test]
    fn a_tilde_is_a_filename_here_because_the_shell_already_had_its_turn() {
        // Every shell croft spawns expands `~` before exec, so one arriving
        // here was quoted and means a file of that name.
        assert_eq!(
            resolve(Path::new("/w"), Path::new("~")),
            PathBuf::from("/w/~")
        );
    }

    #[test]
    fn a_path_that_is_not_utf8_survives_the_round_trip() {
        // The reason `path` is bytes. A lossy String would turn this into
        // U+FFFD and the server would open a different file, or none.
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe-broken.png");
        let original = PathBuf::from(raw);
        assert!(
            original.to_str().is_none(),
            "fixture must be invalid UTF-8, or this test proves nothing"
        );
        let wire = serde_json::to_string(&ViewRequest::new(&original)).unwrap();
        let back: ViewRequest = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.to_path(), original);
    }

    #[test]
    fn a_path_with_a_newline_survives_because_the_encoding_is_not_the_delimiter() {
        // The wire is line-delimited, so a raw path containing `\n` would
        // truncate the request if it were written unencoded. JSON escapes it.
        let original = PathBuf::from("/tmp/two\nlines.pdf");
        let wire = serde_json::to_string(&ViewRequest::new(&original)).unwrap();
        assert!(!wire.contains('\n'), "the encoded form must be one line");
        let back: ViewRequest = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.to_path(), original);
    }

    #[test]
    fn the_socket_is_keyed_by_pid_so_two_crofts_do_not_contend() {
        let dir = Path::new("/c");
        assert_ne!(socket_path(dir, 10), socket_path(dir, 11));
        assert_eq!(socket_path(dir, 10), PathBuf::from("/c/view-10.sock"));
    }

    #[test]
    fn an_extension_hint_that_could_escape_the_staging_dir_is_refused() {
        // Refused, not sanitised: rewriting the user's input to something
        // that "works" is how a staged file lands where nobody named it.
        for bad in ["../x", "a/b", "a.b", "", "  ", "ext with space", "é"] {
            assert_eq!(sanitize_extension(bad), None, "{bad:?} must be refused");
        }
        assert_eq!(sanitize_extension("CSV").as_deref(), Some("csv"));
        assert_eq!(sanitize_extension(".png").as_deref(), Some("png"));
    }

    #[test]
    fn delimited_data_is_recognised_only_when_every_row_agrees() {
        assert_eq!(looks_delimited(b"a,b,c\n1,2,3\n4,5,6\n"), Some("csv"));
        assert_eq!(looks_delimited(b"a\tb\n1\t2\n"), Some("tsv"));
    }

    #[test]
    fn prose_with_commas_is_not_mistaken_for_a_table() {
        // The strictness is the point: a false positive opens an essay in a
        // spreadsheet grid, and the false negative costs one `--as csv`.
        assert_eq!(
            looks_delimited(
                b"Hello, world, and welcome.\nThis line, however, differs.\nOne more.\n"
            ),
            None
        );
    }

    #[test]
    fn a_single_line_is_a_sentence_rather_than_a_table() {
        // A lone header row is indistinguishable from a comma'd sentence.
        assert_eq!(looks_delimited(b"name,age,city\n"), None);
    }

    #[test]
    fn binary_content_never_reaches_the_delimiter_test() {
        assert_eq!(looks_delimited(b"a,b\n\x00\x01,\x02\n"), None);
    }

    #[test]
    fn the_hint_beats_the_sniff_which_beats_the_delimiter_guess() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR";
        // Sniff wins over nothing.
        assert_eq!(stdin_extension(png, None).as_deref(), Some("png"));
        // An explicit hint wins over the sniff: the user knows more than we do.
        assert_eq!(stdin_extension(png, Some("bin")).as_deref(), Some("bin"));
        // Delimited text is the last resort.
        assert_eq!(stdin_extension(b"a,b\n1,2\n", None).as_deref(), Some("csv"));
        // And plain text gets no extension, so it opens as text.
        assert_eq!(
            stdin_extension(b"just a log line\nand another\n", None),
            None
        );
    }

    #[test]
    fn staged_stdin_carries_the_content_and_the_routing_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let p = stage_stdin(tmp.path(), b"a,b\n1,2\n", None).unwrap();
        assert_eq!(
            p.extension().and_then(|e| e.to_str()),
            Some("csv"),
            "the editor routes on extension, so staging without one lands in text"
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"a,b\n1,2\n");
    }

    #[test]
    fn two_pipes_in_one_process_do_not_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let a = stage_stdin(tmp.path(), b"first", Some("txt")).unwrap();
        let b = stage_stdin(tmp.path(), b"second", Some("txt")).unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"first");
        assert_eq!(std::fs::read(&b).unwrap(), b"second");
    }

    #[test]
    fn a_request_and_its_reply_cross_a_real_socket() {
        // The halves are written against each other, so a test that only
        // exercised the codecs would agree with itself. This runs the actual
        // connect / write / read / reply path.
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("v.sock");
        let listener = crate::session::bind_socket_0600(&sock).unwrap();
        let sent = PathBuf::from("/tmp/some\u{fffd}where/a.pdf");
        let server = {
            let sent = sent.clone();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_request(&stream).unwrap();
                assert_eq!(req.to_path(), sent);
                write_reply(&mut stream, &ViewReply::Ok).unwrap();
            })
        };
        assert_eq!(
            send(&sock, &ViewRequest::new(&sent)).unwrap(),
            ViewReply::Ok
        );
        server.join().unwrap();
    }

    #[test]
    fn the_servers_error_text_reaches_the_client_verbatim() {
        // The side that knows why decides the message; the client prints it.
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("v.sock");
        let listener = crate::session::bind_socket_0600(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&stream);
            write_reply(
                &mut stream,
                &ViewReply::Err {
                    message: String::from("/x is a directory"),
                },
            )
            .unwrap();
        });
        let got = send(&sock, &ViewRequest::new(Path::new("/x"))).unwrap();
        assert_eq!(
            got,
            ViewReply::Err {
                message: String::from("/x is a directory")
            }
        );
        server.join().unwrap();
    }

    #[test]
    fn a_server_that_hangs_up_without_replying_is_an_error_not_a_success() {
        // Silence must not read as consent: the client would otherwise exit 0
        // having opened nothing.
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("v.sock");
        let listener = crate::session::bind_socket_0600(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Read the request BEFORE hanging up. Dropping straight after
            // accept makes the kind depend on whether the client's write
            // landed first: EPIPE if not, EOF if so. Reading first pins the
            // case this test is about — croft took the request and died.
            let _ = read_request(&stream);
            drop(stream);
        });
        let err = send(&sock, &ViewRequest::new(Path::new("/x"))).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
        server.join().unwrap();
    }
}
