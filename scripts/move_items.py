#!/usr/bin/env python3
"""Move one stage-B pull request's items out of a big source file, unchanged.

Stage B of the refactor program (#2062) splits croft's big source files into
feature files. A target names the file to split and the folder its pieces go
to: `app` (the default) splits src/app/mod.rs into files under src/app/, and
`editor` splits src/widgets/editor.rs into files under src/widgets/editor/,
beside it (never editor/mod.rs: tar-based remote sync never deletes the old
file). Each target has a manifest (scripts/split_manifest.tsv for `app`) that
says where every item of its file goes: a PR number, 0 for an item that
stays, and a file. "mod.rs" below stands for the target's file. For one PR
this cuts that PR's items out of mod.rs and writes them into the file:

- each item keeps the docs, attributes and `//` comments directly above it,
  byte for byte;
- a method goes under a copy of its original `impl` header, so App methods
  sit in `impl App {` blocks and the methods of other types under theirs;
- items keep their order, and the second PR into a file appends to it;
- a private item becomes `pub(super)`. That is exactly what it could reach
  before: everything in `app`, its sibling files and its tests included. So
  no other file can fail to build for want of it, whatever order the moves
  and other PRs merge in. An item that is already `pub(super)` or wider keeps
  its visibility; a method in a trait impl, a macro call and a comment carry
  none and are copied as they are;
- an `impl` block that has nothing left in mod.rs goes, with its header;
- mod.rs gets `mod <file>;` and, when the file has free items (anything
  outside an `impl`), `use <file>::*;`, so code that named them still does.

It also writes the PR's release-note fragment and the file's row in
docs/ARCHITECTURE.md, then runs `cargo fmt`.

Then it builds the app and its tests with clippy, warnings denied, and fixes
the glue the build asks for, until both are clean:

- an item used outside the target's module (as `crate::app::name`) is
  private through the glob, so mod.rs re-exports it by name at the item's
  own visibility;
- a `use <file>::*;` that only the tests need gets `#[cfg(test)]` (and one
  only the app needs, `#[cfg(not(test))]`); one nothing needs goes.

Any other error stops it, with the error. Prove the result with
`scripts/check_pure_move.py <base> HEAD`.

It refuses to run when the manifest and mod.rs disagree: an item the
manifest names is not in mod.rs, or mod.rs has an item the manifest does not
place (one added since the manifest was written). Add or fix its row (a PR
number, or 0 to keep it) and run again.

A file whose tests sit in an inline `#[cfg(test)] mod tests { ... }` can
first move them to their own file: `--tests-out` writes the block's body,
dedented, to <folder>/tests.rs and leaves `#[cfg(test)] mod tests;` in its
place. Lines that start inside a string keep their indentation, since it is
part of the string. `--tests-out NAME` does the same for another inline test
module the target has words for (`mod NAME`, to <folder>/NAME.rs). A test
file can then be split like any other target.

Usage:
  move_items.py [--target T] --pr N    move PR N's items (run from the repository root)
  move_items.py [--target T] --check   only check the manifest against mod.rs
  move_items.py --target T --tests-out [NAME]
                                       move mod.rs's inline tests (or test module NAME) to their own file
"""

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict, namedtuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check_pure_move as cpm  # noqa: E402

MANIFEST = "scripts/split_manifest.tsv"
MOD = "src/app/mod.rs"
NOTES = "src/release_notes/unreleased"
ARCH = "docs/ARCHITECTURE.md"

# What each new file holds: its `//!` module doc, the words the release note
# uses for its code, and its row in docs/ARCHITECTURE.md.
FILES = {
    "code_scanning": (
        "SARIF results viewer (list, open, preview, baselines, export, fixes and step marks),\n"
        "GitHub code scanning (pick, download, follow branch, dismiss) and SARIF logs shown as\n"
        "problems.",
        "SARIF and GitHub code scanning",
        "SARIF results viewer, GitHub code scanning, SARIF logs as problems",
    ),
    "codeql": (
        "The CodeQL side bar: databases, query history and logs, running queries, suites and\n"
        "packs, result viewers, AST and CFG, the model editor, the run queue, variant analysis,\n"
        "and downloading and updating the CLI.",
        "CodeQL",
        "the CodeQL side bar: databases, queries, results, variant analysis, the CLI",
    ),
    "review": (
        "Pull-request review tabs (fetching with gh, viewed marks, checks, per-file diffs,\n"
        "worktree checkout) and the comment boxes in the editor: review threads, navigator\n"
        "notes and sticky notes.",
        "pull-request review",
        "pull-request review tabs and in-editor comment boxes",
    ),
    "debugger": (
        "Run and Debug: launch configs and Add Configuration, compounds and preLaunchTask waits,\n"
        "adapters, debug builds, attach pickers, the paused view, watches, the console,\n"
        "stepping, every kind of breakpoint, Debug Test and inline values.",
        "debugger",
        "Run and Debug: launch, adapters, breakpoints, stepping, watches, console",
    ),
    "runners": (
        "The Testing view, Tasks (Run Task, the build task, task inputs, problem matchers),\n"
        "runnable Markdown blocks, notebooks and kernels, Fleet, Live Run and .http requests.",
        "tests, tasks, notebooks and other runners",
        "Testing view, Tasks, runnable blocks, notebooks, Fleet, Live Run, .http requests",
    ),
    "viewers": (
        "Read-only viewers: the spreadsheet, CSV and SQLite grid, PDF, archives, hex, rendered\n"
        "logs, and the Markdown preview with its scroll sync.",
        "file viewers",
        "spreadsheet/CSV/SQLite, PDF, archive, hex, log and Markdown viewers",
    ),
    "settings": (
        "The Settings editor and hub, live and per-language settings, watching config files,\n"
        "snippets, file associations and encodings, the Keyboard Shortcuts editor and the F1\n"
        "overlay, the theme picker and profiles.",
        "settings",
        "Settings editor, config watching, Keyboard Shortcuts editor, themes, profiles",
    ),
    "agents_collab": (
        "Coding agents in panes, agent and worktree lanes, approving agent edits, the AI\n"
        "navigator, and multiplayer sessions: relay, presence and peer carets.",
        "coding agents and multiplayer",
        "coding agents, lanes, the AI navigator, multiplayer sessions",
    ),
    "remote_sessions": (
        "SSH connect and install, the ssh-pane offer, handing off to a remote croft, Remote\n"
        "Explorer keys, config sync, drop relay and scp uploads, port forwarding, and opening\n"
        "URLs on the local machine.",
        "remote sessions",
        "SSH sessions, hand-off to remote croft, config sync, uploads, port forwarding",
    ),
    "host_integration": (
        "The host terminal: escape sequences, restoring it on exit or panic, the panic log,\n"
        "the unsupported-terminal notice, the `croft view` and `edit --wait` socket and\n"
        "rebase-todo editing, self-update and relaunch, and the update card.",
        "host terminal integration",
        "host-terminal sequences and restore, panic log, croft view socket, self-update",
    ),
    "welcome_screen": (
        "The welcome page (logo picture and its bake, wordmark, Start list, release-note card,\n"
        "Loch layout) and the guided tour.",
        "welcome page and tour",
        "welcome page drawing and the guided tour",
    ),
    "scm": (
        "Source Control: the panel and its keys, background git and its log, commits,\n"
        "push, pull and sync, stash, branches and pickers, hunks, permalinks, the commit\n"
        "graph, its dialogs, change gutters and inline blame.",
        "Source Control",
        "Source Control panel, background git, commits, branches, hunks, blame",
    ),
    "diff_merge": (
        "Diff editor upkeep and navigation, merge conflicts and the three-way merge editor,\n"
        "the history scrubber, local history and the who-wrote-this lens.",
        "diff and merge",
        "diff editor, merge conflicts, three-way merge, history scrubber, local history",
    ),
    "explorer": (
        "The file tree (keys, type-to-jump, cut, copy and paste, rename and move with server\n"
        "fixes, delete, drag targets), Explorer sections, workspace folders and\n"
        ".code-workspace files, re-rooting, file watching and outside changes, zoxide jump.",
        "Explorer",
        "file tree, Explorer sections, workspace folders, re-rooting, outside changes",
    ),
    "language_servers": (
        "Language servers: sync, messages and progress, decorations, go-to and peek,\n"
        "hierarchies, rename, code actions, formatting, linked editing, smart select,\n"
        "highlights, symbol pickers, code lens, outline, breadcrumbs, sticky scroll, restart.",
        "language server",
        "language-server features: go-to, rename, code actions, outline, breadcrumbs",
    ),
    "completion": (
        "The completion popup, auto-imports, snippets, signature help, ghost-text suggestions\n"
        "and Emmet.",
        "completion",
        "completion popup, signature help, inline suggestions, Emmet",
    ),
    "search": (
        "The Search view and Replace All, Search Editor tabs, the find/replace bar (also over\n"
        "diffs and logs), Quick Open, and Switch Header/Source.",
        "search",
        "Search view, Search Editor, find/replace bar, Quick Open",
    ),
    "terminal_panes": (
        "Terminal panes: split, layout, collapse, maximize and rail, reorder, profiles,\n"
        "rename, notes, close and undo, saving and restoring the panel, redraw tracking,\n"
        "recordings, bells and exits, labels and pane buttons.",
        "terminal pane",
        "terminal panes: split, layout, profiles, recordings, panel save/restore",
    ),
    "terminal_tools": (
        "Typing into terminals (key encoding, broadcast, paste confirm, redaction,\n"
        "timestamps), find, quick select, copy mode, command history and decorations,\n"
        "CAPTURES, links and inline suggestions.",
        "terminal input and tools",
        "terminal input, find, quick select, copy mode, command history, links",
    ),
    "editor_tabs": (
        "Tabs and editor groups, tab menus and pickers, Go Back/Forward, bookmarks, symbol\n"
        "tabs, and moving or copying an editor into a new window.",
        "editor tab",
        "tabs and editor groups, Go Back/Forward, bookmarks, new windows",
    ),
    "save_restore": (
        "Saving (save all, auto save, format on save, sudo save, encoding loss), unsaved\n"
        "prompts, hot exit, session save and restore, Revert, New Untitled, Save As and the\n"
        "launch file.",
        "save and restore",
        "saving, unsaved prompts, hot exit, session save/restore",
    ),
    "bottom_panel": (
        "The bottom panel and its tab strip, the OUTPUT picker, PROBLEMS and the diagnostics\n"
        "store, the whole-project check, build matchers, and the PORTS panel.",
        "bottom panel",
        "bottom panel, OUTPUT, PROBLEMS and diagnostics, project check, PORTS",
    ),
    "workbench": (
        "Screen layout and Customize Layout, the activity bar and its badges, the Manage\n"
        "menu, side-bar auto-hide, splitter drags and screencast mode.",
        "workbench layout",
        "screen layout, Customize Layout, activity bar, splitters",
    ),
    "inline_images": (
        "Pictures in the terminal: support detection, the background chrome bake, sending\n"
        "and clearing each picture every frame (the activity-bar icons included), the\n"
        "editor, terminal and Markdown overlays, and the minimap.",
        "inline picture",
        "inline pictures: detection, chrome bake, per-frame overlays, minimap",
    ),
    "menus": (
        "Right-click and dropdown menus: shortcut hints, ContextMenu methods, geometry, keys,\n"
        "drawing, `dispatch_menu_action`, and the status-bar click menus.",
        "menu",
        "context and dropdown menus, `dispatch_menu_action`",
    ),
    "prompts": (
        "The input popup and the list picker (opening, keys, clicks, drawing),\n"
        "`submit_input_prompt` and `confirm_list_picker`, and the small inline prompt.",
        "prompt and picker",
        "input prompts and list pickers",
    ),
    "editor_input": (
        "Editor keys, vim mode, macros, multi-cursor and line commands, typing toggles,\n"
        "copy, cut and paste routing and the clipboard, and reading dropped file paths out\n"
        "of a paste.",
        "editor input",
        "editor keys, vim mode, macros, multi-cursor, clipboard and paste routing",
    ),
    "commands": (
        "`run_command`, the Command Palette, the Extensions view, extension commands (MCP\n"
        "tools, viewers) and Show Memory Usage.",
        "command",
        "`run_command`, Command Palette, Extensions view",
    ),
    "key_input": (
        "`handle_key` and `handle_key_inner`, Cmd+K chords, every `is_*_key` predicate, the\n"
        "key and mouse contexts for user keymaps, voice input and the on-screen keyboard.",
        "key handling",
        "`handle_key`, Cmd+K chords, key predicates, keymap contexts",
    ),
    "mouse_input": (
        "All mouse handling (`handle_mouse_inner`), hover popups and tooltips, and closing\n"
        "stale popups on a click.",
        "mouse handling",
        "mouse handling, hover popups and tooltips",
    ),
    "frame": (
        "Drawing each frame (`render`), the status-bar text, caret visibility and the\n"
        "screen-reader descriptions.",
        "frame drawing",
        "drawing each frame (`render`), status-bar text, caret visibility",
    ),
}

