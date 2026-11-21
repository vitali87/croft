//! Spotting Cmd+K Shift+Q (Session: Detach) in a client's own keystrokes, so any client
//! can leave, one without write control included.

use super::*;

/// `Cmd+K Shift+Q` (Session: Detach, #679) spotted in a client's own
/// keystrokes, so a client can always leave: a read-only participant's
/// input never reaches the inner croft, and a wedged inner croft reads
/// nothing at all. Filtered here, before the bytes are forwarded.
///
/// The `Cmd+K` leader is WITHHELD until the next key decides it: forwarded
/// at once, it would arm the inner croft's chord leader, and the next key
/// ANY participant typed would complete a chord this client started. A
/// leader followed by anything else is forwarded unchanged, and one left
/// alone is released by [`Self::flush`] after [`DETACH_CHORD_HOLD`].
///
/// `Cmd` reaches the client only as a kitty-protocol key event (CSI 107
/// with the super bit, which croft's keyboard flags turn on, or iTerm2's
/// mapping that sends the same bytes), so Ctrl+K, the editor's kill-line,
/// is never taken.
#[derive(Default)]
pub(super) struct DetachChord {
    /// The withheld leader, plus any release/repeat events of it.
    leader: Vec<u8>,
    /// An escape sequence cut off at the end of a read, completed by the
    /// next one.
    partial: Vec<u8>,
}

/// How long a lone `Cmd+K` is withheld before it goes to the inner croft
/// anyway, so its chord hint still shows for a user who pauses.
pub(super) const DETACH_CHORD_HOLD: Duration = Duration::from_millis(1000);

impl DetachChord {
    /// Filter one read of stdin: the bytes to forward, and whether the
    /// detach chord was typed (everything from it on is dropped).
    pub(super) fn feed(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(input);
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            let Some(len) = token_len(&data[i..]) else {
                // Only what could still become the chord is withheld: a
                // lone ESC is the Esc key, and must never wait for a key
                // that may not come.
                if may_become_chord_key(&data[i..]) {
                    self.partial = data[i..].to_vec();
                } else {
                    out.append(&mut self.leader);
                    out.extend_from_slice(&data[i..]);
                }
                break;
            };
            let token = &data[i..i + len];
            i += len;
            if !self.leader.is_empty() {
                match kitty_key(token) {
                    Some((107, mods, event)) if event != 1 && is_super_only(mods) => {
                        self.leader.extend_from_slice(token);
                        continue;
                    }
                    _ if is_detach_key(token) => {
                        self.leader.clear();
                        return (out, true);
                    }
                    _ => out.append(&mut self.leader),
                }
            }
            if matches!(kitty_key(token), Some((107, mods, 1)) if is_super_only(mods)) {
                self.leader = token.to_vec();
            } else {
                out.extend_from_slice(token);
            }
        }
        (out, false)
    }

    /// True while bytes are withheld waiting for the next key.
    pub(super) fn holding(&self) -> bool {
        !self.leader.is_empty() || !self.partial.is_empty()
    }

    /// Release whatever is withheld (the pause after a lone leader).
    pub(super) fn flush(&mut self) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.leader);
        out.append(&mut self.partial);
        out
    }
}

/// Whether a CSI cut off at the end of a read could still turn out to be
/// the chord's leader or second key (`CSI 107;`, `CSI 113;`, `CSI 81;`).
pub(super) fn may_become_chord_key(partial: &[u8]) -> bool {
    let Some(body) = partial.strip_prefix(b"\x1b[") else {
        return false;
    };
    [&b"107;"[..], b"113;", b"81;"].iter().any(|code| {
        if body.len() <= code.len() {
            code.starts_with(body)
        } else {
            body.starts_with(code)
                && body[code.len()..]
                    .iter()
                    .all(|b| b.is_ascii_digit() || *b == b':' || *b == b';')
        }
    })
}

/// Length of the input token at the start of `data`: a whole CSI sequence,
/// an ESC-prefixed key, or one byte. `None` when a CSI is cut off.
pub(super) fn token_len(data: &[u8]) -> Option<usize> {
    if data[0] != 0x1b {
        return Some(1);
    }
    match data.get(1) {
        None => None,
        Some(b'[') => data[2..]
            .iter()
            .position(|b| (0x40..=0x7e).contains(b))
            .map(|p| p + 3),
        Some(_) => Some(2),
    }
}

/// A kitty-protocol key event `CSI code ; mods [: event] u` as
/// `(code, mods, event)`, with `mods` and `event` defaulting to 1.
pub(super) fn kitty_key(token: &[u8]) -> Option<(u32, u32, u32)> {
    let body = token.strip_prefix(b"\x1b[")?.strip_suffix(b"u")?;
    let body = std::str::from_utf8(body).ok()?;
    let mut fields = body.split(';');
    let code = fields.next()?.split(':').next()?.parse().ok()?;
    let (mods, event) = match fields.next() {
        None => (1, 1),
        Some(m) => {
            let mut parts = m.split(':');
            let mods = parts.next()?.parse().ok()?;
            let event = parts.next().map_or(Some(1), |e| e.parse().ok())?;
            (mods, event)
        }
    };
    Some((code, mods, event))
}

/// The kitty modifier bits held, ignoring Caps Lock and Num Lock.
pub(super) fn held_mods(mods: u32) -> u32 {
    mods.saturating_sub(1) & !(64 | 128)
}

pub(super) fn is_super_only(mods: u32) -> bool {
    held_mods(mods) == 8
}

/// The chord's second key: `Q`, as plain text or as a kitty key event with
/// Shift (and optionally Cmd still held).
pub(super) fn is_detach_key(token: &[u8]) -> bool {
    if token == b"Q" {
        return true;
    }
    matches!(
        kitty_key(token),
        Some((113 | 81, mods, 1)) if matches!(held_mods(mods), 1 | 9)
    )
}
