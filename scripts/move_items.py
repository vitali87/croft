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
part of the string. A test file can then be split like any other target.

Usage:
  move_items.py [--target T] --pr N    move PR N's items (run from the repository root)
  move_items.py [--target T] --check   only check the manifest against mod.rs
  move_items.py --target T --tests-out move mod.rs's inline tests to their own file
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
Target = namedtuple("Target", "name manifest source dir files tree note_prefix tests")

APP = Target(
    "app", MANIFEST, MOD, "src/app", FILES,
    Tree("│   ├── mod.rs", None, "│   ├── ", 16), "", None,
)

# What each file split out of src/widgets/editor.rs holds, as FILES above.
EDITOR_FILES = {}

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

TARGETS = {t.name: t for t in (APP, EDITOR)}

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
    if any(ln.startswith(f"{tree.row}{name} ") for ln in lines):
        return text
    start = next((n for n, ln in enumerate(lines) if ln.startswith(tree.anchor)), None)
    if start is None:
        raise ManifestError(f"{ARCH} has no row starting {tree.anchor.strip()!r} to list {name} by")
    if tree.folder:
        if not lines[start + 1].startswith(tree.folder.split("/")[0] + "/"):
            lines.insert(start + 1, tree.folder)
        start += 1
    row = re.compile("^" + re.escape(tree.row) + r"(\S+)")
    n = start + 1
    while n < len(lines) and (m := row.match(lines[n])) and m.group(1) < name \
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
            existing = f.read().rstrip("\n").split("\n")
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
        for line in settle(root, rows, t=t):
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
    if unknown:
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


def settle(root, rows, rounds=5, t=APP):
    """Build, fix the glue the build asks for, and build again until the app
    and its tests are clean. Returns what was done."""
    done = []
    mod_path = os.path.join(root, t.source)
    for _ in range(rounds):
        app, tests = cargo_errors(root, "--bins"), cargo_errors(root, "--tests")
        if not app and not tests:
            return done
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


TESTS_HEADER = re.compile(r"(^|\n)\s*mod\s+tests\s*\{$")


def tests_out(root, t, fmt=True):
    """Move the inline `mod tests { ... }` at the top level of the target's
    file to <dir>/tests.rs: the block's body, dedented by its indentation,
    and `mod tests;` where the block was. The header's attributes and the
    docs and comments above it stay. A line that starts inside a string keeps
    its indentation, which is part of the string. Writes the release note and
    the docs row too. Returns the number of lines moved."""
    if not t.tests:
        raise ManifestError(f"the {t.name} target has no words for its tests")
    src = os.path.join(root, t.source)
    with open(src, encoding="utf-8") as f:
        text = f.read()
    _, glue = cpm.parse(text, t.source)
    heads = [g for g in glue if g.kind == "header" and not g.scope and TESTS_HEADER.search(g.text)]
    if len(heads) != 1:
        raise ManifestError(f"{t.source} has {len(heads)} top-level `mod tests {{` blocks; expected one")
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
    doc = ["//! " + ln if ln else "//!" for ln in t.tests[0].split("\n")]
    target = os.path.join(root, t.dir, "tests.rs")
    if os.path.exists(target):
        raise ManifestError(f"{t.dir}/tests.rs exists already")
    decl = re.sub(r"mod\s+tests\s*\{$", "mod tests;", lines[open_line])
    rest = lines[:open_line] + [decl] + lines[close + 1:]
    arch = os.path.join(root, ARCH)
    with open(arch, encoding="utf-8") as f:
        arch_text = add_arch_row(f.read(), "tests", t, t.tests[2])
    write_all([
        (target, "\n".join(doc + [""] + out).rstrip("\n") + "\n"),
        (src, "\n".join(rest)),
        (os.path.join(root, NOTES, f"2062-split-{t.note_prefix}tests.md"),
         f"refactor: Groundwork for the refactor program (#2062): {t.tests[1]} moved "
         f"to their own file, {t.dir}/tests.rs. Nothing changes in use.\n"),
        (arch, arch_text),
    ])
    if fmt:
        subprocess.run(["cargo", "fmt"], cwd=root, check=True)
    moved = close - open_line - 1
    print(f"moved {moved} line(s) of tests from {t.source} to {t.dir}/tests.rs")
    return moved


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--target", default="app", choices=sorted(TARGETS), help="the file to split")
    ap.add_argument("--pr", type=int, help="the PR whose items to move")
    ap.add_argument("--check", action="store_true", help="only check the manifest")
    ap.add_argument("--tests-out", action="store_true",
                    help="move the file's inline `mod tests` to its own file")
    ap.add_argument("--root", default=".", help="the repository root")
    ap.add_argument("--no-fmt", action="store_true", help="skip `cargo fmt` (for tests)")
    ap.add_argument("--no-build", action="store_true", help="skip the build and its glue fixes (for tests)")
    args = ap.parse_args(argv)
    t = TARGETS[args.target]
    try:
        if args.tests_out:
            tests_out(args.root, t, fmt=not args.no_fmt)
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
