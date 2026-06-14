# Layout

croft is a three pane workspace in the VS Code arrangement: an Explorer sidebar, a code editor, and a terminal, with an activity bar down the far left. This is the full pane-by-pane reference; the [README](README.md#layout) keeps the high-level summary.

## Panes

* **Left pane (sidebar):** Explorer with multi-select, cut / copy / paste, drag-and-drop moves, and VS Code style icons. Search and a Remote (SSH) explorer swap in via the activity bar.
* **Activity bar:** the icon strip down the far left. View icons (Explorer, Search, Source Control, Remote, Run and Debug) at the top, a settings gear at the bottom whose Color Theme picker switches between Croft Black (`#000000`, default) and Croft Dark (Blue) (`#1e222e`), persisted in `~/.config/croft/config.json`.
* **Top right pane (editor):** code editor with tree-sitter syntax highlighting, an LSP semantic-token overlay, and inline preview tabs for images, PDFs, and spreadsheets. Splits side by side with `Cmd`+`\`, with an optional native vim modal mode on `Cmd`+`E`. The usual VS Code editing commands are built in: move / copy lines, multi-cursor, toggle line and block comments, join lines, sort lines, transform case, trim trailing whitespace, and toggle word wrap.
* **Bottom right pane (terminal):** a real interactive shell, your `$SHELL` on a real PTY.
* All three panes resize by dragging the seams between them, including the seam between the two editor columns when the editor is split.

## Command Palette

`Cmd`/`Ctrl`+`Shift`+`P` fuzzy-searches and runs every named command, the same surface VS Code uses to make actions reachable without memorising a chord.

## Debugging

* **Debug Python with breakpoints:** set breakpoints in the editor gutter (`F9`), press `F5`, and croft launches the file under debugpy over the Debug Adapter Protocol so it stops on the red lines — step over/into/out (`F10`/`F11`), resume (`F5`), pause a running program (`F6`), stop (`Shift+F5`). When paused, the Run and Debug panel shows the call stack and an expandable variables tree, a debug console of program output with a `❯` REPL that evaluates in the selected frame, and hovering a variable in the editor shows its value. Conditional breakpoints (a red `◆`) and break-on-exceptions are in the Command Palette; breakpoints the adapter can't bind show hollow (`○`). Requires CPython 3.14+ (croft provisions a private debugpy venv on first use); no fallback to older interpreters. Rust / C / C++ files route to `lldb-dap` through the same machinery.
* **Or attach to a running Python process:** the Command Palette's "Debug: Attach to Python Process" lists live CPython 3.14+ processes and drops a `pdb` REPL into the one you pick (PEP 768 `sys.remote_exec`, no restart, no instrumentation). Because croft owns a real PTY, the debugger and any `sudo` password prompt run right in a terminal pane, instead of the half-managed console a GUI editor falls back to.

## Language servers

The editor speaks LSP for completion, hover, go-to-definition / references / implementations, rename, and diagnostics, each anchored at the file's own project root so monorepo sub-projects resolve correctly. For Python it runs Astral's `ty` as the primary server, with `basedpyright` as a fallback for the few capabilities `ty` does not yet advertise and `ruff` for lint; for TypeScript / JavaScript it runs `vtsls`. croft provisions `vtsls` (via npm) and `ty` / `ruff` (via uv, or `pkg` on Termux) for itself on first use, and picks up `basedpyright`, `rust-analyzer`, and `gopls` from your PATH if present.
