//! Shell-integration escape sequences: the OSC 133 / 7 / 9 sniffer and the
//! shell hooks that emit them.
//!
//! Modern terminals (VS Code, Ghostty, iTerm2, WezTerm, Kitty) build their
//! command-aware features on FinalTerm's OSC 133 semantic prompt marks:
//! `133;A` prompt start, `133;B` prompt end, `133;C` command output start,
//! `133;D;<exit>` command finished. OSC 7 reports the shell's cwd, OSC 9
//! carries a desktop-notification payload.
//!
//! `alacritty_terminal` drops unknown OSC sequences before they reach any
//! handler (they are parsed and logged internally), so croft cannot observe
//! them through the grid. Instead the PTY reader thread tees each raw byte
//! chunk through [`OscSniffer::scan`] *before* advancing the parser — the
//! same pattern as the port sniffer and the bracketed-paste sniffer. The
//! returned chunk offsets let the reader split its `Processor::advance`
//! calls at each mark so the grid cursor can be sampled exactly where the
//! mark landed.
//!
//! The shell only emits these sequences when integration hooks are
//! installed. [`ensure_zsh_shim`] materialises a `ZDOTDIR` shim (the VS Code
//! / Ghostty approach): zsh reads croft's dotfiles, which source the user's
//! real ones and then add `precmd`/`preexec` hooks that print the marks.

use std::io::Write;
use std::path::PathBuf;

/// A shell-integration event extracted from the PTY byte stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OscEvent {
    /// OSC 133;A — the shell is about to draw its prompt.
    PromptStart,
    /// OSC 133;B — the prompt is done; user input starts here.
    PromptEnd,
    /// OSC 133;C — the command was accepted and its output starts.
    CommandStart,
    /// OSC 133;D;<exit> — the command finished. `None` when the shell
    /// omitted the exit code.
    CommandEnd(Option<i32>),
    /// OSC 7;file://host/path — the shell's current working directory.
    Cwd(PathBuf),
    /// OSC 9;<message> — a notification payload.
    Notify(String),
}

/// Incremental scanner for the OSC sequences croft cares about. Feed it
/// every raw PTY chunk in order; it carries partial sequences across chunk
/// boundaries. Unrelated bytes and unrelated escape sequences are ignored.
#[derive(Default)]
pub struct OscSniffer {
    /// Bytes of an OSC sequence that started near the end of the previous
    /// chunk but has not terminated yet (capped, see [`CARRY_MAX`]).
    carry: Vec<u8>,
}

/// A sniffed event plus the offset *just past* its terminator, in the
/// coordinates of the chunk passed to `scan`. The reader thread advances
/// the VT parser up to `end` before recording the event, so the grid
/// cursor is sampled exactly where the mark landed.
pub type SniffedEvent = (usize, OscEvent);

/// Longest partial sequence carried across chunks before giving up (a
/// legitimate OSC 133/7/9 is far shorter; this bounds a hostile stream).
const CARRY_MAX: usize = 4096;

/// Which of croft's OSC numbers a sequence carries.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OscKind {
    SemanticPrompt, // 133
    Cwd,            // 7
    Notify,         // 9
}

/// What the bytes starting at an ESC look like.
enum Classify {
    /// Not a sequence croft tracks; resume scanning after the ESC.
    No,
    /// Could still become one of ours; the chunk ended mid-prefix.
    NeedMore,
    /// One of ours; the parameter body starts at this offset past the ESC.
    Ours(OscKind, usize),
}

/// How a candidate sequence ended.
enum Terminator {
    /// Body ends at `.0`, the full sequence ends at `.1` (both past-ESC).
    Done(usize, usize),
    /// No terminator in the available bytes; carry and wait for more.
    NeedMore,
    /// An ESC not followed by `\` aborts an OSC; rescan from that offset.
    Abort(usize),
}

const OSC_PREFIXES: &[(&[u8], OscKind)] = &[
    (b"]133;", OscKind::SemanticPrompt),
    (b"]7;", OscKind::Cwd),
    (b"]9;", OscKind::Notify),
];

fn classify(bytes: &[u8]) -> Classify {
    // bytes[0] is the ESC.
    for (prefix, kind) in OSC_PREFIXES {
        let avail = &bytes[1..];
        let n = avail.len().min(prefix.len());
        if avail[..n] == prefix[..n] {
            if n < prefix.len() {
                return Classify::NeedMore;
            }
            return Classify::Ours(*kind, 1 + prefix.len());
        }
    }
    Classify::No
}

