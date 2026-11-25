#!/usr/bin/env python3
"""Whether one file's change between two revisions ships nothing.

The release gate exempts "docs / CI / test-only changes" and encodes that as a
path list, so `src/app/tests/` and `tests/` are exempt while a change
confined to a `#[cfg(test)] mod tests` inside a shipped file is not (#461).
croft keeps most unit tests beside the code they cover, so that is the ordinary
shape of a test-only fix here: the release binary is byte-identical, and the
only way to satisfy the gate is to write user-facing notes for a change the
binary does not contain.

The two ways to be wrong are not symmetric. Waiving notes that were needed
ships a change no note describes, and with no fragment pending the
version-bump workflow releases nothing for it, so two different binaries share
a version; asking for notes that were not needed costs a line of notes. So
every uncertainty resolves to "ships": a module whose opening brace is not on
the `mod` line, a `#[cfg(test)]` on anything other than a module, a file that
is not Rust.

Usage: ships_nothing.py <base> <head> <path>
Exit 0 when the change ships nothing, 1 when it ships something.
"""

import functools
import posixpath
import re
import subprocess
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_doc_ownership import (  # noqa: E402
    ATTR,
    CODE,
    DOC,
    MISSING_PATH,
    SKIP,
    BlockTracker,
    classify,
)

# The attribute on its own line, which is what rustfmt produces and what every
# test module in this repo has. `#[cfg(test)] mod tests {` on one line is legal
# and is deliberately not matched: it is not a shape that occurs here, and
# missing it only costs a release-notes fragment.
CFG_TEST = re.compile(r"^\s*#\[cfg\(test\)\]\s*$")

MOD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_]\w*")

HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def git(*args, cwd=None, allow_missing_path=False):
    """Run git, tolerating "that path is not in that revision" only where the
    caller says a missing path is a legitimate answer.

    Opt-in for the same reason `check_doc_ownership.git` is: tolerating it
    everywhere lets an invalid revision or a malformed object read as empty
    content, and empty content is the WAIVING answer here.
    """
    done = subprocess.run(
        ("git",) + args, cwd=cwd, capture_output=True, text=True
    )
    if done.returncode != 0:
        if allow_missing_path and MISSING_PATH.search(done.stderr):
            return ""
        raise SystemExit(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout


# A module declared without a body, `mod name;` (its body is another file).
# Matched against a line's code, so a trailing `// comment` does not hide it.
MOD_DECLARATION = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_]\w*\s*;\s*$")

def source_lines(text):
    """`text`'s lines as git and rustc count them: split at `\\n` only.

    `str.splitlines` also breaks at U+2028, U+2029, form feeds and a few
    more, which Rust reads as ordinary characters. A string literal holding
    one (croft's SARIF reader tests U+2028 as a newline sequence) shifted
    every later line by one against the diff's line numbers.
    """
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line.removesuffix("\r") for line in lines]


def literal_lines(text):
    """Line numbers (1-indexed) that sit INSIDE a multi-line string literal.

    A blank line is only whitespace when it is code. Inside a raw string it is
    content: croft's model prompt and its tree-sitter queries are multi-line
    literals, and adding or removing a blank line there changes what the
    binary does. `BlockTracker` already tracks the open-literal state for its
    brace counting, so this reads it rather than re-deriving it.
    """
    lines = source_lines(text)
    kinds = classify(lines)
    tracker = BlockTracker()
    inside = set()
    for i, line in enumerate(lines):
        if tracker.open_string is not None:
            inside.add(i + 1)
        if kinds[i] == CODE:
            tracker.feed(line)
    return inside


