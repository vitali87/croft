//! Developer: Show Memory Usage (#694): where croft's memory is, per
//! subsystem, so growth can be attributed without a profiler. The app
//! gathers plain figures; this module only lays them out, so the layout is
//! testable without a terminal or a language server.

use std::fmt::Write;

/// One terminal pane's share.
pub struct TerminalMem {
    pub name: String,
    /// Closed but kept for reopening; its rewind is already released.
    pub closed: bool,
    pub rewind_bytes: usize,
    pub scrollback_lines: usize,
    pub screen_lines: usize,
    pub columns: usize,
}

/// One open buffer's undo history.
pub struct BufferMem {
    pub name: String,
    pub steps: usize,
    pub bytes: usize,
}

/// One OUTPUT channel.
pub struct ChannelMem {
    pub name: String,
    pub lines: usize,
    pub bytes: usize,
}

/// One language server's stored diagnostics.
pub struct ServerMem {
    pub name: String,
    pub files: usize,
    pub diagnostics: usize,
}

pub struct MemoryReport {
    /// `VmRSS` in KiB; `None` where there is no `/proc`.
    pub rss_kb: Option<u64>,
    pub rewind_budget: usize,
    /// Bytes of one terminal grid cell, for the scrollback estimate.
    pub cell_bytes: usize,
    pub terminals: Vec<TerminalMem>,
    pub buffers: Vec<BufferMem>,
    pub channels: Vec<ChannelMem>,
    pub servers: Vec<ServerMem>,
}

