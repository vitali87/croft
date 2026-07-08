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
    /// OSC 7;file://host/path — the shell's current working directory plus
    /// the reporting host (None for an empty authority or a bare path).
    Cwd(PathBuf, Option<String>),
    /// OSC 9;<message> — a notification payload.
    Notify(String),
    /// OSC 9;4;<state>;<percent> — ConEmu progress (systemd, winget, cargo
    /// wrappers; Ghostty/WezTerm render it natively): state 0 clears,
    /// 1 normal, 2 error, 3 indeterminate, 4 warning/paused. Percent is
    /// clamped to 100 (0 when absent).
    Progress(u8, u8),
    /// OSC 1337 `File=…;inline=1:<base64>` (iTerm2's imgcat protocol) — the
    /// decoded image bytes a program printed into the pane. alacritty drops
    /// the sequence as unknown OSC, so this tee is the only way the picture
    /// survives to be drawn as an overlay.
    InlineImage(Vec<u8>),
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

/// Carry cap for OSC 1337 inline-image sequences, whose base64 body is
/// legitimately megabytes. Bounds memory against a hostile or runaway
/// stream; a photo past ~6MB of pixels is dropped rather than buffered
/// forever. (The carry is re-prepended per 64KB chunk, so accumulation is
/// quadratic in chunk count — fine at this cap, a streaming accumulator if
/// bigger images ever matter.)
const IMAGE_CARRY_MAX: usize = 8 * 1024 * 1024;