def cfg_test_ranges(text):
    """Line spans (1-indexed, inclusive) of every `#[cfg(test)]` module.

    The span starts at the attribute rather than at the `mod` line, so a diff
    that only edits the attribute itself still reads as test-only. Braces are
    counted through `BlockTracker`, which removes string and char literals
    first: a `format!("{}")` inside a test would otherwise close the module
    early and leave the rest of the file looking like shipped code.

    A block comment anywhere in a line's code yields no spans at all. The
    tracker strips strings, chars and `//` but not `/* */`, so a brace inside
    one is counted as structure and a span can swallow the shipped code below
    it. Rare enough that refusing to answer is better than parsing it. Read
    off the code with its literals removed: a glob in a string
    (`"src/**/*.rs"`) opens no comment, and a comment right after a string
    (`"x" /* { */`) still does.
    """
    lines = source_lines(text)
    kinds = classify(lines)
    stripper = BlockTracker()
    if any(kinds[i] == CODE and "/*" in stripper.feed(l) for i, l in enumerate(lines)):
        return []
    # A line `classify` reads as a comment can still hold code: `/* x */ }`
    # closes a module after its comment, and that brace is never counted.
    # The token reader sets comments aside exactly, so any token it finds on
    # such a line is code this reading would miss.
    token_lines = []
    try:
        rust_tokens(text, token_lines)
    except LexError:
        return []
    if any(kinds[n - 1] in (SKIP, DOC) for n in set(token_lines)):
        return []
    tracker = BlockTracker()
    ranges = []
    attr_at = None
    open_at = None
    for i, line in enumerate(lines):
        depth_before = tracker.depth
        # A line that starts inside a multi-line string is that string's
        # content: a `#[cfg(test)]` or `mod name;` there is text the binary
        # carries, not an item.
        in_literal = tracker.open_string is not None
        code = tracker.feed(line) if kinds[i] == CODE else ""
        if open_at is not None:
            start, outer = open_at
            if tracker.depth <= outer:
                ranges.append((start + 1, i + 1))
                open_at = None
            continue
        if in_literal:
            attr_at = None
            continue
        if kinds[i] == ATTR and CFG_TEST.match(line):
            # Only a line `classify` read as an attribute: the same text in a
            # block comment is SKIP, and guards nothing.
            attr_at = i
            continue
        if attr_at is None:
            continue
        if kinds[i] in (SKIP, ATTR, DOC):
            # Blank lines, comments, doc comments and further attributes all
            # sit legally between `#[cfg(test)]` and the module it applies
            # to. `classify` labels `///` DOC rather than SKIP, and a
            # documented test module is an ordinary shape: missing it loses
            # the exemption silently rather than waiving wrongly.
            continue
        if MOD.match(line) and tracker.depth > depth_before:
            open_at = (attr_at, depth_before)
        elif kinds[i] == CODE and MOD_DECLARATION.match(code):
            # `#[cfg(test)] mod name;`: the declaration is the whole span. Its
            # body is another file, which compiles only for tests too.
            ranges.append((attr_at + 1, i + 1))
        attr_at = None
    # A module still open at EOF is a miscount or a truncated file. Reporting
    # it as a range would exempt everything below the attribute.
    return ranges


def changed_lines(base, head, path, cwd=None):
    """(lines removed from base, lines added at head, the raw diff, the
    hunks as `(base_start, base_len, head_start, head_len)`), 1-indexed.

    The diff text comes back because "no hunk headers" has two causes: the
    file did not change, and git did not describe the change. `git diff -U0`
    on a file git treats as binary prints `Binary files ... differ` and no
    `@@` line at all, and inferring "unchanged" from that waives a bump.
    """
    diff = git("diff", "--no-renames", "-U0", base, head, "--", path, cwd=cwd)
    removed, added, hunks = [], [], []
    for line in diff.splitlines():
        m = HUNK.match(line)
        if not m:
            continue
        hunk = (
            int(m.group(1)),
            int(m.group(2) or 1),
            int(m.group(3)),
            int(m.group(4) or 1),
        )
        base_start, base_len, head_start, head_len = hunk
        hunks.append(hunk)
        removed.extend(range(base_start, base_start + base_len))
        added.extend(range(head_start, head_start + head_len))
    return removed, added, diff, hunks


def counterpart(n, hunks, side):
    """The line an unchanged line `n` was on the other side of the diff:
    `side` is "head" to map a head line to the base, "base" for the reverse.
    None when `n` is itself a changed line.

    With `-U0` a hunk of length 0 sits AFTER its start line, so every line
    past it shifts by what the hunk took out or put in.
    """
    shift = 0
    for base_start, base_len, head_start, head_len in hunks:
        start, length, other = (
            (head_start, head_len, base_len) if side == "head" else (base_start, base_len, head_len)
        )
        end = start + length - 1 if length else start
        if length and start <= n <= end:
            return None
        if n > end:
            shift += other - length
    return n + shift


def inside(line_no, ranges):
    return any(start <= line_no <= end for start, end in ranges)


class LexError(Exception):
    """Text `rust_tokens` cannot read as Rust: an unterminated string or
    comment, or an unclosed bracket. Whatever asked then answers "ships"."""


