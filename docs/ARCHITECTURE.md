# croft architecture

Notes for maintainers and developers. For what croft does and how to use it, see the [README](../README.md); for the keyboard surface see [KEYBINDINGS.md](KEYBINDINGS.md).

croft is built on [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm), with [portable-pty](https://docs.rs/portable-pty/) for the embedded shell, [alacritty_terminal](https://docs.rs/alacritty_terminal/) for terminal-state parsing, [tree-sitter](https://tree-sitter.github.io/tree-sitter/) for incremental syntax highlighting, [calamine](https://docs.rs/calamine/) for spreadsheet parsing, and an inline-image protocol (iTerm2 OSC 1337, the Kitty graphics protocol on Ghostty / kitty, or DEC sixel on terminals whose DA1 reply advertises it) for image / PDF previews.

Spreadsheets are editable, not just previewed. CSV and TSV grids get a cell cursor and an in-grid editor over `SheetData`, structure ops, and a save path that serialises with the source delimiter; `sheet.rs` holds the model, the app the keys, and the editor the paint plus the frame-truth grid layout for mouse hit-tests. xlsx cell edits track `(sheet, row, col)` touches and save through `umya-spreadsheet`, with calamine staying the read side. The used-range ORIGIN maps grid cells to absolute sheet coordinates, numbers write as numbers, and formula cells are held back until a second forced save consents.

## How the embedded terminal works

`portable_pty::native_pty_system().openpty(...)` allocates a pseudoterminal and `spawn_command(...)` runs `$SHELL` on the slave side. A background thread reads the master fd and feeds the bytes to an `alacritty_terminal` `Processor::<StdSyncHandler>`, which applies them to an `alacritty_terminal` `Term` that maintains the screen cell grid in memory. The render path walks `term.grid()[point]` for every cell in the pane and emits styled cells to the ratatui buffer with proper foreground / background / bold / italic / underline / reverse styles.

Resizes call `master.resize(...)` and `term.resize(...)` so programs like `htop`, `vim`, or your shell prompt redraw to fit the pane. Keystrokes from `crossterm`'s `Event::Key` are translated back to the byte sequences real terminals send (arrow keys to `\x1b[A`, `Ctrl+letter` to `0x01..0x1a`, `Alt+x` to `\x1b<x>`) and written to the master writer.

## Project layout

```
src/
├── main.rs              entry point + module declarations
├── cli.rs               clap CLI: open path, setup-terminal / setup-iterm2 / setup-cross / remote / view / keys subcommands
├── clipboard.rs         native macOS clipboard read/write (NSPasteboard) with pbpaste fallback
├── command_history.rs    durable cross-session shell command history as JSONL under `~/.config/croft/command_history.jsonl` (cwd / exit / duration), read off OSC 133 marks with no shell hooks; Ctrl+Shift+H opens the search popup
├── fleet.rs              running one command across N ssh hosts and diffing the results, behind "Terminal: Fleet Run"; the fleet must be named (`hosts: command`, `*` for explicit broadcast)
├── ghostty.rs           Ghostty config keybinds (setup-ghostty): re-emit every croft chord as its CSI-u sequence so Ghostty's own binds don't swallow them
├── docx.rs               docx/odt read-only preview: walks the zip+XML and emits markdown (headings, runs, lists, pipe tables, extracted images) for the markdown renderer; structure fidelity, not layout; 50MB source cap
├── file_ref.rs          `path:line[:col]` references detected in terminal text (rustc / grep / pytest / Python-traceback forms); backs Cmd/Ctrl+click-to-jump from any terminal pane with zero per-tool problem-matcher config
├── quickfix.rs           parses the last `grep`/`rg`/`git grep` command line into a pattern plus Search toggles and include/exclude globs, so "Search & Replace from Last grep/rg" seeds and runs the Search sidebar via `SearchPanel::seed`
├── git.rs                branch / dirty / ahead-behind status plus the working-tree operations the Source Control panel drives; multi-root, one `GitWorker` per workspace root
├── magic.rs              content-based format detection: a magic-byte signature table (images, PDF, zip family, gzip, tar, SQLite) used as the SECOND routing hint in `Editor::open` — extension first, sniffing decides when it gave no answer or lied
├── markdown.rs           rendered Markdown preview (Cmd/Ctrl+Shift+V): pulldown-cmark events to styled ratatui lines, tree-sitter fenced code, inline local images; pure, the editor holds the state
├── media.rs              audio/video info view for `.mp3`/`.wav`/`.flac`/`.m4a`/`.mp4`/`.m4v`/`.mov`: pure-Rust header parsers over a bounded head read, never decoding streams, rendered as a markdown card with an optional ffmpeg poster frame
├── merge.rs              merge-conflict marker scanning into ConflictBlocks plus Accept Current / Incoming / Both splicing; the editor caches blocks per `edit_seq`, Cmd+. opens the resolve picker, F7/Shift+F7 walk blocks, and "Complete Merge" refuses while any remain
├── merge_editor.rs       three-way merge editor (`MergeView`): diffs base→ours and base→theirs, clusters overlapping hunks, auto-resolves one-sided ones; the Result IS the host editor's ordinary buffer, so LSP, undo and save need nothing special
├── gradient.rs          shared orange→green corner gradient: the welcome activity box border and the Black-theme focused-pane border
├── gui_path.rs           PATH repair for GUI launches, run once at the top of `Cli::run`: detects a launchd/no-rc PATH and rebuilds it from the user's login shell
├── ansi_text.rs          SGR span parser for colour-bearing text: one pass into visible text plus styled spans, dropping every non-SGR escape; `parse_into` reuses a caller-owned line for bulk scans, and this is the ONE implementation of the stripping rules
├── http_file.rs          `.http`/`.rest` request files: REST-Client format parsed with `{{variables}}`, sent through `ureq` on a worker thread, response opened as an ordinary tab; an unresolved variable refuses to send rather than leak the hole
├── log_view.rs           windowed backing store for the rendered ANSI log view: a `memchr` line-offset index plus a 256 KiB parsed window around the viewport, head indexed sync and the tail in a background thread
├── hex.rs                hex viewer/editor core and the routing fallback for files the text heuristic rejects; pure state + windowed file IO, never loads the file whole
├── asciicast.rs          writing a session as an asciicast v2 recording, behind "Session: Record Terminal as Asciicast"; payloads go through `serde_json`, backwards timestamps are clamped
├── archive.rs            archive browser core: zip/jar/whl and tar/tar.gz member listing without payload reads, size-gated before the parse; extract_member writes one member under a strictly lexical containment check; the tab is read-only
├── highlight.rs          tree-sitter highlight registry per language; every span, captured or not, styles from the active theme's `SyntaxPalette`, snapshotted once per pass rather than a hardcoded Base16 literal
├── history.rs            local history: per-save snapshots under `~/.config/croft/history` (raw bytes, deduped, capped, 10s merge window), merged into the Explorer timeline, backing snapshot diff/restore plus a per-line `.seats` authorship sidecar
├── icons.rs             Codicon and file-type Nerd Font glyphs and per-language colors
├── install_session.rs   streams install-progress events while a remote host builds / installs the croft binary
├── iterm2.rs             iTerm2 plist mutation helpers for fonts and Croft key mappings; forwarders are tracked in a `Croft User Keymap Forwarders` ledger and swept before inserts, only while still the exact croft forwarder
├── iterm2_inline.rs      inline-image baking pipeline and protocol dispatch (iTerm2 OSC 1337 / Kitty graphics / DEC sixel): wordmark, image and PDF previews, activity-bar icons, minimap
├── keymap.rs             user key bindings from `~/.config/croft/keybindings.json`: parses chord strings into normalized Chords mapped to palette Command ids, consulted ahead of the built-in chords; tolerant JSONC, reloads on save
├── launcher.rs           macOS `croft install-launcher`: builds a clickable Croft.app via `osacompile` (an AppleScript applet, so double-clicked documents arrive as `odoc` events), brands the plist with PlistBuddy, then ad-hoc `codesign` and `lsregister`
├── notebook.rs           Jupyter rendered view: .ipynb parses into the SAME (lines, images) state the Markdown preview renders, so wrap, scroll, image overlays and Reopen as Text all reuse; no kernels, read-only truth about the file
├── notifications.rs      notification sinks: the `notifications` config key, one delivery worker behind a bounded queue, pure request builders for ntfy and webhooks, plus `termux-notification` and `command` sinks
├── outline_syntax.rs     tree-sitter outline provider: extracts the OUTLINE panel's symbol tree straight from the buffer's syntax tree via per-language queries so the panel paints before a cold LSP answers; the LSP reply supersedes it
├── output.rs             in-process OUTPUT bus behind the panel group's OUTPUT tab: named channels, each a capped ring buffer of levelled lines, pushed to across the codebase and mirrored to `lsp.log`, with a generation counter for re-pulls
├── pdf.rs                PDF rasteriser (pdftoppm, falling back to macOS sips) plus link extraction via `pdftohtml -xml` and click hit-testing against cached link rects
├── plot.rs               `croft plot`: numbers, CSV/TSV or JSON lines on stdin drawn as a line, bar, spark or histogram chart — an SVG emitted with the inline-image escape, or braille/block characters with `--text`; long series bucketed to the dot width
├── port_detect.rs        loopback-port detection behind the PORTS panel: an output-stream scraper in the terminal reader thread plus a subtree-scoped lsof/ss poll, and the Cmd/Ctrl+click URL resolver; the scoped scan says what to SURFACE, never what is still up
├── problem_matchers.rs   user-defined problem matchers from `matchers.json`: regex sequences compiled to `CompiledMatcher`s, tasks.json `problemMatcher` mapping, and background `begins`/`ends` watching
├── provenance.rs         which SEAT wrote each line (`Seat` plus a per-buffer line map) for the gutter overlay and inline blame; the invariant is that a line croft did not watch being written is Unknown, never guessed
├── prefs.rs              durable user preferences (theme, layout chrome, format-on-save, auto-save) at `~/.config/croft/config.json` behind the "Preferences: Open Settings" hub; `host_accents` rules drive per-host terminal pane dressing
├── quick_select.rs       WezTerm-style quick-select pure core for the terminal pane (Ctrl+Shift+Space): a priority-ordered pattern set matched over soft-wrap-stitched logical lines, with home-row labels
├── release_notes.rs      hand-curated "IN THIS RELEASE" highlights on the welcome panel; the text is DATA, one file per version in `src/release_notes/<version>.md`, baked in by `build.rs` — no git log, no network, and a missing file is a build error
├── remote.rs             remote (SSH) target metadata and launch dispatch, plus the ssh-pane re-root offer: `ssh_destination` parses a pane's foreground argv using ssh's own flag grammar
├── remote_bulk.rs       bulk lane for background installs: dedicated BatchMode SSH connection when key auth works (throttled shared mux otherwise) so update bytes never queue ahead of live-session keystrokes
├── remote_connect.rs    interactive SSH connect flow (host + password prompt phases) behind the connect dialog
├── review_threads.rs     GitHub review threads as the editor's comment boxes, behind "Review: Load PR Comments for This File"; distinguishes a thread's current `line` from `original_line`
├── scrubber.rs           moving through a branch's history, the cursor behind "Source Control: Scrub History"; the working tree is a `Position::Working` in the cursor, so leaving restores the live buffer exactly
├── session.rs            local session persistence: `croft attach` / `croft ls` run croft under the session host so terminals/LSP/DAP survive closing the window; legacy dtach sessions keep reattaching, and `ls` skips the collab relay socket
├── session_host.rs       the multiplayer session mux: a hidden `croft session-host` subcommand owning the inner croft's PTY and broadcasting it verbatim to N unix-socket clients, with write control and a presence roster
├── collab.rs             multiplayer Phase D, the collab layer below the app: `CollabDoc` over the `cola` text CRDT, `text_delta_ops` diffing, remote-op integration and `CollabMsg`
├── collab_agent.rs       a headless collab guest exposed as an MCP server on stdio (`croft collab-agent --workspace <path>`): joins the relay and serves five `collab_*` tools over JSON-RPC NDJSON so an external AI co-edits with a live named caret; croft itself contains no LLM code
├── pair.rs               the AI pilot core ("croft pair" / resident navigator): spawns the `claude` CLI as a persistent stream-json conversation and parses its fenced `<<<EDIT>>>` / `<<<NOTE>>>` protocol
├── pair/local.rs         the navigator's local-model transport: one Anthropic-compatible `/v1/messages` streaming call per turn (Ollama, LM Studio, llama.cpp, vLLM), deliberately not the claude CLI; selected by `croft pair --provider ollama [--base-url <url>] --model <m>`
├── pair/proactive.rs     the proactive-look detector (pure text-in/row-out): tree-sitter judges that the driver completed a new construct since the navigator's last look; `App::tick_proactive_navigator` owns the gates and hands the seat a comment-only yield turn
├── pair_host.rs          the resident navigator's in-process host: the App spawns the pilot from the workspace's `<hash>.pair.json` record, drains `PairEvent`s per tick, and drives ask / yield turns
├── session_state.rs     captures open tabs / layout so a self-update re-exec can restore them
├── sheet.rs             CSV / TSV / XLSX / XLS / XLSB / ODS parsing via the csv and calamine crates
├── shell_integration.rs  OSC 133 / 7 / 9 shell integration: `OscSniffer` intercepts the marks alacritty drops, plus the per-shell rc shims that emit them
├── snippets.rs           user snippets from `~/.config/croft/snippets.json` (VS Code format); `parse_body` turns tab-stop syntax into insert text plus ordered stops, and matches are injected into the completion popup; reloads on save
├── sqlite_view.rs        read-only SQLite browser: each table becomes a worksheet in the existing sheet grid, rows capped at 500 with the true count in the name, opened `SQLITE_OPEN_READONLY` over bundled sqlite3 so a live database is never locked or mutated
├── svg.rs                SVG file-preview rasterisation: `usvg` parse plus resvg render into a PNG the standard image-overlay pipeline consumes; the vector render happens once per open, resizes refit the stored PNG, and a parse failure falls through to the XML source
├── symbol_range.rs       keeping a symbol view pointed at its symbol while the file changes: an edit above shifts the range, below moves nothing, inside grows or shrinks it
├── tasks.rs              auto-detected project tasks read from the manifests the repo already has (tasks.json, Makefile, justfile, package.json, Cargo.toml, pyproject.toml); backs "Tasks: Run Task" and the Cmd+Shift+B default build in a reused named terminal pane
├── terminal_session.rs   terminal session restore: pane order, per-pane cwd, names and active index persisted per workspace root in `~/.config/croft/terminal_sessions.json`
├── termux.rs            Termux font auto-install: downloads Meslo Nerd Font Mono into ~/.termux/font.ttf (background, no-clobber) so the activity-bar codicon glyphs render
├── theme.rs              IDE color theme: a `Theme` palette plus `SyntaxPalette`, loaded as data from `[[themes]]` manifest blocks, with `Theme::BLACK` as a const fallback and light-theme chrome adaptation
├── triggers.rs           iTerm2-style terminal triggers from `~/.config/croft/triggers.json`: regex rules driving render-time `highlight`, plus `notify`/`bell` scanned in the PTY reader thread
├── agent_lane.rs         per-agent ledger of files an agent changed while you looked elsewhere; each row carries a review baseline content hash so it leaves the queue only once seen, and a write during two agents is flagged `shared` rather than guessed
├── agents.rs             agent lanes: which panes are running a coding agent (claude, codex, aider, gemini built in, extended by `~/.config/croft/agents.json`) and whether it is working, waiting or idle, judged from output stamps and prompt patterns rather than OSC 133 marks
├── update_check.rs       release-availability check plus staged upgrade: once a day it asks GitHub's latest-release endpoint off-thread and offers Update / Later, staging into `~/.cache/croft/staged` so the binary on PATH is untouched until Relaunch; never automatic
├── update_watch.rs      remote self-update: watch for a newer binary installed under a running remote croft
├── import_vscode.rs      one-shot VS Code profile import (`croft import-vscode`): settings, keybindings, snippets and the colour theme converted into croft's own files; merge never overwrite, and everything unmapped is REPORTED rather than dropped
├── marketplace.rs        fetching a theme from a marketplace: resolves an id or URL to an `ExtensionId`, downloads the `.vsix` and lifts one member out; nothing is installed or executed
├── vscode_theme.rs       VS Code colour theme import: converts a theme `.json` into a croft `[[themes]]` manifest under `~/.config/croft/extensions/`, behind `croft theme-import`
├── workspace.rs          the workspace's root-folder set: `WorkspaceRoots` (ordered, never empty) with `primary`/`owning_root`/`replace_primary`, persisted per primary root, plus `.code-workspace` parsing
├── view_ipc.rs           `croft view <file>` (#362): the channel a pane uses to ask the croft hosting it to open a file, so a shell prompt can reach the paged-PDF / sheet / SQLite viewers that only exist as editor tabs. The pane's shell is a child of that croft, so both ends share a host and uid and a 0600 socket suffices; the CLIENT resolves the path (croft's idea of a pane cwd is a scraped approximation) and it travels as raw BYTES in a JSON line, since `to_string_lossy` would open a different file. Croft binds `view-<pid>-<nonce>.sock` under the cache dir, accepts non-blocking from the frame loop, and sweeps stale sockets by `kill(pid, 0)` on the pid in the NAME rather than by connecting, which would race a concurrent bind. `croft view -` stages piped bytes under a sniffed extension in `~/.cache/croft/view-stdin/`, 0600 under 0700, swept after 24h
├── vim.rs               native modal (vim-style) editing: a pure key state machine (modes, counts, operators, text objects, f/t, search, ex-commands) that emits editing intents the app applies; toggled with Cmd+E
├── voice.rs              voice input for the Termux OSK mic key: delegates to `termux-dialog speech`, auto-installs `termux-api`, and injects the transcript via handle_key; a tap opens the dialog, a second tap cancels
├── zoxide.rs            zoxide integration: strict query + typo-tolerant fuzzy fallback (Damerau-Levenshtein) + ensure-install (pkg on Termux, curl script elsewhere) with a logged outcome + InstallState surfaced to the Cmd+Z jump popup
├── app/                 event loop, three-pane layout + activity bar, key dispatch, status bar, mouse, clipboard, splitters, preview overlays, Customize Layout (compute_chrome_layout / panel_band_rect pure geometry, secondary side bar, Zen Mode)
│   ├── mod.rs           the main App: render, key / mouse dispatch, status bar, splitters
│   ├── click.rs         double / triple click detection
│   ├── cursor_blink.rs  caret blink timing
│   ├── fs_watch.rs      filesystem watch + poll fallback feeding tree / editor / terminal refresh. The poll stats the file behind EVERY open tab, not just the focused one: a boundary dir carries no watch at all (see the storm rules — never root a stream at or above `target/`, `node_modules/`, `.git/`) and an in-place write does not move its parent dir's mtime, so for a file sitting directly in one this stat is the only thing that can notice a change; cost is one stat per open tab per (adaptively backed-off) tick
│   ├── git_worker.rs    off-thread git status / changes worker
│   ├── hover.rs         LSP hover dwell timing
│   ├── nav.rs           editor back / forward navigation history
│   ├── overlay.rs       inline-image overlay state + clear-on-hide latches (iTerm2 cell eviction; the Kitty path adds a delete-all on the same clear frames)
│   ├── perf_hud.rs      F8 performance HUD
│   ├── welcome.rs       welcome-screen state: the "IN THIS RELEASE" highlights, read synchronously from `release_notes::release_notes()` (no thread, no network)
│   └── tests.rs         unit / integration tests
├── dap/                 debugger stack: Debug Adapter Protocol client. debugpy (Python, 3.14+, no fallback) is the verified mechanism; Rust/C/C++ route to lldb-dap; JS/TS route to vscode-js-debug. Which file types each handles is a data-driven extension axis (see registry.rs)
│   ├── mod.rs
│   ├── transport.rs     DAP wire framing (Content-Length + seq envelope, not JSON-RPC, so async-lsp can't be reused); spawns the adapter detached via setsid (so the debuggee can't tcsetpgrp-background and SIGTTIN-suspend croft, and so teardown can killpg the whole group), blocking reader thread frames stdout into an mpsc channel; Drop/kill signals the process group and reaps the child
│   ├── session.rs            one debug launch session: the initialize → setBreakpoints → configurationDone → stopped state machine, event classifier, the stackTrace → scopes → variables chain, evaluate, breakpoints and logpoints, over one adapter-agnostic launch_with
│   ├── registry.rs           data-driven debug-adapter registry: maps a file extension to an AdapterKind from the `[[debug_adapters]]` blocks in bundled + user manifests, skipping adapters whose extension is disabled in the Extensions panel
│   ├── log.rs           optional DAP wire log at ~/.croft/dap.log (gated by CROFT_DAP_LOG), mirroring lsp/log_file.rs
│   ├── install.rs       provisions a private debugpy venv at ~/.croft/debug-venv via uv (PEP 668 forbids pip-ing into the uv-managed CPython; mirrors ~/.croft/servers)
│   ├── remote_attach.rs pure attach planning: parse / gate the CPython version (>=3.14 ships sys.remote_exec), the platform-aware sudo-elevation decision (macOS always, Linux unless Yama ptrace_scope is relaxed), and the `pdb -p` command builder
│   ├── discovery.rs     enumerate attachable CPython 3.14+ processes via sysinfo plus a per-candidate `--version` probe
│   └── reaper.rs             sweeps orphaned vscode-js-debug processes (server plus its detached watchdog) left by a crash or force-quit; kills only `~/.croft/js-debug` scripts reparented to pid 1, so a live session is never touched
├── testing/              Test Runner: a background worker runs the project's test tool off the render loop (cargo/pytest/vitest/jest, picked by `registry::runner_for`) and streams parsed cases into the Testing panel
│   ├── mod.rs           shared NO_RUNNER_STATUS, `suite_pattern` (anchors a suite as `parse::` so cargo's substring filter and the panel's marking cannot sweep `parse_utils::b`), and `regex_escape` (shared by the JS `-t` argv builders and the locator; test titles are arbitrary strings)
│   ├── failure_site.rs       where a failing test actually failed: parses libtest, pytest and jest/vitest frames to a `file:line` in the user's own code, rejecting registry/`node_modules`/`site-packages`/stdlib locations; the last location in a block wins
│   ├── locate.rs             test-name → source location: `find_test_source` walks the workspace grepping `fn <leaf>` and ranks by module path, while pytest and JS node IDs grep just their own file; run on a background thread so a big workspace cannot freeze the render loop
│   ├── model.rs         TestStatus (NotRun/Running/Passed/Failed/Skipped) + TestCase; suite_and_leaf splits a `module::name` path for the tree
│   ├── registry.rs           data-driven test-runner registry: maps a workspace root to a Runner from the `[[test_runners]]` blocks in bundled + user manifests, skipping runners whose extension is disabled in the Extensions panel
│   ├── parse.rs              per-tool output parsers — libtest, pytest, vitest, jest — with all node IDs normalised to `::` separators so one panel tree serves every runner
│   └── worker.rs             the test-runner command layer: RunAll / RunOne / RunFilter / RunSuite / Discover per runner, building each tool's argv and streaming cases over an mpsc channel
├── mcp/                 MCP sidecar host (Tier-1 extensions): croft is a DETERMINISTIC MCP host — a human invokes a contributed palette command and croft calls one pre-known tool on one vetted local server (no LLM picks tools), which removes MCP's whole prompt-injection/tool-poisoning attack family
│   ├── mod.rs           module overview + McpOutcome (the off-thread worker's result)
│   ├── transport.rs     JSON-RPC 2.0 over newline-delimited JSON on the server's stdio (the MCP stdio framing; simpler than DAP's Content-Length); setsid-detached child, reader thread -> mpsc, least-privilege env_clear()+envs()
│   ├── client.rs        the MCP lifecycle: initialize -> notifications/initialized -> tools/list -> tools/call, synchronous id-correlated calls; tool_fingerprint for trust-on-first-use rug-pull detection
│   ├── registry.rs           data-driven command registry: `contributed_commands()` for eager palette registration and `resolve_command_in_dir()` for lazy tool/server resolution from `[[commands]]`/`[[mcp_servers]]`, plus the `[[viewers]]` lookups
│   ├── catalog.rs            the curated MCP catalog (AVAILABLE tier of the Extensions panel): bundled CATALOG_MANIFESTS merged with the remote signed index, with install/uninstall gated by provenance so croft removes only what it added, never a hand-dropped user manifest
│   └── registry_index.rs the remote vetted index (extensions.croft.software): fetches index.json plus its signature and verifies ed25519 against a baked `INDEX_PUBLIC_KEY` BEFORE caching; stale-while-revalidate, disarmed when the key is all-zero
├── lsp/                 LSP client stack
│   ├── mod.rs
│   ├── client.rs             async-lsp client wrapper: routes diagnostics (pushed and pulled) and `$/progress` to the status bar, acknowledges refresh requests into shared re-pull flags, declares `workspace.workspaceFolders` and answers the server→client folder request from the same list initialize carried
│   ├── config.rs             per-language LSP config (basedpyright, ruff, ty, vtsls, rust-analyzer, gopls); the ServerConfig factories are the executable spec the bundled manifests reproduce, and Language is an open newtype so extensions can add one
│   ├── install.rs            croft-managed server provisioning: lazy background installs into `~/.croft/servers` via three backends — npm, uv (rerouted to Termux `pkg` on Android), and Binary (per-platform release download, PATH-first)
│   ├── languages.rs     language table: file-extension -> language, root markers, and server family, all built from extension manifests' [[languages]] blocks (replaces the old hardcoded Language enum match arms)
│   ├── log_file.rs      LSP stderr / debug log sink at ~/.croft/lsp.log
│   ├── manager.rs            LSP lifecycle: workspace pre-warm at startup, spawn, did_open / did_change / did_save, completion, documentSymbol, inlay hints, documentHighlight and whole-project pull diagnostics
│   ├── manifest.rs           declarative `extension.toml` loader for the extension system: parses a manifest's `[[languages]]`, `[[language_servers]]`, `[[themes]]`, `[[debug_adapters]]`, `[[test_runners]]`, `[[mcp_servers]]`, `[[commands]]` and `[[viewers]]` blocks
│   ├── registry.rs      ServerRegistry (language -> ordered servers); built from the bundled manifests in assets/extensions/* plus user-installed extensions, instead of hardcoding configs
│   ├── runtime.rs       Tokio runtime owned by the LSP manager
│   ├── semantic_cache.rs content-keyed disk cache of semantic-token batches at `~/.croft/sem-cache`; a batch is applied or stored only while its `seq` still matches the live buffer, so a stale reply is never decoded over rewritten lines
│   └── trace.rs         raw JSON-RPC trace tee (OUTPUT panel RPC toggle): TraceRead/TraceWrite wrap a server's stdio and, only while tracing is enabled, reassemble Content-Length frames each way into the server's "<name> (trace)" channel (Zed's RPC-messages equivalent)
└── widgets/
    ├── mod.rs
    ├── command_palette.rs   VS Code Command Palette (Cmd/Ctrl+Shift+P): a static command registry + fuzzy-filtered picker; the App's run_command dispatches each entry
    ├── commit_graph.rs       the Source Control COMMITS section: a repo-wide commit graph with box-drawing lane rails; `layout_graph` is the pure lane algorithm, run on the fetch thread
    ├── completion_popup.rs  LSP completion popup (anchored at the cursor, filterable; `area_for` clamps to the editor pane on all four edges — a popup wider than a narrow pane used to be pushed left past the pane's own edge and paint over the Explorer)
    ├── signature_help_popup.rs  LSP signature help / parameter hints: a one-line popup above the caret with the active parameter bolded, auto-triggered on `(`/`,` and dismissed on `)`/Esc (manager `RequestSignatureHelp` + `normalise_signature_help`)
    ├── connect_dialog.rs    remote SSH connect modal (host + auth prompt phases)
    ├── dependencies.rs       collapsible, language-aware DEPENDENCIES section: detects the workspace's package ecosystems from root manifests (Cargo.toml, pyproject.toml, package.json, go.mod), resolves packages off-thread; display-only, gated on detection
    ├── diff.rs               side-by-side file diff renderer behind the explorer's Compare action; also derives hunk ranges and the unified patches behind the diff pane's stage / unstage / revert keys
    ├── editor.rs             tree-sitter highlighted editor with the full write path, mouse-drag selection, native clipboard, image / PDF / spreadsheet preview tabs, minimap rasterizer and code folding
    ├── editor_find.rs        VS Code-style inline Find bar (`Cmd+F`) with match highlight, Enter/Shift+Enter walk, case/whole-word/regex toggles and a Replace row (`Cmd+Opt+F`); Replace All runs through the same `split_for_highlight` as count and paint
    ├── extensions.rs         Extensions sidebar widget: projects bundled and user manifests into a theme-aware list grouped under BUILT-IN/INSTALLED/AVAILABLE, with pill toggles, +Add, trash, a local filter box and clear/refresh; reports clicks back to App, which owns the state
    ├── file_finder.rs   VS Code-style Quick Open (Cmd+P) fuzzy file picker with tiered match ranking (exact filename > prefix > substring > path > subsequence)
    ├── file_tree.rs          `ignore::WalkBuilder` backed file tree: lazy children, fs-watcher refresh, multi-select, drag-drop, bulk trash, reveal-path, multi-root section rows and sticky scroll
    ├── header_pill.rs   shared section-header pill button (chip-backed codicon affordance + theme colours) used by both the Remote Explorer (+ / refresh) and Source Control (refresh) headers
    ├── captures.rs           the panel group's CAPTURES tab (iTerm2's Capture Output): lines collected by `capture` triggers, with Enter jumping to the originating pane row; pure widget capped at 500 entries, and it also shapes the "Ask Navigator about this line" turn
    ├── history_popup.rs      `Ctrl+Shift+H` command-history popup over command_history.rs: query line, newest-first deduped results with exit/duration/cwd meta, `Ctrl+R` scope filter
    ├── hover.rs         shared hover-feedback helpers (pointer hit-test + theme-aware row-hover background) used by the side panels and header pills
    ├── hover_popup.rs        anchored 300 ms-dwell popup: LSP hover, tab-path tooltips and compact button hints off one `ui_tooltip_at` dispatch; also renders Peek Definition (Alt+F12) as a caret-anchored excerpt that Enter converts to the real jump
    ├── open_editors.rs  collapsible OPEN EDITORS section: the open editor tabs projected from EditorTabs each frame (dirty dot, active highlight), click-to-activate; one of the Explorer sub-views toggled by the ⋯ "Views and More Actions" menu on the EXPLORER title line
    ├── outline.rs            collapsible OUTLINE section under the file tree: the active editor's symbol tree, painted instantly from tree-sitter and refined by the LSP documentSymbol reply, with `apply_outline_symbols` the sole judge filtering on active path and exact edit-seq
    ├── osk.rs                on-screen keyboard for Termux: a bottom-docked tappable band synthesizing KeyEvents through `handle_key`, with shift, symbol pages, caps lock and a split foldable layout
    ├── output.rs             the panel group's OUTPUT tab: a read-only viewer over the `output.rs` bus with a channel dropdown, minimum-level filter, RPC trace toggle and clear action, auto-following the tail; the dropdown windows and scrolls long channel lists
    ├── problems.rs           the panel group's PROBLEMS tab: aggregated workspace diagnostics with severity, text and group-by-file filters, plus build problem matchers scanning finished command output
    ├── ports.rs              the panel group's PORTS tab: the registry of detected loopback ports with a `⇄ host` marker for forwarded remote ports; pure widget, App runs open/forward/copy/stop, and `reconcile_live` compares against every loopback listener, never the pane-subtree scan
    ├── process_picker.rs centered selectable list of attachable Python 3.14+ processes for "Debug: Attach to Python Process"; selecting one has the App spawn `pdb -p <pid>` in a PTY
    ├── remote.rs        Remote (SSH) sidebar widget with empty-state hero illustration
    ├── run_debug.rs          Run and Debug sidebar widget: the Run button empty state, and when a session is live the call stack, expandable variables, WATCH, a debug console and a `❯` REPL prompt; the App builds the rows and maps clicks back to frames and variables
    ├── testing.rs            Testing sidebar widget: the test suite tree with pass/fail/skip glyphs, the Activity summary line, and the failing-test count feeding the beaker activity badge
    ├── scrollbar.rs     shared vertical- and horizontal-scrollbar geometry
    ├── search.rs        sidebar search panel (query/replace/include/exclude inputs) + .gitignore-aware walker, glob filtering, and replace
    ├── shortcuts.rs     F1 shortcuts modal: every binding grouped by pane, scrollable
    ├── branch_picker.rs Checkout / Create Branch quick-pick overlay: type to filter the repo's branches, Enter to switch or create-and-switch (also drives the Branch submenu's merge / rebase / delete / create-from via `branch_purpose`); pure widget fed by `git::list_branches`
    ├── input_prompt.rs  single-line text modal for SCM ops that need a typed value (clone URL, branch rename, remote name/URL, tag name), tagged by `InputPurpose`
    ├── list_picker.rs   generic single-choice picker for SCM ops that act on one of a git-owned list (apply/pop/drop a stash, delete a tag, remove/push-to a remote), tagged by `ListPurpose`
    ├── scm_menu.rs      the Source Control "⋯" title menu: a static tree of leaves + one-level fly-out submenus (Commit/Changes/Pull-Push/Branch/Remote/Stash/Tags); every leaf is an `ScmAction` dispatched in `App::dispatch_scm_action`
    ├── source_control.rs     Source Control sidebar widget: multi-root REPOSITORIES overview, branch summary, commit input, change list, inline stage/unstage/discard actions, commit split-button and menu
    ├── symbol_picker.rs VS Code "Go to Symbol in Editor" (Cmd/Ctrl+Shift+O): a fuzzy picker over the active file's outline symbols (kind-iconed, depth-indented); a `:` prefix switches to Go to Line; the App jumps via go_to_definition
    ├── workspace_symbols.rs  Go to Symbol in Workspace picker: query fans out to every running server as `workspace/symbol`, rows show kind icon, name and workspace-relative path; Enter opens the definition
    ├── terminal.rs           `portable-pty` + `alacritty_terminal` + ratatui integration: the embedded terminal with selection, scrollback, drag-reorderable panes and maximize mode
    ├── timeline.rs      collapsible TIMELINE section: the active file's git history (`git log --follow`) fetched off-thread, two rows per commit (summary + author/age), click-to-open the commit's diff; a ⋯-menu Explorer sub-view
    └── zoxide_jump.rs   Cmd+Z zoxide jump popup: fuzzy directory jumper that re-roots + cd's the terminal
tests/cli.rs             integration tests for the CLI surface
```

## Module notes

The tree above is the index. This section carries the design reasons behind the
modules that have them - why an approach was chosen, and which failure forced it.

### terminal.rs

The integrated terminal pane: `portable-pty` plus `alacritty_terminal` rendered through ratatui, with selection and scrollback on top.

**Pane order and live drag-reorder.** Pane order lives in `App::terminals`. A pane can be dragged by its name pill side-by-side, or by its rail row in maximize mode. `App::move_terminal` keeps the active pane pinned to its terminal, and re-seats each pane's `last_area` in SLOT order. The rect is a slot's geometry but it lives on the terminal object, and the main loop drains the whole mouse burst before drawing. Without the re-seat, the next motion report over the same cell would resolve to the pane that just left it and swap the two back. In maximize mode neither positional rule holds: there is exactly ONE live rect, and it belongs to whichever pane was active at the last PAINT — the press and the drag are drained together, so that is not the dragged terminal. The drag leaves the dragged terminal active, so the live rect is found and seated on the pane the reorder leaves active. The session write is marked and flushed once per tick rather than run per motion report.

**Shutdown.** `Drop` kills AND reaps the shell. `portable-pty`'s SIGKILL escalation never waits, so a HUP-trapping shell left a zombie per closed pane. It then wakes the reader through a shutdown pipe it polls beside the pty fd, and joins it. EOF alone is not a reliable wake: on Linux a background job keeps the pty slave open after the shell dies, and a bare blocked `read` would hang the join — and the UI — for as long as the job runs.

**Mouse-wheel forwarding.** `mouse_reporting()` reports whether the child enabled a tracking mode (`TermMode::MOUSE_MODE`). `report_mouse()` and the pure `encode_mouse_report()` emit an SGR (1006) or legacy X10 report. `App`'s wheel handler forwards the notch as a wheel report when the child tracks the mouse, so the child scrolls its own buffer — unless `Shift` is held, which scrolls croft's scrollback. A child that doesn't track the mouse still gets the arrow-key fallback.

**Left-button forwarding.** The LEFT button is forwarded the same way. `App::handle_mouse`'s Down(Left) arm sends the press when `report_mouse` accepts it (child tracking, cell inside the grid, Shift not held) and records the pane in `App::terminal_pointer_forwarded`. The Drag arm sends motion to that pane when the child asked for it (`mouse_motion_reporting`: 1002/1003). Otherwise — for a click-only 1000 child such as Claude Code — it anchors croft's own selection at the remembered press cell on the first motion and extends that pane's selection directly, bypassing the generic drag path, which extends the ACTIVE pane's selection where Cmd+] can move that mid-drag. The Up arm sends the release either way. Both are clamped to the pane's grid via `clamp_to_grid`, so a pointer that leaves the pane reports the edge cell rather than leaving the child with a button it thinks is still held.