/// Which of croft's OSC numbers a sequence carries.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OscKind {
    SemanticPrompt, // 133
    Cwd,            // 7
    Notify,         // 9
    ITerm2File,     // 1337 File= (inline images)
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
    // Order matters: `]1337;` must be tried before `]133;`, whose prefix it
    // contains — `]133;` would otherwise claim it and misparse the body.
    (b"]1337;", OscKind::ITerm2File),
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
            // file://host/path — strip the scheme, keep the authority as the
            // reporting host; a bare absolute path (some shells emit that)
            // passes through hostless.
            let (path, host) = if let Some(rest) = body.strip_prefix(b"file://") {
                let slash = rest.iter().position(|&b| b == b'/')?;
                let host =
                    (slash > 0).then(|| String::from_utf8_lossy(&rest[..slash]).into_owned());
                (&rest[slash..], host)
            } else if body.first() == Some(&b'/') {
                (body, None)
            } else {
                return None;
            };
            let decoded = percent_decode(path);
            Some(OscEvent::Cwd(
                PathBuf::from(String::from_utf8_lossy(&decoded).into_owned()),
                host,
            ))
        }
        OscKind::Notify => {
            // The `9;4;` sub-namespace is ConEmu progress, not a
            // notification; anything else after `9;` (including a message
            // that merely starts with a 4) stays a notification.
            if let Some(rest) = body.strip_prefix(b"4;") {
                let mut parts = rest.split(|&b| b == b';');
                let state = std::str::from_utf8(parts.next().unwrap_or_default())
                    .ok()?
                    .parse::<u8>()
                    .ok()
                    .filter(|s| *s <= 4)?;
                let percent = parts
                    .next()
                    .and_then(|p| std::str::from_utf8(p).ok())
                    .and_then(|p| p.parse::<u32>().ok())
                    .unwrap_or(0)
                    .min(100) as u8;
                return Some(OscEvent::Progress(state, percent));
            }
            Some(OscEvent::Notify(String::from_utf8_lossy(body).into_owned()))
        }
        OscKind::ITerm2File => {
            let rest = body.strip_prefix(b"File=")?;
            let colon = rest.iter().position(|&b| b == b':')?;
            // `inline=1` is what makes the payload a picture to draw; without
            // it the transfer is a download and none of croft's business.
            let inline = rest[..colon]
                .split(|&b| b == b';')
                .any(|p| p == b"inline=1");
            if !inline {
                return None;
            }
            use base64::Engine;
            let b64: Vec<u8> = rest[colon + 1..]
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            let data = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .ok()?;
            (!data.is_empty()).then_some(OscEvent::InlineImage(data))
        }
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
    /// Inline-image sequences get a far larger allowance — their base64
    /// bodies are legitimately megabytes — but once one is abandoned it
    /// stays abandoned (the fuse) so the rest of a huge payload streams by
    /// without being re-buffered from every later chunk.
    fn hold(&mut self, partial: &[u8]) {
        let cap = match classify(partial) {
            Classify::Ours(OscKind::ITerm2File, _) => IMAGE_CARRY_MAX,
            _ => CARRY_MAX,
        };
        if partial.len() > cap {
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
# A CROFT_USER_ZDOTDIR pointing back at the shim is a poisoned inheritance
# (croft launched from a croft pane); the user's real dotfiles live in HOME.
if [[ -n "$CROFT_USER_ZDOTDIR" && "$CROFT_USER_ZDOTDIR" == "$_croft_shim_zdotdir" ]]; then
  export CROFT_USER_ZDOTDIR="$HOME"
fi
ZDOTDIR="${CROFT_USER_ZDOTDIR:-$HOME}"
[[ "$ZDOTDIR" != "$_croft_shim_zdotdir" && -f "$ZDOTDIR/.zshenv" ]] && builtin source "$ZDOTDIR/.zshenv"
# The sourced file may be another terminal's injection shim (Ghostty, kitty
# point ZDOTDIR at their own dir when launching croft as their command);
# their .zshenv restores the REAL ZDOTDIR when sourced, so whatever it holds
# now is the user's true dotfile dir. A user .zshenv that re-roots ZDOTDIR
# (XDG setups) is honoured the same way.
if [[ -z "$ZDOTDIR" || "$ZDOTDIR" == "$_croft_shim_zdotdir" ]]; then
  export CROFT_USER_ZDOTDIR="$HOME"
else
  export CROFT_USER_ZDOTDIR="$ZDOTDIR"
fi
ZDOTDIR="$_croft_shim_zdotdir"
"#;

const ZSH_SHIM_ZPROFILE: &str = r#"# croft shell integration (auto-generated; do not edit)
_croft_user="${CROFT_USER_ZDOTDIR:-$HOME}"
[[ "$_croft_user" != "$ZDOTDIR" && -f "$_croft_user/.zprofile" ]] && ZDOTDIR="$_croft_user" builtin source "$_croft_user/.zprofile"
"#;

const ZSH_SHIM_ZLOGIN: &str = r#"# croft shell integration (auto-generated; do not edit)
_croft_user="${CROFT_USER_ZDOTDIR:-$HOME}"
[[ "$_croft_user" != "$ZDOTDIR" && -f "$_croft_user/.zlogin" ]] && ZDOTDIR="$_croft_user" builtin source "$_croft_user/.zlogin"
"#;

const ZSH_SHIM_ZSHRC: &str = r#"# croft shell integration (auto-generated; do not edit)
_croft_user="${CROFT_USER_ZDOTDIR:-$HOME}"
[[ "$_croft_user" != "$ZDOTDIR" && -f "$_croft_user/.zshrc" ]] && ZDOTDIR="$_croft_user" builtin source "$_croft_user/.zshrc"

# FinalTerm / OSC 133 semantic prompt marks + OSC 7 cwd, the protocol VS
# Code, Ghostty, iTerm2 and WezTerm build command navigation on.
if [[ -o interactive ]] && (( ! ${+_croft_si_installed} )); then
  typeset -g _croft_si_installed=1
  typeset -g _croft_b_mark=$'%{\e]133;B\a%}'
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
    # Mark where the typed command starts (133;B). Themes rebuild PS1 from
    # their own precmd (which ran before this one), so the zero-width
    # suffix is re-asserted at every prompt.
    if [[ "$PS1" != *"$_croft_b_mark" ]]; then
      PS1+="$_croft_b_mark"
    fi
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

/// The user's real dotfile directory for the shim to source. An inherited
/// `ZDOTDIR` pointing into croft's own shell-integration tree is poisoned
/// inheritance — a croft launched from a croft pane, whose environment
/// carries the shim path — and must fall back to `home`, or the shim would
/// source itself in a recursion loop and the user's rc would never run.
pub fn resolve_user_zdotdir(
    inherited: Option<&std::path::Path>,
    shim_root: &std::path::Path,
    home: &std::path::Path,
) -> PathBuf {
    match inherited {
        Some(z) if !z.starts_with(shim_root) => z.to_path_buf(),
        _ => home.to_path_buf(),
    }
}

/// The bash bootstrap, read via `$ENV` by an interactive `bash --posix`
/// (kitty invented the trick; Ghostty copied it). Posix-mode bash reads
/// ONLY `$ENV` at startup — no /etc/profile, no ~/.bashrc — so the shim
/// backs out of posix mode, replays the user's real startup files (login
/// vs non-login), then installs hooks emitting the OSC 133 prompt marks
/// and the OSC 7 cwd report. Runs on every bash croft spawns, from macOS's
/// system 3.2 to 5.3+.
const BASH_SHIM: &str = r#"# croft shell integration bootstrap (auto-generated; do not edit)
[[ "$-" != *i* ]] && builtin return

if [[ -n "$CROFT_BASH_INJECT" ]]; then
  builtin unset CROFT_BASH_INJECT
  builtin set +o posix
  builtin shopt -u inherit_errexit 2>/dev/null
  # Restore the user's own $ENV (rarely set for bash) or drop croft's.
  if [[ -n "$CROFT_BASH_ENV" ]]; then
    builtin export ENV="$CROFT_BASH_ENV"
    builtin unset CROFT_BASH_ENV
  else
    builtin unset ENV
  fi
  # Posix mode defaulted HISTFILE to ~/.sh_history; croft pre-seeded the
  # bash default and flagged it so children do not inherit the export.
  if [[ -n "$CROFT_BASH_UNEXPORT_HISTFILE" ]]; then
    builtin export -n HISTFILE
    builtin unset CROFT_BASH_UNEXPORT_HISTFILE
  fi
  # Replay the startup files posix mode skipped, exactly as bash itself
  # would have chosen them.
  if builtin shopt -q login_shell; then
    [[ -r /etc/profile ]] && builtin source /etc/profile
    for _croft_rc in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
      if [[ -r "$_croft_rc" ]]; then
        builtin source "$_croft_rc"
        break
      fi
    done
  else
    for _croft_rc in /etc/bash.bashrc /etc/bash/bashrc /etc/bashrc; do
      if [[ -r "$_croft_rc" ]]; then
        builtin source "$_croft_rc"
        break
      fi
    done
    [[ -r "$HOME/.bashrc" ]] && builtin source "$HOME/.bashrc"
  fi
  builtin unset _croft_rc
fi

# FinalTerm / OSC 133 semantic prompt marks + OSC 7 cwd, the protocol VS
# Code, Ghostty, iTerm2 and WezTerm build command navigation on.
if [[ -z "$_croft_si_installed" ]]; then
  _croft_si_installed=1
  _croft_b_mark='\[\e]133;B\a\]'
  _croft_preexec() {
    _croft_cmd_inflight=1
    builtin printf '\033]133;C\007'
  }
  _croft_precmd() {
    local _croft_ec=$?
    if [[ -n "$_croft_cmd_inflight" ]]; then
      builtin unset _croft_cmd_inflight
      builtin printf '\033]133;D;%d\007' "$_croft_ec"
    fi
    builtin printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$PWD"
    builtin printf '\033]133;A\007'
    # Prompt themes rewrite PS1 from their own PROMPT_COMMAND entries, so
    # the zero-width input-start mark is re-asserted at every prompt.
    if [[ "$PS1" != *"$_croft_b_mark" ]]; then
      PS1+="$_croft_b_mark"
    fi
  }
  # DEBUG fires for every simple command, including PROMPT_COMMAND's own
  # entries; only the first one after the prompt was armed — and not part
  # of PROMPT_COMMAND itself (an empty Enter goes straight back to it) —
  # is the user's command. bash-preexec's exact discipline.
  _croft_debug_trap() {
    [[ -n "$COMP_LINE" ]] && builtin return
    [[ -z "$_croft_at_prompt" ]] && builtin return
    _croft_at_prompt=
    case ";${PROMPT_COMMAND[*]};" in
      *";$BASH_COMMAND;"*) builtin return ;;
    esac
    _croft_preexec
  }
  _croft_arm() { _croft_at_prompt=1; }
  if [[ -n "${bash_preexec_imported:-${__bp_imported:-}}" ]]; then
    # bash-preexec (iTerm2 integration, Atuin, ...) owns the DEBUG trap;
    # join its cooperative hook arrays instead of fighting over it.
    precmd_functions+=(_croft_precmd)
    preexec_functions+=(_croft_preexec)
  else
    if ((BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 3))); then
      # bash 5.3 funsub runs in the current shell: a trap-free preexec.
      PS0='${ _croft_preexec; }'"$PS0"
    elif [[ -z "$(trap -p DEBUG)" ]]; then
      builtin trap '_croft_debug_trap' DEBUG
    fi
    # An existing DEBUG trap (a hand-rolled preexec) is left alone; marks
    # degrade to prompt + cwd only.
    # Precmd runs FIRST so nothing clobbers $? before the exit capture;
    # the arm runs LAST so user entries never read as the next command.
    if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
      PROMPT_COMMAND=(_croft_precmd "${PROMPT_COMMAND[@]}" _croft_arm)
    else
      PROMPT_COMMAND="_croft_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND};_croft_arm"
    fi
  fi
