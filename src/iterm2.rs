use anyhow::{Context, Result};
use plist::{Dictionary, Value};
use std::path::{Path, PathBuf};

pub const ITERM2_PLIST_REL: &str = "Library/Preferences/com.googlecode.iterm2.plist";
const CMD_SHIFT_F_KEY: &str = "0x46-0x120000-0x3";
const CMD_SHIFT_F_HEX: &str = "0x1b 0x5b 0x37 0x30 0x3b 0x31 0x30 0x75";
const CMD_F_KEY: &str = "0x66-0x100000-0x3";
const CMD_F_HEX: &str = "0x1b 0x5b 0x31 0x30 0x32 0x3b 0x39 0x75";
const CMD_R_KEY: &str = "0x72-0x100000-0xf";
const CMD_R_HEX: &str = "0x1b 0x5b 0x31 0x31 0x34 0x3b 0x39 0x75";
/// `Cmd+Opt+R` -> "Reveal in Finder" (Explorer). Modifier mask 0x180000
/// (Cmd 0x100000 | Opt 0x80000), `kVK_ANSI_R` = 0xf. CSI-u `ESC [ 114 ; 11 u`
/// (114 = 'r'; modbyte 11 = 1 + Alt(2) + Super(8)) so crossterm decodes it as
/// ALT|SUPER, disjoint from plain Cmd+R (Rename). macOS does not bind this
/// chord to a default menu item, so no NSUserKeyEquivalents relocation is
/// needed for the forwarder to win.
const CMD_OPT_R_KEY: &str = "0x72-0x180000-0xf";
const CMD_OPT_R_HEX: &str = "0x1b 0x5b 0x31 0x31 0x34 0x3b 0x31 0x31 0x75";
const CMD_SLASH_KEY: &str = "0x2f-0x100000-0x2c";
const CMD_SLASH_HEX: &str = "0x1b 0x5b 0x34 0x37 0x3b 0x39 0x75";
/// `Cmd+Shift+Return`. Serialized identifier follows iTerm2's
/// iTermKeystroke format `0x<char>-0x<modifiers>-0x<virtualKeyCode>`:
/// character = 0xd (CR, the same unmodified value Return reports under
/// Shift), modifiers = 0x120000 (Cmd 0x100000 | Shift 0x20000),
/// virtualKeyCode = 0x24 (kVK_Return). Without this entry iTerm2 simply
/// drops the chord on the floor, which is why croft never saw it.
const CMD_SHIFT_ENTER_KEY: &str = "0xd-0x120000-0x24";
/// CSI-u sequence `ESC [ 13 ; 10 u` = Enter (codepoint 13) with kitty
/// modifier byte 10 (= 1 base + Shift(1) + Super(8)). Crossterm parses
/// this back into `KeyEvent { code: Enter, modifiers: SHIFT | SUPER }`,
/// which the Explorer's plain Enter handler already routes through
/// `tree.activate()` to toggle expand/collapse on folders.
const CMD_SHIFT_ENTER_HEX: &str = "0x1b 0x5b 0x31 0x33 0x3b 0x31 0x30 0x75";
const CMD_V_KEY: &str = "0x76-0x100000-0x9";
/// GlobalKeyMap keys + CSI-u payloads for the five Mac-style Cmd+letter
/// chords croft uses across panes (terminal copy / source-control
/// select-all / editor save / editor cut / editor undo). iTerm2's
/// `iTermApplication.sendEvent:` checks `GlobalKeyMap` ahead of the
/// NSResponder chain, so a forwarder here intercepts Cmd+letter before
/// AppKit's default copy:/cut:/selectAll:/undo: bindings consume it.
/// Each payload is `ESC [ <codepoint> ; 9 u`, where 9 = 1 base + Super(8)
/// in kitty's CSI-u modifier byte, which crossterm decodes into a
/// `KeyEvent { code: Char(letter), modifiers: SUPER }` that croft's
/// terminal/editor/tree handlers already accept.
const CMD_A_KEY: &str = "0x61-0x100000-0x0";
const CMD_A_HEX: &str = "0x1b 0x5b 0x39 0x37 0x3b 0x39 0x75";
const CMD_C_KEY: &str = "0x63-0x100000-0x8";
const CMD_C_HEX: &str = "0x1b 0x5b 0x39 0x39 0x3b 0x39 0x75";
const CMD_S_KEY: &str = "0x73-0x100000-0x1";
const CMD_S_HEX: &str = "0x1b 0x5b 0x31 0x31 0x35 0x3b 0x39 0x75";
const CMD_X_KEY: &str = "0x78-0x100000-0x7";
const CMD_X_HEX: &str = "0x1b 0x5b 0x31 0x32 0x30 0x3b 0x39 0x75";
const CMD_Z_KEY: &str = "0x7a-0x100000-0x6";
const CMD_Z_HEX: &str = "0x1b 0x5b 0x31 0x32 0x32 0x3b 0x39 0x75";
/// Vim-style chord starts and goto-bottom that the editor consumes.
/// CSI-u `ESC [ <codepoint> ; 9 u` for Cmd+letter and
/// `ESC [ <shifted-glyph> ; 10 u` for Cmd+Shift+letter; modifier byte
/// 9 = 1 base + Super(8), 10 adds Shift(1).
const CMD_D_KEY: &str = "0x64-0x100000-0x2";
const CMD_D_HEX: &str = "0x1b 0x5b 0x31 0x30 0x30 0x3b 0x39 0x75";
const CMD_G_KEY: &str = "0x67-0x100000-0x5";
const CMD_G_HEX: &str = "0x1b 0x5b 0x31 0x30 0x33 0x3b 0x39 0x75";
const CMD_Y_KEY: &str = "0x79-0x100000-0x10";
const CMD_Y_HEX: &str = "0x1b 0x5b 0x31 0x32 0x31 0x3b 0x39 0x75";
const CMD_O_KEY: &str = "0x6f-0x100000-0x1f";
const CMD_O_HEX: &str = "0x1b 0x5b 0x31 0x31 0x31 0x3b 0x39 0x75";
/// `Cmd+E` — toggle native modal (vim) editing in the editor pane (croft's
/// `is_vim_toggle_key`). iTerm2 inherits AppKit's standard Edit > Find
/// submenu, which binds the bare chord to "Use Selection for Find"; the
/// NSUserKeyEquivalents override below relocates that item to Cmd+Opt+E so
/// this GlobalKeyMap forwarder fires instead, exactly as Cmd+F (a sibling in
/// the same submenu) is handled. Codepoint 'e' (0x65 = 101), virtualKeyCode
/// `kVK_ANSI_E` = 0xe, modifier mask 0x100000 (Cmd). CSI-u `ESC [ 101 ; 9 u`
/// (modifier byte 9 = 1 base + Super(8)), which crossterm decodes back to
/// `KeyEvent { code: Char('e'), modifiers: SUPER }`.
const CMD_E_KEY: &str = "0x65-0x100000-0xe";
const CMD_E_HEX: &str = "0x1b 0x5b 0x31 0x30 0x31 0x3b 0x39 0x75";
/// AppKit / iTerm2 menu item that owns bare Cmd+E (Edit > Find > Use
/// Selection for Find) and the chord it is relocated to so croft can claim
/// Cmd+E. Cmd+Opt+E keeps the find-from-selection action reachable.
const USE_SELECTION_FOR_FIND_MENU_KEY: &str = "Use Selection for Find";
const USE_SELECTION_FOR_FIND_MENU_EQUIV: &str = "@~e";
const CMD_SHIFT_G_KEY: &str = "0x47-0x120000-0x5";
const CMD_SHIFT_G_HEX: &str = "0x1b 0x5b 0x37 0x31 0x3b 0x31 0x30 0x75";
const CMD_SHIFT_O_KEY: &str = "0x4f-0x120000-0x1f";
const CMD_SHIFT_O_HEX: &str = "0x1b 0x5b 0x37 0x39 0x3b 0x31 0x30 0x75";
/// `Cmd+Shift+E` — jump to Explorer sidebar from any pane. Codepoint 'E'
/// (0x45 = 69), virtualKeyCode `kVK_ANSI_E` = 0x0e, modifier byte 10 =
/// 1 base + Shift(1) + Super(8). CSI-u payload `ESC [ 69 ; 10 u`.
const CMD_SHIFT_E_KEY: &str = "0x45-0x120000-0xe";
const CMD_SHIFT_E_HEX: &str = "0x1b 0x5b 0x36 0x39 0x3b 0x31 0x30 0x75";
/// `Cmd+Shift+S` — jump to Source Control. Codepoint 'S' (0x53 = 83),
/// virtualKeyCode `kVK_ANSI_S` = 0x01. CSI-u `ESC [ 83 ; 10 u`.
const CMD_SHIFT_S_KEY: &str = "0x53-0x120000-0x1";
const CMD_SHIFT_S_HEX: &str = "0x1b 0x5b 0x38 0x33 0x3b 0x31 0x30 0x75";
/// `Cmd+Shift+D` — jump to Run and Debug. iTerm2 binds the bare chord to
/// "Split Horizontally with Same Profile"; the NSUserKeyEquivalents
/// override below relocates that menu item to Cmd+Opt+Shift+D so this
/// forwarder fires instead. Codepoint 'D' (0x44 = 68), virtualKeyCode
/// `kVK_ANSI_D` = 0x02. CSI-u `ESC [ 68 ; 10 u`.
const CMD_SHIFT_D_KEY: &str = "0x44-0x120000-0x2";
const CMD_SHIFT_D_HEX: &str = "0x1b 0x5b 0x36 0x38 0x3b 0x31 0x30 0x75";
/// `Cmd+Shift+R` — jump to Remote sidebar. Codepoint 'R' (0x52 = 82),
/// virtualKeyCode `kVK_ANSI_R` = 0x0f. CSI-u `ESC [ 82 ; 10 u`.
const CMD_SHIFT_R_KEY: &str = "0x52-0x120000-0xf";
const CMD_SHIFT_R_HEX: &str = "0x1b 0x5b 0x38 0x32 0x3b 0x31 0x30 0x75";
/// `Cmd+Shift+X` — jump to the Extensions sidebar view. Matches VS Code's
/// "View: Show Extensions". Cmd+Shift+X is unbound by any default macOS / iTerm2
/// menu item (verified 2026-06-20), so like Cmd+Shift+S no NSUserKeyEquivalents
/// relocation is needed; the GlobalKeyMap forwarder simply claims the chord.
/// Codepoint 'X' (0x58 = 88), virtualKeyCode `kVK_ANSI_X` = 0x07, modifier mask
/// 0x120000 (Cmd+Shift). CSI-u `ESC [ 88 ; 10 u` (modifier byte 10 = 1 base +
/// Shift(1) + Super(8)), which crossterm decodes back to
/// `KeyEvent { code: Char('X'), modifiers: SHIFT | SUPER }`, accepted
/// case-insensitively by `is_extensions_jump_key`.
const CMD_SHIFT_X_KEY: &str = "0x58-0x120000-0x7";
const CMD_SHIFT_X_HEX: &str = "0x1b 0x5b 0x38 0x38 0x3b 0x31 0x30 0x75";
/// `Cmd+Shift+L` — disconnect a remote session and drop back into the local
/// croft. Codepoint 'L' (0x4c = 76), virtualKeyCode `kVK_ANSI_L` = 0x25.
/// CSI-u `ESC [ 76 ; 10 u`. Forwarded defensively so AppKit / iTerm2 cannot
/// consume the chord before the remote croft's handler sees it.
const CMD_SHIFT_L_KEY: &str = "0x4c-0x120000-0x25";
const CMD_SHIFT_L_HEX: &str = "0x1b 0x5b 0x37 0x36 0x3b 0x31 0x30 0x75";
/// `Cmd+Shift+N` — Explorer "New Folder". Codepoint 'N' (0x4e = 78),
/// virtualKeyCode `kVK_ANSI_N` = 0x2d. CSI-u `ESC [ 78 ; 10 u`. Forwarded
/// defensively so AppKit / iTerm2 cannot consume the chord at the menu
/// layer before croft's Explorer-pane handler sees it.
const CMD_SHIFT_N_KEY: &str = "0x4e-0x120000-0x2d";
const CMD_SHIFT_N_HEX: &str = "0x1b 0x5b 0x37 0x38 0x3b 0x31 0x30 0x75";
/// `Cmd+Shift+T` — focus the Terminal pane from any pane. iTerm2 binds
/// the bare chord to "Restore Closed Session"; the NSUserKeyEquivalents
/// override below relocates that menu item to Cmd+Opt+Shift+T so this
/// forwarder fires instead. Codepoint 'T' (0x54 = 84), virtualKeyCode
/// `kVK_ANSI_T` = 0x11. CSI-u `ESC [ 84 ; 10 u`.
const CMD_SHIFT_T_KEY: &str = "0x54-0x120000-0x11";
const CMD_SHIFT_T_HEX: &str = "0x1b 0x5b 0x38 0x34 0x3b 0x31 0x30 0x75";
/// `Cmd+T` — open another terminal next to the active one (croft's
/// `is_terminal_split_key`). iTerm2 binds the bare chord to the "New Tab"
/// menu item (MainMenu.xib: `keyEquivalent="t"`); the NSUserKeyEquivalents
/// override below relocates it to Cmd+Ctrl+T so this forwarder wins.
/// Codepoint 't' (0x74 = 116), virtualKeyCode `kVK_ANSI_T` = 0x11. CSI-u
/// `ESC [ 116 ; 9 u`, modifier byte 9 = 1 base + Super(8), which crossterm
/// decodes back to `KeyEvent { code: Char('t'), modifiers: SUPER }`.
const CMD_T_KEY: &str = "0x74-0x100000-0x11";
const CMD_T_HEX: &str = "0x1b 0x5b 0x31 0x31 0x36 0x3b 0x39 0x75";
/// iTerm2's "New Tab" menu item title (MainMenu.xib id 867) and the chord
/// it is relocated to so croft can claim the bare Cmd+T. `@^t` = Cmd+Ctrl+T,
/// unbound by default and clear of the Cmd+Opt+T alternate ("New Tab Next
/// to Current Tab"), keeps the iTerm2 action reachable.
const NEW_TAB_MENU_KEY: &str = "New Tab";
const NEW_TAB_MENU_EQUIV: &str = "@^t";
/// `Cmd+[` / `Cmd+]` — cycle to the previous / next terminal (croft's
/// `is_terminal_cycle_back_key` / `is_terminal_cycle_key`). iTerm2 binds
/// the bare chords to "Previous Pane" / "Next Pane" (MainMenu.xib ids
/// 1251/1250, Command-only); the NSUserKeyEquivalents overrides below
/// relocate them to Cmd+Opt+[ / Cmd+Opt+] so these forwarders win.
/// Brackets are normal keys, so they use the 3-part `char-modifiers-keycode`
/// form (like Cmd+T), not the 2-part function-key form arrows use. '[' =
/// 0x5b, virtualKeyCode `kVK_ANSI_LeftBracket` = 0x21; ']' = 0x5d, vkc
/// `kVK_ANSI_RightBracket` = 0x1e. CSI-u `ESC [ 91 ; 9 u` / `ESC [ 93 ; 9 u`
/// (modifier byte 9 = 1 base + Super(8)), which crossterm decodes to
/// `Char('[' / ']') + SUPER`. Arrows are unusable for cycling: Ctrl+arrows
/// are eaten by the macOS Spaces shortcuts, Option+arrows are shell
/// word-motion, and Cmd+arrows are reserved by the user.
const CMD_LBRACKET_KEY: &str = "0x5b-0x100000-0x21";
const CMD_LBRACKET_HEX: &str = "0x1b 0x5b 0x39 0x31 0x3b 0x39 0x75";
const CMD_RBRACKET_KEY: &str = "0x5d-0x100000-0x1e";
const CMD_RBRACKET_HEX: &str = "0x1b 0x5b 0x39 0x33 0x3b 0x39 0x75";
/// `Cmd+\` -> split the editor into two side-by-side columns (VS Code's
/// `workbench.action.splitEditor`, croft's `is_editor_split_key`). Normal
/// key, so the 3-part `char-modifiers-keycode` form like Cmd+T: '\' = 0x5c,
/// Cmd modifier mask 0x100000, `kVK_ANSI_Backslash` = 0x2a. CSI-u
/// `ESC [ 92 ; 9 u` (modifier byte 9 = 1 base + Super(8)), which crossterm
/// decodes to `Char('\\') + SUPER`. Cmd+\ is not bound to any default
/// macOS / iTerm2 menu item, so no NSUserKeyEquivalents relocation is needed.
const CMD_BACKSLASH_KEY: &str = "0x5c-0x100000-0x2a";
const CMD_BACKSLASH_HEX: &str = "0x1b 0x5b 0x39 0x32 0x3b 0x39 0x75";
/// `Cmd+Shift+\` -> Go to Bracket (VS Code's `editor.action.jumpToBracket`,
/// croft's `is_goto_bracket_key`). Same `\` key as Cmd+\ above but with Shift
/// added to the modifier mask (Super 0x100000 | Shift 0x20000 = 0x120000).
/// CSI-u `ESC [ 92 ; 10 u` (modifier byte 10 = 1 base + Shift(1) + Super(8)),
/// which crossterm decodes to `Char('\\') + SHIFT | SUPER`. Not bound to any
/// default macOS / iTerm2 menu item, so no NSUserKeyEquivalents relocation.
const CMD_SHIFT_BACKSLASH_KEY: &str = "0x5c-0x120000-0x2a";
const CMD_SHIFT_BACKSLASH_HEX: &str = "0x1b 0x5b 0x39 0x32 0x3b 0x31 0x30 0x75";
/// `Cmd+Opt+\` -> Select to Bracket (VS Code's `editor.action.selectToBracket`,
/// croft's `is_select_to_bracket_key`). The `\` key with Cmd+Opt (Super
/// 0x100000 | Alt 0x80000 = 0x180000). CSI-u `ESC [ 92 ; 11 u` (modifier byte
/// 11 = 1 base + Alt(2) + Super(8)) -> `Char('\\') + ALT | SUPER`.
const CMD_OPT_BACKSLASH_KEY: &str = "0x5c-0x180000-0x2a";
const CMD_OPT_BACKSLASH_HEX: &str = "0x1b 0x5b 0x39 0x32 0x3b 0x31 0x31 0x75";
/// `Cmd+Opt+Shift+<letter>` for editor commands VS Code leaves unbound, so
/// croft binds them (tenet: everything has a shortcut). Modifier mask Super
/// 0x100000 | Alt 0x80000 | Shift 0x20000 = 0x1a0000; CSI-u modifier byte 12 =
/// 1 base + Shift(1) + Alt(2) + Super(8), which crossterm decodes to
/// `Char + SHIFT | ALT | SUPER`. `S` = Convert Indentation to Spaces, `T` =
/// Convert Indentation to Tabs, `N` = Trim Final Newlines.
const CMD_OPT_SHIFT_S_KEY: &str = "0x53-0x1a0000-0x1";
const CMD_OPT_SHIFT_S_HEX: &str = "0x1b 0x5b 0x38 0x33 0x3b 0x31 0x32 0x75";
const CMD_OPT_SHIFT_T_KEY: &str = "0x54-0x1a0000-0x11";
const CMD_OPT_SHIFT_T_HEX: &str = "0x1b 0x5b 0x38 0x34 0x3b 0x31 0x32 0x75";
const CMD_OPT_SHIFT_N_KEY: &str = "0x4e-0x1a0000-0x2d";
const CMD_OPT_SHIFT_N_HEX: &str = "0x1b 0x5b 0x37 0x38 0x3b 0x31 0x32 0x75";
// Same `Cmd+Opt+Shift+<letter>` tier (modifier byte 12) for the formerly
// palette-only editor commands, so none ships without an accelerator:
// `J` = Join Lines, `U`/`L`/`C` = Transform to Upper/Lower/Title case,
// `A`/`D` = Sort Lines Ascending/Descending, `W` = Trim Trailing Whitespace.
const CMD_OPT_SHIFT_J_KEY: &str = "0x4a-0x1a0000-0x26";
const CMD_OPT_SHIFT_J_HEX: &str = "0x1b 0x5b 0x37 0x34 0x3b 0x31 0x32 0x75";
const CMD_OPT_SHIFT_U_KEY: &str = "0x55-0x1a0000-0x20";
const CMD_OPT_SHIFT_U_HEX: &str = "0x1b 0x5b 0x38 0x35 0x3b 0x31 0x32 0x75";
const CMD_OPT_SHIFT_L_KEY: &str = "0x4c-0x1a0000-0x25";
const CMD_OPT_SHIFT_L_HEX: &str = "0x1b 0x5b 0x37 0x36 0x3b 0x31 0x32 0x75";
const CMD_OPT_SHIFT_C_KEY: &str = "0x43-0x1a0000-0x8";
const CMD_OPT_SHIFT_C_HEX: &str = "0x1b 0x5b 0x36 0x37 0x3b 0x31 0x32 0x75";
const CMD_OPT_SHIFT_A_KEY: &str = "0x41-0x1a0000-0x0";
const CMD_OPT_SHIFT_A_HEX: &str = "0x1b 0x5b 0x36 0x35 0x3b 0x31 0x32 0x75";
const CMD_OPT_SHIFT_D_KEY: &str = "0x44-0x1a0000-0x2";
const CMD_OPT_SHIFT_D_HEX: &str = "0x1b 0x5b 0x36 0x38 0x3b 0x31 0x32 0x75";
const CMD_OPT_SHIFT_W_KEY: &str = "0x57-0x1a0000-0xd";
const CMD_OPT_SHIFT_W_HEX: &str = "0x1b 0x5b 0x38 0x37 0x3b 0x31 0x32 0x75";
// `F` = Format Document (VS Code's `Shift+Alt+F`), CSI-u `ESC [ 70 ; 12 u`.
const CMD_OPT_SHIFT_F_KEY: &str = "0x46-0x1a0000-0x3";
const CMD_OPT_SHIFT_F_HEX: &str = "0x1b 0x5b 0x37 0x30 0x3b 0x31 0x32 0x75";
/// `Cmd+Opt+Left` / `Cmd+Opt+Right` -> move focus to the left / right
/// editor group while split (croft's `is_focus_group_left_key` /
/// `is_focus_group_right_key`). Arrows use the 2-part function-key form
/// `0x<unicode>-0x<modmask>`: `NSLeftArrowFunctionKey` = 0xf702,
/// `NSRightArrowFunctionKey` = 0xf703, Cmd+Opt mask 0x180000 (Cmd 0x100000
/// | Opt 0x80000). Payload is the legacy modified-arrow sequence
/// `ESC [ 1 ; 11 (D|C)` (modifier byte 11 = 1 base + Alt(2) + Super(8)),
/// which crossterm decodes to `KeyCode::Left/Right + ALT | SUPER` - the
/// same ALT|SUPER mask proven by `CMD_OPT_R` above, disjoint from the bare
/// `Opt+Left/Right` word-motion the shell consumes.
const CMD_OPT_LEFT_KEY: &str = "0xf702-0x180000";
const CMD_OPT_LEFT_HEX: &str = "0x1b 0x5b 0x31 0x3b 0x31 0x31 0x44";
const CMD_OPT_RIGHT_KEY: &str = "0xf703-0x180000";
const CMD_OPT_RIGHT_HEX: &str = "0x1b 0x5b 0x31 0x3b 0x31 0x31 0x43";
/// `Cmd+Opt+Up` / `Cmd+Opt+Down` -> Add Cursor Above / Below (VS Code
/// multi-cursor). Mirrors the Cmd+Opt+Left/Right forwarders above:
/// `NSUpArrowFunctionKey` = 0xf700, `NSDownArrowFunctionKey` = 0xf701, Cmd+Opt
/// mask 0x180000. Payload `ESC [ 1 ; 11 (A|B)` decodes to `KeyCode::Up/Down +
/// ALT | SUPER`, which `handle_editor_key` routes to `add_cursor_above/below`.
const CMD_OPT_UP_KEY: &str = "0xf700-0x180000";
const CMD_OPT_UP_HEX: &str = "0x1b 0x5b 0x31 0x3b 0x31 0x31 0x41";
const CMD_OPT_DOWN_KEY: &str = "0xf701-0x180000";
const CMD_OPT_DOWN_HEX: &str = "0x1b 0x5b 0x31 0x3b 0x31 0x31 0x42";
/// `Cmd+Shift+P` -> Command Palette. Mirrors the `Cmd+Shift+F` forwarder:
/// `kVK_ANSI_P` = 0x23, char 'P' = 0x50, modifier mask 0x120000 (Cmd | Shift).
/// CSI-u `ESC [ 80 ; 10 u` (codepoint 80 = 'P', modifier 10 = Super(8) +
/// Shift(1) + 1), which crossterm decodes to `Char('p') + SUPER | SHIFT`.
const CMD_SHIFT_P_KEY: &str = "0x50-0x120000-0x23";
const CMD_SHIFT_P_HEX: &str = "0x1b 0x5b 0x38 0x30 0x3b 0x31 0x30 0x75";
/// iTerm2 menu items that own bare `Cmd+[` / `Cmd+]`, relocated so the
/// bracket forwarders above reach croft. Cmd+Opt+[ / Cmd+Opt+] keep pane
/// navigation reachable.
const PREV_PANE_MENU_KEY: &str = "Previous Pane";
const PREV_PANE_MENU_EQUIV: &str = "@~[";
const NEXT_PANE_MENU_KEY: &str = "Next Pane";
const NEXT_PANE_MENU_EQUIV: &str = "@~]";
/// GlobalKeyMap arrow keys an earlier croft build hijacked for cycling
/// (both the 2-part form iTerm2 matches and the dead 3-part form). Cycling
/// now lives on Cmd+[ / Cmd+], so these are removed on apply to hand
/// Cmd+Left/Right back to iTerm2's defaults.
const CMD_ARROW_CLEANUP_KEYS: &[&str] = &[
    "0xf702-0x300000",
    "0xf703-0x300000",
    "0xf702-0x300000-0x7b",
    "0xf703-0x300000-0x7c",
];
/// `Ctrl+Shift+J` — toggle "maximize terminal" so the editor / welcome
/// pane collapses and the terminal fills the right column. Codepoint 'J'
/// (0x4a = 74), modifier mask 0x60000 = NSEventModifierFlagControl(0x40000)
/// + NSEventModifierFlagShift(0x20000), virtualKeyCode `kVK_ANSI_J` = 0x26.
///
/// CSI-u `ESC [ 74 ; 6 u` where modifier byte 6 = 1 base + Shift(1) +
/// Control(4). Crossterm decodes it back to
/// `KeyEvent { code: Char('J'), modifiers: CONTROL | SHIFT }`, which
/// `is_terminal_maximize_key` accepts case-insensitively. Forwarded
/// defensively to mirror the Cmd-chord pattern even though Ctrl+Shift+J
/// is not bound by AppKit / iTerm2 menus today, so a future iTerm2 build
/// that adds a default cannot silently swallow the chord.
const CTRL_SHIFT_J_KEY: &str = "0x4a-0x60000-0x26";
const CTRL_SHIFT_J_HEX: &str = "0x1b 0x5b 0x37 0x34 0x3b 0x36 0x75";
/// `Cmd+F12` -> editor "Go to Implementations" (VS Code's real macOS binding).
/// Function keys use the 2-part `0x<unicode>-0x<modmask>` iTermKeystroke form
/// (like the arrow-cleanup keys), not the 3-part letter form: `NSF12FunctionKey`
/// = 0xf70f, Cmd modifier mask 0x100000. The bare F12 family (plain / Shift /
/// Ctrl) reaches croft as escape sequences without a forwarder, but Cmd+F12 is
/// captured by macOS, so this forwarder is required.
///
/// Payload is the legacy modified-function-key sequence `ESC [ 24 ; 9 ~`
/// (`24` = F12, modifier byte 9 = 1 base + Super(8)), which crossterm decodes
/// back to `KeyEvent { code: F(12), modifiers: SUPER }` via the same
/// `parse_csi_special_key_code` path that already delivers Shift+F12 as
/// `ESC [ 24 ; 2 ~`. Cmd+F12 is not bound to any default macOS / iTerm2 menu
/// item, so no NSUserKeyEquivalents relocation is needed.
const CMD_F12_KEY: &str = "0xf70f-0x100000";
const CMD_F12_HEX: &str = "0x1b 0x5b 0x32 0x34 0x3b 0x39 0x7e";
/// `Ctrl+Shift+F12` -> editor "Go to Declaration". VS Code leaves Declaration
/// unbound, so croft keeps it in the F12 navigation family; it moved here off
/// the bare `Shift+F12` it once held when Go to References (VS Code's real
/// `Shift+F12`) was added. `NSF12FunctionKey` = 0xf70f, modifier mask 0x60000 =
/// NSEventModifierFlagControl(0x40000) + NSEventModifierFlagShift(0x20000).
///
/// Payload is the legacy modified-function-key sequence `ESC [ 24 ; 6 ~`
/// (`24` = F12, modifier byte 6 = 1 base + Shift(1) + Control(4)), which
/// crossterm decodes back to `KeyEvent { code: F(12), modifiers: SHIFT |
/// CONTROL }` via the same `parse_csi_special_key_code` path that delivers the
/// rest of the family. Unlike `Cmd+F12`, macOS does not reserve this chord, so
/// iTerm2 would already emit these exact bytes natively; the forwarder is
/// installed defensively (mirroring `Ctrl+Shift+J`) so a future iTerm2 default
/// cannot silently swallow the chord. No NSUserKeyEquivalents relocation needed.
const CTRL_SHIFT_F12_KEY: &str = "0xf70f-0x60000";
const CTRL_SHIFT_F12_HEX: &str = "0x1b 0x5b 0x32 0x34 0x3b 0x36 0x7e";
/// `Cmd+B` — toggle the primary side bar (croft's `is_sidebar_toggle_key`),
/// matching VS Code's "View: Toggle Primary Side Bar" macOS default. iTerm2
/// does not bind the bare chord to any default menu item (newer builds only
/// *suggest* Cmd+B as an opt-in leader key, which the user must enable), so
/// like Cmd+F12 no NSUserKeyEquivalents relocation is needed for this
/// forwarder to win. Codepoint 'b' (0x62 = 98), virtualKeyCode `kVK_ANSI_B`
/// = 0x0b, modifier mask 0x100000 (Cmd). CSI-u `ESC [ 98 ; 9 u` (modifier
/// byte 9 = 1 base + Super(8)), which crossterm decodes back to
/// `KeyEvent { code: Char('b'), modifiers: SUPER }`.
const CMD_B_KEY: &str = "0x62-0x100000-0xb";
const CMD_B_HEX: &str = "0x1b 0x5b 0x39 0x38 0x3b 0x39 0x75";
/// `Cmd+P`: VS Code-style Quick Open file finder. macOS binds Cmd+P to the
/// standard File > Print menu item across virtually every app (iTerm2
/// included), so AppKit catches the chord at the menu layer before
/// iTerm2's `GlobalKeyMap` is consulted. The NSUserKeyEquivalents
/// override below repoints File > Print at Cmd+Opt+P so this
/// GlobalKeyMap forwarder can fire.
/// Encoding: 'p' (codepoint 0x70 = 112) with Cmd, virtualKeyCode
/// `kVK_ANSI_P` = 0x23.
const CMD_P_KEY: &str = "0x70-0x100000-0x23";
/// CSI-u `ESC [ 112 ; 9 u` (= 0x1b 0x5b '1' '1' '2' ';' '9' 'u') so
/// crossterm decodes it back to `KeyEvent { code: Char('p'), modifiers: SUPER }`.
const CMD_P_HEX: &str = "0x1b 0x5b 0x31 0x31 0x32 0x3b 0x39 0x75";
/// Menu-item title macOS / iTerm2 expose for File > Print.
const PRINT_MENU_KEY: &str = "Print";
/// Cmd+Opt+P. Unbound by default in iTerm2; gives the user a path back to
/// the Print dialog when they need it without sacrificing the file finder.
const PRINT_MENU_EQUIV: &str = "@~p";
/// `Cmd+W` — close the active editor tab (croft's `is_close_tab_key`). iTerm2's
/// File menu binds the bare chord to "Close" (`closeCurrentSession:`), which
/// closes the current session and quits iTerm2 when it is the last one; that
/// menu key-equivalent is resolved by AppKit ahead of iTerm2's GlobalKeyMap,
/// so without relocating it the chord never reaches croft. Codepoint 'w'
/// (0x77 = 119), virtualKeyCode `kVK_ANSI_W` = 0xd, modifier mask 0x100000
/// (Cmd). CSI-u `ESC [ 119 ; 9 u` (modifier byte 9 = 1 base + Super(8)), which
/// crossterm decodes back to `KeyEvent { code: Char('w'), modifiers: SUPER }`.
const CMD_W_KEY: &str = "0x77-0x100000-0xd";
const CMD_W_HEX: &str = "0x1b 0x5b 0x31 0x31 0x39 0x3b 0x39 0x75";
/// iTerm2's File > "Close" menu item title (MainMenu.xib id 1184) and the chord
/// it is relocated to so croft can claim the bare Cmd+W. Cmd+Opt+W (`@~w`) is
/// already "Close All Panes in Tab" and Cmd+Shift+W is "Close Terminal Window",
/// so the close-session action moves to Cmd+Ctrl+W (`@^w`), unbound by default
/// and mirroring the `New Tab -> @^t` relocation.
const CLOSE_SESSION_MENU_KEY: &str = "Close";
const CLOSE_SESSION_MENU_EQUIV: &str = "@^w";
/// Cmd+0..Cmd+9 forward as CSI-u so the editor's vim chord can use them
/// as count digits (e.g. `Cmd+5 Cmd+g g` jumps to line 5). Without these,
/// iTerm2 catches Cmd+digit for its own "Select Tab N" action and croft
/// never sees the keystroke. Mac virtual key codes for the number row
/// are non-contiguous: `kVK_ANSI_1=0x12 … kVK_ANSI_5=0x17`, with 0 last
/// at `kVK_ANSI_0=0x1d`.
const CMD_DIGIT_CHORDS: &[(&str, &str)] = &[
    ("0x30-0x100000-0x1d", "0x1b 0x5b 0x34 0x38 0x3b 0x39 0x75"),
    ("0x31-0x100000-0x12", "0x1b 0x5b 0x34 0x39 0x3b 0x39 0x75"),
    ("0x32-0x100000-0x13", "0x1b 0x5b 0x35 0x30 0x3b 0x39 0x75"),
    ("0x33-0x100000-0x14", "0x1b 0x5b 0x35 0x31 0x3b 0x39 0x75"),
    ("0x34-0x100000-0x15", "0x1b 0x5b 0x35 0x32 0x3b 0x39 0x75"),
    ("0x35-0x100000-0x17", "0x1b 0x5b 0x35 0x33 0x3b 0x39 0x75"),
    ("0x36-0x100000-0x16", "0x1b 0x5b 0x35 0x34 0x3b 0x39 0x75"),
    ("0x37-0x100000-0x1a", "0x1b 0x5b 0x35 0x35 0x3b 0x39 0x75"),
    ("0x38-0x100000-0x1c", "0x1b 0x5b 0x35 0x36 0x3b 0x39 0x75"),
    ("0x39-0x100000-0x19", "0x1b 0x5b 0x35 0x37 0x3b 0x39 0x75"),
];
/// Top-level plist key that disables iTerm2's mouse-reporting-frustration
/// banner. Backed by iTermAdvancedSettingsModel's
/// `noSyncNeverAskAboutMouseReportingFrustration` property whose plist
/// storage key is PascalCase per `DEFINE_SETTABLE_BOOL`.
const MOUSE_REPORTING_FRUSTRATION_KEY: &str = "NoSyncNeverAskAboutMouseReportingFrustration";
/// `Cmd+Shift+/`. Character is the *shifted* glyph `?` (0x3f), modifiers
/// are Cmd+Shift (0x120000), virtualKeyCode is `kVK_ANSI_Slash` (0x2c).
/// macOS reserves this chord for the Help-menu Search field (Apple
/// writes it as Cmd+?). The `NSUserKeyEquivalents` override below
/// repoints "Show Help Menu" away from Cmd+?, freeing the chord so this
/// GlobalKeyMap forwarder can fire.
const CMD_SHIFT_SLASH_KEY: &str = "0x3f-0x120000-0x2c";
/// CSI-u sequence `ESC [ 63 ; 10 u` = '?' (codepoint 63) with kitty
/// modifier byte 10 (= 1 base + Shift(1) + Super(8)). Crossterm decodes
/// this back to `KeyEvent { code: Char('?'), modifiers: SHIFT | SUPER }`,
/// which `is_tree_make_parent_root_key` accepts via its `Char('?')`
/// branch.
const CMD_SHIFT_SLASH_HEX: &str = "0x1b 0x5b 0x36 0x33 0x3b 0x31 0x30 0x75";
const FIND_GLOBALLY_MENU_EQUIV: &str = "@~^f";
const FIND_MENU_EQUIV: &str = "@~f";
/// macOS calls the Help-menu Cmd+? Search shortcut "Show Help Menu" in
/// its keyboard-shortcuts UI; that is the menu-item title NSUserKeyEquivalents
/// recognizes. Setting it here in iTerm2's plist overrides the system
/// binding for iTerm2 only.
const HELP_MENU_KEY: &str = "Show Help Menu";
/// Cmd+Opt+? (i.e., Cmd+Opt+Shift+/). Picked because it is unbound by
/// default on macOS and Opt being held neutralizes croft's Explorer
/// predicate (which rejects ALT), so the chord can no longer fall back
/// into croft's parent-folder action either.
const HELP_MENU_EQUIV: &str = "@~?";