# Where a target's new files are listed in docs/ARCHITECTURE.md. `anchor` is
# the start of the row of the file being split. With no `folder`, each new
# file gets a row of its own right below the anchor, in name order (src/app/
# lists mod.rs and its siblings side by side). With a `folder` row, that row
# goes below the anchor once, and each new file is a row under it.
Tree = namedtuple("Tree", "anchor folder row width")

# A file to split: its manifest, the file, the folder its pieces go to, what
# each piece holds (as FILES above), where the docs list them, the prefix of
# its release-note fragments, and the docs and note words for its tests
# moved out (`--tests-out`): (module doc, release-note phrase, docs row).
Target = namedtuple("Target", "name manifest source dir files tree note_prefix tests more_tests",
                    defaults=({},))

APP = Target(
    "app", MANIFEST, MOD, "src/app", FILES,
    Tree("│   ├── mod.rs", None, "│   ├── ", 16), "", None,
)

# What each file split out of src/widgets/editor.rs holds, as FILES above.
EDITOR_FILES = {
    "viewers": (
        "Files the editor shows rather than edits as text: pictures and SVG, PDF pages,\n"
        "spreadsheet and SQLite grids (their edits and saves too), archives, hex, logs and gzip,\n"
        "SARIF, and the Markdown and notebook previews.",
        "editor's file viewer",
        "pictures, PDF, spreadsheets and SQLite, archives, hex, logs, Markdown and notebook previews",
    ),
    "diff_view": (
        "The diff and merge views: side-by-side and inline diffs, the three-way merge panes and\n"
        "their accept chips, merge conflicts, and a file as it was at a commit.",
        "editor's diff and merge view",
        "side-by-side and inline diffs, merge panes, conflicts, a file at a commit",
    ),
    "span_edits": (
        "Edits that arrive from elsewhere: language servers, formatters and peers, applied as\n"
        "text spans that keep the caret, the view and the undo history in step.",
        "editor's span edit",
        "edits from language servers, formatters and peers, applied as text spans",
    ),
    "undo": (
        "Undo and redo: snapshots, undo groups and coalescing, the stack limits, and the history\n"
        "a closed tab keeps for when its file is opened again.",
        "editor's undo history",
        "undo and redo, undo groups, history kept for a reopened file",
    ),
    "disk": (
        "Opening, reloading and saving a file: reading and decoding it, line endings and\n"
        "encodings, outside changes and conflicts, write protection, and the cleanups on save.",
        "editor's open and save",
        "opening, reloading and saving: encodings, line endings, outside changes, save cleanups",
    ),
    "gutter": (
        "The gutter and its marks: breakpoints, bookmarks, git change marks and blame, the\n"
        "overview ruler, line numbers, and keeping marks on their lines as text moves.",
        "editor gutter",
        "breakpoints, bookmarks, git marks and blame, overview ruler, line numbers",
    ),
    "wrap": (
        "Soft wrap and comment boxes: the rows each line takes on screen, and drawing and\n"
        "clicking the comment boxes shown between lines.",
        "editor's soft wrap and comment box",
        "soft wrap rows, comment boxes between lines",
    ),
    "highlighting": (
        "Syntax colouring: highlight passes on and off the UI thread, carrying colours across an\n"
        "edit, bracket-pair colours and colour swatches.",
        "editor's syntax colouring",
        "syntax highlight passes, colours carried across edits, bracket colours, swatches",
    ),
    "lsp_overlays": (
        "What language servers report, laid over the text: semantic tokens, inlay hints, links,\n"
        "colours, occurrences, diagnostics, inline values, code lenses and ghost text.",
        "editor's language-server overlay",
        "semantic tokens, inlay hints, diagnostics, links, code lenses, ghost text",
    ),
    "typing": (
        "Typing and deleting: inserting and pasting text, overwrite mode, auto-closed pairs,\n"
        "quotes and tags, and what completion needs to know about the caret.",
        "editor's typing",
        "typing, pasting and deleting, auto-closed pairs, quotes and tags",
    ),
    "on_enter": (
        "What Enter does: auto-indent, continuing comments and lists, the per-language Enter\n"
        "rules (Markdown, YAML and others), and re-indenting a paste.",
        "editor's Enter and auto-indent",
        "Enter, auto-indent, comment continuation, per-language Enter rules, paste re-indent",
    ),
    "snippets": (
        "Snippets and their tab stops, and Emmet: expanding abbreviations, wrapping, and editing\n"
        "the tag at the caret.",
        "editor's snippet and Emmet",
        "snippets and tab stops, Emmet",
    ),
    "multi_cursor": (
        "Several carets at once: adding carets, selecting occurrences, box and column\n"
        "selection, and applying an edit at every caret.",
        "editor's multi-cursor",
        "multiple carets, occurrence selection, box and column selection",
    ),
    "selection": (
        "Selections: making and clearing them, the selected text, linked editing, and expanding\n"
        "or shrinking a selection by syntax.",
        "editor's selection",
        "selections, linked editing, expand and shrink selection",
    ),
    "folding": (
        "Folding and indent guides: fold ranges from indentation, language servers and #region\n"
        "markers, hidden lines, and the guides drawn at each indent.",
        "editor's folding",
        "fold ranges and hidden lines, #region markers, indent guides",
    ),
    "line_commands": (
        "Whole-line commands: move, duplicate, delete, join and sort lines, indent and dedent,\n"
        "trim whitespace, replace in place, and converting indentation.",
        "editor's line command",
        "move, duplicate, delete, join, sort and indent lines, convert indentation",
    ),
    "comments": (
        "Comment toggling: line and block comments for each language, JSX comments, and the\n"
        "comment tokens each language uses.",
        "editor's comment toggling",
        "toggle line and block comments, per-language comment tokens",
    ),
    "transforms": (
        "Text transforms: changing case, bumping the number at the caret, and transposing\n"
        "characters.",
        "editor's text transform",
        "change case, bump numbers, transpose characters",
    ),
    "navigation": (
        "Moving the caret and the view: arrows, words, Home and End, pages, Go to Line,\n"
        "scrolling, the minimap and the scrollbar.",
        "editor's caret movement and scrolling",
        "caret movement, scrolling, Go to Line, minimap",
    ),
    "symbol_tabs": (
        "Symbol tabs and sibling tabs of one file: keeping a symbol tab on its symbol, and\n"
        "mirroring a sibling's text, unsaved state and caret.",
        "editor's symbol tab",
        "symbol tabs, sibling tabs of one file kept in step",
    ),
    "brackets": (
        "Brackets: matching pairs, jumping and selecting to the matching bracket, and\n"
        "highlighting the pair at the caret.",
        "editor's bracket matching",
        "bracket matching, jump and select to the matching bracket",
    ),
    "mouse": (
        "The mouse in the editor: clicks, double and triple clicks, drags, and what lies under\n"
        "the pointer (a word, a diagnostic, a hover region).",
        "editor's mouse handling",
        "clicks, drags, and what lies under the pointer",
    ),
    "language": (
        "Language and indentation settings: picking a buffer's language, detecting and setting\n"
        "its indentation, whitespace display and rulers.",
        "editor's language and indentation setting",
        "language picking, indentation detection, whitespace display, rulers",
    ),
    "render": (
        "Drawing the editor: each row's text, colours and overlays, the caret, selections and\n"
        "search matches, diagnostics, and how wide each character is drawn.",
        "editor drawing",
        "drawing rows, caret, selections, search matches and diagnostics; character widths",
    ),
    "tabs": (
        "The editor's tabs: opening, closing, pinning and moving them, previews and diff tabs,\n"
        "the tab strip and its labels, and the breadcrumbs.",
        "editor tab",
        "editor tabs: open, close, pin, move, previews, tab strip, breadcrumbs",
    ),
}

