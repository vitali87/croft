//! Bulk-transfer lane for background remote installs.
//!
//! A remote-launched croft session is interactive traffic on a single
//! multiplexed SSH TCP connection. Any bulk transfer (binary ship, source
//! sync) pushed through that same connection serializes ahead of the
//! user's keystrokes — SSH channels share one in-order TCP stream, so a
//! multi-megabyte upload adds seconds of head-of-line latency to every
//! keypress. This module routes bulk work onto its own dedicated SSH
//! connection when the host allows non-interactive auth, and throttles
//! it to half the measured link rate so the uplink queue stays drained
//! either way.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Bytes pushed through the live mux to estimate link throughput before
/// the real transfer. Small enough to finish in well under a second on a
/// healthy link, large enough to amortize channel-open overhead.
pub const THROUGHPUT_PROBE_BYTES: u64 = 768 * 1024;

/// Throttle floor in KB/s. Applied when the probe fails or the measured
/// link is slower than twice this value; keeps a 20 MB binary ship under
/// ~90 s even on a degraded link while bounding per-keystroke queueing.
pub const BWLIMIT_FLOOR_KBPS: u32 = 256;

/// How the bulk bytes reach the remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneMode {
    /// A second SSH connection dedicated to bulk transfers; the
    /// interactive master never carries a bulk byte.
    Dedicated { socket_path: PathBuf },
    /// Non-interactive auth unavailable: bulk shares the interactive
    /// master and relies on the throttle alone.
    SharedMux,
}

/// A routing + pacing decision for all bulk transfers of one install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkLane {
    mode: LaneMode,
    bwlimit_kbps: u32,
}

impl BulkLane {
    pub fn new(mode: LaneMode, bwlimit_kbps: u32) -> Self {
        Self { mode, bwlimit_kbps }
    }

    fn socket<'a>(&'a self, interactive_socket: &'a Path) -> &'a Path {
        match &self.mode {
            LaneMode::Dedicated { socket_path } => socket_path.as_path(),
            LaneMode::SharedMux => interactive_socket,
        }
    }

    /// The `-e` remote-shell string rsync should use: the bulk socket in
    /// dedicated mode, the interactive socket otherwise.
    pub fn rsync_ssh_arg(&self, interactive_socket: &Path) -> String {
        format!(
            "ssh -S {} -o ControlMaster=no",
            crate::remote::shell_quote_for_e_arg(self.socket(interactive_socket)),
        )
    }

    /// Extra rsync args pacing the transfer. Always present: even a
    /// dedicated connection shares the physical uplink with the live
    /// session, and a saturated uplink queue delays every packet.
    pub fn rsync_throttle_args(&self) -> Vec<String> {
        vec![format!("--bwlimit={}", self.bwlimit_kbps)]
    }

    /// An `ssh` Command routed over the bulk socket in dedicated mode,
    /// or over the interactive socket otherwise. Used for long-running
    /// remote commands tied to the install (remote cargo build, tar
    /// pipe) so they never occupy the interactive master.
    /// An ssh invocation on the bulk lane. Everything the lane carries runs in
    /// the background while the attached remote session owns the terminal, so
    /// `-n` is not optional: without it ssh forwards the caller's stdin to the
    /// remote, and the caller's stdin is the tty the session is reading. The
    /// longest command on this path is the fallback `cargo install`, which
    /// would hold the user's keystrokes for the whole multi-minute build.
    pub fn ssh_command(&self, interactive_socket: &Path) -> Command {
        let mut command = Command::new("ssh");
        command
            .arg("-S")
            .arg(self.socket(interactive_socket))
            .arg("-o")
            .arg("ControlMaster=no")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ServerAliveInterval=10")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("-n")
            .stdin(Stdio::null());
        command
    }

    pub fn bwlimit_kbps(&self) -> u32 {
        self.bwlimit_kbps
    }
}