// (H) Pointer key earlier croft versions wrote to route Cmd+click to Go to
// Definition via a bracketed-paste sentinel; that transport never fired under
// croft's mouse reporting, so go-to-definition now keys on the Meta/Alt bit
// iTerm2 reports for Cmd+click. Re-running setup purges any stale copy.
const STALE_GOTO_DEF_POINTER_KEY: &str = "Button,0,1,c,";

/// CSI-u Send-Hex payloads exposed to the crate for the Ghostty keybind
/// generator (`crate::ghostty`). The byte sequences are defined exactly once,
/// in the const block above, as the single source of truth for what each croft
/// chord puts on the wire. Ghostty emits the identical bytes through its `csi:`
/// keybind action, so croft receives byte-for-byte the same input under Ghostty
/// as it does under iTerm2's GlobalKeyMap forwarders. A descendant module may
/// read its parent's private items, so these `pub(crate)` aliases copy the
/// private payloads without widening any original const's visibility.
pub(crate) mod payloads {
    pub(crate) const CMD_A_HEX: &str = super::CMD_A_HEX;
    pub(crate) const CMD_B_HEX: &str = super::CMD_B_HEX;
    pub(crate) const CMD_BACKSLASH_HEX: &str = super::CMD_BACKSLASH_HEX;
    pub(crate) const CMD_SHIFT_BACKSLASH_HEX: &str = super::CMD_SHIFT_BACKSLASH_HEX;
    pub(crate) const CMD_OPT_BACKSLASH_HEX: &str = super::CMD_OPT_BACKSLASH_HEX;
    pub(crate) const CMD_OPT_SHIFT_S_HEX: &str = super::CMD_OPT_SHIFT_S_HEX;
    pub(crate) const CMD_OPT_SHIFT_T_HEX: &str = super::CMD_OPT_SHIFT_T_HEX;
    pub(crate) const CMD_OPT_SHIFT_N_HEX: &str = super::CMD_OPT_SHIFT_N_HEX;
    pub(crate) const CMD_OPT_SHIFT_J_HEX: &str = super::CMD_OPT_SHIFT_J_HEX;
    pub(crate) const CMD_OPT_SHIFT_U_HEX: &str = super::CMD_OPT_SHIFT_U_HEX;
    pub(crate) const CMD_OPT_SHIFT_L_HEX: &str = super::CMD_OPT_SHIFT_L_HEX;
    pub(crate) const CMD_OPT_SHIFT_C_HEX: &str = super::CMD_OPT_SHIFT_C_HEX;
    pub(crate) const CMD_OPT_SHIFT_A_HEX: &str = super::CMD_OPT_SHIFT_A_HEX;
    pub(crate) const CMD_OPT_SHIFT_D_HEX: &str = super::CMD_OPT_SHIFT_D_HEX;
    pub(crate) const CMD_OPT_SHIFT_W_HEX: &str = super::CMD_OPT_SHIFT_W_HEX;
    pub(crate) const CMD_OPT_SHIFT_F_HEX: &str = super::CMD_OPT_SHIFT_F_HEX;
    pub(crate) const CMD_C_HEX: &str = super::CMD_C_HEX;
    pub(crate) const CMD_D_HEX: &str = super::CMD_D_HEX;
    pub(crate) const CMD_DIGIT_CHORDS: &[(&str, &str)] = super::CMD_DIGIT_CHORDS;
    pub(crate) const CMD_E_HEX: &str = super::CMD_E_HEX;
    pub(crate) const CMD_F12_HEX: &str = super::CMD_F12_HEX;
    pub(crate) const CMD_F_HEX: &str = super::CMD_F_HEX;
    pub(crate) const CMD_G_HEX: &str = super::CMD_G_HEX;
    pub(crate) const CMD_LBRACKET_HEX: &str = super::CMD_LBRACKET_HEX;
    pub(crate) const CMD_O_HEX: &str = super::CMD_O_HEX;
    pub(crate) const CMD_OPT_LEFT_HEX: &str = super::CMD_OPT_LEFT_HEX;
    pub(crate) const CMD_OPT_R_HEX: &str = super::CMD_OPT_R_HEX;
    pub(crate) const CMD_OPT_RIGHT_HEX: &str = super::CMD_OPT_RIGHT_HEX;
    pub(crate) const CMD_OPT_UP_HEX: &str = super::CMD_OPT_UP_HEX;
    pub(crate) const CMD_OPT_DOWN_HEX: &str = super::CMD_OPT_DOWN_HEX;
    pub(crate) const CMD_SHIFT_P_HEX: &str = super::CMD_SHIFT_P_HEX;
    pub(crate) const CMD_P_HEX: &str = super::CMD_P_HEX;
    pub(crate) const CMD_R_HEX: &str = super::CMD_R_HEX;
    pub(crate) const CMD_RBRACKET_HEX: &str = super::CMD_RBRACKET_HEX;
    pub(crate) const CMD_S_HEX: &str = super::CMD_S_HEX;
    pub(crate) const CMD_SHIFT_D_HEX: &str = super::CMD_SHIFT_D_HEX;
    pub(crate) const CMD_SHIFT_E_HEX: &str = super::CMD_SHIFT_E_HEX;
    pub(crate) const CMD_SHIFT_ENTER_HEX: &str = super::CMD_SHIFT_ENTER_HEX;
    pub(crate) const CMD_SHIFT_F_HEX: &str = super::CMD_SHIFT_F_HEX;
    pub(crate) const CMD_SHIFT_G_HEX: &str = super::CMD_SHIFT_G_HEX;
    pub(crate) const CMD_SHIFT_L_HEX: &str = super::CMD_SHIFT_L_HEX;
    pub(crate) const CMD_SHIFT_N_HEX: &str = super::CMD_SHIFT_N_HEX;
    pub(crate) const CMD_SHIFT_O_HEX: &str = super::CMD_SHIFT_O_HEX;
    pub(crate) const CMD_SHIFT_R_HEX: &str = super::CMD_SHIFT_R_HEX;
    pub(crate) const CMD_SHIFT_S_HEX: &str = super::CMD_SHIFT_S_HEX;
    pub(crate) const CMD_SHIFT_SLASH_HEX: &str = super::CMD_SHIFT_SLASH_HEX;
    pub(crate) const CMD_SHIFT_T_HEX: &str = super::CMD_SHIFT_T_HEX;
    pub(crate) const CMD_SHIFT_X_HEX: &str = super::CMD_SHIFT_X_HEX;
    pub(crate) const CMD_SLASH_HEX: &str = super::CMD_SLASH_HEX;
    pub(crate) const CMD_T_HEX: &str = super::CMD_T_HEX;
    pub(crate) const CMD_W_HEX: &str = super::CMD_W_HEX;
    pub(crate) const CMD_X_HEX: &str = super::CMD_X_HEX;
    pub(crate) const CMD_Y_HEX: &str = super::CMD_Y_HEX;
    pub(crate) const CMD_Z_HEX: &str = super::CMD_Z_HEX;
    pub(crate) const CTRL_SHIFT_F12_HEX: &str = super::CTRL_SHIFT_F12_HEX;
    pub(crate) const CTRL_SHIFT_J_HEX: &str = super::CTRL_SHIFT_J_HEX;
}

