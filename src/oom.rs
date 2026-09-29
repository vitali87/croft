//! Steering the Linux OOM killer away from croft (#694).
//!
//! When memory runs out the kernel kills the process with the highest
//! `oom_score`, which is mostly resident size. croft owns the PTYs of every
//! terminal pane, so a kill of croft takes the user's shells and their jobs
//! with it. The heavy helpers croft starts (rust-analyzer alone reached
//! 2.4 GB in the report) are cheap to lose by comparison: a killed language
//! server takes its editor features until croft is relaunched or the
//! workspace reopened, not a shell. Raising their `oom_score_adj` makes the kernel
//! reclaim them first. Raising it needs no privilege; lowering croft's own
//! would.

/// `oom_score_adj` for language servers: well above croft and its panes
/// (which keep the inherited value, normally 0), below the 1000 the remote
/// source build takes.
pub const HELPER_SCORE: i32 = 500;

/// Best-effort: ask the kernel to kill `pid` before croft when memory runs
/// out. A no-op off Linux, and when the process is already gone.
pub fn prefer_to_reclaim(pid: u32, score: i32) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::fs::write(format!("/proc/{pid}/oom_score_adj"), score.to_string());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, score);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn raises_a_childs_oom_score_adj() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        prefer_to_reclaim(child.id(), HELPER_SCORE);
        let adj = std::fs::read_to_string(format!("/proc/{}/oom_score_adj", child.id()));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(adj.unwrap().trim(), HELPER_SCORE.to_string());
    }

    #[test]
    fn a_gone_process_is_ignored() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        prefer_to_reclaim(pid, HELPER_SCORE);
    }
}