EDITOR = Target(
    "editor", "scripts/split_editor_manifest.tsv", "src/widgets/editor.rs",
    "src/widgets/editor", EDITOR_FILES,
    Tree(
        "    ├── editor.rs ",
        "    ├── editor/               the editor's feature files, beside editor.rs; each "
        "file's `//!` doc says what it holds",
        "    │   ├── ", 16,
    ),
    "editor-",
    (
        "The editor's own tests, moved out of editor.rs (#2062).",
        "the editor's tests",
        "the editor's own tests",
    ),
)

# What each file split out of src/lsp/manager.rs holds, as FILES above.
LSP_FILES = {
    "discovery": (
        "Finding each file's server: its language and project root, the server's config, and\n"
        "the executable on PATH, in a toolchain's folders or through rustup, and the note shown\n"
        "once when a server is missing.",
        "language-server manager's server discovery",
        "a file's language and project root, server config, finding the executable, rustup",
    ),
    "capabilities": (
        "What croft tells a server it can do: the client capabilities sent at initialize, and\n"
        "the standard semantic token types and modifiers it declares.",
        "language-server manager's client capabilities",
        "the client capabilities sent at initialize, standard semantic token types",
    ),
    "diagnostics": (
        "Diagnostics: the problems servers report, pushed or pulled per document and for the\n"
        "whole workspace, with their result ids, debounce and timeouts.",
        "language-server manager's diagnostics",
        "diagnostics pushed and pulled, per document and workspace-wide",
    ),
    "completion": (
        "Completion and its kin: completion lists and resolving an item, trigger characters,\n"
        "signature help, and inline (ghost text) completion.",
        "language-server manager's completion",
        "completion, trigger characters, signature help, inline completion",
    ),
    "hover": (
        "Hover, and rust-analyzer's own requests (expand macro, parent module and others).",
        "language-server manager's hover",
        "hover, rust-analyzer's own requests",
    ),
    "navigation": (
        "Going to code: definition, declaration, type definition, implementation, references,\n"
        "and the call and type hierarchies.",
        "language-server manager's go-to navigation",
        "definition, declaration, type definition, implementation, references, hierarchies",
    ),
    "symbols": (
        "Symbols: a document's outline, and workspace symbol search.",
        "language-server manager's symbols",
        "document outline, workspace symbol search",
    ),
    "semantic_tokens": (
        "Semantic tokens: whole-document and range requests, the server's legend, and turning\n"
        "its token stream into the editor's batches.",
        "language-server manager's semantic tokens",
        "semantic tokens, whole and by range, the server's legend",
    ),
    "decorations": (
        "What servers lay over the text besides tokens: inlay hints, document links, code\n"
        "lenses, folding ranges, and document colours and their presentations.",
        "language-server manager's text decorations",
        "inlay hints, links, code lenses, folding ranges, document colours",
    ),
    "text_ranges": (
        "Ranges a server reports around the caret: occurrences of the symbol, linked editing\n"
        "ranges, and the selection ranges that expand and shrink a selection.",
        "language-server manager's text range",
        "symbol occurrences, linked editing ranges, selection ranges",
    ),
    "rename": (
        "Renaming: rename and prepare rename, and the edits servers make when files are renamed.",
        "language-server manager's rename",
        "rename, prepare rename, file renames",
    ),
    "code_actions": (
        "Code actions: asking for them with the diagnostics at hand, resolving one, and running\n"
        "a server command.",
        "language-server manager's code action",
        "code actions, resolving them, server commands",
    ),
    "formatting": (
        "Formatting: a whole document, a range, and as you type.",
        "language-server manager's formatting",
        "formatting a document, a range, and on type",
    ),
    "documents": (
        "Keeping servers in step with open documents: open, change, save and close, the text\n"
        "each server last saw, and the files the workspace watcher reports changed.",
        "language-server manager's document sync",
        "open, change, save and close notifications, watched files",
    ),
    "servers": (
        "Running the servers: starting the ones a file needs, pre-warming the workspace,\n"
        "restarting a server that died, swapping in a new registry, and which server serves a\n"
        "document.",
        "language-server manager's server lifecycle",
        "starting, pre-warming, restarting and replacing servers; which server serves a file",
    ),
    "worker": (
        "The worker loop: the manager's thread that takes each command and routes it to the\n"
        "servers.",
        "language-server manager's worker loop",
        "the worker loop that routes each command to the servers",
    ),
}

LSP = Target(
    "lsp", "scripts/split_lsp_manifest.tsv", "src/lsp/manager.rs",
    "src/lsp/manager", LSP_FILES,
    Tree(
        "│   ├── manager.rs ",
        "│   ├── manager/              the language-server manager's feature files, beside "
        "manager.rs; each file's `//!` doc says what it holds",
        "│   │   ├── ", 20,
    ),
    "lsp-",
    (
        "The language-server manager's own tests, moved out of manager.rs (#2062).",
        "the language-server manager's tests",
        "the language-server manager's own tests",
    ),
)

# What each file split out of src/widgets/terminal.rs holds, as FILES above.
TERMINAL_FILES = {
    "mouse": (
        "Mouse reporting: whether the program in the pane asked for mouse events, a click, drag or\n"
        "wheel encoded as the report it expects, and which grid cell a screen point falls on.",
        "terminal pane's mouse reporting",
        "mouse reporting to the program in the pane; which grid cell a screen point is",
    ),
    "selection": (
        "Selecting text: making, extending and clearing a selection, words and blocks, keeping it\n"
        "on its text as output scrolls, alt-screen selections, the copy cursor, and the text a\n"
        "selection holds.",
        "terminal pane's selection",
        "selections: make, extend, clear, words and blocks, keep them on their text as output "
        "scrolls; the selected text",
    ),
    "clock": (
        "The scroll clock and row times: how far output has scrolled, which keeps selections and\n"
        "notes on their text, and the time each row was printed.",
        "terminal pane's scroll clock",
        "the scroll clock that keeps selections and notes in place; each row's print time",
    ),
    "captures": (
        "What a clear wipes away: spotting a program clearing the screen, keeping the rows it\n"
        "cleared, finding a captured line again, and the rewind buffer.",
        "terminal pane's captured rows",
        "spotting screen clears, keeping the rows they wipe, the rewind buffer",
    ),
    "annotations": (
        "Notes pinned to output, and the text under a masked secret for click-to-reveal.",
        "terminal pane's note",
        "notes pinned to output; revealing a masked secret",
    ),
    "commands": (
        "Shell integration: the prompt and command marks the shell reports, finished commands and\n"
        "their decorations, a command's input and output text, jumping between prompts, and the\n"
        "prompt line as typed.",
        "terminal pane's shell integration",
        "shell-integration marks, finished commands and their decorations, prompt jumps, the "
        "typed prompt line",
    ),
    "shell_env": (
        "The environment a pane's shell starts in: shell integration for each shell, the terminal\n"
        "and locale variables, and croft's helpers on PATH.",
        "terminal pane's shell environment",
        "the shell's start-up environment: integration for each shell, terminal and locale "
        "variables, croft's helpers on PATH",
    ),
    "processes": (
        "The pane's processes: the shell and the foreground process, their names, command lines\n"
        "and working directories, and changing directory.",
        "terminal pane's process",
        "the shell and foreground process: names, command lines, working directories, `cd`",
    ),
    "spawn": (
        "Starting a pane: spawning the shell or a program, attaching to a hosted pane, the reader\n"
        "thread that parses its output, and replaying a saved transcript.",
        "terminal pane's start-up",
        "starting a pane: spawn or attach, the output reader thread, replaying a saved "
        "transcript",
    ),
    "backend": (
        "The process behind a pane: the local pty or the session host, waiting on its output,\n"
        "resizing it, noticing it exit, and shutting it down.",
        "terminal pane's process link",
        "the local pty or session host behind a pane: output waits, resize, exit, shutdown",
    ),
    "text": (
        "Reading the grid as text: visible and wrapped lines, logical rows across soft wraps, find\n"
        "lines, and the screen as ANSI with its colours.",
        "terminal pane's grid text",
        "the grid as text: visible and wrapped lines, logical rows, find lines, ANSI with colours",
    ),
    "scroll": (
        "Scrolling and resizing: the scrollback offset, scrolling to a line, clearing the screen\n"
        "and scrollback, and reflowing the grid on resize.",
        "terminal pane's scrolling",
        "scrolling, clearing the scrollback, reflowing the grid on resize",
    ),
    "input": (
        "Writing to the pane: typed input, pastes and bracketed paste, and the clipboard\n"
        "sequences.",
        "terminal pane's input",
        "typed input, pastes, bracketed paste, clipboard sequences",
    ),
    "images": (
        "Pictures a program prints into the pane: keeping them and placing them on the grid.",
        "terminal pane's image",
        "pictures printed into the pane and where they sit",
    ),
    "overlays": (
        "What is painted over the text: find matches, quick-select hints, trigger highlights,\n"
        "problem-matcher watches, links and the colour palette.",
        "terminal pane's overlay",
        "find matches, quick-select hints, triggers, problem watches, links, the palette",
    ),
    "status": (
        "The pane's state the app reads each frame: its label and name, the agent in it, its\n"
        "reported status and progress, bells, notifications and ports, and whether it is quiet\n"
        "or untouched.",
        "terminal pane's status",
        "label and name, agent, status and progress, bells, notifications, ports, quiet and "
        "untouched",
    ),
    "render": (
        "Drawing the pane: each cell's text and colours, selections, overlays, decorations and\n"
        "notes.",
        "terminal pane's drawing",
        "drawing each cell, selections, overlays, decorations and notes",
    ),
}

TERMINAL = Target(
    "terminal", "scripts/split_terminal_manifest.tsv", "src/widgets/terminal.rs",
    "src/widgets/terminal", TERMINAL_FILES,
    Tree(
        "    ├── terminal.rs ",
        "    ├── terminal/             the terminal pane's feature files, beside terminal.rs; "
        "each file's `//!` doc says what it holds",
        "    │   ├── ", 16,
    ),
    "terminal-",
    (
        "The terminal pane's own tests, moved out of terminal.rs (#2062).",
        "the terminal pane's tests",
        "the terminal pane's own tests",
    ),
)