/// Args for the auth probe: succeeds only when the host authenticates
/// without prompting (key/agent), over a fresh connection that must not
/// touch any existing control socket.
pub fn probe_args(host: &str) -> Vec<String> {
    [
        "-o",
        "BatchMode=yes",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-o",
        "ConnectTimeout=5",
        host,
        "true",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Args opening the dedicated bulk master. BatchMode so a password host
/// fails fast instead of hanging on a prompt nobody can answer, IPQoS
/// throughput so routers with working AQM deprioritize the bulk flow
/// under the interactive session's lowdelay traffic.
pub fn bulk_master_args(socket_path: &Path, host: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        String::from("-M"),
        String::from("-S"),
        socket_path.to_string_lossy().into_owned(),
        String::from("-f"),
        String::from("-N"),
        String::from("-T"),
    ];
    args.extend(
        [
            "-o",
            "BatchMode=yes",
            "-o",
            "IPQoS=throughput",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
        ]
        .into_iter()
        .map(String::from),
    );
    args.push(host.to_string());
    args
}

/// Half the measured rate, clamped to the floor. Halving leaves steady
/// headroom for the interactive flow and keeps the sender from filling
/// the bottleneck queue; the floor catches failed or absurd probes.
pub fn bwlimit_kbps_from_probe(bytes: u64, elapsed: Duration) -> u32 {
    let secs = elapsed.as_secs_f64();
    if bytes == 0 || secs <= 0.0 {
        return BWLIMIT_FLOOR_KBPS;
    }
    let measured_kbps = (bytes as f64 / 1024.0) / secs;
    let half = (measured_kbps / 2.0) as u32;
    half.max(BWLIMIT_FLOOR_KBPS)
}

/// Owns the dedicated bulk master for the lifetime of one install and
/// tears it down (`ssh -O exit`) when dropped.
struct MasterGuard {
    host: String,
    socket_dir: PathBuf,
    socket_path: PathBuf,
}

impl Drop for MasterGuard {
    fn drop(&mut self) {
        let _ = Command::new("ssh")
            .arg("-S")
            .arg(&self.socket_path)
            .arg("-O")
            .arg("exit")
            .arg(&self.host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

/// A lane plus ownership of its dedicated master, if any. Keep this
/// alive for the whole install; dropping it closes the bulk connection.
pub struct EstablishedLane {
    pub lane: BulkLane,
    _master: Option<MasterGuard>,
}

/// Decide and set up the bulk route for one install: try a dedicated
/// BatchMode connection, fall back to the shared mux, then measure the
/// link through whichever socket bulk will actually use and derive the
/// throttle from it.
pub fn establish(host: &str, interactive_socket: &Path, log: impl Fn(String)) -> EstablishedLane {
    let (mode, master) = match open_dedicated_master(host) {
        Some(guard) => {
            log(String::from(
                "Bulk lane: dedicated SSH connection (non-interactive auth available); the live session's connection stays clear",
            ));
            (
                LaneMode::Dedicated {
                    socket_path: guard.socket_path.clone(),
                },
                Some(guard),
            )
        }
        None => {
            log(String::from(
                "Bulk lane: host needs interactive auth, sharing the session connection with a throttle",
            ));
            (LaneMode::SharedMux, None)
        }
    };
    let probe_lane = BulkLane::new(mode.clone(), 0);
    let bwlimit = match measure_throughput(&probe_lane, interactive_socket, host) {
        Some((bytes, elapsed)) => {
            let limit = bwlimit_kbps_from_probe(bytes, elapsed);
            log(format!(
                "Link probe: {} KB in {} ms; capping bulk transfers at {limit} KB/s",
                bytes / 1024,
                elapsed.as_millis(),
            ));
            limit
        }
        None => {
            log(format!(
                "Link probe failed; capping bulk transfers at the {BWLIMIT_FLOOR_KBPS} KB/s floor",
            ));
            BWLIMIT_FLOOR_KBPS
        }
    };
    EstablishedLane {
        lane: BulkLane::new(mode, bwlimit),
        _master: master,
    }
}

fn open_dedicated_master(host: &str) -> Option<MasterGuard> {
    let probe_ok = Command::new("ssh")
        .args(probe_args(host))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !probe_ok {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let socket_dir =
        std::env::temp_dir().join(format!("croft-ssh-bulk-{}-{now}", std::process::id()));
    std::fs::create_dir_all(&socket_dir).ok()?;
    let socket_path = socket_dir.join("ctl");
    let started = Command::new("ssh")
        .args(bulk_master_args(&socket_path, host))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !started {
        let _ = std::fs::remove_dir_all(&socket_dir);
        return None;
    }
    Some(MasterGuard {
        host: host.to_string(),
        socket_dir,
        socket_path,
    })
}

/// Time a fixed-size upload through the lane's socket. Includes the
/// channel-open round trip, which under-reports the rate slightly — the
/// safe direction for a throttle derived from it.
fn measure_throughput(
    lane: &BulkLane,
    interactive_socket: &Path,
    host: &str,
) -> Option<(u64, Duration)> {
    let mut cmd = lane.ssh_command(interactive_socket);
    cmd.arg(host)
        .arg("cat >/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let started = Instant::now();
    let mut child = cmd.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let chunk = vec![0u8; 64 * 1024];
    let mut sent: u64 = 0;
    while sent < THROUGHPUT_PROBE_BYTES {
        if stdin.write_all(&chunk).is_err() {
            let _ = child.wait();
            return None;
        }
        sent += chunk.len() as u64;
    }
    drop(stdin);
    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }
    Some((sent, started.elapsed()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_requires_batchmode_and_bypasses_every_control_socket() {
        let args = probe_args("myhost");
        assert!(args.contains(&String::from("BatchMode=yes")));
        assert!(args.contains(&String::from("ControlMaster=no")));
        assert!(
            args.contains(&String::from("ControlPath=none")),
            "the probe must prove a FRESH connection can auth; reusing the interactive master would always succeed and lie"
        );
        assert_eq!(args.last(), Some(&String::from("true")));
        assert!(args.contains(&String::from("myhost")));
    }

    #[test]
    fn bulk_master_opens_a_second_batchmode_master_marked_as_bulk_traffic() {
        let args = bulk_master_args(Path::new("/tmp/bulk/ctl"), "myhost");
        assert!(args.contains(&String::from("-M")));
        assert!(args.contains(&String::from("/tmp/bulk/ctl")));
        assert!(args.contains(&String::from("-N")));
        assert!(
            args.contains(&String::from("BatchMode=yes")),
            "a password prompt on the bulk master would hang the install thread forever"
        );
        assert!(
            args.contains(&String::from("IPQoS=throughput")),
            "bulk flow must be DSCP-marked below the interactive session"
        );
        assert!(args.contains(&String::from("myhost")));
    }

    #[test]
    fn dedicated_lane_routes_rsync_over_the_bulk_socket_only() {
        let lane = BulkLane::new(
            LaneMode::Dedicated {
                socket_path: PathBuf::from("/tmp/bulk/ctl"),
            },
            512,
        );
        let e = lane.rsync_ssh_arg(Path::new("/tmp/interactive/ctl"));
        assert!(e.contains("/tmp/bulk/ctl"));
        assert!(
            !e.contains("/tmp/interactive/ctl"),
            "a single bulk byte on the interactive master defeats the lane"
        );
        assert!(e.contains("ControlMaster=no"));
    }

    #[test]
    fn shared_lane_keeps_the_interactive_socket() {
        let lane = BulkLane::new(LaneMode::SharedMux, 512);
        let e = lane.rsync_ssh_arg(Path::new("/tmp/interactive/ctl"));
        assert!(e.contains("/tmp/interactive/ctl"));
    }

    #[test]
    fn every_lane_mode_throttles_to_its_bwlimit() {
        let dedicated = BulkLane::new(
            LaneMode::Dedicated {
                socket_path: PathBuf::from("/tmp/bulk/ctl"),
            },
            900,
        );
        assert_eq!(
            dedicated.rsync_throttle_args(),
            vec![String::from("--bwlimit=900")],
            "a dedicated TCP flow still shares the physical uplink; unthrottled it bufferbloats the link for the interactive session too"
        );
        let shared = BulkLane::new(LaneMode::SharedMux, 300);
        assert_eq!(
            shared.rsync_throttle_args(),
            vec![String::from("--bwlimit=300")]
        );
    }

    // The lane's longest command is the fallback `cargo install`, which runs
    // for minutes while the user is already attached. ssh forwards the caller's
    // stdin to the remote unless `-n` is given, and here that stdin is the tty
    // the attached session is reading: two processes read() it and each
    // keystroke reaches exactly one of them, so typing vanishes for the whole
    // build. Verified against a real host — `ssh h 'sleep 1' < probe` drains
    // every byte, `ssh -n h 'sleep 1' < probe` leaves them all.
    #[test]
    fn every_lane_ssh_command_gives_up_the_callers_stdin() {
        for mode in [
            LaneMode::Dedicated {
                socket_path: PathBuf::from("/tmp/bulk/ctl"),
            },
            LaneMode::SharedMux,
        ] {
            let lane = BulkLane::new(mode, 512);
            let cmd = lane.ssh_command(Path::new("/tmp/interactive/ctl"));
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(
                args.contains(&String::from("-n")),
                "a bulk command sharing the session's tty steals its keystrokes"
            );
        }
    }

    #[test]
    fn dedicated_ssh_command_targets_the_bulk_socket() {
        let lane = BulkLane::new(
            LaneMode::Dedicated {
                socket_path: PathBuf::from("/tmp/bulk/ctl"),
            },
            512,
        );
        let cmd = lane.ssh_command(Path::new("/tmp/interactive/ctl"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&String::from("/tmp/bulk/ctl")));
        assert!(!args.contains(&String::from("/tmp/interactive/ctl")));
        assert!(args.contains(&String::from("ControlMaster=no")));
    }

    #[test]
    fn shared_ssh_command_falls_back_to_the_interactive_socket() {
        let lane = BulkLane::new(LaneMode::SharedMux, 512);
        let cmd = lane.ssh_command(Path::new("/tmp/interactive/ctl"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&String::from("/tmp/interactive/ctl")));
    }

    #[test]
    fn bwlimit_is_half_the_probed_rate() {
        // 1 MiB in one second = 1024 KB/s measured -> 512 KB/s limit.
        assert_eq!(
            bwlimit_kbps_from_probe(1024 * 1024, Duration::from_secs(1)),
            512
        );
        // 8 MiB in two seconds = 4096 KB/s measured -> 2048 KB/s limit.
        assert_eq!(
            bwlimit_kbps_from_probe(8 * 1024 * 1024, Duration::from_secs(2)),
            2048
        );
    }

    #[test]
    fn bwlimit_clamps_to_the_floor_on_slow_links_and_degenerate_probes() {
        // 100 KB/s link: half would be 50, below the floor.
        assert_eq!(
            bwlimit_kbps_from_probe(100 * 1024, Duration::from_secs(1)),
            BWLIMIT_FLOOR_KBPS
        );
        // Zero-duration probe must not divide by zero or return absurdity.
        assert_eq!(
            bwlimit_kbps_from_probe(1024, Duration::from_secs(0)),
            BWLIMIT_FLOOR_KBPS
        );
        // Zero bytes (probe failed outright).
        assert_eq!(
            bwlimit_kbps_from_probe(0, Duration::from_secs(1)),
            BWLIMIT_FLOOR_KBPS
        );
    }
}