A gesture that stayed the child's skips click-to-move, copy-on-select and the annotation/secret popups; one that became a selection falls through to the normal mouse-up. The Cmd/Ctrl link-open runs before the forward, so it still wins over a tracking child. Shift+click bypasses the forward and makes croft's own selection; Shift+click on an existing selection extends it. Middle and right clicks are not forwarded, since they mean paste and context menu.

**Selection drift correction.** Selection and copy-cursor coordinates are down-anchored grid lines, re-based every frame against a monotonic scroll clock. The clock is a one-cell tracer selection parked in alacritty's own (otherwise unused) `Term::selection` slot, which alacritty rotates on every scroll including ring rotation after scrollback saturates — the point where `history_size` freezes and can no longer serve as the clock. History growth is the fallback for the rare windows where a clear kills the tracer, and the alt screen freezes the clock.

**Alt-screen selections.** A selection made ON the alt screen instead carries an `AltSelAnchor` content fingerprint: the full text of its rows. Alt-screen apps such as Claude Code scroll by repainting, so every rebase re-finds the rows by the exact-row count per candidate shift. That is at least 2 exact rows when the block has two, at least 1 non-blank otherwise, with ties broken to the shift whose captured neighbour rows also match — so a one-row selection on a repeated divider row follows its own copy, not the nearest lookalike — and then to smallest movement. App chrome like Claude Code's input box or floating pills overdraws rows anywhere in the block as scrolling slides it underneath, so partial survival anchors by however many exact rows remain.

Only the surviving rows are painted, using `visible` per-row column clips. A row part-covered by a pill keeps BOTH intact ends via `partial_overlap_cols`: paint-only credit at the anchored shift, trailing-blank runs stripped, at least 4 solid chars per end, and no minimum share, so a mid-row pill leaves under-half ends lit. Overlapping ends — an animated counter mid-row — keep the row whole. Drift is measured against `AltSelAnchor.top`, the anchor's OWN remembered position, never the selection's top, which mid-drag is the pointer, and walked upward drags' anchors one row down per event. Copy is served from the remembered extract whenever the block is not fully live.

While a mouse drag is held (`drag_selecting`, ended by `end_drag` from the app's mouse-up) the selection never goes dormant and follows the pointer over whatever the grid shows — drags start on blank or animated rows whose anchors can't match — with the definitive anchor captured at release. It goes dormant (hidden, coordinates frozen) when no run survives, revives when the content returns, and dies when the app leaves the alt screen. Edge-drag autoscroll on the alt screen forwards wheel reports to a mouse-tracking app — it scrolls, the anchor drags the selection, the head re-pins to the edge — instead of scrolling nonexistent scrollback.

**Find in terminal.** `Cmd+F` works everywhere, plus `Ctrl+F` off macOS only: on macOS Ctrl+F is a control byte the child owns and reaches it as 0x06, matching VS Code and iTerm2. Find reuses `editor_find`'s match helpers over `grid_lines_and_clock()`, which returns the whole grid plus scrollback as `Vec<String>` with the absolute `Line` of row 0 and the scroll-clock reading, under one lock. `App::terminal_find` holds the query and is bound to the pane it opened on, like quick-select and copy mode (`terminal_find_pane`, per-frame invariant plus sibling-click close; closing clears every pane's highlight). `set_search` and `set_current_match` drive the render-loop highlight, with the active match clock-stamped so it rides streaming output, and navigation re-bases its anchor before walking. `scroll_offset_for_line()` centers the active match in the viewport.

**Wide characters and text extraction.** Wide chars (CJK, emoji) occupy a `WIDE_CHAR` cell plus a spacer cell. `row_text_and_cols()` skips the spacers and returns a char-index → grid-column map, so copy and search read contiguous text while highlights still land on the right cells. `extract_selection_text` consults the WRAPLINE flag when joining rows: a soft-wrapped logical line copies as ONE line — and is stored that way in the durable history and pinned that way in the sticky header — while only hard row breaks keep their newline.

**Hyperlinks and the bell.** OSC 8 hyperlinks surface via `hyperlink_at` and `hyperlink_at_screen`, checked by the app's Cmd/Ctrl+click before the plain-text URL regex. Only http/https URIs open: the cell can carry any scheme behind unrelated visible text, so `open_detected_url` refuses non-web links and surfaces the real destination in the status line. BEL latches into a flag the app drains per tick (`take_bell` → status bar).

**Explorer re-root seeding.** An Explorer re-root seeds `cd` into the active pane only when `cwd_seed_is_safe()`. That means either the shell owns the tty's foreground group (`foreground_is_shell`, tcgetpgrp vs shell pid), or the pane is still in shell startup — nothing ever written toward the child's stdin (`input_seen`) and no OSC 133 mark recorded — where a non-shell foreground group can only be rc startup, and the seed queues as type-ahead for the first prompt. Gating on `foreground_is_shell` alone skipped the seed whenever Make Root landed inside the startup window.

**Command decorations.** OSC 133 marks are anchored on the scroll clock like annotations and images: `(line_rec, clock_rec)`, stable past scrollback saturation. A destructive clear drops the marks whose content it erased, so no phantom dots resurface. `pair_decorations()` folds the mark stream into one `CommandDecoration` per finished command: prompt line, exit code, and duration measured 133;C→133;D in the reader thread. A `CommandEnd` with no pending `CommandStart` is dropped, which filters a second integration layer's duplicate marks. The render loop paints each as a dot on the pane's left border — blue for ok, red for fail. `decoration_at_screen` hit-tests clicks and opens the decoration menu (Copy Output / Select Output / Re-run Command, headed by exit code and `human_duration`); `⌘K ⇧C`, `⇧S` and `⇧R` apply the same to the last finished command. The record carries the `PromptEnd` cell and the `CommandStart`..`CommandEnd` span, so `command_input_text`, `command_output_text` and `select_command_output` can extract or highlight exactly the typed command and its output. `drain_finished_commands` feeds the app's long-command notification for unfocused panes.

