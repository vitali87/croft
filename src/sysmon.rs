use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use sysinfo::{Components, MINIMUM_CPU_UPDATE_INTERVAL, Networks, System};

#[derive(Debug, Clone, Copy)]
pub struct SystemSample {
    pub cpu_pct: u8,
    pub mem_pct: u8,
    pub net_bps: u64,
    pub disk_pct: u8,
    pub temp_c: Option<u8>,
}

const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

pub fn system_monitor_loop(tx: Sender<SystemSample>) {
    let mut sys = System::new();
    let mut nets = Networks::new_with_refreshed_list();
    let mut comps = Components::new_with_refreshed_list();

    sys.refresh_memory();
    sys.refresh_cpu_usage();
    std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);

    let mut last_net_total: u64 = sum_net_bytes(&nets);
    let mut last_tick = Instant::now();

    loop {
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        nets.refresh(true);
        comps.refresh(true);

        let now = Instant::now();
        let elapsed = now.duration_since(last_tick).as_secs_f64().max(0.001);
        last_tick = now;

        let cpu_pct = sys.global_cpu_usage().clamp(0.0, 100.0).round() as u8;

        let mem_total = sys.total_memory().max(1);
        let mem_pct = ((sys.used_memory() as f64 / mem_total as f64) * 100.0)
            .clamp(0.0, 100.0)
            .round() as u8;

        let net_total = sum_net_bytes(&nets);
        let net_delta = net_total.saturating_sub(last_net_total);
        last_net_total = net_total;
        let net_bps = ((net_delta as f64) / elapsed).round() as u64;

        let disk_pct = home_disk_pct().unwrap_or(0);

        let temp_c = peak_temp(&comps);

        if tx
            .send(SystemSample {
                cpu_pct,
                mem_pct,
                net_bps,
                disk_pct,
                temp_c,
            })
            .is_err()
        {
            return;
        }

        std::thread::sleep(SAMPLE_INTERVAL);
    }
}

fn sum_net_bytes(nets: &Networks) -> u64 {
    nets.values()
        .map(|d| d.total_received().saturating_add(d.total_transmitted()))
        .sum()
}

/// Usage of the filesystem holding the user's home directory - the storage
/// the user can actually fill - via the same `statvfs` call `df $HOME`
/// makes. Measuring the `/` mount instead is wrong on Android: there the
/// rootfs is the sealed read-only system partition (permanently ~100%)
/// while all writable storage lives on the /data partition, so the gauge
/// pinned at 100% on Termux no matter how much space was free.
fn home_disk_pct() -> Option<u8> {
    let home = std::env::var_os("HOME").unwrap_or_else(|| std::ffi::OsString::from("/"));
    let path = std::ffi::CString::new(home.as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    disk_pct_from(st.f_blocks as u64, st.f_bfree as u64, st.f_bavail as u64)
}

/// df's percentage: used blocks over (used + available-to-user), rounded
/// up like coreutils, so the gauge agrees with what `df` prints. The
/// root-reserved slice (`bfree - bavail`) is excluded from the
/// denominator, again matching df.
fn disk_pct_from(blocks: u64, bfree: u64, bavail: u64) -> Option<u8> {
    let used = blocks.saturating_sub(bfree);
    let total = used.saturating_add(bavail);
    if total == 0 {
        return None;
    }
    Some(
        ((used as f64 / total as f64) * 100.0)
            .ceil()
            .clamp(0.0, 100.0) as u8,
    )
}

fn peak_temp(comps: &Components) -> Option<u8> {
    let mut hottest: Option<f32> = None;
    for c in comps.iter() {
        if let Some(t) = c.temperature()
            && t.is_finite()
            && t > 0.0
        {
            hottest = Some(hottest.map_or(t, |h| h.max(t)));
        }
    }
    if let Some(t) = hottest {
        return Some(t.clamp(0.0, 250.0).round() as u8);
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(t) = linux_peak_temp() {
            return Some(t);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_peak_temp() -> Option<u8> {
    let mut hottest: Option<f32> = None;
    let candidates = [
        std::path::PathBuf::from("/sys/class/thermal"),
        std::path::PathBuf::from("/sys/class/hwmon"),
    ];
    for root in &candidates {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            let temp_files: Vec<std::path::PathBuf> = if dir
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("thermal_zone"))
            {
                vec![dir.join("temp")]
            } else if dir
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("hwmon"))
            {
                let Ok(inner) = std::fs::read_dir(&dir) else {
                    continue;
                };
                inner
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name();
                        let s = name.to_str()?;
                        if s.starts_with("temp") && s.ends_with("_input") {
                            Some(e.path())
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                continue;
            };
            for path in temp_files {
                let Ok(contents) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(millis) = contents.trim().parse::<i32>() else {
                    continue;
                };
                let celsius = millis as f32 / 1000.0;
                if celsius.is_finite() && (5.0..200.0).contains(&celsius) {
                    hottest = Some(hottest.map_or(celsius, |h| h.max(celsius)));
                }
            }
        }
    }
    hottest.map(|t| t.clamp(0.0, 250.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_pct_uses_dfs_used_over_used_plus_avail_formula() {
        // 75 used, 25 available to the user: 75%.
        assert_eq!(disk_pct_from(100, 25, 25), Some(75));
        // Root-reserved blocks (bfree > bavail) shrink the denominator,
        // exactly like df: used=499, avail=450 -> 499/949 -> ceil 53%.
        assert_eq!(disk_pct_from(1000, 501, 450), Some(53));
        // A degenerate filesystem reports nothing rather than a fake 0%.
        assert_eq!(disk_pct_from(0, 0, 0), None);
    }

    #[test]
    fn home_disk_pct_measures_a_real_filesystem() {
        let pct = home_disk_pct().expect("home filesystem must be measurable");
        assert!(pct <= 100);
    }
}