# Rust's tokens, as far as telling items apart from comments and literals
# needs: a `mod` inside a string or a comment declares nothing, and a real
# declaration can run over lines or carry a comment between its words, which
# a line-by-line reading misses. Numbers and multi-character operators come
# apart into pieces that nothing here looks at.
_TOKEN = re.compile(
    r"""
    (?P<ws>\s+)
    |(?P<line>//[^\n]*)
    |(?P<block>/\*)
    |(?P<raw>[bc]?r(?P<hashes>\#*)")
    |(?P<str>[bc]?"(?:[^"\\]|\\.)*")
    |(?P<char>b?'(?:[^'\\\n]|\\u\{[0-9a-fA-F_]*\}|\\x[0-9a-fA-F]{2}|\\.)')
    |(?P<life>'(?:r\#)?[^\W\d]\w*)
    |(?P<ident>r\#[^\W\d]\w*|[^\W\d]\w*)
    |(?P<num>\d\w*)
    |(?P<punct>.)
    """,
    re.X | re.S,
)

_BLOCK_EDGE = re.compile(r"/\*|\*/")

_OPENERS = {"(": ")", "[": "]", "{": "}"}

# The macros that read a file at compile time, whatever its module tree says.
# `include!` pastes the file in as code, which can declare modules and read
# further files itself; `include_str!` and `include_bytes!` only embed it.
INCLUDE_MACROS = {"include", "include_str", "include_bytes"}

# A name `mod` can declare, so one a file can be the module of.
MODULE_NAME = re.compile(r"[^\W\d]\w*")

CARGO_TARGETS = ("lib", "bin", "test", "bench", "example")


def rust_tokens(text, lines=None):
    """`(kind, text)` for every token of `text` but whitespace and comments.
    Kinds are `ident`, `str`, `char`, `life`, `num` and `punct`. A string
    token's text is its contents, or None when they hold an escape, which
    nothing here decodes. Block comments nest, as Rust's do. Given a list as
    `lines`, each token's 1-indexed line is appended to it."""
    out = []
    pos, end = 0, len(text)
    line, counted = 1, 0
    while pos < end:
        m = _TOKEN.match(text, pos)
        kind = m.lastgroup
        if lines is not None and kind not in ("ws", "line", "block"):
            line += text.count("\n", counted, m.start())
            counted = m.start()
            lines.append(line)
        if kind == "block":
            depth = 1
            for edge in _BLOCK_EDGE.finditer(text, m.end()):
                depth += 1 if edge.group() == "/*" else -1
                if depth == 0:
                    pos = edge.end()
                    break
            else:
                raise LexError("unterminated block comment")
            continue
        if m.group("raw") is not None:
            close = '"' + m.group("hashes")
            stop = text.find(close, m.end())
            if stop < 0:
                raise LexError("unterminated raw string")
            out.append(("str", text[m.end() : stop]))
            pos = stop + len(close)
            continue
        pos = m.end()
        if kind in ("ws", "line"):
            continue
        if kind == "str":
            body = m.group()[m.group().index('"') + 1 : -1]
            out.append(("str", None if "\\" in body else body))
        elif kind == "punct" and m.group() in "\"'":
            raise LexError(f"unterminated {m.group()}")
        else:
            out.append((kind, m.group()))
    return out


def _closing(toks, i):
    """The index of the bracket that closes the one at `toks[i]`."""
    opener = toks[i]
    closer = ("punct", _OPENERS[opener[1]])
    depth = 0
    for j in range(i, len(toks)):
        if toks[j] == opener:
            depth += 1
        elif toks[j] == closer:
            depth -= 1
            if depth == 0:
                return j
    raise LexError("unclosed bracket")


def _opening(toks, j):
    """The index of the bracket that opens the one closing at `toks[j]`."""
    closer = toks[j]
    opener = ("punct", {v: k for k, v in _OPENERS.items()}[closer[1]])
    depth = 0
    for i in range(j, -1, -1):
        if toks[i] == closer:
            depth += 1
        elif toks[i] == opener:
            depth -= 1
            if depth == 0:
                return i
    raise LexError("unopened bracket")


def _opens(tok):
    return tok[0] == "punct" and tok[1] in _OPENERS


def _brace_depths(toks):
    """How many `{` enclose each token: 0 is an item at the file's top."""
    depths, depth = [], 0
    for tok in toks:
        if tok == ("punct", "}"):
            depth -= 1
        depths.append(depth)
        if tok == ("punct", "{"):
            depth += 1
    return depths


def _past_attributes(toks, k):
    """The first token at or after `k` past outer attributes and a
    visibility (`pub`, `pub(crate)`): where the item itself starts."""
    while k + 1 < len(toks) and toks[k] == ("punct", "#") and toks[k + 1] == ("punct", "["):
        k = _closing(toks, k + 1) + 1
    if k < len(toks) and toks[k] == ("ident", "pub"):
        k += 1
        if k < len(toks) and toks[k] == ("punct", "("):
            k = _closing(toks, k) + 1
    return k