**Quick-select and triggers.** Quick-select (Ctrl+Shift+Space) pushes `HintSpan`s in via `set_hints`; the render loop paints match spans green and overlays black-on-gold label cells through the same colmap (see quick_select.rs). Trigger highlights ride the same per-row pass from the `set_triggers` Arc (see triggers.rs).

**Pane inline images.** OscSniffer also captures iTerm2 OSC 1337 `File=…;inline=1` payloads with a dedicated multi-megabyte carry cap; alacritty drops the sequence, so the tee is the only survivor. Images are anchored on the scroll clock like annotations (`(line_rec, clock_rec)`), so the picture rides ring rotation past scrollback saturation. `pane_images()` surfaces them with their current grid line. A destructive clear drops them — the pane's Clear, and program-emitted wipes via WipeSniffer over the raw bytes, where ED 2 erases on-screen anchors, ED 3 scrollback ones and RIS both; the sniffer tracks alt-screen transitions positionally within each chunk and ignores only wipes that land while the alternate screen is active. The app's `update_terminal_image_overlay` bakes the active pane's newest image through the standard ImageOverlay pipeline (`fit_image_auto` → `build_inline_image_z` at `KITTY_Z_BELOW_TEXT_AND_BG`, unique `KITTY_ID_TERMINAL` slot, clear latch in `consume_any_image_clear`). It reuses the baked payload on a pure anchor move (same seq, pane and cell size — no per-row re-decode), yields to an open context menu on iTerm2/Sixel like the editor preview, and hides the image when the anchor scrolls off screen, in the alt screen, or when the panel is hidden.

**Colors and scrollback.** Named and Indexed 0-15 cell colors render through the theme's 16-color ANSI palette (`set_palette`, VS Code dark defaults, `ansi` array override in `[[themes]]` manifests). Shift+PageUp/PageDown/Home/End drive keyboard scrollback, and all four fall through to an alt-screen program, End included. Scrollback depth comes from `terminal_scrollback` in config.json at spawn.

**Broadcast input.** Broadcast input (Cmd+K I) lives in the app: `write_terminal_key` and `paste_terminal_input` mirror shell-bound input to every non-excluded pane. Keystrokes are encoded per pane because DECCKM is per-PTY state — a zsh pane gets SS3 arrows while a bash pane gets CSI. The focused pane always receives its own input, and `broadcast_excluded` is the per-pane opt-out (Cmd+K Shift+I). Enabling routes through a red confirm modal, receiving panes wear a red ⇶ name pill, and closing down to one pane switches broadcast off.

**Block selection and copy mode.** Selection carries a `block` flag for copy mode's Ctrl+V rectangle: `block_bounds` min/maxes rows and columns independently, `cell_in_block_selection` paints, and `block_selection_text` extracts the same column slice per row. Copy mode itself is app state (`App::terminal_copy_mode`, Ctrl+Shift+Y) driving `set_selection` and `set_copy_cursor` — a green modal block painted last in the cell loop — with vi motions over `row_text` and `grid_bounds`. Each keypress first folds the scroll clock's movement since the last key into the stored coordinates, so motions continue from the re-anchored highlight instead of teleporting back to stale lines. The mode is bound to the pane it opened on: activating a sibling pane closes it, tmux-style, rather than folding that pane's independent clock into the coordinates. `w`, `b` and `e` all wrap across rows like vim.

**Command history and OSC 7 trust.** At the 133;D mark the reader thread also extracts the typed command (`last_command_input_text` over the B→C span) and the OSC 7 cwd, sending a `FinishedCommand` down the drain — the durable command history's feed. OSC 7 paths are trusted for LOCAL filesystem use (split-in-cwd, session save) only via `local_shell_cwd()`. That requires the reporting host to be this machine (`command_history::is_local_host`: full-hostname match after one mDNS `.local` strip, or the bare short name — never first-label vs first-label) AND the claimed path to name the directory the kernel says the shell is in (`cwd_of_pid`, canonicalized so macOS's /tmp → /private/tmp symlink still matches). PTY bytes carry no author and a foreground job can print a claim and die before the bytes are parsed, so no tty-ownership sample establishes provenance; a forged claim equal to the kernel's truth grants nothing. The zsh and bash shims percent-escape `%` in `$PWD` (fish always escaped) because the sniffer percent-decodes every report, and the bash DEBUG-trap guard joins an ARRAY `PROMPT_COMMAND` with `;` (scoped IFS) so empty Enters never fabricate a command.

**Close undo and session persistence.** Closing a pane parks it in `App::closed_terminals` for a 10s undo window; Cmd+K ⇧T restores process plus scrollback. The tick reaps expired parks, whose `Drop` then kills the child. The panel layout persists across restarts via terminal_session.rs.

**Progress gauge.** OSC 9;4 progress renders as a gauge over the bottom border's glyphs: heavy strokes in the state's colour (blue, red, yellow), with a wall-clock-phased sweeping segment for indeterminate. The name pill appends the percent when painted. It is held in a shared slot the reader thread writes and clears.

**Timestamps gutter.** The reader thread stamps rows by content id keyed on the shared scroll clock (clock reading plus cursor grid line — stable through scrollback saturation, frozen across alt-screen trips, never wiped by a cursor-up redraw), at chunk granularity, into a bounded BTreeMap. The "Terminal: Toggle Timestamps" palette command paints each stamped row's HH:MM:SS (libc `localtime`) down the right edge, amber with a warning mark when a row landed `STALL_GAP_MS` after its predecessor.

**Sticky command header.** When the view is scrolled so the top row falls inside a command's output with its prompt above the view, the typed command pins to the pane's top row with the scroll depth. The text comes from the decoration span for finished commands, or the newest `CommandStart`'s B→C span for the running one.

**Click-to-move prompt cursor.** `prompt_click_arrows` maps a plain click on the cursor's own prompt row (newest mark = 133;B) to synthesized ←/→ sequences, clamped to the typed span. The app's mouse-up hook writes them pane-locally, never broadcast.

**Annotations.** Cmd+K N on a selection creates a `PaneAnnotation` span anchored on the scroll clock like selections: `(line_rec, clock_rec)` captured when the prompt OPENS, so rows streaming while the note is typed don't shift the pin. They are GC'd off the scrollback floor and rendered amber and underlined, under the cursor, selection and find layers. A plain click pops the note in a `HoverPopup`, Cmd+K N over a note edits it, and Cmd+K ⇧N deletes it — all via `PromptKind::AnnotateTerminal`.

**Host accents.** The app matches each pane's `shell_host()` against the compiled prefs rules per frame and sets `accent` and `accent_badge`. An accented pane's border wears the rule colour over the focus blue with the gradient suppressed, the name pill goes black-on-accent with a warning mark, and the badge paints dim top-right.

**Collapsing a pane.** The `‹` header button, Cmd+K `[`, or the pane menu sets `PtyTerminal::collapsed` — a flag on the PANE rather than a set of indices held by the app, since indices shift on a close or a reorder and a stored one comes to name a different terminal. `terminal_pane_constraints` turns the flags into the row's widths: `Length(1)` per collapsed pane and `Fill(1)` for the rest, deliberately not `Ratio(1, k)`, which is a share of the WHOLE row and would ask the solver for more columns than exist. A collapsed pane is NOT rendered, and that is what preserves it: rendering is what resizes the PTY, and `resize` clamps to two columns, so painting a folded pane would quietly rewrap its running shell rather than erroring. `paint_terminal_collapsed_strip` draws the one-column strip (expand chevron, then the pane name down the column, single-width characters only) and records `terminal_strip_rects`, so a click anywhere on it expands. At least one pane is always expanded: `toggle_terminal_collapse` refuses to fold the last one, and `close_terminal_at` calls `ensure_a_terminal_pane_is_expanded`, because closing the expanded pane reaches the same state from the other side. Collapse and maximize are orthogonal — maximize ignores the flags rather than clearing them — and the state is session-scoped, never written to the terminal session store.

**Grid dump.** Cmd+K D dumps `grid_lines()` (blank tail trimmed) into an `open_text_buffer` scratch editor tab named after the pane.

### editor.rs

A tree-sitter highlighted editor with a full write path, mouse-drag selection, native-clipboard copy and cut, plus image, PDF and spreadsheet preview tabs.

**Canvas backgrounds.** The CSV and diff canvases fill with `canvas_bg`: `Reset` on iTerm2 to inherit the SetColors session bg, and the theme's `editor_bg` on every other host, where `Reset` would override the frame prefill and leave a host-black island. The image and PDF canvas uses `image_canvas_bg` instead — still `Reset` on Kitty hosts, because the preview sits at `KITTY_Z_BELOW_TEXT_AND_BG` and Kitty paints any cell with a non-default background over an image that deep, so an explicit fill would hide the picture behind its own canvas. The themed look survives via the bake's theme-bg letterbox.

**Minimap.** The minimap rasterizer is `minimap_rgba`.

**Code folding.** Folding is indentation-based. `fold_range` drives a gutter chevron. `is_line_hidden` binary-searches `hidden_ranges`, the merged spans rebuilt on every write to `folded` — it runs per rendered row per frame, so deriving it from the fold set there made Fold All on a large file cost millions of iterations a frame. The render's row builder skips hidden lines, and cursor movement steps OVER a collapsed block. `reveal_cursor_fold` guarantees the caret is never left on a hidden line: it runs from `pin_on_edit` so no edit lands on one, and again at the top of the render, where every direct `cursor_row` setter converges (Go to Definition, search, Ctrl+G, a restored session) and where a caret with no painted row would otherwise leave nothing drawn at all. `open` drops fold state — line numbers into the old file — only when the incoming path DIFFERS, since `open` is also the same-path reload behind every FS-sync sweep, and dropping it there popped the user's blocks open whenever anything rewrote the file.

**Debugger inline values.** This is VS Code's `debug.inlineValues: auto`. On `InspectionUpdated` the app feeds `inline_locals` — the Locals-named scopes' already-fetched variables, with the first scope as fallback — into `set_inline_values_from_locals`. That annotates each line from the enclosing fold header down to the stop that mentions a local as a whole identifier (`identifier_tokens`) with a capped, middle-elided `name = value` trailer. It is painted like the blame trailer, which yields to it on the cursor line, and cleared wherever the stop arrow clears. Palette "Debug: Toggle Inline Values" and Settings control it, persisted as `disable_inline_values`.

**Testing play glyph.** `locate::test_fn_on_line` marks `#[test]`-family fns and pytest `test_*` defs with a green ▷ in the sign margin; the breakpoint dot and stop arrow outrank it. `test_glyph_at` maps a click to the test's name, which the app runs through the test worker.

**Git gutter.** `refresh_git_marks` diffs the buffer against the app-supplied HEAD baseline, and `GitMark` bars paint in the gutter. The app fetches and invalidates those baselines across EVERY editor group, since inactive split panes render their editors too, and a baseline left at the old HEAD keeps marking lines a commit already cleaned.

**Bracket matching.** `bracket_match_pair` tints the bracket beside the caret and its partner.

**Render whitespace.** `WhitespaceMode` has three settings. Selection — the VS Code default — swaps space and tab cells to `·` and `→` inside the primary and secondary-caret selections; All does it across the row. Both are painted after the indent guides so the real character's glyph wins the cell, while the later selection band only recolours its background. Palette "View: Toggle Render Whitespace" cycles selection→all→none, persisted as the `render_whitespace` string, frame-synced to every editor.

**Bracket-pair colorization.** VS Code has had this on by default since 1.67. `scan_bracket_colors` rebuilds per-line `(col, colour idx)` pairs alongside every `recompute_highlights`, using one shared depth counter cycling three theme colours (`bracket_pair_color` picks the VS Code dark or light set by bg luminance). Unmatched closers are red. Brackets inside the grammar's string and comment captures are skipped via `highlight_text_with_protected`'s merged byte ranges. The row loop repaints just those cells' fg after the text, so search and selection layers still win. Palette "Editor: Toggle Bracket Pair Colorization" and Settings control it, persisted as `disable_bracket_colors`, frame-synced to every editor.

**Indentation guides.** This is VS Code's `editor.guides.indentation`. The row loop paints a dim `│` into blank cells at each indent-unit column below a line's leading-whitespace width. `guide_indent_width` lets a blank line borrow the MIN of its non-blank neighbours, so guides cross gaps inside a block but stop between blocks. `active_indent_guide` resolves the cursor block's innermost guide once per frame — a header activating the body it opens — painted brighter over that block's rows only. First visual segment only, translated through horizontal scroll and inlay cells like every painter. On by default; palette "View: Toggle Indent Guides" and Settings control it, persisted as `disable_indent_guides`, pushed to every editor in every group by the per-frame focus sync.

**LSP occurrence highlights.** `apply_occurrences` converts documentHighlight's UTF-16 spans to char columns. The row loop tints reads with `occurrence_bg` and writes with the stronger `occurrence_write_bg`, cleared on every edit or caret move. This is VS Code's word highlight.

**Multi-cursor editing.** `carets` holds them: `select_next_occurrence` for Cmd+D, `add_caret_at_screen` for Alt+click, and `begin_box_select` / `box_select_to` for Shift+Alt column selection. ALT owns these because SGR mouse reports carry only shift, meta and ctrl and no super, so Cmd+click cannot be told from Option+click — and Go to Definition rides CTRL instead, the modifier the terminal pane already uses for its file and URL clicks.

**LSP inlay hints.** `apply_inlay_hints` decodes the seq-gated batches into per-line `inlay_spans`. The render splices each label into the row as dim italic virtual cells at its anchor. Every overlay painter, the caret (`cursor_screen_pos`), horizontal scroll (`ensure_cursor_col_visible`, `content_cols`) and mouse mapping (`buffer_pos_at`, where hint cells snap to the anchor) translate buffer columns past them via `inlay_cells_before` — except carets (primary, secondary blocks, ghost carets), which use the strictly-before rule so a caret AT a hint's anchor sits left of the hint like VS Code. Wrap mode suppresses hints. On by default; palette "Editor: Toggle Inlay Hints", persisted as `disable_inlay_hints`.

**Tab-title disambiguation.** Following VS Code's labelFormat, `disambiguated_tab_labels` gives colliding basenames the shortest distinguishing trailing directory, going deeper only when parents also collide. The strip and the OPEN EDITORS projection share it, so the two views never name one file two ways.

**Breadcrumbs and sticky scroll.** `EditorTabs` renders both. App pushes `breadcrumbs` and `sticky_lines` each frame from the outline scope chain, and `breadcrumb_target_at` / `sticky_line_at` map clicks to jumps. Crumbs are budgeted and advanced in display CELLS via `set_stringn`, so a CJK segment is neither overpainted by the next crumb nor mis-hit. The sticky band floats over content but stops above the caret's PAINTED row, read back out of the frame's own row layout so a collapsed fold or comment box between the top and the caret cannot make the band overshoot. A caret under the band cannot be painted at all, so `caret_floor_row` keeps the scroll from parking it there in the first place — dragging it to the first row the band does not cover, instead of to `scroll`, which had suppressed the band for the whole of a wheel scroll.

### src/remote.rs

Remote (SSH) target metadata and launch dispatch, plus the ssh-pane re-root offer.

**Detecting the host from argv.** `ssh_destination` parses a pane's foreground ARGV for the host it is connected to. It uses argv rather than scrollback because what the user typed may have scrolled away, been edited, or never been typed at all — a shell function, or a `Host` alias ssh resolved itself — while argv is what is running. The parse needs ssh's own flag grammar: without knowing which flags take a separate argument, `ssh -p 2222 box` yields `2222`, the first word not starting with `-`. Boolean flags need no listing, so a flag added to ssh later degrades to "skipped" rather than to a wrong host. `resolve_offer_host` matches alias or HostName case-insensitively, ignoring any `user@` — the user is how you log in, not which machine it is — and refuses a host with no config entry, because the remote flow it hands off to is keyed on one.

**Why a palette command first.** The first slice used a palette command rather than an automatic prompt. An offer that appears on detection needs a per-host opt-out, a memory of hosts that refused provisioning, and a decision about nagging, none of which should be invented alongside the detection.

**Launch dispatch and stdin discipline.** An installed remote croft is attached to immediately — the presence probe fires the launch signal within one roundtrip — while any update cross-builds and ships on a background thread, surfacing as the running croft's F9 reload. The install must never read the terminal the attached session is reading, or the two split the user's keystrokes between them. So every ssh command it runs directly carries `-n`, and rsync and the zig cross-build (whose nested ssh has no `-n` of its own) are spawned with stdin closed, which their children inherit. The one exception is the throughput probe, which pipes its payload into a remote `cat` and so must keep the stdin it feeds.

**Toolchain probes run from the checkout.** The toolchain-sensitive rustup queries on this path run FROM THE CHECKOUT (`cross_tool_command_in_checkout`); plain availability probes stay directory-neutral via `cross_tool_command`. `rust-toolchain.toml` resolves per working directory, so a probe run elsewhere answers for the DEFAULT toolchain while `cargo zigbuild` answers for the pinned one. A target present on only one of them made the "target missing" guard pass immediately before the build died with E0463 — four days of silent fallback to remote `cargo install` after the 1.95.0 to 1.97.1 bump. Rustup targets are per-toolchain and a bump orphans them all, which `.github/workflows/ci.yml` and `remote::tests::the_pinned_toolchain_has_every_cross_target` now catch.

**Source sync uses an allow-list.** The sync ships an ALLOW-list built from the same `SOURCE_STAMP_INPUTS` the stamp hashes (`source_sync_filter_args`), never a deny-list. The old `--exclude=target` never matched this repo's real build dir `target.noindex`, so every fallback install pushed 176 GB of artifacts to the box at the bulk lane's throttle, and three servers sat 23 to 27 GB deep in it. Each sync first clears the remote source dir of everything but the `target/` build cache and the inputs the local checkout still contains (`remote_source_dir_prep`), since `--exclude=*` protects stale unlisted files from `--delete` and the tar fallback never deletes. Current inputs stay, so a concurrent install's source is never yanked and rsync stays incremental. The source stamp gating reinstalls hashes only the build inputs (src/, assets/, Cargo.toml/lock, build.rs, rust-toolchain.toml), never build artifacts or docs.

