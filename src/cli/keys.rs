//! `croft keys`: the diagnostic that prints each key and mouse event as croft receives it,
//! with its kind and modifiers.

use super::*;

/// How `croft keys` names a key event's kind.
pub(super) fn key_kind_label(kind: crossterm::event::KeyEventKind) -> &'static str {
    use crossterm::event::KeyEventKind;
    match kind {
        KeyEventKind::Press => "Press",
        KeyEventKind::Repeat => "Repeat",
        KeyEventKind::Release => "Release",
    }
}

pub(super) fn keys_diagnostic(mouse: bool) -> Result<()> {
    use crossterm::event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    };
    use crossterm::{execute, terminal};
    use std::io::stdout;
    use std::time::Duration;

    println!("croft keys: press any key to inspect; Ctrl+C to quit.");
    println!("If the kitty keyboard protocol is negotiated, modifier keys");
    println!("(including Cmd/Super on macOS) will appear in the modifier list.");
    println!("kind= is Press, Repeat or Release; the last two arrive only when the");
    println!("terminal honours the protocol's event-type reporting.");
    if mouse {
        println!("Mouse capture is on: click and drag to inspect mouse events.");
    }
    println!();

    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut out = stdout();
    let kbd_enhanced = execute!(
        out,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    )
    .is_ok();
    let mouse_capture_enabled = mouse && execute!(out, EnableMouseCapture).is_ok();

    let result = (|| -> Result<()> {
        loop {
            if event::poll(Duration::from_millis(500))? {
                match event::read()? {
                    Event::Key(k) => {
                        // Every kind is shown: what a terminal sends for
                        // Repeat and Release is exactly what this tool is for.
                        let kind = key_kind_label(k.kind);
                        let released = k.kind == KeyEventKind::Release;
                        let mut mods: Vec<&str> = Vec::new();
                        if k.modifiers.contains(KeyModifiers::CONTROL) {
                            mods.push("CONTROL");
                        }
                        if k.modifiers.contains(KeyModifiers::ALT) {
                            mods.push("ALT");
                        }
                        if k.modifiers.contains(KeyModifiers::SHIFT) {
                            mods.push("SHIFT");
                        }
                        if k.modifiers.contains(KeyModifiers::SUPER) {
                            mods.push("SUPER (Cmd)");
                        }
                        if k.modifiers.contains(KeyModifiers::HYPER) {
                            mods.push("HYPER");
                        }
                        if k.modifiers.contains(KeyModifiers::META) {
                            mods.push("META");
                        }
                        let mods_s = if mods.is_empty() {
                            String::from("none")
                        } else {
                            mods.join(" + ")
                        };
                        // Quit on Ctrl+C.
                        if !released
                            && matches!(k.code, KeyCode::Char('c'))
                            && k.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            print!("\r\nQuitting (Ctrl+C).\r\n");
                            break;
                        }
                        let clipboard_probe =
                            if !released && is_paste_probe_key(k.code, k.modifiers) {
                                match crate::clipboard::read_string() {
                                    Some(s) if s.is_empty() => String::from("  clipboard=empty"),
                                    Some(s) => format!(
                                        "  clipboard=ok chars={} bytes={}",
                                        s.chars().count(),
                                        s.len()
                                    ),
                                    None => String::from("  clipboard=read_failed"),
                                }
                            } else {
                                String::new()
                            };
                        print!(
                            "\r  kind={:<7}  key={:?}  code={:?}  modifiers=[{}]  kitty={}{}\r\n",
                            kind, k, k.code, mods_s, kbd_enhanced, clipboard_probe
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    Event::Paste(s) => {
                        print!(
                            "\r  paste_event chars={} bytes={} kitty={}\r\n",
                            s.chars().count(),
                            s.len(),
                            kbd_enhanced
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    Event::Mouse(mev) => {
                        let mut mods: Vec<&str> = Vec::new();
                        if mev.modifiers.contains(KeyModifiers::CONTROL) {
                            mods.push("CONTROL");
                        }
                        if mev.modifiers.contains(KeyModifiers::ALT) {
                            mods.push("ALT");
                        }
                        if mev.modifiers.contains(KeyModifiers::SHIFT) {
                            mods.push("SHIFT");
                        }
                        let mods_s = if mods.is_empty() {
                            String::from("none")
                        } else {
                            mods.join(" + ")
                        };
                        print!(
                            "\r  mouse kind={:?}  col={}  row={}  modifiers=[{}]\r\n",
                            mev.kind, mev.column, mev.row, mods_s
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    other => {
                        print!("\r  event={other:?} kitty={kbd_enhanced}\r\n");
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                }
            }
        }
        Ok(())
    })();

    if mouse_capture_enabled {
        execute!(stdout(), DisableMouseCapture).ok();
    }
    if kbd_enhanced {
        execute!(stdout(), PopKeyboardEnhancementFlags).ok();
    }
    terminal::disable_raw_mode().ok();
    result
}

pub(super) fn is_paste_probe_key(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    let crossterm::event::KeyCode::Char(c) = code else {
        return false;
    };
    c == '\u{16}'
        || (c.eq_ignore_ascii_case(&'v')
            && (modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                || modifiers.contains(crossterm::event::KeyModifiers::SUPER)))
}
