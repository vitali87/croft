//! OSC 7501, the program status protocol: a program in a pane reports for
//! itself whether it is idle, working, blocked on the user, done or failed,
//! so croft does not have to guess from the screen. Claude Code (2.1.295
//! and later) and Crush (0.98 and later) send it. Claude Code only starts
//! after the terminal answers its probe, `ESC ] 7501 ; ? ST`, with
//! [`PROBE_REPLY`].
//!
//! [`parse`] reads one sequence's body. [`Records`] is the per-pane store
//! those reports update, with the spec's lifetimes: a new shell prompt drops
//! what was live, and a full reset drops everything. The wire format is
//! revision 0.4 of the spec,
//! <https://www.superlogical.com/rex/docs/build/program-status>.

use base64::Engine;

/// The longest legal sequence, from ESC `]` through its terminator. A
/// longer one is ignored whole.
pub const SEQUENCE_MAX: usize = 4096;

/// What croft answers the probe with: "yes, with the default states and
/// kinds", which the spec fixes as idle, working, done, blocked, error and
/// permission, question, auth.
pub const PROBE_REPLY: &str = "\x1b]7501;?\x1b\\";

/// Most records a pane keeps; the least recently updated goes first.
const RECORDS_MAX: usize = 256;
/// Longest key.
const KEY_MAX: usize = 16;
/// `msg`: longest encoded, then longest decoded.
const MSG_MAX: (usize, usize) = (2732, 2048);
/// `title`: longest encoded, then longest decoded.
const TITLE_MAX: (usize, usize) = (256, 192);
/// `id`: longest whole, longest segment, deepest.
const ID_MAX: (usize, usize, usize) = (128, 32, 8);

/// Base64 with the padding optional, as the spec allows.
const B64: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

/// What a program says it is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// At rest, waiting for input.
    Idle,
    /// Running.
    Working,
    /// Finished, with a result the user has not seen yet.
    Done,
    /// Waiting on the user: a permission, a question, a sign-in.
    Blocked,
    /// Failed and stopped.
    Error,
}

impl State {
    /// How much it needs the user, for picking one state out of several
    /// records: a blocked record outranks everything, then work under way.
    fn urgency(self) -> u8 {
        match self {
            State::Blocked => 4,
            State::Working => 3,
            State::Error => 2,
            State::Done => 1,
            State::Idle => 0,
        }
    }
}

/// One OSC 7501 sequence, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The probe: the program asks whether this terminal speaks 7501.
    Query,
    /// Set the record at `id` (empty for the root record).
    Set { id: String, state: State },
    /// Remove the record at `id` and every record beneath it; with no `id`,
    /// every record.
    Clear { id: Option<String> },
}

/// Read one sequence's body (between `7501;` and the terminator). `None`
/// when the spec says to ignore it: no state or one croft does not know, a
/// bad `id`, or `msg`/`title` text that is too long, not base64, or holds a
/// control character. A malformed pair or an unknown key is skipped.
pub fn parse(body: &[u8]) -> Option<Report> {
    if body.trim_ascii() == b"?" {
        return Some(Report::Query);
    }
    let (mut state, mut id, mut msg, mut title) = (None, None, None, None);
    for pair in body.split(|&b| b == b':') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let (key, value) = (pair[..eq].trim_ascii(), pair[eq + 1..].trim_ascii());
        if key.is_empty() || !key.iter().all(u8::is_ascii_lowercase) {
            continue;
        }
        if key.len() > KEY_MAX {
            return None;
        }
        // A repeated key: the last one counts.
        match key {
            b"state" => state = Some(value),
            b"id" => id = Some(value),
            b"msg" => msg = Some(value),
            b"title" => title = Some(value),
            _ => {}
        }
    }
    // Every limit is checked before anything is stored.
    if let Some(m) = msg {
        text_ok(m, MSG_MAX)?;
    }
    if let Some(t) = title {
        text_ok(t, TITLE_MAX)?;
    }
    // A bad id ignores the report rather than landing on the root.
    let id = match id {
        Some(id) => Some(id_ok(id)?.to_string()),
        None => None,
    };
    let state = match state? {
        b"idle" => State::Idle,
        b"working" => State::Working,
        b"done" => State::Done,
        b"blocked" => State::Blocked,
        b"error" => State::Error,
        b"clear" => return Some(Report::Clear { id }),
        _ => return None,
    };
    Some(Report::Set {
        id: id.unwrap_or_default(),
        state,
    })
}