**Drop-relay pump.** The local launcher tails the remote `requests.log` and runs each verb locally: pull, clipboard (fetch the local clipboard for a remote paste), copy (the reverse — put a remote croft's copied text on the LOCAL clipboard, so `Cmd+C` on a remote box reaches the clipboard the user pastes from), open, plus forward (adds an `-L` to the live SSH master via `ssh -O forward` and opens the local browser) and unforward (`ssh -O cancel`).

**The automatic offer.** In the second slice, the label thread in `refresh_terminal_labels` — already off the loop for the sysinfo name lookups — also runs `ssh_pane_samples`. For a pane whose foreground is ssh, that reads the argv (`process_cmdline`) and resolves the host (`ssh_offer_host` against `discover_ssh_targets`, parsed once per pass and only when some pane is ssh). An ssh whose argv cannot be read gets no verdict rather than a session end. It ships `(shell pid, host)` over `ssh_rx`; `apply_ssh_samples` maps the shell pid to the pane uid and hands it to `consider_ssh_offer`, which raises an `SshOffer` in the status bar when a pane's host CHANGES to a known one. `offer_allowed` requires that it is not `disable_remote_offer`, not in `remote_offer_excluded_hosts`, and not in the learned-refusal set. The offer drops when the session ends, the pane moves to another host, the pane closes (`expire_ssh_offer`, from the top of `render`, no system calls), Esc is pressed (Press/Repeat only, non-consuming, top of `handle_key`), or 90 s pass. Cmd+K G accepts through `request_remote_launch`.

**Remembering failed installs.** Hosts whose install failed are remembered in `~/.cache/croft/remote-offer-refused.json` (`note_provisioning_failed` → `remember_refused_host`, with `load_refused_hosts` at startup) and not offered again until a later install of that host succeeds (`note_provisioning_succeeded` → `forget_refused_host`). The seven-day TTL is checked by `refusal_live` each time the policy is asked, so it binds entries learned during the run too, and a timestamp from the future — a clock moved back — counts as expired. Both prefs re-apply on a settings remerge, which also takes down an offer the new settings no longer allow.

### git.rs

Branch, dirty and ahead-behind status, plus the full set of working-tree operations the Source Control panel drives.

**Multi-root.** There is one `GitWorker` per workspace root (`App::git` primary plus `git_extra`), each discovering its own toplevel. The SCM surfaces — status-line branch, panel, COMMITS graph, and `scm_root` for panel ops — follow the ACTIVE repo, meaning the root owning the focused file, with the primary as fallback. Non-active workers drain status-only and have their change lists re-requested on switch. HEAD-oid invalidation for gutter baselines is per repo, gutter/blame/timeline pair each file with its owning root, and the Explorer's ignored set is unioned across every root's cwd-scoped query.

**Toplevel versus workspace root.** The status poll discovers the repository toplevel (`repo_toplevel`, `rev-parse --show-toplevel` — the same probe that answers repo membership) and carries it on `GitStatus.repo_root`. Porcelain and numstat paths and `HEAD:<rel>` pathspecs resolve against the TOPLEVEL however `-C` is set, so every porcelain-derived join, Source Control mutation and HEAD read goes through `App::scm_root` (the toplevel, or the workspace root outside a repo). That way a workspace rooted in a repo subdirectory stages, discards and diffs correctly. Paths croft itself computed relative to the workspace — merge-complete staging, blame, file history — keep their workspace pairing, and the Explorer's ignored-set query deliberately stays at the workspace root, because `ls-files` is cwd-scoped and that is exactly the subtree the Explorer needs.

**Operations.** stage(/all), unstage(/all), commit (staged / all / amend), push (/force/to-remote/publish), pull (/rebase), fetch, sync, clone, branch list / checkout / create (/from) / rename / delete / merge / rebase, remote list / add / remove, stash push (/untracked/staged) / list / apply / pop / drop, tag list / create / delete, discard (/all), and diffs. Hunk-level staging feeds self-generated single-hunk unified patches to `git apply --cached[/-R]` on stdin. Every read-only poll runs with `GIT_OPTIONAL_LOCKS=0`, so background status never takes `index.lock` out from under a user mutation. `blame` and `parse_blame` (`git blame --line-porcelain`) back the editor's GitLens-style inline annotation.

**The Explorer's greyed-out set.** This is two steps. `ls-files --others --ignored --directory` gives a cheap candidate list, where a fully-ignored dir collapses to one entry instead of enumerating `target/`. Then `check-ignore -z --stdin` confirms each, because the listing also collapses any entirely-untracked directory whose contents merely happen to be ignored — `logs/` would otherwise grey with no rule matching it. Both calls read RAW bytes (`git_raw`): git sorts a leading-space filename first, so a blanket `trim()` would eat that space, and a filename need not be UTF-8.

**Worktree lanes.** The pane half lives in `App::open_lane_pane`: a pane named `Lane: <slug>` in the worktree, with the `lane_agent` row's launch line typed in, recorded in `lane_panes` and saved on the pane's `terminal_session::PaneRecord.lane` so `restore_terminal_session` seats the agent again. `close_lane_panes` closes it when the lane is removed. `sync_lane_root_badges` → `FileTree::root_badges` shows it on the lane's Explorer root row — branch plus the seated agent's badge — and the review queue is grouped by root through `AgentLedger::lane_by_root` → `App::agent_lane_rows`, each group named by its lane's branch.

`WorktreeLane::plan` derives a branch `agent/<slug>` and a SIBLING directory from one slug. A worktree nested inside its own repo is a working tree git then tries to track, and the Explorer would show the lane inside the root it was cut from. One slug covers git's ref grammar (no `..`, whitespace, or `~^:?*[\`) and the filesystem together, rather than sanitising twice with two chances to disagree. A name with nothing usable is REFUSED rather than becoming the branch `agent/`.

**Lane removal fails closed.** `lane_removal_block` runs `status --porcelain --ignored`, because plain `--porcelain` OMITS ignored entries while `git worktree remove` deletes them anyway. Measured: a `.env` in a lane was destroyed while the check reported the tree clean, and a lane is exactly where an uncommitted local config or an expensive `node_modules` lives. This is the safety rule and the one check here where being wrong destroys work. `git worktree remove` discards a dirty tree, so the check FAILS CLOSED: a status git cannot report on is treated as unsafe, since refusing costs one manual removal while guessing costs the user their work. `remove_worktree_lane` deliberately omits `--force`, which would make that refusal advisory. And the caller runs git BEFORE dropping the workspace folder, because "is there uncommitted work" is not the same question as "will git remove this" — a LOCKED worktree reports a clean status and is then refused — so the other order would leave the lane gone from the workspace and the directory still on disk.

### collab.rs

Multiplayer Phase D (docs/MULTIPLAYER.md): the whole collab layer below the app. A replicated text document (`CollabDoc`) over the `cola` text CRDT lets participants editing the same buffer from independent viewports converge without a central authority.

**The document.** `CollabDoc` owns the canonical text next to a `cola::Replica`, which holds positions only. `text_delta_ops` diffs a buffer's last-synced text against its current text and emits convergent ops, so the editor needs no apply-edit chokepoint: multi-cursor, paste, undo and wholesale reloads all reduce to text-state transitions. `apply_remote` integrates inbound ops into sequentially replayable spans, draining cola's causal backlog with insertion text stashed by run identity. `byte_offset` and `position` bridge `(row, char-column)` to byte offsets, UTF-8 aware.

**The wire.** `CollabMsg` carries ops, the SnapshotRequest/SnapshotReply bootstrap handshake, carets, and the AI-stream pair StreamState/StreamCancel. Unknown variants are skipped at drain, so peers across versions interoperate. It rides `relay_serve`, a dumb attach-or-create fan-out over `<hash>.collab.sock`. Forwarding writes are bounded with `SO_SNDTIMEO` 2s, and a wedged peer is shut down and dropped, since forwarding holds the global client mutex and one SIGSTOPped peer used to deadlock relay and participants alike.

**The channel and reconnect.** `CollabChannel` is a participant's non-blocking framed connection (CROFT_COLLAB_SOCKET / CROFT_COLLAB_ROLE, exported by the launch tails). The read is inert under `cfg(test)` so a suite launched from a croft session never hands test-built Apps a live seat. EOF or a failed send latches it dead, and the app drops the session — harvesting each live doc's last replicated text as a merge base — then reconnects, replaying offline buffer divergence into the re-bootstrapped doc as a three-way merge against that base (`merge_offline_texts`, line-level, ours wins a same-line conflict). Neither the offline work nor the edits the owner made during the outage are wiped. Before this, a killed relay used to read exactly like an idle one.

**The per-file state machine.** `CollabSession` handles it. Guests bootstrap each opened workspace file from the owner, input-gated until the snapshot lands, timing out to local-only when no owner answers — a latch that plain re-requests respect, that exempts the file from the guest save gate, and that only the collab agent's `collab_open` rejoins. The owner allocates site ids, pid-seeded so a restarted owner never re-issues an id a surviving guest holds. Every wire file key is checked with `contained_path` — plain components plus canonical containment of the deepest existing ancestor, so symlinks cannot smuggle a path outside the root — before touching the filesystem. The session is the single writer to disk.

**Pumping it.** `App::poll_collab` runs per tick with extract-before-apply, the echo-storm invariant. Remote spans go into every attached tab via `apply_span_edits`. Buffers carry the doc's `text_gen`: a fresh split or reopen of a live file holds stale disk text, never extracts, is input-gated until seeded from the replica, and `Editor::open` drops the attachment on a path change while keeping it for same-path reloads. Behind panes mirror, so all panes of a live file converge — text tabs only, and every matching one, since diff, image and sheet tabs carry the path without being documents. With the link down, the active pane mirrors onto its siblings so offline panes cannot diverge, and a disconnected guest may save. Peers' carets paint as colored ghosts with a name tag; the name rides `CollabMsg::Caret`, and the tag paints on the row above the caret and hides 2s after it rests, VS Code Live Share style.

**Launch.** `croft attach --solo` / `croft remote <host> <path> --solo`.

### log_view.rs

A windowed backing store for the rendered ANSI log view.

**The line index.** A `u64` line-offset index is built with `memchr`, because a per-byte comparison cannot use the vector instructions that make a newline search memory-bound, and this scan IS the cost of opening a large log: `188ms against 53ms over 122 MiB`. It is capped at `MAX_INDEXED_LINES`, past which the view reports truncation rather than showing a prefix as if it were the whole file. A 256 KiB parsed window around the viewport means each scroll costs one bounded read.

**Split indexing.** `open` indexes the first `HEAD_INDEX_BYTES` (8 MiB) synchronously, and a background thread streams the rest back in per-chunk batches that `App::poll_log_index` folds in from the main loop. A multi-gigabyte log therefore opens in the time its head takes to scan. Until the pass finishes, `len()` is a lower bound, the header says "so far, indexing…", and a sweep that reaches the moving end reports out-of-reach or partial rather than absent.

**Find.** `find_next`, `find_prev` and `count_matches` stream their own windows WITHOUT touching the viewport cache, matching on the stripped text through the shared `line_matches`, with an allocation-free `line_may_match` pre-filter in front of it.

**Mouse selection.** It lives here for the same reason: the tab's `lines` is a stub. The view carries its own `selection` in absolute (line, char column), the renderer publishes the body rect it painted (frame truth) for hit-testing, and `selection_text` gathers the STRIPPED text through the same windowed sweep, capped at `MAX_COPY_BYTES` and reporting when it clamped.

**Budgeted sweeps.** Every sweep is budgeted (`FIND_SCAN_BYTES`) because find runs per keystroke and an unbounded sweep would stream the whole file per keypress. A budgeted count is reported AS budgeted — `count_truncated` renders `N+` — since a partial count shown as a total is the one outcome a reader cannot detect.

**Highlighting.** `ensure` runs each line of the window that is BOTH free of escape bytes AND starting from the default carried style through the `tailspin` crate (library only, `default-features = false`) before `parse_line`. A line inside a colour block an earlier line opened is not "plain", and lines wider than `MAX_HIGHLIGHT_BYTES` are skipped. Routing is unchanged: `Editor::should_open_as_log` still requires ANSI colour in the first 8 KiB, so an uncoloured log stays an editable text file and only a coloured log's plain lines are highlighted — widening that is a separate decision. Dates, numbers, UUIDs, IPs, URLs, paths, quotes and severity keywords come out as ordinary SGR spans through the same palette path as a self-coloured log, and a line that carries its own colours is parsed as written. There is one `Highlighter` per thread, a `thread_local`, since building it compiles its regex set.

**The highlight flag.** The per-view `highlight` flag (`set_highlight` drops the parsed window so the next paint re-parses; index, selection and find never read colours) is seeded from a process-wide default. The app sets that from `disable_log_highlight` at startup, on a settings remerge (a workspace layer may set the key), and on the "Log: Toggle Highlighting (tailspin)" palette or Settings toggle — each of which also flips every open view through `App::set_log_highlight`.

### iterm2_inline.rs

The inline-image baking pipeline and protocol dispatch: iTerm2 OSC 1337, Kitty graphics, and DEC sixel via a DA1 probe. It serves the welcome wordmark, image and PDF preview, activity-bar icons including the settings gear, the SSH empty-state hero, and the editor minimap.

**Icon colors are chosen at bake time.** A baked icon picks its colors at bake time, not at the render call site, so it cannot route through `Theme::ui` the way a styled cell does. The whole icon family — activity bar, Customize Layout toolbar, Run and Debug headline — takes an `IconInk` palette (resting and selected-and-hovered glyph, plus selection pill) that App resolves from the active theme. That is what keeps a light bar from painting the selected icon white on white. The glyph fallback for image-less terminals reads the same `Theme::activity_icon_*` values, so both paths draw the bar in one palette.

**Stale-image eviction is protocol-keyed.** `App::image_eviction_needs_screen_wipe` decides: iTerm2 and sixel cells need the main loop's full `2J` plus repaint, while Kitty images are deleted on their own layer with the delete-all escape and the text buffer untouched. A Kitty screen wipe would blank the app for an SSH round trip on every clear latch — the every-keystroke blink bug.

**Content-only overlay changes.** A minimap rebake on edit or scroll re-emits over the same cells and must never arm the clear latch; only a moved or resized placement may.

**The minimap and menus.** The minimap raster is emitted at Kitty z=0, above text, after ratatui's diff on EVERY frame, so any pane that ratatui paints over the strip would be painted back over. A menu whose rect actually INTERSECTS the strip (`App::menu_covers`, checked against both `menu_rect` and the further-right `submenu_rect`) evicts it and suppresses the re-emit — the same collision `cb0481b` fixed for the welcome logo. The test is the intersection and not `context_menu.is_some()`, because menus are clamped to the frame rather than snapped to it: gating on any open menu would arm the clear latch on every right-click in the app, which is a full screen wipe and repaint on the cell-buffer protocols.

**The one unconditional wipe.** Resize/WINCH (`App::consume_resize_repaint`) always wipes. A dtach reattach fires WINCH against a blank physical screen while ratatui's back buffer says every cell is painted, so the next frame must clear and repaint on every protocol, Kitty included.

**Re-asserting terminal modes on reattach.** The same WINCH also re-asserts the startup terminal modes (`mode_reassert_seq` in app/mod.rs: alt screen, mouse tracking, bracketed paste, kitty keyboard flags via the SET form, never a push) when `CROFT_SESSION_PERSISTENT` is set, because a reattached terminal never received the startup DECSETs and would otherwise have a dead mouse until the next F9 re-exec. `takeover_mode_seq` is the single source of truth for those modes — startup, the post-scp TUI restore, and the reattach re-assert all emit the same block, with a test pinning the embedding — so a mode added at startup can never go missing from the reattach path. New startup modes go there, never inline at a call site.

### session_host.rs

The multiplayer session mux (docs/MULTIPLAYER.md): a hidden `croft session-host` subcommand whose server owns the inner croft's PTY and broadcasts output verbatim to N unix-socket clients. It is byte-transparent, so Kitty graphics survive.

**What the mux adds over dtach.** Per-connection input attribution with server-enforced write control: the first attacher holds it, Grant/Revoke frames move it, and read-only input is dropped at the host. Winsize is the min of clients, with a same-size repaint jiggle on attach — the role dtach's `-r winch` played. There is a presence roster (an atomic sidecar JSON next to the socket plus Presence frames), and inner exit-code propagation, so drop-to-local's 88 survives the mux where dtach swallowed it.

**Framing.** Both ways: `[type u8][len u32 be][payload]`. Type 0 is raw PTY bytes; type 1 is one compact-JSON Control message.

**The privileged channel.** A token-authenticated channel (CROFT_SESSION_TOKEN, `InnerChannel`) lets the inner croft grant, revoke and kick, and receive Typing attribution frames, sent on writer change and ordered before that client's bytes reach the PTY. Those drive per-participant parked carets and the editor's colored ghost carets. The env read is inert under `cfg(test)` so test-built Apps never authenticate to a live host. The remote launcher prefers the mux behind a `--probe` guard and falls back to dtach.

**Version mismatch no longer strands an attach.** A stale server that survived binary upgrades used to. Now Hello carries the client version, the server answers with ServerHello before any PTY bytes, and a mismatch — or a pre-0.1.698 server, detected by a 2s reply timeout — shows a plain-text banner offering continue, restart (SIGTERM via the socket-unique argv, then respawn), or detach, never a silent blank screen.

**Host self-replacement.** A host replacing its own image latches `swapping` under the clients lock BEFORE it broadcasts HostSwap, and from then on answers a newly accepted connection with HostSwap plus close instead of seating it. The reconnect its own invitation triggers arrives ~200ms later, inside the flush pause, and a client seated by a process about to `exec` reads the resulting EOF as "session over" and exits — which is how a background update kicked every attached client off a live remote session. The latch is released if the exec fails, so a stale host still serves.

**Two smaller fixes.** The attach repaint jiggle runs even when every client is a size-less observer; `min_winsize` returning None used to skip it. And `broadcast` bounds each client write with `poll(2)` at `WRITE_FRAME_DEADLINE`, evicting a non-draining peer and recomputing the shared size so a dead ghost releases the min winsize it pinned — a plain `write_all` used to wedge the PTY pump forever while holding the clients lock.

### fleet.rs

Running one command across N ssh hosts and comparing what came back, behind "Terminal: Fleet Run".

**Output shape.** Results are TEXT LINES in a `Fleet` OUTPUT channel: one per host marked `same`/`DIFFERS`/`FAILED`, then the summary. This is not the tiled view the issue describes, which is still to build — see #363. Broadcast typing covers "type once"; this is the other half, and the comparison is the whole value, because reading ten near-identical `uname -r` outputs by eye is the task a person is worst at.

**The fleet must be named.** The request is `hosts: command`, with `*` as the explicit broadcast. Defaulting to every entry in `~/.ssh/config` would mean running arbitrary text on every remote the user ever configured — a live production box beside a `github.com` entry that is not a shell host — and a confirmation reading "run on 5 hosts?" asks them to approve a list they cannot inspect. Choosing the fleet is the feature, not a nicety on top of it.

**The reference is the plurality output.** Keying on the first host makes the answer depend on the order hosts happen to be listed in, so the same fleet listed differently would highlight a different set. Only SUCCESSFUL hosts vote: three identical `Permission denied` results becoming the normal would paint every working host as the deviant one, the exact inversion of what the summary is for. An even split yields no reference at all, because with 2-and-2 there is no honest normal, and choosing one deterministically is worse than choosing none — a stable wrong answer reads as a considered one.

**Parallel, and off the event loop.** Hosts run in parallel, one thread each, and a host that never reports still gets a tile: a fleet view that silently drops a host is worse than one showing it red, since the user counts tiles to know the run finished. Results arrive through a channel drained on the tick, like the port poll and the label lookup. `run_on_hosts` returns a `Vec` so calling it inline reads naturally — and that would freeze the editor for up to the timeout while the network answers, leaving the user unable to scroll, type or cancel.

**The ssh invocation.** It carries `-n`, or the remote commands read the terminal croft is using and race for the user's keystrokes, and `BatchMode=yes`, so a host wanting a password fails rather than hanging on a prompt nobody can see.

### review_threads.rs

GitHub review threads rendered as the editor's comment boxes, behind "Review: Load PR Comments for This File".

**The core difficulty.** GitHub anchors a comment to the diff line it was written against, and NULLS that `line` once the branch moves under it, while keeping `original_line`. The two mean different things — where the comment is NOW versus where it WAS — so rendering an outdated thread at `original_line` silently attaches someone's objection to whatever code now occupies that number, which after a rebase is routinely a different function.

**Verified against the live API, not a model of it.** An outdated comment really does arrive as `line: null` with `original_line` set. The REST comments endpoint carries NO resolution state at all: `isResolved` lives only on GraphQL's `reviewThreads`, a different query keyed on threads rather than comments, so `resolved` here is filled by a caller that has merged it in, and defaults to unresolved otherwise. And `position` / `original_position` are DIFF offsets, not file lines.

**Marking outdated threads.** An outdated thread is anchored but MARKED, in the title rather than only in a colour, since a reviewer skims titles and a colour is what a screenshot or a colour-blind reader loses.

**Nothing is dropped.** A thread with neither line becomes a file-level box rather than being dropped: a review comment nobody sees is the failure this exists to prevent. An outdated line past the end of a since-shrunken file is clamped into view, because a box at line 4000 of a 100-line file is the same as losing the comment.

**One conversion site.** Lines are converted 1-based to 0-based ONCE, in `anchor_of`, because two conversion sites is how an off-by-one reaches the screen.

**Filtering to the file.** Threads are filtered to the file being viewed, or another file's objections hang off this buffer's line numbers. That filter compares against the REPOSITORY TOPLEVEL, not the workspace root: the API's `path` is repo-relative, and the two coincide only when the root IS the toplevel, so opening croft on `repo/src` made every comparison fail silently and a file with comments reported having none — the same toplevel-versus-workspace-root rule as git.rs, hit again in a new place. Components are joined with forward slashes, since the API always uses them and a Windows `to_string_lossy` would yield backslashes that match nothing.

### theme.rs

The IDE color theme. A `Theme` is a palette — background, accent, selection, button, gradient flag, OSK colors, 16-color ANSI, and a `SyntaxPalette` of eight code-highlight colors — loaded as data from `[[themes]]` blocks in bundled and user extension manifests. Croft Black, Dark and Light ship in `assets/extensions/themes/` and are baked in. `Theme::BLACK` is a const fallback so croft is never themeless.

**Light themes.** `Theme::is_light` (background luma) flips the derived blends: sticky headers, ignored-file grey, occurrence tints, tab lift. `Theme::ui` adapts the legacy hardcoded dark-chrome constants — identity on every dark theme, a curated VS Code Light Modern mapping (plus a hue-keeping luminance-flip fallback) on light ones — so popups, pickers, sidebars and menus follow Croft Light without repainting dark themes.

**Host terminal colors.** The theme drives SetColors, baked-image fills, and the portable OSC 10/11 dynamic-colors pair, emitted at startup and on switch for every host terminal and reset via OSC 110/111 on exit and in the panic restore. So the HOST terminal's default fg/bg follow the theme on Ghostty and friends, not just iTerm2.

**Syntax palette.** It is pushed into `highlight::set_syntax_palette` on every theme switch and at startup, so tree-sitter and LSP semantic-token colors follow the active theme, with open editors re-highlighted on the switch. Omitted `syn_*` manifest fields fall back to the historical Base16-Ocean-Dark defaults.

**Tab-strip chrome.** Strip background, inactive and active tab, hover lift and close pill are per-theme data too (`tab_*` manifest fields). The two built-ins pin their historical constants, and every other theme derives them from its palette — strip from `search`, active from `selection`, hover and pill from `accent` — so the tab bar follows the theme instead of staying navy.

**Integrated terminal.** Each pane is prefilled with `theme.editor_bg()` before the grid paints, so default-bg cells show the theme background even on Ghostty and Kitty, which ignore the `SetColors` OSC that iTerm2 honors. Its 16-color ANSI palette comes from the theme's `ansi` array via `set_palette`.

### workspace.rs

The workspace's root-folder set for multi-root support: `WorkspaceRoots`, an ordered, never-empty list.

**The four accessors.** `primary()` is the launch identity — session sockets, relay and pair records stay keyed on it. `owning_root()` is the longest-prefix owner that per-root subsystems resolve against. `replace_primary` is the re-root semantic: a re-root changes what the window IS and collapses the set. `add` and `remove` are the Add/Remove Folder gestures, and the primary is never removable.

**Persistence.** The folder set persists per PRIMARY root in `~/.config/croft/workspace_folders.json`, following the terminal-session model where an empty list prunes its key. It is restored at startup and on a re-root through the normal add fan-out, with vanished folders dropped from the record.

**`.code-workspace` files.** VS Code-compatible files are supported. `parse_workspace_file` is JSONC-tolerant via keymap's comment strip, resolves folders against the file's dir, and ignores settings, launch and tasks, since croft's settings are global. `write_workspace_file` writes its own dir as `.`, siblings through `..`, and absolute paths only without a shared prefix. `croft x.code-workspace` opens one from the CLI, where the first folder is primary and the file's set replaces the auto-store restore. Palette "Workspaces: Save Workspace As" and "Open Workspace from File" round-trip in-session.

**Fan-out on add.** `App::add_workspace_folder` fans an added root out to: the Explorer section; a per-root `FsWatch` (the per-OS watch caps split across roots, since they bound kernel resources per PROCESS, and each instance attributes drain events against ITS root); the multi-root Cmd+P index (`build_multi_file_index`, with root-name-prefixed rels and `filename_start` shifted so filename-tier scoring holds); the search worker (`SetRoots`, one balanced Hits stream over every root, with the panel stripping display paths against the owning root, name-prefixed when multi); the LSP layer (no `didChangeWorkspaceFolders`, because croft runs a server per (language, project root), so `project_root_for` just bounds its manifest walk by the OWNING root and the added root is prewarmed like a fresh workspace); and the window title's " (Workspace)" marker.

### gui_path.rs

PATH repair for GUI launches, run once at the top of `Cli::run` before anything spawns.

**Why it is needed.** macOS launchd starts Croft.app — from the Dock, Spotlight, or an opened document — with only `/usr/bin:/bin:/usr/sbin:/sbin` plus whatever bundle dir the launching app appends. Ghostty's `--initial-command` runs croft under `bash --noprofile --norc`, so no rc file ever restores the rest. Every tool croft shells out to (pdftoppm, pdfinfo, git, ripgrep, the language servers, the pair CLIs) becomes invisible, and PDF previews freeze on page 1 with no page count.

**Detection and repair.** A PATH holding nothing but those system directories — `.app/` entries discounted, since an app putting its own bundle there is not a shell having run — is taken as a GUI inheritance. Repair asks the user's login shell via `$SHELL -l -i -c`. csh and tcsh accept `-l` only alone, so that family is marked a login shell the way login(1) does it, with a leading dash in argv[0], and runs `-i`, which makes tcsh read `~/.login`, the conventional home of a csh user's `path`. The value is printed by an inner `/bin/sh` reading the environment, because fish expands its own quoted `$PATH` space-joined. It is marker-delimited so rc-file chatter cannot be mistaken for the value, and read up to the END marker so a backgrounded grandchild holding the pipe open cannot stall it. It is bounded by a 3s timeout and reaped off-thread, so a wedged rc cannot wedge startup. And it runs in its own session: an interactive zsh takes the terminal's foreground process group for job control, and a probe left in croft's session stole the TTY and made raw-mode setup fail with EIO once it exited.

**What skips the repair.** The pure liveness probes (`session-host --probe`, `collab-relay --probe`) return before the repair runs, because the remote launch script fires them on every SSH connect and remote attach never waits. The recovered answer goes in front of what we inherited, deduped, and nothing inherited is dropped. A terminal launch never probes. This is the same approach VS Code and Zed take.

### worker.rs

The test worker: RunAll, RunOne, RunFilter, RunSuite (header click) and Discover, per runner.

**Suite anchoring differs per runner.** The cargo arm anchors the suite with `suite_pattern`. pytest gets it positionally as a node-ID prefix. vitest and jest anchor the describe chain via `suite_title_anchor`, because their `-t` is an unanchored regex over the space-joined full name.

**The runners.** cargo (`test --no-fail-fast`, `-- --list`), pytest, vitest (`run --reporter=tap-flat`, `list`, file plus regex-escaped `-t`, since vitest's `-t` is a regex like jest's) and jest (`--json`, `--listTests` for file-level discovery, file plus regex-escaped `-t`). They run via `cargo_cmd` / `pytest_cmd` / `js_cmd` — absolute binaries for the GUI-stripped PATH, `node_modules/.bin` first, NO_COLOR and CI for JS — plus `run_streaming`, which tees stderr to the Test Runner OUTPUT channel and streams Started(Activity)/Case/Finished over an mpsc channel the app drains each tick. The parse contract is a Vec per line, since jest's one JSON line carries the whole run.

**Debug-a-test binary selection.** `test_binary_candidates` builds candidates (lib before bin before integration, each carrying its target name and `src_path`), narrowed by the gesture's source file. A file that IS a target's `src_path` owns that target outright — exact even for a renamed `[[test]] path` target the file stem cannot predict. A module file narrows to its side of the build: `tests/` to integration harnesses, anything else to lib/bin, never widening. A `--list` probe then matches the listed name's final `::` segment exactly, so a lib test never claims an integration test's bare name, or vice versa. A nonzero cargo exit is a build error even when partial artifacts came out.

**Building off the UI thread.** The app runs `build_test_binary` on a background thread, with `pending_test_debug` drained per tick, so a cold build never freezes the UI. The pending slot remembers its build root, and the drain discards a binary whose workspace was re-rooted mid-build, instead of debugging the old project in the new cwd.

### quick_select.rs

A WezTerm-style quick-select pure core for the terminal pane (Ctrl+Shift+Space).

**Patterns.** A priority-ordered set: markdown and scheme URLs, scp git remotes, email, diff paths, sha256 digests, filesystem paths with optional `:line:col`, UUIDs, IPFS CIDs, git SHAs, IPv4/IPv6, hex colours and addresses, and 4+-digit numbers. Earlier patterns claim their span, so a URL is never shredded into path plus number.

**Matching across soft wraps.** Matching runs over the visible viewport rows stitched into LOGICAL lines by their soft-wrap flags (`find_matches_wrapped`). The pane hands per-row WRAPLINE bits alongside the text, keeping a wrapping row's trailing spaces since they are real mid-line cells. A match crossing the pane edge is seen whole, with its label anchored on the head span and highlight-only tail segments on the continuation rows — never an invisible head plus a mislabelled path-fragment tail.

**Labels.** WezTerm's scheme: a home-row alphabet, single chars until they run out, then the alphabet tail reserved as two-char prefixes. `label_matches` pairs bottom-priority: labels are assigned in reverse so the bottom-most match is cheapest to type, and when matches outnumber labels the TOP overflow is dropped, never the prompt-adjacent bottom. The status line counts the unlabelled.

**Mode ownership.** The app owns the mode (`App::terminal_quick_select`), pane-bound like copy mode: a per-frame invariant in `sync_focus_flags` closes it whenever anything moves the active pane. Typed chars filter the labels, a completed label copies via the clipboard, and UPPERCASE also pastes into the shell. The app pushes `HintSpan`s into the pane at the open-time scroll-clock reading (one `visible_lines_and_clock` snapshot), which the render loop re-bases every frame so labels follow their matches through streaming output. That render loop paints match spans green and overlays black-on-gold label cells through the same char-index colmap the find highlight uses.

### asciicast.rs

Writing a session as an asciicast v2 recording, behind "Session: Record Terminal as Asciicast". A cast is JSON Lines: a header object, then `[time, "o", data]` per output event and `[time, "r", "COLSxROWS"]` per resize. Both traps are in that sentence.

**The data is a JSON string, not bytes.** A terminal stream is full of control characters, so a raw quote, backslash or escape byte written with `format!` produces a file that is not JSON at all, and the player rejects the WHOLE recording rather than the one frame. Every payload goes through `serde_json` for that reason.

**Time must not go backwards.** `SystemTime` can step back across an NTP correction, and a player reading a decreasing timestamp either sleeps forever or panics. A backwards frame is therefore CLAMPED onto the previous instant, which shows as a burst — rather than dropped, which loses output the user saw, or written raw, which breaks playback for everything after it.

**It records the screen, not the byte stream.** The PTY bytes are consumed by the grid before the app sees them. Sampling comes from the dirty-drain, so an idle session records nothing rather than a frame per tick.

**Two sizing traps, both caught in review.** The header and the `r` events take the pane's INNER rect, because `Borders::ALL` costs a cell each side and the PTY is sized from the inner one; recording the outer size declares a terminal two columns too wide and the player re-wraps every long line at the wrong column. And a frame carries only the VISIBLE rows: `grid_lines` starts at `topmost_line()`, which is negative scrollback of up to 5000 rows, so writing all of it after a clear scrolls the live screen off the top. The cast would then show the tail of the history rather than what the user was looking at, at roughly 400 KB per frame.

### pair.rs

The AI pilot core (docs/MULTIPLAYER.md, "croft pair" / "resident navigator"). It spawns the `claude` CLI as a persistent stream-json conversation on stdio and teaches it a fenced protocol via `--append-system-prompt`.

**The protocol.** `<<<EDIT file:R:C-R:C>>>` bodies stream into buffers token by token; `<<<NOTE file:R>>>` bodies anchor commentary to a line. Coordinates are 0-based char coordinates throughout. `FenceMachine` parses the token deltas incrementally, correctly across arbitrary splits, even mid-marker. EDIT headers also take a two-integer whole-line form `<file>:SR-ER` (rows inclusive, no column counting), which is what local models are steered to.

**Applying edits.** Edits apply through a regular collab guest seat at a tracked byte anchor. Both the anchor and every note offset shift via `shift_offset` over incoming RemoteEdit spans.

**Cancellation.** Any participant cancels mid-run via the gutter `■`, Cmd+K X, or the palette, which sends `CollabMsg::StreamCancel`. The pilot interrupts claude (`control_request`, feature-detected), reverts the streamed region, and broadcasts `CollabMsg::StreamState` inactive.

**Comment-only turns.** Yielded turns are comment-only at the host: `PairState::comment_only` discards EDIT fences whatever the model says.

**Sandboxing.** The claude child is sandboxed read-only — Read/Grep/Glob plus a read-only collab-agent MCP seat, `--strict-mcp-config`. The fence is the only write path.

**Seats and drivers.** `seat_pilot` builds the claude seat (child plus reader, stderr and pump threads); `seat_local` builds the childless local-endpoint seat (pair/local.rs). Both return the transport-polymorphic `Pilot`: a shared `TurnSink` for turn injection plus a `Transport` enum for teardown. That serves both drivers, the hidden `croft pair --repl` terminal REPL and the in-process PairHost. `Provider` (claude or local `base_url`) rides `PairConfig` and dispatches the seat. It is e2e-tested against a scripted python3 fake claude.

### hex.rs

The hex viewer and editor core. It is the routing fallback for every file the text heuristic rejects, plus the explicit "File: Reopen as Hex", replacing the old "Binary file" error.

**Pure state plus windowed file IO.** A 256 KiB window around the viewport is read on demand, so a multi-GB file opens instantly and is never loaded whole. Find streams in 1 MiB chunks with needle-length overlap, wraps at EOF, and is capped at 64 MB per invocation with the cap reported honestly.

**Rendering and frame truth.** The editor widget paints it (`render_hex`: offset gutter, hex grid with an 8-byte group gap, ASCII gutter, cursor and selection, status row) and writes the frame's layout and chosen bytes-per-row — 16, or 8 in narrow panes — back into the view for navigation and mouse hit-testing.

**FS-sync reload.** It refreshes the view in place with a new length and the same reader offset, following an atomic-write rename to a new inode.

**Save is refused for previews.** Save is structurally refused for every preview kind at the `write_buffer_to_disk` choke point, because the placeholder text buffer used to truncate the previewed file.

**Editing.** Overwrite-mode only. A sparse offset→byte map overlays the window (`effective_byte`). Bytes are typed as hex nibbles (high first) in the grid, or as characters in the ASCII gutter; Tab switches, and the cursor's mirror in the other pane stays visible. Undo/redo is an offset-plus-prior-value stack, with an amber tint on pending cells. `hex_save` writes just the changed bytes IN PLACE — an atomic rename would copy gigabytes for a two-byte fix — through its own path, never the text choke point, with the text tabs' disk-conflict double-press contract. Pending edits survive a same-path re-open; Revert discards them explicitly (`discard_edits`) before the reload; and read-only files refuse typing up front.

### symbol_range.rs

Keeps a symbol view pointed at its symbol while the file changes. A symbol tab is a view over a byte range, not a copy — that is what lets edits, LSP, diagnostics, undo and collab all work through one pipeline instead of two. The cost is that every edit can move the range.

**Three edit cases, and the one that matters.** An edit above the range shifts the whole range. An edit below it moves nothing. An edit inside it must grow or shrink the range rather than shift it. The third case is the one a naive implementation gets wrong: a symbol tab exists to be typed in, so shifting on an inside edit slides the range off the end of the very function it is showing.

**Two boundary decisions.** A deletion ending exactly at `start` counts as above, because its text was not the symbol's. A zero-width insertion at `start` counts as inside: the typed bytes land at the symbol's first position, and treating it as above would push the tab down and off as you type at the top of the function.

**Straddling an edge reports `Gone`.** An edit that straddles either boundary does not re-anchor. The surviving range would be half a function glued to whatever followed it, so closing the tab with a notice is the honest answer.

**`enclosing_symbol` picks by narrowest span.** It takes the innermost match by narrowest span, not by depth, so opening from inside a method shows the method rather than its `impl`. On a tie the last one wins, matching `OutlinePanel::follow_caret`. Symbols arrive parents-before-children, and two pickers disagreeing about "innermost" would highlight one symbol in the Outline while opening another as a tab.

**The tab carries its file as well as its range.** An offset means nothing in another buffer. Without the file, a collaborator editing `other.rs` would move a tab pointed at `main.rs`.

### osk.rs

On-screen keyboard for Termux, needed because mouse tracking blocks the native soft keyboard. It is a bottom-docked tappable band whose keys synthesize KeyEvents through `handle_key`.

**Layers.** Lower, shift, and two symbol pages: a Gboard `?123` page of digits and punctuation, and a `=\<` more-symbols page holding tilde, backtick, pipe, the bracket family, and math, typographic and currency glyphs. The row-3 lead key switches between them, in the position where Shift sits on the letter layers. There is also caps lock (letters only) and one-shot ctrl and alt latches on the bottom row next to space.

**Physical-keyboard geometry.** Structural keys carry max cell widths. The left column staggers `esc` < `tab` < `caps` < `shift` like a MacBook. Letters and space absorb wide-frame slack by water-filling. Enter grows into a two-row L on the right in both merged and split layouts, full-height even on one-row bands, and the collapse `⌄` key is about twice the width of the `split` key.

**Gboard-style split layout for foldables.** The number and top rows split 5|5; the home and bottom rows split one column earlier so `g` and `b` fall in the right thumb cluster. The top-right cluster parks a `\` at the far edge (`y u i o p \`), and qwerty letters keep their natural order. The halves are solved independently around a center gap of 2·width/9, space appears on both halves, and the layout merges again under 60 cols. The `split` key toggles it and it persists as `osk_split` in prefs.

**Sizing and theming.** Thumb-sized keys scale with frame height, and the non-focused pane folds away while the band is up. Key caps are drawn from the active theme's palette — brand-teal armed accent on Black, the historical navy on Dark.

**Dictation.** A tap-to-dictate mic key sits immediately right of the left alt, eating into the space bar (and into the left space half when split). It drives `voice.rs`.

### scrubber.rs

Moves through a branch's history. This is the cursor behind "Source Control: Scrub History": arrows step between commits, Home returns to the working tree.

**Leaving must restore the live buffer exactly, unsaved edits included.** Scrubbing is a way of looking, not of editing, and losing uncommitted work to answer a question about history would be worse than not having the feature. So the working tree is a *position* in the cursor rather than something the scrubber replaces — there is no state where the live buffer has been discarded and the scrubber owes it back.

**`Position::Working` is a variant, not `Option::None`.** An `Option` over a commit index invites "nothing selected" and "back at the working tree" to be the same value, and the second is where the user started.

**The key hook sits below every modal guard in `handle_key_inner`.** A palette, prompt or picker opened while scrubbing keeps its own arrows. This holds by construction: each of those returns unconditionally, so a key only reaches the scrubber when none of them wanted it.

**Fed by `git::branch_history` (`--first-parent HEAD`), not `commit_graph`.** The commit graph's `--branches --tags` ref set exists to draw the repo-wide graph. With any other branch present, index 0 there is whatever topological order put first rather than HEAD.

**Two boundary decisions.** Stepping back from the tree lands on HEAD rather than skipping it — they are different views, since one has your unsaved edits. And the oldest commit is a wall rather than a wrap, because a drag that reappeared at the present would read as the slider slipping.

### pair_host.rs

The resident navigator's in-process host. The App spawns the pilot itself, the way it hosts LSP servers, when the workspace's `<hash>.pair.json` activation record (written by `croft pair`, `session::pair_record_path`) is enabled. On a plain launch it self-appoints the collab OWNER seat first: only an owner answers the pilot's snapshot requests, and solo guests never host.

**The pilot's voice arrives as `PairEvent`s.** They are drained once per tick by `App::poll_pair`, covering anchored notes (gutter `◆` diamonds plus the anchored note popup, F4 cycles, Esc dismisses), commentary on the Navigator OUTPUT channel, turn ends, and death. On death the seat is unseated with no auto-respawn, following the LSP precedent.

**Two kinds of turn.** Ask turns (`Cmd+K Q`, gutter and selection menus) may edit. Yield turns (`Cmd+K Y`) carry the diff since the navigator's last look and are comment-only.

**A persistent visible caret.** The seat keeps one in the navigator's identity orange (`NAVIGATOR_ACCENT`, the comment-box accent). It parks at the ask/reply row and at every landed note (`PairState::park_caret`; pending parks are resolved by the pump after bootstrap and superseded by any newer explicit caret), and streams along during edits. It is identified by the seat's per-file site ids (`caret_sites`, never the display name), both for coloring and for the `collab_carets` cleanup on every unseat path.

**Teardown.** Dropping the host reverts, hangs up, and grace-kills the claude child. A local seat has no child: its turn queue closes and the worker drains out.

**Backend.** It comes from the record's provider — claude by default, `ollama` meaning pair/local.rs — and the idle `◆` badge names the local model via `title()`.

### marketplace.rs

Fetches a theme from a marketplace. It resolves a `publisher.name` id, a marketplace or Open VSX URL, or a `vscode:extension/...` link into an `ExtensionId`, downloads the `.vsix`, and lifts one member out of it for `vscode_theme.rs` to convert.

**A `.vsix` is treated as a hostile container.** It is a zip that may carry JavaScript, native binaries, and a manifest telling an editor to run them. So no extension code is fetched to be run or kept, no `package.json` field but `contributes.themes` is read, nothing is installed or executed, and the scratch directory goes whether the conversion succeeded or not.

**Drawing the fetch boundary.**

- The URL is rebuilt from the parsed id rather than echoing the input, so nothing attacker-shaped reaches the fetch even if host parsing were loose.
- Redirects are followed one hop at a time, with the host re-checked at each hop against a small fetch allowlist. ureq re-applies no host policy across a hop, so following them blindly would carry the fetch off the boundary. Refusing them outright is not the answer either: Open VSX 302s its downloads to its CDN, so every Open VSX theme would fail.
- The URL Open VSX names is re-checked against that same allowlist.
- The theme LABEL is sanitised before it names a file. The label is the one field arriving from inside the archive that no charset rule had touched, and `Path::join` with an absolute one discards the base entirely.

**Both galleries are real destinations, not one with a spare alias.** The themes bundled with VS Code answer 404 on the Microsoft gallery and 200 on Open VSX.

### testing/

Test Runner. A background worker runs the project's test tool off the render loop and streams parsed cases into the Testing panel, mirroring the drain-per-tick pattern of app/git_worker and dap.

**Runner selection.** `cargo test` for Rust, `pytest` for Python, `vitest` or `jest` for JS/TS, picked per workspace by `registry::runner_for` from `[[test_runners]]` manifest data. When no enabled runner claims the root, the four run entry points refuse with a status, and the worker refuses a queued request with a dedicated `Refused` response *before* any `Started`. The panel rolls the Running marks back and keeps its tree. Neither layer ever falls through to cargo, so a disabled runner extension really does stop runs.

**Re-rooting.** An Explorer re-root rebinds the worker, resets the panel, and re-discovers. So does focusing a file in a different workspace folder under multi-root (`active_test_root`, the same focus-follow as the SCM surfaces; tasks, run-active-file, and the DAP launch cwd/venv ceiling resolve per owning folder too). Responses are epoch-tagged, so a run still streaming for the old root is dropped by the drain — the commit graph's root-tag idea.

**Debug-a-test.** `Cmd+K Shift+Enter`, or Alt+click on the gutter `▷`, hands a test to the DAP stack. pytest goes as a debugpy `module` launch under the project's interpreter (`pytest_debug_launch_request`; the adapter injects debugpy into the debuggee). cargo goes via `worker::build_test_binary` (`cargo test --no-run --message-format=json`, with the bin/lib artifact outranking integration targets), launched under lldb-dap with the test name as its libtest filter argv (`lldb_test_launch_request`).

### commit_graph.rs

The Source Control COMMITS section: a repo-wide commit graph with box-drawing lane rails — VS Code's Source Control Graph in the tig idiom.

**`layout_graph` is the pure classic lane algorithm.** Each lane holds the hash it expects next. A commit lands on the first lane expecting it; duplicate expectations close into the node (`╯`/`╰`); a merge's extra parents open lanes (`╮`/`╭`) or junction into a lane already expecting that parent (`┤`/`├`); pass-through rails cross long connectors as `┼`. It runs on the fetch thread over `git::commit_graph` (`git log --branches --tags HEAD --topo-order`, 400 commits).

**Row painting and interaction.** Rows paint rails colour-cycled by lane, ref badges (HEAD bold accent, tags gold), the subject, and a right-aligned age. Clicking a row opens the commit's full patch (`git::show_commit`) in a read-only tab.

**Refetching.** It refetches whenever the git worker sees `head_oid` move (commit, checkout, pull). A refresh triggered while one is inflight is queued to re-fire when the stale reply drains, so a Make Root racing a fetch never strands the panel on Loading. The section collapses from its header and keeps scroll across same-tip background refreshes.

**Section hit rects are zeroed every frame.** This panel's rects, and the Explorer's OPEN EDITORS / TIMELINE / DEPENDENCIES / OUTLINE rects, are zeroed at the top of every `App::render` and re-recorded by whichever sections actually paint. Otherwise a section that stops rendering keeps its stale rect, and `handle_mouse`'s swallow-on-containment checks ate every click the new view painted in those cells: the Explorer's bottom sections shadowed this graph's rows after a view switch, killing commit clicks.

### markdown.rs

Rendered Markdown preview (`Cmd/Ctrl+Shift+V`): a pulldown-cmark event stream turned into styled ratatui lines.

**Pure module, stateful editor.** The module is pure; the editor holds the `MarkdownPreview` state and paints it through a wrapping Paragraph, so text reflows with the pane. It is rebuilt lazily when `built_seq` falls behind the buffer's edit seq, and rebuilt eagerly by `rehighlight_for_theme` on a theme switch, since the lines bake the theme's colors in.

**What it renders.** Headings (H1/H2 accent bold), emphasis and strikethrough, inline code, links (underlined accent), nested, ordered and task lists, blockquote bars, aligned tables with a header rule, horizontal rules, and fenced code blocks highlighted by the same tree-sitter registry the editor uses (`lang_for_fence` maps info strings).

**Local images render inline.** The build resolves non-URL paths against the file's directory and reserves aspect-derived blank rows per picture, using trim-safe anchors. The render publishes each anchor's wrapped visual row plus the paragraph area as frame truth. The app's overlay bakes the topmost visible picture once per `(path, mtime, fitted cells)` into a dedicated image-overlay slot and re-emits it per frame. On Kitty that slot is its own image id at the below-text z; iTerm2 and sixel paint cells directly. It hides under menus and popups like every image overlay.

**Remote URLs and missing files keep the labelled placeholder** — the preview never fetches.

**Chord forwarding.** `setup-iterm2` forwards the chord (and also relocates iTerm2's "Paste Selection" menu item off `Cmd+Shift+V`), as does `setup-ghostty`.

### testing.rs

The Testing sidebar widget: the test suite tree, grouped by module or pytest file, with live pass/fail/skip status glyphs.

**Summary line.** It reflects the Activity — Discovering, Running, or a tally — with a leading `run failed` marker when the runner exited nonzero without streaming a Failed case. A compile error reports no cases, and a stale failure from an earlier run cannot mask the marker. The failing-test count here feeds the beaker activity badge.

**A failure during a run is never downgraded by a later pass for the same name.** Two harnesses in one `cargo test --no-fail-fast` can report the same test path, and stdout carries no harness identity to separate them (cargo's "Running unittests …" banner is on stderr, read by another thread). The row they share is honest only when it shows the worst outcome. The `Running` run-start mark is exempt, since `start_single` paints it through the same call and a fixed test must be able to go green on a re-run.

**Rollback on finish.** A finished run rolls back marked cases it never reported, the same as a worker refusal, so a compile error cannot strand Running dots.

**Run at cursor** resolves a leaf that is unique in the tree to its full name, for an exact run.

**Clipping.** Rows clip by display width (`set_stringn`), so CJK names stop at the scrollbar lane.

**Controls.** The view discovers on first open, Enter runs all tests, and `r` re-discovers. Each row's play/status glyph is the click-to-run button, while clicking a case's name reveals its source (`hit_at` maps the click).

### triggers.rs

iTerm2-style terminal triggers read from `~/.config/croft/triggers.json` — tolerant JSONC like `keybindings.json`, opened from the palette via "Preferences: Open Terminal Triggers (JSON)", and reloaded on save. Each rule is a regex plus an action.

**`highlight` is painted at render time** over the visible rows, with per-rule `#rrggbb` fg/bg drawn under find, quick-select, cursor and selection so those win. It is free when idle and persists into scrollback.

**`notify` and `bell` run in the PTY reader thread**, as a third sniffer beside `PortSniffer` and `OscSniffer`. `TriggerScanner` strips escape sequences with a CSI/OSC/DCS state machine, buffers the current line capped at `LINE_CAP`, treats a lone `\r` as an in-place rewrite so progress bars never spam, but treats `\r\n` as the PTY line ending. It matches once per completed line.

**Alt-screen tracking is positional.** It tracks entry and exit by DECSET/DECRST 47/1047/1049 and RIS in the stream itself, so alt-screen content never fires while primary text sharing a chunk with a transition still does. The reader feeds it every chunk unconditionally — otherwise its state machine would desync and splice phantom lines across skipped gaps.

**Hit delivery.** Hits flow over an mpsc channel drained by the app tick into the status bar, with `\0`-`\9` capture interpolation. `capture` hits carry the whole matched line and land in the CAPTURES panel instead — see `widgets/captures.rs`.

**Distribution to panes.** Panes get the set via `set_triggers`, a ptr-eq no-op re-pushed every drain tick, so panes are covered wherever they were created; the inner Arc swaps on config reload.

### terminal_session.rs

Terminal session restore. The panel's layout — pane order, per-pane cwd and manual name, active index — persists per workspace root in `~/.config/croft/terminal_sessions.json`.

**When it saves.** On structural changes (split, close, undo-close, rename, reorder), at quit, and on a workspace re-root.

**Re-root behavior.** A re-root files the outgoing root's layout under *its* key before the switch. It then restores the incoming root's saved layout the way startup does, but only when the panel is still the untouched startup default: one unnamed shell, no input ever written, no launched program (`PtyTerminal::is_pristine`). A panel with any user state survives the re-root untouched, since navigation must never destroy live shells.

**Restored panes come back at startup as fresh shells in their directories** — running programs are not re-run.

**Pruning the default.** The default single-unnamed-pane-at-root layout prunes its key. Directory equality goes through `canonicalize`, so macOS's `/private` symlink never defeats the prune.

**Testability.** `App::terminal_session_path` is a field so tests can redirect it.

**An unreadable store refuses updates rather than being treated as empty.** Treating an existing but unreadable or unparseable store as empty wiped every other workspace's layout. A missing file, by contrast, is the normal first run.

**Save safety.** Saves run under `workspace::update_json_store`'s exclusive cross-process lock with unique-temp-file renames, and a failure is latched into the next structural status (`terminal_status`).

### pdf.rs

PDF rasteriser. It prefers `pdftoppm` (poppler) and falls back to macOS `sips`.

**Link extraction.** `page_links` shells out to `pdftohtml -xml` and parses the result with `parse_pdf2xml_links`. A click on the preview maps its cell back through the `fit_image_auto` placement (`iterm2_inline::cell_source_fraction`) to page fractions, then hit-tests the current page's cached link rects.

**Per-anchor geometry is approximated.** Each `<a>` gets the proportional horizontal slice of its text run's rect, because pdf2xml carries no per-anchor geometry and poppler often wraps only part of a run. Among several overlapping hits — adjacent TOC lines overlap outright — the link covering most of the cell wins.

**Where links go.** External URLs open with the OS opener behind an http/https/mailto allowlist. A document-supplied `file://` or custom scheme is a one-click app launch and is dropped. Internal GoTo targets flip the page.

**Extraction is lazy.** Text-anchored links only, extracted on first click and re-extracted after a page flip.

**FS-sync reload keeps your place in one rasterisation.** When pdflatex rebuilds the open file, the reader comes up on its current page in a single render: `open_pdf` consumes the editor's `pdf_restore_page` request, clamped to the fresh page count. Restoring the page with a *second* render after the open left a window where that render's transient failure silently snapped the reader to page 1. Now such a failure fails the whole open, and the failed-reload path keeps both the last good render and the place.

### problems.rs

The panel group's PROBLEMS tab: aggregated workspace diagnostics projected from the App's per-file store.

**Toolbar.** A severity filter cycling All / Errors / Errors & Warnings; a free-text filter typed while the tab is focused that substring-matches message or path and composes with the severity cycle; and a group-by-file toggle between collapsible per-file headers and a flat file-annotated list. Click a diagnostic to jump, a header to collapse. The filter drops files with no matching diagnostic, but the tab badge counts stay unfiltered.

**Build problem matchers.** Every finished command's output — captured escape-free at the `133;D` mark into `FinishedCommand::output`, with a clock-rebased span from the C mark's row and a tail cap of 5000 lines — runs through `build_matchers::scan`. That covers rustc two-line pairs, tsc (both shapes), gcc/clang generic, python tracebacks (deepest frame), and eslint stylish. The hits land beside the LSP entries under their tool's source badge, with paths resolved lexically against the command's cwd.

**Replacement, not accumulation.** A pane's next scanned command replaces its previous contribution, so a clean rebuild clears its errors. "Problems: Clear Build Diagnostics" wipes them all.

**User-defined matchers** (`problem_matchers.rs`) extend the same boundary scan, and background matchers publish mid-run — see that module's entry.

### diff.rs

Side-by-side file diff renderer, used by the explorer's Compare action. It also derives hunk ranges from the row model and builds the single-hunk unified patches behind the diff pane's S / U / R stage / unstage / revert keys.

**Staging is gated to the Source Control HEAD diff** via `left_is_git_head` — a Timeline snapshot diff never reaches the index. Patches restore each line's CRLF bytes and git's no-newline marker. The action row follows the newest click or hunk jump, never a stale caret.

**Live refresh.** Every `DiffData` records a `DiffSource` plus `(mtime, len)` stamps of its file-backed sides. The sources are `HeadVsWorking {root, rel}` for the Source Control view, `FixedLeft` for a Timeline snapshot, `TwoFiles` for Compare, `GitCommand` for the staged, branch and previous-commit views, and `Static` for a deletion tombstone.

**The sweep.** `App::refresh_open_diff_views` walks every split group's diff tabs and rebuilds through `rebuild_diff_view`: from disk when a side's stamp moved (called from `drain_fs_events` and the poll), and from HEAD or a re-run `git diff` when the git worker's status lands (called from `drain_git_responses`, so the cadence is the worker's debounce).

