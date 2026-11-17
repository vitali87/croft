//! Writing to the pane: typed input, pastes and bracketed paste, and the clipboard
//! sequences.

use super::*;

impl PtyTerminal {
    /// Whether the child enabled application cursor keys (DECCKM, `\e[?1h`).
    /// Arrows and Home/End must then arrive as SS3 (`\eOA`…) — the form
    /// terminfo advertises, and the only one apps that bind keys from
    /// terminfo (Python's REPL, less) recognize.
    pub fn app_cursor_keys(&self) -> bool {
        self.term.lock().mode().contains(TermMode::APP_CURSOR)
    }

    /// The text the child last copied through OSC 52 since the previous
    /// call, if any (drains the latch). Only the newest copy survives a
    /// tick: the clipboard holds one value, so older ones were overwritten
    /// anyway.
    pub fn take_clipboard_store(&self) -> Option<String> {
        self.clipboard_store.lock().unwrap().take()
    }

    /// Whether input was written that the shell has not echoed yet, so the
    /// prompt line on screen lags what was typed.
    pub fn awaiting_echo(&self) -> bool {
        self.last_input_ms > self.last_output_ms.load(Ordering::Relaxed)
    }

    /// Bytes advanced into the grid since the last redraw, without
    /// clearing. The main loop uses this to classify a PTY-only redraw as
    /// interactive echo (small, redraw now) or bulk stream (large, stay
    /// capped). Reset on the next `take_dirty`.
    pub fn peek_pending_bytes(&self) -> usize {
        self.pty_pending_bytes.load(Ordering::Relaxed)
    }

    pub fn write_input(&mut self, data: &[u8]) {
        self.reset_scrollback();
        self.input_seen = true;
        self.last_input_ms = now_ms();
        #[cfg(test)]
        self.written_for_test
            .lock()
            .unwrap()
            .extend_from_slice(data);
        let _ = self.input_tx.send(data.to_vec());
        self.pty_dirty.store(true, Ordering::Release);
    }

    /// Test-only: every byte the app has written toward the child's stdin.
    #[cfg(test)]
    pub fn written_bytes_for_test(&self) -> Vec<u8> {
        self.written_for_test.lock().unwrap().clone()
    }

    /// Whether the program in the pane turned on bracketed paste, so a
    /// pasted line break waits for Enter instead of running the line (#1274).
    pub fn bracketed_paste(&self) -> bool {
        self.term.lock().mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Paste a payload into the embedded shell. Bracketed-paste markers
    /// are added only if the inner program asked for them; otherwise the
    /// payload is sent raw so simple shells don't see literal `\e[200~`.
    pub fn paste_input(&mut self, payload: &[u8]) {
        // The terminal's own mode, as its parser tracked it: the byte
        // sniffer missed a `\e[?2004h` split across two reads, a combined
        // `\e[?1049;2004h` and a reset, and pasted line by line into a shell
        // that had asked for bracketed paste.
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            self.write_input(b"\x1b[200~");
            // No ESC inside the brackets, as xterm and VS Code do: pasted
            // text holding `\e[201~` would close the paste early, and the
            // shell would run what follows it (`ls\e[201~rm -rf ~\r`). Any
            // program in a pane can plant that on the clipboard via OSC 52.
            let clean: Vec<u8> = payload.iter().copied().filter(|&b| b != 0x1b).collect();
            self.write_input(&clean);
            self.write_input(b"\x1b[201~");
        } else {
            self.write_input(payload);
        }
    }
}

/// Build the OSC 52 escape sequence that asks the host terminal to put
/// `text` on the system clipboard.
pub fn osc52_copy_seq(text: &str) -> Vec<u8> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = Vec::with_capacity(encoded.len() + 8);
    out.extend_from_slice(b"\x1b]52;c;");
    out.extend_from_slice(encoded.as_bytes());
    out.push(0x07);
    out
}