# What each file split out of src/git.rs holds, as FILES above.
GIT_FILES = {
    "operations": (
        "A merge, rebase, cherry-pick, revert or bisect in progress: which one it is, the branch a\n"
        "rebase is rewriting, and continuing, skipping or aborting it.",
        "git in-progress operation",
        "a merge, rebase, cherry-pick, revert or bisect in progress; continuing, skipping or "
        "aborting it",
    ),
    "status": (
        "The repository's status: branch, ahead and behind, whether anything changed, ignored\n"
        "paths, the commit message a merge or squash prepared, and the repositories one folder\n"
        "down.",
        "git status",
        "the status snapshot: branch, ahead/behind, changes, ignored paths, the prepared commit "
        "message, repositories one folder down",
    ),
    "changes": (
        "The changed files: `git status --porcelain` parsed into entries, each change's kind,\n"
        "section and badge, the Explorer's decorations, and each file's added and removed line\n"
        "counts.",
        "git changes list",
        "changed files from `git status --porcelain`: kind, section and badge; Explorer "
        "decorations; line counts",
    ),
    "contents": (
        "A file's contents at HEAD, at a commit or at an index stage, a conflict's three stages\n"
        "merged again in diff3 style, and the clean/smudge filter (Git LFS) a path goes through.",
        "git file contents",
        "a file at HEAD, a commit or an index stage; a conflict re-merged in diff3 style; a "
        "path's clean/smudge filter",
    ),
    "patches": (
        "Applying a hunk or line patch to the index or the working tree, keeping a file's CRLF\n"
        "line endings as they are.",
        "git patch",
        "applying a hunk or line patch to the index or working tree, CRLF endings kept",
    ),
    "messages": (
        "What git said, in one line: the headline and summary of a command's output, the result\n"
        "of a merge, pull or push, conflict summaries, errors, and refusals to switch branches.",
        "git output summary",
        "one-line summaries of git's output: results, conflicts, errors and refusals",
    ),
    "diffs": (
        "Diffs: what is staged, against a branch or where it forked, against the previous\n"
        "commit, and what changed in a submodule.",
        "git diff",
        "staged diffs; against a branch, its merge base or the previous commit; submodule diffs",
    ),
    "history": (
        "History: a file's commits across renames, the commit graph, a branch's commits, a\n"
        "commit's files and diffs, its message and hover card, and how long ago it was.",
        "git history",
        "a file's history across renames, the commit graph, a branch's commits, a commit's "
        "files, diffs, message and card",
    ),
    "blame": (
        "Blame: which commit last changed each line of a file.",
        "git blame",
        "which commit last changed each line of a file",
    ),
    "worktrees": (
        "Worktrees: a lane's branch in a sibling folder of its own, checking a pull request out\n"
        "into a worktree, and the reasons a worktree is kept rather than removed.",
        "git worktree",
        "lanes and pull-request checkouts in worktrees of their own, and when one is kept",
    ),
    "staging": (
        "Staging, unstaging and removing paths, and discarding their changes, one at a time or\n"
        "all at once.",
        "git staging",
        "stage, unstage, remove and discard paths, one or all",
    ),
    "commits": (
        "Committing: refusing unmerged paths and leftover conflict markers, committing and\n"
        "amending, undoing the last commit, and the commit graph's actions (cherry-pick, revert,\n"
        "checkout, tag, reset).",
        "git commit",
        "commit and amend with their refusals, undo the last commit, the commit graph's "
        "cherry-pick, revert, checkout, tag and reset",
    ),
    "branches": (
        "Branches: the default branch, local and remote branches listed, and switching to,\n"
        "tracking, creating, renaming, deleting, merging and rebasing them.",
        "git branch",
        "the default branch; list, switch, track, create, rename, delete, merge and rebase "
        "branches",
    ),
    "remotes": (
        "Remotes: pulling, pushing and publishing, fetching and cloning, adding and removing\n"
        "remotes, and a link to a file on the remote's website.",
        "git remote",
        "pull, push, publish, fetch and clone; add and remove remotes; a file's web link",
    ),
    "stash": (
        "The stash: stashing (everything, with untracked files, or only what is staged), and\n"
        "listing, showing, applying, popping and dropping entries.",
        "git stash",
        "stash, list, show, apply, pop and drop stashes",
    ),
    "tags": (
        "Tags: listing, creating and deleting them, here and on a remote, and pushing them.",
        "git tag",
        "list, create, delete and push tags, here and on a remote",
    ),
    "worker": (
        "The background git worker: the requests the app posts to it, how a burst of them\n"
        "coalesces into one, and the answers it sends back.",
        "git worker",
        "the background git worker: requests, coalescing and answers",
    ),
}

GIT = Target(
    "git", "scripts/split_git_manifest.tsv", "src/git.rs", "src/git", GIT_FILES,
    Tree(
        "├── git.rs ",
        "├── git/                  git's feature files, beside git.rs; each file's `//!` doc "
        "says what it holds",
        "│   ├── ", 17,
    ),
    "git-",
    (
        "git.rs's own tests, moved out of git.rs (#2062).",
        "git's tests",
        "git.rs's own tests",
    ),
)

# remote.rs's feature files (#2062): (module doc, release-note phrase, docs row).
REMOTE_FILES = {
    "hosts": (
        "The SSH hosts croft knows: the targets in ~/.ssh/config and the state of each entry,\n"
        "and the host a running ssh is connected to, read from its argv with ssh's own flag\n"
        "grammar.",
        "remote host",
        "the hosts in ~/.ssh/config, and the host a running ssh's argv names",
    ),
    "offer": (
        "The ssh-pane offer: which host and folder a pane's ssh is in, whether to offer to\n"
        "re-root there, and the hosts whose install failed, remembered for a week.",
        "remote ssh-pane offer",
        "the ssh-pane offer: a pane's host and folder, whether to offer, hosts whose install "
        "failed",
    ),
    "control": (
        "The SSH control master a remote session runs over: starting or adopting it, the\n"
        "commands sent through its socket, and the socket's private directory.",
        "remote SSH control master",
        "the SSH control master: starting or adopting it, commands over its socket",
    ),
    "relay": (
        "The drop relay: the pump that tails a remote croft's request log and runs each\n"
        "request on this machine, the requests it understands, and how each is claimed and\n"
        "answered.",
        "remote drop relay",
        "the drop relay's pump: reading, claiming and answering a remote croft's requests",
    ),
    "clipboard": (
        "The relay's clipboard requests: this machine's clipboard for a remote paste, and a\n"
        "remote croft's copied text put on this machine's clipboard.",
        "remote clipboard relay",
        "the relay's clipboard requests: pasting from and copying to this machine's clipboard",
    ),
    "pull": (
        "The relay's pull request: which remote paths may be pulled, and copying them to this\n"
        "machine.",
        "remote pull relay",
        "the relay's pull request: copying remote files to this machine",
    ),
    "forward": (
        "The relay's open, forward and unforward requests: opening a link in this machine's\n"
        "browser, and forwarding a remote port to a free local one.",
        "remote open and port forward relay",
        "the relay's open and forward requests: links in the local browser, forwarded ports",
    ),
    "session": (
        "A remote croft session: launching it, its outcome, and keeping it alive across a\n"
        "dropped connection by reconnecting the master and handing the terminal back.",
        "remote session",
        "launching a remote croft session, its outcome, and reconnecting after a drop",
    ),
    "launch_command": (
        "The command line that starts croft on the remote, and the session, mux and collab\n"
        "sockets it names.",
        "remote launch command",
        "the command line that starts croft on the remote, and the sockets it names",
    ),
    "install_stream": (
        "The install as the connect dialog runs it, each step's output streamed to the\n"
        "dialog and kept in the install log, and the background install beside an attached\n"
        "session.",
        "streamed remote install",
        "the install the connect dialog runs, its output streamed and logged; the background "
        "install",
    ),
    "install": (
        "Installing croft on the remote: whether it needs an install or an update, the release\n"
        "binary when this croft is one, the remote build when nothing faster can run, and\n"
        "swapping the new binary in.",
        "remote install",
        "whether the remote needs croft, the release binary, the remote build, swapping the "
        "binary in",
    ),
    "cross_build": (
        "The local cross-build: whether a static croft for the remote can be built here,\n"
        "finding the toolchain from the checkout, building it and shipping it.",
        "remote cross-build",
        "building a static croft for the remote here: toolchain probes, the build, shipping it",
    ),
    "source": (
        "This croft's source: the stamp that says whether the remote is up to date, and\n"
        "syncing the source to the remote by allow-list.",
        "remote source sync",
        "the source stamp, and syncing the source to the remote by allow-list",
    ),
    "config_push": (
        "Pushing the user's config to the remote: the files that sync, how each is shipped\n"
        "and logged, and `croft sync-config` with its diff.",
        "remote config push",
        "pushing the user's config to the remote; `croft sync-config` and its diff",
    ),
    "config_repush": (
        "The live config re-push: watching the synced files during a session and shipping\n"
        "each change over the bulk lane.",
        "remote config re-push",
        "re-pushing changed config during a session over the bulk lane",
    ),
}

REMOTE = Target(
    "remote", "scripts/split_remote_manifest.tsv", "src/remote.rs", "src/remote", REMOTE_FILES,
    Tree(
        "├── remote.rs ",
        "├── remote/               remote's feature files, beside remote.rs; each file's `//!` doc "
        "says what it holds",
        "│   ├── ", 17,
    ),
    "remote-",
    (
        "remote.rs's own tests, moved out of remote.rs (#2062).",
        "remote's tests",
        "remote.rs's own tests",
    ),
    {
        "offer_tests": (
            "The ssh-pane offer's tests, moved out of remote.rs (#2062).",
            "the ssh-pane offer's tests",
            "the ssh-pane offer's tests",
        ),
    },
)