def _attributes_of(toks, m):
    """The texts inside each outer attribute on the item whose keyword is
    `toks[m]`, read back past its visibility."""
    k = m
    if k >= 1 and toks[k - 1] == ("ident", "pub"):
        k -= 1
    elif k >= 1 and toks[k - 1] == ("punct", ")"):
        opened = _opening(toks, k - 1)
        if opened >= 1 and toks[opened - 1] == ("ident", "pub"):
            k = opened - 1
    found = []
    while k >= 2 and toks[k - 1] == ("punct", "]"):
        opened = _opening(toks, k - 1)
        if opened < 1 or toks[opened - 1] != ("punct", "#"):
            break
        found.append([text for _, text in toks[opened + 1 : k - 1]])
        k = opened - 1
    return found


def _file_name(value):
    """The last component of a path a macro or attribute names, or None
    when there is none to read."""
    if value is None:
        return None
    return posixpath.basename(value) or None


def _included_name(arg):
    """The name of the file an `include!`-family macro reads, from its
    argument's tokens; None when this cannot tell. A plain string names
    it, and so does a `concat!` whose last pieces are strings that hold a
    `/`: `concat!(env!("OUT_DIR"), "/notes.md")` reads a `notes.md`, while
    `concat!(env!("D"), "x.rs")` could read any name ending in `x.rs`."""
    if arg and arg[-1] == ("punct", ","):
        arg = arg[:-1]
    if len(arg) == 1 and arg[0][0] == "str":
        return _file_name(arg[0][1])
    if (
        len(arg) >= 4
        and arg[0] == ("ident", "concat")
        and arg[1] == ("punct", "!")
        and _opens(arg[2])
        and _closing(arg, 2) == len(arg) - 1
    ):
        pieces = arg[3:-1]
        if pieces and pieces[-1] == ("punct", ","):
            pieces = pieces[:-1]
        tail = []
        while pieces and pieces[-1][0] == "str":
            tail.insert(0, pieces[-1][1])
            pieces = pieces[:-1]
            if pieces and pieces[-1] == ("punct", ","):
                pieces = pieces[:-1]
            else:
                break
        if tail and None not in tail:
            joined = "".join(tail)
            if not pieces or "/" in joined:
                return _file_name(joined)
    return None


def _resolved(folder, value):
    """`value`, a `#[path]` relative to `folder`, as a path in the
    repository; None when it is unreadable, absolute or leaves the
    repository."""
    if value is None or value.startswith("/"):
        return None
    target = posixpath.normpath(posixpath.join(folder, value))
    if target == ".." or target.startswith("../"):
        return None
    return target


def _test_module_spans(toks):
    """The token spans `(first, last)` of every `#[cfg(test)] mod name { .. }`
    in `toks`: from the attribute's `#` to the module's closing brace.

    Read off the tokens rather than the lines `cfg_test_ranges` reports: a
    line can hold code on both sides of a test module's edge
    (`} const SRC: &str = include_str!("a.rs");`), and the include there is
    shipped. Only the exact `cfg(test)` attribute on a module with a body
    counts, as `cfg_test_ranges` reads it; anything else is left shipped,
    which at worst asks for a release note this did not need.
    """
    spans = []
    for i in range(len(toks) - 1):
        if toks[i] != ("punct", "#") or toks[i + 1] != ("punct", "["):
            continue
        end = _closing(toks, i + 1)
        if toks[i + 2 : end] != [
            ("ident", "cfg"),
            ("punct", "("),
            ("ident", "test"),
            ("punct", ")"),
        ]:
            continue
        k = _past_attributes(toks, end + 1)
        if (
            k + 2 < len(toks)
            and toks[k] == ("ident", "mod")
            and toks[k + 1][0] == "ident"
            and toks[k + 2] == ("punct", "{")
        ):
            spans.append((i, _closing(toks, k + 2)))
    return spans


