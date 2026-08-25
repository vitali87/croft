//! Native modal (vim-style) editing for the editor pane.
//!
//! This module is a pure input state machine: it consumes [`KeyEvent`]s and
//! emits [`VimAction`]s describing the editing intent, without ever touching
//! an `Editor`. The application layer (`app::apply_vim_action`) maps those
//! intents onto the editor's existing primitives (`move_left`, `delete_lines`,
//! `yank_lines`, ...). Keeping the parser side-effect-free is what makes the
//! brittle parts — counts, operator-pending, text objects, `f`/`t`, search,
//! ex-commands — unit-testable in isolation.
//!
//! Scope is the practical daily-driver subset: Normal / Insert / Visual modes,
//! motions `h j k l w b e 0 ^ $ gg G f t F T ; ,`, operators `d y c` (plus the
//! doubled `dd yy cc` and the `iw`/`aw` text objects), `x`, `i a I A o O`,
//! `p P`, `u`, numeric counts, `/ ? n N` search, and `:` ex-commands. It is an
//! emulation, not embedded neovim: plugins and `init.lua` are out of scope (a
//! real `nvim` still runs in croft's shell pane for that).
//!
//! When vim mode is disabled the machine reports [`VimKeyResult::PassThrough`]
//! for every key, so the editor behaves exactly as before.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// High-level mode shown in the status bar and used to decide whether keys
/// reach the underlying editor (Insert) or are interpreted (everything else).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    /// Typing an ex command after `:`.
    Command,
    /// Typing a search pattern after `/` or `?`.
    Search,
}

/// The three operators in scope. Each combines with a motion, a text object,
/// or itself (the doubled linewise form `dd`/`yy`/`cc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Delete,
    Yank,
    Change,
}

/// Direction of a character search (`f`/`t` family) within the current line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindKind {
    /// `f` — land on the next occurrence to the right.
    CharForward,
    /// `t` — land just before the next occurrence to the right.
    TillForward,
    /// `F` — land on the previous occurrence to the left.
    CharBackward,
    /// `T` — land just after the previous occurrence to the left.
    TillBackward,
}

/// A cursor movement. Distances default to one cell; counts are carried
/// separately on the emitted action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    /// `0` — first column.
    LineStart,
    /// `^` — first non-blank column.
    FirstNonBlank,
    /// `$` — end of line.
    LineEnd,
    /// `gg` with no count.
    FileStart,
    /// `G` with no count.
    FileEnd,
    /// `{n}G` or `{n}gg` — jump to an absolute 1-based line.
    GotoLine(usize),
    /// `f`/`t`/`F`/`T` with the resolved target character.
    Find(FindKind, char),
    /// `;` — repeat the last `f`/`t` in its original direction.
    RepeatFind,
    /// `,` — repeat the last `f`/`t` reversed.
    RepeatFindRev,
}

/// The two text objects in scope, selected by `iw` / `aw`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextObject {
    /// `iw` — the word under the cursor, no surrounding whitespace.
    InnerWord,
    /// `aw` — the word under the cursor plus trailing whitespace.
    AWord,
}

/// What an operator acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Motion(Motion),
    /// The doubled-operator linewise form (`dd`, `yy`, `cc`).
    Lines,
    Object(TextObject),
}

/// Where `i a I A o O` place the cursor before entering Insert mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertAt {
    /// `i` — before the cursor.
    Cursor,
    /// `a` — after the cursor.
    After,
    /// `I` — first non-blank of the line.
    LineStart,
    /// `A` — end of the line.
    LineEnd,
    /// `o` — open a new line below.
    Below,
    /// `O` — open a new line above.
    Above,
}

/// Paste placement for `p` / `P`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteAt {
    /// `p` — after the cursor / below the line.
    After,
    /// `P` — before the cursor / above the line.
    Before,
}

/// Search direction for `/` `?` and the `n` `N` repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDir {
    Forward,
    Backward,
}

impl SearchDir {
    fn reversed(self) -> Self {
        match self {
            SearchDir::Forward => SearchDir::Backward,
            SearchDir::Backward => SearchDir::Forward,
        }
    }
}

/// A resolved editing intent. The application layer translates each variant
/// into calls on the editor; the vim machine never mutates a buffer itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VimAction {
    Move(Motion, usize),
    Operate {
        op: Op,
        target: Target,
        count: usize,
    },
    DeleteChar(usize),
    Insert(InsertAt),
    Paste(PasteAt, usize),
    Undo,
    Redo,
    EnterVisual {
        line: bool,
    },
    ExitVisual,
    /// Apply an operator to the active visual selection, then leave Visual.
    VisualOperate(Op),
    /// Leave Insert mode; the app clamps the caret one cell left (vim style).
    ExitInsert,
    /// Run an ex command (text after `:`, without the colon).
    RunEx(String),
    /// Execute a search for `pattern` in `dir` and move to the next hit.
    Search(SearchDir, String),
    /// Clear any pending operator/count and drop the visual selection.
    ClearPending,
    /// `q{a-z}` — start recording into a register, or `q` while recording
    /// stops it. The app owns the recording; the machine only routes the key.
    MacroRecord(Option<char>),
    /// `@{a-z}` with a count; `@@` replays the last macro (`None`).
    MacroReplay(Option<char>, usize),
}