# The search panel's feature files (#2062): (module doc, release-note phrase, docs row).
SEARCH_FILES = {
    "matcher": (
        "How a query matches text: the matcher built from the query and its Aa / ab / .*\n"
        "toggles, the pattern the editor's Find bar shares, and the error an invalid query shows.",
        "search query matcher",
        "the query's matcher and its toggles, the pattern the Find bar shares, an invalid "
        "query's error",
    ),
    "path_filter": (
        "The files to include and to exclude: their comma-separated globs, compiled as VS Code\n"
        "reads them (a bare pattern matches at any depth, a folder's name admits what is in\n"
        "it), and the test a path must pass.",
        "search include and exclude filter",
        "the files to include / exclude globs, and the test a path must pass",
    ),
    "walk": (
        "The search itself: walking the workspace in parallel, honouring .gitignore and leaving\n"
        "out binaries and version-control stores, streaming each file's hits, and searching an\n"
        "unsaved buffer's text the same way.",
        "workspace search walk",
        "walking the workspace and streaming each file's hits; searching an unsaved buffer",
    ),
    "highlight": (
        "Where a hit's line splits so its matches are highlighted, for a plain query and for a\n"
        "regex.",
        "search match highlighting",
        "splitting a hit's line around its matches, for highlighting",
    ),
    "worker": (
        "The background search worker: the requests the panel sends it, the events it answers\n"
        "with, and the pause in typing it waits for before a search starts.",
        "search worker",
        "the background search worker, its requests and events, and the typing pause",
    ),
    "replace": (
        "Replace: a replacement's $1 and ${name} references and Preserve Case, replacing in a\n"
        "line, a text or a file, and the panel's Replace All on disk.",
        "search replace",
        "replacement text, replacing in a line, a text or a file, and Replace All",
    ),
    "fields": (
        "The panel's input fields: the caret and selection in each, typing and deleting, which\n"
        "field has focus, the Replace and details rows shown or hidden, and the include and\n"
        "exclude filter the fields make.",
        "search panel input field",
        "the input fields: caret, selection, typing, focus, the Replace and details rows",
    ),
    "hit_test": (
        "What a click lands on: the toggles, the buttons beside the fields, the fields\n"
        "themselves, a result row and its path, and the tooltip under the pointer.",
        "search panel hit-testing",
        "what a click or the pointer lands on: toggles, buttons, fields, result rows",
    ),
    "results": (
        "The results list: the worker's hits added in order, the counts in the header, the\n"
        "query's error, moving the selection and scrolling, and the mode toggles.",
        "search results list",
        "the results list: hits in order, counts, selection, scrolling, the mode toggles",
    ),
    "render": (
        "Drawing the panel: each input field with its fill, caret and glob caption, and the\n"
        "whole panel, results included.",
        "search panel drawing",
        "drawing the panel and each input field",
    ),
}

SEARCH = Target(
    "search", "scripts/split_search_manifest.tsv", "src/widgets/search.rs",
    "src/widgets/search", SEARCH_FILES,
    Tree(
        "    ├── search.rs ",
        "    ├── search/          the search panel's feature files, beside search.rs; each "
        "file's `//!` doc says what it holds",
        "    │   ├── ", 16,
    ),
    "search-",
    (
        "The search panel's own tests, moved out of search.rs (#2062).",
        "the search panel's tests",
        "the search panel's own tests",
    ),
)

# The session host's feature files (#2062): (module doc, release-note phrase, docs row).
SESSION_HOST_FILES = {
    "protocol": (
        "The wire format between the host and its clients: the frame tags, the control messages\n"
        "and the roster entry they carry, encoding a frame, and the decoder that turns socket\n"
        "chunks back into frames.",
        "session host wire format",
        "the wire format: frame tags, control messages, encoding and decoding frames",
    ),
    "roster": (
        "What the roster says: the notice for clients older than this croft, the version\n"
        "clients are measured against, the one client a Session: Detach can come from, and the\n"
        "PTY size every client can show.",
        "session roster",
        "what the roster says: stale clients, the session's version, the detach target, the "
        "shared size",
    ),
    "sidecars": (
        "The files the host keeps beside its socket for the inner croft: the presence roster,\n"
        "the stale-binary marker and the host's version, with their paths and readers.",
        "session host sidecar",
        "the files beside the socket: the presence roster, the stale-binary marker, the "
        "host's version",
    ),
    "identity": (
        "Who a client is: the user@host name the roster shows, and the stable identity that\n"
        "lets a reconnect displace its own earlier registration.",
        "session client identity",
        "a client's roster name and its stable identity across reconnects",
    ),
    "frame_write": (
        "Writing one frame to a socket within a deadline, so a stopped or wedged peer can never\n"
        "hang the writer.",
        "session frame write",
        "writing one frame to a socket within a deadline",
    ),
    "outbox": (
        "A client's outbound queue: queueing frames without ever blocking on the socket,\n"
        "counting what was delivered, closing once drained, and the writer thread that drains\n"
        "it.",
        "session client outbox",
        "each client's outbound queue, its delivery count, and the writer thread that drains "
        "it",
    ),
    "pty": (
        "The PTY master and the inner command as the host holds them, whether it spawned them\n"
        "itself or inherited them across a host swap: resizing the master and waiting for the\n"
        "child.",
        "session host PTY handle",
        "the PTY master and the inner command, held fresh or inherited: resize and wait",
    ),
    "swap": (
        "Moving a live session onto an updated binary (#238): spotting the replaced image,\n"
        "handing the socket, PTY and child across an exec, adopting them in the successor, and\n"
        "the fence that lets clients reconnect.",
        "session host swap",
        "moving a live session onto an updated binary: the watch, the handoff, the adoption, "
        "the fence",
    ),
    "server": (
        "The host proper: starting a session, pumping PTY output to every client, keeping the\n"
        "PTY at the smallest client size, and publishing the roster.",
        "session host server",
        "the host proper: starting a session, broadcasting output, the shared PTY size, the "
        "roster",
    ),
    "connection": (
        "One client's connection: its thread, whose typing reaches the PTY, granting and\n"
        "revoking write control and the rules for it, and disconnecting a participant.",
        "session client connection",
        "one client's connection: its thread, typing attribution, write control, kicks",
    ),
    "launch": (
        "Attaching to a session or starting one first: the detached server's command line and\n"
        "its spawn, and ending a stale server.",
        "session launch",
        "attach-or-create: spawning the detached server, ending a stale one",
    ),
    "detach_chord": (
        "Spotting Cmd+K Shift+Q (Session: Detach) in a client's own keystrokes, so any client\n"
        "can leave, one without write control included.",
        "session detach chord",
        "spotting the detach chord in a client's own keystrokes",
    ),
    "attach": (
        "The attaching client: raw mode, pumping stdin to the socket and output to the\n"
        "terminal, the version-mismatch banner, reconnecting across a host swap, and handing\n"
        "the terminal back on exit.",
        "session attach client",
        "the attaching client: the pump, the version banner, reconnects across a swap, the "
        "exit",
    ),
    "inner_channel": (
        "The inner croft's privileged channel to its host: who is typing, granting and revoking\n"
        "write control, and disconnecting clients.",
        "session inner channel",
        "the inner croft's privileged channel: typing, write control, kicks",
    ),
}

SESSION_HOST = Target(
    "session_host", "scripts/split_session_host_manifest.tsv", "src/session_host.rs",
    "src/session_host", SESSION_HOST_FILES,
    Tree(
        "├── session_host.rs ",
        "├── session_host/         the session host's feature files, beside session_host.rs; "
        "each file's `//!` doc says what it holds",
        "│   ├── ", 17,
    ),
    "session-host-",
    (
        "The session host's own tests, moved out of session_host.rs (#2062).",
        "the session host's tests",
        "the session host's own tests",
    ),
)

PAIR_FILES = {
    "fence": (
        "The fenced edit protocol's parser: the fence markers, the events the machine resolves\n"
        "from streamed text, the machine itself and its header parsers.",
        "pair fence parser",
        "the fence machine: the `<<<EDIT>>>` / `<<<NOTE>>>` markers, the events it "
        "resolves from streamed text, and its header parsers",
    ),
    "offsets": (
        "Byte offsets into a file's replica text: a fence's range and a note's row as offsets,\n"
        "and keeping a tracked offset true through other participants' edits.",
        "pair byte offset",
        "byte offsets into the replica text: a fence range or a note row as offsets, kept "
        "true through remote edits",
    ),
    "provider": (
        "Which model the navigator talks to and what it is taught: the provider (the claude CLI\n"
        "or a local endpoint), the config `croft pair` sits down with, and the system prompt\n"
        "each provider's model gets.",
        "pair provider",
        "the provider (the claude CLI or a local endpoint), `PairConfig`, and the system "
        "prompt each model is taught",
    ),
    "state": (
        "The pilot state's turn books: a fresh seat, a file's live lines, whether a turn is\n"
        "running, arming a turn and undoing a staged one, what the navigator last saw of a file,\n"
        "and the bounded lock the App's threads take on the state.",
        "pair turn state",
        "the turn books: arming a turn and undoing a staged one, what the navigator last "
        "saw, and the bounded lock the App's threads take",
    ),
    "notes": (
        "The navigator's anchored notes: listing them, dropping one or all, replying in one, and\n"
        "landing one from the app side.",
        "pair note",
        "the navigator's anchored notes: listing, dropping and replying to them, and "
        "landing one from the app side",
    ),
    "caret": (
        "The navigator's visible caret: parking it at a row until the file goes live, moving it\n"
        "with an edit or a note, and this seat's site ids.",
        "pair caret",
        "the navigator's caret: parking it at a row until the file is live, and this "
        "seat's site ids",
    ),
    "voice": (
        "Where the pilot's words go: the model's prose and the pilot's diagnostics, to the\n"
        "in-process host when seated inside croft, else the REPL's stdout and stderr.",
        "pair voice",
        "the pilot's voice and diagnostics: to the in-process host inside croft, else "
        "stdout and stderr",
    ),
    "claude_cli": (
        "The claude CLI: the tools and settings it may use, finding it on a stripped GUI PATH,\n"
        "the command that launches it, reading its stream-json events, and the scripted fake\n"
        "claude the tests drive.",
        "pair claude CLI",
        "the claude CLI: its sandbox, finding it on a GUI PATH, its launch command, its "
        "stream-json events, and the tests' fake claude",
    ),
    "seat": (
        "Seating the pilot: connecting the collab seat, the claude and local-endpoint seats and\n"
        "their threads, what a turn's end reports, and tearing a seat down.",
        "pair seat",
        "seating the pilot: the claude and local-endpoint seats, their threads, and "
        "teardown",
    ),
    "apply": (
        "Applying the stream to the collab seat: waiting for a file to go live, applying each\n"
        "fence event, opening and reverting the streamed region, and the pump tick that keeps it\n"
        "true against the relay.",
        "pair fence apply",
        "applying fence events to the collab seat: the bootstrap wait, the streamed "
        "region and its revert, and the pump tick",
    ),
    "turns": (
        "User turns: where a turn goes (the claude child's stdin or the local worker), composing\n"
        "the task, ask, yield and reply turns, and sending one with any pending cancel note.",
        "pair user turn",
        "user turns: the sink, composing task, ask, yield and reply turns, and sending "
        "one",
    ),
    "repl": (
        "`croft pair --repl`: the debug REPL driver, joining the relay and running until stdin\n"
        "closes.",
        "pair REPL",
        "`croft pair --repl`, the debug REPL driver",
    ),
}

