# croft architecture

Notes for maintainers and developers. For what croft does and how to use it, see the [README](README.md); for the keyboard surface see [KEYBINDINGS.md](KEYBINDINGS.md).

croft is built on [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm), with [portable-pty](https://docs.rs/portable-pty/) for the embedded shell, [alacritty_terminal](https://docs.rs/alacritty_terminal/) for terminal-state parsing, [tree-sitter](https://tree-sitter.github.io/tree-sitter/) for incremental syntax highlighting, [calamine](https://docs.rs/calamine/) for spreadsheet parsing, and an inline-image protocol (iTerm2 OSC 1337, the Kitty graphics protocol on Ghostty / kitty, or DEC sixel on terminals whose DA1 reply advertises it) for image / PDF previews.

## How the embedded terminal works

`portable_pty::native_pty_system().openpty(...)` allocates a pseudoterminal and `spawn_command(...)` runs `$SHELL` on the slave side. A background thread drains the master fd into a `vt100::Parser`, which maintains the screen cell grid in memory. The render path walks `screen.cell(y, x)` for every cell in the pane and emits styled cells to the ratatui buffer with proper foreground / background / bold / italic / underline / reverse styles.

Resizes call `master.resize(...)` and `parser.set_size(...)` so programs like `htop`, `vim`, or your shell prompt redraw to fit the pane. Keystrokes from `crossterm`'s `Event::Key` are translated back to the byte sequences real terminals send (arrow keys to `\x1b[A`, `Ctrl+letter` to `0x01..0x1a`, `Alt+x` to `\x1b<x>`) and written to the master writer.

## Project layout

```
src/
├── main.rs              entry point + module declarations
├── cli.rs               clap CLI: open path, setup-terminal / setup-iterm2 / setup-cross / remote / keys subcommands
├── clipboard.rs         native macOS clipboard read/write (NSPasteboard) with pbpaste fallback
├── ghostty.rs           Ghostty config keybinds (setup-ghostty): re-emit every croft chord as its CSI-u sequence so Ghostty's own binds don't swallow them
├── git.rs               branch / dirty / ahead-behind status, plus anonymous git-protocol fetch for the welcome screen recents
├── gradient.rs          shared orange→green corner gradient: the welcome activity box border and the Black-theme focused-pane border
├── highlight.rs         tree-sitter highlight registry per language
├── icons.rs             Codicon / Devicon / Seti glyphs and per-language colors
├── install_session.rs   streams install-progress events while a remote host builds / installs the croft binary
├── iterm2.rs            iTerm2 plist mutation helpers for fonts and Croft key mappings
├── iterm2_inline.rs     inline-image baking pipeline + protocol dispatch (iTerm2 OSC 1337 / Kitty graphics / DEC sixel via DA1 probe): welcome wordmark, image / PDF preview, activity-bar icons incl. the settings gear, SSH empty-state hero
├── pdf.rs               PDF rasteriser: prefers pdftoppm (poppler), falls back to macOS sips
├── prefs.rs             durable user preferences (color theme) persisted at ~/.config/croft/config.json
├── remote.rs            remote (SSH) target metadata and launch dispatch
├── remote_bulk.rs       bulk lane for background installs: dedicated BatchMode SSH connection when key auth works (throttled shared mux otherwise) so update bytes never queue ahead of live-session keystrokes
├── remote_connect.rs    interactive SSH connect flow (host + password prompt phases) behind the connect dialog
├── session_state.rs     captures open tabs / layout so a self-update re-exec can restore them
├── sheet.rs             CSV / TSV / XLSX / XLS / XLSB / ODS parsing via the csv and calamine crates
├── sysmon.rs            system-metrics sampler loop (CPU / memory / network / disk / temp); disk gauge statvfs's $HOME's filesystem with df's formula (the `/` mount is Android's sealed read-only system partition, permanently ~100%)
├── termux.rs            Termux font auto-install: downloads Meslo Nerd Font Mono into ~/.termux/font.ttf (background, no-clobber) so the activity-bar codicon glyphs render
├── theme.rs             IDE color theme (Croft Dark / Croft Black): the background palette driving SetColors + baked-image fills
├── update_watch.rs      remote self-update: watch for a newer binary installed under a running remote croft
├── vim.rs               native modal (vim-style) editing: a pure key state machine (modes, counts, operators, text objects, f/t, search, ex-commands) that emits editing intents the app applies; toggled with Cmd+E
├── zoxide.rs            zoxide integration: strict query + typo-tolerant fuzzy fallback (Damerau-Levenshtein) + ensure-install (pkg on Termux, curl script elsewhere) with a logged outcome + InstallState surfaced to the Cmd+Z jump popup
├── app/                 event loop, three-pane layout + activity bar, key dispatch, status bar, mouse, clipboard, splitters, preview overlays
│   ├── mod.rs           the main App: render, key / mouse dispatch, status bar, splitters
│   ├── click.rs         double / triple click detection
│   ├── cursor_blink.rs  caret blink timing
│   ├── fs_watch.rs      filesystem watch + poll fallback feeding tree / editor / terminal refresh
│   ├── git_worker.rs    off-thread git status / changes worker
│   ├── hover.rs         LSP hover dwell timing
│   ├── nav.rs           editor back / forward navigation history
│   ├── overlay.rs       inline-image overlay state + clear-on-hide latches (iTerm2 cell eviction; the Kitty path adds a delete-all on the same clear frames)
│   ├── perf_hud.rs      F8 performance HUD
│   ├── sys_monitor.rs   background system-metrics poller driving the SYSTEM panel
│   ├── welcome.rs       welcome-screen state + async recent-repos drain
│   └── tests.rs         unit / integration tests
├── lsp/                 LSP client stack
│   ├── mod.rs
│   ├── client.rs        async-lsp client wrapper; router forwards diagnostics + work-done progress ($/progress, e.g. rust-analyzer "Indexing…") to the status bar
│   ├── config.rs        per-language LSP config (basedpyright, ruff, ty, vtsls, rust-analyzer, gopls)
│   ├── install.rs       croft-managed server provisioning: lazy background installs of vtsls (npm) and ty/ruff (uv, rerouted to Termux's pkg on Android) into ~/.croft/servers, incl. the uv bootstrap
│   ├── log_file.rs      LSP stderr / debug log sink at ~/.croft/lsp.log
│   ├── manager.rs       lifecycle: spawn / did_open / did_change / completion / shutdown
│   ├── registry.rs      language detection from file extension and shebang
│   ├── runtime.rs       Tokio runtime owned by the LSP manager
│   └── semantic_cache.rs content-keyed disk cache of semantic-token batches at ~/.croft/sem-cache
└── widgets/
    ├── mod.rs
    ├── completion_popup.rs  LSP completion popup (anchored at the cursor, filterable)
    ├── connect_dialog.rs    remote SSH connect modal (host + auth prompt phases)
    ├── diff.rs          side-by-side file diff renderer used by the explorer's Compare action
    ├── editor.rs        tree-sitter highlighted editor with full write path, mouse-drag selection, native-clipboard copy / cut, plus image / PDF / spreadsheet preview tabs
    ├── editor_find.rs   VS Code-style inline Find bar (Cmd+F) with active-match orange highlight, Enter / Shift+Enter walk, case-sensitive / whole-word / regex toggles
    ├── file_finder.rs   VS Code-style Quick Open (Cmd+P) fuzzy file picker with tiered match ranking (exact filename > prefix > substring > path > subsequence)
    ├── file_tree.rs     ignore::WalkBuilder backed tree, lazy children, fs-watcher refresh, multi-select, drag-drop, bulk trash, reveal-path on Cmd+P open
    ├── hover_popup.rs   LSP hover popup (300 ms dwell, anchored at the cursor)
    ├── osk.rs           on-screen keyboard for Termux (mouse tracking blocks the native soft keyboard): bottom-docked tappable band whose keys synthesize KeyEvents through handle_key; lower / shift / symbol layers plus caps lock (letters only), one-shot ctrl + alt latches on the bottom row next to space; physical-keyboard geometry (structural keys carry max cell widths, the left column staggers esc < tab < caps < shift like a MacBook, letters and space absorb wide-frame slack via water-filling, Enter grows into a two-row L on the right in both merged and split layouts (full-height even on one-row bands) and the collapse `⌄` key is about twice the `split` key); Gboard-style split layout for foldables (5|5 halves solved independently around a width/6 center gap, space on both halves, merged again under 60 cols), toggled by the `split` key and persisted as `osk_split` in prefs; thumb-sized keys scale with frame height and the non-focused pane folds away while the band is up
    ├── remote.rs        Remote (SSH) sidebar widget with empty-state hero illustration
    ├── run_debug.rs     Run and Debug sidebar widget: empty state plus Run [filename] button that spawns the active file in a fresh terminal
    ├── scrollbar.rs     shared vertical- and horizontal-scrollbar geometry
    ├── search.rs        sidebar search panel + .gitignore-aware substring walker
    ├── shortcuts.rs     F1 shortcuts modal: every binding grouped by pane, scrollable
    ├── source_control.rs Source Control sidebar widget: branch summary, commit input, change list, commit button, no-repo hero
    ├── system_panel.rs  collapsible SYSTEM metrics panel pinned to the sidebar bottom
    ├── terminal.rs      portable-pty + alacritty_terminal + ratatui integration with selection + scrollback
    └── zoxide_jump.rs   Cmd+Z zoxide jump popup: fuzzy directory jumper that re-roots + cd's the terminal
tests/cli.rs             integration tests for the CLI surface
```