/// PostScript name iTerm2 stores in `Normal Font` and `Non Ascii Font`.
/// Format is "<PostScriptName> <size>".
pub fn primary_font_value(font_ps: &str, size: u32) -> String {
    format!("{font_ps} {size}")
}

#[derive(Debug, thiserror::Error)]
pub enum ITerm2Error {
    #[error("iTerm2 plist not found at {0}; install and launch iTerm2 first")]
    PlistMissing(PathBuf),
    #[error("iTerm2 plist has no `Default Bookmark Guid` (the default profile)")]
    NoDefaultGuid,
    #[error("iTerm2 plist has no profile matching the default GUID `{0}`")]
    NoMatchingProfile(String),
    #[error("iTerm2 plist top level is not a dictionary")]
    NotADictionary,
    #[error("`New Bookmarks` is missing or not an array")]
    NoBookmarksArray,
}

/// Apply font settings to the *default profile* in an iTerm2 plist value.
/// Pure function: no I/O, mutates the value in place.
pub fn apply_font_settings(
    plist: &mut Value,
    primary_font_ps: &str,
    nonascii_font_ps: &str,
    size: u32,
) -> Result<(), ITerm2Error> {
    let dict = plist
        .as_dictionary_mut()
        .ok_or(ITerm2Error::NotADictionary)?;

    let profile = default_profile_mut(dict)?;

    set_string(
        profile,
        "Normal Font",
        primary_font_value(primary_font_ps, size),
    );
    set_string(
        profile,
        "Non Ascii Font",
        primary_font_value(nonascii_font_ps, size),
    );
    profile.insert("Use Non-ASCII Font".into(), Value::Boolean(true));
    profile.insert("Non-ASCII Anti Aliased".into(), Value::Boolean(true));
    Ok(())
}

