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
- each item's visibility is spelled to reach, from its new file, exactly
  what it reached before: a private item becomes `pub(super)` (everything in
  `app`, its sibling files and its tests included), a `pub(super)` one
  moving a level down names the module it reached (`pub(in crate::app)`
  from src/app/codeql.rs into src/app/codeql/), and `pub(crate)` and `pub`
  stay. So no other file can fail to build for want of it, whatever order
  the moves and other PRs merge in, and nothing can see more than before.
  A method in a trait impl, a macro call and a comment carry no visibility
  and are copied as they are;
- an `impl` block that has nothing left in mod.rs goes, with its header;
- mod.rs gets `mod <file>;` and, when the file has free items (anything
  outside an `impl`), `use <file>::*;`, so code that named them still does.

It also writes the PR's release-note fragment and the file's row in
docs/ARCHITECTURE.md, then runs `cargo fmt`.

Then it builds the app and its tests with clippy, warnings denied, and fixes
the glue the build asks for, until both are clean:

- an item used outside the target's module (as `crate::app::name`) is
  private through the glob, so mod.rs re-exports it by name at the item's
  own visibility; so is one named bare where a file above globs mod.rs
  (src/app/mod.rs's `use debugger::*;` for src/app/debugger.rs), which
  mod.rs's own private `use <file>::*;` does not pass on, the re-export
  spelled to reach what the item reaches;
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
# `more_tests` has the same words for any other inline module moved out
# whole, by name: remote.rs's second test module, file_tree.rs's Android
# trash.
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

# The Source Control panel's feature files (#2062): (module doc, release-note phrase, docs row).
SOURCE_CONTROL_FILES = {
    "palette": (
        "The panel's colours and glyphs, and the width a row's action icons take.",
        "source control colour and glyph",
        "the panel's colours and glyphs, and the width of a row's action icons",
    ),
    "commit_menu": (
        "The Commit button's dropdown: its items in VS Code's order, their labels and icons, and\n"
        "the clicks that open it and pick an item.",
        "source control commit menu",
        "the Commit button's dropdown: its items, labels and icons, and the clicks that "
        "open and pick from it",
    ),
    "repositories": (
        "The multi-root REPOSITORIES overview: one row per workspace folder that is a git\n"
        "repository, drawing them, and the click that picks one.",
        "source control repositories overview",
        "the multi-root REPOSITORIES overview: its rows, drawing them, and the click that "
        "picks one",
    ),
    "message": (
        "The commit message box: its placeholder and key hint, typing and moving the caret across\n"
        "the subject and body lines, git's prepared message, clearing it after a commit, and where\n"
        "its caret is on screen.",
        "source control commit message",
        "the commit message box: typing, the caret across subject and body, git's "
        "prepared message, and clearing it",
    ),
    "change_list": (
        "The change list: taking in git's status, selecting rows (one, several or all), moving the\n"
        "selection in the order rows are drawn, scrolling, and laying the list out as section\n"
        "headers and entry rows.",
        "source control change list",
        "the change list: git's status, selecting and moving through rows, scrolling, and "
        "its layout",
    ),
    "clicks": (
        "What a click lands on: the header pills, the branch name, a row's stage, unstage and\n"
        "discard icons, the Commit and Initialize buttons, and a nested repository's Open button.",
        "source control hit-testing",
        "what a click lands on: the header pills, the branch, a row's action icons and "
        "the buttons",
    ),
    "empty_state": (
        "The \"No repository detected\" card: its text, its buttons and the Git fork drawing, full\n"
        "size or compact.",
        "source control empty state",
        "the \"No repository detected\" card and its Git fork drawing",
    ),
    "buttons": (
        "The buttons every view draws: the primary button (Commit, Initialize, Open, Run and Debug)\n"
        "in its boxed and rounded looks, the Commit split button, their colours and the lift a\n"
        "pointer gives them.",
        "source control button",
        "the primary button every view draws, the Commit split button, their colours and "
        "hover",
    ),
    "render": (
        "Drawing the panel: the header, the branch summary, the message box, the Commit button,\n"
        "the change list and the commit feedback, wrapped to fit.",
        "source control panel drawing",
        "drawing the panel: header, branch, message box, Commit button, change list and "
        "feedback",
    ),
}

SOURCE_CONTROL = Target(
    "source_control", "scripts/split_source_control_manifest.tsv",
    "src/widgets/source_control.rs", "src/widgets/source_control", SOURCE_CONTROL_FILES,
    Tree(
        "    ├── source_control.rs ",
        "    ├── source_control/       the Source Control panel's feature files, beside "
        "source_control.rs; each file's `//!` doc says what it holds",
        "    │   ├── ", 17,
    ),
    "source-control-",
    (
        "The Source Control panel's own tests, moved out of source_control.rs (#2062).",
        "the Source Control panel's tests",
        "the Source Control panel's own tests",
    ),
)

# The command line's feature files (#2062): (module doc, release-note phrase, docs row).
CLI_FILES = {
    "version": (
        "croft's version: the plain `x.y.z` that `--version` prints, and the provenance\n"
        "`--build-info` prints (commit and build time, and how the binary was installed when a\n"
        "package manager owns it).",
        "CLI version",
        "the plain version `--version` prints, and the provenance line `--build-info` "
        "prints",
    ),
    "typos": (
        "A misspelled subcommand: the word clap took as the `PATH` argument when it is a typo of\n"
        "a subcommand, turned into a usage error that names the subcommand meant, rather than a\n"
        "folder to open.",
        "misspelled subcommand",
        "a misspelled subcommand clap took as a folder, turned into an error that names "
        "the one meant",
    ),
    "workspace": (
        "The `PATH` and `--open-file` arguments: the workspace root and the file to open, a\n"
        "`file:line[:col]` location, a `.code-workspace` file's folders, and a file not written\n"
        "yet.",
        "launch workspace",
        "the `PATH` and `--open-file` arguments: the workspace root, the file to open and "
        "its `file:line[:col]` location",
    ),
    "git_editor": (
        "`croft edit` as git's editor: the user's own `sequence.editor`, the editor git itself\n"
        "would run, and running it as git does, for when no croft is there to edit in.",
        "git editor",
        "`croft edit` as git's editor: the user's own sequence editor, and git's own "
        "editor when no croft is there",
    ),
    "links": (
        "`croft open-link` and `croft install-link-handler`: checking a link and becoming the\n"
        "`croft remote` or `croft attach` it describes, and making croft the link's handler.",
        "link",
        "`croft open-link` and `croft install-link-handler`: following a link into a "
        "session, and handling the links",
    ),
    "plot": (
        "`croft plot`: a chart of what arrives on stdin, printed as an inline image where the\n"
        "terminal shows one and as text otherwise, in the active theme's colours.",
        "plot command",
        "`croft plot`: a chart of stdin, as an inline image or as text, in the theme's "
        "colours",
    ),
    "pair_provider": (
        "`croft pair`'s provider flags as recorded: `--base-url` alone means ollama, and ollama\n"
        "without an endpoint gets the standard local port.",
        "pair provider flag",
        "`croft pair`'s provider flags: `--base-url` alone means ollama, which defaults "
        "to its local port",
    ),
    "hook": (
        "`croft hook`: Claude Code's `PreToolUse` hook itself, and installing or removing it in\n"
        "Claude Code's settings.",
        "agent hook command",
        "`croft hook`: Claude Code's edit hook itself, and installing or removing it",
    ),
    "keys": (
        "`croft keys`: the diagnostic that prints each key and mouse event as croft receives it,\n"
        "with its kind and modifiers.",
        "key diagnostic",
        "`croft keys`: printing each key and mouse event as croft receives it",
    ),
    "terminal_setup": (
        "`croft setup-terminal`, `setup-iterm2` and `setup-ghostty`: the fonts they set, and\n"
        "setting up Terminal.app, iTerm2 and Ghostty for croft on macOS.",
        "terminal setup",
        "`croft setup-terminal`, `setup-iterm2` and `setup-ghostty`: setting up macOS "
        "terminals for croft",
    ),
    "vscode_import": (
        "`croft import-vscode`: converting a VS Code profile into croft's config, and the report\n"
        "of what was adopted, compared, dropped or refused.",
        "VS Code import",
        "`croft import-vscode`: converting a VS Code profile, and the report of what came "
        "across",
    ),
    "theme_import": (
        "`croft theme-import`: converting a VS Code theme, from a file or the marketplace, and\n"
        "installing it.",
        "theme import",
        "`croft theme-import`: converting a VS Code theme from a file or the marketplace, "
        "and installing it",
    ),
    "launcher_install": (
        "`croft install-launcher`: the macOS Croft.app that opens a folder or a file in croft\n"
        "inside a new Ghostty window.",
        "launcher install",
        "`croft install-launcher`: the macOS Croft.app that opens croft in a new Ghostty "
        "window",
    ),
    "cross": (
        "`croft setup-cross`: the targets the local cross-build for `croft <host>` builds for, and\n"
        "installing zig, cargo-zigbuild and those targets.",
        "cross-build setup",
        "`croft setup-cross`: the cross-build's targets, and installing zig, "
        "cargo-zigbuild and the targets",
    ),
    "commands": (
        "Every subcommand and its flags, as clap parses them: `CliCommand`, and `croft hook`'s\n"
        "actions.",
        "CLI subcommand",
        "every subcommand and its flags, as clap parses them",
    ),
    "run": (
        "Running what was parsed: `Cli::run` hands each subcommand to its code, or opens the\n"
        "workspace; and a closed stdout ends a command that only prints quietly.",
        "CLI dispatch",
        "running what was parsed: each subcommand handed to its code, or the workspace "
        "opened",
    ),
}

CLI = Target(
    "cli", "scripts/split_cli_manifest.tsv", "src/cli.rs", "src/cli", CLI_FILES,
    Tree(
        "├── cli.rs ",
        "├── cli/                 the command line's feature files, beside cli.rs; "
        "each file's `//!` doc says what it holds",
        "│   ├── ", 17,
    ),
    "cli-",
    (
        "The command line's own tests, moved out of cli.rs (#2062).",
        "the command line's tests",
        "the command line's own tests",
    ),
)

# The CodeQL side bar's feature files (#2062): (module doc, release-note phrase, docs row).
# The App methods move a level down, from src/app/codeql.rs into src/app/codeql/, so a
# `pub(super)` one becomes `pub(in crate::app)`: the same reach, all of `app`.
CODEQL_FILES = {
    "test_runs": (
        "CodeQL: Run Tests and Accept Test Output: every CodeQL test run through the Testing\n"
        "view's run-all, and a failing test's output made its expected output.",
        "CodeQL test run",
        "CodeQL: Run Tests through the Testing view, and Accept Test Output",
    ),
    "cli_install": (
        "The CodeQL CLI: which `codeql` every run uses (settings, `PATH` or croft's own copy),\n"
        "its version, downloading it into croft's cache, and checking for a newer release.",
        "CodeQL CLI install",
        "the CodeQL CLI: which `codeql` runs, its version, downloading it and checking for "
        "updates",
    ),
    "databases": (
        "The CodeQL database list: the current database, removing, renaming, sorting and\n"
        "revealing databases, upgrading one to the CLI's schema, deleting unused copies, and\n"
        "where databases and their sources are kept.",
        "CodeQL database list",
        "the database list: removing, renaming, sorting, upgrading, deleting unused copies, "
        "and where databases are kept",
    ),
    "database_sources": (
        "Adding a CodeQL database: the places one comes from (a folder, an archive, a URL,\n"
        "GitHub), the database picker, downloading and unpacking it on a worker thread,\n"
        "registering it, and adding a database's source to the workspace.",
        "CodeQL database source",
        "adding a database from a folder, an archive, a URL or GitHub, and its source to the "
        "workspace",
    ),
    "history": (
        "The CodeQL query history: where runs and their results are kept, listing, removing,\n"
        "renaming and sorting entries, an entry's query and its text, its evaluator log, and\n"
        "its results folder.",
        "CodeQL query history",
        "the query history: its entries, their queries, logs and results folders",
    ),
    "logs": (
        "A run's logs: the evaluator log as raw JSON, as summary text and in the log viewer,\n"
        "the query log, and comparing two runs' performance.",
        "CodeQL log",
        "a run's evaluator and query logs, the log viewer, and Compare Performance",
    ),
    "results": (
        "A run's results: walking a results table, decoding result sets, choosing a set, the\n"
        "alert and raw-results views, comparing two runs' results, and exporting them.",
        "CodeQL result",
        "a run's results: result sets, the alert and raw views, Compare Results, Export "
        "Results",
    ),
    "queries": (
        "The workspace's queries: finding them for the side bar on a worker thread, creating\n"
        "one, a quick query, the query a `.qlref` points at, and previewing a query's help.",
        "CodeQL query",
        "the workspace's queries: finding and creating them, Quick Query, query help",
    ),
    "packs": (
        "Suites and packs: installing a pack's dependencies, downloading packs on a worker\n"
        "thread, running a suite, a pack, or a published pack, and refusing a suite whose\n"
        "results cannot be shown.",
        "CodeQL pack",
        "suites and packs: installing, downloading and running them",
    ),
    "run": (
        "Running a query: the open query on the current database or another, quick\n"
        "evaluation, Debug Query, running `codeql` and cancelling it, and recording a\n"
        "finished run in the query history.",
        "CodeQL query run",
        "running a query, quick evaluation and Debug Query, cancelling and recording a run",
    ),
    "queue": (
        "The run queue: several queries run one after another (all queries in the workspace,\n"
        "those in selected files), one query on several databases and their checklist, and\n"
        "cancelling what waits.",
        "CodeQL run queue",
        "the run queue: many queries in turn, or one query on several databases",
    ),
    "contextual": (
        "Contextual queries: what they run on and the queries they resolve, View AST and its\n"
        "viewer, and View CFG.",
        "CodeQL AST and CFG",
        "contextual queries: View AST and View CFG",
    ),
    "model_editor": (
        "The model editor: reading a database's endpoints, modeling an endpoint, and writing\n"
        "the models to the database's model pack.",
        "CodeQL model editor",
        "the model editor: endpoints, their models, and the model pack",
    ),
    "variant_repos": (
        "What variant analysis runs against: its config, the controller repository, lists,\n"
        "repositories and owners, adding repositories from a Code Search, and opening the\n"
        "selected one on GitHub.",
        "variant analysis repository list",
        "variant analysis targets: the controller, lists, repositories, owners and Code "
        "Search",
    ),
    "variant_runs": (
        "Variant analysis runs: submitting the open query, following the runs, their logs,\n"
        "fetching and opening their results, and exporting them.",
        "variant analysis run",
        "variant analysis runs: submitting, following, fetching and exporting results",
    ),
    "side_bar": (
        "The CodeQL side bar itself: whether it is available, revealing it, its keys, what\n"
        "each row does when activated, and keeping the chrome in step when CodeQL is added,\n"
        "removed, enabled or disabled.",
        "CodeQL side bar",
        "the side bar itself: availability, its keys and what each row does",
    ),
}

CODEQL = Target(
    "codeql", "scripts/split_codeql_manifest.tsv", "src/app/codeql.rs", "src/app/codeql",
    CODEQL_FILES,
    Tree(
        "│   ├── codeql.rs ",
        "│   ├── codeql/          the CodeQL side bar's feature files, beside codeql.rs; "
        "each file's `//!` doc says what it holds",
        "│   │   ├── ", 19,
    ),
    "codeql-",
    # Its tests are already their own file, src/app/tests/codeql.rs.
    None,
)

# What each file split out of src/app/debugger.rs holds, as FILES above.
DEBUGGER_FILES = {
    "adapters": (
        "Finding and installing debug adapters: lldb-dap under each name LLVM ships it as, what\n"
        "to say when it is missing, whether a file is a runnable binary, and Debug: Install Go\n"
        "Debugger (delve).",
        "debug adapter",
        "finding lldb-dap, telling a runnable binary, and installing delve",
    ),
    "python": (
        "The interpreter a debugged Python program runs under: the project's own (its venv, or\n"
        "`python3`), the one a launch request or a launch.json config names, Debug Test's, and\n"
        "the note on one older than attach's floor.",
        "debuggee Python",
        "which Python a debugged program runs under, and the note on an old one",
    ),
    "debug_test": (
        "Debug Test: the test at the caret or a named one, the runner for the focused file, its\n"
        "build (`cargo test --no-run`) and launch under lldb-dap, debugpy or delve, and the\n"
        "temporary breakpoint on the assertion the last run failed at.",
        "Debug Test",
        "Debug Test: the test at the caret or by name, its build, launch and failure breakpoint",
    ),
    "builds": (
        "Debug builds: a launch.json `cargo` config's build, F5's C/C++ build and a single Rust\n"
        "file's, each on a worker thread, and launching lldb-dap on the binary they make.",
        "debug build",
        "debug builds: `cargo`, C/C++ and single Rust files, then lldb-dap on the binary",
    ),
    "active_file": (
        "The active file: Run (how to run a file, in a new terminal) and F5 with no launch\n"
        "configuration, which picks the adapter by the file's kind: debugpy, delve, js-debug or\n"
        "lldb-dap.",
        "active file's run and debug",
        "running and debugging the active file, the adapter picked by its kind",
    ),
    "events": (
        "The debug sessions' events, drained every frame: stops and the paused line, output, a\n"
        "session ending (and which one, in a compound) and the message it leaves.",
        "debug event",
        "the sessions' events, drained each frame: stops, output and endings",
    ),
    "view": (
        "The Run and Debug view: its keys, the Run or Debug button, the paused-state tree (call\n"
        "stack, variables), clicks and the row menu with Copy Value and Copy Call Stack,\n"
        "revealing the view, and how a launch failure shows.",
        "Run and Debug view",
        "the view: its keys, button, paused-state tree, clicks, row menu and errors",
    ),
    "watches": (
        "Watch expressions: adding and removing them, evaluating them at each stop, and what\n"
        "they keep when a session ends.",
        "watch expression",
        "watch expressions: adding, removing and evaluating them",
    ),
    "console": (
        "The debug console: its lines, program output, the REPL, mirroring to OUTPUT's Debug\n"
        "Console channel, and hover-to-evaluate while paused.",
        "debug console",
        "the debug console, its REPL and OUTPUT mirror, and hover-to-evaluate",
    ),
    "launch": (
        "Starting a launch: saving unsaved editors first, F5 (start or continue), the selected\n"
        "configuration, resolving a launch.json configuration and its program, spawning its\n"
        "adapter (a headless browser when there is no display), and installing the new session.",
        "debug launch",
        "F5 and launching a configuration: saving first, resolving, spawning, installing",
    ),
    "compounds": (
        "Compounds and the tasks around a launch: launching a compound's members, parking a\n"
        "launch behind its `preLaunchTask` until the task ends, `postDebugTask`, a member\n"
        "ending, and switching between the running sessions.",
        "compound launch",
        "compounds, preLaunchTask waits, postDebugTask and switching sessions",
    ),
    "configs": (
        "Launch configurations: Debug: Add Configuration's form, writing the draft to\n"
        "`.croft/launch.json`, and Debug: Select and Start Debugging's picker.",
        "launch configuration",
        "Add Configuration and the Select and Start Debugging picker",
    ),
    "breakpoints": (
        "Every kind of breakpoint: line breakpoints with conditions, hit counts and logpoints,\n"
        "function and data breakpoints, break on raised exceptions, and sending a file's\n"
        "breakpoints to the running sessions.",
        "breakpoint",
        "line, conditional, hit-count, function and data breakpoints, logpoints, exceptions",
    ),
    "breakpoint_list": (
        "The BREAKPOINTS list in the Run and Debug view: its rows, keeping it current, a click\n"
        "on one, and removing a breakpoint from it.",
        "breakpoint list",
        "the BREAKPOINTS list: its rows, clicks and removal",
    ),
    "controls": (
        "The debug toolbar's controls: continue, pause, step over, into and out, Step Into\n"
        "Target, Run to Cursor, restart and stop.",
        "debug control",
        "continue, pause, stepping, Step Into Target, Run to Cursor, restart and stop",
    ),
    "attach": (
        "Attaching: the process picker for a `${command:pickProcess}` attach and for Debug:\n"
        "Attach to Python Process, its keys, mouse and drawing, and attaching to the chosen\n"
        "process.",
        "attach picker",
        "the attach process pickers and attaching to the chosen process",
    ),
    "inline_values": (
        "Inline values: the values of the variables on the paused line, shown after them in the\n"
        "editor, cleared and toggled.",
        "inline value",
        "inline values beside the paused lines",
    ),
}

DEBUGGER = Target(
    "debugger", "scripts/split_debugger_manifest.tsv", "src/app/debugger.rs", "src/app/debugger",
    DEBUGGER_FILES,
    Tree(
        "│   ├── debugger.rs ",
        "│   ├── debugger/        the Run and Debug feature files, beside debugger.rs; "
        "each file's `//!` doc says what it holds",
        "│   │   ├── ", 19,
    ),
    "debugger-",
    # Its tests are already their own file, src/app/tests/debug.rs.
    None,
)

# What each file split out of src/app/language_servers.rs holds, as FILES above.
LANGUAGE_SERVERS_FILES = {
    "sync": (
        "Keeping the language servers in step with the open files: each tab's document opened,\n"
        "changed and closed on its servers, the notice when shellcheck is missing, re-opening the\n"
        "files a just-installed server covers, filing each server's diagnostics, the\n"
        "workspace-diagnostics refresh, and Language Servers: Restart All and Restart for This\n"
        "File.",
        "language server sync",
        "keeping servers in step with the open files, their diagnostics, and restarts",
    ),
    "rust_analyzer": (
        "rust-analyzer's own `Rust:` commands: whether it serves the active file, sending one for\n"
        "the caret, and acting on the answer (a macro's expansion opened as a tab, a jump to the\n"
        "parent module or Cargo.toml, a proc-macro rebuild or workspace reload reported).",
        "rust-analyzer command",
        "rust-analyzer's `Rust:` commands: sending them and acting on the answers",
    ),
    "semantic": (
        "Semantic highlighting: semantic-token replies handed to every editor showing the file,\n"
        "the refresh a server asks for, which of two overlapping replies wins, and swapping in\n"
        "each tab's finished background syntax pass.",
        "semantic token",
        "semantic tokens, their refresh, and the background syntax pass landing",
    ),
    "inlay_hints": (
        "Inlay hints: replies routed to every editor showing the file (the seq-gated fan-out\n"
        "that links and colors share), the refresh a server asks for, the range each request\n"
        "covers, and Editor: Toggle Inlay Hints.",
        "inlay hint",
        "inlay hints: their replies, refresh, requests and the toggle",
    ),
    "decorations": (
        "What a server marks in the text besides tokens and hints: document links and following\n"
        "one, folding ranges, color swatches, and Change Color Presentation's picker.",
        "document link, folding range and color",
        "document links, folding ranges, colors and Change Color Presentation",
    ),
    "messages": (
        "What the servers tell croft: work-done progress and the status-bar line it makes,\n"
        "`window/*` messages as status lines, a server's questions in the list picker, and\n"
        "`window/showDocument`.",
        "server message",
        "server progress, messages, questions and show-document requests",
    ),
    "navigation": (
        "Go to Definition, Declaration, Type Definition, Implementations and References: the\n"
        "request at the caret or under a Cmd-click, jumping to the answer with navigation\n"
        "history, and the picker when there are several.",
        "go-to",
        "Go to Definition and its kin: requests, jumps and the multi-location picker",
    ),
    "peek": (
        "Peek Definition and Peek References: the popup with its numbered excerpt, stepping\n"
        "through the references, and closing it.",
        "peek",
        "Peek Definition and Peek References: the popup, its excerpt and stepping",
    ),
    "hierarchy": (
        "Call and type hierarchies: asking for the callers, callees, supertypes or subtypes at\n"
        "the caret, and the picker their answer opens.",
        "hierarchy",
        "call and type hierarchies: the requests and the picker of answers",
    ),
    "capabilities": (
        "The active editor's language and whether its server supports a feature (declaration,\n"
        "type definition, implementations, references, call hierarchy, code actions): the gates\n"
        "on the right-click menu's rows.",
        "language capability",
        "the active file's language and which features its server supports",
    ),
    "breadcrumbs": (
        "The breadcrumbs bar and sticky scroll: the crumbs for the caret, each crumb's picker of\n"
        "folders, files or sibling symbols and its filter, View: Focus and Select Breadcrumbs,\n"
        "and the scope headers pinned above the editor.",
        "breadcrumb",
        "breadcrumbs, their pickers, and the sticky-scroll headers",
    ),
    "outline": (
        "The OUTLINE: keeping it in step with the active editor, the syntax-tree outline (parsed\n"
        "off the UI thread for a large buffer) until the server's `documentSymbol` answer lands,\n"
        "the secondary side bar that shows it, and the grammar symbol tabs parse with.",
        "outline",
        "the OUTLINE: syntax and server symbols, and the secondary side bar",
    ),
    "typing": (
        "What a server does as you type: on-type formatting, linked editing of paired tags, the\n"
        "characters that open parameter hints, and user snippets as completion items.",
        "as-you-type",
        "on-type formatting, linked editing, parameter-hint triggers and snippet items",
    ),
    "selection": (
        "Expand and Shrink Selection by the server's selection ranges, and highlighting the\n"
        "other occurrences of the symbol under a resting caret.",
        "smart select and highlight",
        "Expand and Shrink Selection, and occurrence highlights",
    ),
    "workspace_edits": (
        "Applying a server's workspace edit: open buffers in any split edited in memory as one\n"
        "undo step, closed files written on disk, and refusing an edit to a buffer typed into\n"
        "since the request.",
        "workspace edit",
        "applying workspace edits to buffers and files, and refusing stale ones",
    ),
    "rename": (
        "Rename Symbol (F2): the prompt, pre-filled from prepare-rename or the word at the caret,\n"
        "the rename request, and landing its reply.",
        "rename",
        "Rename Symbol: the prompt, prepare-rename and landing the reply",
    ),
    "format": (
        "Format Document and Format Selection: the server's reply or Prettier's run applied to\n"
        "the tab it formats, and format-on-save, which writes the file unformatted once its\n"
        "formatter has taken too long.",
        "formatting",
        "Format Document and Selection, Prettier, and format-on-save",
    ),
    "code_actions": (
        "Quick Fix (Cmd+.): the range and diagnostics it asks about, the picker of actions, and\n"
        "applying the chosen one's edit and command, resolving it first when the server sent it\n"
        "bare.",
        "code action",
        "Quick Fix: requesting, picking, resolving and applying code actions",
    ),
    "go_to_symbol": (
        "Go to Symbol in Editor (Cmd/Ctrl+Shift+O): opening it on the active file's outline,\n"
        "its keys, mouse and drawing, and jumping to the chosen symbol.",
        "Go to Symbol in Editor",
        "Go to Symbol in Editor: the picker, its keys, mouse and drawing",
    ),
    "workspace_symbols": (
        "Go to Symbol in Workspace (`#` in Quick Open): the debounced query to every running\n"
        "server, its replies, the picker's keys, mouse and drawing, and jumping to the chosen\n"
        "symbol.",
        "Go to Symbol in Workspace",
        "Go to Symbol in Workspace: the debounced query, replies and the picker",
    ),
    "code_lens": (
        "CodeLens: asking for the active file's lenses once its edits settle, landing them on\n"
        "every tab showing the file, running one by click or from the keyboard, and Editor:\n"
        "Toggle CodeLens.",
        "CodeLens",
        "CodeLens: requesting, landing, running and toggling lenses",
    ),
    "hover_text": (
        "What a hover says: the diagnostics under the pointer as one block, errors first, above\n"
        "the server's hover text.",
        "hover text",
        "the hover's text: diagnostics first, then the server's answer",
    ),
}

LANGUAGE_SERVERS = Target(
    "language_servers", "scripts/split_language_servers_manifest.tsv",
    "src/app/language_servers.rs", "src/app/language_servers",
    LANGUAGE_SERVERS_FILES,
    Tree(
        "│   ├── language_servers.rs ",
        "│   ├── language_servers/ the language server feature files, beside "
        "language_servers.rs; each file's `//!` doc says what it holds",
        "│   │   ├── ", 20,
    ),
    "language-servers-",
    # Its tests are already their own file, src/app/tests/lsp.rs.
    None,
)

# The debug session's feature files (#2062): (module doc, release-note phrase, docs row).
DAP_SESSION_FILES = {
    "events": (
        "Reading what an adapter sends: a raw DAP message classified into the [`DapEvent`]\n"
        "the app acts on, which program output is worth showing, and why an adapter refused\n"
        "to start.",
        "debug event",
        "a raw DAP message classified into a `DapEvent`, the output worth showing, and why "
        "an adapter refused to start",
    ),
    "start_requests": (
        "The requests that start and end a session: `initialize`, each adapter family's\n"
        "`launch` (debugpy, lldb-dap, delve and vscode-js-debug), `configurationDone` and\n"
        "`disconnect`, and the adapter families themselves.",
        "debug start request",
        "`initialize`, each adapter family's `launch`, `configurationDone` and "
        "`disconnect`",
    ),
    "child_sessions": (
        "vscode-js-debug's child sessions: its `startDebugging` reverse request, the answer\n"
        "every reverse request needs, and opening the child.",
        "js-debug child session",
        "vscode-js-debug's `startDebugging` reverse request, the answer every reverse "
        "request needs, and opening the child",
    ),
    "breakpoints": (
        "Breakpoints: line, function and data breakpoints and logpoints, what the adapter\n"
        "reports back about each, pushing them mid-session, and Run to Cursor's one-stop\n"
        "breakpoint.",
        "debug breakpoint",
        "line, function and data breakpoints, what the adapter reports back, and Run to "
        "Cursor",
    ),
    "exceptions": (
        "Exception breakpoints: the filters an adapter offers, which of them mean \"break on\n"
        "raised exceptions\", turning those on and off, and the one-line summary of what was\n"
        "thrown.",
        "exception breakpoint",
        "the exception filters an adapter offers, breaking on raised exceptions, and what "
        "was thrown",
    ),
    "stepping": (
        "Running and stepping: pause, continue, step over, into and out, and Step Into\n"
        "Target's choice among the calls on the line.",
        "debug stepping",
        "pause, continue, step over, into and out, and Step Into Target",
    ),
    "inspection": (
        "Inspecting a stopped program: threads, the call stack, scopes and variables,\n"
        "expensive scopes, evaluate and the hover's lookup, and the locals inline values\n"
        "draw from.",
        "debug inspection",
        "threads, the call stack, scopes and variables, evaluate, and the locals inline "
        "values draw from",
    ),
    "running_set": (
        "The debug sessions in flight: each one's name, Debug Console streams and background\n"
        "backlog, and which one is focused.",
        "debug session set",
        "the sessions in flight: their names, console streams and backlogs, and which one "
        "is focused",
    ),
    "launch": (
        "Starting and ending a session: spawning or reaching each adapter (debugpy, lldb-dap,\n"
        "delve, vscode-js-debug, a server already listening), where delve builds the program,\n"
        "the fresh session around its transport, and disconnecting.",
        "debug launch",
        "spawning or reaching each adapter, delve's build folder, the fresh session, and "
        "disconnecting",
    ),
    "messages": (
        "The per-frame poll: draining what the adapters sent, the handshake, each message's\n"
        "effect on the session, and which connection and capabilities user-facing requests\n"
        "go to.",
        "debug message",
        "the per-frame poll: the handshake, each message's effect on the session, and the "
        "adapter's capabilities",
    ),
}

DAP_SESSION = Target(
    "dap_session", "scripts/split_dap_session_manifest.tsv", "src/dap/session.rs",
    "src/dap/session", DAP_SESSION_FILES,
    Tree(
        # remote/session.rs's row starts the same way, and comes first.
        "│   ├── session.rs            one debug launch session",
        "│   ├── session/         the debug session's feature files, beside session.rs; "
        "each file's `//!` doc says what it holds",
        "│   │   ├── ", 20,
    ),
    "dap-session-",
    (
        "The debug session's own tests, moved out of session.rs (#2062).",
        "the debug session's tests",
        "the debug session's own tests",
    ),
    {
        "session_set_tests": (
            "The tests of the set of debug sessions in flight, moved out of session.rs (#2062).",
            "the session set's tests",
            "the tests of the set of debug sessions in flight",
        ),
    },
)

# The Explorer's file tree's feature files (#2062): (module doc, release-note phrase, docs row).
FILE_TREE_FILES = {
    "roots": (
        "The workspace roots the Explorer lists: replacing the primary root, adding and\n"
        "removing the others' sections, and which root owns a path.",
        "Explorer root",
        "the workspace roots: replacing the primary, adding and removing the others, and "
        "which owns a path",
    ),
    "decorations": (
        "How a row looks: its git decoration and name colour, the folder icon's colour, the\n"
        "active indent guide, and whether a path is ignored, gone at the scrubbed commit, or\n"
        "changed by an agent and not yet reviewed.",
        "Explorer row decoration",
        "a row's git decoration, name and icon colours, the active indent guide, and "
        "ignored or agent-changed paths",
    ),
    "selection": (
        "Selecting rows: the cursor, the multi-selection and its shift-anchor, Select All,\n"
        "and the paths Cut, Copy, Delete and drag act on.",
        "Explorer selection",
        "the cursor, the multi-selection and its anchor, and the paths an action takes",
    ),
    "listing": (
        "Listing a folder's entries: reading them, Explorer order, compact folders, loading\n"
        "and refreshing children with their expansion kept, and which folder a file system\n"
        "event refreshes.",
        "Explorer listing",
        "reading a folder, Explorer order, compact folders, and refreshing children on "
        "file system events",
    ),
    "navigation": (
        "Moving through the tree: the arrow, page, Home and End keys with and without\n"
        "Shift, scrolling, expanding and collapsing, Collapse All, revealing a path, and\n"
        "type-to-find.",
        "Explorer navigation",
        "the arrow, page, Home and End keys, scrolling, expanding and collapsing, "
        "revealing a path, and type-to-find",
    ),
    "sticky": (
        "Sticky scroll: the folders pinned at the top while their contents scroll, and a\n"
        "click on one.",
        "Explorer sticky scroll",
        "the folders pinned at the top while their contents scroll",
    ),
    "new_and_rename": (
        "New File, New Folder and Rename: where a new entry goes, checking the name typed,\n"
        "creating it, and renaming, a respelling that only changes case included.",
        "Explorer new and rename",
        "New File, New Folder and Rename: where an entry goes, checking its name, and "
        "renaming",
    ),
    # Not `trash`: a module of that name in file_tree.rs would shadow the `trash` crate
    # its code calls as `trash::delete`.
    "deletion": (
        "Delete: which rows may be deleted, and moving one or many paths to the system's\n"
        "trash, recoverably.",
        "Explorer delete",
        "which rows may be deleted, and moving paths to the system's trash",
    ),
    "move_and_copy": (
        "Pasting and dropping: a destination name that collides with nothing, refusing a\n"
        "folder into itself, and moving or copying across file systems.",
        "Explorer move and copy",
        "pasting and dropping: a free destination name, no folder into itself, moving and "
        "copying",
    ),
    "filter": (
        "The tree filter: VS Code's fuzzy match of what was typed, and a row's name with the\n"
        "matched letters bold.",
        "Explorer filter",
        "the tree filter's fuzzy match, and the matched letters bold in a row's name",
    ),
    "drawing": (
        "Drawing the tree: its rows, icons, marks and guides, the sticky band, the scrollbar,\n"
        "and the dot on a file an agent changed.",
        "Explorer drawing",
        "drawing the rows, icons, guides, sticky band and scrollbar",
    ),
}

FILE_TREE = Target(
    "file_tree", "scripts/split_file_tree_manifest.tsv", "src/widgets/file_tree.rs",
    "src/widgets/file_tree", FILE_TREE_FILES,
    Tree(
        "    ├── file_tree.rs ",
        "    ├── file_tree/        the Explorer's file tree's feature files, beside "
        "file_tree.rs; each file's `//!` doc says what it holds",
        "    │   ├── ", 18,
    ),
    "file-tree-",
    (
        "The Explorer's file tree's own tests, moved out of file_tree.rs (#2062).",
        "the file tree's tests",
        "the file tree's own tests",
    ),
    {
        "android_trash": (
            "Moving an entry to the trash on Android, where the `trash` crate has none: the\n"
            "freedesktop.org home trash, moved out of file_tree.rs (#2062).",
            "the Android trash's functions",
            "the freedesktop home trash on Android, where the `trash` crate has none",
        ),
    },
)

TARGETS = {
    t.name: t
    for t in (
        APP, EDITOR, LSP, TERMINAL, GIT, REMOTE, SEARCH, SESSION_HOST, PAIR, SOURCE_CONTROL,
        CLI, CODEQL, DEBUGGER, LANGUAGE_SERVERS, DAP_SESSION, FILE_TREE,
    )
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
# A name code outside a nested split found through its parent's glob, bare
# or as `super::name`, which the split file's own private `use <file>::*;` no
# longer passes on.
NOT_FOUND = re.compile(r"^(?:cannot find [a-z ,]+ `(\w+)` in (?:this scope|module `[\w:]+`)"
                       r"|failed to resolve: use of undeclared type `(\w+)`)$")


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


def respell(vis, src=None, dst=None):
    """The visibility (whitespace-free, as `cpm` keeps it) that lets an item
    moved from module `src` to module `dst` reach exactly what `vis` reached
    in `src`: `pub(super)` for a private item moving a level down, and
    `pub(in crate::app)` for a `pub(super)` one moving from src/app/codeql.rs
    into src/app/codeql/. Without the two modules (or a reach that cannot be
    told), a private item gets `pub(super)` and any other keeps `vis`."""
    reach = cpm.scope(vis, src) if dst is not None else None
    if reach is None or dst[: len(reach)] != reach:
        return vis or "pub(super)"
    return re.sub(r"\s+", "", cpm.spelled(reach, dst))


def widen(lines, it, kinds, clines, ctx, src=None, dst=None):
    """`lines` of item `it` with its visibility spelled by `respell` for a
    move from module `src` to module `dst`: `pub(super) ` put before a
    private declaration, or the old visibility replaced. An item that cannot
    carry a visibility, or keeps its spelling, is as it is."""
    body = lines[it.start : it.end + 1]
    if it.kind not in VISIBLE or (ctx and TRAIT_IMPL.search(ctx)):
        return body
    want = respell(it.vis, src, dst)
    if want == it.vis:
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
    end = at
    if it.vis and (m := cpm.VIS.match(lines[j], at)):
        at, end = m.start(1), m.end(1)
    body[j - it.start] = lines[j][:at] + cpm.shown(want) + " " + lines[j][end:]
    return body


def render_items(lines, picked, blocks, kinds, clines, gone=frozenset(), src=None, dst=None):
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
        out.extend(widen(lines, it, kinds, clines, ctx, src, dst))
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
    its top, a blank line between. The top ends at the file's first item:
    a split file that has shrunk keeps its `#[cfg(test)] mod tests;` at the
    bottom, below its last struct, and a new file's `mod` goes with the
    others, not beside that one."""
    decl = re.compile(r"^(pub(\(\w+\))? )?mod \w+;$")
    items, _ = cpm.parse("\n".join(lines))
    first = min((it.start for it in items), default=len(lines))
    mods = [n for n, ln in enumerate(lines[:first]) if decl.match(ln)]
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
    src, dst = cpm.module_of(t.source), cpm.module_of(f"{t.dir}/{file}.rs")
    body = render_items(lines, picked, blocks, kinds, clines, emptied(picked, blocks, items),
                        src, dst)
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
    respelled = Counter(
        (cpm.shown(it.vis), cpm.shown(respell(it.vis, src, dst))) for it in picked
        if it.kind in VISIBLE and not (enclosing(blocks, it) and TRAIT_IMPL.search(it.ctx or ""))
        and respell(it.vis, src, dst) != it.vis
    )
    print(f"PR {pr}: moved {len(picked)} item(s) from {t.source} to {t.dir}/{file}.rs"
          + "".join(f", {k} {a} -> {b}" for (a, b), k in sorted(respelled.items())))
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


def reexport_spelling(vis, file, paths=None):
    """How the split file spells a re-export of an item of visibility `vis`
    in its feature file `file`, so the re-export reaches what the item
    reaches, or None when that is no further than the split file itself and
    no user outside it could have reached the item. `paths` is the split
    file and its folder; without them a `pub` or `pub(crate)` item keeps its
    spelling and nothing else is re-exported."""
    if paths is None:
        return vis if vis in ("pub", "pub(crate)") else None
    source, folder = paths
    split = cpm.module_of(source)
    reach = cpm.scope(re.sub(r"\s+", "", vis), cpm.module_of(f"{folder}/{file}.rs"))
    if split is None or reach is None or len(reach) >= len(split) or split[:len(reach)] != reach:
        return None
    return cpm.shown(cpm.spelled(reach, split))


def glob_line(lines, file):
    """The line of `use <file>::*;` in mod.rs, or None."""
    return next((n for n, ln in enumerate(lines) if ln == f"use {file}::*;"), None)


def reexport_line(lines, file, name):
    """The line of a re-export `<vis> use <file>::<name>;` in mod.rs, or None."""
    pat = re.compile(rf"^pub(\([^)]*\))? use {file}::{name};$")
    return next((n for n, ln in enumerate(lines) if pat.match(ln)), None)


def item_cfgs(text):
    """The `#[cfg(...)]` lines on an item, from its text: the attributes
    among its doc comment, above its declaration."""
    cfgs = []
    for line in text.split("\n"):
        line = line.strip()
        if line.startswith("#[cfg(") and line.endswith(")]"):
            cfgs.append(line)
        elif not (line.startswith(("//", "#[")) or not line):
            break
    return cfgs


def fix_glue(lines, app, tests, homes, scope="app", paths=None):
    """mod.rs `lines` with the glue the build errors ask for, and what was
    done. `scope` names the module being split, for the messages. `app` and `tests` are the errors of the two builds; `homes` maps a
    moved free item's name to (file, visibility) or (file, visibility,
    cfgs): `cfgs` are the item's own `#[cfg(...)]` lines, which its
    re-export takes too, so a test-only item is re-exported for the tests
    alone and the app build does not look for it. `paths` is the split
    file and the folder its files go to, so a re-export is spelled from
    the reach the item has in its file; without them only `pub` and
    `pub(crate)` items, which read the same anywhere, are re-exported.
    Raises BuildError when an error is not one this fixes.

    An item is re-exported when code outside the split file names it: by a
    path through the split file's private glob (E0603), or, when the split
    file sits below another file that globs it (src/app/mod.rs's `use
    debugger::*;`), by the bare name that glob used to pass on and the
    split file's own private `use <file>::*;` does not (E0425 and kin)."""
    lines, done, unknown = list(lines), [], []
    private, unused = set(), {"app": set(), "tests": set()}
    unused_one = {"app": set(), "tests": set()}
    for side, errors in (("app", app), ("tests", tests)):
        for code, message in errors:
            if code == "E0603" and (m := PRIVATE_IMPORT.match(message)) and m[1] in homes:
                private.add(m[1])
            elif (m := NOT_FOUND.match(message)) and (m[1] or m[2]) in homes:
                private.add(m[1] or m[2])
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
        file, vis, *rest = homes[name]
        cfgs = list(rest[0]) if rest else []
        again = reexport_spelling(vis, file, paths)
        if again is None:
            shown = cpm.shown(re.sub(r"\s+", "", vis))
            raise BuildError(f"{name} is used outside {scope} but is {shown} in {file}.rs")
        at = glob_line(lines, file)
        if at is None:
            at = next(n for n, ln in enumerate(lines) if ln == f"mod {file};")
        lines[at + 1:at + 1] = cfgs + [f"{again} use {file}::{name};"]
        only = f" only under {', '.join(c[2:-1] for c in cfgs)}" if cfgs else ""
        done.append(f"re-exported {file}::{name} ({again}){only}: used outside {scope}")
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
                        vis[(file, it.name)] = (it.vis, item_cfgs(it.norm))
        homes = {n: (f, *vis[(f, n)]) for n, f in homes.items() if (f, n) in vis}
        with open(mod_path, encoding="utf-8") as f:
            lines = f.read().split("\n")
        lines, now = fix_glue(lines, app, tests, homes, t.name, (t.source, t.dir))
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
    """The header line of an inline `mod <name> {`, with or without a
    visibility: dap/session.rs's tests are `pub(crate) mod tests`, as the
    app's tests borrow its fake adapters."""
    return re.compile(rf"(^|\n)\s*(?:pub(?:\([^()]*\))?\s+)?mod\s+{re.escape(name)}\s*\{{$")


def tests_out(root, t, fmt=True, name="tests"):
    """Move the inline `mod <name> { ... }` (`mod tests` unless the target
    names another module) at the top level of the target's file to
    <dir>/<name>.rs: the block's body, dedented by its indentation, and
    `mod <name>;` where the block was, keeping the module's visibility. The
    module keeps its name and its place, so its `use super::*;` still
    reaches the split file. The header's attributes (a `#[cfg(...)]`
    included) and the docs and comments above it stay. A line that starts
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
    print(f"moved {moved} line(s) of `mod {name}` from {t.source} to {t.dir}/{name}.rs")
    return moved


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--target", default="app", choices=sorted(TARGETS), help="the file to split")
    ap.add_argument("--pr", type=int, help="the PR whose items to move")
    ap.add_argument("--check", action="store_true", help="only check the manifest")
    ap.add_argument("--tests-out", nargs="?", const="tests", metavar="MODULE",
                    help="move the file's inline `mod tests` (or the inline module named) to its own file")
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