fi
"#;

/// Write croft's bash `$ENV` shim (idempotently) and return the file path.
pub fn ensure_bash_shim(config_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let dir = config_dir.join("shell-integration").join("bash");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("croft.bash");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(BASH_SHIM) {
        let mut f = std::fs::File::create(&path)?;
        f.write_all(BASH_SHIM.as_bytes())?;
    }
    Ok(path)
}

/// The fish integration, auto-sourced from `<data dir>/fish/vendor_conf.d/`
/// via a prepended `XDG_DATA_DIRS` entry (the mechanism kitty, Ghostty and
/// VS Code all use; fish reads vendor_conf.d before the user's config).
///
/// fish 4.0+ (the Rust rewrite) emits the OSC 133 marks and the OSC 7 cwd
/// report natively, for every terminal, on by default — a second emitter
/// would double-mark each prompt, so like kitty (and unlike Ghostty, which
/// double-marks) croft's hooks install only on old fish, detected the way
/// kitty does it: modern fish ships the `forward-char-passive` binding.
const FISH_INTEGRATION: &str = r#"# croft shell integration (auto-generated; do not edit)

# First: put XDG_DATA_DIRS back so child processes never see croft's
# injection dir.
if set -q CROFT_FISH_XDG_DATA_DIR
    if set -q XDG_DATA_DIRS
        set --local _croft_i (contains --index -- "$CROFT_FISH_XDG_DATA_DIR" $XDG_DATA_DIRS)
        and set --erase XDG_DATA_DIRS[$_croft_i]
        if not set -q XDG_DATA_DIRS[1]
            set --erase XDG_DATA_DIRS
        else
            set --global --export --path XDG_DATA_DIRS $XDG_DATA_DIRS
        end
    end
    set --erase CROFT_FISH_XDG_DATA_DIR