/// Apply the iTerm2-side pieces needed for Croft's macOS keyboard gestures.
/// Installs the Cmd+Shift+F search shortcut globally, frees the matching
/// menu equivalent so macOS doesn't eat it for "Find Globally...", and
/// scrubs any legacy Cmd+V or Paste-menu remappings that older croft
/// versions wrote in. Cmd+V is intentionally **not** bound: leaving it on
/// the default Edit menu shortcut routes through iTerm2's native Paste
/// action, which emits a bracketed-paste sequence carrying the local
/// clipboard. That works identically in local and SSH'd croft sessions
/// (croft handles `Event::Paste`); intercepting Cmd+V as a key event
/// instead — the previous design — broke paste over SSH because the
/// remote process has no path to the local Mac clipboard.
pub fn apply_croft_key_settings(plist: &mut Value) -> Result<(), ITerm2Error> {
    let dict = plist
        .as_dictionary_mut()
        .ok_or(ITerm2Error::NotADictionary)?;

    let menu = dict_entry_mut(dict, "NSUserKeyEquivalents");
    set_string(
        menu,
        "Find Globally...",
        FIND_GLOBALLY_MENU_EQUIV.to_string(),
    );
    // Relocate iTerm's "Find" menu item off Cmd+F so the GlobalKeyMap
    // binding below wins reliably. Users that still want iTerm's
    // in-pane find can use Cmd+Opt+F.
    set_string(menu, "Find...", FIND_MENU_EQUIV.to_string());
    // Reclaim Cmd+Shift+/ from the macOS Help-menu Search field.
    // AppKit binds Cmd+? to the Help menu at the app level, ahead of
    // iTerm2's GlobalKeyMap; without this override the chord opens
    // Help instead of reaching croft. Pointing "Show Help Menu" at
    // Cmd+Opt+? leaves Help reachable on a chord croft does not use.
    set_string(menu, HELP_MENU_KEY, HELP_MENU_EQUIV.to_string());
    // Relocate iTerm2's Edit menu items off the standard Cmd+letter
    // shortcuts so croft's terminal-pane Cmd+C (copy via OSC 52),
    // editor Cmd+S / Cmd+X / Cmd+Z, and Source Control / editor Cmd+A
    // can all reach their handlers. Without this, even after stripping
    // the profile-level Send-Hex bindings below, AppKit's standard
    // Edit menu would still claim the chord at the menu layer.
    set_string(menu, "Copy", "@~c".to_string());
    set_string(menu, "Cut", "@~x".to_string());
    set_string(menu, "Select All", "@~a".to_string());
    set_string(menu, "Undo", "@~z".to_string());
    menu.remove("Paste");
    // Relocate iTerm2 menu items that would otherwise catch the editor's
    // vim chords at the menu-bar layer. Each is moved to Cmd+Opt+<key>
    // so the original action stays reachable, but the bare Cmd+<key>
    // chord is freed for the GlobalKeyMap forwarder below.
    set_string(
        menu,
        "Split Vertically with Same Profile",
        "@~d".to_string(),
    );
    set_string(
        menu,
        "Split Horizontally with Same Profile",
        "@~D".to_string(),
    );
    set_string(menu, "Find Next", "@~g".to_string());
    set_string(menu, "Find Previous", "@~G".to_string());
    set_string(menu, "Jump to Selection", "@~y".to_string());
    // Relocate File > Print off Cmd+P so croft's Quick Open finder can
    // receive the chord. Without this AppKit's app-level Print menu
    // binding opens the macOS Print dialog before iTerm2's GlobalKeyMap
    // is consulted. Cmd+Opt+P keeps Print reachable on a chord croft
    // does not use.
    set_string(menu, PRINT_MENU_KEY, PRINT_MENU_EQUIV.to_string());
    // Relocate iTerm2's File > "Close" (closeCurrentSession:) off bare Cmd+W so
    // croft's editor close-tab chord reaches the app instead of closing the
    // iTerm2 session (which quits iTerm2 when it is the last one). The menu
    // key-equivalent layer is consulted before GlobalKeyMap, so the forwarder
    // alone is not enough; the menu item must move. Cmd+Opt+W is already
    // iTerm2's "Close All Panes in Tab" and Cmd+Shift+W is "Close Terminal
    // Window", so the close-session action moves to Cmd+Ctrl+W, unbound by
    // default and reachable.
    set_string(
        menu,
        CLOSE_SESSION_MENU_KEY,
        CLOSE_SESSION_MENU_EQUIV.to_string(),
    );
    // Relocate AppKit's Edit > Find > "Use Selection for Find" off Cmd+E so
    // croft's native-modal (vim) toggle can receive the chord. Like Cmd+F in
    // the same submenu, the bare chord is otherwise claimed at the menu layer
    // before iTerm2's GlobalKeyMap is consulted. Cmd+Opt+E keeps the
    // find-from-selection action reachable on a chord croft does not use.
    set_string(
        menu,
        USE_SELECTION_FOR_FIND_MENU_KEY,
        USE_SELECTION_FOR_FIND_MENU_EQUIV.to_string(),
    );
    // Relocate iTerm2's "Restore Closed Session" off Cmd+Shift+T so
    // croft's terminal-focus chord can claim it. Cmd+Opt+Shift+T keeps
    // the iTerm2 action reachable on a chord croft does not use.
    set_string(menu, "Restore Closed Session", "@~T".to_string());
    // Relocate iTerm2's "New Tab" off Cmd+T so croft's new-terminal chord
    // can claim it. Cmd+Ctrl+T keeps the iTerm2 action reachable and stays
    // clear of the Cmd+Opt+T "New Tab Next to Current Tab" alternate.
    set_string(menu, NEW_TAB_MENU_KEY, NEW_TAB_MENU_EQUIV.to_string());
    // Relocate iTerm2's "Previous Pane" / "Next Pane" off Cmd+[ / Cmd+] so
    // croft's terminal-cycle chords can claim them. Cmd+Opt+[ / Cmd+Opt+]
    // keep pane navigation reachable.
    set_string(menu, PREV_PANE_MENU_KEY, PREV_PANE_MENU_EQUIV.to_string());
    set_string(menu, NEXT_PANE_MENU_KEY, NEXT_PANE_MENU_EQUIV.to_string());
    // iTerm2's Window menu binds Cmd+1..Cmd+9 to Select Tab. Move each
    // to Cmd+Opt+digit so croft can capture Cmd+digit as a vim count.
    for (i, label) in [
        "Select Tab 1",
        "Select Tab 2",
        "Select Tab 3",
        "Select Tab 4",
        "Select Tab 5",
        "Select Tab 6",
        "Select Tab 7",
        "Select Tab 8",
        "Select Tab 9",
    ]
    .iter()
    .enumerate()
    {
        set_string(menu, label, format!("@~{}", i + 1));
    }

    let global = dict_entry_mut(dict, "GlobalKeyMap");
    global.insert(CMD_SHIFT_F_KEY.into(), send_hex_action(CMD_SHIFT_F_HEX, 0));
    // Explorer shortcuts: forward Cmd+F / Cmd+R / Cmd+/ to croft as
    // CSI-u sequences. Croft handles them only when the Explorer pane
    // is focused; elsewhere the keys are passed through as raw input,
    // which means giving up iTerm's own actions on those chords
    // (Find / Clear Buffer) while croft is running. The user agreed.
    global.insert(CMD_F_KEY.into(), send_hex_action(CMD_F_HEX, 0));
    global.insert(CMD_R_KEY.into(), send_hex_action(CMD_R_HEX, 0));
    global.insert(CMD_SLASH_KEY.into(), send_hex_action(CMD_SLASH_HEX, 0));
    global.insert(
        CMD_SHIFT_SLASH_KEY.into(),
        send_hex_action(CMD_SHIFT_SLASH_HEX, 0),
    );
    global.insert(
        CMD_SHIFT_ENTER_KEY.into(),
        send_hex_action(CMD_SHIFT_ENTER_HEX, 0),
    );
    // Mac-style Cmd+letter chords: forward each as a CSI-u sequence so
    // AppKit's NSResponder defaults (copy: / cut: / selectAll: / undo:)
    // don't consume them at the textview layer. Without these, even
    // with the Edit menu items relocated via NSUserKeyEquivalents above,
    // Cmd+C still never reaches croft because PTYTextView still answers
    // copy: from the default key bindings dictionary.
    for (key, hex) in [
        (CMD_A_KEY, CMD_A_HEX),
        (CMD_C_KEY, CMD_C_HEX),
        (CMD_S_KEY, CMD_S_HEX),
        (CMD_X_KEY, CMD_X_HEX),
        (CMD_Z_KEY, CMD_Z_HEX),
        (CMD_D_KEY, CMD_D_HEX),
        (CMD_G_KEY, CMD_G_HEX),
        (CMD_Y_KEY, CMD_Y_HEX),
        (CMD_O_KEY, CMD_O_HEX),
        (CMD_E_KEY, CMD_E_HEX),
        (CMD_P_KEY, CMD_P_HEX),
        (CMD_W_KEY, CMD_W_HEX),
        (CMD_SHIFT_G_KEY, CMD_SHIFT_G_HEX),
        (CMD_SHIFT_O_KEY, CMD_SHIFT_O_HEX),
        (CMD_SHIFT_E_KEY, CMD_SHIFT_E_HEX),
        (CMD_SHIFT_S_KEY, CMD_SHIFT_S_HEX),
        (CMD_SHIFT_D_KEY, CMD_SHIFT_D_HEX),
        (CMD_SHIFT_R_KEY, CMD_SHIFT_R_HEX),
        (CMD_SHIFT_X_KEY, CMD_SHIFT_X_HEX),
        (CMD_SHIFT_L_KEY, CMD_SHIFT_L_HEX),
        (CMD_SHIFT_N_KEY, CMD_SHIFT_N_HEX),
        (CMD_SHIFT_T_KEY, CMD_SHIFT_T_HEX),
        (CMD_T_KEY, CMD_T_HEX),
        (CMD_LBRACKET_KEY, CMD_LBRACKET_HEX),
        (CMD_RBRACKET_KEY, CMD_RBRACKET_HEX),
        (CMD_OPT_R_KEY, CMD_OPT_R_HEX),
        (CTRL_SHIFT_J_KEY, CTRL_SHIFT_J_HEX),
        (CMD_F12_KEY, CMD_F12_HEX),
        (CTRL_SHIFT_F12_KEY, CTRL_SHIFT_F12_HEX),
        (CMD_B_KEY, CMD_B_HEX),
        (CMD_BACKSLASH_KEY, CMD_BACKSLASH_HEX),
        (CMD_SHIFT_BACKSLASH_KEY, CMD_SHIFT_BACKSLASH_HEX),
        (CMD_OPT_BACKSLASH_KEY, CMD_OPT_BACKSLASH_HEX),
        (CMD_OPT_SHIFT_S_KEY, CMD_OPT_SHIFT_S_HEX),
        (CMD_OPT_SHIFT_T_KEY, CMD_OPT_SHIFT_T_HEX),
        (CMD_OPT_SHIFT_N_KEY, CMD_OPT_SHIFT_N_HEX),
        (CMD_OPT_SHIFT_J_KEY, CMD_OPT_SHIFT_J_HEX),
        (CMD_OPT_SHIFT_U_KEY, CMD_OPT_SHIFT_U_HEX),
        (CMD_OPT_SHIFT_L_KEY, CMD_OPT_SHIFT_L_HEX),
        (CMD_OPT_SHIFT_C_KEY, CMD_OPT_SHIFT_C_HEX),
        (CMD_OPT_SHIFT_A_KEY, CMD_OPT_SHIFT_A_HEX),
        (CMD_OPT_SHIFT_D_KEY, CMD_OPT_SHIFT_D_HEX),
        (CMD_OPT_SHIFT_W_KEY, CMD_OPT_SHIFT_W_HEX),
        (CMD_OPT_SHIFT_F_KEY, CMD_OPT_SHIFT_F_HEX),
        (CMD_OPT_LEFT_KEY, CMD_OPT_LEFT_HEX),
        (CMD_OPT_RIGHT_KEY, CMD_OPT_RIGHT_HEX),
        (CMD_OPT_UP_KEY, CMD_OPT_UP_HEX),
        (CMD_OPT_DOWN_KEY, CMD_OPT_DOWN_HEX),
        (CMD_SHIFT_P_KEY, CMD_SHIFT_P_HEX),
    ] {
        global.insert(key.into(), send_hex_action(hex, 0));
    }
    for (key, hex) in CMD_DIGIT_CHORDS {
        global.insert((*key).into(), send_hex_action(hex, 0));
    }
    global.remove(CMD_V_KEY);
    for key in CMD_ARROW_CLEANUP_KEYS {
        global.remove(key);
    }

    dict.insert(MOUSE_REPORTING_FRUSTRATION_KEY.into(), Value::Boolean(true));

    let bookmarks = dict
        .get_mut("New Bookmarks")
        .and_then(|v| v.as_array_mut())
        .ok_or(ITerm2Error::NoBookmarksArray)?;
    for profile in bookmarks.iter_mut().filter_map(|v| v.as_dictionary_mut()) {
        if let Some(Value::Dictionary(profile_keys)) = profile.get_mut("Keyboard Map") {
            profile_keys.remove(CMD_V_KEY);
        }
    }

    let drop_pointer_actions =
        if let Some(Value::Dictionary(pointer)) = dict.get_mut("PointerActions") {
            pointer.remove(STALE_GOTO_DEF_POINTER_KEY);
            pointer.is_empty()
        } else {
            false
        };
    if drop_pointer_actions {
        dict.remove("PointerActions");
    }

    Ok(())
}

