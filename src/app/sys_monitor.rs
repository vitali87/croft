use std::sync::mpsc::Receiver;

use crate::sysmon::{SystemSample, system_monitor_loop};
use crate::widgets::system_panel::SystemPanel;

pub struct SysMonitor {
    rx: Receiver<SystemSample>,
}

impl SysMonitor {
    pub fn spawn() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<SystemSample>();
        std::thread::spawn(move || {
            system_monitor_loop(tx);
        });
        Self { rx }
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