def _loaders_in(path, toks):
    """What one file's own text can load beside its plain `mod` children:
    `(files a top-level #[path] mod x; loads, folders another #[path] can
    load from, names an include_str! or include_bytes! reads, files an
    include! pastes in)`. None stands for one this cannot pin down: an
    `include!` of anything but a plain string, whose pasted code could be
    any file's, and an include macro renamed by a `use` (`use
    std::include_str as text;`), after which `text!("helper.rs")` reads a
    file under a name this does not know. Only a `use` can rename a macro;
    elsewhere `include` is an ordinary name (croft has a search field called
    that). An `include!("file")` names the file it pastes in, relative to
    this one's folder: its name joins the names, since the file ships as
    release code, and the caller reads that file's code as part of this
    one's (#2722).

    An include macro inside a `#[cfg(test)]` module compiles only for tests,
    so it is not counted: croft's port detector has a test that reads its own
    source that way. Inside means between the module's braces, by token
    (`_test_module_spans`), not on one of its lines: a shipped include can
    share a line with the brace that closes a test module.

    A macro this does not know can still read files: a procedural macro, or
    a `macro_rules!` handed `include_str` as an argument. So can a build
    script, writing what it read into a file the crate then embeds. croft
    has no such macro, and its build script reads only release notes; this
    does not try to see them.

    A `#[path]` on a top-level `mod x;` loads exactly one file, relative to
    this one's folder, and that file then finds its own children beside it,
    as a crate root does. Anywhere else - on an inline `mod x { .. }`, or on
    a module nested in one - the path is relative to the module's folder,
    somewhere under this file's, so all of that folder counts.
    """
    depths = _brace_depths(toks)
    folder = posixpath.dirname(path)
    roots, folders, names, pasted = [], [], [], []
    in_use = False
    # The test modules only matter to a file that names an include macro.
    test_spans = (
        _test_module_spans(toks)
        if any(k == "ident" and w.removeprefix("r#") in INCLUDE_MACROS for k, w in toks)
        else []
    )
    for i, tok in enumerate(toks):
        if tok == ("ident", "use"):
            in_use = True
        elif tok == ("punct", ";"):
            in_use = False
        if tok == ("punct", "#"):
            j = i + 1
            inner = j < len(toks) and toks[j] == ("punct", "!")
            if inner:
                j += 1
            if j >= len(toks) or toks[j] != ("punct", "["):
                continue
            end = _closing(toks, j)
            body = toks[j + 1 : end]
            values = [
                body[n + 2][1] if n + 2 < len(body) and body[n + 2][0] == "str" else None
                for n in range(len(body) - 1)
                if body[n][0] == "ident"
                and body[n][1].removeprefix("r#") == "path"
                and body[n + 1] == ("punct", "=")
            ]
            if not values:
                continue
            k = _past_attributes(toks, end + 1)
            file_module = (
                not inner
                and depths[i] == 0
                and k + 2 < len(toks)
                and toks[k] == ("ident", "mod")
                and toks[k + 1][0] == "ident"
                and toks[k + 2] == ("punct", ";")
            )
            for value in values:
                if file_module:
                    roots.append(_resolved(folder, value))
                elif value is None or value.startswith("/") or ".." in value.split("/"):
                    folders.append(None)
                else:
                    folders.append(folder)
        elif tok[0] == "ident" and tok[1].removeprefix("r#") in INCLUDE_MACROS:
            if any(first < i < last for first, last in test_spans):
                continue
            called = (
                i + 2 < len(toks) and toks[i + 1] == ("punct", "!") and _opens(toks[i + 2])
            )
            arg = toks[i + 3 : _closing(toks, i + 2)] if called else []
            if arg and arg[-1] == ("punct", ","):
                arg = arg[:-1]
            if called and tok[1].removeprefix("r#") != "include":
                names.append(_included_name(arg))
            elif called and len(arg) == 1 and arg[0][0] == "str" and _resolved(folder, arg[0][1]):
                # Code pasted in from a file this names: the caller reads it
                # for what it loads, and the file itself ships, as one an
                # include_str! embeds does, whatever else declares it.
                pasted.append(_resolved(folder, arg[0][1]))
                names.append(_file_name(arg[0][1]))
            elif called or in_use:
                # Code pasted in from a file this cannot name, or a renamed
                # macro: it can embed a file or declare a module that
                # nothing here sees.
                names.append(None)
                roots.append(None)
    return roots, folders, names, pasted


def _declares_a_module(toks):
    """Whether `toks` declare a module: `mod name`, with a body or without."""
    return any(
        toks[m] == ("ident", "mod") and toks[m + 1][0] == "ident" for m in range(len(toks) - 1)
    )


def _cargo_roots(commit, cwd):
    """The crate roots `Cargo.toml` names with a `path` or `build` key,
    and None when it cannot be read."""
    text = git("show", f"{commit}:Cargo.toml", cwd=cwd, allow_missing_path=True)
    if not text:
        return []
    try:
        manifest = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return [None]
    found = []
    for key in CARGO_TARGETS:
        targets = manifest.get(key, [])
        for target in targets if isinstance(targets, list) else [targets]:
            if isinstance(target, dict) and "path" in target:
                found.append(_resolved("", target["path"]) if isinstance(target["path"], str) else None)
    build = manifest.get("package", {}).get("build")
    if isinstance(build, str):
        found.append(_resolved("", build))
    return found