/// The outcome of feeding one key to the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VimKeyResult {
    /// The key was interpreted; apply these actions and do not let the editor
    /// see the key. An empty vector means "consumed, nothing to do yet" (e.g.
    /// a pending operator or a digit of a count).
    Consumed(Vec<VimAction>),
    /// The key is not claimed by vim; the editor should handle it normally
    /// (Insert-mode typing, or any Cmd/Ctrl shortcut in Normal mode).
    PassThrough,
}

/// The modal editing state for the editor pane. One instance lives on `App`.
#[derive(Clone, Debug)]
pub struct VimState {
    pub enabled: bool,
    mode: VimMode,
    /// Count accumulated for the next motion/command (`None` = unset = 1).
    count: Option<usize>,
    /// Operator awaiting its target, set by `d`/`y`/`c` in Normal mode.
    pending_op: Option<Op>,
    /// Count captured at the moment the operator was pressed.
    op_count: Option<usize>,
    /// `f`/`t`/`F`/`T` awaiting their target character.
    pending_find: Option<FindKind>,
    /// `q` pressed in Normal mode, awaiting the register letter. Set only
    /// when not already recording — a second `q` stops instead.
    pending_record: bool,
    /// The APP's recording state, pushed in by [`VimState::set_recording`]
    /// after every change. Never written by the machine: `q` needs to know
    /// whether it starts or stops, and a copy the machine maintained itself
    /// drifted out of sync with the palette's recorder.
    app_recording: bool,
    /// `@` pressed, awaiting the register letter (or a second `@`).
    pending_replay: bool,
    /// After an operator, `i`/`a` was pressed and we await the object char.
    /// `Some(true)` = inner (`i`), `Some(false)` = around (`a`).
    pending_textobj: Option<bool>,
    /// `g` was pressed and we await a second `g` (for `gg`).
    pending_g: bool,
    /// Buffer for the `:` / `/` / `?` line currently being typed.
    cmdline: String,
    /// Direction of the search line currently being typed.
    search_dir: SearchDir,
    /// Last completed search, reused by `n` / `N`.
    last_search: Option<(SearchDir, String)>,
    /// Anchor of the active visual selection (row, col).
    visual_anchor: Option<(usize, usize)>,
}

impl Default for VimState {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: VimMode::Normal,
            count: None,
            pending_op: None,
            op_count: None,
            pending_find: None,
            pending_record: false,
            app_recording: false,
            pending_replay: false,
            pending_textobj: None,
            pending_g: false,
            cmdline: String::new(),
            search_dir: SearchDir::Forward,
            last_search: None,
            visual_anchor: None,
        }
    }
}

