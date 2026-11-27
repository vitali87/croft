//! Every ssh, rsync and scp call croft makes, recorded (#2062, card 12).
//!
//! One scenario drives each entry point that starts one of them, with each
//! replaced by a stand-in that writes down what croft handed it and then
//! answers as the real program would: ssh runs the remote command on this
//! machine, in a scratch folder that is the remote's home, rsync copies the
//! one file it ships, and scp copies nothing. The record is saved in
//! `src/app/tests/golden/ssh-calls.txt`. A change to how croft starts these
//! programs must leave it byte-identical, unless the change is meant to move
//! a call, which `CROFT_BLESS=1` then writes down.
//!
//! The scenario is the same on every machine. The remote's tools are a fixed
//! few, so a croft or dtach installed here never answers for it; `uname -m`
//! there answers `x86_64`; the cross-build's tools are stand-ins too; and
//! every scratch path, the source stamp, the measured link rate, the port a
//! forward picks and the session's nonce are written as names.
//!
//! Left out, each for a reason:
//! - `install_remote_croft`'s own rsync of the release binary: it runs only
//!   for a croft installed from crates.io (`remote_prebuilt::eligible`),
//!   which a build of this checkout never is. `place_prebuilt`, which it
//!   ships for, is recorded with that path's terminal runner.
//! - `ConfigRepush`'s thread and `spawn_background_install`: each runs an
//!   entry point recorded here (`push_over_bulk_lane`,
//!   `install_only_streaming`) on a thread of its own, on a timer or beside
//!   the session, so where its calls land among the others is not fixed.
//! - `reconnect_master`: it is `SshControl::start` again, once a connection
//!   has dropped, which no stand-in connection does.

use super::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SAVED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/app/tests/golden/ssh-calls.txt"
);

/// The variable that names where [`child_records_every_ssh_call`] writes
/// its record, in the scratch folder that holds the whole scenario; the
/// test does nothing without it.
const RECORD_TO: &str = "CROFT_RECORD_SSH_CALLS_TO";

/// Variables croft might set for ssh, rsync or scp or take away from them,
/// each given [`SEEDED_VALUE`] in the scenario's environment: a call that
/// removes one shows the removal, and one that sets it shows its own value.
const SEEDED: [&str; 6] = [
    "TERM",
    "DISPLAY",
    "SSH_ASKPASS",
    "SSH_ASKPASS_REQUIRE",
    "SSH_AUTH_SOCK",
    "RSYNC_RSH",
];

/// The value every [`SEEDED`] variable starts with. The stand-ins take it
/// away again before they answer, keeping any value croft set.
const SEEDED_VALUE: &str = "croft-recorded-seed";

/// The one remote host.
const HOST: &str = "box";

/// What the remote's `uname -m` makes of it.
const TRIPLE: &str = "x86_64-unknown-linux-musl";

/// The programs the remote has, this machine's own. No croft and no dtach:
/// whether the remote has croft is up to the scenario.
const REMOTE_TOOLS: [&str; 14] = [
    "cat",
    "chmod",
    "dirname",
    "find",
    "gzip",
    "head",
    "mkdir",
    "mv",
    "rm",
    "sha256sum",
    "sleep",
    "tail",
    "tar",
    "touch",
];

/// The system's own `name`, for the remote's tools: from `/usr/bin` or
/// `/bin` where it is there, past any wrapper earlier on `PATH` that would
/// look for the program it wraps on the remote's `PATH`, else from `PATH`.
fn system_program(name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    ["/usr/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .unwrap_or_else(|| crate::proc::record::real_program(name))
}

/// The stand-in ssh, after it has written a run down. `-O` requests and
/// control masters succeed, the master leaving a plain file where its
/// socket would be: one that stays in the foreground, as the connect
/// dialog's does, runs until it is killed or asked to exit. Any other call
/// drops ssh's own flags and the host and runs the remote command here,
/// with the remote's home and tools, except the remote compile, which only
/// succeeds.
const SSH_ANSWERS: &str = r#"op=; master=; fork=; sock=
while [ $# -gt 0 ]; do
  case "$1" in
    -O) op=$2; shift 2;;
    -S) sock=$2; shift 2;;
    -o|-L|-R|-D|-p|-i|-l|-F|-J|-E|-c|-m|-b|-e|-W|-w|-B|-Q|-I) shift 2;;
    -M) master=1; shift;;
    -f) fork=1; shift;;
    --) shift; break;;
    -*) shift;;
    *) break;;
  esac