**Reader state survives.** `carry_view_from` keeps the reader's whitespace mode, scroll, pan, hunk anchor and find. Identical content is left untouched, so selection and find survive a no-op write.

### manager.rs

LSP lifecycle: pre-warm, spawn, `did_open`, `did_change`, `did_save`, completion, `documentSymbol`, inlay hints, `documentHighlight`, call hierarchy, and shutdown.

**Workspace pre-warm at startup** (`prewarm_workspace`) follows VS Code's `workspaceContains` activation: every language whose root marker sits in the workspace root gets its servers spawned before any file opens, so rust-analyzer's cold indexing overlaps with tree browsing instead of starting at the first `.rs` open. The scan is non-recursive stat calls, and the command is inert under `cfg(test)` so test-built Apps never spawn real servers.

**`did_save`.** `Cmd+S` makes rust-analyzer re-run check-on-save, so PROBLEMS refreshes.

**Pull diagnostics (LSP 3.17 `workspace/diagnostic`).** Servers that advertise `diagnosticProvider.workspaceDiagnostics` (taplo does; vtsls does not, which is why the `tsc --noEmit` fallback exists) are asked for the whole project's problems over the existing connection, once as they spawn and again whenever one sends `workspace/diagnostic/refresh`. Three things make it work rather than merely happen:

