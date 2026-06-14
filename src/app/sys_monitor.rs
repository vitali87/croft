use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use crate::sysmon::{SystemSample, system_monitor_loop};
use crate::widgets::system_panel::SystemPanel;

pub struct SysMonitor {
    rx: Receiver<SystemSample>,
    /// Drives whether the sampler thread collects: false (the default, panel
    /// collapsed) means it idles without touching sysinfo; the app flips it true
    /// once the SYSTEM panel is expanded.
    active: Arc<AtomicBool>,
}

impl SysMonitor {
    pub fn spawn() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<SystemSample>();
        let active = Arc::new(AtomicBool::new(false));
        let thread_active = Arc::clone(&active);
        std::thread::spawn(move || {
            system_monitor_loop(tx, thread_active);
        });
        Self { rx, active }
    }

    /// Start or stop collection. Cheap (an atomic store); called each frame from
    /// the app with the SYSTEM panel's expanded state.
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    pub fn drain(&self, panel: &mut SystemPanel) -> bool {
        let mut changed = false;
        while let Ok(sample) = self.rx.try_recv() {
            panel.apply_sample(sample);
            changed = true;
        }
        changed
    }
}