/// `n` bytes as the largest unit that keeps it at or above 1.
fn human(n: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

impl MemoryReport {
    /// The report as plain text, one section per subsystem.
    pub fn format(&self) -> String {
        let mut out = String::from("croft memory usage\n\n");
        match self.rss_kb {
            Some(kb) => {
                let _ = writeln!(out, "Process RSS: {}", human(kb as usize * 1024));
            }
            None => out.push_str("Process RSS: unavailable (no /proc on this platform)\n"),
        }

        let _ = writeln!(out, "\nTerminal panes ({})", self.terminals.len());
        let mut rewind_total = 0;
        let mut grid_total = 0;
        for t in &self.terminals {
            let grid = (t.scrollback_lines + t.screen_lines) * t.columns * self.cell_bytes;
            rewind_total += t.rewind_bytes;
            grid_total += grid;
            let closed = if t.closed {
                " (closed, reopenable)"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  {}{closed}: rewind {}, scrollback {} lines (~{} est.)",
                t.name,
                human(t.rewind_bytes),
                t.scrollback_lines,
                human(grid)
            );
        }
        let _ = writeln!(
            out,
            "  Rewind total: {} of {} shared budget",
            human(rewind_total),
            human(self.rewind_budget)
        );
        let _ = writeln!(
            out,
            "  Grid total: ~{} est. (rows × columns × {} B per cell)",
            human(grid_total),
            self.cell_bytes
        );

        let _ = writeln!(out, "\nUndo history ({} buffers)", self.buffers.len());
        let mut buffers: Vec<&BufferMem> = self.buffers.iter().collect();
        buffers.sort_by_key(|b| std::cmp::Reverse(b.bytes));
        for b in &buffers {
            let _ = writeln!(out, "  {}: {} in {} steps", b.name, human(b.bytes), b.steps);
        }
        let undo_total: usize = buffers.iter().map(|b| b.bytes).sum();
        let _ = writeln!(out, "  Total: {} (text only)", human(undo_total));

        let _ = writeln!(out, "\nOUTPUT channels ({})", self.channels.len());
        for c in &self.channels {
            let _ = writeln!(out, "  {}: {} lines, {}", c.name, c.lines, human(c.bytes));
        }
        let output_total: usize = self.channels.iter().map(|c| c.bytes).sum();
        let _ = writeln!(out, "  Total: {} (text only)", human(output_total));

        // Only diagnostics are stored per server, so a running server that
        // has published none has nothing here: say so rather than print a
        // count that reads as "no servers running" (#694).
        let count = match self.servers.len() {
            1 => String::from("1 server"),
            n => format!("{n} servers"),
        };
        let _ = writeln!(out, "\nLanguage server diagnostics ({count})");
        if self.servers.is_empty() {
            out.push_str("  none held\n");
        }
        for s in &self.servers {
            let _ = writeln!(
                out,
                "  {}: {} diagnostics in {} files",
                s.name, s.diagnostics, s.files
            );
        }
        out.push_str(
            "\nText figures count string bytes, not allocator overhead; \
             run the command again to refresh.\n",
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    fn report() -> MemoryReport {
        MemoryReport {
            rss_kb: Some(300 * 1024),
            rewind_budget: 256 * MIB,
            cell_bytes: 24,
            terminals: vec![
                TerminalMem {
                    name: String::from("zsh"),
                    closed: false,
                    rewind_bytes: 3 * MIB,
                    scrollback_lines: 10_000,
                    screen_lines: 40,
                    columns: 100,
                },
                TerminalMem {
                    name: String::from("cargo"),
                    closed: true,
                    rewind_bytes: 0,
                    scrollback_lines: 0,
                    screen_lines: 24,
                    columns: 80,
                },
            ],
            buffers: vec![
                BufferMem {
                    name: String::from("small.rs"),
                    steps: 2,
                    bytes: 100,
                },
                BufferMem {
                    name: String::from("big.json"),
                    steps: 16,
                    bytes: 40 * MIB,
                },
            ],
            channels: vec![ChannelMem {
                name: String::from("Git"),
                lines: 12,
                bytes: 2048,
            }],
            servers: vec![ServerMem {
                name: String::from("rust-analyzer"),
                files: 3,
                diagnostics: 17,
            }],
        }
    }

    #[test]
    fn sizes_read_in_the_largest_fitting_unit() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(256 * MIB), "256.0 MiB");
        assert_eq!(human(3 * 1024 * MIB), "3.0 GiB");
    }

    /// #694: each subsystem gets its own section, with totals against the
    /// rewind budget and estimates labelled as such.
    #[test]
    fn the_report_attributes_memory_per_subsystem() {
        let text = report().format();
        assert!(text.contains("Process RSS: 300.0 MiB"), "{text}");
        // (10_000 + 40) × 100 × 24 B.
        assert!(
            text.contains("zsh: rewind 3.0 MiB, scrollback 10000 lines (~23.0 MiB est.)"),
            "{text}"
        );
        assert!(
            text.contains("cargo (closed, reopenable): rewind 0 B"),
            "{text}"
        );
        assert!(
            text.contains("Rewind total: 3.0 MiB of 256.0 MiB shared budget"),
            "{text}"
        );
        assert!(text.contains("Git: 12 lines, 2.0 KiB"), "{text}");
        assert!(
            text.contains("rust-analyzer: 17 diagnostics in 3 files"),
            "{text}"
        );
        assert!(text.contains("Total: 40.0 MiB (text only)"), "{text}");
    }

    /// The buffer holding the most history is listed first.
    #[test]
    fn buffers_are_listed_largest_first() {
        let text = report().format();
        let big = text.find("big.json").unwrap();
        let small = text.find("small.rs").unwrap();
        assert!(big < small, "{text}");
    }

    /// Only servers holding diagnostics are listed, so the heading says what
    /// it counts, and an empty list does not read as "no server running".
    #[test]
    fn language_servers_are_counted_by_the_diagnostics_they_hold() {
        let text = report().format();
        assert!(
            text.contains("Language server diagnostics (1 server)"),
            "{text}"
        );
        let text = MemoryReport {
            servers: Vec::new(),
            ..report()
        }
        .format();
        assert!(
            text.contains("Language server diagnostics (0 servers)\n  none held\n"),
            "{text}"
        );
    }

    #[test]
    fn a_platform_without_proc_says_so() {
        let text = MemoryReport {
            rss_kb: None,
            ..report()
        }
        .format();
        assert!(text.contains("Process RSS: unavailable"), "{text}");
    }
}