- **croft has to ask to be told.** A conforming server publishes `diagnosticProvider` only when the client declares `textDocument.diagnostic`, and sends the refresh only when it declares `workspace.diagnostics.refreshSupport` - plural, the one capability the spec pluralises, and the one `lsp-types` 0.95 emits in the singular. croft declares BOTH names, since a server reading the spec name would otherwise never see it and never send a refresh. Undeclared, the capability reads as "no server supports this" rather than as a missing declaration.
- **`previousResultIds` are what make it cheap.** Each server's last ids go back with the next pull, and files that have not moved come back as `Unchanged` and are not re-processed. A `Full` report with no id *forgets* the file: sending back an id the server never issued for the current content invites an `Unchanged` answer for a file that did change.
- **The reports rejoin the push path.** They land on the same channel under the same server name, so the per-file, per-server store already replaces a server's set wholesale and `rebuild_problems` already walks files no editor has open. An empty report is the server clearing a fixed file, so it is forwarded rather than skipped.

**Inlay hints.** A whole-document `textDocument/inlayHint` once per edit-batch, capability-gated, with labels normalised (parts joined, padding folded to spaces). The reply is seq-tagged so the app drops a batch computed against older text.

**`documentHighlight`.** The occurrences of the symbol under a resting caret, fired by the app's 250ms idle tick. It is request-id and edit-seq gated, so a reply for a moved caret or edited text never paints.

**Call hierarchy.** `prepareCallHierarchy` plus `incomingCalls`/`outgoingCalls`, one level per `Cmd+K H` / `Cmd+K Shift+H` invocation into the shared location picker. Incoming rows jump to the call expression, outgoing to the callee's definition.

### problem_matchers.rs

User-defined problem matchers, read from `~/.config/croft/matchers.json` and workspace `.croft/matchers.json` (palette "Preferences: Open Problem Matchers (JSON)", reloaded on save). They compile into `CompiledMatcher`s: single or multi-line regex sequences with named groups (file/line/col/severity/message/code, VS Code-style `loop` tails), `applies_to` command globs, and `severity_map`.

**tasks.json `problemMatcher` values.** `$tsc`, `$tsc-watch`, `$rustc`, `$eslint-stylish` and `$gcc` map onto the built-in table narrowed by source tag. Inline VS Code pattern objects with numeric indices translate. Unknown `$names` degrade to the built-in scan. These are assigned per task pane on run and are exclusive for that pane's batch scans.

**Background matchers (`begins`/`ends`) run on the live stream.** `WatchEngine` consumes the completed lines the `TriggerScanner` already produces (`scan_collect` — no second byte scanner), collects the window with a cap, and on `ends` publishes the scanned batch over an mpsc the app drains. That replaces the pane's PROBLEMS batch every cycle, with the watcher's own exit-rescan skipped once so old cycles never resurrect.

**Bad regexes drop at load.** A regex compile error drops the entry with a warning in OUTPUT · Matchers, so the stream path only ever sees compiled regexes.

### vscode_theme.rs

VS Code colour theme import: converts a theme `.json` (JSONC, plus `include` chains, base first) into a croft `[[themes]]` manifest under `~/.config/croft/extensions/`, behind `croft theme-import`. It reads a theme file; `marketplace.rs` next door is what fetches one.

**The mapping is real work, not a rename.** VS Code names hundreds of workbench keys and colours code by TextMate scope, while a croft theme is a small fixed palette.

- **Workbench keys map by a documented priority list per slot.** `activityBarBadge.background` leads the accent, because most themes set `focusBorder` to a muted grey.
- **Translucent colours composite over the resolved background** rather than dropping their alpha.
- **The `terminal.ansi*` palette is taken all-16-or-none**, because a half-themed terminal reads as a rendering bug.
- **Each syntax role takes the first TextMate scope a theme defines for it, matching downward only.** A rule for `entity.name.function.decorator` colours decorators and must not answer for functions in general — which is what turned One Dark Pro's calls yellow before it was fixed.

**Derived slots are reported.** Every croft slot the source theme never named is derived and reported, so an imported theme's invented colours are visible rather than silent.

### file_tree.rs

An `ignore::WalkBuilder`-backed tree with lazy children, fs-watcher refresh, multi-select, drag-drop, bulk trash, and reveal-path on `Cmd+P` open.

**Multi-root ready.** `add_root` appends further workspace roots as depth-0 section rows (`root_paths` enumerates them; `self.root` stays the primary). Every root-special behavior keys on the row's *depth*, not index 0: Collapse All spares all root rows, the delete guard refuses them, and `reveal_path` plus the ignored-set ancestor walk resolve through the owning root (longest prefix, so nested roots pick the deeper one). Sticky ancestors were depth-based already.

