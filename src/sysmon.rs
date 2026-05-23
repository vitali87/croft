use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use sysinfo::{Components, Disks, MINIMUM_CPU_UPDATE_INTERVAL, Networks, System};

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
    let mut disks = Disks::new_with_refreshed_list();
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
        disks.refresh(true);
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

        let disk_pct = root_disk_pct(&disks).unwrap_or(0);

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
    nets.iter()
        .map(|(_, d)| d.total_received().saturating_add(d.total_transmitted()))
        .sum()
}

fn root_disk_pct(disks: &Disks) -> Option<u8> {
    let root = disks.iter().find(|d| d.mount_point().as_os_str() == "/")?;
    let total = root.total_space();
    if total == 0 {
        return None;
    }
    let used = total.saturating_sub(root.available_space());
    Some(((used as f64 / total as f64) * 100.0).clamp(0.0, 100.0).round() as u8)
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
    hottest.map(|t| t.clamp(0.0, 250.0).round() as u8)
}
