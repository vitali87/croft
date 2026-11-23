//! The debug sessions in flight: each one's name, Debug Console streams and background
//! backlog, and which one is focused.

use super::*;

/// A session and the name it was launched under.
///
/// `DapSession` itself has no name: it is built by four different adapter
/// constructors and none of them takes one. With a single session the UI
/// could say "the debugger" and be unambiguous; with several it has to say
/// WHICH, so the name rides alongside rather than being threaded through
/// every constructor (#310).
pub struct NamedSession<S = DapSession> {
    pub name: String,
    pub session: S,
    /// Events this session produced while it was NOT focused, kept for the
    /// moment it is (#567). Only the focused session's events drive the UI;
    /// dropping a background member's `stopped` left a breakpoint hit that
    /// nothing showed, so the member looked hung. Replayed in order on focus.
    pub backlog: Vec<DapEvent>,
    /// This session's Debug Console streams, one per output category: its
    /// own (#1501), so a sibling's colour or cut escape never runs into its
    /// output, and one per category (#932), so a `stderr` line or a progress
    /// bar never lands in the middle of a `stdout` line still being printed.
    pub console: Vec<ConsoleStream>,
}

impl<S> NamedSession<S> {
    /// The console stream for output `category`, started on its first use.
    pub fn console_stream(&mut self, category: &str) -> &mut ConsoleStream {
        let at = match self.console.iter().position(|c| c.category == category) {
            Some(at) => at,
            None => {
                self.console.push(ConsoleStream {
                    category: category.to_string(),
                    ..ConsoleStream::default()
                });
                self.console.len() - 1
            }
        };
        &mut self.console[at]
    }
}

/// One output category of a session, as the Debug Console reads it (#932).
#[derive(Default)]
pub struct ConsoleStream {
    pub category: String,
    pub ansi: crate::ansi_text::AnsiStream,
    /// Where the stream's open line is shown in the Debug Console, counted
    /// from the first line the console ever held so that lines dropped off
    /// its front do not move it. The line is redrawn there as later chunks
    /// add to it, and `None` means none is shown.
    pub shown_at: Option<usize>,
}

/// The debug sessions in flight, one of them FOCUSED (#310).
///
/// croft was single-session by construction: `Option<DapSession>`, and every
/// site that touched it meant "the session" because there could only be one.
/// A compound needs several, so the UI reads the FOCUSED one - call stack,
/// variables, stepping, status - while the protocol operations mean all of
/// them: every adapter is polled, every session is stopped, every session
/// takes a breakpoint.
///
/// Generic over the session type ONLY so the set's own rules - which session
/// is focused after a push, a replace, a removal - can be tested without an
/// adapter to talk to. Production always uses the default.
pub struct DebugSessions<S = DapSession> {
    sessions: Vec<NamedSession<S>>,
    /// Index into `sessions`. Meaningless when empty, and never left dangling:
    /// every mutation that can shrink the set clamps it.
    focused: usize,
}

// Hand-written rather than derived: a derive would demand `S: Default`, and
// a session has nothing sensible to be a default OF. An empty set does.
impl<S> Default for DebugSessions<S> {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            focused: 0,
        }
    }
}

impl<S> DebugSessions<S> {
    /// Whether nothing is being debugged at all.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// How many sessions are in flight.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// The session the UI is looking at: the call stack, the variables view,
    /// the stepping commands and the status line all mean this one.
    pub fn focused(&self) -> Option<&S> {
        self.sessions.get(self.focused).map(|s| &s.session)
    }

    /// The focused session's name, for a status line that has to say which.
    pub fn focused_name(&self) -> Option<&str> {
        self.sessions.get(self.focused).map(|s| s.name.as_str())
    }

    /// Mutable counterpart of [`focused`](Self::focused).
    pub fn focused_mut(&mut self) -> Option<&mut S> {
        self.sessions.get_mut(self.focused).map(|s| &mut s.session)
    }

    /// Replace the whole set with one session and focus it.
    ///
    /// What a single-configuration launch does, and it is deliberately not
    /// `push`: starting a configuration has always terminated whatever was
    /// running, and this slice does not change that. The caller disconnects
    /// the old ones first; this only drops them.
    pub fn replace_with(&mut self, name: impl Into<String>, session: S) {
        self.sessions.clear();
        self.sessions.push(NamedSession {
            name: name.into(),
            session,
            backlog: Vec::new(),
            console: Vec::new(),
        });
        self.focused = 0;
    }

    /// Add a session and focus it, leaving the others running.
    ///
    /// What a COMPOUND does, and the difference from
    /// [`replace_with`](Self::replace_with) is the whole of #310: a
    /// configuration launch has always terminated whatever was running, and
    /// a compound must not, or its second member would kill its first.
    pub fn push(&mut self, name: impl Into<String>, session: S) {
        self.sessions.push(NamedSession {
            name: name.into(),
            session,
            backlog: Vec::new(),
            console: Vec::new(),
        });
        self.focused = self.sessions.len() - 1;
    }

    /// Focus the session at `index`, if it exists.
    pub fn focus(&mut self, index: usize) -> bool {
        if index < self.sessions.len() {
            self.focused = index;
            true
        } else {
            false
        }
    }

    /// Every session's name, in launch order, for a picker or a status line.
    pub fn names(&self) -> Vec<String> {
        self.sessions.iter().map(|s| s.name.clone()).collect()
    }

    /// Take the session at `index` out of the set, clamping the focus.
    ///
    /// The caller disconnects it; this only removes it. One member of a
    /// compound ending must NOT take the others with it - dropping the whole
    /// set reaches the siblings as a `SIGKILL` through `DapTransport`'s drop,
    /// which is precisely the graceful handshake js-debug needs to retire its
    /// detached watchdog (#310).
    pub fn remove(&mut self, index: usize) -> Option<NamedSession<S>> {
        if index >= self.sessions.len() {
            return None;
        }
        let gone = self.sessions.remove(index);
        // Keep the focus on a session that exists, and prefer to stay where
        // the user was looking. The rule is positional, not identity-based:
        // removing a session BEFORE the focused one shifts the index down so
        // the same session stays focused, and removing the focused one itself
        // leaves the index where it was - which lands on the session that
        // moved up into that slot, or steps back when it was the last. So
        // retiring the middle of [A, B, C] while watching B leaves the user
        // on C, the next live session, rather than jumping back to A.
        if self.focused > index || self.focused >= self.sessions.len() {
            self.focused = self.focused.saturating_sub(1);
        }
        Some(gone)
    }

    /// Which session the UI is looking at, by index.
    pub fn focused_index(&self) -> usize {
        self.focused
    }

    /// Every session with its position and name, for the poll that routes a
    /// background member's events by who produced them (#567).
    pub fn iter_named_mut_indexed(
        &mut self,
    ) -> impl Iterator<Item = (usize, &mut NamedSession<S>)> {
        self.sessions.iter_mut().enumerate()
    }

    /// The focused session's queued background events, emptied: what it did
    /// while the user was looking at another member, to replay now.
    pub fn take_focused_backlog(&mut self) -> Vec<DapEvent> {
        self.sessions
            .get_mut(self.focused)
            .map(|s| std::mem::take(&mut s.backlog))
            .unwrap_or_default()
    }

    /// Drop every session without disconnecting - the old `= None`.
    pub fn clear(&mut self) {
        self.sessions.clear();
        self.focused = 0;
    }

    /// Every session, for the operations that mean all of them: the poll that
    /// drains each adapter, and the stop that must not orphan the rest.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut S> {
        self.sessions.iter_mut().map(|s| &mut s.session)
    }
}
