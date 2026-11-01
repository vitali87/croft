//! Where a launch's time goes, step by step, written after every launch to
//! `~/.cache/croft/startup-trace.txt` (`CROFT_STARTUP_TRACE=<file>` puts it
//! elsewhere).
//!
//! croft's first tenet is a first frame in milliseconds, and a launch that
//! misses it on someone's machine has to say why there, not on a developer's.
//! Each `mark` notes how long after `main` began a startup step finished and
//! how much of that this thread spent computing rather than waiting; `finish`
//! writes the list once the first frame is on screen, headed by when each
//! process that started croft began. That head is what a launch from
//! Croft.app needs: there the terminal is a new process too, and its own
//! start counts toward the wait as much as croft's does.
//!
//! Always on, because a GUI launch carries no environment of ours to switch
//! it on with: a mark is a clock read and a push, a few dozen per launch,
//! and the file is written after the frame is flushed.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static ON: AtomicBool = AtomicBool::new(false);
static TRACE: Mutex<Option<Trace>> = Mutex::new(None);

struct Trace {
    path: std::path::PathBuf,
    /// When `main` began, and this thread's CPU time then.
    start: Instant,
    start_cpu: Option<Duration>,
    marks: Vec<Mark>,
}

struct Mark {
    label: &'static str,
    at: Instant,
    cpu: Option<Duration>,
}

/// A process that led to croft: this one or one of its ancestors.
#[derive(Debug, Clone, PartialEq)]
struct Lineage {
    pid: u32,
    name: String,
    /// How long before `main` began the process was started.
    age: Duration,
}

/// Start the clock. Call first in `main`.
pub fn init() {
    let path = match std::env::var_os("CROFT_STARTUP_TRACE").filter(|p| !p.is_empty()) {
        Some(path) => path.into(),
        None => crate::app::croft_cache_dir().join("startup-trace.txt"),
    };
    let start = Instant::now();
    if let Ok(mut slot) = TRACE.lock() {
        *slot = Some(Trace {
            path,
            start,
            start_cpu: thread_cpu(),
            marks: Vec::with_capacity(32),
        });
        ON.store(true, Ordering::Relaxed);
    }
}

/// Note that the startup step `label` has just finished.
pub fn mark(label: &'static str) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let mark = Mark {
        label,
        at: Instant::now(),
        cpu: thread_cpu(),
    };
    if let Ok(mut slot) = TRACE.lock()
        && let Some(trace) = slot.as_mut()
    {
        trace.marks.push(mark);
    }
}

/// The first frame is on screen: stop recording, and write the trace on a
/// thread of its own. Off the UI thread because a home folder on a stalled
/// network mount, or a FIFO no one reads named by `CROFT_STARTUP_TRACE`,
/// would otherwise hold the editor, input and quit included, at its first
/// frame.
pub fn finish() {
    if !ON.swap(false, Ordering::Relaxed) {
        return;
    }
    let Some(trace) = TRACE.lock().ok().and_then(|mut slot| slot.take()) else {
        return;
    };
    std::thread::spawn(move || write(&trace));
}

fn write(trace: &Trace) {
    let since_main = trace.start.elapsed();
    let lineage = lineage()
        .into_iter()
        .map(|mut p| {
            // `lineage` measured ages from now; the trace counts from `main`.
            p.age = p.age.saturating_sub(since_main);
            p
        })
        .collect::<Vec<_>>();
    if let Some(dir) = trace.path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&trace.path, render(trace, &lineage));
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn render(trace: &Trace, lineage: &[Lineage]) -> String {
    let mut out = format!("croft {} startup\n\n", env!("CARGO_PKG_VERSION"));
    if !lineage.is_empty() {
        out.push_str("Started before croft's own code ran (ms before main):\n");
        for p in lineage.iter().rev() {
            out.push_str(&format!(
                "{:>10.1}  {} (pid {})\n",
                ms(p.age),
                p.name,
                p.pid
            ));
        }
        out.push('\n');
    }
    out.push_str("Inside croft (ms after main; cpu is time spent computing, the rest waiting):\n");
    out.push_str("     at      step       cpu  step\n");
    let mut prev = trace.start;
    let mut prev_cpu = trace.start_cpu;
    for mark in &trace.marks {
        let cpu = match (mark.cpu, prev_cpu) {
            (Some(now), Some(then)) => format!("{:>8.1}", ms(now.saturating_sub(then))),
            _ => format!("{:>8}", "-"),
        };
        out.push_str(&format!(
            "{:>7.1}  {:>8.1}  {cpu}  {}\n",
            ms(mark.at.duration_since(trace.start)),
            ms(mark.at.duration_since(prev)),
            mark.label,
        ));
        prev = mark.at;
        prev_cpu = mark.cpu;
    }
    let inside = prev.duration_since(trace.start);
    out.push_str(&format!("\nFirst frame {:.1} ms after main", ms(inside)));
    if let Some(oldest) = lineage.last() {
        out.push_str(&format!(
            ", {:.1} ms after {} started",
            ms(oldest.age + inside),
            oldest.name
        ));
    }
    out.push_str(".\n");
    out
}

/// This thread's CPU time so far.
fn thread_cpu() -> Option<Duration> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer for the duration of the call.
    let ok = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts) } == 0;
    ok.then(|| Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
}

/// How many ancestors are worth naming: the terminal, its login wrapper and
/// a shell or two. Past the terminal is the OS's own launcher.
const LINEAGE_DEPTH: usize = 5;