end

if status is-interactive
    # fish 4.0+ marks prompts natively (OSC 133 + OSC 7 for every
    # terminal); emitting again would double-decorate. Old fish (<= 3.7)
    # lacks the forward-char-passive binding and needs croft's hooks.
    if not bind --function-names 2>/dev/null | string match -q forward-char-passive
        function _croft_mark_prompt --on-event fish_prompt --on-event fish_cancel --on-event fish_posterror
            if test "$_croft_prompt_state" = command
                # the started command never reported back: close the cycle
                printf '\e]133;D\a'
            end
            set --global _croft_prompt_state prompt
            printf '\e]7;file://%s%s\a' "$hostname" (string escape --style=url -- "$PWD")
            printf '\e]133;A\a'
        end
        function _croft_mark_output_start --on-event fish_preexec
            set --global _croft_prompt_state command
            printf '\e]133;C\a'
        end
        function _croft_mark_output_end --on-event fish_postexec
            set --local _croft_s $status
            set --global _croft_prompt_state finished
            printf '\e]133;D;%s\a' $_croft_s
        end
    end
end
"#;

/// Write croft's fish `vendor_conf.d` integration (idempotently) and return
/// the XDG data dir to prepend to `XDG_DATA_DIRS`.
pub fn ensure_fish_integration(config_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let dir = config_dir.join("shell-integration").join("fish");
    let conf_dir = dir.join("fish").join("vendor_conf.d");
    std::fs::create_dir_all(&conf_dir)?;
    let path = conf_dir.join("croft.fish");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(FISH_INTEGRATION) {
        let mut f = std::fs::File::create(&path)?;
        f.write_all(FISH_INTEGRATION.as_bytes())?;
    }
    Ok(dir)
}