done
[ $# -gt 0 ] && shift
[ "${1-}" = -- ] && shift
if [ -n "$op" ]; then
  if [ "$op" = exit ]; then
    p=$(cat "$sock.pid" 2>/dev/null)
    [ -n "$p" ] && [ "$(tr -d '\0' < "/proc/$p/cmdline" 2>/dev/null)" = sleep600 ] && kill "$p"
    rm -f "$sock" "$sock.pid"
  fi
  exit 0
fi
if [ -n "$master" ]; then
  : > "$sock"
  [ -n "$fork" ] && exit 0
  echo $$ > "$sock.pid"
  exec sleep 600
fi
case "$*" in
  true) [ -e '@FLAGS@/probe-fails' ] && exit 255;;
  'uname -m') echo x86_64; exit 0;;
  *'cargo install --path'*) exit 0;;
esac
HOME='@REMOTE@'; PATH='@REMOTE@/.cargo/bin:@TOOLS@'; export HOME PATH
exec /bin/sh -c "$*"
"#;

/// The stand-in rsync: it copies the one file a call ships into the
/// remote's home, the croft binary being the remote's stand-in croft, and
/// copies no folder; it fails while the scenario says so.
const RSYNC_ANSWERS: &str = r#"[ -e '@FLAGS@/rsync-fails' ] && exit 12
from=; to=
while [ $# -gt 0 ]; do
  case "$1" in
    -e) shift 2;;
    -*) shift;;
    *) from=$to; to=$1; shift;;
  esac
done
case "$to" in *:*) to='@REMOTE@'/"${to#*:}";; esac
case "$to" in
  */croft.new) mkdir -p "${to%/*}" && cp '@CROFT@' "$to";;
  *) if [ -f "$from" ]; then mkdir -p "${to%/*}" && cp "$from" "$to"; fi;;
esac
exit 0
"#;

/// croft as the remote has it once installed: every command and probe
/// succeeds. A session ends at once, but only after the drop relay's tail
/// has the session's request log open, so the tail, which starts beside the
/// session, is written down before the session's end stops it.
const REMOTE_CROFT: &str = r#"#!/bin/sh
if [ "${1-}" = session-host ] && [ "${2-}" = --socket ] && [ -n "${CROFT_RELAY_KEY-}" ]; then
  log="$HOME/.cache/croft/relay-$CROFT_RELAY_KEY/requests.log"
  i=0
  while [ $i -lt 200 ] && [ -z "$(find /proc/[0-9]*/fd -maxdepth 1 -lname "$log" 2>/dev/null | head -n 1)" ]; do
    sleep 0.05
    i=$((i + 1))
  done
fi
exit 0
"#;

/// The record so far, one titled section per step.
struct Script {
    recording: crate::proc::record::Recording,
    names: Vec<(PathBuf, String)>,
    /// Text that is not a path but differs from run to run, with its name.
    words: Vec<(String, String)>,
    out: String,
}

impl Script {
    /// Write `path` as `name` from now on.
    fn name(&mut self, path: impl Into<PathBuf>, name: &str) {
        self.names.push((path.into(), name.to_string()));
    }

    /// Write `word` as `name` from now on.
    fn word(&mut self, word: impl Into<String>, name: &str) {
        self.words.push((word.into(), name.to_string()));
    }

    /// The runs since the last step, each named.
    fn runs(&mut self) -> Vec<String> {
        let names: Vec<(&Path, &str)> = self
            .names
            .iter()
            .map(|(path, name)| (path.as_path(), name.as_str()))
            .collect();
        let runs = self.recording.take(&names);
        runs.into_iter()
            .map(|run| {
                let run = self
                    .words
                    .iter()
                    .fold(run, |run, (word, name)| run.replace(word, name));
                nonce_named(&measured_rate_named(&run))
            })
            .collect()
    }

    /// Write down, under `title`, every run since the last step.
    fn step(&mut self, title: &str) {
        let runs = self.runs();
        self.write(title, runs);
    }

    /// [`Script::step`] for an entry point that starts some of its programs
    /// side by side, so the order they start in is not fixed: its runs are
    /// written sorted.
    fn step_in_any_order(&mut self, title: &str) {
        let mut runs = self.runs();
        runs.sort();
        self.write(title, runs);
    }