fn set_string(dict: &mut Dictionary, key: &str, value: String) {
    dict.insert(key.into(), Value::String(value));
}

fn default_profile_mut(dict: &mut Dictionary) -> Result<&mut Dictionary, ITerm2Error> {
    let default_guid = dict
        .get("Default Bookmark Guid")
        .and_then(|v| v.as_string())
        .ok_or(ITerm2Error::NoDefaultGuid)?
        .to_string();

    let bookmarks = dict
        .get_mut("New Bookmarks")
        .and_then(|v| v.as_array_mut())
        .ok_or(ITerm2Error::NoBookmarksArray)?;

    bookmarks
        .iter_mut()
        .filter_map(|v| v.as_dictionary_mut())
        .find(|d| d.get("Guid").and_then(|g| g.as_string()) == Some(&default_guid))
        .ok_or(ITerm2Error::NoMatchingProfile(default_guid))
}

fn dict_entry_mut<'a>(dict: &'a mut Dictionary, key: &str) -> &'a mut Dictionary {
    if !matches!(dict.get(key), Some(Value::Dictionary(_))) {
        dict.insert(key.into(), Value::Dictionary(Dictionary::new()));
    }
    dict.get_mut(key)
        .and_then(|v| v.as_dictionary_mut())
        .expect("dictionary value was just inserted")
}

