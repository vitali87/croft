//! How croft starts ssh (#2062, card 12 step 8).
//!
//! Every ssh croft starts builds through [`Ssh`] and is named by what it is
//! for, so each kind's options and streams are written once:
//!
//! - [`Ssh::client`]: a command through the shared connection, with a
//!   connect timeout and keepalive, as the session's own commands run;
//!   [`Ssh::bare_client`] is one with neither, as the relay's helpers have
//!   always run;
//! - [`Ssh::background`], [`Ssh::no_stdin`] and [`Ssh::quiet`]: what a
//!   client may touch of croft's streams, for one that runs while the remote
//!   session owns the terminal;
//! - [`Ssh::control`]: a request to the connection's master (`-O exit`,
//!   `-O forward`, `-O cancel`);
//! - the ways a connection opens, kept apart on purpose since each waits,
//!   prompts and keeps alive differently: [`Ssh::session_master`] (the
//!   terminal's own), [`Ssh::bulk_master`] (an install's own, never
//!   prompting), [`Ssh::fresh_check`] (can the host be reached without a
//!   prompt) and [`DIALOG_MASTER_OPTIONS`] (the connect dialog's, on a
//!   pseudo-terminal the user answers);
//! - [`Ssh::direct`]: a connection of its own, as the user's `ssh host`;
//! - [`rsync_transport`], the `-e` command line rsync runs ssh with.
//!
//! The arguments come out in the order they always had, so the saved record
//! of every ssh, rsync and scp call (`src/remote/recorded_calls.rs`) is the
//! same before and after the move.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

/// One ssh call, built up before it runs.
pub(crate) struct Ssh(Command);

/// What a client through the connection sets beyond using it: each connect
/// attempt is bounded, so a reconnect against a host that is asleep or
/// offline fails fast instead of hanging, and the link is checked every
/// 10 s, so a dead one is noticed after three misses.
const CLIENT_OPTIONS: [&str; 6] = [
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=10",
    "-o",
    "ServerAliveCountMax=3",
];

/// An install's own master: `BatchMode` so a password host fails fast
/// instead of hanging on a prompt nobody can answer, `IPQoS` throughput so
/// routers with working AQM deprioritize the bulk flow under the
/// interactive session's lowdelay traffic, a 10 s connect timeout, and a
/// keepalive every 15 s.
const BULK_MASTER_OPTIONS: [&str; 8] = [
    "-o",
    "BatchMode=yes",
    "-o",
    "IPQoS=throughput",
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=15",
];

/// The connect dialog's master, on a pseudo-terminal where the user answers
/// a password or passphrase prompt: it may prompt (`BatchMode=no`), reaching
/// the host and its greeting is bounded to 10 s (not the login: the user
/// may take longer to type), as `croft remote`'s is (#2796), and it keeps
/// alive every 15 s.
pub(crate) const DIALOG_MASTER_OPTIONS: [&str; 6] = [
    "-o",
    "BatchMode=no",
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=15",
];

/// The check for an install's own connection: succeeds only when the host
/// authenticates without prompting (key/agent), over a fresh connection
/// that must not touch any existing control socket, within 5 s.
const FRESH_CHECK_OPTIONS: [&str; 8] = [
    "-o",
    "BatchMode=yes",
    "-o",
    "ControlMaster=no",
    "-o",
    "ControlPath=none",
    "-o",
    "ConnectTimeout=5",
];

impl Ssh {
    /// `ssh`, with nothing asked of it yet. Named through
    /// [`crate::proc::command`], so a test can put a stand-in in its place.
    fn new() -> Self {
        Self(crate::proc::command("ssh"))
    }

    /// Through the shared connection at `socket`, never opening a master of
    /// its own: `-S socket -o ControlMaster=no`, and nothing more.
    pub(crate) fn bare_client(socket: &Path) -> Self {
        let mut ssh = Self::new();
        ssh.0
            .arg("-S")
            .arg(socket)
            .arg("-o")
            .arg("ControlMaster=no");
        ssh
    }