/// Find BEL or ST past the body start. An ESC that isn't part of ST aborts.
fn find_terminator(bytes: &[u8], body_start: usize) -> Terminator {
    let mut i = body_start;
    while i < bytes.len() {
        match bytes[i] {
            0x07 => return Terminator::Done(i, i + 1),
            0x1b => {
                if i + 1 >= bytes.len() {
                    return Terminator::NeedMore;
                }
                if bytes[i + 1] == b'\\' {
                    return Terminator::Done(i, i + 2);
                }
                return Terminator::Abort(i);
            }
            _ => i += 1,
        }
    }
    Terminator::NeedMore
}

/// Decode `%XX` escapes (OSC 7 file URLs percent-encode special chars).
fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn parse_event(kind: OscKind, body: &[u8]) -> Option<OscEvent> {
    match kind {
        OscKind::SemanticPrompt => match body.first()? {
            b'A' => Some(OscEvent::PromptStart),
            b'B' => Some(OscEvent::PromptEnd),
            b'C' => Some(OscEvent::CommandStart),
            b'D' => {
                let exit = (body.len() > 2 && body[1] == b';')
                    .then(|| std::str::from_utf8(&body[2..]).ok()?.parse::<i32>().ok())
                    .flatten();
                Some(OscEvent::CommandEnd(exit))
            }
            _ => None,
        },
        OscKind::Cwd => {
            // file://host/path — strip the scheme and authority; a bare
            // absolute path (some shells emit that) passes through.
            let path = if let Some(rest) = body.strip_prefix(b"file://") {
                let slash = rest.iter().position(|&b| b == b'/')?;
                &rest[slash..]
            } else if body.first() == Some(&b'/') {
                body
            } else {
                return None;
            };
            let decoded = percent_decode(path);
            Some(OscEvent::Cwd(PathBuf::from(
                String::from_utf8_lossy(&decoded).into_owned(),
            )))
        }
        OscKind::Notify => Some(OscEvent::Notify(String::from_utf8_lossy(body).into_owned())),
    }
}

impl OscSniffer {
    pub fn scan(&mut self, chunk: &[u8]) -> Vec<SniffedEvent> {
        let mut out = Vec::new();
        // Prepend any carried partial sequence; `base` converts combined
        // offsets back into chunk coordinates.
        let owned: Vec<u8>;
        let (buf, base) = if self.carry.is_empty() {
            (chunk, 0usize)
        } else {
            let mut v = std::mem::take(&mut self.carry);
            v.extend_from_slice(chunk);
            owned = v;
            let b = owned.len() - chunk.len();
            (owned.as_slice(), b)
        };
        let mut i = 0;
        while i < buf.len() {
            let Some(rel) = buf[i..].iter().position(|&b| b == 0x1b) else {
                break;
            };
            let esc = i + rel;
            match classify(&buf[esc..]) {
                Classify::No => i = esc + 1,
                Classify::NeedMore => {
                    self.hold(&buf[esc..]);
                    return out;
                }
                Classify::Ours(kind, body_start) => {
                    match find_terminator(&buf[esc..], body_start) {
                        Terminator::Done(body_end, seq_end) => {
                            if let Some(ev) =
                                parse_event(kind, &buf[esc + body_start..esc + body_end])
                            {
                                out.push((esc + seq_end - base, ev));
                            }
                            i = esc + seq_end;
                        }
                        Terminator::NeedMore => {
                            self.hold(&buf[esc..]);
                            return out;
                        }
                        Terminator::Abort(off) => i = esc + off,
                    }
                }
            }
        }
        out
    }

    /// Carry a partial sequence to the next chunk, abandoning runaway ones.
    fn hold(&mut self, partial: &[u8]) {
        if partial.len() > CARRY_MAX {
            self.carry.clear();
        } else {
            self.carry = partial.to_vec();
        }
    }
}

