//! Mouse reporting: whether the program in the pane asked for mouse events, a click, drag or
//! wheel encoded as the report it expects, and which grid cell a screen point falls on.

use super::*;

/// A mouse button (or wheel direction) to forward to the child program
/// when it has mouse tracking enabled. Encoded as an SGR (1006) report when
/// the child selected that encoding, otherwise as a legacy X10 report.
#[derive(Clone, Copy, PartialEq)]
pub enum MouseButtonKind {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

/// What happened to the button: a press, a release, or motion while held.
#[derive(Clone, Copy, PartialEq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// Keyboard modifiers held during a mouse event, folded into the report's
/// button byte (Shift +4, Alt +8, Ctrl +16).
#[derive(Clone, Copy, Default)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl PtyTerminal {
    pub fn cell_at(&self, col: u16, row: u16) -> Option<(u16, u16)> {
        let inner = self.last_inner;
        if inner.width == 0 || inner.height == 0 {
            return None;
        }
        if col < inner.x || col >= inner.x + inner.width {
            return None;
        }
        if row < inner.y || row >= inner.y + inner.height {
            return None;
        }
        Some((row - inner.y, col - inner.x))
    }

    /// The host-screen cell nearest `(col, row)` that is still inside the
    /// pane's grid. A forwarded drag or release that leaves the pane is
    /// delivered at the edge cell it crossed, as xterm does with a grabbed
    /// pointer, rather than dropped: a child that got the press must also
    /// get the release, or it keeps a button held that the user let go.
    /// Returns the input unchanged when the pane has no grid yet.
    pub fn clamp_to_grid(&self, col: u16, row: u16) -> (u16, u16) {
        let inner = self.last_inner;
        if inner.width == 0 || inner.height == 0 {
            return (col, row);
        }
        (
            col.clamp(inner.x, inner.x + inner.width - 1),
            row.clamp(inner.y, inner.y + inner.height - 1),
        )
    }

    /// True when the child program has enabled any mouse-tracking mode
    /// (DECSET 1000 click / 1002 button-drag / 1003 any-motion). When it has,
    /// wheel events should be forwarded to it via `report_mouse` so it scrolls
    /// its own buffer rather than croft synthesising arrow keys.
    pub fn mouse_reporting(&self) -> bool {
        self.term.lock().mode().intersects(TermMode::MOUSE_MODE)
    }

    /// True when the child asked for button-held MOTION too (DECSET 1002
    /// button-drag or 1003 any-motion), not just clicks (1000). Decides who
    /// a left-button drag belongs to: a child that only asked for clicks
    /// cannot use the drag, so croft keeps it as a text selection (#474).
    pub fn mouse_motion_reporting(&self) -> bool {
        self.term
            .lock()
            .mode()
            .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    }

    /// Encode a mouse gesture at host-screen cell `(col, row)` as a mouse
    /// report and send it to the child. Returns false without writing when
    /// the cell is outside the pane, the child isn't tracking the mouse, or
    /// the event is motion the child didn't ask for (no 1002/1003). The report
    /// is SGR (1006) when the child selected it, otherwise legacy X10.
    pub fn report_mouse(
        &mut self,
        button: MouseButtonKind,
        action: MouseAction,
        col: u16,
        row: u16,
        mods: MouseMods,
    ) -> bool {
        let Some((r, c)) = self.cell_at(col, row) else {
            return false;
        };
        let mode = *self.term.lock().mode();
        if !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        // Motion (a held-button drag) is only wanted under 1002/1003.
        if action == MouseAction::Motion
            && !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
        {
            return false;
        }
        let report = encode_mouse_report(
            mode.contains(TermMode::SGR_MOUSE),
            button,
            action,
            c,
            r,
            mods,
        );
        #[cfg(test)]
        self.written_for_test
            .lock()
            .unwrap()
            .extend_from_slice(&report);
        let _ = self.input_tx.send(report);
        self.input_seen = true;
        self.pty_dirty.store(true, Ordering::Release);
        true
    }
}

/// Encode a mouse event as the byte sequence a child program expects.
/// `sgr` selects SGR (1006) encoding over legacy X10. `local_col`/`local_row`
/// are 0-based cells within the pane; the wire protocol is 1-based, so both
/// are offset by one. Button/modifier bit layout follows xterm: Left/Middle/
/// Right = 0/1/2, wheel up/down = 64/65, +32 for motion, +4/+8/+16 for
/// Shift/Alt/Ctrl. Under X10 a release reports button code 3 and coordinates
/// clamp to 223 (the largest a single 0x20-based byte can carry).
pub fn encode_mouse_report(
    sgr: bool,
    button: MouseButtonKind,
    action: MouseAction,
    local_col: u16,
    local_row: u16,
    mods: MouseMods,
) -> Vec<u8> {
    let mut cb: u8 = match button {
        MouseButtonKind::Left => 0,
        MouseButtonKind::Middle => 1,
        MouseButtonKind::Right => 2,
        MouseButtonKind::WheelUp => 64,
        MouseButtonKind::WheelDown => 65,
    };
    if action == MouseAction::Motion {
        cb += 32;
    }
    if mods.shift {
        cb += 4;
    }
    if mods.alt {
        cb += 8;
    }
    if mods.ctrl {
        cb += 16;
    }
    let cx = local_col + 1;
    let cy = local_row + 1;
    if sgr {
        let terminator = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        format!("\x1b[<{cb};{cx};{cy}{terminator}").into_bytes()
    } else {
        if action == MouseAction::Release {
            cb = (cb & !0b11) | 0b11;
        }
        let enc = |v: u16| (v.min(223) as u8).wrapping_add(32);
        vec![0x1b, b'[', b'M', cb.wrapping_add(32), enc(cx), enc(cy)]
    }
}