PAIR = Target(
    "pair", "scripts/split_pair_manifest.tsv", "src/pair.rs", "src/pair", PAIR_FILES,
    Tree(
        "├── pair.rs ",
        "├── pair/                 the pilot's feature files, beside pair.rs; "
        "each file's `//!` doc says what it holds",
        "│   ├── ", 17,
    ),
    "pair-",
    (
        "The pilot's own tests, moved out of pair.rs (#2062).",
        "the pilot's tests",
        "the pilot's own tests",
    ),
)

TARGETS = {
    t.name: t
    for t in (APP, EDITOR, LSP, TERMINAL, GIT, REMOTE, SEARCH, SESSION_HOST, PAIR)
}

# Kinds whose declaration can carry a visibility.
VISIBLE = {"fn", "const", "static", "struct", "enum", "union", "trait", "type"}
# `impl Trait for Type`: the methods of a trait impl take no visibility. `for<`
# is a higher-ranked bound, which an inherent impl can have too.
TRAIT_IMPL = re.compile(r"\bfor\s+(?!<)")


class ManifestError(Exception):
    pass


class BuildError(Exception):
    pass


PRIVATE_IMPORT = re.compile(r"^[a-z]+(?: [a-z]+)* import `(\w+)` is private$")
UNUSED_GLOB = re.compile(r"^unused import: `(\w+)::\*`$")
UNUSED_ONE = re.compile(r"^unused import: `(\w+)::(\w+)`$")


def item_key(it):
    return (it.kind, it.ctx or "-", "::".join([*it.mods, it.name]))


def read_manifest(path):
    """[(pr, file, kind, impl, name)] in file order."""
    rows = []
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            line = line.rstrip("\n")
            if not line or line.startswith("#") or line.startswith("pr\t"):
                continue
            parts = line.split("\t")
            if len(parts) != 5:
                raise ManifestError(f"{path}:{n}: expected 5 tab-separated fields, got {len(parts)}")
            pr, file, kind, impl, name = parts
            rows.append((int(pr), file, kind, impl, name))
    return rows


def landed(root, rows, t=APP):
    """{file: Counter of item keys} for the files the manifest moves items
    to that exist already: what earlier moves put there."""
    out = {}
    for file in sorted({file for pr, file, *_ in rows if pr}):
        path = os.path.join(root, t.dir, f"{file}.rs")
        if os.path.exists(path):
            with open(path, encoding="utf-8") as f:
                out[file] = Counter(item_key(it) for it in cpm.parse(f.read(), path)[0])
    return out


def place(items, rows, moved=None):
    """Each item's (pr, file), matching manifest rows to items by kind, impl
    header and name. Every row is an item that is either still in mod.rs or
    already in the file its row names (`moved`, from `landed`), never both.
    Twins (one key, several rows) all go to one place. Raises ManifestError
    listing every item the manifest misses and every row with no item."""
    moved = moved or {}
    want = defaultdict(list)
    for pr, file, kind, impl, name in rows:
        want[(kind, impl, name)].append((pr, file))
    errors = []
    for key, places in want.items():
        if len(set(places)) > 1:
            errors.append(f"twins of {key[0]} {key[2]} in {key[1]} go to different places: {sorted(set(places))}")
    here = Counter(item_key(it) for it in items)
    out = []
    for it in items:
        key = item_key(it)
        if key not in want:
            errors.append(f"not in the manifest: {it.kind} {key[2]} in {key[1]} "
                          f"(mod.rs:{it.start + 1}); add a row for it")
        out.append(want[key][0] if key in want else None)
    for key, places in want.items():
        pr, file = places[0]
        there = moved.get(file, Counter())[key] if pr else 0
        if here[key] > len(places):
            errors.append(f"not in the manifest: {here[key] - len(places)} more {key[0]} {key[2]} in "
                          f"{key[1]} than its {len(places)} row(s); add a row for each")
        elif here[key] + there < len(places):
            errors.append(f"not in mod.rs: {key[0]} {key[2]} in {key[1]} "
                          f"({len(places) - here[key] - there} of {len(places)} row(s), and not in "
                          f"{file or 'mod'}.rs either); fix or drop its row")
        elif here[key] and there:
            errors.append(f"both in mod.rs and in {file}.rs: {key[0]} {key[2]} in {key[1]}; "
                          f"a move was cut short")
    if errors:
        raise ManifestError("\n".join(errors))
    return out


def impl_blocks(glue):
    """(header start, header end, close) of every `impl`/`mod` block, from
    the parser's glue, innermost last."""
    blocks, stack = [], []
    for g in sorted(glue, key=lambda g: g.start):
        if g.kind == "header":
            stack.append(g)
        elif g.kind == "close" and stack:
            h = stack.pop()
            blocks.append((h.start, h.end, g.start))
    return blocks


def enclosing(blocks, it):
    inner = [b for b in blocks if b[1] < it.start and b[2] > it.end]
    return max(inner, key=lambda b: b[0]) if inner else None


def header_code(lines, block):
    """The header of `block` as code: its attribute and `impl ... {` lines,
    without the docs and comments that sit above it."""
    out = []
    for ln in lines[block[0] : block[1] + 1]:
        s = ln.strip()
        if not s or s.startswith("//"):
            continue
        out.append(ln)
    return out


def emptied(picked, blocks, items):
    """The blocks the move leaves with no item in mod.rs: they go from mod.rs,
    and the docs and comments on their headers go with their last items."""
    moving = {it.start for it in picked}
    out = set()
    for block in blocks:
        inside = [it for it in items if block[1] < it.start and it.end < block[2]]
        if inside and all(it.start in moving for it in inside):
            out.add(block)
    return out


def widen(lines, it, kinds, clines, ctx):
    """`lines` of item `it` with `pub(super) ` before its declaration when it
    is private and can carry a visibility; otherwise as they are."""
    body = lines[it.start : it.end + 1]
    if it.vis or it.kind not in VISIBLE or (ctx and TRAIT_IMPL.search(ctx)):
        return body
    # Past the docs, comments and attributes, whichever lines they take, to
    # the declaration itself.
    j, col = it.start, 0
    while True:
        if kinds[j] != cpm.CODE or not clines[j][col:].strip():
            j, col = j + 1, 0
            if j > it.end:
                raise cpm.ParseError(f"line {it.start + 1}: no declaration found for {it.name}")
            continue
        if not (m := cpm.OUTER_ATTR.match(clines[j], col)):
            break
        j, col = cpm.balanced_end(clines, j, m.end() - 1, it.end + 1)
    rest = lines[j][col:]
    at = col + len(rest) - len(rest.lstrip())
    body[j - it.start] = lines[j][:at] + "pub(super) " + lines[j][at:]
    return body


def render_items(lines, picked, blocks, kinds, clines, gone=frozenset()):
    """The moved items as Rust text: free items as they are, methods under a
    copy of their impl header, consecutive methods of one block together. A
    block in `gone` leaves mod.rs with this move, so its header comes whole,
    docs and comments too; any other header comes as code only, its docs
    staying on the block in mod.rs."""
    out, current = [], None
    for it in picked:
        block = enclosing(blocks, it)
        if block != current:
            if current is not None:
                out.append("}")
            if out:
                out.append("")
            if block in gone:
                head = lines[block[0] : block[1] + 1]
                while head and not head[0].strip():
                    head = head[1:]
                out.extend(head)
            elif block is not None:
                out.extend(header_code(lines, block))
            current = block
        elif out and out[-1] != "" and not out[-1].rstrip().endswith("{"):
            out.append("")
        ctx = it.ctx if block is not None else ""
        out.extend(widen(lines, it, kinds, clines, ctx))
    if current is not None:
        out.append("}")
    return out


def cut(lines, picked, blocks, items):
    """mod.rs without the picked items, and without any impl block that has
    no item left. Where a cut joins two gaps, one blank line stays; where it
    leaves a gap at the top or bottom of a block, none does. Only blank lines
    between items are touched: one inside an item may sit in a string."""
    drop = set()
    for it in picked:
        drop.update(range(it.start, it.end + 1))
    for block in emptied(picked, blocks, items):
        drop.update(range(block[0], block[1] + 1))
        drop.add(block[2])
    inside = set()
    for it in items:
        inside.update(range(it.start, it.end + 1))
    opens = {b[1] for b in blocks}
    closes = {b[2] for b in blocks}

    def gap(n):
        return n not in inside and not lines[n].strip()

    kept = [n for n in range(len(lines)) if n not in drop]
    out = []
    for k, n in enumerate(kept):
        if gap(n):
            prev = out[-1] if out else None
            nxt = next((m for m in kept[k + 1 :] if not gap(m)), None)
            cut_here = any(m in drop for m in range((prev or 0) + 1, nxt if nxt is not None else len(lines)))
            if prev is not None and gap(prev):
                continue
            if cut_here and (prev in opens or nxt in closes):
                continue
        out.append(n)
    return [lines[n] for n in out]