fn send_hex_action(text: &str, apply_mode: i64) -> Value {
    let mut action = Dictionary::new();
    action.insert("Action".into(), Value::Integer(11.into()));
    action.insert("Apply Mode".into(), Value::Integer(apply_mode.into()));
    action.insert("Escaping".into(), Value::Integer(2.into()));
    action.insert("Text".into(), Value::String(text.to_string()));
    action.insert("Version".into(), Value::Integer(2.into()));
    Value::Dictionary(action)
}

/// Load → mutate → save the iTerm2 plist on disk.
#[cfg(test)]
pub fn install_font_settings(
    plist_path: &Path,
    primary_font_ps: &str,
    nonascii_font_ps: &str,
    size: u32,
) -> Result<()> {
    if !plist_path.exists() {
        return Err(ITerm2Error::PlistMissing(plist_path.to_path_buf()).into());
    }
    let mut value: Value = Value::from_file(plist_path)
        .with_context(|| format!("reading {}", plist_path.display()))?;
    apply_font_settings(&mut value, primary_font_ps, nonascii_font_ps, size)?;
    value
        .to_file_binary(plist_path)
        .with_context(|| format!("writing {}", plist_path.display()))?;
    Ok(())
}

/// Load → mutate → save every iTerm2 setting Croft needs: font fallback plus
/// Cmd+Shift+F and Search paste behavior.
pub fn install_croft_settings(
    plist_path: &Path,
    primary_font_ps: &str,
    nonascii_font_ps: &str,
    size: u32,
) -> Result<()> {
    if !plist_path.exists() {
        return Err(ITerm2Error::PlistMissing(plist_path.to_path_buf()).into());
    }
    let mut value: Value = Value::from_file(plist_path)
        .with_context(|| format!("reading {}", plist_path.display()))?;
    apply_font_settings(&mut value, primary_font_ps, nonascii_font_ps, size)?;
    apply_croft_key_settings(&mut value)?;
    value
        .to_file_binary(plist_path)
        .with_context(|| format!("writing {}", plist_path.display()))?;
    Ok(())
}