def module_loaders(rev, cwd=None):
    """Everything at `rev` that can load a source file into a build,
    other than a plain `mod name;` in a conventional parent:
    `(root files, folders a #[path] can load from, file names an
    include_str! or include_bytes! reads, whether some module loading could
    not be pinned down, whether some file embedding could not)`.

    Roots are the files that find their children beside them: `src/main.rs`,
    `src/lib.rs`, `build.rs`, `src/bin/*.rs`, `src/bin/*/main.rs`, any target
    `Cargo.toml` names, and every file a top-level `#[path] mod x;` loads.
    Every Rust file in the tree is read for these, and so is every file a
    `#[path]` loads, whatever its name; a non-Rust file can otherwise take
    part only through an `include!`, which counts as unknown.
    The answer is the same for every file a check asks about at one commit,
    so it is worked out once per commit.
    """
    commit = git("rev-parse", "--verify", f"{rev}^{{commit}}", cwd=cwd).strip()
    return _module_loaders(commit, None if cwd is None else str(cwd))


@functools.lru_cache(maxsize=None)
def _module_loaders(commit, cwd):
    tree, links = set(), set()
    for entry in git("ls-tree", "-r", "-z", commit, cwd=cwd).split("\0"):
        if not entry:
            continue
        meta, name = entry.split("\t", 1)
        tree.add(name)
        if meta.split(" ", 1)[0] == "120000":
            links.add(name)
    roots = [p for p in ("src/main.rs", "src/lib.rs", "build.rs") if p in tree]
    roots += [p for p in tree if re.fullmatch(r"src/bin/[^/]+\.rs|src/bin/[^/]+/main\.rs", p)]
    roots += _cargo_roots(commit, cwd)
    folders, names = [], []
    # Every Rust file, read whole: a text search that picks candidates first
    # misses a comment between tokens (`path /* x */ = "helper.rs"`), which
    # Rust reads as plain whitespace.
    paths = sorted(p for p in tree if p.endswith(".rs"))
    read = set()
    # Files an `include!("file")` pastes in, and whether each file read
    # declares a module (or could not be read).
    pasted, declares = set(), {}
    while paths:
        read.update(paths)
        for path, text in zip(paths, _blobs(commit, paths, cwd)):
            if path in links:
                # A symlink's blob is the path it points to, not the code
                # rustc reads through it: what that code loads is unknown.
                roots.append(None)
                names.append(None)
                declares[path] = True
                continue
            try:
                toks = rust_tokens(text)
                more_roots, more_folders, more_names, more_pasted = _loaders_in(path, toks)
                declares[path] = _declares_a_module(toks)
            except LexError:
                more_roots, more_folders, more_names, more_pasted = [None], [], [None], []
                declares[path] = True
            roots += more_roots
            folders += more_folders
            names += more_names
            pasted.update(more_pasted)
        # A `#[path]` can load a file of any name (`production.inc`), and
        # that file's own loaders count as much as a `.rs` file's: read
        # each one too, until none is new. So can an `include!` paste one
        # in. One that is not in the tree is generated at build time, and
        # what it loads or embeds is unknown.
        loaded = {r for r in roots if r is not None and r not in read}
        loaded |= pasted - read
        missing = [r for r in loaded if r not in tree]
        roots += [None for _ in missing]
        names += [None for _ in missing]
        paths = sorted(r for r in loaded if r in tree)
    # Pasted code that declares a module: the declaration check reads only
    # the file that is the module's parent, so it would miss this one.
    if any(declares.get(p, True) for p in pasted if p in tree):
        roots.append(None)
    unknown_load = None in roots or None in folders
    return (
        frozenset(r for r in roots if r is not None),
        tuple(f for f in folders if f is not None),
        frozenset(n for n in names if n is not None),
        unknown_load,
        # A module this could not pin down, or one a `#[path]` loads from a
        # folder without naming the file, is never read: it can embed any
        # file.
        None in names or unknown_load or bool(folders),
    )