impl VimState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> VimMode {
        self.mode
    }

    /// Anchor used by the app to rebuild the visual selection after a motion.
    pub fn visual_anchor(&self) -> Option<(usize, usize)> {
        self.visual_anchor
    }

    /// Record where Visual mode started so motions can extend the selection.
    pub fn set_visual_anchor(&mut self, row: usize, col: usize) {
        self.visual_anchor = Some((row, col));
    }

    /// Short uppercase label for the status bar, or `None` while typing a
    /// `:`/`/`/`?` line (the caller shows [`Self::command_line`] instead).
    pub fn mode_label(&self) -> Option<&'static str> {
        match self.mode {
            VimMode::Normal => Some("NORMAL"),
            VimMode::Insert => Some("INSERT"),
            VimMode::Visual => Some("VISUAL"),
            VimMode::VisualLine => Some("V-LINE"),
            VimMode::Command | VimMode::Search => None,
        }
    }

    /// Tell the machine whether a macro recording is live. The app calls
    /// this after every start/stop from EITHER entry point (vim `q` or the
    /// palette), so `q` always branches on the real state instead of a copy
    /// the machine maintained for itself.
    pub fn set_recording(&mut self, recording: bool) {
        self.app_recording = recording;
        if !recording {
            self.pending_record = false;
        }
    }

    /// The full command/search line including its leading sigil, when one is
    /// being typed.
    pub fn command_line(&self) -> Option<String> {
        match self.mode {
            VimMode::Command => Some(format!(":{}", self.cmdline)),
            VimMode::Search => {
                let sigil = match self.search_dir {
                    SearchDir::Forward => '/',
                    SearchDir::Backward => '?',
                };
                Some(format!("{sigil}{}", self.cmdline))
            }
            _ => None,
        }
    }

    /// Toggle modal editing. Always resets to a clean Normal state so a
    /// half-typed command can't survive the flip.
    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.reset_to_normal();
    }

    /// Enter Insert mode. Used by the app after a change operator (`cw`,
    /// `ciw`, `cc`) where the deletion happens app-side and the machine must
    /// then accept typed text.
    pub fn enter_insert_mode(&mut self) {
        self.clear_pending();
        self.mode = VimMode::Insert;
    }

    fn reset_to_normal(&mut self) {
        self.mode = VimMode::Normal;
        self.clear_pending();
        self.visual_anchor = None;
        self.cmdline.clear();
    }

    fn clear_pending(&mut self) {
        self.count = None;
        self.pending_op = None;
        self.op_count = None;
        self.pending_find = None;
        self.pending_textobj = None;
        self.pending_g = false;
        self.pending_record = false;
        self.pending_replay = false;
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1)
    }

    /// Effective count for an operator+motion: the count before the operator
    /// multiplied by the count before the motion (vim's `2d3w` = 6 words).
    fn operator_eff(&mut self) -> usize {
        self.op_count.take().unwrap_or(1) * self.count.take().unwrap_or(1)
    }

    /// Apply the resolved motion: as an operator target if one is pending,
    /// otherwise as a plain cursor move (which the app extends in Visual).
    fn resolve_motion(&mut self, motion: Motion) -> VimKeyResult {
        self.pending_g = false;
        if let Some(op) = self.pending_op.take() {
            let count = self.operator_eff();
            return VimKeyResult::Consumed(vec![VimAction::Operate {
                op,
                target: Target::Motion(motion),
                count,
            }]);
        }
        let count = self.take_count();
        VimKeyResult::Consumed(vec![VimAction::Move(motion, count)])
    }

    fn exit_visual(&mut self) {
        self.mode = VimMode::Normal;
        self.visual_anchor = None;
        self.clear_pending();
    }

    /// Feed one key to the machine.
    pub fn on_key(&mut self, key: KeyEvent) -> VimKeyResult {
        if !self.enabled {
            return VimKeyResult::PassThrough;
        }
        match self.mode {
            VimMode::Insert => self.on_key_insert(key),
            VimMode::Command => self.on_key_cmdline(key, false),
            VimMode::Search => self.on_key_cmdline(key, true),
            VimMode::Normal | VimMode::Visual | VimMode::VisualLine => self.on_key_normal_like(key),
        }
    }

    fn on_key_insert(&mut self, key: KeyEvent) -> VimKeyResult {
        if key.code == KeyCode::Esc {
            self.mode = VimMode::Normal;
            return VimKeyResult::Consumed(vec![VimAction::ExitInsert]);
        }
        VimKeyResult::PassThrough
    }

    fn on_key_cmdline(&mut self, key: KeyEvent, is_search: bool) -> VimKeyResult {
        match key.code {
            KeyCode::Esc => {
                self.cmdline.clear();
                self.mode = VimMode::Normal;
                VimKeyResult::Consumed(vec![])
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.cmdline);
                self.mode = VimMode::Normal;
                if text.is_empty() {
                    return VimKeyResult::Consumed(vec![]);
                }
                if is_search {
                    self.last_search = Some((self.search_dir, text.clone()));
                    VimKeyResult::Consumed(vec![VimAction::Search(self.search_dir, text)])
                } else {
                    VimKeyResult::Consumed(vec![VimAction::RunEx(text)])
                }
            }
            KeyCode::Backspace => {
                if self.cmdline.pop().is_none() {
                    // Backspacing past the leading sigil cancels the line.
                    self.mode = VimMode::Normal;
                }
                VimKeyResult::Consumed(vec![])
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.cmdline.push(c);
                VimKeyResult::Consumed(vec![])
            }
            _ => VimKeyResult::Consumed(vec![]),
        }
    }

    fn on_key_normal_like(&mut self, key: KeyEvent) -> VimKeyResult {
        // A pending f/t/F/T consumes the very next character as its target.
        if let Some(kind) = self.pending_find {
            self.pending_find = None;
            if let Some(c) = plain_char(key) {
                return self.resolve_motion(Motion::Find(kind, c));
            }
            self.clear_pending();
            return VimKeyResult::Consumed(vec![]);
        }
        // A pending text object (after operator + i/a) expects the object char.
        if let Some(inner) = self.pending_textobj {
            self.pending_textobj = None;
            if let (KeyCode::Char('w'), Some(op)) = (key.code, self.pending_op.take()) {
                let obj = if inner {
                    TextObject::InnerWord
                } else {
                    TextObject::AWord
                };
                let count = self.operator_eff();
                return VimKeyResult::Consumed(vec![VimAction::Operate {
                    op,
                    target: Target::Object(obj),
                    count,
                }]);
            }
            self.clear_pending();
            return VimKeyResult::Consumed(vec![]);
        }

        match key.code {
            KeyCode::Esc => {
                if matches!(self.mode, VimMode::Visual | VimMode::VisualLine) {
                    self.exit_visual();
                    return VimKeyResult::Consumed(vec![VimAction::ExitVisual]);
                }
                self.clear_pending();
                return VimKeyResult::Consumed(vec![VimAction::ClearPending]);
            }
            KeyCode::Up => return self.resolve_motion(Motion::Up),
            KeyCode::Down => return self.resolve_motion(Motion::Down),
            KeyCode::Left => return self.resolve_motion(Motion::Left),
            KeyCode::Right => return self.resolve_motion(Motion::Right),
            _ => {}
        }

        // Ctrl+R: redo (vim's redo). Normal mode with nothing pending; it
        // arrives as a Ctrl chord that `plain_char` would otherwise drop to
        // PassThrough. `Cmd/Ctrl+Shift+Z` still works too via the editor chord.
        if self.mode == VimMode::Normal
            && self.pending_op.is_none()
            && !self.pending_g
            && matches!(key.code, KeyCode::Char('r' | 'R'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.clear_pending();
            return VimKeyResult::Consumed(vec![VimAction::Redo]);
        }

        let Some(c) = plain_char(key) else {
            // Cmd/Ctrl/Alt chords and unhandled special keys fall through so
            // the editor's own shortcuts (save, copy, undo) keep working.
            return VimKeyResult::PassThrough;
        };

        // Second `g` of a `gg`, honoured under a pending operator too.
        if self.pending_g {
            self.pending_g = false;
            if c == 'g' {
                let m = match self.count.take() {
                    Some(n) => Motion::GotoLine(n),
                    None => Motion::FileStart,
                };
                return self.resolve_motion(m);
            }
            self.clear_pending();
            return VimKeyResult::Consumed(vec![]);
        }

        if self.pending_op.is_some() {
            return self.on_operator_pending_char(c);
        }

        if matches!(self.mode, VimMode::Visual | VimMode::VisualLine) {
            match c {
                'd' | 'x' => {
                    self.exit_visual();
                    return VimKeyResult::Consumed(vec![VimAction::VisualOperate(Op::Delete)]);
                }
                'y' => {
                    self.exit_visual();
                    return VimKeyResult::Consumed(vec![VimAction::VisualOperate(Op::Yank)]);
                }
                'c' | 's' => {
                    self.visual_anchor = None;
                    self.clear_pending();
                    self.mode = VimMode::Insert;
                    return VimKeyResult::Consumed(vec![VimAction::VisualOperate(Op::Change)]);
                }
                'v' => {
                    self.exit_visual();
                    return VimKeyResult::Consumed(vec![VimAction::ExitVisual]);
                }
                'V' => {
                    self.mode = VimMode::VisualLine;
                    return VimKeyResult::Consumed(vec![]);
                }
                'i' | 'a' | 'I' | 'A' | 'o' | 'O' | 'p' | 'P' | 'u' | ':' => {
                    return VimKeyResult::Consumed(vec![]);
                }
                _ => {}
            }
        }

        self.on_normal_char(c)
    }

    fn on_operator_pending_char(&mut self, c: char) -> VimKeyResult {
        // Digit: `0` with no count is the line-start motion (d0); else count.
        if c.is_ascii_digit() {
            if c == '0' && self.count.is_none() {
                return self.resolve_motion(Motion::LineStart);
            }
            let d = c as usize - '0' as usize;
            self.count = Some(self.count.unwrap_or(0) * 10 + d);
            return VimKeyResult::Consumed(vec![]);
        }
        // Doubled operator (dd / yy / cc) is linewise.
        let op = self.pending_op.expect("operator pending");
        if c == op_char(op) {
            self.pending_op = None;
            let count = self.operator_eff();
            return VimKeyResult::Consumed(vec![VimAction::Operate {
                op,
                target: Target::Lines,
                count,
            }]);
        }
        match c {
            'i' => {
                self.pending_textobj = Some(true);
                VimKeyResult::Consumed(vec![])
            }
            'a' => {
                self.pending_textobj = Some(false);
                VimKeyResult::Consumed(vec![])
            }
            'g' => {
                self.pending_g = true;
                VimKeyResult::Consumed(vec![])
            }
            'f' => self.arm_find(FindKind::CharForward),
            't' => self.arm_find(FindKind::TillForward),
            'F' => self.arm_find(FindKind::CharBackward),
            'T' => self.arm_find(FindKind::TillBackward),
            'G' => {
                let m = match self.count.take() {
                    Some(n) => Motion::GotoLine(n),
                    None => Motion::FileEnd,
                };
                self.resolve_motion(m)
            }
            ';' => self.resolve_motion(Motion::RepeatFind),
            ',' => self.resolve_motion(Motion::RepeatFindRev),
            _ => {
                if let Some(m) = simple_motion(c) {
                    return self.resolve_motion(m);
                }
                self.clear_pending();
                VimKeyResult::Consumed(vec![])
            }
        }
    }

    fn on_normal_char(&mut self, c: char) -> VimKeyResult {
        // A pending register letter consumes the NEXT key whatever it is, so
        // it must be read before digits and motions — `q1` names register
        // `1`, it does not start a count.
        if self.pending_record {
            self.pending_record = false;
            if c.is_ascii_alphanumeric() {
                return VimKeyResult::Consumed(vec![VimAction::MacroRecord(Some(c))]);
            }
            // Not a register name: swallow, like every other bad gesture.
            return VimKeyResult::Consumed(vec![]);
        }
        if self.pending_replay {
            self.pending_replay = false;
            let count = self.take_count();
            // `@@` repeats the last macro; `@{a-z}` names a register.
            if c == '@' {
                return VimKeyResult::Consumed(vec![VimAction::MacroReplay(None, count)]);
            }
            if c.is_ascii_alphanumeric() {
                return VimKeyResult::Consumed(vec![VimAction::MacroReplay(Some(c), count)]);
            }
            return VimKeyResult::Consumed(vec![]);
        }

        if c.is_ascii_digit() {
            if c == '0' && self.count.is_none() {
                return self.resolve_motion(Motion::LineStart);
            }
            let d = c as usize - '0' as usize;
            self.count = Some(self.count.unwrap_or(0) * 10 + d);
            return VimKeyResult::Consumed(vec![]);
        }

        if let Some(m) = simple_motion(c) {
            return self.resolve_motion(m);
        }

        match c {
            'i' => self.enter_insert(InsertAt::Cursor),
            'a' => self.enter_insert(InsertAt::After),
            'I' => self.enter_insert(InsertAt::LineStart),
            'A' => self.enter_insert(InsertAt::LineEnd),
            'o' => self.enter_insert(InsertAt::Below),
            'O' => self.enter_insert(InsertAt::Above),
            'G' => {
                let m = match self.count.take() {
                    Some(n) => Motion::GotoLine(n),
                    None => Motion::FileEnd,
                };
                self.resolve_motion(m)
            }
            'q' => {
                // Vim's stop gesture is a BARE `q`, so whether this key needs
                // a register letter depends on the app's recording state. The
                // app pushes that in via `set_recording` rather than the
                // machine keeping its own copy — mirroring it desynchronised
                // the two entry points, so a palette Stop left vim believing
                // it was still recording.
                if self.app_recording {
                    VimKeyResult::Consumed(vec![VimAction::MacroRecord(None)])
                } else {
                    self.pending_record = true;
                    VimKeyResult::Consumed(vec![])
                }
            }
            '@' => {
                self.pending_replay = true;
                VimKeyResult::Consumed(vec![])
            }
            'g' => {
                self.pending_g = true;
                VimKeyResult::Consumed(vec![])
            }
            'x' => {
                let count = self.take_count();
                VimKeyResult::Consumed(vec![VimAction::DeleteChar(count)])
            }
            'd' => self.arm_operator(Op::Delete),
            'y' => self.arm_operator(Op::Yank),
            'c' => self.arm_operator(Op::Change),
            'p' => {
                let count = self.take_count();
                VimKeyResult::Consumed(vec![VimAction::Paste(PasteAt::After, count)])
            }
            'P' => {
                let count = self.take_count();
                VimKeyResult::Consumed(vec![VimAction::Paste(PasteAt::Before, count)])
            }
            'u' => {
                self.clear_pending();
                VimKeyResult::Consumed(vec![VimAction::Undo])
            }
            'v' => {
                self.clear_pending();
                self.mode = VimMode::Visual;
                VimKeyResult::Consumed(vec![VimAction::EnterVisual { line: false }])
            }
            'V' => {
                self.clear_pending();
                self.mode = VimMode::VisualLine;
                VimKeyResult::Consumed(vec![VimAction::EnterVisual { line: true }])
            }
            'f' => self.arm_find(FindKind::CharForward),
            't' => self.arm_find(FindKind::TillForward),
            'F' => self.arm_find(FindKind::CharBackward),
            'T' => self.arm_find(FindKind::TillBackward),
            ';' => {
                let count = self.take_count();
                VimKeyResult::Consumed(vec![VimAction::Move(Motion::RepeatFind, count)])
            }
            ',' => {
                let count = self.take_count();
                VimKeyResult::Consumed(vec![VimAction::Move(Motion::RepeatFindRev, count)])
            }
            '/' => self.enter_search(SearchDir::Forward),
            '?' => self.enter_search(SearchDir::Backward),
            ':' => {
                self.clear_pending();
                self.cmdline.clear();
                self.mode = VimMode::Command;
                VimKeyResult::Consumed(vec![])
            }
            'n' => match self.last_search.clone() {
                Some((dir, term)) => VimKeyResult::Consumed(vec![VimAction::Search(dir, term)]),
                None => VimKeyResult::Consumed(vec![]),
            },
            'N' => match self.last_search.clone() {
                Some((dir, term)) => {
                    VimKeyResult::Consumed(vec![VimAction::Search(dir.reversed(), term)])
                }
                None => VimKeyResult::Consumed(vec![]),
            },
            _ => {
                // Unmapped printable key: swallow so it never lands in the
                // buffer while in Normal mode.
                self.clear_pending();
                VimKeyResult::Consumed(vec![])
            }
        }
    }

    fn enter_insert(&mut self, at: InsertAt) -> VimKeyResult {
        self.clear_pending();
        self.mode = VimMode::Insert;
        VimKeyResult::Consumed(vec![VimAction::Insert(at)])
    }

    fn arm_operator(&mut self, op: Op) -> VimKeyResult {
        self.op_count = self.count.take();
        self.pending_op = Some(op);
        VimKeyResult::Consumed(vec![])
    }

    fn arm_find(&mut self, kind: FindKind) -> VimKeyResult {
        self.pending_find = Some(kind);
        VimKeyResult::Consumed(vec![])
    }

    fn enter_search(&mut self, dir: SearchDir) -> VimKeyResult {
        self.clear_pending();
        self.cmdline.clear();
        self.search_dir = dir;
        self.mode = VimMode::Search;
        VimKeyResult::Consumed(vec![])
    }
}