    fn write(&mut self, title: &str, runs: Vec<String>) {
        self.out.push_str(&format!("## {title}\n"));
        for run in runs {
            self.out.push_str(&run);
            self.out.push('\n');
        }
    }
}

/// `text` with each bulk transfer's rate cap written `<measured>`: it is
/// half what the link probe measured, which is never the same twice. A
/// cap below the floor was set, not measured, and stays.
fn measured_rate_named(text: &str) -> String {
    const FLAG: &str = "--bwlimit=";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(FLAG) {
        let after = &rest[at + FLAG.len()..];
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        out.push_str(&rest[..at + FLAG.len()]);
        match after[..digits].parse::<u32>() {
            Ok(rate) if rate >= crate::remote_bulk::BWLIMIT_FLOOR_KBPS => {
                out.push_str("<measured>")
            }
            _ => out.push_str(&after[..digits]),
        }
        rest = &after[digits..];
    }
    out.push_str(rest);
    out
}

/// `text` with the session's client nonce written `<nonce>`: croft makes a
/// new one for every process, on purpose.
fn nonce_named(text: &str) -> String {
    const KEY: &str = "CROFT_CLIENT_NONCE=";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(KEY) {
        let after = &rest[at + KEY.len()..];
        let quotes = after
            .chars()
            .take_while(|c| matches!(c, '\'' | '\\'))
            .count();
        out.push_str(&rest[..at + KEY.len() + quotes]);
        let after = &after[quotes..];
        let hex = after.chars().take_while(char::is_ascii_hexdigit).count();
        if hex == 16 {
            out.push_str("<nonce>");
            rest = &after[hex..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// How many runs the stand-ins have written down so far.
fn runs_started(bin: &Path) -> usize {
    std::fs::read_to_string(bin.join("ssh.recorded/order"))
        .map(|order| order.lines().count())
        .unwrap_or(0)
}

/// Wait until `count` runs are written down: a program croft started and
/// does not wait for may be stopped before its stand-in has written its run.
fn wait_for_runs(bin: &Path, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while runs_started(bin) < count {
        assert!(Instant::now() < deadline, "{count} runs were written down");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Turn the scenario's switch `name` on or off.
fn switch(flags: &Path, name: &str, on: bool) {
    let path = flags.join(name);
    if on {
        std::fs::write(path, "").unwrap();
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// Wait for the connect dialog's connection to authenticate.
fn authenticated(auth: &mut crate::remote_connect::SshAuth) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        for event in auth.drain_events() {
            match event {
                crate::remote_connect::SshAuthEvent::Authenticated => return,
                crate::remote_connect::SshAuthEvent::Failed(why) => panic!("{why}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "the connection authenticated");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A port nothing listens on now, for a forward to mirror.
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Run `then` with this process's stdin a terminal that `answer` has been
/// typed into, as `croft remote`'s is when it asks before a remote compile.
fn answering_on_a_terminal<T>(answer: &str, then: impl FnOnce() -> T) -> T {
    // SAFETY: each call works on a descriptor this function opens or on
    // fd 0, which it puts back before it returns; every pointer handed over
    // is to a live buffer of the stated length.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        assert!(master >= 0, "a pseudo-terminal opens");
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0);
        let terminal = libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        assert!(terminal >= 0, "its terminal side opens");
        let stdin = libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 0);
        assert!(stdin >= 0);
        assert_eq!(libc::dup2(terminal, 0), 0);
        let typed = libc::write(master, answer.as_ptr().cast(), answer.len());
        assert_eq!(typed, answer.len() as isize);
        let out = then();
        assert_eq!(libc::dup2(stdin, 0), 0);
        for fd in [stdin, terminal, master] {
            libc::close(fd);
        }
        out
    }
}

/// The binary a cross-build leaves in the checkout, which the stand-in
/// cargo builds none of: made for the scenario when it is not there, and
/// taken away with whatever folders it needed when the scenario ends.
struct CrossBuilt {
    made: Option<PathBuf>,
}

impl CrossBuilt {
    fn new() -> Self {
        let binary = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(TRIPLE)
            .join("remote-fast")
            .join("croft");
        if binary.exists() {
            return Self { made: None };
        }
        // The binary itself, or the highest folder above it that is missing.
        let mut made = binary.clone();
        while let Some(parent) = made.parent().filter(|p| !p.exists()) {
            made = parent.to_path_buf();
        }
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "built\n").unwrap();
        Self { made: Some(made) }
    }
}

impl Drop for CrossBuilt {
    fn drop(&mut self) {
        match &self.made {
            Some(dir) if dir.is_dir() => {
                let _ = std::fs::remove_dir_all(dir);
            }
            Some(file) => {
                let _ = std::fs::remove_file(file);
            }
            None => {}
        }
    }
}

/// The scenario runs in a fresh copy of this test binary in a session of its
/// own, so nothing it starts can reach the terminal this test runs in, with
/// an empty environment but `PATH` (the stand-ins first), a scratch `HOME`
/// and `TMPDIR`, a `SHELL` and the [`SEEDED`] variables. Run here, a
/// variable the machine already sets hides croft setting the same value,
/// and the connect dialog's terminal names the user's own shell. Its three
/// streams are files, so a stream a program shares with croft reads as
/// `croft's`, told apart from a pipe or nothing that croft gives it.
#[test]
fn every_ssh_call_is_made_as_recorded() {
    // A published crate's source leaves the saved record out, and the
    // scenario would put a built binary in that source's own folder.
    if Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("Cargo.toml.orig")
        .is_file()
    {
        return;
    }
    // Not a hidden folder: a relay pull refuses any source under one.
    let root = tempfile::Builder::new()
        .prefix("croft-ssh-calls-")
        .tempdir()
        .unwrap();
    let root = root.path().canonicalize().unwrap();
    for dir in ["bin", "home", "tmp"] {
        std::fs::create_dir(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("stdin"), "").unwrap();
    let record = root.join("ssh-calls.txt");
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "remote::recorded_calls::child_records_every_ssh_call",
            "--exact",
            "--test-threads=1",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("TMPDIR", root.join("tmp"))
        .env("SHELL", "/bin/sh")
        .envs(SEEDED.map(|name| (name, SEEDED_VALUE)))
        .env(RECORD_TO, &record)
        .stdin(std::fs::File::open(root.join("stdin")).unwrap())
        .stdout(std::fs::File::create(root.join("stdout")).unwrap())
        .stderr(std::fs::File::create(root.join("stderr")).unwrap());
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: `setsid` is async-signal-safe and touches no memory.
        unsafe {
            child.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let status = child.status().unwrap();
    let log = format!(
        "{}{}",
        std::fs::read_to_string(root.join("stdout")).unwrap_or_default(),
        std::fs::read_to_string(root.join("stderr")).unwrap_or_default()
    );
    assert!(status.success(), "the recording failed:\n{log}");
    assert!(
        log.contains("1 passed"),
        "the recording did not run:\n{log}"
    );
    let now = std::fs::read_to_string(&record).unwrap();
    crate::proc::record::check_saved(Path::new(SAVED), &now);
}

/// The scenario, run by [`every_ssh_call_is_made_as_recorded`].
#[test]
fn child_records_every_ssh_call() {
    let Some(record_to) = std::env::var_os(RECORD_TO).map(PathBuf::from) else {
        return;
    };
    let root = record_to.parent().unwrap().to_path_buf();
    let bin = root.join("bin");
    let home = root.join("home");
    let remote = root.join("remote");
    let tools = root.join("remote-tools");
    let flags = root.join("flags");
    let pull = root.join("pull");
    let upload = root.join("upload");
    let work = root.join("work");
    let prebuilt = root.join("prebuilt");
    let adopted_dir = root.join("adopted");
    for dir in [
        &remote,
        &tools,
        &flags,
        &pull,
        &upload,
        &work,
        &prebuilt,
        &adopted_dir,
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/config"), format!("Host {HOST}\n")).unwrap();
    for tool in REMOTE_TOOLS {
        std::os::unix::fs::symlink(system_program(tool), tools.join(tool)).unwrap();
    }
    let remote_croft = root.join("remote-croft");
    std::fs::write(&remote_croft, REMOTE_CROFT).unwrap();
    let config = crate::paths::config_dir();
    let keybindings_path = config.join("keybindings.json");
    std::fs::write(&keybindings_path, "[]\n").unwrap();
    // The release binary is never fetched: a published crate's build would
    // otherwise download it from GitHub.
    FORCE_SOURCE_BUILD.store(true, std::sync::atomic::Ordering::Relaxed);
    let _built = CrossBuilt::new();

    let fill = |answers: &str| {
        answers
            .replace("@FLAGS@", &flags.display().to_string())
            .replace("@REMOTE@", &remote.display().to_string())
            .replace("@TOOLS@", &tools.display().to_string())
            .replace("@CROFT@", &remote_croft.display().to_string())
    };
    let unseed = format!(
        "for v in {}; do eval \"[ \\\"\\${{$v-}}\\\" = {SEEDED_VALUE} ] && unset $v\"; done\n",
        SEEDED.join(" ")
    );
    let mut recording =
        crate::proc::record::record(&bin, "ssh", &format!("{unseed}{}", fill(SSH_ANSWERS)));
    recording.also(&bin, "rsync", &format!("{unseed}{}", fill(RSYNC_ANSWERS)));
    recording.also(&bin, "scp", &format!("{unseed}exit 0\n"));
    // Not written down: the cross-build's tools, which build nothing, and
    // the opener a relay `open` hands its link to.
    for (name, body) in [
        (
            "cargo",
            String::from(
                "case \"${1-}\" in --version) echo 'cargo 0.0.0 (stand-in)';; esac\nexit 0\n",
            ),
        ),
        (
            "cargo-zigbuild",
            fill("[ -e '@FLAGS@/no-cross' ] && exit 1\nexit 0\n"),
        ),
        ("zig", String::from("exit 0\n")),
        (
            "rustup",
            format!("[ \"$*\" = 'target list --installed' ] && echo {TRIPLE}\nexit 0\n"),
        ),
        ("xdg-open", String::from("exit 0\n")),
    ] {
        crate::proc::stand_in_script(&bin, name, &body);
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stamp = local_source_stamp().unwrap();
    let mut s = Script {
        recording,
        names: Vec::new(),
        words: vec![(stamp.clone(), String::from("<stamp>"))],
        out: String::from(
            "# Every ssh, rsync and scp call croft makes: src/remote/recorded_calls.rs. \
             Rewrite with CROFT_BLESS=1 after a change meant to move one.\n",
        ),
    };
    for (path, name) in [
        (&bin, "<bin>"),
        (&home, "<home>"),
        (&remote, "<remote>"),
        (&tools, "<remote-tools>"),
        (&pull, "<pull>"),
        (&upload, "<upload>"),
        (&work, "<work>"),
        (&prebuilt, "<prebuilt>"),
        (&adopted_dir, "<adopted>"),
        (&source, "<source>"),
        (&config, "<config>"),
    ] {
        s.name(path.clone(), name);
    }
    for stream in ["stdin", "stdout", "stderr"] {
        s.name(root.join(stream), "croft's");
    }
    let adopted = || AdoptedMaster {
        host: String::from(HOST),
        socket_dir: adopted_dir.clone(),
        socket_path: adopted_dir.join("ctl"),
    };
    let remote_bin = remote.join(".cargo/bin");
    let remote_stamp = remote.join(".cache/croft/install-stamp");

    // The control master and the commands sent through its socket.
    let ssh = SshControl::start(HOST).unwrap();
    s.name(ssh.socket_dir.clone(), "<master>");
    s.step("SshControl::start");
    let sock = ssh.socket_path.clone();
    let _ = ssh.command().arg(HOST).arg("echo command").status();
    s.step("SshControl::command");
    let _ = ssh
        .background_command()
        .arg(HOST)
        .arg("echo background")
        .status();
    s.step("SshControl::background_command");
    let _ = ssh.background_shell("echo shell").status();
    s.step("SshControl::background_shell");
    let read = ssh_socket_read(&sock, HOST, "echo read").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&read.stdout), "read\n");
    s.step("ssh_socket_read");
    let _ = ssh_socket_command(&sock, false)
        .arg(HOST)
        .arg("echo socket")
        .status();
    s.step("ssh_socket_command, not in the background");
    assert!(ssh_exec(HOST, &sock, "true"));
    s.step("ssh_exec");

    // What the remote has.
    assert!(!remote_croft_present(&ssh).unwrap());
    s.step("remote_croft_present, no croft there");
    assert!(remote_install_needed(&ssh, &stamp).unwrap());
    s.step("remote_install_needed");
    assert_eq!(remote_target_triple(&ssh).unwrap(), Some(TRIPLE));
    s.step("remote_target_triple");
    assert!(!remote_has_session_supervisor(&ssh));
    s.step("remote_has_session_supervisor, no croft there");
    mark_remote_updating(&ssh);
    s.step("mark_remote_updating");
    clear_remote_updating(&ssh);
    s.step("clear_remote_updating");
    let env = vec![
        (String::from("CROFT_RELAY_KEY"), relay_session_id("proj")),
        (
            String::from("CROFT_CLIENT_NONCE"),
            String::from("0123456789abcdef"),
        ),
    ];
    let ran = run_remote_croft(&ssh, Some("proj"), &env, false).unwrap();
    assert_eq!(ran.code(), Some(127), "no croft there");
    s.step("run_remote_croft, no croft there");
    let _ = run_remote_croft(&ssh, None, &env, true);
    s.step("run_remote_croft solo, no croft there");

    // The bulk lane.
    let lane = crate::remote_bulk::establish(HOST, &sock, |_| {});
    s.step("remote_bulk::establish, a connection of its own");
    drop(lane);
    s.step("remote_bulk::establish's connection closed");
    switch(&flags, "probe-fails", true);
    let lane = crate::remote_bulk::establish(HOST, &sock, |_| {});
    switch(&flags, "probe-fails", false);
    s.step("remote_bulk::establish, sharing the session's connection");
    drop(lane);
    s.step("remote_bulk::establish's shared lane dropped");

    // Placing the release binary, with each install's runner.
    let binary = prebuilt.join("croft");
    std::fs::write(&binary, "release\n").unwrap();
    let placed = place_prebuilt(
        &ssh,
        &binary,
        "prebuilt-stamp",
        &mut |_| {
            std::fs::copy(&remote_croft, remote_bin.join("croft.new"))?;
            Ok(())
        },
        &mut |mut step| Ok(step.status()?),
    );
    assert!(placed.is_ok(), "{placed:?}");
    s.step("place_prebuilt on the terminal, as `croft remote` runs it (its copy is the test's)");
    let (log_tx, _log) = std::sync::mpsc::channel();
    let shared = crate::remote_bulk::BulkLane::new(crate::remote_bulk::LaneMode::SharedMux, 0);
    let placed = place_prebuilt(
        &ssh,
        &binary,
        "prebuilt-stamp",
        &mut |b| ship_prebuilt_logged(&shared, &ssh, b, &log_tx),
        &mut |step| run_command_streaming(step, &log_tx),
    );
    assert!(placed.is_ok(), "{placed:?}");
    assert!(remote_bin.join("croft").is_file(), "croft is on the remote");
    s.step("place_prebuilt into the install log, as an install inside croft runs it");

    // The install inside croft.
    let streaming = |confirm: &mut dyn FnMut(&str) -> bool| {
        let (log_tx, _log) = std::sync::mpsc::channel();
        let (launch_tx, _launch) = std::sync::mpsc::channel();
        install_only_streaming(adopted(), log_tx, launch_tx, confirm)
    };
    let installed = streaming(&mut |_| false);
    assert!(installed.is_ok(), "{:?}", installed.err());
    s.step("install_only_streaming: the cross-build ships");
    let installed = streaming(&mut |_| false);
    assert!(installed.is_ok(), "{:?}", installed.err());
    s.step("install_only_streaming: already up to date");
    std::fs::remove_file(&remote_stamp).unwrap();
    switch(&flags, "no-cross", true);
    assert!(streaming(&mut |_| true).is_err(), "the update is declined");
    s.step("install_only_streaming beside a running croft: no cross-build, so no update");
    std::fs::remove_file(remote_bin.join("croft")).unwrap();
    let mut asked = false;
    let installed = streaming(&mut |_| {
        asked = true;
        true
    });
    assert!(installed.is_ok(), "{:?}", installed.err());
    assert!(asked, "the remote compile is asked about");
    s.step("install_only_streaming: no cross-build, so the remote compiles");

    // The install `croft remote` runs on the terminal.
    switch(&flags, "no-cross", false);
    let installed = install_remote_croft(&ssh, &stamp);
    assert!(installed.is_ok(), "{installed:?}");
    s.step("install_remote_croft: the cross-build ships");
    switch(&flags, "no-cross", true);
    let installed = answering_on_a_terminal("y\n", || install_remote_croft(&ssh, &stamp));
    assert!(installed.is_ok(), "{installed:?}");
    s.step(
        "install_remote_croft on a terminal: no cross-build, so the remote compiles, as answered",
    );
    switch(&flags, "no-cross", false);
    switch(&flags, "rsync-fails", true);
    let synced = sync_local_source_to_remote(&ssh);
    switch(&flags, "rsync-fails", false);
    assert!(synced.is_ok(), "{synced:?}");
    assert!(
        remote.join(".cache/croft/source/Cargo.toml").is_file(),
        "the source arrived"
    );
    s.step("sync_local_source_to_remote: rsync fails, so the tar pipe");

    // Config sync.
    push_config_files(&ssh, &SyncResolution::default(), &mut |_| {});
    let remote_keybindings = remote.join(".config/croft/keybindings.json");
    assert_eq!(
        std::fs::read_to_string(&remote_keybindings).unwrap(),
        "[]\n"
    );
    s.step("push_config_files");
    let hashes = read_remote_hashes(ssh_socket_read(
        &sock,
        HOST,
        &crate::config_sync::remote_hash_script(),
    ));
    assert!(
        hashes.is_some_and(|h| h.contains_key("keybindings.json")),
        "the remote's copies read"
    );
    s.step("read_remote_hashes");
    let copy = read_remote_config(&sock, HOST, "keybindings.json").unwrap();
    assert_eq!(copy, b"[]\n");
    s.step("read_remote_config");
    std::fs::write(&keybindings_path, "[1]\n").unwrap();
    print_config_diff(&ssh, "keybindings.json").unwrap();
    s.step("print_config_diff");
    let keybindings = *crate::config_sync::SYNCABLE
        .iter()
        .find(|s| s.name == "keybindings.json")
        .unwrap();
    let pushed = push_over_bulk_lane(
        HOST,
        &sock,
        &[(keybindings, keybindings_path.clone())],
        &crate::config_sync::SyncState::path(),
    );
    assert_eq!(pushed, PushResult::default());
    assert_eq!(
        std::fs::read_to_string(&remote_keybindings).unwrap(),
        "[1]\n"
    );
    s.step("push_over_bulk_lane");
    sync_config_now(HOST, &SyncResolution::default(), Some("keybindings.json")).unwrap();
    s.step("sync_config_now --diff");
    std::fs::write(&keybindings_path, "[2]\n").unwrap();
    sync_config_now(HOST, &SyncResolution::default(), None).unwrap();
    assert_eq!(
        std::fs::read_to_string(&remote_keybindings).unwrap(),
        "[2]\n"
    );
    s.step("sync_config_now");

    // The drop relay.
    let before = runs_started(&bin);
    let pump = DropPump::start(&ssh, &relay_session_id("")).unwrap();
    wait_for_runs(&bin, before + 2);
    drop(pump);
    s.step("DropPump::start, then stopped");
    let inbox = remote.join(format!(".cache/croft/relay-{}/inbox", relay_session_id("")));
    assert!(inbox.is_dir(), "the relay set up its inbox");
    let inbox_dir = inbox.display().to_string();
    assert!(ssh_exec(
        HOST,
        &sock,
        &claim_command(&inbox_dir, "pull-1-1")
    ));
    s.step("a relay request claimed");
    write_relay_err(HOST, &sock, &inbox_dir, "pull-1-0", "it went wrong");
    s.step("write_relay_err");
    std::fs::write(pull.join("notes.txt"), "notes\n").unwrap();
    let notes = pull.join("notes.txt");
    assert!(
        pull_source_allowed(&notes),
        "{} is not hidden",
        notes.display()
    );
    handle_pull_request(
        HOST,
        &sock,
        &inbox_dir,
        "pull-1-1",
        &notes.display().to_string(),
    );
    assert!(
        inbox.join("pull-1-1/notes.txt").is_file(),
        "the file arrived"
    );
    assert!(inbox.join("pull-1-1/.ok").is_file());
    s.step("handle_pull_request");
    std::fs::write(pull.join(".secret"), "secret\n").unwrap();
    let secret = pull.join(".secret").display().to_string();
    handle_pull_request(HOST, &sock, &inbox_dir, "pull-1-2", &secret);
    s.step("handle_pull_request of a hidden file");
    let gone = pull.join("gone.txt").display().to_string();
    handle_pull_request(HOST, &sock, &inbox_dir, "pull-1-3", &gone);
    s.step("handle_pull_request of a missing file");
    assert!(crate::clipboard::write_string("copied here"));
    handle_clipboard_request(HOST, &sock, &inbox_dir, "clipboard-1-1");
    assert_eq!(
        std::fs::read_to_string(inbox.join("clipboard-1-1/clipboard.txt")).unwrap(),
        "copied here"
    );
    s.step("handle_clipboard_request");
    std::fs::write(inbox.join("copy-1-1.txt"), "copied there").unwrap();
    handle_copy_request(HOST, &sock, &inbox_dir, "copy-1-1");
    assert_eq!(
        crate::clipboard::read_string().as_deref(),
        Some("copied there")
    );
    s.step("handle_copy_request");
    handle_open_request(HOST, &sock, &inbox_dir, "open-1-1", "https://example.com/");
    assert!(inbox.join("open-1-1/.ok").is_file());
    s.step("handle_open_request");
    let port = free_port();
    s.word(format!("127.0.0.1:{port}"), "127.0.0.1:<port>");
    s.word(format!("%s {port} "), "%s <port> ");
    handle_forward_request(
        HOST,
        &sock,
        &inbox_dir,
        "forward-1-1",
        &port.to_string(),
        true,
    );
    assert!(inbox.join("forward-1-1/.ok").is_file());
    s.step("handle_forward_request");
    handle_unforward_request(HOST, &sock, &port.to_string(), &port.to_string());
    s.step("handle_unforward_request");

    // The connect dialog's connection, on a terminal of its own.
    let mut auth = crate::remote_connect::SshAuth::start(HOST).unwrap();
    authenticated(&mut auth);
    auth.cancel();
    drop(auth);
    s.step("SshAuth::start, then cancel");
    let mut auth = crate::remote_connect::SshAuth::start(HOST).unwrap();
    authenticated(&mut auth);
    drop(auth);
    s.step("SshAuth::start, then dropped");
    let mut auth = crate::remote_connect::SshAuth::start(HOST).unwrap();
    authenticated(&mut auth);
    close_master(auth.hand_off());
    s.step("SshAuth::start, handed off, then close_master");

    // A fleet run.
    let results =
        crate::fleet::run_on_hosts(&[String::from(HOST)], "echo fleet", Duration::from_secs(30));
    assert_eq!(results[0].output, "fleet");
    s.step("fleet::run_on_hosts");

    // A croft:// link for this host.
    {
        use clap::Parser as _;
        let link = format!(
            "croft://decide?host={HOST}&path=~/proj&token={}&decision=allow",
            "0123456789abcdef".repeat(2)
        );
        crate::cli::Cli::parse_from(["croft", "open-link", link.as_str()])
            .run()
            .unwrap();
    }
    s.step("croft open-link, a decision for a croft on another machine");

    // Whole sessions: the in-app launch once its install is done, and
    // `croft remote` to a remote without croft.
    assert!(remote_bin.join("croft").is_file(), "croft is on the remote");
    let outcome = launch_only(adopted(), Some("proj")).unwrap();
    assert_eq!(outcome, RemoteOutcome::Exited);
    s.step_in_any_order("launch_only");
    std::fs::create_dir_all(&adopted_dir).unwrap();
    std::fs::remove_file(remote_bin.join("croft")).unwrap();
    let outcome = launch_croft_with(HOST, Some("proj"), None, false).unwrap();
    assert_eq!(outcome, RemoteOutcome::Exited);
    s.step_in_any_order("launch_croft_with, no croft there, so it is installed first");

    // A drop on the Remote Explorer, copied by scp.
    std::fs::write(upload.join("dropped.txt"), "dropped\n").unwrap();
    crate::app::run_scp_uploads_for_record(
        work.clone(),
        HOST,
        &[upload.join("dropped.txt"), upload.join("gone.txt")],
    )
    .unwrap();
    s.step("the scp uploads of a drop on the Remote Explorer");

    // Closing the connections.
    close_master(adopted());
    s.step("close_master");
    drop(ssh);
    s.step("SshControl dropped");

    std::fs::write(record_to, &s.out).unwrap();
}