def add_glue(lines, file, free):
    """`mod file;` after the last `mod x;` at the top of mod.rs and, when the
    file has free items, `use file::*;` after the `use` lines that follow. A
    file with no `mod x;` yet gets its first after the `use` declarations at
    its top, a blank line between."""
    decl = re.compile(r"^(pub(\(\w+\))? )?mod \w+;$")
    mods = [n for n, ln in enumerate(lines[:200]) if decl.match(ln)]
    if mods:
        last_mod = max(mods)
    else:
        last_mod = top_uses_end(lines)
        lines.insert(last_mod + 1, "")
        last_mod += 1
    lines.insert(last_mod + 1, f"mod {file};")
    if free:
        n = last_mod + 2
        while n < len(lines) and re.match(r"^use \w+(::\{[^}]*\}|::\w+)*;$", lines[n]):
            n += 1
        lines.insert(n, f"use {file}::*;")
    return lines


def top_uses_end(lines):
    """The last line of the `use` declarations at the top of a file, the ones
    before its first item."""
    items, glue = cpm.parse("\n".join(lines))
    first = min((it.start for it in items), default=len(lines))
    ends = [g.end for g in glue if g.kind == "use" and not g.scope and g.start < first]
    if not ends:
        raise ManifestError("no `mod` or `use` declaration at the top of the file to put `mod <file>;` after")
    return max(ends)


def note_path(file, half, t=APP):
    suffix = f"-{half}" if half else ""
    return os.path.join(NOTES, f"2062-split-{t.note_prefix}{file.replace('_', '-')}{suffix}.md")


def note_text(file, half, t=APP):
    what = t.files[file][1]
    part = {1: "the first part of the ", 2: "the rest of the "}.get(half, "the ")
    return (f"refactor: Groundwork for the refactor program (#2062): {part}{what} code "
            f"moved to its own file, {t.dir}/{file}.rs. Nothing changes in use.\n")


def add_arch_row(text, file, t=APP, desc=None):
    """docs/ARCHITECTURE.md with a row for <dir>/<file>.rs where the target's
    tree lists its files, in name order, and the folder row first when the
    target has one and it is missing. `desc` is the row's text, by default
    the file's own from the target's FILES."""
    lines = text.split("\n")
    tree = t.tree
    name = f"{file}.rs"
    start = next((n for n, ln in enumerate(lines) if ln.startswith(tree.anchor)), None)
    if start is None:
        raise ManifestError(f"{ARCH} has no row starting {tree.anchor.strip()!r} to list {name} by")
    folder = tree.folder and lines[start + 1].startswith(tree.folder.split("/")[0] + "/")
    if tree.folder:
        start += 1
    row = re.compile("^" + re.escape(tree.row) + r"(\S+)")
    # Only the target's own rows count: another folder (the editor's beside
    # the terminal's) lists its files with the same indentation.
    end = start + 1
    while end < len(lines) and row.match(lines[end]):
        end += 1
    if (folder or not tree.folder) and any(
            ln.startswith(f"{tree.row}{name} ") for ln in lines[start + 1:end]):
        return text
    if tree.folder and not folder:
        lines.insert(start, tree.folder)
        end = start + 1
    n = start + 1
    while n < end and (m := row.match(lines[n])) and m.group(1) < name \
            and not m.group(1).endswith("/"):
        n += 1
    text_ = t.files[file][2] if desc is None else desc
    lines.insert(n, f"{tree.row}{name:<{tree.width}} {text_}")
    return "\n".join(lines)


def halves(rows):
    """{pr: (file, half)} where half is 1 or 2 for a file that takes two PRs,
    0 for one that takes one."""
    prs = defaultdict(set)
    for pr, file, *_ in rows:
        if pr:
            prs[file].add(pr)
    out = {}
    for file, ns in prs.items():
        ns = sorted(ns)
        for k, pr in enumerate(ns, 1):
            out[pr] = (file, k if len(ns) > 1 else 0)
    return out


def write_all(files):
    """Write each (path, text), making a new file's folder. Called only once
    every text is worked out, so a step that fails first leaves every file as
    it was."""
    for path, text in files:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(text)


def move(root, pr, fmt=True, build=True, t=APP):
    rows = read_manifest(os.path.join(root, t.manifest))
    mod_path = os.path.join(root, t.source)
    with open(mod_path, encoding="utf-8") as f:
        text = f.read()
    items, glue = cpm.parse(text, t.source)
    placed = place(items, rows, landed(root, rows, t))
    files = halves(rows)
    if pr not in files:
        raise ManifestError(f"no item in the manifest goes in PR {pr}")
    file, half = files[pr]
    picked = [it for it, p in zip(items, placed) if p == (pr, file)]
    if not picked:
        raise ManifestError(f"PR {pr}'s items are not in mod.rs: has it moved already?")
    for it in picked:
        if it.mods:
            raise ManifestError(f"{it.name} sits in an inline module ({'::'.join(it.mods)}); this tool moves only top-level items and impl members")
    lines = text.split("\n")
    code, starts_in, _ = cpm.lex(text)
    clines = code.split("\n")
    kinds = [cpm.line_kind(ln, c, starts_in.get(i)) for i, (ln, c) in enumerate(zip(lines, clines))]
    blocks = impl_blocks(glue)
    body = render_items(lines, picked, blocks, kinds, clines, emptied(picked, blocks, items))
    free = any(enclosing(blocks, it) is None for it in picked)

    target = os.path.join(root, t.dir, f"{file}.rs")
    if os.path.exists(target):
        with open(target, encoding="utf-8") as f:
            existing = with_super_glob(f.read().rstrip("\n").split("\n"))
        new_file = existing + [""] + body
        glue_free = free and not any(re.match(rf"^use {file}::\*;$", ln) for ln in lines)
    else:
        doc = ["//! " + ln if ln else "//!" for ln in t.files[file][0].split("\n")]
        new_file = doc + ["", "use super::*;", ""] + body
        glue_free = free
    rest = cut(lines, picked, blocks, items)
    if not os.path.exists(target):
        rest = add_glue(rest, file, glue_free)
    elif glue_free:
        rest = add_glue_use_only(rest, file)
    arch = os.path.join(root, ARCH)
    with open(arch, encoding="utf-8") as f:
        arch_text = add_arch_row(f.read(), file, t)
    write_all([
        (target, "\n".join(new_file).rstrip("\n") + "\n"),
        (mod_path, "\n".join(rest)),
        (os.path.join(root, note_path(file, half, t)), note_text(file, half, t)),
        (arch, arch_text),
    ])
    if fmt:
        subprocess.run(["cargo", "fmt"], cwd=root, check=True)
    widened = sum(1 for it in picked if not it.vis and it.kind in VISIBLE
                  and not (enclosing(blocks, it) and TRAIT_IMPL.search(it.ctx or "")))
    print(f"PR {pr}: moved {len(picked)} item(s) from {t.source} to {t.dir}/{file}.rs, "
          f"{widened} widened to pub(super)")
    if build:
        for line in settle(root, rows, t=t, new=file):
            print(f"  glue: {line}")
        print("  the app and its tests build clean under clippy")
    return file


def add_glue_use_only(lines, file):
    decl = re.compile(rf"^mod {file};$")
    at = next(n for n, ln in enumerate(lines) if decl.match(ln))
    n = at + 1
    while n < len(lines) and re.match(r"^(pub(\(\w+\))? )?mod \w+;$", lines[n]):
        n += 1
    while n < len(lines) and re.match(r"^use \w+(::\{[^}]*\}|::\w+)*;$", lines[n]):
        n += 1
    lines.insert(n, f"use {file}::*;")
    return lines


def cargo_errors(root, mode):
    """[(code, message)] of every error `cargo clippy <mode>` reports with
    warnings denied: `--bins` builds the app, `--tests` its tests."""
    cmd = ["cargo", "clippy", mode, "--keep-going", "--message-format=json", "--", "-D", "warnings"]
    run = subprocess.run(cmd, cwd=root, capture_output=True, text=True)
    return read_errors(mode, run.returncode, run.stdout, run.stderr)


def read_errors(mode, status, stdout, stderr):
    """The compiler errors in cargo's JSON `stdout`, as (code, message); an
    error with no code has code "". rustc's closing "aborting due to ..."
    only counts the others, so it is left out. A build that failed with no
    compiler error to show for it (a dependency, a build script, cargo
    itself) raises BuildError: a failure is never read as a clean build."""
    errors = []
    for line in stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        msg = m.get("message") or {}
        if m.get("reason") != "compiler-message" or msg.get("level") not in ("error", "error: internal compiler error"):
            continue
        if not msg.get("code") and msg.get("message", "").startswith("aborting due to"):
            continue
        errors.append(((msg.get("code") or {}).get("code", ""), msg.get("message", "")))
    if status != 0 and not errors:
        tail = "\n  ".join(stderr.strip().splitlines()[-15:])
        raise BuildError(f"cargo clippy {mode} failed with no compiler error to fix:\n  {tail}")
    return sorted(set(errors))


def glob_line(lines, file):
    """The line of `use <file>::*;` in mod.rs, or None."""
    return next((n for n, ln in enumerate(lines) if ln == f"use {file}::*;"), None)


def reexport_line(lines, file, name):
    """The line of a re-export `<vis> use <file>::<name>;` in mod.rs, or None."""
    pat = re.compile(rf"^pub(\(\w+\))? use {file}::{name};$")
    return next((n for n, ln in enumerate(lines) if pat.match(ln)), None)