/// The `XDG_DATA_DIRS` value for a croft-spawned fish: croft's injection
/// data dir first, then the inherited value (or the XDG spec default when
/// unset/empty, so system data dirs stay visible to fish).
pub fn fish_xdg_data_dirs(data_dir: &std::path::Path, inherited: Option<&str>) -> String {
    const XDG_DEFAULT: &str = "/usr/local/share:/usr/share";
    let rest = match inherited {
        Some(x) if !x.is_empty() => x,
        _ => XDG_DEFAULT,
    };
    format!("{}:{rest}", data_dir.display())
}

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
    fn inline_image_osc_1337_is_captured_across_chunks() {
        use base64::Engine;
        let payload: Vec<u8> = (0u32..600).map(|i| (i % 251) as u8).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&payload);
        let seq = format!("\x1b]1337;File=name=eC5wbmc=;size=600;inline=1:{b64}\x07");
        let bytes = seq.as_bytes();
        // Split inside the params and inside the base64 body: the payload is
        // far longer than any OSC 133/7/9, so this exercises the large-image
        // carry path.
        let mut s = OscSniffer::default();
        let events = scan_all(&mut s, &[&bytes[..20], &bytes[20..300], &bytes[300..]]);
        assert_eq!(events, vec![OscEvent::InlineImage(payload.clone())]);
        // Without inline=1 the payload is a download, not a picture: ignored.
        let seq = format!("\x1b]1337;File=name=eC5wbmc=:{b64}\x07");
        assert!(scan_all(&mut s, &[seq.as_bytes()]).is_empty());
        // Marks flowing after an image still parse (the carry fully resets).
        assert_eq!(
            scan_all(&mut s, &[b"\x1b]133;A\x07"]),
            vec![OscEvent::PromptStart]
        );
    }

    #[test]
    fn runaway_inline_image_payloads_are_abandoned() {
        // A payload past the image carry cap is dropped without an event and
        // without wedging the sniffer for later sequences.
        let mut s = OscSniffer::default();
        assert!(s.scan(b"\x1b]1337;File=inline=1:").is_empty());
        let filler = vec![b'A'; IMAGE_CARRY_MAX + 4096];
        for chunk in filler.chunks(65536) {
            assert!(s.scan(chunk).is_empty());
        }
        assert!(
            s.scan(b"\x07").is_empty(),
            "the oversized image never fires"
        );
        assert_eq!(
            scan_all(&mut s, &[b"\x1b]133;C\x07"]),
            vec![OscEvent::CommandStart]
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
            vec![OscEvent::Cwd(
                PathBuf::from("/Users/me/dir with space"),
                Some(String::from("mac.local"))
            )]
        );
    }

    #[test]
    fn osc7_carries_the_reporting_host() {
        let mut s = OscSniffer::default();
        let events = scan_all(&mut s, &[b"\x1b]7;file://prod-db-1/var/lib\x07"]);
        assert_eq!(
            events,
            vec![OscEvent::Cwd(
                PathBuf::from("/var/lib"),
                Some(String::from("prod-db-1"))
            )]
        );
        // An empty authority (file:///path) and a bare path carry no host.
        let events = scan_all(&mut s, &[b"\x1b]7;file:///tmp\x07\x1b]7;/opt\x07"]);
        assert_eq!(
            events,
            vec![
                OscEvent::Cwd(PathBuf::from("/tmp"), None),
                OscEvent::Cwd(PathBuf::from("/opt"), None),
            ]
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
    fn osc_9_4_progress_is_parsed_and_plain_osc_9_stays_notify() {
        let mut s = OscSniffer::default();
        // ConEmu progress: state 1 (normal) at 46%.
        assert_eq!(
            scan_all(&mut s, &[b"\x1b]9;4;1;46\x07"]),
            vec![OscEvent::Progress(1, 46)]
        );
        // State 0 clears; percent may be absent. Split across chunks.
        assert_eq!(
            scan_all(&mut s, &[b"\x1b]9;4", b";0\x07"]),
            vec![OscEvent::Progress(0, 0)]
        );
        // Error keeps its percent; out-of-range percents clamp; junk state drops.
        assert_eq!(
            scan_all(
                &mut s,
                &[b"\x1b]9;4;2;63\x07\x1b]9;4;1;250\x07\x1b]9;4;x;5\x07"]
            ),
            vec![OscEvent::Progress(2, 63), OscEvent::Progress(1, 100)]
        );
        // A plain OSC 9 notification is untouched, even one starting with 4.
        assert_eq!(
            scan_all(&mut s, &[b"\x1b]9;Build finished\x07"]),
            vec![OscEvent::Notify(String::from("Build finished"))]
        );
        assert_eq!(
            scan_all(&mut s, &[b"\x1b]9;4 done\x07"]),
            vec![OscEvent::Notify(String::from("4 done"))]
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
    fn resolve_user_zdotdir_rejects_croft_shim_paths() {
        let shim_root = std::path::Path::new("/cfg/croft/shell-integration");
        let home = std::path::Path::new("/Users/me");
        // Poisoned: inherited ZDOTDIR inside croft's shim tree → HOME.
        assert_eq!(
            resolve_user_zdotdir(Some(&shim_root.join("zsh")), shim_root, home),
            home
        );
        // A genuine user ZDOTDIR redirect is honoured.
        let custom = std::path::Path::new("/Users/me/.config/zsh");
        assert_eq!(resolve_user_zdotdir(Some(custom), shim_root, home), custom);
        // No inherited ZDOTDIR → HOME.
        assert_eq!(resolve_user_zdotdir(None, shim_root, home), home);
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

    #[test]
    fn fish_native_mark_dialect_is_parsed() {
        // fish 4.x (the Rust rewrite) emits the marks itself, for every
        // terminal, in its own dialect: ST-terminated, `A` carrying
        // `click_events=1`, `C` carrying the URL-escaped command line, and a
        // bare `D` (no exit code) when it synthesizes a missing output-end
        // after a cancelled prompt. All must parse.
        let mut s = OscSniffer::default();
        let events = scan_all(
            &mut s,
            &[b"\x1b]133;A;click_events=1\x1b\\> \x1b]133;B\x1b\\cmd\x1b]133;C;cmdline_url=cmd%20--flag\x1b\\out\x1b]133;D;2\x1b\\"],
        );
        assert_eq!(
            events,
            vec![
                OscEvent::PromptStart,
                OscEvent::PromptEnd,
                OscEvent::CommandStart,
                OscEvent::CommandEnd(Some(2)),
            ]
        );
        let bare = scan_all(&mut s, &[b"\x1b]133;D\x1b\\"]);
        assert_eq!(bare, vec![OscEvent::CommandEnd(None)]);
    }

    #[test]
    fn bash_shim_backs_out_of_posix_mode_and_installs_hooks() {
        // The kitty/Ghostty ENV + --posix injection: an interactive
        // `bash --posix` reads ONLY $ENV, so the shim must leave posix mode,
        // replay the user's real startup files (login vs non-login), and
        // install the OSC 133 / OSC 7 hooks.
        let tmp = tempfile::tempdir().unwrap();
        let file = ensure_bash_shim(tmp.path()).expect("shim must be written");
        assert!(file.is_file(), "croft.bash must exist");
        let s = std::fs::read_to_string(&file).unwrap();
        for needle in [
            "set +o posix",
            "login_shell",
            ".bash_profile",
            ".bashrc",
            "PROMPT_COMMAND",
            "133;A",
            "133;B",
            "133;C",
            "133;D",
            "]7;file://",
            "HISTFILE",
        ] {
            assert!(s.contains(needle), "bash shim must contain {needle:?}");
        }
        ensure_bash_shim(tmp.path()).expect("shim rewrite must succeed");
    }

    #[test]
    fn fish_integration_writes_a_vendor_conf_that_defers_to_native_marks() {
        // fish is injected via the standard XDG_DATA_DIRS vendor_conf.d
        // mechanism. The script must restore XDG_DATA_DIRS first, and only
        // emit marks itself on old fish (<= 3.7) — modern fish marks
        // natively, and a second emitter would double-decorate.
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = ensure_fish_integration(tmp.path()).expect("must be written");
        let f = data_dir
            .join("fish")
            .join("vendor_conf.d")
            .join("croft.fish");
        assert!(
            f.is_file(),
            "croft.fish must exist under fish/vendor_conf.d"
        );
        let s = std::fs::read_to_string(&f).unwrap();
        for needle in [
            "CROFT_FISH_XDG_DATA_DIR",
            "forward-char-passive",
            "fish_preexec",
            "fish_postexec",
            "133;A",
            "133;C",
            "133;D",
            "]7;file://",
        ] {
            assert!(
                s.contains(needle),
                "fish integration must contain {needle:?}"
            );
        }
        ensure_fish_integration(tmp.path()).expect("rewrite must succeed");
    }

    #[test]
    fn fish_xdg_data_dirs_prepends_and_keeps_the_spec_default() {
        let d = std::path::Path::new("/cfg/shell-integration/fish");
        assert_eq!(
            fish_xdg_data_dirs(d, Some("/a:/b")),
            "/cfg/shell-integration/fish:/a:/b"
        );
        // Unset or empty inherits the XDG spec default, so fish never loses
        // sight of the system data dirs.
        for inherited in [None, Some("")] {
            assert_eq!(
                fish_xdg_data_dirs(d, inherited),
                "/cfg/shell-integration/fish:/usr/local/share:/usr/share"
            );
        }
    }
}