/// The zsh hook + bootstrap files, written under
/// `<config>/shell-integration/zsh/`. zsh reads `$ZDOTDIR/.zshenv`,
/// `.zprofile` (login), `.zshrc` (interactive), `.zlogin` (login) — each
/// shim file sources the user's real counterpart from
/// `$CROFT_USER_ZDOTDIR` so their setup runs unchanged, then `.zshrc`
/// installs `precmd`/`preexec` hooks emitting OSC 133 marks and the OSC 7
/// cwd report.
const ZSH_SHIM_ZSHENV: &str = r#"# croft shell integration bootstrap (auto-generated; do not edit)
_croft_shim_zdotdir="$ZDOTDIR"
ZDOTDIR="${CROFT_USER_ZDOTDIR:-$HOME}"
[[ -f "$ZDOTDIR/.zshenv" ]] && builtin source "$ZDOTDIR/.zshenv"
ZDOTDIR="$_croft_shim_zdotdir"
"#;

const ZSH_SHIM_ZPROFILE: &str = r#"# croft shell integration (auto-generated; do not edit)
_croft_user="${CROFT_USER_ZDOTDIR:-$HOME}"
[[ -f "$_croft_user/.zprofile" ]] && ZDOTDIR="$_croft_user" builtin source "$_croft_user/.zprofile"
"#;

const ZSH_SHIM_ZLOGIN: &str = r#"# croft shell integration (auto-generated; do not edit)
_croft_user="${CROFT_USER_ZDOTDIR:-$HOME}"
[[ -f "$_croft_user/.zlogin" ]] && ZDOTDIR="$_croft_user" builtin source "$_croft_user/.zlogin"
"#;

const ZSH_SHIM_ZSHRC: &str = r#"# croft shell integration (auto-generated; do not edit)
_croft_user="${CROFT_USER_ZDOTDIR:-$HOME}"
[[ -f "$_croft_user/.zshrc" ]] && ZDOTDIR="$_croft_user" builtin source "$_croft_user/.zshrc"

# FinalTerm / OSC 133 semantic prompt marks + OSC 7 cwd, the protocol VS
# Code, Ghostty, iTerm2 and WezTerm build command navigation on.
if [[ -o interactive ]] && (( ! ${+_croft_si_installed} )); then
  typeset -g _croft_si_installed=1
  builtin autoload -Uz add-zsh-hook
  _croft_preexec() {
    typeset -g _croft_cmd_inflight=1
    builtin printf '\033]133;C\007'
  }
  _croft_precmd() {
    local _croft_ec=$?
    if (( ${+_croft_cmd_inflight} )); then
      builtin unset _croft_cmd_inflight
      builtin printf '\033]133;D;%d\007' "$_croft_ec"
    fi
    builtin printf '\033]7;file://%s%s\007' "${HOST:-localhost}" "$PWD"
    builtin printf '\033]133;A\007'
  }
  add-zsh-hook preexec _croft_preexec
  add-zsh-hook precmd _croft_precmd
fi

# Restore the user's ZDOTDIR so nested shells and tools see their own.
if [[ -n "$CROFT_USER_ZDOTDIR" && "$CROFT_USER_ZDOTDIR" != "$HOME" ]]; then
  ZDOTDIR="$CROFT_USER_ZDOTDIR"
else
  builtin unset ZDOTDIR
fi
"#;

