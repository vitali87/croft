//! The debug console: its lines, program output, the REPL, mirroring to OUTPUT's Debug
//! Console channel, and hover-to-evaluate while paused.

use super::*;

/// A session is ending: mirror to OUTPUT's "Debug Console" channel each line
/// its console streams leave open (#932), as `console` shows it, since no
/// newline will come to finish it. `dropped` is how many lines have left the
/// front of `console`, which the streams' positions count from.
pub(in crate::app) fn mirror_open_console_lines(
    streams: &mut [crate::dap::session::ConsoleStream],
    console: &[crate::ansi_text::AnsiLine],
    dropped: usize,
) {
    for stream in streams {
        let Some(line) = stream.ansi.finish() else {
            continue;
        };
        let shown = stream
            .shown_at
            .take()
            .and_then(|at| at.checked_sub(dropped))
            .and_then(|i| console.get(i));
        crate::output::push(
            crate::output::CHANNEL_DEBUG_CONSOLE,
            crate::output::OutputLevel::Info,
            &shown.unwrap_or(&line).text,
        );
    }
}

impl App {
    /// Append a line to the debug console, capping the backlog so a chatty
    /// program can't grow it without bound. The line is mirrored to OUTPUT's
    /// "Debug Console" channel (#867), where it reads at full width and can
    /// be searched and copied; that channel keeps its own cap.
    pub(in crate::app) fn debug_console_push(&mut self, line: String) {
        self.debug_console_push_line(crate::ansi_text::AnsiLine::plain(line));
    }

    /// Append an output event of the session at `index` to the debug
    /// console, with its colour sequences turned into spans and every other
    /// escape dropped (#1501). The event is a chunk of that session's
    /// `category` stream, not a line (#932), and is parsed with the stream's
    /// own state: a sequence the event ends in the middle of waits for the
    /// next event, a colour it leaves on carries only into its own lines, and
    /// a line ends at its newline, blank ones included. The line a chunk
    /// leaves open is shown as it reads so far and redrawn in place as later
    /// chunks extend it, so `print: i =` and ` 0` read as one line and a
    /// progress bar's `\r` redraws it rather than piling up copies. `tagged`
    /// puts the session's name in front of each line, for a background member.
    pub(in crate::app) fn debug_console_push_output(
        &mut self,
        index: usize,
        category: &str,
        text: &str,
        tagged: bool,
    ) {
        let Some((_, named)) = self
            .debug_sessions
            .iter_named_mut_indexed()
            .find(|(i, _)| *i == index)
        else {
            let mut alone = crate::ansi_text::AnsiStream::default();
            let mut lines = alone.feed(text);
            lines.extend(alone.finish());
            for line in lines {
                self.debug_console_push_line(line);
            }
            return;
        };
        let prefix = tagged.then(|| format!("[{}] ", named.name));
        let stream = named.console_stream(category);
        let ended = stream.ansi.feed(text);
        let open = stream.ansi.open_line().cloned();
        let mut slot = stream
            .shown_at
            .take()
            .and_then(|at| at.checked_sub(self.debug_console_dropped))
            .filter(|&i| i < self.debug_console.len());
        let tag = |mut line: crate::ansi_text::AnsiLine| {
            if let Some(prefix) = &prefix {
                line.prefix(prefix);
            }
            line
        };
        // The first line the chunk ends is the one shown open, if any:
        // finish it where it stands.
        for line in ended.into_iter().map(tag) {
            match slot.take() {
                Some(i) => {
                    crate::output::push(
                        crate::output::CHANNEL_DEBUG_CONSOLE,
                        crate::output::OutputLevel::Info,
                        &line.text,
                    );
                    self.debug_console[i] = line;
                }
                None => self.debug_console_push_line(line),
            }
        }
        let shown_at = open.map(tag).map(|line| match slot {
            Some(i) => {
                self.debug_console[i] = line;
                self.debug_console_dropped + i
            }
            None => {
                self.debug_console_append(line);
                self.debug_console_dropped + self.debug_console.len() - 1
            }
        });
        if let Some((_, named)) = self
            .debug_sessions
            .iter_named_mut_indexed()
            .find(|(i, _)| *i == index)
        {
            named.console_stream(category).shown_at = shown_at;
        }
    }

    /// Append a finished line to the debug console and mirror it to
    /// OUTPUT's "Debug Console" channel.
    pub(in crate::app) fn debug_console_push_line(&mut self, line: crate::ansi_text::AnsiLine) {
        crate::output::push(
            crate::output::CHANNEL_DEBUG_CONSOLE,
            crate::output::OutputLevel::Info,
            &line.text,
        );
        self.debug_console_append(line);
    }

    /// Append a line to the debug console without mirroring it, capping the
    /// backlog and counting the lines the cap drops off its front.
    pub(in crate::app) fn debug_console_append(&mut self, line: crate::ansi_text::AnsiLine) {
        const CAP: usize = 1000;
        self.debug_console.push(line);
        if self.debug_console.len() > CAP {
            let overflow = self.debug_console.len() - CAP;
            self.debug_console.drain(..overflow);
            self.debug_console_dropped += overflow;
        }
    }

    /// Submit the Run & Debug panel's REPL input: echo + evaluate it at the
    /// selected frame. No-op on empty input or with no session.
    pub(in crate::app) fn debug_repl_submit(&mut self) {
        let expr = self.run_debug.repl_input.trim().to_string();
        self.run_debug.repl_input.clear();
        if expr.is_empty() {
            return;
        }
        match self.debug_sessions.focused_mut() {
            Some(session) => session.evaluate(&expr, "repl"),
            None => self.debug_console_push(String::from("(no debug session)")),
        }
    }

    /// Hover-to-evaluate: while a debug session is paused, return a popup string
    /// for the identifier at `(line, c)` if it's a loaded local/global, else
    /// None (so the normal LSP hover proceeds).
    pub(in crate::app) fn debug_hover_value(&self, line: usize, c: usize) -> Option<String> {
        use crate::dap::session::SessionPhase;
        let session = self.debug_sessions.focused()?;
        if session.phase != SessionPhase::Stopped {
            return None;
        }
        let word = self.editor.word_string_at(line, c)?;
        let var = session.lookup_local(&word)?;
        let ty = if var.type_name.is_empty() {
            String::new()
        } else {
            format!(": {}", var.type_name)
        };
        Some(format!("{} = {}{ty}", var.name, var.value))
    }
}
