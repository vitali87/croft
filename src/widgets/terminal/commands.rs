//! Shell integration: the prompt and command marks the shell reports, finished commands and
//! their decorations, a command's input and output text, jumping between prompts, and the
//! prompt line as typed.

use super::*;

/// Source of [`StoredMark::id`], shared by every pane.
///
/// Process-wide rather than per-pane so an id is unambiguous wherever it
/// travels; `u64` so it cannot realistically wrap (at one mark per
/// microsecond it lasts half a million years), which matters because the
/// whole value of the id is that a stale one never matches a live command.
pub(super) static NEXT_MARK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Take the next mark id.
///
/// `Relaxed` is sufficient and is not a shortcut. Uniqueness needs no
/// ordering at all — `fetch_add` on a single atomic is totally ordered on
/// itself, so no two callers can take the same value. And the id's
/// PUBLICATION is ordered by something else entirely: it reaches a reader
/// only inside a `StoredMark` behind the `marks` mutex, whose unlock/lock
/// pair supplies the happens-before. There is no second variable whose
/// visibility this atomic would have to fence, which is the only thing a
/// stronger ordering would buy. Do not "upgrade" it to `SeqCst`.
pub(super) fn next_mark_id() -> u64 {
    NEXT_MARK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A finished command as reported by the reader thread at its `133;D`
/// mark: exit + duration (for the long-command notice) plus the typed
/// command text (read off the B→C mark span at the C mark, before the
/// command can clear it) and the pane's OSC 7 cwd at finish time —
/// everything the durable command history records.
#[derive(Clone, Debug)]
pub struct FinishedCommand {
    pub exit: Option<i32>,
    pub dur: std::time::Duration,
    pub cmd: String,
    pub cwd: Option<std::path::PathBuf>,
    /// The OSC 7 reporting hostname AT COMPLETION time. Captured with the
    /// 133;D mark, not read later at drain: an in-pane `ssh`/`exit` between
    /// the two would attribute the command to the wrong machine.
    pub host: Option<String>,
    /// The command's output (the 133;C→133;D span), escape-free, captured at
    /// the D mark while the rows are guaranteed still present. Capped to the
    /// LAST [`FINISHED_OUTPUT_CAP_LINES`] lines — the tail keeps a compiler's
    /// error summary; a build long enough to overflow the cap loses its
    /// earliest lines. Feeds the build problem matchers (#119).
    pub output: String,
}

/// Tail cap on [`FinishedCommand::output`].
pub const FINISHED_OUTPUT_CAP_LINES: usize = 5000;

impl RunningCommand {
    /// Follow a wipe that just landed, `now` being the clock read right
    /// after it. Once the first output row is erased, the output that
    /// survives starts at the top of the screen the wipe left: an ED 2
    /// erases that row only when it sat blank below the screen's last text
    /// (alacritty pushes the text into the scrollback), and an ED 3 only
    /// when it sat in the scrollback it empties.
    pub(super) fn follow_wipe(&mut self, wipe: Wipe, now: i64) {
        let line = self.out_line - (now - self.out_clock) as i32;
        if (line >= 0 && wipe.screen) || (line < 0 && wipe.hist) {
            self.out_line = 0;
            self.out_clock = now;
        }
    }
}

/// A finished command derived from the OSC 133 marks: the grid line of the
/// prompt it was typed at (current coords, negative = scrollback), its exit
/// code (`None` when the shell omitted it), how long it ran, where its typed
/// text starts (`PromptEnd` line + column), and its output span
/// (`CommandStart` line up to but excluding the `CommandEnd` line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDecoration {
    /// Identity of the command, stable across eviction (#440).
    ///
    /// Taken from the `CommandStart` mark, because that is the mark every
    /// finished command has exactly one of — the prompt marks before it are
    /// optional in the pairing below, so keying on one of those would leave
    /// some decorations without an id.
    ///
    /// **Only the identity is stable; the presentation fields are not.** The
    /// id, `exit` and the output span come from the `CommandStart`/`CommandEnd`
    /// pair, so they hold for as long as the decoration exists at all. But
    /// `line` and `input` come from the PROMPT marks, which sit on earlier
    /// grid rows and so cross the scrollback floor FIRST: the GC in
    /// `marks_snapshot` retains per mark, not per command, so a command can
    /// outlive its own prompt. When that happens `line` falls back to
    /// `output_start` and `input` becomes `None`, while the id keeps naming
    /// the same command throughout.
    ///
    /// So a consumer keyed on the id gets a durable answer to "which
    /// command", and a best-effort answer to "where on screen was it typed".
    /// That is the right split for the callers there are — the gutter wants
    /// a row and can use the fallback, and a re-run wants the text and must
    /// handle its absence — but it is a real limit, not an oversight, and
    /// worth knowing before keying anything else on the pair.
    pub id: u64,
    pub line: i32,
    pub exit: Option<i32>,
    pub duration: Option<std::time::Duration>,
    pub input: Option<(i32, usize)>,
    pub output_start: i32,
    pub output_end: i32,
}