/// `msg` and `title` are base64 UTF-8 on one line, with no control
/// characters; the encoded length is checked before decoding.
fn text_ok(value: &[u8], (encoded, decoded): (usize, usize)) -> Option<()> {
    if value.len() > encoded {
        return None;
    }
    let raw = B64.decode(value).ok()?;
    let text = std::str::from_utf8(&raw).ok()?;
    (raw.len() <= decoded && !text.chars().any(char::is_control)).then_some(())
}

/// An id is `/`-separated segments of letters, digits and `_ . + -`.
fn id_ok(id: &[u8]) -> Option<&str> {
    let (whole, segment, depth) = ID_MAX;
    let ok = id.len() <= whole
        && id.split(|&b| b == b'/').count() <= depth
        && id.split(|&b| b == b'/').all(|s| {
            (1..=segment).contains(&s.len())
                && s.iter()
                    .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'))
        });
    ok.then(|| std::str::from_utf8(id).ok()).flatten()
}

/// One pane's records, one per id, as the reports left them.
#[derive(Debug, Default)]
pub struct Records {
    /// `(id, state, the report count when it was last set)`.
    entries: Vec<(String, State, u64)>,
    /// Reports taken so far, clears included.
    seq: u64,
}

impl Records {
    /// Take one report. A query changes nothing.
    pub fn apply(&mut self, report: Report) {
        match report {
            Report::Query => {}
            Report::Set { id, state } => {
                self.seq += 1;
                let seq = self.seq;
                if let Some(e) = self.entries.iter_mut().find(|e| e.0 == id) {
                    *e = (id, state, seq);
                    return;
                }
                if self.entries.len() >= RECORDS_MAX
                    && let Some(oldest) = (0..self.entries.len()).min_by_key(|&i| self.entries[i].2)
                {
                    self.entries.remove(oldest);
                }
                self.entries.push((id, state, seq));
            }
            Report::Clear { id } => {
                self.seq += 1;
                match id {
                    None => self.entries.clear(),
                    Some(id) => self.entries.retain(|(e, ..)| {
                        !(e == &id
                            || e.strip_prefix(id.as_str())
                                .is_some_and(|r| r.starts_with('/')))
                    }),
                }
            }
        }
    }

    /// A new shell prompt: the program that reported is gone, so what it
    /// was in the middle of goes too. A result the user has not seen
    /// (`done`, `error`) stays.
    pub fn shell_prompt(&mut self) {
        self.entries
            .retain(|(_, s, _)| matches!(s, State::Done | State::Error));
    }

    /// A full reset (RIS) removes every record.
    pub fn reset(&mut self) {
        self.entries.clear();
    }