    /// Through the shared connection at `socket`, with the connect timeout
    /// and keepalive ([`CLIENT_OPTIONS`]) the session's commands run with.
    pub(crate) fn client(socket: &Path) -> Self {
        let mut ssh = Self::bare_client(socket);
        ssh.0.args(CLIENT_OPTIONS);
        ssh
    }

    /// Read nothing: `-n`, and stdin nowhere. While the remote session owns
    /// the terminal, ssh would otherwise forward croft's stdin, the tty the
    /// session reads, to the remote, and every keystroke would reach only one
    /// of the two.
    pub(crate) fn no_stdin(mut self) -> Self {
        self.0.arg("-n").stdin(Stdio::null());
        self
    }

    /// Every stream nowhere: what ssh says does not land on croft's screen.
    pub(crate) fn quiet(mut self) -> Self {
        self.0
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self
    }

    /// For a command run while the remote session owns the terminal:
    /// [`Ssh::no_stdin`] and [`Ssh::quiet`]. The interactive session is the
    /// one client that keeps the terminal.
    pub(crate) fn background(self) -> Self {
        self.no_stdin().quiet()
    }

    /// A request to the master behind `socket`: `-S socket -O op`.
    pub(crate) fn control(socket: &Path, op: &str) -> Self {
        let mut ssh = Self::new();
        ssh.0.arg("-S").arg(socket).arg("-O").arg(op);
        ssh
    }

    /// A master at `socket` that goes to the background once it is up
    /// (`-f`), with no command and no terminal of its own (`-N -T`).
    fn master(socket: &Path) -> Self {
        let mut ssh = Self::new();
        ssh.0
            .arg("-M")
            .arg("-S")
            .arg(socket)
            .args(["-f", "-N", "-T"]);
        ssh
    }

    /// The terminal session's own master at `socket`, with the client's
    /// connect timeout and keepalive ([`CLIENT_OPTIONS`]), so it tears
    /// itself down promptly once the link goes away.
    pub(crate) fn session_master(socket: &Path) -> Self {
        let mut ssh = Self::master(socket);
        ssh.0.args(CLIENT_OPTIONS);
        ssh
    }

    /// An install's own master at `socket` ([`BULK_MASTER_OPTIONS`]).
    pub(crate) fn bulk_master(socket: &Path) -> Self {
        let mut ssh = Self::master(socket);
        ssh.0.args(BULK_MASTER_OPTIONS);
        ssh
    }

    /// Whether `host` lets croft in without a prompt
    /// ([`FRESH_CHECK_OPTIONS`]): it runs `true` there.
    pub(crate) fn fresh_check(host: &str) -> Self {
        let mut ssh = Self::new();
        ssh.0.args(FRESH_CHECK_OPTIONS).arg(host).arg("true");
        ssh
    }

    /// A connection of its own to `host`, as the user's own `ssh host`
    /// makes, with their configuration and on croft's terminal: `ssh --
    /// host`, the `--` so a host can never be read as an option.
    pub(crate) fn direct(host: &str) -> Self {
        let mut ssh = Self::new();
        ssh.0.arg("--").arg(host);
        ssh
    }

    /// One more argument: the host, a remote command, a `-L` spec.
    pub(crate) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.0.arg(arg);
        self
    }

    /// The command, for the call to connect its streams and run as it needs.
    pub(crate) fn into_command(self) -> Command {
        self.0
    }
}

/// The `-e` command line rsync runs ssh with: a bare client through the
/// connection at `socket` ([`Ssh::bare_client`]), the socket quoted for the
/// shell rsync hands the line to.
pub(crate) fn rsync_transport(socket: &Path) -> String {
    format!(
        "ssh -S {} -o ControlMaster=no",
        super::shell_quote_for_e_arg(socket),
    )
}

/// The arguments of a connect-dialog master at `socket` for `host`, for the
/// pseudo-terminal it runs on, which takes no [`Command`].
pub(crate) fn dialog_master_args(socket: &Path, host: &str) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["-M".into(), "-S".into(), socket.into()];
    args.extend(["-N", "-T"].map(Into::into));
    args.extend(DIALOG_MASTER_OPTIONS.map(Into::into));
    args.push(host.into());
    args
}