/// This process and the ones that started it, nearest first, each with how
/// long ago it started, up to (not including) the init process.
fn lineage() -> Vec<Lineage> {
    let mut out = Vec::new();
    let mut pid = std::process::id();
    while pid > 1 && out.len() < LINEAGE_DEPTH {
        let Some((info, ppid)) = process_info(pid) else {
            break;
        };
        out.push(info);
        pid = ppid;
    }
    out
}

/// `pid`'s name and age, and its parent, from the kernel's process table,
/// where it keeps start times to the microsecond.
#[cfg(target_os = "macos")]
fn process_info(pid: u32) -> Option<(Lineage, u32)> {
    // SAFETY: `info` is a plain C struct the call fills; the size passed is
    // its own, and a short return is rejected before it is read.
    let info = unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        );
        if got != size {
            return None;
        }
        info
    };
    let started = std::time::SystemTime::UNIX_EPOCH
        + Duration::new(
            info.pbi_start_tvsec,
            (info.pbi_start_tvusec as u32).saturating_mul(1000),
        );
    let name_bytes = if info.pbi_name[0] != 0 {
        &info.pbi_name[..]
    } else {
        &info.pbi_comm[..]
    };
    let name: String = name_bytes
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8 as char)
        .collect();
    let age = std::time::SystemTime::now()
        .duration_since(started)
        .unwrap_or_default();
    Some((Lineage { pid, name, age }, info.pbi_ppid))
}

/// `pid`'s name and age, and its parent, from `/proc`. Linux keeps start
/// times in clock ticks, a hundredth of a second, which is still enough to
/// tell a slow terminal from a slow croft.
#[cfg(target_os = "linux")]
fn process_info(pid: u32) -> Option<(Lineage, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (name, start_ticks, ppid) = parse_proc_stat(&stat)?;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    // SAFETY: sysconf has no preconditions.
    let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks_per_sec <= 0 {
        return None;
    }
    let started = start_ticks as f64 / ticks_per_sec as f64;
    let age = Duration::from_secs_f64((uptime - started).max(0.0));
    Some((Lineage { pid, name, age }, ppid))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_info(_pid: u32) -> Option<(Lineage, u32)> {
    None
}

/// The name, start time (clock ticks since boot) and parent of a
/// `/proc/<pid>/stat` line. The name sits in parentheses and may itself
/// hold spaces and parentheses, so the fields are counted from the last `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat(stat: &str) -> Option<(String, u64, u32)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_string();
    // After the name: state (field 3), ppid (4), ..., starttime (22).
    let rest: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    let ppid = rest.get(1)?.parse().ok()?;
    let start = rest.get(19)?.parse().ok()?;
    Some((name, start, ppid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace_with(marks: &[(&'static str, u64, u64)]) -> Trace {
        let start = Instant::now();
        Trace {
            path: std::path::PathBuf::new(),
            start,
            start_cpu: Some(Duration::ZERO),
            marks: marks
                .iter()
                .map(|&(label, at, cpu)| Mark {
                    label,
                    at: start + Duration::from_millis(at),
                    cpu: Some(Duration::from_millis(cpu)),
                })
                .collect(),
        }
    }

    #[test]
    fn a_trace_lists_each_step_with_its_wall_and_cpu_time() {
        let trace = trace_with(&[("settings", 3, 2), ("first frame drawn", 10, 6)]);
        let text = render(&trace, &[]);
        assert!(
            text.contains("    3.0       3.0       2.0  settings"),
            "{text}"
        );
        assert!(
            text.contains("   10.0       7.0       4.0  first frame drawn"),
            "{text}"
        );
        assert!(text.contains("First frame 10.0 ms after main."), "{text}");
    }

    /// A launch from Croft.app starts a terminal first; the trace says how
    /// long before croft it started, oldest first, so its own startup is
    /// told apart from croft's.
    #[test]
    fn the_processes_that_started_croft_head_the_trace_oldest_first() {
        let trace = trace_with(&[("first frame drawn", 20, 10)]);
        let lineage = [
            Lineage {
                pid: 30,
                name: "croft".into(),
                age: Duration::from_millis(2),
            },
            Lineage {
                pid: 20,
                name: "login".into(),
                age: Duration::from_millis(40),
            },
            Lineage {
                pid: 10,
                name: "ghostty".into(),
                age: Duration::from_millis(600),
            },
        ];
        let text = render(&trace, &lineage);
        let ghostty = text.find("ghostty (pid 10)").expect("ghostty listed");
        let login = text.find("login (pid 20)").expect("login listed");
        let croft = text.find("croft (pid 30)").expect("croft listed");
        assert!(ghostty < login && login < croft, "{text}");
        assert!(text.contains("     600.0  ghostty"), "{text}");
        assert!(
            text.contains("First frame 20.0 ms after main, 620.0 ms after ghostty started."),
            "{text}"
        );
    }

    #[test]
    fn a_proc_stat_line_gives_name_start_and_parent() {
        let line = "4242 (my (odd) name) S 17 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765 1000 200";
        assert_eq!(
            parse_proc_stat(line),
            Some(("my (odd) name".to_string(), 98765, 17))
        );
        assert_eq!(parse_proc_stat("garbage"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn this_process_reads_as_the_first_of_its_lineage() {
        let lineage = lineage();
        assert_eq!(lineage.first().map(|p| p.pid), Some(std::process::id()));
    }
}
