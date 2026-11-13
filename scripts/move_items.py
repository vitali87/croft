#!/usr/bin/env python3
"""Move one stage-B pull request's items out of src/app/mod.rs, unchanged.

Stage B of the refactor program (#2062) splits src/app/mod.rs into feature
files under src/app/. scripts/split_manifest.tsv says where every item of
mod.rs goes: a PR number, 0 for an item that stays, and a file. For one PR
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

- an item used outside `app` (as `crate::app::name`) is private through the
  glob, so mod.rs re-exports it by name at the item's own visibility;
- a `use <file>::*;` that only the tests need gets `#[cfg(test)]` (and one
  only the app needs, `#[cfg(not(test))]`); one nothing needs goes.

Any other error stops it, with the error. Prove the result with
`scripts/check_pure_move.py <base> HEAD`.

It refuses to run when the manifest and mod.rs disagree: an item the
manifest names is not in mod.rs, or mod.rs has an item the manifest does not
place (one added since the manifest was written). Add or fix its row (a PR
number, or 0 to keep it) and run again.

Usage:
  move_items.py --pr N    move PR N's items (run from the repository root)
  move_items.py --check   only check the manifest against mod.rs
"""

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict

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


def landed(root, rows):
    """{file: Counter of item keys} for the files the manifest moves items
    to that exist already: what earlier moves put there."""
    out = {}
    for file in sorted({file for pr, file, *_ in rows if pr}):
        path = os.path.join(root, "src", "app", f"{file}.rs")
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
    file has free items, `use file::*;` after the `use` lines that follow."""
    decl = re.compile(r"^(pub(\(\w+\))? )?mod \w+;$")
    last_mod = max(n for n, ln in enumerate(lines[:200]) if decl.match(ln))
    lines.insert(last_mod + 1, f"mod {file};")
    if free:
        n = last_mod + 2
        while n < len(lines) and re.match(r"^use \w+(::\{[^}]*\}|::\w+)*;$", lines[n]):
            n += 1
        lines.insert(n, f"use {file}::*;")
    return lines


def note_path(file, half):
    suffix = f"-{half}" if half else ""
    return os.path.join(NOTES, f"2062-split-{file.replace('_', '-')}{suffix}.md")


def note_text(file, half):
    what = FILES[file][1]
    part = {1: "the first part of the ", 2: "the rest of the "}.get(half, "the ")
    return (f"refactor: Groundwork for the refactor program (#2062): {part}{what} code "
            f"moved to its own file, src/app/{file}.rs. Nothing changes in use.\n")


def add_arch_row(text, file):
    """docs/ARCHITECTURE.md with a row for src/app/<file>.rs in the app/ tree,
    in name order after mod.rs."""
    lines = text.split("\n")
    row = re.compile(r"^│   ├── (\S+)")
    start = next(n for n, ln in enumerate(lines) if ln.startswith("│   ├── mod.rs"))
    name = f"{file}.rs"
    if any(ln.startswith(f"│   ├── {name} ") for ln in lines):
        return text
    n = start + 1
    while (m := row.match(lines[n])) and m.group(1) < name and not m.group(1).endswith("/"):
        n += 1
    lines.insert(n, f"│   ├── {name:<16} {FILES[file][2]}")
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


def move(root, pr, fmt=True, build=True):
    rows = read_manifest(os.path.join(root, MANIFEST))
    mod_path = os.path.join(root, MOD)
    with open(mod_path, encoding="utf-8") as f:
        text = f.read()
    items, glue = cpm.parse(text, MOD)
    placed = place(items, rows, landed(root, rows))
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

    target = os.path.join(root, "src", "app", f"{file}.rs")
    if os.path.exists(target):
        with open(target, encoding="utf-8") as f:
            existing = f.read().rstrip("\n").split("\n")
        new_file = existing + [""] + body
        glue_free = free and not any(re.match(rf"^use {file}::\*;$", ln) for ln in lines)
    else:
        doc = ["//! " + ln if ln else "//!" for ln in FILES[file][0].split("\n")]
        new_file = doc + ["", "use super::*;", ""] + body
        glue_free = free
    rest = cut(lines, picked, blocks, items)
    if not os.path.exists(target):
        rest = add_glue(rest, file, glue_free)
    elif glue_free:
        rest = add_glue_use_only(rest, file)
    with open(target, "w", encoding="utf-8") as f:
        f.write("\n".join(new_file).rstrip("\n") + "\n")
    with open(mod_path, "w", encoding="utf-8") as f:
        f.write("\n".join(rest))
    with open(os.path.join(root, note_path(file, half)), "w", encoding="utf-8") as f:
        f.write(note_text(file, half))
    arch = os.path.join(root, ARCH)
    with open(arch, encoding="utf-8") as f:
        text = f.read()
    with open(arch, "w", encoding="utf-8") as f:
        f.write(add_arch_row(text, file))
    if fmt:
        subprocess.run(["cargo", "fmt"], cwd=root, check=True)
    widened = sum(1 for it in picked if not it.vis and it.kind in VISIBLE
                  and not (enclosing(blocks, it) and TRAIT_IMPL.search(it.ctx or "")))
    print(f"PR {pr}: moved {len(picked)} item(s) from {MOD} to src/app/{file}.rs, "
          f"{widened} widened to pub(super)")
    if build:
        for line in settle(root, rows):
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


def fix_glue(lines, app, tests, homes):
    """mod.rs `lines` with the glue the build errors ask for, and what was
    done. `app` and `tests` are the errors of the two builds; `homes` maps a
    moved free item's name to (file, visibility). Raises BuildError when an
    error is not one this fixes."""
    lines, done, unknown = list(lines), [], []
    private, unused = set(), {"app": set(), "tests": set()}
    for side, errors in (("app", app), ("tests", tests)):
        for code, message in errors:
            if code == "E0603" and (m := PRIVATE_IMPORT.match(message)) and m[1] in homes:
                private.add(m[1])
            elif code == "unused_imports" and (m := UNUSED_GLOB.match(message)) \
                    and glob_line(lines, m[1]) is not None:
                unused[side].add(m[1])
            else:
                unknown.append(f"{side}: {code}: {message}")
    if unknown:
        raise BuildError("the build fails in a way this tool does not fix:\n  " + "\n  ".join(unknown))
    for name in sorted(private):
        file, vis = homes[name]
        if vis not in ("pub", "pub(crate)"):
            raise BuildError(f"{name} is used outside app but is {vis or 'private'} in {file}.rs")
        at = glob_line(lines, file)
        if at is None:
            at = next(n for n, ln in enumerate(lines) if ln == f"mod {file};")
        lines.insert(at + 1, f"{vis} use {file}::{name};")
        done.append(f"re-exported {file}::{name} ({vis}): used outside app")
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
    return lines, done


def settle(root, rows, rounds=5):
    """Build, fix the glue the build asks for, and build again until the app
    and its tests are clean. Returns what was done."""
    done = []
    mod_path = os.path.join(root, MOD)
    for _ in range(rounds):
        app, tests = cargo_errors(root, "--bins"), cargo_errors(root, "--tests")
        if not app and not tests:
            return done
        homes = {}
        for pr, file, kind, impl, name in rows:
            path = os.path.join(root, "src", "app", f"{file}.rs")
            if pr and impl == "-" and os.path.exists(path):
                homes.setdefault(name, file)
        vis = {}
        for file in set(homes.values()):
            path = os.path.join(root, "src", "app", f"{file}.rs")
            with open(path, encoding="utf-8") as f:
                for it in cpm.parse(f.read(), path)[0]:
                    if not it.ctx and not it.mods:
                        vis[(file, it.name)] = it.vis
        homes = {n: (f, vis[(f, n)]) for n, f in homes.items() if (f, n) in vis}
        with open(mod_path, encoding="utf-8") as f:
            lines = f.read().split("\n")
        lines, now = fix_glue(lines, app, tests, homes)
        with open(mod_path, "w", encoding="utf-8") as f:
            f.write("\n".join(lines))
        subprocess.run(["cargo", "fmt"], cwd=root, check=True)
        done += now
    raise BuildError(f"still failing after {rounds} rounds of fixes: {done}")


def check(root):
    rows = read_manifest(os.path.join(root, MANIFEST))
    with open(os.path.join(root, MOD), encoding="utf-8") as f:
        items, _ = cpm.parse(f.read(), MOD)
    placed = place(items, rows, landed(root, rows))
    left = Counter(p[0] for p in placed)
    todo = len([k for k in left if k])
    done = len({pr for pr, *_ in rows if pr}) - todo
    print(f"The manifest places all {len(items)} item(s) of {MOD}; "
          f"{left[0]} stay, the rest move in {todo} PR(s); {done} PR(s) moved already.")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--pr", type=int, help="the PR whose items to move")
    ap.add_argument("--check", action="store_true", help="only check the manifest")
    ap.add_argument("--root", default=".", help="the repository root")
    ap.add_argument("--no-fmt", action="store_true", help="skip `cargo fmt` (for tests)")
    ap.add_argument("--no-build", action="store_true", help="skip the build and its glue fixes (for tests)")
    args = ap.parse_args(argv)
    try:
        if args.check:
            return check(args.root)
        if args.pr is None:
            ap.error("give --pr N or --check")
        move(args.root, args.pr, fmt=not args.no_fmt, build=not args.no_build)
        return 0
    except (ManifestError, BuildError, cpm.ParseError) as e:
        print(f"move_items: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