/// Write croft's zsh `ZDOTDIR` shim (idempotently) and return its directory.
pub fn ensure_zsh_shim(config_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let dir = config_dir.join("shell-integration").join("zsh");
    std::fs::create_dir_all(&dir)?;
    for (name, content) in [
        (".zshenv", ZSH_SHIM_ZSHENV),
        (".zprofile", ZSH_SHIM_ZPROFILE),
        (".zshrc", ZSH_SHIM_ZSHRC),
        (".zlogin", ZSH_SHIM_ZLOGIN),
    ] {
        let path = dir.join(name);
        // Rewrite only on content drift so the mtime stays stable.
        if std::fs::read_to_string(&path).ok().as_deref() != Some(content) {
            let mut f = std::fs::File::create(&path)?;
            f.write_all(content.as_bytes())?;
        }
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(sniffer: &mut OscSniffer, chunks: &[&[u8]]) -> Vec<OscEvent> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend(sniffer.scan(c).into_iter().map(|(_, e)| e));
        }
        out
    }

    #[test]
    fn sniffs_the_four_osc133_marks_with_both_terminators() {
        let mut s = OscSniffer::default();
        // BEL-terminated and ST (ESC \) terminated forms both count.
        let events = scan_all(
            &mut s,
            &[b"\x1b]133;A\x07out\x1b]133;B\x1b\\\x1b]133;C\x07\x1b]133;D;1\x07"],
        );
        assert_eq!(
            events,
            vec![
                OscEvent::PromptStart,
                OscEvent::PromptEnd,
                OscEvent::CommandStart,
                OscEvent::CommandEnd(Some(1)),
            ]
        );
    }

    #[test]
    fn command_end_without_exit_code_yields_none() {
        let mut s = OscSniffer::default();
        let events = scan_all(&mut s, &[b"\x1b]133;D\x07"]);
        assert_eq!(events, vec![OscEvent::CommandEnd(None)]);
    }

    #[test]
    fn end_offsets_point_just_past_each_terminator() {
        let mut s = OscSniffer::default();
        let chunk = b"ab\x1b]133;A\x07cd";
        let got = s.scan(chunk);
        assert_eq!(got, vec![(10, OscEvent::PromptStart)]);
        // Splitting the chunk at 10 leaves "cd" for the second advance.
        assert_eq!(&chunk[10..], b"cd");
    }

    #[test]
    fn sequences_split_across_chunks_are_reassembled() {
        let mut s = OscSniffer::default();
        // Split inside the prefix, inside the body, and before the terminator.
        assert!(s.scan(b"foo\x1b]13").is_empty());
        assert!(s.scan(b"3;D;12").is_empty());
        let got = s.scan(b"7\x07bar");
        assert_eq!(got, vec![(2, OscEvent::CommandEnd(Some(127)))]);
    }

    #[test]
    fn a_lone_trailing_escape_is_carried_not_dropped() {
        let mut s = OscSniffer::default();
        assert!(s.scan(b"output\x1b").is_empty());
        let got = s.scan(b"]133;A\x07");
        assert_eq!(got, vec![(7, OscEvent::PromptStart)]);
    }

    #[test]
    fn osc7_cwd_is_decoded_from_the_file_url() {
        let mut s = OscSniffer::default();
        let events = scan_all(
            &mut s,
            &[b"\x1b]7;file://mac.local/Users/me/dir%20with%20space\x07"],
        );
        assert_eq!(
            events,
            vec![OscEvent::Cwd(PathBuf::from("/Users/me/dir with space"))]
        );
    }

    #[test]
    fn osc9_notification_payload_is_extracted() {
        let mut s = OscSniffer::default();
        let events = scan_all(&mut s, &[b"\x1b]9;Build finished\x07"]);
        assert_eq!(
            events,
            vec![OscEvent::Notify(String::from("Build finished"))]
        );
    }

    #[test]
    fn unrelated_osc_and_plain_output_are_ignored() {
        let mut s = OscSniffer::default();
        // OSC 0 title set, OSC 52 clipboard, CSI colours, plain text.
        let events = scan_all(
            &mut s,
            &[b"\x1b]0;title\x07\x1b]52;c;Zm9v\x07\x1b[31mred\x1b[0m plain"],
        );
        assert!(events.is_empty(), "got: {events:?}");
    }

    #[test]
    fn a_runaway_unterminated_sequence_is_abandoned() {
        let mut s = OscSniffer::default();
        assert!(s.scan(b"\x1b]133;D;").is_empty());
        // Far more than CARRY_MAX bytes of garbage with no terminator: the
        // sniffer must abandon the carry rather than buffer forever, and
        // still work afterwards.
        for _ in 0..10 {
            assert!(s.scan(&[b'x'; 1024]).is_empty());
        }
        let got = s.scan(b"\x1b]133;A\x07");
        assert_eq!(got, vec![(8, OscEvent::PromptStart)]);
    }

    #[test]
    fn zsh_shim_writes_all_four_dotfiles_sourcing_the_user_rc() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ensure_zsh_shim(tmp.path()).expect("shim must be written");
        for f in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            let p = dir.join(f);
            assert!(p.is_file(), "{f} must exist");
        }
        let rc = std::fs::read_to_string(dir.join(".zshrc")).unwrap();
        assert!(rc.contains("133;A"), "precmd must emit the prompt mark");
        assert!(rc.contains("133;C"), "preexec must emit the command mark");
        assert!(rc.contains("133;D"), "precmd must emit the finished mark");
        assert!(
            rc.contains("]7;file://"),
            "precmd must report cwd via OSC 7"
        );
        assert!(
            rc.contains("CROFT_USER_ZDOTDIR"),
            "the shim must source the user's own rc files"
        );
        // Idempotent: a second call rewrites without error.
        ensure_zsh_shim(tmp.path()).expect("shim rewrite must succeed");
    }
}