    /// How many reports have been taken: a mark to read
    /// [`Self::reported_since`] from.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The program's own status from the reports after `mark`, the most
    /// pressing state among the records they set. `None` when it has said
    /// nothing since, so its status has to come from somewhere else. One
    /// that reported and then cleared its records (or a prompt dropped
    /// them) is at rest: [`State::Idle`].
    pub fn reported_since(&self, mark: u64) -> Option<State> {
        (self.seq > mark).then(|| {
            self.entries
                .iter()
                .filter(|(_, _, seq)| *seq > mark)
                .map(|(_, s, _)| *s)
                .max_by_key(|s| s.urgency())
                .unwrap_or(State::Idle)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(id: &str, state: State) -> Option<Report> {
        Some(Report::Set {
            id: id.to_string(),
            state,
        })
    }

    #[test]
    fn reports_read_the_state_the_id_and_the_probe() {
        // The spec's own example: msg is base64 for "Apply 3 to add, 1 to
        // change, 0 to destroy?".
        assert_eq!(
            parse(
                b"state=blocked:kind=permission:app=terraform:msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/"
            ),
            set("", State::Blocked)
        );
        assert_eq!(parse(b"state=working:progress=40"), set("", State::Working));
        assert_eq!(
            parse(b"state=done:id=agent/sub-1"),
            set("agent/sub-1", State::Done)
        );
        assert_eq!(parse(b" state = idle "), set("", State::Idle), "trimmed");
        assert_eq!(
            parse(b"state=idle:state=error"),
            set("", State::Error),
            "last wins"
        );
        assert_eq!(parse(b"?"), Some(Report::Query));
        assert_eq!(parse(b"state=clear"), Some(Report::Clear { id: None }));
        assert_eq!(
            parse(b"state=clear:id=a/b"),
            Some(Report::Clear {
                id: Some("a/b".into())
            })
        );
        // Skipped, not fatal: a pair with no `=`, a key that is not
        // lowercase letters, an unknown key, an unknown kind or progress.
        assert_eq!(
            parse(b"junk:Bad=1:x1=2:future=yes:kind=nope:progress=900:state=working"),
            set("", State::Working)
        );
    }

    #[test]
    fn a_report_the_spec_says_to_ignore_is_none() {
        for body in [
            &b""[..],
            b"kind=permission",
            b"state=sleeping",
            b"state=WORKING",
            // A bad id never falls back to the root record.
            b"state=working:id=",
            b"state=working:id=a//b",
            b"state=working:id=has space",
            b"state=working:id=a/b/c/d/e/f/g/h/i",
            b"state=working:msg=not base64!",
            // Base64 for "two\nlines": a control character.
            b"state=working:msg=dHdvCmxpbmVz",
            b"state=working:averyveryverylongkey=1",
        ] {
            assert_eq!(parse(body), None, "{}", String::from_utf8_lossy(body));
        }
        let segment = "a".repeat(33);
        assert_eq!(parse(format!("state=idle:id={segment}").as_bytes()), None);
        let long = B64.encode("x".repeat(2049));
        assert_eq!(parse(format!("state=idle:msg={long}").as_bytes()), None);
        let fits = B64.encode("x".repeat(2048));
        assert_eq!(
            parse(format!("state=idle:msg={fits}").as_bytes()),
            set("", State::Idle)
        );
        let title = B64.encode("x".repeat(193));
        assert_eq!(parse(format!("state=idle:title={title}").as_bytes()), None);
    }

    #[test]
    fn records_keep_one_per_id_and_clear_a_subtree() {
        let mut r = Records::default();
        for (id, state) in [
            ("", State::Idle),
            ("a", State::Working),
            ("a/b", State::Blocked),
            ("ab", State::Working),
        ] {
            r.apply(Report::Set {
                id: id.into(),
                state,
            });
        }
        assert_eq!(r.reported_since(0), Some(State::Blocked));
        r.apply(Report::Clear {
            id: Some("a".into()),
        });
        // `ab` is not beneath `a`.
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.reported_since(0), Some(State::Working));
        r.apply(Report::Set {
            id: "ab".into(),
            state: State::Done,
        });
        assert_eq!(r.entries.len(), 2, "the id's record was replaced");
        assert_eq!(r.reported_since(0), Some(State::Done));
        r.apply(Report::Clear { id: None });
        assert!(r.entries.is_empty());
        assert_eq!(r.reported_since(0), Some(State::Idle), "said, then cleared");
        r.apply(Report::Query);
        assert_eq!(r.seq(), 7, "a query is not a report");
    }

    #[test]
    fn a_prompt_drops_live_records_and_a_reset_drops_all() {
        let mut r = Records::default();
        for (id, state) in [
            ("w", State::Working),
            ("b", State::Blocked),
            ("i", State::Idle),
            ("d", State::Done),
            ("e", State::Error),
        ] {
            r.apply(Report::Set {
                id: id.into(),
                state,
            });
        }
        r.shell_prompt();
        let left: Vec<&str> = r.entries.iter().map(|(id, ..)| id.as_str()).collect();
        assert_eq!(left, ["d", "e"]);
        r.reset();
        assert!(r.entries.is_empty());
    }

    #[test]
    fn reported_since_reads_only_what_came_after_the_mark() {
        let mut r = Records::default();
        assert_eq!(r.reported_since(0), None, "nothing said yet");
        r.apply(Report::Set {
            id: String::new(),
            state: State::Done,
        });
        let mark = r.seq();
        assert_eq!(r.reported_since(mark), None, "an earlier program's done");
        r.apply(Report::Set {
            id: "x".into(),
            state: State::Working,
        });
        assert_eq!(r.reported_since(mark), Some(State::Working));
    }

    #[test]
    fn a_full_store_evicts_the_least_recently_updated() {
        let mut r = Records::default();
        for i in 0..RECORDS_MAX {
            r.apply(Report::Set {
                id: format!("r{i}"),
                state: State::Idle,
            });
        }
        // Touch the oldest, so the second oldest is the one to go.
        r.apply(Report::Set {
            id: "r0".into(),
            state: State::Working,
        });
        r.apply(Report::Set {
            id: "new".into(),
            state: State::Idle,
        });
        assert_eq!(r.entries.len(), RECORDS_MAX);
        assert!(r.entries.iter().any(|(id, ..)| id == "r0"));
        assert!(!r.entries.iter().any(|(id, ..)| id == "r1"));
    }
}