def _blobs(commit, paths, cwd):
    """The text of each of `paths` at `commit`, read in one `git cat-file`."""
    if not paths:
        return []
    out = subprocess.run(
        ("git", "cat-file", "--batch"),
        cwd=cwd,
        input="".join(f"{commit}:{p}\n" for p in paths).encode(),
        capture_output=True,
        check=True,
    ).stdout
    texts, at = [], 0
    for _ in paths:
        header_end = out.index(b"\n", at)
        size = int(out[at:header_end].split()[2])
        texts.append(out[header_end + 1 : header_end + 1 + size].decode(errors="replace"))
        at = header_end + 1 + size + 1
    return texts


def _under(path, folder):
    """Whether `path` is `folder` or inside it; "" is the repository."""
    return folder == "" or path == folder or path.startswith(folder + "/")


def declared_for_tests_only(rev, path, cwd=None):
    """Whether, at `rev`, nothing but tests compiles the file `path`: a
    parent declares its module as `#[cfg(test)] mod name;` and nothing else
    that can load the file names it. A test helper module like
    `src/test_git.rs` is the case.

    The parents that can declare it with a plain `mod name;` are the roots in
    its folder and the `<folder>.rs` / `<folder>/mod.rs` above it, and there
    every mention of `mod name` must be that test-only declaration, at the top
    of the file. A file further up can reach it through inline modules
    (`mod folder { mod name; }`), so any mention of `mod name` there counts as
    shipped, and so does a file a `#[path]` or an `include!` can load (see
    `module_loaders`). The parents are read as tokens, so a comment inside a
    declaration, a line break after `mod` or a raw identifier `r#name` cannot
    hide one, and a string or comment cannot fake one.
    """
    parts = Path(path).parts
    if len(parts) < 2 or parts[0] != "src" or not path.endswith(".rs"):
        return False
    roots, folders, names, unknown_load, unknown_embed = module_loaders(rev, cwd=cwd)
    if unknown_load or unknown_embed or path in roots or Path(path).name in names:
        return False
    if any(_under(path, folder) for folder in folders):
        return False
    if parts[-1] == "mod.rs":
        if len(parts) < 3:
            return False
        name, home = parts[-2], "/".join(parts[:-2])
    else:
        name, home = Path(parts[-1]).stem, "/".join(parts[:-1])
    if not MODULE_NAME.fullmatch(name):
        return False
    steps = home.split("/")
    declarers = {r for r in roots if posixpath.dirname(r) == home}
    declarers |= {f"{home}.rs", f"{home}/mod.rs"}
    reachers = {
        r for r in roots if posixpath.dirname(r) != home and _under(home, posixpath.dirname(r))
    }
    reachers |= {
        f"{'/'.join(steps[:n])}{suffix}" for n in range(1, len(steps)) for suffix in (".rs", "/mod.rs")
    }
    found = False
    for parent in sorted(declarers | reachers):
        text = git("show", f"{rev}:{parent}", cwd=cwd, allow_missing_path=True)
        if not text:
            continue
        try:
            toks = rust_tokens(text)
            depths = _brace_depths(toks)
            for m in range(len(toks) - 1):
                if toks[m] != ("ident", "mod") or toks[m + 1][0] != "ident":
                    continue
                if toks[m + 1][1].removeprefix("r#") != name:
                    continue
                if parent not in declarers:
                    return False
                if depths[m] != 0 or m + 2 >= len(toks) or toks[m + 2] != ("punct", ";"):
                    return False
                if ["cfg", "(", "test", ")"] not in _attributes_of(toks, m):
                    return False
                found = True
        except LexError:
            return False
    return found