**Sticky scroll** (VS Code 1.86's tree sticky). Scrolled deep, the off-screen ancestor chain of the top visible row pins to the top band. `sticky_ancestors` walks nearest-smaller-depth backwards, with a cap of 3 that keeps the deepest levels. The band overpaints content rows but never the selected row: the scroll clamp keeps selection on screen, so its band-relative row bounds the band.

**Mouse handling against the band.** Hit rows are recorded per render as frame truth. A left-click on a pinned row selects it and scrolls it to the top; a right-click context-menus the pinned dir instead of the covered row. Both mouse arms resolve the band *before* `node_at_y`.

### shell_integration.rs

`OSC 133` / `7` / `9` shell integration. The module holds `OscSniffer`, an incremental scanner that the PTY reader thread tees raw bytes through, plus the per-shell shim writers that make the shell emit those marks.

**Why croft sniffs the bytes itself.** alacritty drops unknown OSC before any handler sees it, so croft splits its `Processor::advance` at each mark and samples the cursor exactly where it landed.

**zsh.** `ensure_zsh_shim` writes a `ZDOTDIR` shim that sources the user's real dotfiles, then installs precmd/preexec hooks emitting prompt marks and cwd. `CROFT_SHELL_INTEGRATION=0` opts out. An inherited `ZDOTDIR` pointing at croft's own shim falls back to HOME. One pointing at another terminal's injection shim — Ghostty or kitty launching croft as their command — is chained through instead: after sourcing that shim's `.zshenv`, whatever `ZDOTDIR` it restored is taken as the user's true dotfile dir.

**bash.** `ensure_bash_shim` uses the `$ENV` + `--posix` bootstrap kitty invented and Ghostty copied. Posix-mode bash reads only `$ENV`, so the shim leaves posix mode, replays the login/non-login startup files bash itself would have picked, then wires precmd via `PROMPT_COMMAND` — array-aware on 5.1+, exit capture first, arm last — and preexec via funsub `PS0` on 5.3+ or a bash-preexec-style `DEBUG` trap below that. Where bash-preexec is present, it joins that project's hook arrays. A version probe gates the whole thing on bash >= 4.4, because macOS's system 3.2 ignores `$ENV` in posix mode and would lose its startup files.

**fish.** `ensure_fish_integration` injects a `vendor_conf.d` script by prepending `XDG_DATA_DIRS`. The script restores `XDG_DATA_DIRS` first so children never see it, then defers to fish 4's native `OSC 133`/`7` emission — kitty's `forward-char-passive` detection. It installs event hooks only on old fish, since a second emitter double-decorates.

**What the marks feed.** They power Cmd+Opt+Up/Down command navigation and the command decorations. `OSC 7` feeds split-pane cwd inheritance and carries the reporting host (`Cwd(path, Option<host>)`) — an in-pane SSH session with integration moves it to the remote hostname, feeding the per-host accents. `OSC 9` surfaces in the status bar.

**Progress reports.** The `9;4;state;percent` sub-namespace (ConEmu progress: 0 clear / 1 normal / 2 error / 3 indeterminate / 4 warning, percent clamped) parses to `OscEvent::Progress` rather than a notification, and drives the pane's bottom-border gauge — held per pane, cleared on state 0 or the command's `133;D`.

### source_control.rs

The Source Control sidebar widget, including the multi-root REPOSITORIES overview.

**The repositories overview.** One row per workspace folder that is a repo, each with a disambiguated label, its branch, and a porcelain change count from `GitStatus.changed_count`. The active row is highlighted, and the overview paints in the no-repo state too so other folders' repos stay reachable. Clicking a row PINS the panel via `App::scm_pin`, released when the focus-derived folder changes. The activity badge sums `changed_count` across all workers.
### agents.rs

Tracks agent lanes: which panes are running a coding agent, and whether that agent is working, waiting for the user, or idle. `claude`, `codex`, `aider` and `gemini` are built in, and `~/.config/croft/agents.json` extends or replaces those rows and is reloaded on save. A row's `launch` is the command a worktree lane starts the agent with.

**How the state is judged.** The status comes from the pane's last-output stamp and its last visible rows, matched against the agent's prompt patterns. An agent in the foreground never emits OSC 133 marks, so there is no shell-integration signal to read instead.

**What the UI shows.** The pill wears `◆ claude ◐`, and the status bar counts seated agents with a click that reveals the waiting pane. `AgentEvent::{Seated, Working, Waiting, Gone}` are queued for the tick, with `Waiting` fired once per prompt.

### ansi_text.rs

An SGR span parser for colour-bearing text. One pass turns a raw line into visible text plus styled spans, covering 16 / 256 / truecolor, bold, dim, italic, underline and inverse.

**Why non-SGR escapes are dropped.** Cursor movement, OSC and DCS sequences are discarded because a log file is a stream transcript, not a screen.

**Two entry points, one allocation.** `parse_into` writes into a caller-owned line so a bulk scan reuses one allocation instead of building a String and a span Vec per line. `parse_line` wraps it for one-shot callers.

**One implementation on purpose.** There is a single copy of the stripping rules. A second "just strip the escapes" implementation would drift from this one, and everything that trusts the stripped text — find, copy, `path:line` scanning — must see the same characters the renderer painted.

### archive.rs

The archive browser core. It lists members without reading payloads: zip/jar/whl through the central directory, tar and tar.gz through a header walk.

**Why the size gate comes before the parse.** Each branch is bounded by FILE SIZE before parsing. Tar is capped at `TAR_LIST_CAP`, with an additional decoded-byte cap while a `.tar.gz` streams. Zip is capped at `ZIP_LIST_CAP`, because a zip's members cost ~87 bytes each, so a file under the editor's 50MB cap can declare 400k of them and take 1.65s to list on the frame loop. `ZipArchive::new` parses the whole central directory before any count exists, so the gate has to precede it.

**Extraction is lexically contained.** `extract_member` writes ONE member into a destination under a strictly LEXICAL containment check: absolute paths, drive prefixes and `..`-traversal are refused before anything is written, and zip-slip is tested. Extraction is capped at 100MB.

**How it reaches the editor.** The editor holds the read-only browser tab with selection nav and click-select/click-open. Enter extracts to the session scratch dir and reopens the copy through the standard open dispatch, so every member renders with its real viewer. Sniffed zip, gzip and tar containers route here before the hex fallback.

### captures.rs

The panel group's CAPTURES tab, modelled on iTerm2's Capture Output. It holds output lines collected by `capture` triggers — the pane label, the interpolated message, and the whole escape-stripped line — with the newest selected. It is a pure widget, capped at 500 entries.

**Jumping back to the line.** Enter or a click has the App switch to TERMINAL, find the pane by shell pid, locate the newest grid row matching the captured text, scroll it into view and select it. The match is a prefix match, since long lines wrap.

**Asking Navigator about a line.** It also shapes the "Ask Navigator about this line" turn: `context_window` gives the rows around the hit, `first_file_ref` gives the leftmost `path:line` in the line, and `ask_prompt` masks the line, the trigger message and the context through the caller's redaction, capping the excerpt at `ASK_CONTEXT_CHARS`.

### catalog.rs

The curated MCP catalog, the AVAILABLE tier of the Extensions panel. Bundled `CATALOG_MANIFESTS` (Web Fetch, Time, MarkItDown and csvlens) are merged with the remote signed index. csvlens is a `[[viewers]]` entry: a terminal program run on a file in its own pane rather than an MCP sidecar.

**Install and uninstall paths.** `install()` writes a bundled entry's manifest, or delegates a remote id to `registry_index::install_remote` (Available → Add → Installed). `uninstall_in()` removes it (Installed → Available).

**The provenance gate.** `is_removable()` is `is_catalog_entry() ‖ registry_index::is_index_entry()`, so croft removes only what it added — bundled or vetted-index — and never a hand-dropped user manifest.

**Destructive actions need confirmation.** Add goes through the panel's +Add. Remove goes through the trash button or Delete, and both open a confirmation popup (`InputPurpose::ExtensionUninstall`, Enter uninstalls, Esc keeps), so a destructive action never fires from a single click or keypress.

### lsp/client.rs

The async-lsp client wrapper. Its router forwards diagnostics and work-done progress (`$/progress`, for example rust-analyzer's "Indexing…") to the status bar, and acknowledges workspace refresh requests (`semanticTokens/refresh`, `inlayHint/refresh`, `diagnostic/refresh`) into shared re-pull flags the app polls.

**Workspace folders declared and answered from one list.** It declares the `workspace.workspaceFolders` capability, because without it a server may legally ignore the folders array in `initialize`. It answers the LSP 3.6 server→client `workspace/workspaceFolders` request with the same folder list `initialize` carried — one list serves both, so they can never disagree. `didChangeWorkspaceFolders` waits for a multi-root add/remove-folder lifecycle.

**Server stderr keeps its severity.** stderr lines land in OUTPUT at the severity of their tracing level token (`stderr_level`), not a blanket info.

### command_history.rs

Durable cross-session shell command history, the atuin model embedded in the editor. Every command a shell-integrated pane finishes is appended as one JSONL line under `~/.config/croft/command_history.jsonl` with cwd, exit, duration and timestamp.

**No shell-side hooks needed.** The reader thread extracts the typed text from the OSC 133 B→C mark span at the `133;D` mark (`FinishedCommand`), so the record needs nothing installed on the shell side.

**Search and compaction.** `search` is newest-first, case-insensitive substring, deduped to the newest run per command text, with scopes all / this-directory / failed-only. The file compacts back to the newest 10k entries once it doubles past the cap.

**Using it.** Ctrl+Shift+H opens the popup (`widgets/history_popup.rs`); Enter types the pick at the prompt without running it.

### extensions.rs

The Extensions sidebar widget. It projects the bundled and user extension manifests (`lsp/manifest.rs` summaries) into a theme-aware list, grouped under BUILT-IN, INSTALLED and AVAILABLE headers.

**Row anatomy and affordances.** Each row is a coloured language/file mark, a name, a blurb, and a rounded pill toggle (Powerline caps plus knob, brand-teal on, grey off). AVAILABLE rows get a +Add affordance; removable INSTALLED rows get a trash button, just left of the switch. A local filter box narrows by name, blurb or id — there is no marketplace — with a clear (✕) and a refresh (⟳) affordance.

**The widget reports, App owns the state.** Row, switch, trash, clear and refresh clicks are reported back to App through `click_uninstall` / `click_action` / `click_clear` / `click_refresh` / `item_at`. App owns the disabled-set prefs, the catalog install/uninstall, the remote-index refresh, and the per-control hover tooltips, which fire only over a control and never over the row body.

### hover_popup.rs

An anchored popup with a 300 ms dwell. It serves the LSP hover (wide), the tab-path tooltip, and `new_compact` button hints (shrink-to-fit) for chrome controls, all driven off one `ui_tooltip_at` dispatch in `app/mod.rs`.

**Peek Definition.** It also renders Peek Definition (Alt+F12 or the palette). The same LSP definition request, tagged with `peek_definition_request_id`, opens a caret-anchored excerpt popup: a `status_path(path):line` header, a `peek_excerpt` window (4 above, 12 total, clamped), and the definition row marked `▶`. Text comes from the buffer when the target file is the open one, so unsaved edits are visible, and from disk otherwise. Enter converts to the real jump, Esc closes, and any other key closes and keeps its meaning.

**F12 no longer tolerates a stray ALT bit.** Bare F12 now rejects it, so that fold-noise lands on the strictly-less-disruptive peek instead.

### http_file.rs

Support for `.http` and `.rest` request files. The REST-Client format is parsed — `###` blocks, headers, bodies, and `{{variables}}` from a `.http.env.json` beside the file or from `{{$env.NAME}}` — and sent on a worker thread through `ureq`.

**What counts as an error.** A non-2xx status is a response, not an error. Bodies are capped at 8 MiB.

**The response is an ordinary tab.** The result renders as a response document the editor opens like any other tab: `.jsonc` with the status and headers as `//` comments and the body pretty-printed, or `.xml`, `.txt`, or the image bytes themselves.

**Secrets never leave substitution.** History and the response document carry the RAW request line with variables unresolved, and a `{{name}}` with no value refuses to send rather than leak the hole to a server.

### import_vscode.rs

A one-shot VS Code profile import behind `croft import-vscode`. It locates the user directory per platform (VS Code, Insiders, VSCodium, Cursor, Windsurf) and converts `settings.json`, `keybindings.json`, `snippets/*.json`, and the colour theme named by `workbench.colorTheme` — resolved from the product's extensions directory and converted through `vscode_theme.rs` — into croft's own files.

**Three conversions of very different difficulty.** Snippets are nearly free, since croft's format already mirrors VS Code's; the only real work is that VS Code carries a snippet's language in the FILE NAME while croft carries it in a `scope` field. Keybindings share a file shape but no command ids, so they convert through a table whose every entry is checked against `Command::from_id` by a test. Settings genuinely differ, so only keys croft has a real equivalent for are mapped.

**Unmapped items are reported, not dropped.** "My settings imported" and "my settings are gone" must not look the same.

**Merge, never overwrite.** An existing croft value always wins and the conflict is listed, which is also what makes a second run a no-op.

### launcher.rs

The macOS `croft install-launcher` command. It builds a clickable Croft.app via `osacompile`, then brands the plist with PlistBuddy.

**Why an AppleScript applet, not a shell bundle.** A double-clicked document arrives as an `odoc` Apple Event that only an `on open` handler can receive, so a `#!/bin/sh` bundle cannot work. Opened files launch `--zen`.

**Why the deletes are separate best-effort calls.** PlistBuddy exits non-zero on deleting an absent key, and the cleanup deletes target keys `osacompile` never emits: `CFBundleIconName` is always absent, and `CFBundleDocumentTypes` is cleared before the rebuild. One shared exit status failed every install before signing and registration, so the deletes now run as separate best-effort invocations while the required Add/Set batch stays fatal.

**Signing and registration.** It finishes with an ad-hoc `codesign`, because editing `Info.plist` invalidates `osacompile`'s signature, plus an `lsregister` nudge so Spotlight sees the bundle.

### locate.rs

Maps a test name to a source location. `--list` carries no file or line, so `find_test_source` walks the workspace grepping `fn <leaf>` and ranks files by module-path match.

**What the walk skips.** `.gitignore` is honoured even outside a git checkout, via `require_git(false)`. `target` and `node_modules` are always skipped, because build output can define a same-named fn.

**Languages that carry their own file.** pytest and JS node IDs carry their file, so those grep just that file with the escaped title. JS prefers a `test(`/`it(` declaration line over a comment or fixture that merely mentions the title, falling back to the bare title.

**Kept off the render loop.** The app runs it on a background thread, with `test_jump_rx` drained in `sync_explorer_panels` and replies from before a re-root dropped, so a big workspace cannot freeze the render loop.

**Buffer-local scans.** `enclosing_fn_name` (run-at-cursor, total on an empty buffer) and `test_fn_on_line` (the gutter play glyph) scan the open buffer.

### magic.rs

Content-based format detection: a pure magic-byte signature table covering PNG, JPEG, GIF, WebP and BMP, PDF, the zip family, gzip, tar's offset-257 `ustar`, and SQLite.

**Second hint, not first.** It is consulted as the SECOND routing hint in `Editor::open`. The extension stays first, and sniffing decides when the extension gave no answer or lied — an image or sheet extension whose decode or parse fails falls through rather than failing the open.

**Where sniffed types go.** Sniffed images and PDFs re-route to their viewers. A zip container gets one xlsx attempt through `open_sheet_with_kind`, which bypasses calamine's extension-resolved `open_workbook_auto`. Every failed attempt continues into the text/binary path, whose hex fallback guarantees the open never dead-ends.

### merge.rs

Merge-conflict machinery. It scans VS Code-regex markers (`<<<<<<<`, `|||||||` diff3 base, `=======`, `>>>>>>>`) into `ConflictBlock`s, and does Accept Current / Incoming / Both resolution by line-splicing.

**Caching that cannot go stale.** The editor caches blocks per `edit_seq`. Every whole-buffer swap bumps the seq, including image, sheet and PDF previews, so the cache can never go stale. The editor tints the regions, and Cmd+. opens the resolve picker. Palette merge commands refuse non-text tabs.

**The guided flow.** Source Control classifies unmerged porcelain codes into a MERGE CONFLICTS section whose entries open the working file parked on the first conflict. F7 and Shift+F7 wrap between blocks. Each header row paints clickable `[Accept …]` actions, with hit spans cleared per render — the frame-truth invariant — and the inline blame annotation is suppressed inside blocks so it cannot overpaint them.

**Bulk accept and completion.** "Accept All Current/Incoming" splices every block back-to-front, one undo step each. "Merge: Complete Merge" refuses while blocks remain, then saves and `git add`s the file so it rejoins the staged flow.

### merge_editor.rs

A three-way merge editor following VS Code's 2022 merge-editor model. `MergeView` diffs base→ours and base→theirs with `build_diff_rows`, clusters transitively overlapping hunks in doubled coordinates so that same-point insertions conflict while adjacent hunks stay independent, auto-resolves one-sided clusters straight into the initial Result, and keeps base text in the conflict regions.

**The Result is the ordinary buffer.** The view only tracks each region's span. Accepts splice through one undo step, and manual edits reconcile per-frame off `merge_edit_row`/`edit_seq` and mark the region manually resolved. Because the Result is the host editor's own buffer, LSP, undo and save need nothing special, and the renderer just carves Current | (Base) | Incoming panes off the top of the editor rect — stacked when narrow, with checkbox gutters clickable per conflict.

**Where the sides come from.** Inputs are `git show :1:/:2:/:3:` via `git::read_file_at_stage`, where missing stages are empty sides (AA/DU/UD), or synthesized from the marker scan for a plain conflicted file. SCM's MERGE CONFLICTS entries open this editor, and "Reopen as Text" reaches the unchanged in-buffer marker flow.

### notebook.rs

The Jupyter rendered view. An `.ipynb` file parses into the same `(lines, images)` state the Markdown preview machinery already renders, so wrap, scroll, the inline-image overlay and Reopen as Text (the raw JSON, sticky via `force_text`) all come for free.

**Cell rendering.** Markdown cells go through the markdown builder, with anchors offset into the merged document. Code cells render as fenced blocks in the kernelspec language under an `In [n]` frame. Stream and error outputs paint dim or red with ANSI stripped, and `image/png` outputs decode into hash-named scratch files that reserve overlay rows the way Markdown pictures do.

**Dispatch.** The preview carries a `notebook` flag so the stale-rebuild and theme-switch paths route to this builder rather than the plain Markdown one.

**No kernels.** The view is read-only truth about the file.

### outline.rs

A collapsible OUTLINE section under the file tree: the active editor's symbol tree, indented and kind-iconed, with follow-cursor highlight and click-to-jump.

**Two sources, one judge.** The tree paints instantly from tree-sitter (see `outline_syntax.rs`) and is refined by the LSP `documentSymbol` reply when it arrives. Each request carries the edit seq, and `drain_lsp_document_symbols` forwards EVERY reply to `apply_outline_symbols`, which is the sole judge: it filters on the active path and on exact edit-seq equality, so a slow server cannot flicker the breadcrumb's symbol crumb while you type.

**No pre-selection in the drain.** Collapsing a tick's replies to whichever arrived last let a late reply for the file just left evict the active file's. Collapsing by highest seq is no better, since seq is a per-`Editor` counter that restarts at zero in a fresh editor group.

**Placement.** It is one of the ⋯-menu Explorer sub-views, alongside Open Editors, Folders, Timeline and Dependencies. `App::render_explorer_sections` stacks the visible ones, with the tree absorbing the leftover rows.

### pair/proactive.rs

The proactive-look detector, pure text-in and row-out. Tree-sitter judges whether the driver COMPLETED a new construct since the navigator's last look.

**Language-specific completion tests.** Code languages multiset-compare outline symbols via `outline_syntax::symbols_for`; half-typed code parses as an `ERROR` node and never fires. Markdown walks its block grammar for a new heading or an ADDED paragraph, keyed on count growth, so prose edits never re-fire.

**Gates live in the App.** `App::tick_proactive_navigator` owns them: seated, idle, a 2s typing pause, the file previously yielded, and one scan per buffer state. It then hands the seat the same comment-only yield turn Cmd+K Y would, anchored at the new construct.

**Opt-out.** The `disable_proactive_navigator` pref and the palette command "Navigator: Toggle Proactive Comments".

### parse.rs

Per-tool output parsers. All node IDs are normalised to `::` separators so one panel tree serves every runner.

**The runners covered.** libtest (`parse_test_line` for run lines, `parse_list_line` for discovery), pytest (`-v` result lines, `--collect-only` node IDs), vitest (`parse_vitest_list_line` for `vitest list`, `parse_vitest_tap_line` for the streaming `--reporter=tap-flat` run lines, whose `file > describe > test` IDs are complete per line), and jest (`parse_jest_json`).

**Why jest is different.** jest prints one `--json` document to stdout at run end, with human output going to stderr, yielding every assertion with its describe chain and treating `pending`/`todo` as the skip family.

### port_detect.rs

Loopback-port detection behind the PORTS panel. A stateful output-stream scraper runs in the terminal reader thread, watching for URL banners and `listening on :PORT` lines and emitting each port once, alongside a low-cadence lsof/ss socket poll scoped to the shell's process subtree. The module also holds the Cmd/Ctrl+click URL resolver.

**The bare-announce regex needs a colon or the word `port`.** An announce verb followed by a plain count is ordinary output — `running 289 tests`, `Started 15 workers` — and fabricating a port from it offers to forward one nothing is listening on.

**Subtree scoping answers only one question.** It says what should be SURFACED, never what is still UP. `poll_all_listening` is the unscoped companion the reconciliation uses, and it returns `None` (no evidence) rather than an empty set when neither tool runs.

### ports.rs

The panel group's PORTS tab: a registry of detected loopback ports (port, address, process, origin) fed by `port_detect.rs`, with an orange `⇄ host` marker for a forwarded remote port. It is a pure widget handling selection and click hit-testing; the App runs the open, forward, copy and stop actions.

**Reconciliation uses the unscoped scan.** `reconcile_live`'s `live` set is EVERY loopback listener on the box, from `port_detect::poll_all_listening`, never the pane-subtree scan that decides what to surface. A container's published port is nobody's descendant, and retiring it for being missing from a snapshot that could never contain it dropped the row and `ssh -O cancel`ed a live tunnel seconds after the scrape found it. A failed probe reconciles nothing at all.

**`x` means two things, and they are not the same.** `stop_forwarding` tears the tunnel down and LEAVES the row; `remove` dismisses the row and suppresses re-detection for the session. Routing the stop through `remove` blacklisted a still-listening port the user could never forward again.

**Scrolling.** The body scrolls to keep the selection visible, since `f` and `x` act on it.

### provenance.rs

Which SEAT wrote each line: a `Seat` (you, navigator, agent-by-pane, or collab peer) plus a per-buffer line map, feeding the gutter overlay and the inline blame annotation. Git blame answers "which commit", which cannot answer "did I write this or did the model?" — a commit records the author of the SAVE, not of the keystrokes.

**A line croft did not watch being written is Unknown, never guessed.** This invariant shapes the whole module. An overlay that is right most of the time gets read as fact, and the one line it attributes wrongly is exactly the line someone is arguing about.

**Splicing drops rather than carries.** `splice` drops the attribution of every line an edit replaced instead of carrying it onto the replacement, and the caller records the new lines against the seat that made the edit. A caller that forgets leaves them unknown, which is the safe direction.

**The seat is a call-site parameter.** It is a parameter on `insert_str_as` rather than editor state, because the same buffer takes text from several seats and which one is making THIS edit is known only at the call site.

### quickfix.rs

Parses the last `grep`, `rg` or `git grep` command line into a pattern, Search toggles, and include/exclude glob lists, so "Terminal: Search & Replace from Last grep/rg" can seed and run the Search sidebar — reusing its multi-file replace-all — from a terminal search.

**Flag handling.** `-g`/`--glob`s accumulate comma-separated. rg's `!`-negated globs and grep's `--exclude`/`--exclude-dir` land in files-to-exclude. rg and ag treat `-s` as forcing case sensitivity, while grep's `-s` stays the no-messages flag.

**The seed replaces wholesale.** `SearchPanel::seed` swaps the panel's query and both filter lists outright, and also resets field selections, because a stale byte range into shorter seeded text panicked. It then refocuses the query.

### registry_index.rs

The remote vetted index at `extensions.croft.software`. It fetches `index.json` and `index.json.sig` over Cloudflare HTTPS, verifies an ed25519 signature against a baked public key (`INDEX_PUBLIC_KEY`) BEFORE caching — verify-at-write, trust-on-read under `~/.cache/croft` — gates entries by `api_version`, and on install fetches the manifest and checks its sha256 against the signed index. It is disarmed (no network, bundled-only) when the key is all-zero.

**Stale-while-revalidate.** Cached entries render synchronously while a background refresh always refetches. `App::drain_ext_index_refresh` signals a panel rebuild only when the verified index changed, so a new extension appears on the next launch with no TTL wait. The panel's ⟳ button (`App::refresh_extension_index`) re-pulls mid-session.

**Where the index lives.** It is git-hosted at `codeberg.org/vitali87/croft-extensions` and published to the VPS by a systemd timer mirroring croft-docs.

### release_notes.rs

Hand-curated "IN THIS RELEASE" highlights (feature or fix, glyph plus summary) shown on the welcome panel. The text is DATA: one file per version in `src/release_notes/<version>.md`, baked in by `build.rs` and parsed once. No git log, no network.

**One file per version.** A single shared const sat on every open pull request's rebase path. A missing file for the current version is a BUILD error, so a binary always describes itself.

**The card is sized before the logo.** `welcome_card_inner_width` is the one wrap width shared by the height measure and the paint pass, and the logo yields down to its 4-row minimum before the card may clip. The logo-first order kept a full-height logo above a note clipped mid-sentence in height-starved windows.

### run_debug.rs

The Run and Debug sidebar widget: an empty-state Run [filename] button, and when a session is live the paused-state tree (call stack, expandable variables, WATCH), a debug console of program output, and a `❯` REPL prompt. The App builds the rows and maps clicks back to frames and variables.

**WATCH is frame-relative.** Session-scoped expressions are re-evaluated on every stop via DAP `evaluate` with context `watch`, against the SELECTED frame. The stop selects the top frame, and clicking a call-stack frame re-evaluates every watch against that frame.

**The changed-value baseline rotates exactly once per stop.** It is armed by `Stopped` and consumed by the first `InspectionUpdated`. Later `InspectionUpdated`s — frame switches, variable expansions — re-evaluate WITHOUT rotating, or the stop's own values would clobber the comparison.

**Row mechanics.** A rejected expression renders `<not available>`, the `success:false` branch of `DapEvent::Evaluated`. Rows carry a right-edge remove `✕` with hit rects cleared per render, and the trailing "+ Add Expression" row and the palette's "Debug: Add Watch Expression" both open the input popup.

### dap/session.rs

One launch session: the initialize -> setBreakpoints -> configurationDone -> stopped state machine, the event classifier, the stackTrace -> scopes -> variables inspection chain, `evaluate` (REPL, hover, watch), breakpoint-verification tracking, pause and reverse-request replies — all over one adapter-agnostic `launch_with`.

**Conditional breakpoints and logpoints.** `SourceBreakpoint` carries `condition` and `log_message`. A logpoint's message is interpolated and printed by the adapter instead of pausing; it is set with Shift+Alt+F9 or the gutter menu and shows as an amber diamond in the gutter.

**No vendored protocol types.** Requests are built from `Value`-based builders, and `AdapterKind` names the launch mechanisms.

### src/session.rs

Local session persistence. `croft attach` and `croft ls` run croft under the session host (`session_host.rs`) so terminals, LSP and DAP survive closing the window. Live legacy dtach sessions keep reattaching through dtach.

**`ls` skips the collab relay socket.** The workspace's `<hash>.collab.sock` shares the directory and the keying but is not a session, so listing it printed a phantom row. Pruning it belongs to the relay's own bind path, not here.

### svg.rs

SVG file-preview rasterisation: `usvg` parse plus `resvg` render into a PNG that the standard image-overlay pipeline consumes unchanged.

**Vector render once, bitmap refit after.** The vector render happens once per open at a fixed quality — longest edge `1600px`, with small icons capped at 8× natural so they stay crisp instead of blurring. A pane resize refits that stored PNG through `fit_image_auto` like any image tab: a bitmap rescale, never a fresh vector render.

**The `<text>` fontdb is lazy.** It is built on the FIRST preview, never at startup, because `init_graphics`' icon bake is the critical path and codicons carry no text. Generic families are remapped to a real face when the host lacks the fontdb defaults (Arial, Times) — imperfect typography beats invisible text.

**Fallbacks and overrides.** A parse failure falls through to the XML source in the text editor. The per-tab `force_text` override ("File: Reopen as Text") skips every preview route and sticks across same-path FS-sync reloads, so an SVG edited in one split refreshes the preview in the other.

### tasks.rs

Auto-detected project tasks. It reads the manifests the repo already has — `.vscode/tasks.json` with JSONC tolerated, Makefile, justfile, `package.json` with a lockfile-matched runner, `Cargo.toml`, `pyproject.toml` — into runnable `Task` commands, backing "Tasks: Run Task" and the Cmd+Shift+B default build. A `tasks.json` `isDefault` outranks the first build task.

**Terminal-pane reuse is strict about the directory.** Each task runs in a named terminal pane that is reused only while its shell sits idle at EXACTLY the task's directory; a shell that has cd'd into a subdirectory is not reused. Where the platform cannot report a cwd at all (Android, a remote pane's ssh process), reuse falls back to the pane's name alone. The cwd is kernel-reported, and the write clears a half-typed prompt line first.

### update_check.rs

A release-availability check plus a staged upgrade. Once a day, tracked by the cache file `~/.cache/croft/update-check.json`, a local croft asks GitHub's latest-release endpoint off-thread; a newer, undismissed version raises a click-only popup at bottom-left offering Update or Later.

**Update stages, it does not replace.** Update runs `cargo install croft-software --version X --root ~/.cache/croft/staged` in the background, so the binary on PATH is untouched and a fresh launch stays on the current version. The popup then offers Relaunch, which copies the staged binary over the installed one and re-execs — the same path F9 takes.

**Never automatic.** Later remembers the version so it is not offered again, and `CROFT_NO_UPDATE_CHECK` disables the check entirely.

### voice.rs

Voice input for the Termux OSK mic key. It delegates to `termux-dialog speech` (Android's SpeechRecognizer, the same engine Gboard's mic uses), auto-installs the `termux-api` package via `InstallState`, and sends the transcript over a channel the app injects through `handle_key`.

**`termux-dialog speech`, not `termux-speech-to-text`.** The service closes its output on `onEndOfSpeech` and so discards the final `onResults` transcript.

**Tap, not hold.** A tap opens the system speech dialog and the result is injected when Android finalizes on silence; a second tap cancels, since killing preempts the result. The process runs in its own process group so cancel kills the whole tree. It is a tap rather than a hold because Termux steals finger-holds for text selection.

### collab_agent.rs

A headless collab guest exposed as an MCP server on stdio, behind the hidden `croft collab-agent --workspace <path>` subcommand. It joins the workspace's relay and serves five `collab_*` tools (open/read/replace/caret/status) over JSON-RPC 2.0 NDJSON, so an external AI co-edits with a live named caret — for example Claude Code via `claude mcp add croft-collab -- croft collab-agent --workspace <path>`.

**Keeping the replica fresh.** A `30ms` pump thread runs between tool calls, so the agent's replica does not go stale while it is thinking.

**It is an ordinary guest.** The agent inherits every guest property: owner-only disk writes, per-file site ids, CRDT convergence, and the 0600-socket trust boundary. croft itself contains no LLM code.

### pair/local.rs

The navigator's local-model transport: one minimal Anthropic-compatible `/v1/messages` streaming call per turn, against Ollama, LM Studio, llama.cpp or vLLM. Selected with `croft pair --provider ollama [--base-url <url>] --model <m>`.

**Deliberately not the claude CLI.** The CLI's roughly `213 KB` tool-schema prefill `500s` local servers regardless of flags, so this transport talks to the endpoint directly instead.

**A per-seat worker owns the conversation**, because the endpoint is stateless. It maps SSE text deltas into the shared fence machine and apply path. When the endpoint is down it fails the turn naming that endpoint, and the seat survives.

**Keyed gateways.** `ANTHROPIC_AUTH_TOKEN` is read from the environment; a token is never persisted.


### agent_lane.rs

A per-agent ledger of the files an agent changed while you were looking elsewhere. While a pane's agent is `Working`, every workspace content event is attributed to it.

**Each row carries a review baseline.** The baseline is an FNV-1a content hash taken at the last "mark reviewed", so a row leaves the queue only once its content has actually been seen. Reviewing is not dismissing.

**Concurrent writes are attributed, not guessed.** A write while two agents work is attributed to BOTH and flagged `shared`. A write with no agent working belongs to the user and never enters a lane.

### dap/configs.rs

Reads `launch.json` (`.croft` over `.vscode`), both its `configurations` and its `compounds`, and resolves a chosen entry into a DAP request.

**`presentation` is honoured for configurations AND compounds, in one sorted list.** VS Code puts both kinds through the same comparator, so a config and a compound naming the same `presentation.group` belong together; sorting the two kinds separately would split that group no matter what the file asked for. `visible_and_sorted` is transcribed from VS Code's `getVisibleAndSorted` rather than invented, because each of its rules is one croft would otherwise get subtly wrong: an entry that HAS a `presentation` sorts ahead of one that has none, a grouped entry ahead of an ungrouped one, groups by name rather than first-seen, and a numeric `order` ahead of a missing one. The sort is stable, which is what leaves a workspace declaring no `presentation` in exactly its file order. One divergence, deliberate: VS Code compares group names with locale-aware `localeCompare` where croft compares bytewise, which agrees on ASCII and would otherwise cost a collation dependency for a picker's ordering.

**Row ids are built before the sort.** `compound:{i}` and `{i}` index the UNSORTED lists, so a row keeps naming the same entry however the sort moves it; indexing the sorted sequence would launch a different configuration than the row says as soon as one entry declares a `presentation`.

**A one-member compound refuses only what croft cannot READ.** `preLaunchTask`, `hidden`, `group` and `order` are all honoured; what remains in `unsupported_keys` is a value croft cannot read — a `presentation` that is not an object, or a `hidden`, `group` or `order` that is not a bool, string or number. A key requesting nothing (`null`, `false`, `""`, an empty block) is not recorded, since croft delivers that by doing nothing, and a mistyped KEY is ignored rather than refused so that whatever VS Code adds next is not rejected. Those keep refusing because `{"hidden": "true"}` is a typo, and launching it unhidden with no signal is worse than the refusal. `stopAll` is absent at either value: it decides whether ending one session ends the others, which is meaningless for the single session a one-member compound launches (#310 owns the rest).

### dap/reaper.rs

Sweeps orphaned vscode-js-debug processes left behind when croft crashes or force-quits without running `Drop` — both the server and its detached watchdog, which setsids into its own session and so survives a group-kill. It runs async at startup and after each session teardown.

**It kills only scripts under `~/.croft/js-debug` that have been reparented to init (pid 1).** That restriction is what keeps a live session from ever being touched.

### dap/registry.rs

A data-driven debug-adapter registry. It maps a file extension to an `AdapterKind` using the `[[debug_adapters]]` blocks in the bundled and user manifests, and skips adapters whose extension is disabled in the Extensions panel.

**Manifest data replaced a hardcoded match.** It supersedes the old `adapter_for_extension` match, but only the mapping is data-driven: the launch mechanisms themselves stay native, like the PDF/CSV viewers.

### docx.rs

A read-only preview for docx and odt. Both are zip+XML, so the walker emits markdown — headings from styles and outline levels, bold/italic runs, list items, tables as pipe rows, and embedded pictures extracted hash-named into scratch — which the proven markdown builder then renders, reusing its styling, wrap, and inline-image overlay.

**The preview stores its `doc_path`** so a theme switch can re-walk the file.

**The text side is a stub.** Reopen as Text routes the zip container onward to the archive browser, and then to hex.

**Structure fidelity, not layout**, with a 50MB source cap.

### history.rs

Local history: per-save snapshots under `~/.config/croft/history`, FNV-keyed with legacy-key migration, stored as raw bytes so any encoding round-trips, deduped and capped. It merges into the Explorer TIMELINE beside git commits (out-of-root files list snapshots only) and backs snapshot diff and restore. Recording runs off the render thread.

**Saves inside a 10s merge window replace the newest entry**, the way VS Code's mergeWindow does.

**A `<millis>.seats` JSON sidecar records who typed each line.** It is written and removed with its snapshot, and read back onto an unchanged file.

### iterm2.rs

iTerm2 plist mutation helpers for fonts and Croft key mappings.

**User-keybinding forwarders are tracked in a ledger.** They are recorded in a root-plist ledger (`Croft User Keymap Forwarders`, key to hex) and swept on the next setup run before any inserts, but only while their value is still the exact croft forwarder. That way a chord deleted from `keybindings.json` releases its Cmd chord without destroying an entry the user has since rewired in iTerm2 prefs.

### keymap.rs

User key bindings from `~/.config/croft/keybindings.json`. It parses chord strings (ctrl/alt/shift/cmd/mod + key) into normalized Chords mapped to palette Command ids.

**User chords are consulted at the top of `handle_key`, ahead of the built-in chords** — only off the terminal, and only for modifier/function-key chords.

**The parser is tolerant JSONC** (it strips `//` comments), and it reloads on save.

### lsp/config.rs

Per-language LSP config for basedpyright, ruff, ty, vtsls, rust-analyzer and gopls. The `ServerConfig` factories are the executable spec that the bundled manifests reproduce.

**Language is an open newtype — an interned `lsp_id`, not a closed enum** — so an extension can contribute a new language with no Rust change. Per-language data (extensions, root markers, family) lives in the language table.

### lsp/install.rs

Croft-managed server provisioning: lazy background installs into `~/.croft/servers` through three backends. npm covers vtsls, JSON/HTML/CSS, yaml and bash; uv covers ty and ruff, including the uv bootstrap, and is rerouted to Termux's `pkg` on Android; Binary covers clangd, taplo and rust-analyzer, downloading and unzipping or gunzipping a host-agnostic per-platform release binary, PATH-first including `~/.cargo/bin`.

**A Binary can carry a `termux_pkg`.** On Android the glibc release can't run on bionic, so such a backend reroutes to `pkg`, as rust-analyzer does.

### lsp/semantic_cache.rs

A content-keyed disk cache of semantic-token batches at `~/.croft/sem-cache`.

**Only a batch whose `seq` still matches the live buffer is applied OR stored.** A reply can sit in the request retry backoff while an external tool rewrites the file; decoding it over the new lines would both mis-colour the buffer and persist that result under the NEW content's key, replaying it on every future open.

### mcp/registry.rs

Data-driven MCP registration. `contributed_commands()` does eager palette registration and skips disabled entries; `resolve_command_in_dir()` is lazy, yielding a tool plus a spawnable server with a pinned `Provision`, with the app passing the config dir it carries. Both read the `[[commands]]` and `[[mcp_servers]]` manifests.

**Viewers come from the same manifest path.** `contributed_viewer_commands_in_dir()` and `viewer_for_path_in_dir()` read `[[viewers]]`, which is a terminal program run on a file in its own pane.

### media.rs

An audio/video info view for `.mp3`, `.wav`, `.flac`, `.m4a`, `.mp4`, `.m4v` and `.mov`.

**Pure-Rust header parsers over a bounded head read; streams are never decoded.** The parsers are the WAV fmt chunk, FLAC STREAMINFO, MP3 ID3v2 text tags plus the first MPEG frame with a size-based duration estimate, and an MP4/MOV mvhd + tkhd box walk.

**The card renders as markdown through the preview machinery**, with `doc_path` and media flags driving theme rebuilds. Video gets a hash-named ffmpeg poster frame when the tool exists on PATH, following the pdftoppm optional-tool pattern.

**Junk wearing a media extension falls through to the binary path.**

### notifications.rs

Notification sinks, configured under the `notifications` config key: one delivery worker fed by a bounded queue, pure request builders for ntfy and webhooks, and `termux-notification` and `command` sinks that pass the payload in `CROFT_*` environment variables.

**Events arrive from three sources**: the finished-command drain, the OSC 9 drain, and the Test Explorer's once-per-run failed latch.

### outline_syntax.rs

The tree-sitter outline provider. It extracts the OUTLINE panel's symbol tree — functions, structs, methods, fields and so on — directly from the buffer's syntax tree using hand-written per-language queries.

**Tree-sitter first, LSP second.** The panel paints instantly instead of waiting for a cold language server to answer `documentSymbol`; the LSP reply supersedes the syntax-tree outline when it arrives. This is the same approach Zed and aerial.nvim take. Rust, Python, JS, TS, TSX and Go are covered by queries; other languages fall back to the LSP outline.

### src/output.rs

The in-process OUTPUT bus behind the panel group's OUTPUT tab. It holds named channels — one per language server, plus Debug Adapter, Git, Server Provisioning and Navigator (the resident AI pair programmer's commentary) — each a capped ring buffer of levelled lines.

**Producers push here and mirror to disk.** Code across the codebase writes to the bus while also mirroring to the on-disk `lsp.log`. A generation counter lets the widget re-pull only when something actually changed.

### plot.rs

`croft plot` reads numbers, CSV/TSV or JSON lines on stdin — delimiter and header auto-detected, with `--x`/`--y` picking columns — and draws them as a line, bar, spark or histogram chart.

**Image output, with a text fallback.** An SVG in the theme's colours is rasterised through `svg.rs` and emitted with the previews' inline-image escape. iTerm2/Kitty are detected from the environment; the sixel probe needs a raw tty on stdin, which here is the data pipe. With no image protocol, or with `--text`, the same data renders as braille (line/spark) or block (bar/hist) characters.

**Long series are bucketed to the pixel/dot width.** That keeps 10k rows linear.

### prefs.rs

Durable user preferences — color theme, Customize Layout chrome, format-on-save, auto-save — persisted at `~/.config/croft/config.json`. The searchable "Preferences: Open Settings" hub toggles these and links to the raw JSON.

**`host_accents` dresses terminal panes per host.** Its rules ({pattern glob, accent hex, badge}) are compiled to globset matchers in the App and re-read whenever `config.json` is saved.

### snippets.rs

User snippets loaded from `~/.config/croft/snippets.json` in VS Code format: prefix, body as a string or array, and an optional language scope. It reloads on save.

**`parse_body` turns tab-stop syntax into stops the editor drives.** A body's `$1`/`$0`/`${1:placeholder}` syntax becomes insert text plus ordered stops that the editor walks on Tab.

**Snippets also reach the completion popup.** Matching snippets are injected as `CompletionItem.is_snippet` and expanded on accept — the same path LSP snippet completions take.

### sqlite_view.rs

A read-only SQLite browser. Every table becomes a worksheet in the existing sheet grid, so navigation, the cell cursor and Tab table-switching are all reused. Rows are capped at 500 with the true count shown in the sheet name, and cells are typed (NULL empty, blobs summarised). Files are routed both by extension and by the magic-byte sniff.

**Read-only over the bundled sqlite3.** It opens with `SQLITE_OPEN_READONLY` over the BUNDLED sqlite3, so there is no system dependency, and a live application database is never locked, mutated, or created. Locked or corrupt files surface their error.

### testing/failure_site.rs

Works out where a failing test actually failed. It parses libtest panic lines (both the current and the pre-2023 shapes), pytest traceback frames, and jest/vitest stack frames down to a `file:line`.

**Only locations in the user's own code count.** A location under `.cargo/registry`, `node_modules`, `site-packages` or the standard library is rejected rather than offered, because a breakpoint there fires before the interesting state exists.

**The LAST location in a failure block wins.** Every runner prints outward-in.

### testing/registry.rs

A data-driven test-runner registry. It maps a workspace root to a Runner using the `[[test_runners]]` blocks in the bundled and user manifests, matching on marker files and/or `package.json` (dev)dependency names.

**Manifests replaced a hardcoded ladder.** This supersedes the old hardcoded `runner_for` ladder, and it skips runners whose extension is disabled in the Extensions panel. The run mechanisms — commands, parsers, binary resolution — stay native, like the debug adapters.

### widgets/dependencies.rs

A collapsible, language-aware DEPENDENCIES section, offered as a ⋯-menu Explorer sub-view and display-only. It detects the workspace's package ecosystem(s) from manifest files at the root (`Cargo.toml`→Rust via `cargo metadata`, `pyproject.toml`/`requirements.txt`→Python, `package.json`→Node, `go.mod`→Go), resolves the packages off-thread, and labels the header for what it found ("RUST DEPENDENCIES" and so on).

**Gated on detection.** A folder with no manifest shows no dependency view and drops it from the ⋯ menu. The App holds the detected ecosystems in `dep_ecosystems`, recomputed on every re-root.

### widgets/editor_find.rs

A VS Code-style inline Find bar (Cmd+F) with an active-match orange highlight, Enter / Shift+Enter to walk matches, and case-sensitive / whole-word / regex toggles. The Replace row (Cmd+Opt+F) adds Tab to switch field, Enter to replace-and-advance, and Cmd+Opt+Enter to replace all in one undo step.

**Replace All uses the same enumeration as the display.** It walks matches via the same `split_for_highlight` that the count, paint and navigation use, so it never touches what the bar cannot show, and it recounts afterwards.

**Regex replacements expand `$n` captures** plus VS Code's `\n`/`\t` escapes, where a replacement newline splits the line.

### widgets/history_popup.rs

The Ctrl+Shift+H command-history popup over `command_history.rs`: a query line with a caret, newest-first deduped results (green/red exit dot, plus duration, cwd and non-zero exit meta), and Ctrl+R to cycle the scope filter.

**A pure widget.** The App runs the search on each edit and types the pick via `paste_input`.

### widgets/output.rs

The panel group's OUTPUT tab: a read-only viewer over the `output.rs` bus with a channel dropdown, a minimum-level filter, an RPC trace toggle, and a clear action. It renders the selected channel's level-coloured tail and auto-follows new lines until the user scrolls up.

**The open dropdown windows its list when the channels outnumber the body rows** (`dropdown_scroll`). It opens at the current selection, the wheel and PageUp/Down scroll the window instead of the log, and a one-column scrollbar (click-to-jump) marks the truncation. Before this, overflow channels were unreachable by any gesture.

### widgets/workspace_symbols.rs

VS Code's "Go to Symbol in Workspace", reached with `#` in Quick Open. The query box's text fans out to every running server as `workspace/symbol`. Rows show a kind icon, the name and the workspace-relative path; Enter opens the file at the definition.

**Requests are debounced per keystroke burst and keyed by request id,** so stale replies are dropped. Selection restarts at the top on each reply.

### File encoding

How croft decodes a file on open, re-encodes it on save, and what it does when the buffer holds characters the target encoding cannot represent.

**Re-encoding on save.** Saves re-encode through `Editor::encode_for_disk`, which re-emits the file's byte-order mark and hand-rolls UTF-16. `encoding_rs` is decode-only for UTF-16 by the WHATWG spec, so `Encoding::encode` silently falls back to UTF-8. Taking its output at face value wrote a file that disagreed with the encoding the status bar reported.

**BOM before the binary check.** UTF-16 output always carries its BOM, and `open` sniffs the BOM before the `is_binary` heuristic. UTF-16 text is half NUL bytes, so the heuristic would otherwise reject it as binary. Without both halves, croft could write UTF-16 it could never reopen.

**Sticky encoding choice.** `reopen_with_encoding` records the encoding `decode` actually used, which a BOM overrides, and that choice sticks across a same-path reload. `open` is also the FS-sync sweep and every revert, so re-detecting there would decode the file as UTF-8 and hand the next save the mojibake to write back.

**Reopening a dirty buffer.** "Reopen with Encoding" refuses on a dirty buffer and points at undo, never at saving. The buffer may hold mojibake from a wrong decode, and saving would write those replacement chars over the real bytes.

**Refusing a lossy save.** A save whose encoding cannot represent the buffer is refused (`SaveOutcome::EncodingLoss`). `encoding_rs` substitutes HTML numeric character references (`&#26085;`), not `?`, so the write would be silent irreversible loss.

**One-shot, named consent.** The refusal latches (`encoding_loss`), so auto save stops retrying. The explicit path arms a one-shot consent (`lossy_save_armed`): the second `Cmd+S` — told which characters are doomed — writes the substitutions. Any edit revokes both flags (`mark_buffer_changed`), because consent named the characters at prompt time, so a changed buffer re-prompts. Force save (disk-conflict overwrite) does not bypass the guard; only the armed consent does.