pub fn default_plist_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(ITERM2_PLIST_REL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Value;

    fn synth_plist(default_guid: &str, profile_guids: &[&str]) -> Value {
        let mut bookmarks: Vec<Value> = Vec::new();
        for g in profile_guids {
            let mut d = Dictionary::new();
            d.insert("Guid".into(), Value::String((*g).to_string()));
            d.insert("Name".into(), Value::String(format!("Profile {g}")));
            bookmarks.push(Value::Dictionary(d));
        }
        let mut top = Dictionary::new();
        top.insert(
            "Default Bookmark Guid".into(),
            Value::String(default_guid.to_string()),
        );
        top.insert("New Bookmarks".into(), Value::Array(bookmarks));
        Value::Dictionary(top)
    }

    fn profile_in<'a>(plist: &'a Value, guid: &str) -> &'a Dictionary {
        plist
            .as_dictionary()
            .unwrap()
            .get("New Bookmarks")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|p| {
                p.as_dictionary()
                    .and_then(|d| d.get("Guid"))
                    .and_then(|v| v.as_string())
                    == Some(guid)
            })
            .unwrap()
            .as_dictionary()
            .unwrap()
    }

    fn dict_in<'a>(dict: &'a Dictionary, key: &str) -> &'a Dictionary {
        dict.get(key).unwrap().as_dictionary().unwrap()
    }

    fn action_text<'a>(dict: &'a Dictionary, key: &str) -> &'a str {
        dict.get(key)
            .unwrap()
            .as_dictionary()
            .unwrap()
            .get("Text")
            .unwrap()
            .as_string()
            .unwrap()
    }

    /// The historical kitty CSI-u Cmd+V escape that older croft versions
    /// installed in iTerm2 plists. Tests use it to seed a "legacy state"
    /// fixture so we can prove `apply_croft_key_settings` cleans it up.
    const LEGACY_CMD_V_HEX: &str = "0x1b 0x5b 0x31 0x31 0x38 0x3b 0x39 0x75";

    fn seed_stale_cmd_v_mappings(plist: &mut Value) {
        let top = plist.as_dictionary_mut().unwrap();
        dict_entry_mut(top, "GlobalKeyMap")
            .insert(CMD_V_KEY.into(), send_hex_action(LEGACY_CMD_V_HEX, 0));
        let bookmarks = top
            .get_mut("New Bookmarks")
            .unwrap()
            .as_array_mut()
            .unwrap();
        for profile in bookmarks.iter_mut().filter_map(|v| v.as_dictionary_mut()) {
            dict_entry_mut(profile, "Keyboard Map")
                .insert(CMD_V_KEY.into(), send_hex_action(LEGACY_CMD_V_HEX, 0));
        }
    }

    #[test]
    fn primary_font_value_concatenates_postscript_name_and_size() {
        assert_eq!(
            primary_font_value("MesloLGSNFM-Regular", 13),
            "MesloLGSNFM-Regular 13"
        );
        assert_eq!(
            primary_font_value("FiraCodeNFM-Reg", 14),
            "FiraCodeNFM-Reg 14"
        );
    }

    #[test]
    fn apply_font_settings_writes_normal_and_nonascii_font() {
        let mut plist = synth_plist("DEFAULT-GUID", &["OTHER-GUID", "DEFAULT-GUID"]);
        apply_font_settings(&mut plist, "MesloLGSNFM-Regular", "SymbolsNFM", 13).unwrap();
        let p = profile_in(&plist, "DEFAULT-GUID");
        assert_eq!(
            p.get("Normal Font").unwrap().as_string(),
            Some("MesloLGSNFM-Regular 13")
        );
        assert_eq!(
            p.get("Non Ascii Font").unwrap().as_string(),
            Some("SymbolsNFM 13")
        );
    }

    #[test]
    fn apply_font_settings_enables_use_non_ascii_font() {
        let mut plist = synth_plist("G1", &["G1"]);
        apply_font_settings(&mut plist, "X-Reg", "Y-Reg", 12).unwrap();
        let p = profile_in(&plist, "G1");
        assert_eq!(
            p.get("Use Non-ASCII Font").unwrap().as_boolean(),
            Some(true)
        );
    }

    #[test]
    fn apply_font_settings_only_touches_default_profile() {
        let mut plist = synth_plist("DEFAULT-GUID", &["OTHER-GUID", "DEFAULT-GUID"]);
        apply_font_settings(&mut plist, "F-Reg", "S-Reg", 13).unwrap();
        let other = profile_in(&plist, "OTHER-GUID");
        assert!(
            other.get("Normal Font").is_none(),
            "other profile should be untouched"
        );
        let defaultp = profile_in(&plist, "DEFAULT-GUID");
        assert!(defaultp.get("Normal Font").is_some());
    }

    #[test]
    fn apply_font_settings_errors_when_no_default_guid() {
        let mut top = Dictionary::new();
        top.insert("New Bookmarks".into(), Value::Array(vec![]));
        let mut plist = Value::Dictionary(top);
        let err = apply_font_settings(&mut plist, "F", "S", 13).unwrap_err();
        assert!(matches!(err, ITerm2Error::NoDefaultGuid));
    }

    #[test]
    fn apply_font_settings_errors_when_default_guid_does_not_match_any_profile() {
        let mut plist = synth_plist("MISSING-GUID", &["A", "B"]);
        let err = apply_font_settings(&mut plist, "F", "S", 13).unwrap_err();
        assert!(matches!(err, ITerm2Error::NoMatchingProfile(g) if g == "MISSING-GUID"));
    }

    #[test]
    fn apply_font_settings_errors_when_top_level_is_not_dict() {
        let mut plist = Value::Array(vec![]);
        let err = apply_font_settings(&mut plist, "F", "S", 13).unwrap_err();
        assert!(matches!(err, ITerm2Error::NotADictionary));
    }

    #[test]
    fn install_font_settings_round_trips_through_disk() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let plist = synth_plist("GUID-1", &["GUID-1"]);
        plist.to_file_xml(tmp.path()).unwrap();
        install_font_settings(tmp.path(), "MesloLGSNFM-Regular", "SymbolsNFM", 13).unwrap();
        let reloaded: Value = Value::from_file(tmp.path()).unwrap();
        let p = profile_in(&reloaded, "GUID-1");
        assert_eq!(
            p.get("Normal Font").unwrap().as_string(),
            Some("MesloLGSNFM-Regular 13")
        );
    }

    #[test]
    fn install_font_settings_errors_when_plist_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bogus = tmp.path().join("nonexistent.plist");
        let err = install_font_settings(&bogus, "F", "S", 13).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not found"),
            "expected 'not found' message, got: {msg}"
        );
    }

    #[test]
    fn apply_croft_key_settings_frees_find_menu_shortcut_only() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get("Find Globally...").and_then(|v| v.as_string()),
            Some(FIND_GLOBALLY_MENU_EQUIV)
        );
        assert!(
            menu.get("Paste").is_none(),
            "Paste must remain on its default Cmd+V menu shortcut so iTerm2 fires its native bracketed-paste action when Cmd+V is pressed; remapping it off Cmd+V breaks paste over SSH because the resulting key event has no clipboard reachable from the remote process"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_letter_chords_as_csi_u_so_iterm_responder_chain_does_not_consume_them()
     {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for (key, hex, label) in [
            (CMD_A_KEY, CMD_A_HEX, "Cmd+A (select all / multi-select)"),
            (CMD_C_KEY, CMD_C_HEX, "Cmd+C (copy via OSC 52)"),
            (
                CMD_S_KEY,
                CMD_S_HEX,
                "Cmd+S (editor save / source control stage)",
            ),
            (CMD_X_KEY, CMD_X_HEX, "Cmd+X (editor cut)"),
            (CMD_Z_KEY, CMD_Z_HEX, "Cmd+Z (editor undo)"),
        ] {
            assert_eq!(
                action_text(global, key),
                hex,
                "GlobalKeyMap is missing the CSI-u forwarder for {label}; without it, AppKit's NSResponder default key bindings catch the chord (Cmd+C -> copy: on PTYTextView) before croft's terminal handler sees a Char-with-Super key event, which is why Cmd+C silently fails to copy the croft selection even with the NSUserKeyEquivalents Edit-menu relocations in place"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_e_for_vim_mode_toggle() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_E_KEY),
            CMD_E_HEX,
            "GlobalKeyMap must forward Cmd+E as a CSI-u sequence so croft's `is_vim_toggle_key` fires and toggles native modal editing. Without it, iTerm2's Edit > Find > Use Selection for Find owns the bare chord at the menu layer and croft never sees a Char('e') + SUPER key event, so the toggle is inert"
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_use_selection_for_find_off_cmd_e() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get(USE_SELECTION_FOR_FIND_MENU_KEY)
                .and_then(|v| v.as_string()),
            Some(USE_SELECTION_FOR_FIND_MENU_EQUIV),
            "iTerm2's Use Selection for Find must be relocated off bare Cmd+E so croft's vim-mode toggle can claim the chord; @~e = Cmd+Opt+E keeps the find-from-selection action reachable on a chord croft does not use"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_backslash_for_go_to_bracket() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_SHIFT_BACKSLASH_KEY),
            CMD_SHIFT_BACKSLASH_HEX,
            "GlobalKeyMap must forward Cmd+Shift+\\ as a CSI-u sequence so croft's `is_goto_bracket_key` fires and jumps to the matching bracket (VS Code editor.action.jumpToBracket). Without it the chord never reaches croft and Go to Bracket is only reachable from the Command Palette"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_the_new_editor_command_chords() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for (key, hex, label) in [
            (
                CMD_OPT_BACKSLASH_KEY,
                CMD_OPT_BACKSLASH_HEX,
                "Cmd+Opt+\\ Select to Bracket",
            ),
            (
                CMD_OPT_SHIFT_S_KEY,
                CMD_OPT_SHIFT_S_HEX,
                "Cmd+Opt+Shift+S Convert Indentation to Spaces",
            ),
            (
                CMD_OPT_SHIFT_T_KEY,
                CMD_OPT_SHIFT_T_HEX,
                "Cmd+Opt+Shift+T Convert Indentation to Tabs",
            ),
            (
                CMD_OPT_SHIFT_N_KEY,
                CMD_OPT_SHIFT_N_HEX,
                "Cmd+Opt+Shift+N Trim Final Newlines",
            ),
        ] {
            assert_eq!(
                action_text(global, key),
                hex,
                "GlobalKeyMap must forward {label} as a CSI-u sequence so the palette command also has a working chord, honoring croft's tenet that every action has a shortcut. Without it the chord never reaches croft and the command is palette-only"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_forwards_the_formerly_palette_only_editor_chords() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for (key, hex, label) in [
            (
                CMD_OPT_SHIFT_J_KEY,
                CMD_OPT_SHIFT_J_HEX,
                "Cmd+Opt+Shift+J Join Lines",
            ),
            (
                CMD_OPT_SHIFT_U_KEY,
                CMD_OPT_SHIFT_U_HEX,
                "Cmd+Opt+Shift+U Transform to Uppercase",
            ),
            (
                CMD_OPT_SHIFT_L_KEY,
                CMD_OPT_SHIFT_L_HEX,
                "Cmd+Opt+Shift+L Transform to Lowercase",
            ),
            (
                CMD_OPT_SHIFT_C_KEY,
                CMD_OPT_SHIFT_C_HEX,
                "Cmd+Opt+Shift+C Transform to Title Case",
            ),
            (
                CMD_OPT_SHIFT_A_KEY,
                CMD_OPT_SHIFT_A_HEX,
                "Cmd+Opt+Shift+A Sort Lines Ascending",
            ),
            (
                CMD_OPT_SHIFT_D_KEY,
                CMD_OPT_SHIFT_D_HEX,
                "Cmd+Opt+Shift+D Sort Lines Descending",
            ),
            (
                CMD_OPT_SHIFT_W_KEY,
                CMD_OPT_SHIFT_W_HEX,
                "Cmd+Opt+Shift+W Trim Trailing Whitespace",
            ),
            (
                CMD_OPT_SHIFT_F_KEY,
                CMD_OPT_SHIFT_F_HEX,
                "Cmd+Opt+Shift+F Format Document",
            ),
        ] {
            assert_eq!(
                action_text(global, key),
                hex,
                "GlobalKeyMap must forward {label} as a CSI-u sequence so this command has a working chord and no longer ships palette-only, honoring croft's tenet that every action has a shortcut"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_b_for_sidebar_toggle() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_B_KEY),
            CMD_B_HEX,
            "GlobalKeyMap must forward Cmd+B as a CSI-u sequence so croft's `is_sidebar_toggle_key` fires and toggles the primary side bar, matching VS Code's Cmd+B. Without it the chord never reaches croft and the side bar can only be toggled with the raw Ctrl+B control byte"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_w_for_close_tab() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_W_KEY),
            CMD_W_HEX,
            "GlobalKeyMap must forward Cmd+W as a CSI-u sequence so croft's `is_close_tab_key` fires and closes the active editor tab. Without it, iTerm2's File > Close (closeCurrentSession:) owns the bare chord at the menu layer and the keystroke closes the iTerm2 session, quitting iTerm2 when it is the last one, instead of reaching croft"
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_close_off_cmd_w() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        let equiv = menu
            .get(CLOSE_SESSION_MENU_KEY)
            .and_then(|v| v.as_string())
            .filter(|s| !s.is_empty());
        assert!(
            equiv.is_some(),
            "iTerm2's File > Close must be relocated off bare Cmd+W (a non-empty NSUserKeyEquivalents entry) so croft's close-tab forwarder reaches the app instead of the menu layer closing the session"
        );
        assert_eq!(
            equiv,
            Some(CLOSE_SESSION_MENU_EQUIV),
            "the relocation chord must be Cmd+Ctrl+W (@^w): Cmd+Opt+W is already iTerm2's \"Close All Panes in Tab\" (MainMenu.xib id 635) and Cmd+Shift+W is \"Close Terminal Window\" (id 598), so neither can be reused"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_editor_vim_chord_starts_and_count_digits() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for (key, hex, label) in [
            (CMD_D_KEY, CMD_D_HEX, "Cmd+D (vim dd chord start)"),
            (CMD_G_KEY, CMD_G_HEX, "Cmd+G (vim gg chord start)"),
            (CMD_Y_KEY, CMD_Y_HEX, "Cmd+Y (vim yy chord start)"),
            (CMD_O_KEY, CMD_O_HEX, "Cmd+O (open line below)"),
            (
                CMD_SHIFT_G_KEY,
                CMD_SHIFT_G_HEX,
                "Cmd+Shift+G (goto bottom)",
            ),
            (
                CMD_SHIFT_O_KEY,
                CMD_SHIFT_O_HEX,
                "Cmd+Shift+O (open line above)",
            ),
        ] {
            assert_eq!(
                action_text(global, key),
                hex,
                "GlobalKeyMap missing CSI-u forwarder for {label}; without it, iTerm2 swallows the chord at the menu/responder layer (e.g. Cmd+D = Split Pane) and the editor's chord state never advances"
            );
        }
        for (key, hex) in CMD_DIGIT_CHORDS {
            assert_eq!(
                action_text(global, key),
                *hex,
                "GlobalKeyMap missing CSI-u forwarder for Cmd+digit {key}; without it, iTerm2 catches Cmd+digit for Select Tab N and the editor's vim count chord cannot start with a leading digit"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_n_for_explorer_new_folder() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_SHIFT_N_KEY),
            CMD_SHIFT_N_HEX,
            "GlobalKeyMap must forward Cmd+Shift+N as a CSI-u sequence so the Explorer's New Folder chord reaches croft regardless of any AppKit / iTerm2 menu binding that might otherwise consume it at the menu layer"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_sidebar_jumps_as_csi_u() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for (key, hex, label) in [
            (
                CMD_SHIFT_E_KEY,
                CMD_SHIFT_E_HEX,
                "Cmd+Shift+E (jump to Explorer)",
            ),
            (
                CMD_SHIFT_S_KEY,
                CMD_SHIFT_S_HEX,
                "Cmd+Shift+S (jump to Source Control)",
            ),
            (
                CMD_SHIFT_D_KEY,
                CMD_SHIFT_D_HEX,
                "Cmd+Shift+D (jump to Run and Debug)",
            ),
            (
                CMD_SHIFT_R_KEY,
                CMD_SHIFT_R_HEX,
                "Cmd+Shift+R (jump to Remote)",
            ),
            (
                CMD_SHIFT_L_KEY,
                CMD_SHIFT_L_HEX,
                "Cmd+Shift+L (disconnect remote, drop to local)",
            ),
        ] {
            assert_eq!(
                action_text(global, key),
                hex,
                "GlobalKeyMap must forward {label} as a CSI-u sequence so the sidebar-jump chord reaches croft; without it, AppKit / iTerm2 menu bindings (most notably Cmd+Shift+D = Split Horizontally) would swallow the chord at the menu layer"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_forwards_ctrl_shift_j_for_terminal_maximize() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CTRL_SHIFT_J_KEY),
            CTRL_SHIFT_J_HEX,
            "GlobalKeyMap must forward Ctrl+Shift+J as a CSI-u sequence so the maximize-terminal chord reaches croft as Char('J') + CONTROL+SHIFT regardless of any future AppKit / iTerm2 default that might bind the chord and swallow it at the menu layer"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_f12_for_go_to_implementations() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_F12_KEY),
            CMD_F12_HEX,
            "GlobalKeyMap must forward Cmd+F12 as the legacy modified-function-key sequence ESC[24;9~ so the editor's Go to Implementations chord reaches croft as F(12) + SUPER; unlike the bare-F12 family (plain / Shift / Ctrl) which passes through untouched, Cmd+F12 is captured by macOS and never reaches croft without this forwarder"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_ctrl_shift_f12_for_go_to_declaration() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CTRL_SHIFT_F12_KEY),
            CTRL_SHIFT_F12_HEX,
            "GlobalKeyMap must forward Ctrl+Shift+F12 as the legacy modified-function-key sequence ESC[24;6~ so the editor's Go to Declaration chord reaches croft as F(12) + SHIFT + CONTROL; Declaration moved here off the bare Shift+F12 it once held when Go to References took that VS Code default, and the forwarder is installed defensively so a future iTerm2 default cannot swallow the chord"
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_split_horizontally_menu_off_cmd_shift_d() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get("Split Horizontally with Same Profile")
                .and_then(|v| v.as_string()),
            Some("@~D"),
            "iTerm2's Split Horizontally with Same Profile must be relocated off Cmd+Shift+D so croft's Run-and-Debug jump can claim the chord; @~D = Cmd+Opt+Shift+D keeps the iTerm2 split reachable on a chord croft does not use"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_t_for_terminal_focus() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_SHIFT_T_KEY),
            CMD_SHIFT_T_HEX,
            "GlobalKeyMap must forward Cmd+Shift+T as a CSI-u sequence so croft's `is_terminal_focus_key` fires from any pane. Encoding: 'T' (codepoint 0x54 = 84) with kitty modifier byte 10 = 1 base + Shift(1) + Super(8), giving `ESC [ 84 ; 10 u`"
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_restore_closed_session_menu_off_cmd_shift_t() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get("Restore Closed Session")
                .and_then(|v| v.as_string()),
            Some("@~T"),
            "iTerm2's Restore Closed Session must be relocated off Cmd+Shift+T so croft's terminal-focus chord can claim it; @~T = Cmd+Opt+Shift+T keeps the iTerm2 action reachable on a chord croft does not use"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_t_for_new_terminal() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_T_KEY),
            CMD_T_HEX,
            "GlobalKeyMap must forward Cmd+T as a CSI-u sequence so croft's `is_terminal_split_key` fires and opens a new terminal. Encoding: 't' (codepoint 0x74 = 116) with kitty modifier byte 9 = 1 base + Super(8), giving `ESC [ 116 ; 9 u`"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_opt_r_for_reveal_in_finder() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_OPT_R_KEY),
            CMD_OPT_R_HEX,
            "GlobalKeyMap must forward Cmd+Opt+R as a CSI-u sequence so croft's `is_tree_reveal_in_finder_key` fires and reveals the selected entry in Finder. Encoding: 'r' (codepoint 0x72 = 114) with kitty modifier byte 11 = 1 base + Alt(2) + Super(8), giving `ESC [ 114 ; 11 u` (decoded as ALT|SUPER, disjoint from plain Cmd+R = Rename)"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_backslash_for_editor_split() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_BACKSLASH_KEY),
            CMD_BACKSLASH_HEX,
            "GlobalKeyMap must forward Cmd+\\ as a CSI-u sequence so croft's `is_editor_split_key` fires and splits the editor side by side. Encoding: '\\' (codepoint 0x5c = 92) with kitty modifier byte 9 = 1 base + Super(8), giving `ESC [ 92 ; 9 u`"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_opt_arrows_for_editor_group_focus() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_OPT_LEFT_KEY),
            CMD_OPT_LEFT_HEX,
            "GlobalKeyMap must forward Cmd+Opt+Left as `ESC [ 1 ; 11 D` so croft's `is_focus_group_left_key` moves focus to the left editor group. Modifier byte 11 = 1 base + Alt(2) + Super(8), decoded as Left + ALT | SUPER, disjoint from the bare Opt+Left word-motion"
        );
        assert_eq!(
            action_text(global, CMD_OPT_RIGHT_KEY),
            CMD_OPT_RIGHT_HEX,
            "GlobalKeyMap must forward Cmd+Opt+Right as `ESC [ 1 ; 11 C` so croft's `is_focus_group_right_key` moves focus to the right editor group"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_opt_up_down_for_multi_cursor() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_OPT_UP_KEY),
            CMD_OPT_UP_HEX,
            "GlobalKeyMap must forward Cmd+Opt+Up as `ESC [ 1 ; 11 A` so croft adds a cursor above; modifier byte 11 decodes to Up + ALT | SUPER"
        );
        assert_eq!(
            action_text(global, CMD_OPT_DOWN_KEY),
            CMD_OPT_DOWN_HEX,
            "GlobalKeyMap must forward Cmd+Opt+Down as `ESC [ 1 ; 11 B` so croft adds a cursor below"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_p_for_command_palette() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_SHIFT_P_KEY),
            CMD_SHIFT_P_HEX,
            "GlobalKeyMap must forward Cmd+Shift+P as `ESC [ 80 ; 10 u` so croft's `is_command_palette_key` opens the Command Palette; modifier byte 10 = 1 base + Shift(1) + Super(8), decoded as Char('p') + SUPER | SHIFT"
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_new_tab_menu_off_cmd_t() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get(NEW_TAB_MENU_KEY).and_then(|v| v.as_string()),
            Some(NEW_TAB_MENU_EQUIV),
            "iTerm2's New Tab must be relocated off Cmd+T so croft's new-terminal chord can claim it; @^t = Cmd+Ctrl+T keeps the iTerm2 action reachable on a chord croft does not use"
        );
    }

    #[test]
    fn apply_croft_key_settings_purges_stale_goto_definition_pointer_binding() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        {
            let top = plist.as_dictionary_mut().unwrap();
            let pa = dict_entry_mut(top, "PointerActions");
            let mut ctx = Dictionary::new();
            ctx.insert(
                "Action".into(),
                Value::String("kContextMenuPointerAction".into()),
            );
            pa.insert("Button,1,1,,".into(), Value::Dictionary(ctx));
            let mut goto = Dictionary::new();
            goto.insert(
                "Action".into(),
                Value::String("kSendHexCodePointerAction".into()),
            );
            pa.insert("Button,0,1,c,".into(), Value::Dictionary(goto));
        }
        apply_croft_key_settings(&mut plist).unwrap();
        let pointer = dict_in(plist.as_dictionary().unwrap(), "PointerActions");
        assert!(
            pointer.get("Button,0,1,c,").is_none(),
            "re-running setup must purge the dead Cmd+click sentinel binding earlier croft versions wrote"
        );
        assert!(
            pointer.get("Button,1,1,,").is_some(),
            "unrelated pointer actions such as the right-click context menu must survive the purge"
        );
    }

    #[test]
    fn apply_croft_key_settings_does_not_synthesize_pointer_actions_when_absent() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        assert!(
            plist
                .as_dictionary()
                .unwrap()
                .get("PointerActions")
                .is_none(),
            "with no PointerActions in the plist, setup must not create one (an empty dict would suppress iTerm2's built-in right-click menu)"
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_brackets_for_terminal_cycle() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for (key, hex, label) in [
            (
                CMD_LBRACKET_KEY,
                CMD_LBRACKET_HEX,
                "Cmd+[ (previous terminal)",
            ),
            (CMD_RBRACKET_KEY, CMD_RBRACKET_HEX, "Cmd+] (next terminal)"),
        ] {
            assert_eq!(
                action_text(global, key),
                hex,
                "GlobalKeyMap must forward {label} as a CSI-u sequence so croft's terminal-cycle predicate fires as Char('[' / ']') + SUPER. Arrows cannot be used: Ctrl+arrows are eaten by macOS Spaces, Option+arrows are shell word-motion, Cmd+arrows are reserved by the user"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_relocates_pane_nav_menus_off_cmd_brackets() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        for (item, expected) in [
            (PREV_PANE_MENU_KEY, PREV_PANE_MENU_EQUIV),
            (NEXT_PANE_MENU_KEY, NEXT_PANE_MENU_EQUIV),
        ] {
            assert_eq!(
                menu.get(item).and_then(|v| v.as_string()),
                Some(expected),
                "iTerm2's {item} must be relocated off bare Cmd+[ / Cmd+] so croft's terminal-cycle chord can claim it; Cmd+Opt+brackets keep pane navigation reachable"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_drops_earlier_cmd_arrow_cycle_overrides() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        {
            let top = plist.as_dictionary_mut().unwrap();
            let global = dict_entry_mut(top, "GlobalKeyMap");
            for key in CMD_ARROW_CLEANUP_KEYS {
                global.insert((*key).into(), send_hex_action("0x99", 0));
            }
        }
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        for key in CMD_ARROW_CLEANUP_KEYS {
            assert!(
                global.get(key).is_none(),
                "an earlier croft build hijacked {key} for cycling; now that cycling lives on Cmd+[ / Cmd+], that override must be removed so Cmd+Left/Right return to iTerm2's defaults"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_p_as_csi_u_so_iterm_print_menu_does_not_eat_quick_open()
     {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_P_KEY),
            CMD_P_HEX,
            "GlobalKeyMap must forward Cmd+P as a CSI-u sequence so croft's Quick Open file finder fires. Without it, AppKit's app-level File > Print binding opens the macOS Print dialog before iTerm2 forwards anything to the TUI — exactly what the user reported on 2026-05-13."
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_print_menu_off_cmd_p_so_appkit_does_not_steal_quick_open()
    {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get(PRINT_MENU_KEY).and_then(|v| v.as_string()),
            Some(PRINT_MENU_EQUIV),
            "NSUserKeyEquivalents must repoint File > Print at Cmd+Opt+P; otherwise AppKit's app-level Print binding catches Cmd+P at the menu layer and the macOS Print dialog opens instead of croft's Quick Open finder. Cmd+Opt+P keeps Print reachable on a chord croft does not use."
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_iterm_menu_items_off_vim_chord_letters() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        for (item, expected) in [
            ("Split Vertically with Same Profile", "@~d"),
            ("Find Next", "@~g"),
            ("Find Previous", "@~G"),
            ("Select Tab 1", "@~1"),
            ("Select Tab 9", "@~9"),
        ] {
            assert_eq!(
                menu.get(item).and_then(|v| v.as_string()),
                Some(expected),
                "iTerm2 menu item {item} must be relocated off its default Cmd-chord, otherwise the menu bar catches the chord before the GlobalKeyMap forwarder fires"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_silences_iterm_mouse_reporting_frustration_banner() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        assert_eq!(
            top.get(MOUSE_REPORTING_FRUSTRATION_KEY)
                .and_then(|v| v.as_boolean()),
            Some(true),
            "iTerm2's iTermMouseReportingFrustrationDetector watches raw Cmd+C keyDown and pops the 'Looks like you're trying to copy to the pasteboard...' banner whenever mouse reporting is on and iTerm2 has no selection (which is the steady state under croft, since croft owns the mouse). The advanced setting NoSyncNeverAskAboutMouseReportingFrustration suppresses that detector entirely."
        );
    }

    #[test]
    fn apply_croft_key_settings_relocates_edit_menu_items_off_cmd_letter_so_iterm_does_not_steal_them_back()
     {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        for (item, expected) in [
            ("Copy", "@~c"),
            ("Cut", "@~x"),
            ("Select All", "@~a"),
            ("Undo", "@~z"),
        ] {
            assert_eq!(
                menu.get(item).and_then(|v| v.as_string()),
                Some(expected),
                "iTerm2's Edit > {item} menu item must be relocated off its default Cmd-letter shortcut, otherwise once the profile-level Send-Hex bindings are stripped the menu shortcut would still claim the chord and croft would never see the keystroke"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_relocates_help_menu_off_cmd_shift_slash() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert_eq!(
            menu.get(HELP_MENU_KEY).and_then(|v| v.as_string()),
            Some(HELP_MENU_EQUIV),
            "Cmd+Shift+/ is reserved by macOS as Cmd+? for the Help menu's Search field; AppKit captures the chord at the app level before iTerm2's GlobalKeyMap is consulted. Re-pointing the 'Show Help Menu' NSUserKeyEquivalents item at Cmd+Opt+? frees the chord so the GlobalKeyMap CSI-u forwarder below can forward it to croft."
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_slash_as_csi_u_so_explorer_make_parent_root_fires()
     {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_SHIFT_SLASH_KEY),
            CMD_SHIFT_SLASH_HEX,
            "GlobalKeyMap must forward Cmd+Shift+/ as a CSI-u sequence so croft's `is_tree_make_parent_root_key` predicate fires from the Explorer pane. Encoding: '?' (shifted '/', codepoint 0x3f = 63) with kitty modifier byte 10 = 1+Shift(1)+Super(8), giving `ESC [ 63 ; 10 u`."
        );
    }

    #[test]
    fn apply_croft_key_settings_forwards_cmd_shift_enter_as_csi_u_so_iterm_does_not_swallow_it() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(
            action_text(global, CMD_SHIFT_ENTER_KEY),
            CMD_SHIFT_ENTER_HEX,
            "Cmd+Shift+Return must be hex-bound at the iTerm2 level: with no binding, iTerm2 never forwards the chord to the TUI, so croft never sees the keystroke. The CSI-u payload encodes Enter (codepoint 13) with kitty modifier byte 10 = 1+Shift(1)+Super(8), which crossterm decodes back to KeyEvent {{ code: Enter, modifiers: SHIFT|SUPER }}."
        );
    }

    #[test]
    fn apply_croft_key_settings_installs_global_search_only() {
        let mut plist = synth_plist("GUID-1", &["GUID-1", "GUID-2"]);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert_eq!(action_text(global, CMD_SHIFT_F_KEY), CMD_SHIFT_F_HEX);
        assert!(
            global.get(CMD_V_KEY).is_none(),
            "Cmd+V must not be hex-bound at the iTerm2 level; intercepting it as a key event prevents the terminal's native paste from emitting a bracketed-paste sequence, which is the only clipboard path that works over SSH"
        );
        for guid in ["GUID-1", "GUID-2"] {
            let profile = profile_in(&plist, guid);
            let cmd_v_in_profile = profile
                .get("Keyboard Map")
                .and_then(|v| v.as_dictionary())
                .and_then(|d| d.get(CMD_V_KEY));
            assert!(
                cmd_v_in_profile.is_none(),
                "profile-level Cmd+V binding must not exist (whether the Keyboard Map dict is absent or just missing this key) so every profile defers to iTerm2's native paste action"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_clears_legacy_cmd_v_bindings() {
        let mut plist = synth_plist("GUID-1", &["GUID-1", "GUID-2"]);
        seed_stale_cmd_v_mappings(&mut plist);
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let global = dict_in(top, "GlobalKeyMap");
        assert!(
            global.get(CMD_V_KEY).is_none(),
            "re-running setup must remove the legacy GlobalKeyMap Cmd+V hex binding installed by older croft versions"
        );
        for guid in ["GUID-1", "GUID-2"] {
            let profile = profile_in(&plist, guid);
            let profile_keys = dict_in(profile, "Keyboard Map");
            assert!(
                profile_keys.get(CMD_V_KEY).is_none(),
                "re-running setup must remove the legacy profile-level Cmd+V hex binding"
            );
        }
    }

    #[test]
    fn apply_croft_key_settings_clears_legacy_paste_menu_remap() {
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        {
            let top = plist.as_dictionary_mut().unwrap();
            let menu = dict_entry_mut(top, "NSUserKeyEquivalents");
            set_string(menu, "Paste", "@~^v".to_string());
        }
        apply_croft_key_settings(&mut plist).unwrap();
        let top = plist.as_dictionary().unwrap();
        let menu = dict_in(top, "NSUserKeyEquivalents");
        assert!(
            menu.get("Paste").is_none(),
            "re-running setup must remove the legacy Paste -> Cmd+Opt+Ctrl+V menu remap that older croft versions installed; the menu must fall back to the default Cmd+V shortcut so the native paste action fires"
        );
    }

    #[test]
    fn install_croft_settings_round_trips_fonts_and_clears_cmd_v_through_disk() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut plist = synth_plist("GUID-1", &["GUID-1"]);
        seed_stale_cmd_v_mappings(&mut plist);
        plist.to_file_xml(tmp.path()).unwrap();
        install_croft_settings(tmp.path(), "MesloLGSNFM-Regular", "SymbolsNFM", 13).unwrap();
        let reloaded: Value = Value::from_file(tmp.path()).unwrap();
        let profile = profile_in(&reloaded, "GUID-1");
        assert_eq!(
            profile.get("Normal Font").unwrap().as_string(),
            Some("MesloLGSNFM-Regular 13")
        );
        let top = reloaded.as_dictionary().unwrap();
        assert!(
            dict_in(top, "GlobalKeyMap").get(CMD_V_KEY).is_none(),
            "round-trip: Cmd+V binding must not survive on disk after a fresh setup"
        );
        let profile_keys = dict_in(profile, "Keyboard Map");
        assert!(
            profile_keys.get(CMD_V_KEY).is_none(),
            "round-trip: profile-level Cmd+V binding must not survive on disk after a fresh setup"
        );
    }
}