def ships_nothing(base, head, path, cwd=None):
    """True when every line this change touches is inside a test module.

    Both sides of the diff are read. A deletion leaves no line at the head to
    inspect, so a head-only check would waive the removal of shipped code as
    readily as the removal of a test.
    """
    if not path.endswith(".rs"):
        return False
    base_text = git("show", f"{base}:{path}", cwd=cwd, allow_missing_path=True)
    head_text = git("show", f"{head}:{path}", cwd=cwd, allow_missing_path=True)
    # A module only tests compile, on every side it exists: a change to any
    # line of it ships nothing.
    sides = [rev for rev, text in ((base, base_text), (head, head_text)) if text]
    if sides and all(declared_for_tests_only(rev, path, cwd=cwd) for rev in sides):
        return True
    # A file an `include_str!` or `include_bytes!` embeds ships as text, its
    # test modules too: an edit anywhere in it changes the binary. So does
    # every file when some embedding cannot be pinned down.
    for rev in sides:
        _, _, names, _, unknown_embed = module_loaders(rev, cwd=cwd)
        if unknown_embed or Path(path).name in names:
            return False
    removed, added, diff, hunks = changed_lines(base, head, path, cwd=cwd)
    if not removed and not added and diff.strip():
        # git described a change it did not give hunk headers for - a file it
        # treats as binary. Nothing here can read that, so it ships.
        return False
    if not removed and not added:
        # An empty diff means "nothing changed" for a path that exists at both
        # ends, and "git could not resolve that pathspec" otherwise. Only the
        # first is a reason to waive a bump, and the second is reachable: a
        # path that matches nothing makes `git diff` exit 0 with no output.
        return bool(base_text) and bool(head_text)
    base_ranges = cfg_test_ranges(base_text)
    head_ranges = cfg_test_ranges(head_text)
    base_lines = source_lines(base_text)
    head_lines = source_lines(head_text)

    base_literals = literal_lines(base_text)
    head_literals = literal_lines(head_text)

    def blank(lines, n, literals):
        """A line that carries no CODE cannot change the binary, whichever
        side of the diff it is on: adding or removing the blank line above a
        new test module is the common case. Inside a string literal it is not
        whitespace at all but content, and croft has 42 such lines in `src/`
        - the model's system prompt and the tree-sitter queries - where a
        blank line changes what the binary does."""
        return 1 <= n <= len(lines) and not lines[n - 1].strip() and n not in literals

    removed = [n for n in removed if not blank(base_lines, n, base_literals)]
    added = [n for n in added if not blank(head_lines, n, head_literals)]
    if not all(inside(n, base_ranges) for n in removed):
        return False
    if not all(inside(n, head_ranges) for n in added):
        return False
    # The `#[cfg(test)]` line is what DECIDES whether the module compiles, and
    # it sits inside the span it opens, so a diff that only adds or removes it
    # falls inside the span and would be waived. Both directions change the
    # binary: adding the attribute takes a shipping module out, removing it
    # puts a test module in. A touched attribute is therefore only test-only
    # when its WHOLE module moved with it, which is what a test module added
    # or deleted in one piece looks like.
    if not (
        _whole_span_moved(base_ranges, removed, base_lines)
        and _whole_span_moved(head_ranges, added, head_lines)
    ):
        return False
    # An attribute the diff did not touch can still change what it applies
    # to. `mod tests;` inserted under an existing `#[cfg(test)]` takes the
    # attribute off the item below, which then ships; deleting it does the
    # reverse. Every span the change touched must therefore open at an
    # untouched attribute that guards the same code on the other side too,
    # or have moved with its attribute (checked above).
    return _attribute_kept_its_span(
        head_ranges, added, base_ranges, hunks, "head"
    ) and _attribute_kept_its_span(base_ranges, removed, head_ranges, hunks, "base")


def _attribute_kept_its_span(ranges, touched, other_ranges, hunks, side):
    """True when every span holding a touched line either had its attribute
    touched too (a module added or removed whole) or opens at an untouched
    attribute that still guards the same code on the other side of the diff.

    Opening a span on both sides is not enough. `mod added;` inserted between
    an existing `#[cfg(test)]` and the module it guarded takes the attribute
    off that module, which then ships, while the attribute still opens a span
    on both sides. So every unchanged line of either span must sit in the
    other side's span: the change only touched lines inside the module, and
    the module still covers the code it covered."""
    other_side = "base" if side == "head" else "head"
    other_end = dict(other_ranges)
    marked = set(touched)
    for start, end in ranges:
        if start in marked or not any(start <= n <= end for n in marked):
            continue
        twin = counterpart(start, hunks, side)
        if twin not in other_end:
            return False
        twin_end = other_end[twin]
        for n in range(start, end + 1):
            moved = counterpart(n, hunks, side)
            if moved is not None and not twin <= moved <= twin_end:
                return False
        for n in range(twin, twin_end + 1):
            moved = counterpart(n, hunks, other_side)
            if moved is not None and not start <= moved <= end:
                return False
    return True


def _whole_span_moved(ranges, touched, lines):
    """True unless a span's attribute line was touched without the rest.

    Blank lines inside the span are not required to have moved: they were
    already dropped from `touched` above, and every one of the 159
    `#[cfg(test)]` modules in `src/` contains at least one, so demanding them
    made a whole module added or deleted in one piece - the case this
    exemption exists for - unreachable for any module shaped like the ones
    already here.
    """
    marked = set(touched)
    return all(
        start not in marked
        or all(n in marked for n in range(start, end + 1) if lines[n - 1].strip())
        for start, end in ranges
    )


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: ships_nothing.py <base> <head> <path>")
    base, head, path = sys.argv[1:4]
    return 0 if ships_nothing(base, head, path) else 1


if __name__ == "__main__":
    sys.exit(main())