/// The literal key that doubles an operator into its linewise form.
fn op_char(op: Op) -> char {
    match op {
        Op::Delete => 'd',
        Op::Yank => 'y',
        Op::Change => 'c',
    }
}

/// The single-key motions shared by Normal and operator-pending dispatch.
/// `0` is handled separately because it doubles as a count digit.
fn simple_motion(c: char) -> Option<Motion> {
    Some(match c {
        'h' => Motion::Left,
        'l' => Motion::Right,
        'j' => Motion::Down,
        'k' => Motion::Up,
        'w' => Motion::WordForward,
        'b' => Motion::WordBackward,
        'e' => Motion::WordEnd,
        '$' => Motion::LineEnd,
        '^' => Motion::FirstNonBlank,
        _ => return None,
    })
}

/// The char of an unmodified (or shift-only) key, or `None` for any key
/// carrying Ctrl/Alt/Cmd so those chords pass through to the editor.
fn plain_char(key: KeyEvent) -> Option<char> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }
    match key.code {
        KeyCode::Char(c) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn shift(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn enabled() -> VimState {
        let mut v = VimState::new();
        v.enabled = true;
        v
    }

    /// Drive a sequence of plain-char keys and return the actions emitted by
    /// the final key (earlier keys are expected to be pending no-ops).
    fn last_actions(v: &mut VimState, keys: &str) -> Vec<VimAction> {
        let mut out = Vec::new();
        for c in keys.chars() {
            out = match v.on_key(ch(c)) {
                VimKeyResult::Consumed(a) => a,
                VimKeyResult::PassThrough => Vec::new(),
            };
        }
        out
    }

    #[test]
    fn disabled_machine_passes_every_key_through() {
        let mut v = VimState::new();
        assert_eq!(v.on_key(ch('d')), VimKeyResult::PassThrough);
        assert_eq!(v.on_key(ch('i')), VimKeyResult::PassThrough);
    }

    #[test]
    fn starts_in_normal_mode_when_enabled() {
        assert_eq!(enabled().mode(), VimMode::Normal);
    }

    #[test]
    fn i_enters_insert_and_esc_returns_to_normal() {
        let mut v = enabled();
        assert_eq!(
            v.on_key(ch('i')),
            VimKeyResult::Consumed(vec![VimAction::Insert(InsertAt::Cursor)])
        );
        assert_eq!(v.mode(), VimMode::Insert);
        // A printable key in Insert mode falls through to the editor.
        assert_eq!(v.on_key(ch('x')), VimKeyResult::PassThrough);
        assert_eq!(
            v.on_key(key(KeyCode::Esc)),
            VimKeyResult::Consumed(vec![VimAction::ExitInsert])
        );
        assert_eq!(v.mode(), VimMode::Normal);
    }

    #[test]
    fn insert_variants_map_to_the_right_placement() {
        for (c, at) in [('a', InsertAt::After), ('o', InsertAt::Below)] {
            let mut v = enabled();
            assert_eq!(
                v.on_key(ch(c)),
                VimKeyResult::Consumed(vec![VimAction::Insert(at)])
            );
            assert_eq!(v.mode(), VimMode::Insert);
        }
        let mut v = enabled();
        assert_eq!(
            v.on_key(shift('I')),
            VimKeyResult::Consumed(vec![VimAction::Insert(InsertAt::LineStart)])
        );
        let mut v = enabled();
        assert_eq!(
            v.on_key(shift('A')),
            VimKeyResult::Consumed(vec![VimAction::Insert(InsertAt::LineEnd)])
        );
        let mut v = enabled();
        assert_eq!(
            v.on_key(shift('O')),
            VimKeyResult::Consumed(vec![VimAction::Insert(InsertAt::Above)])
        );
    }

    #[test]
    fn hjkl_emit_single_step_motions() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "h"),
            vec![VimAction::Move(Motion::Left, 1)]
        );
        assert_eq!(
            last_actions(&mut v, "j"),
            vec![VimAction::Move(Motion::Down, 1)]
        );
        assert_eq!(
            last_actions(&mut v, "k"),
            vec![VimAction::Move(Motion::Up, 1)]
        );
        assert_eq!(
            last_actions(&mut v, "l"),
            vec![VimAction::Move(Motion::Right, 1)]
        );
    }

    #[test]
    fn digits_build_a_count_for_the_next_motion() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "3j"),
            vec![VimAction::Move(Motion::Down, 3)]
        );
        // Multi-digit.
        assert_eq!(
            last_actions(&mut v, "12k"),
            vec![VimAction::Move(Motion::Up, 12)]
        );
    }

    #[test]
    fn zero_is_line_start_without_count_but_extends_an_existing_count() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "0"),
            vec![VimAction::Move(Motion::LineStart, 1)]
        );
        // 10j, not (line-start then j).
        assert_eq!(
            last_actions(&mut v, "10j"),
            vec![VimAction::Move(Motion::Down, 10)]
        );
    }

    #[test]
    fn word_motions() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "w"),
            vec![VimAction::Move(Motion::WordForward, 1)]
        );
        assert_eq!(
            last_actions(&mut v, "b"),
            vec![VimAction::Move(Motion::WordBackward, 1)]
        );
        assert_eq!(
            last_actions(&mut v, "e"),
            vec![VimAction::Move(Motion::WordEnd, 1)]
        );
    }

    #[test]
    fn dollar_and_caret() {
        let mut v = enabled();
        assert_eq!(
            v.on_key(ch('$')),
            VimKeyResult::Consumed(vec![VimAction::Move(Motion::LineEnd, 1)])
        );
        assert_eq!(
            v.on_key(ch('^')),
            VimKeyResult::Consumed(vec![VimAction::Move(Motion::FirstNonBlank, 1)])
        );
    }

    #[test]
    fn gg_and_capital_g_navigate_the_file() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "gg"),
            vec![VimAction::Move(Motion::FileStart, 1)]
        );
        let mut v = enabled();
        assert_eq!(
            v.on_key(shift('G')),
            VimKeyResult::Consumed(vec![VimAction::Move(Motion::FileEnd, 1)])
        );
    }

    #[test]
    fn count_with_capital_g_goes_to_an_absolute_line() {
        let mut v = enabled();
        // 5G
        assert_eq!(v.on_key(ch('5')), VimKeyResult::Consumed(vec![]));
        assert_eq!(
            v.on_key(shift('G')),
            VimKeyResult::Consumed(vec![VimAction::Move(Motion::GotoLine(5), 1)])
        );
    }

    #[test]
    fn x_deletes_chars_under_cursor_with_count() {
        let mut v = enabled();
        assert_eq!(
            v.on_key(ch('x')),
            VimKeyResult::Consumed(vec![VimAction::DeleteChar(1)])
        );
        let mut v = enabled();
        assert_eq!(last_actions(&mut v, "3x"), vec![VimAction::DeleteChar(3)]);
    }

    #[test]
    fn dd_yy_cc_are_linewise_operators() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "dd"),
            vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Lines,
                count: 1
            }]
        );
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "yy"),
            vec![VimAction::Operate {
                op: Op::Yank,
                target: Target::Lines,
                count: 1
            }]
        );
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "cc"),
            vec![VimAction::Operate {
                op: Op::Change,
                target: Target::Lines,
                count: 1
            }]
        );
    }

    #[test]
    fn count_before_dd_multiplies_the_lines() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "3dd"),
            vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Lines,
                count: 3
            }]
        );
    }

    #[test]
    fn operator_plus_motion() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "dw"),
            vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Motion(Motion::WordForward),
                count: 1
            }]
        );
        let mut v = enabled();
        assert_eq!(v.on_key(ch('d')), VimKeyResult::Consumed(vec![]));
        assert_eq!(
            v.on_key(ch('$')),
            VimKeyResult::Consumed(vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Motion(Motion::LineEnd),
                count: 1
            }])
        );
    }

    #[test]
    fn operator_motion_counts_multiply() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "2d3w"),
            vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Motion(Motion::WordForward),
                count: 6
            }]
        );
    }

    #[test]
    fn inner_and_around_word_text_objects() {
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "diw"),
            vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Object(TextObject::InnerWord),
                count: 1
            }]
        );
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "ciw"),
            vec![VimAction::Operate {
                op: Op::Change,
                target: Target::Object(TextObject::InnerWord),
                count: 1
            }]
        );
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "daw"),
            vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Object(TextObject::AWord),
                count: 1
            }]
        );
    }

    #[test]
    fn paste_after_and_before() {
        let mut v = enabled();
        assert_eq!(
            v.on_key(ch('p')),
            VimKeyResult::Consumed(vec![VimAction::Paste(PasteAt::After, 1)])
        );
        let mut v = enabled();
        assert_eq!(
            v.on_key(shift('P')),
            VimKeyResult::Consumed(vec![VimAction::Paste(PasteAt::Before, 1)])
        );
        let mut v = enabled();
        assert_eq!(
            last_actions(&mut v, "2p"),
            vec![VimAction::Paste(PasteAt::After, 2)]
        );
    }

    #[test]
    fn u_undoes() {
        let mut v = enabled();
        assert_eq!(
            v.on_key(ch('u')),
            VimKeyResult::Consumed(vec![VimAction::Undo])
        );
    }

    #[test]
    fn ctrl_r_redoes() {
        let mut v = enabled();
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(
            v.on_key(ctrl_r),
            VimKeyResult::Consumed(vec![VimAction::Redo])
        );
    }

    #[test]
    fn visual_mode_enter_motion_operate_exit() {
        let mut v = enabled();
        assert_eq!(
            v.on_key(ch('v')),
            VimKeyResult::Consumed(vec![VimAction::EnterVisual { line: false }])
        );
        assert_eq!(v.mode(), VimMode::Visual);
        // Motions inside visual mode still emit Move (app extends selection).
        assert_eq!(
            v.on_key(ch('l')),
            VimKeyResult::Consumed(vec![VimAction::Move(Motion::Right, 1)])
        );
        // d operates on the selection and leaves visual mode.
        assert_eq!(
            v.on_key(ch('d')),
            VimKeyResult::Consumed(vec![VimAction::VisualOperate(Op::Delete)])
        );
        assert_eq!(v.mode(), VimMode::Normal);
    }

    #[test]
    fn visual_line_mode_and_escape() {
        let mut v = enabled();
        assert_eq!(
            v.on_key(shift('V')),
            VimKeyResult::Consumed(vec![VimAction::EnterVisual { line: true }])
        );
        assert_eq!(v.mode(), VimMode::VisualLine);
        assert_eq!(
            v.on_key(key(KeyCode::Esc)),
            VimKeyResult::Consumed(vec![VimAction::ExitVisual])
        );
        assert_eq!(v.mode(), VimMode::Normal);
    }

    #[test]
    fn find_char_forward_and_till() {
        let mut v = enabled();
        assert_eq!(v.on_key(ch('f')), VimKeyResult::Consumed(vec![]));
        assert_eq!(
            v.on_key(ch('x')),
            VimKeyResult::Consumed(vec![VimAction::Move(
                Motion::Find(FindKind::CharForward, 'x'),
                1
            )])
        );
        let mut v = enabled();
        assert_eq!(v.on_key(ch('t')), VimKeyResult::Consumed(vec![]));
        assert_eq!(
            v.on_key(ch(',')),
            VimKeyResult::Consumed(vec![VimAction::Move(
                Motion::Find(FindKind::TillForward, ','),
                1
            )])
        );
    }

    #[test]
    fn operator_with_find_char() {
        let mut v = enabled();
        // dft -> delete to (and including) the next 't'
        assert_eq!(v.on_key(ch('d')), VimKeyResult::Consumed(vec![]));
        assert_eq!(v.on_key(ch('f')), VimKeyResult::Consumed(vec![]));
        assert_eq!(
            v.on_key(ch(';')),
            VimKeyResult::Consumed(vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Motion(Motion::Find(FindKind::CharForward, ';')),
                count: 1
            }])
        );
    }

    #[test]
    fn repeat_find_with_semicolon_and_comma() {
        let mut v = enabled();
        // Establish a find first.
        v.on_key(ch('f'));
        v.on_key(ch('q'));
        assert_eq!(
            v.on_key(ch(';')),
            VimKeyResult::Consumed(vec![VimAction::Move(Motion::RepeatFind, 1)])
        );
        assert_eq!(
            v.on_key(ch(',')),
            VimKeyResult::Consumed(vec![VimAction::Move(Motion::RepeatFindRev, 1)])
        );
    }

    #[test]
    fn forward_search_accumulates_then_runs_on_enter() {
        let mut v = enabled();
        assert_eq!(v.on_key(ch('/')), VimKeyResult::Consumed(vec![]));
        assert_eq!(v.mode(), VimMode::Search);
        v.on_key(ch('f'));
        v.on_key(ch('o'));
        v.on_key(ch('o'));
        assert_eq!(v.command_line().as_deref(), Some("/foo"));
        assert_eq!(
            v.on_key(key(KeyCode::Enter)),
            VimKeyResult::Consumed(vec![VimAction::Search(
                SearchDir::Forward,
                String::from("foo")
            )])
        );
        assert_eq!(v.mode(), VimMode::Normal);
    }

    #[test]
    fn backward_search_uses_question_mark() {
        let mut v = enabled();
        v.on_key(ch('?'));
        v.on_key(ch('a'));
        assert_eq!(v.command_line().as_deref(), Some("?a"));
        assert_eq!(
            v.on_key(key(KeyCode::Enter)),
            VimKeyResult::Consumed(vec![VimAction::Search(
                SearchDir::Backward,
                String::from("a")
            )])
        );
    }

    #[test]
    fn search_escape_cancels_without_action() {
        let mut v = enabled();
        v.on_key(ch('/'));
        v.on_key(ch('z'));
        assert_eq!(v.on_key(key(KeyCode::Esc)), VimKeyResult::Consumed(vec![]));
        assert_eq!(v.mode(), VimMode::Normal);
    }

    #[test]
    fn n_and_capital_n_repeat_last_search_in_each_direction() {
        let mut v = enabled();
        v.on_key(ch('/'));
        v.on_key(ch('x'));
        v.on_key(key(KeyCode::Enter));
        assert_eq!(
            v.on_key(ch('n')),
            VimKeyResult::Consumed(vec![VimAction::Search(
                SearchDir::Forward,
                String::from("x")
            )])
        );
        assert_eq!(
            v.on_key(shift('N')),
            VimKeyResult::Consumed(vec![VimAction::Search(
                SearchDir::Backward,
                String::from("x")
            )])
        );
    }

    #[test]
    fn n_without_prior_search_is_a_noop() {
        let mut v = enabled();
        assert_eq!(v.on_key(ch('n')), VimKeyResult::Consumed(vec![]));
    }

    #[test]
    fn ex_command_runs_on_enter() {
        let mut v = enabled();
        assert_eq!(v.on_key(ch(':')), VimKeyResult::Consumed(vec![]));
        assert_eq!(v.mode(), VimMode::Command);
        v.on_key(ch('w'));
        v.on_key(ch('q'));
        assert_eq!(v.command_line().as_deref(), Some(":wq"));
        assert_eq!(
            v.on_key(key(KeyCode::Enter)),
            VimKeyResult::Consumed(vec![VimAction::RunEx(String::from("wq"))])
        );
        assert_eq!(v.mode(), VimMode::Normal);
    }

    #[test]
    fn command_line_backspace_edits_then_cancels_when_emptied() {
        let mut v = enabled();
        v.on_key(ch(':'));
        v.on_key(ch('w'));
        assert_eq!(
            v.on_key(key(KeyCode::Backspace)),
            VimKeyResult::Consumed(vec![])
        );
        // Backspacing past the sigil cancels and returns to Normal.
        assert_eq!(
            v.on_key(key(KeyCode::Backspace)),
            VimKeyResult::Consumed(vec![])
        );
        assert_eq!(v.mode(), VimMode::Normal);
    }

    #[test]
    fn modified_keys_pass_through_in_normal_mode() {
        let mut v = enabled();
        // Cmd+S etc. must reach the editor's normal handling untouched.
        let cmd_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::SUPER);
        assert_eq!(v.on_key(cmd_s), VimKeyResult::PassThrough);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(v.on_key(ctrl_c), VimKeyResult::PassThrough);
    }

    #[test]
    fn unmapped_printable_key_is_swallowed_in_normal_mode() {
        // So a stray letter never lands in the buffer while in Normal mode.
        let mut v = enabled();
        assert_eq!(v.on_key(ch('z')), VimKeyResult::Consumed(vec![]));
    }

    #[test]
    fn esc_in_normal_clears_a_pending_operator() {
        let mut v = enabled();
        v.on_key(ch('d'));
        assert_eq!(
            v.on_key(key(KeyCode::Esc)),
            VimKeyResult::Consumed(vec![VimAction::ClearPending])
        );
        // Subsequent dd should behave fresh.
        assert_eq!(
            last_actions(&mut v, "dd"),
            vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Lines,
                count: 1
            }]
        );
    }

    #[test]
    fn toggle_resets_state() {
        let mut v = enabled();
        v.on_key(ch('d')); // leave an operator pending
        v.toggle(); // now disabled
        assert!(!v.enabled);
        v.toggle(); // back on, must be clean Normal
        assert!(v.enabled);
        assert_eq!(v.mode(), VimMode::Normal);
        assert_eq!(
            last_actions(&mut v, "dd"),
            vec![VimAction::Operate {
                op: Op::Delete,
                target: Target::Lines,
                count: 1
            }]
        );
    }
}