/// One mark as a caller sees it: its identity, its CURRENT grid line, and
/// the fields the pairing needs.
///
/// A struct rather than the tuple this used to be. The tuple had reached four
/// positional fields and adding the id would have made five, at which point
/// `m.3` at a call site says nothing about what it holds — and the whole
/// change here is about not confusing a position with an identity.
#[derive(Clone, Debug)]
pub struct MarkView {
    /// Stable identity from [`StoredMark::id`] (#440).
    pub id: u64,
    pub kind: crate::shell_integration::OscEvent,
    /// Current grid line; negative means scrollback.
    pub line: i32,
    /// Cursor column when the mark landed.
    pub col: usize,
    /// For `CommandEnd`: how long the command ran.
    pub dur: Option<std::time::Duration>,
}

/// Pair the mark stream (oldest first, each with its current grid line and,
/// for `CommandEnd`, the measured duration) into one record per *finished*
/// command: the last `PromptStart` line before a `CommandStart` names the
/// row, the following `CommandEnd` supplies exit + duration. A `CommandEnd`
/// with no pending `CommandStart` is dropped — that's how a second
/// integration layer's duplicate marks (Ghostty's hooks chained behind
/// croft's) stay out of the record.
pub fn pair_decorations(marks: &[MarkView]) -> Vec<CommandDecoration> {
    use crate::shell_integration::OscEvent as E;
    let mut out = Vec::new();
    let mut prompt: Option<i32> = None;
    let mut input: Option<(i32, usize)> = None;
    // The id of the `CommandStart` currently open, carried alongside its
    // line so the decoration is keyed on the mark that actually defines the
    // command rather than on whichever prompt mark happened to precede it.
    let mut started: Option<(u64, i32)> = None;
    for m in marks {
        match &m.kind {
            E::PromptStart => {
                prompt = Some(m.line);
                input = None;
            }
            E::PromptEnd => input = Some((m.line, m.col)),
            E::CommandStart => started = Some((m.id, m.line)),
            E::CommandEnd(exit) => {
                if let Some((id, output_start)) = started.take() {
                    out.push(CommandDecoration {
                        id,
                        line: prompt.unwrap_or(output_start),
                        exit: *exit,
                        duration: m.dur,
                        input,
                        output_start,
                        output_end: m.line,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Compact human form of a command duration: "480ms", "3.4s", "2m 05s".
pub fn human_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

/// Cap on retained marks per pane (a mark per prompt/command boundary;
/// thousands would mean a very long session — drop the oldest).
pub(super) const MARKS_MAX: usize = 2000;

/// The nearest prompt line strictly above (`forward == false`) or below
/// (`forward == true`) the viewport-top grid line `current_top`. Feeds
/// Cmd+Up / Cmd+Down command navigation.
pub fn pick_prompt_jump(prompt_lines: &[i32], current_top: i32, forward: bool) -> Option<i32> {
    if forward {
        prompt_lines
            .iter()
            .copied()
            .filter(|&l| l > current_top)
            .min()
    } else {
        prompt_lines
            .iter()
            .copied()
            .filter(|&l| l < current_top)
            .max()
    }
}

impl PtyTerminal {
    /// OSC 133 marks with their *current* grid line (negative = scrollback),
    /// oldest first. Marks whose content scrolled past the scrollback floor
    /// are garbage-collected here.
    pub fn command_marks(&self) -> Vec<(crate::shell_integration::OscEvent, i32)> {
        self.marks_snapshot()
            .into_iter()
            .map(|m| (m.kind, m.line))
            .collect()
    }

    /// The marks with current grid lines and, for `CommandEnd`, the measured
    /// command duration. GC + drift adjustment as in [`Self::command_marks`].
    pub(super) fn marks_snapshot(&self) -> Vec<MarkView> {
        let mut term = self.term.lock();
        let now = self.clock.lock().unwrap().tick(&mut term);
        let floor = term.grid().topmost_line().0;
        drop(term);
        let mut marks = self.marks.lock().unwrap();
        marks.retain(|m| m.line_rec - (now - m.clock_rec) as i32 >= floor);
        marks
            .iter()
            .map(|m| MarkView {
                id: m.id,
                kind: m.kind.clone(),
                line: m.line_rec - (now - m.clock_rec) as i32,
                col: m.col_rec,
                dur: m.dur,
            })
            .collect()
    }

    /// One record per finished command, for the gutter decorations: prompt
    /// line (current grid coords), exit code, duration.
    pub fn command_decorations(&self) -> Vec<CommandDecoration> {
        pair_decorations(&self.marks_snapshot())
    }

    /// Finished commands since the last drain: (exit code, duration). Feeds
    /// the status-bar notification for long commands in unfocused panes.
    pub fn drain_finished_commands(&self) -> Vec<FinishedCommand> {
        self.finished_rx.try_iter().collect()
    }

    /// The text a finished command printed: its output span (`output_start`
    /// up to but excluding `output_end`), trailing blank lines dropped.
    pub fn command_output_text(&self, d: &CommandDecoration) -> String {
        if d.output_end <= d.output_start {
            return String::new();
        }
        let term = self.term.lock();
        extract_selection_text(
            &term,
            d.output_start,
            0,
            d.output_end - 1,
            term.columns().saturating_sub(1),
        )
    }

    /// The command as it was typed at its prompt: from the `PromptEnd`
    /// mark's cell to the end of the input rows (the row before the output
    /// starts), soft-wrapped rows joined.
    pub fn command_input_text(&self, d: &CommandDecoration) -> String {
        let Some((line, col)) = d.input else {
            return String::new();
        };
        if d.output_start <= line {
            return String::new();
        }
        let term = self.term.lock();
        without_continuation_prompts(&extract_selection_text(
            &term,
            line,
            col,
            d.output_start - 1,
            term.columns().saturating_sub(1),
        ))
    }

    /// Select the command's output span (and snap the view to it), so the
    /// user sees exactly what Copy Output grabbed. No-op for empty output.
    pub fn select_command_output(&mut self, d: &CommandDecoration) {
        if d.output_end <= d.output_start {
            return;
        }
        self.selection = Some(Selection {
            anchor: (d.output_start, 0),
            head: (d.output_end - 1, self.cols.saturating_sub(1)),
            block: false,
        });
        self.stamp_selection_clock();
        self.scroll_to_line(d.output_start);
    }

    /// The decoration whose gutter dot sits at screen cell (col, row), if
    /// any: col must be the pane's left border column and row a viewport
    /// row whose grid line carries a finished command.
    pub fn decoration_at_screen(&self, col: u16, row: u16) -> Option<CommandDecoration> {
        let inner = self.last_inner;
        if col != self.last_area.x || row < inner.y || row >= inner.y + inner.height {
            return None;
        }
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let display_offset = term.grid().display_offset() as i32;
        drop(term);
        let line = i32::from(row - inner.y) - display_offset;
        self.command_decorations()
            .into_iter()
            .find(|d| d.line == line)
    }

    /// Current grid lines of the PromptStart marks — the Cmd+Up/Cmd+Down
    /// navigation targets.
    pub fn prompt_lines(&self) -> Vec<i32> {
        self.command_marks()
            .into_iter()
            .filter(|(kind, _)| *kind == crate::shell_integration::OscEvent::PromptStart)
            .map(|(_, line)| line)
            .collect()
    }

    /// What has been typed at the shell prompt, when the cursor sits at its
    /// end (#614): `(typed, cursor screen x, cursor screen y, pane right
    /// edge)`. `None` unless the newest shell-integration mark is the prompt
    /// end on the cursor's own row, the view is live (not scrolled back, not
    /// a full-screen program), and nothing is drawn right of the cursor.
    /// That last rule is what keeps croft out of the way of a shell drawing
    /// its own suggestion (fish, zsh-autosuggestions), which lands there.
    pub fn prompt_tail(&self) -> Option<(String, u16, u16, u16)> {
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) || term.grid().display_offset() != 0 {
            return None;
        }
        let cursor = term.grid().cursor.point;
        let line = cursor.line.0;
        let now = self.clock_now(&term);
        let ms = self.marks.lock().unwrap();
        let last = ms.last()?;
        if !matches!(last.kind, crate::shell_integration::OscEvent::PromptEnd)
            || last.line_rec - (now - last.clock_rec) as i32 != line
        {
            return None;
        }
        let (text, colmap) = row_text_and_cols(&term, line);
        let cur = cursor.column.0;
        let b_col = last.col_rec;
        // Characters at or right of the cursor: must be none.
        if colmap
            .iter()
            .zip(text.chars())
            .any(|(&c, ch)| c >= cur && !ch.is_whitespace())
        {
            return None;
        }
        let typed: String = colmap
            .iter()
            .zip(text.chars())
            .filter(|(c, _)| **c >= b_col && **c < cur)
            .map(|(_, ch)| ch)
            .collect();
        let inner = self.last_inner;
        if inner.width == 0 || line < 0 || line as u16 >= inner.height {
            return None;
        }
        Some((
            typed,
            inner.x + cur as u16,
            inner.y + line as u16,
            inner.x + inner.width,
        ))
    }

    /// The shell cursor's screen cell, when it is on screen and the program
    /// hasn't hidden it (#621: screen reader mode parks the cursor here).
    pub fn screen_cursor(&self) -> Option<(u16, u16)> {
        let term = self.term.lock();
        if !term.mode().contains(TermMode::SHOW_CURSOR) || term.grid().display_offset() != 0 {
            return None;
        }
        let p = term.grid().cursor.point;
        let inner = self.last_inner;
        let (line, col) = (p.line.0, p.column.0 as u16);
        if inner.width == 0 || line < 0 || line as u16 >= inner.height || col >= inner.width {
            return None;
        }
        Some((inner.x + col, inner.y + line as u16))
    }

    /// The arrow-key bytes a plain click at screen cell (col, row) should
    /// send to walk the shell cursor to the clicked column (Ghostty's
    /// click-to-move-cursor). `Some` only when the shell is sitting at a
    /// prompt (the newest OSC 133 mark is B, on this same row), the click
    /// lands on the cursor's own row, and the target cell differs from the
    /// cursor. The target clamps to the typed span: never left of where
    /// input starts, never past one cell after the row's last glyph. The
    /// line itself is untouched — only cursor motion is synthesized, so it
    /// works identically in zsh, bash, and fish line editors.
    pub fn prompt_click_arrows(&self, col: u16, row: u16) -> Option<Vec<u8>> {
        let (vr, vc) = self.cell_at(col, row)?;
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let off = term.grid().display_offset() as i32;
        let line = vr as i32 - off;
        let cursor = term.grid().cursor.point;
        if line != cursor.line.0 {
            return None;
        }
        let now = self.clock_now(&term);
        let ms = self.marks.lock().unwrap();
        let last = ms.last()?;
        if !matches!(last.kind, crate::shell_integration::OscEvent::PromptEnd) {
            return None;
        }
        if last.line_rec - (now - last.clock_rec) as i32 != line {
            return None;
        }
        let b_col = last.col_rec as u16;
        let (text, colmap) = row_text_and_cols(&term, line);
        let chars = text.trim_end().chars().count();
        let end_col = if chars == 0 {
            b_col
        } else {
            colmap
                .get(chars - 1)
                .map(|&c| c as u16 + 1)
                .unwrap_or(b_col)
        };
        let cur = cursor.column.0 as u16;
        // The row may have been rewritten SHORTER than the recorded 133;B
        // column (a backgrounded progress writer, a shrunk pane): the live
        // bounds then sit left of `b_col`, and `clamp` with min > max
        // panics. Bound the lower edge by the upper so the gesture
        // degrades instead of taking the app down.
        let hi = end_col.max(cur);
        let target = vc.clamp(b_col.min(hi), hi);
        if target == cur {
            return None;
        }
        let (seq, n) = if target > cur {
            (b"\x1b[C".as_slice(), target - cur)
        } else {
            (b"\x1b[D".as_slice(), cur - target)
        };
        Some(seq.repeat(n as usize))
    }

    /// Test-only: record an OSC 133 mark as the reader thread would,
    /// stamped against the grid's CURRENT cursor/history state, so tests
    /// can stage prompt-dependent gestures without a live shell.
    #[cfg(test)]
    pub fn push_mark_for_test(&self, kind: crate::shell_integration::OscEvent, col_rec: usize) {
        let mut term = self.term.lock();
        let line_rec = term.grid().cursor.point.line.0;
        let clock_rec = self.clock.lock().unwrap().tick(&mut term);
        drop(term);
        self.marks.lock().unwrap().push(StoredMark {
            id: next_mark_id(),
            kind,
            line_rec,
            clock_rec,
            col_rec,
            dur: None,
        });
    }

    /// Test-only: report a finished command as the reader thread does at
    /// its `133;D` mark, so app tests can end a pane's command without a
    /// shell that speaks OSC 133.
    #[cfg(test)]
    pub fn finish_command_for_test(&self, exit: Option<i32>, dur: std::time::Duration) {
        let _ = self.finished_tx_for_test.send(FinishedCommand {
            exit,
            dur,
            cmd: String::new(),
            cwd: None,
            host: None,
            output: String::new(),
        });
    }
}

/// The typed text of the command now ending: from the newest PromptEnd
/// (`133;B`) cell to the line before the newest CommandStart (`133;C`) —
/// the same span `pair_decorations` derives after the fact. Runs in the
/// reader thread at the `133;D` mark with the term lock held; empty when
/// the B/C marks are missing (no shell integration on this pane).
pub(super) fn last_command_input_text(
    term: &Term<VoidListener>,
    marks: &[StoredMark],
    clock_now: i64,
) -> String {
    use crate::shell_integration::OscEvent as E;
    let cur = |m: &StoredMark| m.line_rec - (clock_now - m.clock_rec) as i32;
    let Some(ci) = marks
        .iter()
        .rposition(|m| matches!(m.kind, E::CommandStart))
    else {
        return String::new();
    };
    let Some(b) = marks[..ci]
        .iter()
        .rev()
        .find(|m| matches!(m.kind, E::PromptEnd))
    else {
        return String::new();
    };
    let bl = cur(b);
    let cl = cur(&marks[ci]);
    if cl <= bl {
        return String::new();
    }
    without_continuation_prompts(&extract_selection_text(
        term,
        bl,
        b.col_rec,
        cl - 1,
        term.columns().saturating_sub(1),
    ))
}

/// Typed input read off the screen, minus the shell's continuation prompt
/// on each line after the first (#1250). Only a hard newline starts such a
/// line, so a soft-wrapped row is never touched. The prompt is bash's `> `
/// or zsh's `%_> ` (`for> `, `cmdsubst dquote> `); stored as input, a
/// recalled command would run it as a redirect.
pub(super) fn without_continuation_prompts(text: &str) -> String {
    let strip = |line: &str| -> String {
        let is_ps2 = |head: &str| {
            !head.starts_with(' ') && head.chars().all(|c| c.is_ascii_lowercase() || c == ' ')
        };
        if let Some(i) = line.find("> ")
            && is_ps2(&line[..i])
        {
            return line[i + 2..].to_string();
        }
        match line.strip_suffix('>') {
            Some(head) if is_ps2(head) => String::new(),
            _ => line.to_string(),
        }
    };
    let mut lines = text.split('\n');
    let mut out = lines.next().unwrap_or_default().to_string();
    for line in lines {
        out.push('\n');
        out.push_str(&strip(line));
    }
    out
}

/// The finished command's OUTPUT: the rows from `start`, its first output
/// row in current grid coordinates, to the row the cursor sits on as its
/// `133;D` arrives. Escape-free (the grid never stores escapes), tail-capped
/// at [`FINISHED_OUTPUT_CAP_LINES`].
pub(super) fn command_output_text(term: &Term<VoidListener>, start: i32) -> String {
    // The cursor row is INCLUDED: `133;D` moves no cursor, so a diagnostic
    // printed without a trailing newline leaves the mark ON that row — the
    // old `- 1` dropped exactly the line the matchers needed. With a
    // trailing newline the cursor row is a fresh blank, trimmed below.
    let end = term.grid().cursor.point.line.0;
    if end < start {
        return String::new();
    }
    let text = extract_selection_text(term, start, 0, end, term.columns().saturating_sub(1));
    let text = text.trim_end_matches('\n');
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > FINISHED_OUTPUT_CAP_LINES {
        lines[lines.len() - FINISHED_OUTPUT_CAP_LINES..].join("\n")
    } else {
        text.to_string()
    }
}