def fix_glue(lines, app, tests, homes, scope="app"):
    """mod.rs `lines` with the glue the build errors ask for, and what was
    done. `scope` names the module being split, for the messages. `app` and `tests` are the errors of the two builds; `homes` maps a
    moved free item's name to (file, visibility). Raises BuildError when an
    error is not one this fixes."""
    lines, done, unknown = list(lines), [], []
    private, unused = set(), {"app": set(), "tests": set()}
    unused_one = {"app": set(), "tests": set()}
    for side, errors in (("app", app), ("tests", tests)):
        for code, message in errors:
            if code == "E0603" and (m := PRIVATE_IMPORT.match(message)) and m[1] in homes:
                private.add(m[1])
            elif code == "unused_imports" and (m := UNUSED_GLOB.match(message)) \
                    and glob_line(lines, m[1]) is not None:
                unused[side].add(m[1])
            elif code == "unused_imports" and (m := UNUSED_ONE.match(message)) \
                    and reexport_line(lines, m[1], m[2]) is not None:
                unused_one[side].add((m[1], m[2]))
            else:
                unknown.append(f"{side}: {code}: {message}")
    # An error this does not fix can follow from a private import: a type
    # reached through one is not resolved, and rustc then reports what it
    # could not infer about the type's fields too. So the re-exports go in,
    # and the next build says whether anything else is left. Any other
    # error stops the tool.
    if unknown and not private:
        raise BuildError("the build fails in a way this tool does not fix:\n  " + "\n  ".join(unknown))
    for name in sorted(private):
        file, vis = homes[name]
        if vis not in ("pub", "pub(crate)"):
            raise BuildError(f"{name} is used outside {scope} but is {vis or 'private'} in {file}.rs")
        at = glob_line(lines, file)
        if at is None:
            at = next(n for n, ln in enumerate(lines) if ln == f"mod {file};")
        lines.insert(at + 1, f"{vis} use {file}::{name};")
        done.append(f"re-exported {file}::{name} ({vis}): used outside {scope}")
    for file in sorted(unused["app"] | unused["tests"]):
        at = glob_line(lines, file)
        guarded = at > 0 and lines[at - 1].startswith("#[cfg(")
        start = at - 1 if guarded else at
        if file in unused["app"] and file in unused["tests"] or guarded:
            del lines[start : at + 1]
            done.append(f"dropped use {file}::*: nothing needs it")
        else:
            only = "test" if file in unused["app"] else "not(test)"
            lines.insert(at, f"#[cfg({only})]")
            done.append(f"use {file}::* only under cfg({only})")
    # A re-export only the tests use (an item a test elsewhere names by its
    # path) is unused in the app build: it gets `#[cfg(test)]` the same way.
    for file, name in sorted(unused_one["app"] | unused_one["tests"]):
        at = reexport_line(lines, file, name)
        guarded = at > 0 and lines[at - 1].startswith("#[cfg(")
        start = at - 1 if guarded else at
        both = (file, name) in unused_one["app"] and (file, name) in unused_one["tests"]
        if both or guarded:
            del lines[start : at + 1]
            done.append(f"dropped the re-export of {file}::{name}: nothing needs it")
        else:
            only = "test" if (file, name) in unused_one["app"] else "not(test)"
            lines.insert(at, f"#[cfg({only})]")
            done.append(f"re-export of {file}::{name} only under cfg({only})")
    return lines, done


UNUSED_SUPER = ("unused_imports", "unused import: `super::*`")


def with_super_glob(lines):
    """A feature file's lines with `use super::*;` after its `//!` doc. An
    earlier move's build step drops the import from a file that needs nothing
    from the split file; a later move appending items that do would then not
    build. The build step drops it again if it is still unused."""
    if "use super::*;" in lines:
        return lines
    at = 0
    while at < len(lines) and lines[at].startswith("//!"):
        at += 1
    return lines[:at] + ["", "use super::*;"] + lines[at:]


def without_super_glob(lines):
    """A new file's `lines` without the `use super::*;` its header got, and
    the blank line after it."""
    at = lines.index("use super::*;")
    end = at + 2 if at + 1 < len(lines) and lines[at + 1] == "" else at + 1
    return lines[:at] + lines[end:]


def settle(root, rows, rounds=5, t=APP, new=None):
    """Build, fix the glue the build asks for, and build again until the app
    and its tests are clean. `new` is the file this move wrote, whose
    `use super::*;` goes when nothing moved into it needs the split file,
    in the app and its tests alike: every other file built clean before.
    Returns what was done."""
    done = []
    mod_path = os.path.join(root, t.source)
    for _ in range(rounds):
        app, tests = cargo_errors(root, "--bins"), cargo_errors(root, "--tests")
        if not app and not tests:
            return done
        if new and UNUSED_SUPER in app and UNUSED_SUPER in tests:
            path = os.path.join(root, t.dir, f"{new}.rs")
            with open(path, encoding="utf-8") as f:
                lines = f.read().split("\n")
            if "use super::*;" in lines:
                with open(path, "w", encoding="utf-8") as f:
                    f.write("\n".join(without_super_glob(lines)))
                done.append(f"dropped use super::* from {new}.rs: nothing in it needs {t.source}")
                continue
        homes = {}
        for pr, file, kind, impl, name in rows:
            path = os.path.join(root, t.dir, f"{file}.rs")
            if pr and impl == "-" and os.path.exists(path):
                homes.setdefault(name, file)
        vis = {}
        for file in set(homes.values()):
            path = os.path.join(root, t.dir, f"{file}.rs")
            with open(path, encoding="utf-8") as f:
                for it in cpm.parse(f.read(), path)[0]:
                    if not it.ctx and not it.mods:
                        vis[(file, it.name)] = it.vis
        homes = {n: (f, vis[(f, n)]) for n, f in homes.items() if (f, n) in vis}
        with open(mod_path, encoding="utf-8") as f:
            lines = f.read().split("\n")
        lines, now = fix_glue(lines, app, tests, homes, t.name)
        with open(mod_path, "w", encoding="utf-8") as f:
            f.write("\n".join(lines))
        subprocess.run(["cargo", "fmt"], cwd=root, check=True)
        done += now
    raise BuildError(f"still failing after {rounds} rounds of fixes: {done}")


def check(root, t=APP):
    rows = read_manifest(os.path.join(root, t.manifest))
    with open(os.path.join(root, t.source), encoding="utf-8") as f:
        items, _ = cpm.parse(f.read(), t.source)
    placed = place(items, rows, landed(root, rows, t))
    left = Counter(p[0] for p in placed)
    todo = len([k for k in left if k])
    done = len({pr for pr, *_ in rows if pr}) - todo
    print(f"The manifest places all {len(items)} item(s) of {t.source}; "
          f"{left[0]} stay, the rest move in {todo} PR(s); {done} PR(s) moved already.")
    return 0




def tests_header(name):
    return re.compile(rf"(^|\n)\s*mod\s+{re.escape(name)}\s*\{{$")


def tests_out(root, t, fmt=True, name="tests"):
    """Move the inline `mod <name> { ... }` (`mod tests` unless the target
    names another test module) at the top level of the target's file to
    <dir>/<name>.rs: the block's body, dedented by its indentation, and
    `mod <name>;` where the block was. The module keeps its name and its
    place, so its `use super::*;` still reaches the split file. The header's
    attributes and the docs and comments above it stay. A line that starts
    inside a string keeps its indentation, which is part of the string.
    Writes the release note and the docs row too. Returns the number of
    lines moved."""
    words = t.tests if name == "tests" else t.more_tests.get(name)
    if not words:
        raise ManifestError(f"the {t.name} target has no words for its {name} module")
    src = os.path.join(root, t.source)
    with open(src, encoding="utf-8") as f:
        text = f.read()
    _, glue = cpm.parse(text, t.source)
    header = tests_header(name)
    heads = [g for g in glue if g.kind == "header" and not g.scope and header.search(g.text)]
    if len(heads) != 1:
        raise ManifestError(f"{t.source} has {len(heads)} top-level `mod {name} {{` blocks; expected one")
    head = heads[0]
    block = next(b for b in impl_blocks(glue) if b[0] == head.start)
    _, open_line, close = block
    lines = text.split("\n")
    _, starts_in, _ = cpm.lex(text)
    body = lines[open_line + 1 : close]
    indent = os.path.commonprefix([
        ln[: len(ln) - len(ln.lstrip())]
        for k, ln in enumerate(body, open_line + 1)
        if ln.strip() and starts_in.get(k) != "string"
    ])
    out = []
    for k, ln in enumerate(body, open_line + 1):
        if starts_in.get(k) == "string":
            out.append(ln)
        else:
            out.append(ln[len(indent):] if ln.startswith(indent) else ln.strip())
    while out and not out[0].strip():
        out.pop(0)
    doc = ["//! " + ln if ln else "//!" for ln in words[0].split("\n")]
    target = os.path.join(root, t.dir, f"{name}.rs")
    if os.path.exists(target):
        raise ManifestError(f"{t.dir}/{name}.rs exists already")
    decl = re.sub(rf"mod\s+{re.escape(name)}\s*\{{$", f"mod {name};", lines[open_line])
    rest = lines[:open_line] + [decl] + lines[close + 1:]
    arch = os.path.join(root, ARCH)
    with open(arch, encoding="utf-8") as f:
        arch_text = add_arch_row(f.read(), name, t, words[2])
    write_all([
        (target, "\n".join(doc + [""] + out).rstrip("\n") + "\n"),
        (src, "\n".join(rest)),
        (os.path.join(root, NOTES, f"2062-split-{t.note_prefix}{name.replace('_', '-')}.md"),
         f"refactor: Groundwork for the refactor program (#2062): {words[1]} moved "
         f"to their own file, {t.dir}/{name}.rs. Nothing changes in use.\n"),
        (arch, arch_text),
    ])
    if fmt:
        subprocess.run(["cargo", "fmt"], cwd=root, check=True)
    moved = close - open_line - 1
    print(f"moved {moved} line(s) of tests from {t.source} to {t.dir}/{name}.rs")
    return moved


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--target", default="app", choices=sorted(TARGETS), help="the file to split")
    ap.add_argument("--pr", type=int, help="the PR whose items to move")
    ap.add_argument("--check", action="store_true", help="only check the manifest")
    ap.add_argument("--tests-out", nargs="?", const="tests", metavar="MODULE",
                    help="move the file's inline `mod tests` (or the test module named) to its own file")
    ap.add_argument("--root", default=".", help="the repository root")
    ap.add_argument("--no-fmt", action="store_true", help="skip `cargo fmt` (for tests)")
    ap.add_argument("--no-build", action="store_true", help="skip the build and its glue fixes (for tests)")
    args = ap.parse_args(argv)
    t = TARGETS[args.target]
    try:
        if args.tests_out:
            tests_out(args.root, t, fmt=not args.no_fmt, name=args.tests_out)
            return 0
        if args.check:
            return check(args.root, t)
        if args.pr is None:
            ap.error("give --pr N, --check or --tests-out")
        move(args.root, args.pr, fmt=not args.no_fmt, build=not args.no_build, t=t)
        return 0
    except (ManifestError, BuildError, cpm.ParseError) as e:
        print(f"move_items: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
