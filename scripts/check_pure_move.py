#!/usr/bin/env python3
"""Prove a change only moves Rust items, byte for byte, and list what moved.

Splitting an 86k-line file moves thousands of lines, and nobody can review
that by reading it. This parses every changed `.rs` file at both revisions
into items - each free item, and each item inside an `impl` or inline `mod`
block - and matches them by text. An item's text is the item plus the docs,
attributes and `//` comments directly above it; before comparing, only the
common indentation and the visibility on its signature line (moves widen it)
are normalised. A method keeps the code of its `impl` header as part of its
identity, so moving `fmt` from `impl Display for X` to `impl Debug for X`, or
out of a `#[cfg(test)] impl`, is not a move.

Glue is allowed and listed apart: `use` and `mod x;` declarations, `impl` and
`mod` block headers (their docs and comments too) and closing braces, blank
lines and `//!` docs. An inline `mod` header's attributes are glue as well, so
a `#[cfg(test)] mod tests { }` that becomes its own file still matches; a cfg
changed there shows only in the glue. Every other line belongs to an item,
and an item that changed, or appeared or vanished without an identical twin,
fails the check.

Usage:
  check_pure_move.py [--if-declared] <base-rev> <head-rev>
      --if-declared: only fail when a commit message in base..head has the
      line `pure-move: true`; otherwise print the report without diffs and
      exit 0.
  check_pure_move.py list <file.rs>
      Print every item in a file: kind, name, impl header, lines, line count.
"""

import bisect
import difflib
import os
import re
import sys
from collections import Counter, defaultdict, namedtuple

from check_doc_ownership import exists_at, git

Item = namedtuple("Item", "file kind name mods ctx start end norm")
Glue = namedtuple("Glue", "file kind start end text")

# The start of anything whose braces are not structure. Only the raw prefix
# changes how a literal ends (`b"`, `c"` and `b'` lex like `"` and `'`); it
# counts when it is not the tail of an identifier, and `r#ident` is a raw
# identifier rather than a raw string because no `"` follows its hashes.
SPECIAL = re.compile(r"//|/\*|(?<!\w)[bc]?r#*\"|[\"']")
# One character, or one escape, between quotes. Anything else after a `'`
# (`'a`, `'static`, `'outer:`) is a lifetime or a label, which is code.
CHAR = re.compile(r"'(?:[^\\'\n]|\\(?:u\{[0-9a-fA-F_]{1,8}\}|x[0-9a-fA-F]{2}|.))'")
STRING_TAIL = re.compile(r'[^"\\]*(?:\\.[^"\\]*)*"', re.DOTALL)
NESTED = re.compile(r"/\*|\*/")
NOT_NEWLINE = re.compile(r"[^\n]")
STRUCT = re.compile(r"[(\[{)\]};]")
BRACKETS = re.compile(r"[(\[{)\]}]")
OUTER_ATTR = re.compile(r"\s*#\s*\[")
INNER_ATTR = re.compile(r"\s*#\s*!\s*\[")
VIS = re.compile(r"\s*(pub\b(?:\s*\([^)]*\))?\s*)")
SIG = re.compile(
    r"\s*(?:pub\b(?:\s*\([^)]*\))?\s*)?"
    r"(?:(?:default|const|async|unsafe|safe|extern|auto)\s+)*"
    r"(?P<kw>fn|struct|enum|union|trait|type|const|static|macro_rules!|use|mod|impl"
    r"|extern\s+crate|extern)(?!\w)(?:\s+(?:mut\s+)?(?P<name>r#\w+|\w+))?"
)
MACRO = re.compile(r"\s*((?:\w+::)*\w+)\s*!")
# Kinds whose `{` belongs to a value or a path, not a body: only `;` ends them.
SEMI_ONLY = {"const", "static", "type", "use"}
PURE_MOVE = re.compile(r"^\s*pure-move:\s*true\s*$", re.MULTILINE | re.IGNORECASE)
BLANK, COMMENT, DOC, INNER, CODE = range(5)


class ParseError(Exception):
    pass


def lex(text):
    """`text` with every string, char literal and comment blanked to spaces
    (newlines kept, so lines and columns still line up), and what each line
    that starts inside a literal or a block comment starts inside: "string",
    "comment", or "inner" for a `/*!` doc. Re-indenting a line inside a
    string changes the string, so it is never dedented."""
    newlines = [m.start() for m in re.finditer("\n", text)]
    spans, starts_in, pos = [], {}, 0

    def fail(at, what):
        raise ParseError(f"line {bisect.bisect_left(newlines, at) + 1}: unterminated {what}")

    while m := SPECIAL.search(text, pos):
        start, tok = m.start(), m.group()
        inside = None
        if tok == "//":
            end = text.find("\n", start)
            end = len(text) if end < 0 else end
        elif tok == "/*":
            depth, end = 1, start + 2
            while depth:
                if not (n := NESTED.search(text, end)):
                    fail(start, "comment")
                depth += 1 if n.group() == "/*" else -1
                end = n.end()
            inside = "inner" if text.startswith("/*!", start) else "comment"
        elif tok.endswith('"'):
            if "r" in tok:
                close = '"' + "#" * tok.count("#")
                end = text.find(close, m.end())
                end = end + len(close) if end >= 0 else -1
            else:
                n = STRING_TAIL.match(text, m.end())
                end = n.end() if n else -1
            if end < 0:
                fail(start, "string")
            inside = "string"
        elif n := CHAR.match(text, m.end() - 1):
            end = n.end()
        else:
            pos = m.end()
            continue
        if inside:
            first = bisect.bisect_left(newlines, start) + 1
            last = bisect.bisect_left(newlines, end - 1)
            starts_in.update(dict.fromkeys(range(first, last + 1), inside))
        spans.append((start, end))
        pos = end
    pieces, pos = [], 0
    for start, end in spans:
        pieces += [text[pos:start], NOT_NEWLINE.sub(" ", text[start:end])]
        pos = end
    pieces.append(text[pos:])
    return "".join(pieces), starts_in


def line_kind(line, code, starts_in):
    """BLANK, CODE or the kind of comment `line` is; `starts_in` is what it
    starts inside, as `lex` reports it."""
    s = line.strip()
    if not s:
        return BLANK
    if code.strip() or starts_in == "string":
        return CODE
    if starts_in == "comment":
        return COMMENT
    if starts_in == "inner" or s.startswith(("//!", "/*!")):
        return INNER
    if s.startswith("///") and not s.startswith("////"):
        return DOC
    if s.startswith("/**") and not s.startswith(("/***", "/**/")):
        return DOC
    # A line holding nothing but a literal is code.
    return COMMENT if s.startswith(("//", "/*")) else CODE


def collapse(lines):
    return " ".join(" ".join(lines).split())


def balanced_end(clines, i, col, hi):
    """(line, column) just past the bracket group opening at (i, col)."""
    depth = 0
    for j in range(i, hi):
        for m in BRACKETS.finditer(clines[j], col if j == i else 0):
            depth += 1 if m.group() in "([{" else -1
            if depth == 0:
                return j, m.end()
    raise ParseError(f"line {i + 1}: unterminated attribute")


def extent(clines, j, col, hi, semi_only):
    """Where the item starting at (j, col) ends: (last line, (line, col) of
    its body's `{` or None, line of that body's `}` or None). It ends at the
    first line end at depth 0 after a `;` at depth 0 or, unless `semi_only`,
    after the body closes."""
    depth, body, close, done = 0, None, None, False
    for k in range(j, hi):
        for m in STRUCT.finditer(clines[k], col if k == j else 0):
            c = m.group()
            if c == ";":
                done = done or depth == 0
            elif c in "([{":
                if c == "{" and depth == 0 and body is None and not done:
                    body = (k, m.start())
                depth += 1
            else:
                depth -= 1
                if depth < 0:
                    raise ParseError(f"line {k + 1}: unbalanced {c!r}")
                if c == "}" and depth == 0 and not semi_only and close is None and not done:
                    close, done = k, True
        if done and depth == 0:
            return k, body, close
    raise ParseError(f"line {j + 1}: item never ends")


def parse(text, file=""):
    """Every item in `text` and every line of glue between them. Together
    they cover each line exactly once; a file that cannot be read that way
    raises ParseError rather than being half-checked."""
    code, starts_in = lex(text)
    lines, clines = text.split("\n"), code.split("\n")
    kinds = [line_kind(ln, c, starts_in.get(i)) for i, (ln, c) in enumerate(zip(lines, clines))]
    items, glue = [], []

    def item(kind, name, mods, ctx, start, sig, col, end):
        body = lines[start : end + 1]
        if m := VIS.match(body[sig - start], col):
            body[sig - start] = body[sig - start][: m.start(1)] + body[sig - start][m.end(1) :]
        free = [(ln, starts_in.get(start + n) != "string") for n, ln in enumerate(body)]
        indent = os.path.commonprefix(
            [ln[: len(ln) - len(ln.lstrip())] for ln, ok in free if ok and ln.strip()]
        )
        norm = "\n".join(
            ln[len(indent) :] if ok and ln.startswith(indent) else ln for ln, ok in free
        )
        items.append(Item(file, kind, name, mods, ctx, start, end, norm))

    def walk(lo, hi, mods, ctx):
        lead, sticky, i = None, False, lo
        while i < hi:
            kind = kinds[i]
            if kind != CODE:
                # A doc or an attribute reaches its item across blank lines;
                # plain comments split from it at the first one.
                sticky = sticky or kind == DOC
                if kind in (BLANK, INNER) and not sticky:
                    if lead is not None:
                        item("comment", lines[lead].strip()[:60], mods, ctx, lead, lead, 0, i - 1)
                        lead = None
                    what = "blank" if kind == BLANK else "doc"
                    glue.append(Glue(file, what, i, i, lines[i].strip()))
                elif lead is None:
                    lead = i
                i += 1
                continue
            start = i if lead is None else lead
            if INNER_ATTR.match(clines[i]):
                end = balanced_end(clines, i, clines[i].index("["), hi)[0]
                item("attr", collapse(clines[i : end + 1]), mods, ctx, start, i, 0, end)
                lead, sticky, i = None, False, end + 1
                continue
            j, col = i, 0
            while m := OUTER_ATTR.match(clines[j], col):
                j, col = balanced_end(clines, j, m.end() - 1, hi)
            if (j, col) != (i, 0) and not clines[j][col:].strip():
                lead, sticky, i = start, True, j + 1
                continue
            sig = clines[j][col:]
            if m := SIG.match(sig):
                kind = " ".join(m["kw"].split())
                name = m["name"] if kind != "impl" else None
            elif m := MACRO.match(sig):
                kind, name = "macro", m[1] + "!"
            else:
                kind, name = "other", None
            end, body, close = extent(clines, j, col, hi, kind in SEMI_ONLY)
            name = name or " ".join(sig.split())[:60]
            lead, sticky = None, False
            if (
                kind in ("impl", "mod")
                and body
                and close == end
                and body[0] < close
                and not clines[body[0]][body[1] + 1 :].strip()
                and clines[close].strip() == "}"
            ):
                # A method's identity is its header's code, attributes
                # included, so one moved under a `#[cfg(test)] impl` is not
                # the same method; the header's docs and comments stay out of
                # it, so two `impl App` blocks still match. The glue keeps
                # every line as written, so an edit to any of them is listed.
                header = [lines[k] for k in range(start, body[0]) if kinds[k] == CODE]
                head = collapse([*header, lines[body[0]][: body[1]]])
                written = "\n".join(ln.strip() for ln in lines[start : body[0] + 1])
                glue.append(Glue(file, "header", start, body[0], written))
                if kind == "impl":
                    walk(body[0] + 1, close, mods, head)
                else:
                    walk(body[0] + 1, close, (*mods, name), ctx)
                glue.append(Glue(file, "close", close, close, lines[close].strip()))
            elif kind == "use" or (kind == "mod" and body is None):
                glue.append(Glue(file, kind, start, end, collapse(lines[start : end + 1])))
            else:
                item(kind, name, mods, ctx, start, j, col, end)
            i = end + 1
        if lead is not None:
            item("comment", lines[lead].strip()[:60], mods, ctx, lead, lead, 0, hi - 1)

    walk(0, len(lines), (), "")
    return items, glue


def label(it):
    return "::".join([it.file, *it.mods, *([it.ctx] if it.ctx else []), it.name])


def span(it):
    return f"{it.file}:{it.start + 1}-{it.end + 1}"


def compare(old, new):
    """(unchanged count, moved pairs, changed pairs, removed, added).

    Identical text under the same impl header is the same item. A twin in
    the same place is unchanged; anywhere else it moved. What is left pairs
    up by kind and name as a change, and the rest was added or removed."""
    groups = defaultdict(lambda: ([], []))
    for side, its in enumerate((old, new)):
        for it in its:
            groups[(it.ctx, it.norm)][side].append(it)
    unchanged, moved, gone, came = 0, [], [], []
    for olds, news in groups.values():
        at = defaultdict(list)
        for n in news:
            at[(n.file, n.mods, n.name)].append(n)
        rest_old = []
        for o in olds:
            if at[(o.file, o.mods, o.name)]:
                at[(o.file, o.mods, o.name)].pop()
                unchanged += 1
            else:
                rest_old.append(o)
        rest_new = [n for twins in at.values() for n in twins]
        moved += zip(rest_old, rest_new)
        gone += rest_old[len(rest_new) :]
        came += rest_new[len(rest_old) :]
    by_name = defaultdict(list)
    for o in gone:
        by_name[(o.kind, o.name)].append(o)
    changed, added = [], []
    for n in came:
        twins = by_name.get((n.kind, n.name))
        if not twins:
            added.append(n)
            continue
        o = max(twins, key=lambda o: (o.ctx == n.ctx, o.file == n.file, o.mods == n.mods))
        twins.remove(o)
        changed.append((o, n))
    removed = [o for twins in by_name.values() for o in twins]
    return unchanged, moved, changed, removed, added


def print_glue(sides):
    """The glue one side has and the other lacks, in file order, so a
    rewritten `//!` module doc reads top to bottom; blank lines only as a
    count."""
    counts, first = [], []
    for _, glue in sides:
        counts.append(Counter((g.file, g.kind, g.text) for g in glue))
        at = {}
        for g in glue:
            at[(g.file, g.kind, g.text)] = min(at.get((g.file, g.kind, g.text), g.start), g.start)
        first.append(at)
    blanks = []
    for sign, side, delta in (("-", 0, counts[0] - counts[1]), ("+", 1, counts[1] - counts[0])):
        blanks.append(f"{sign}{sum(n for (_, kind, _), n in delta.items() if kind == 'blank')}")
        for key, count in sorted(delta.items(), key=lambda kv: (kv[0][0], first[side][kv[0]])):
            f, kind, text = key
            if kind != "blank":
                shown = text.replace("\n", "\n" + " " * 10)
                times = f" (x{count})" if count > 1 else ""
                print(f"  glue {sign}  {f}:{first[side][key] + 1}: {shown}{times}")
    if blanks != ["-0", "+0"]:
        print(f"  glue    blank lines {' '.join(blanks)}")


def check(base, head, if_declared):
    # NUL-separated, because a quoted non-ASCII path does not end in `.rs`
    # and would be skipped: a gate that passes by not looking.
    changed_files = [
        f
        for f in git("diff", "--name-only", "--no-renames", "-z", base, head).split("\0")
        if f.endswith(".rs")
    ]
    if not changed_files:
        print("No Rust files changed.")
        return 0
    declared = bool(PURE_MOVE.search(git("log", f"{base}..{head}", "--format=%B")))
    enforced = declared or not if_declared
    sides = ([], []), ([], [])
    errors = []
    for f in changed_files:
        for rev, (items, glue) in zip((base, head), sides):
            if exists_at(rev, f):
                try:
                    its, gl = parse(git("show", f"{rev}:{f}"), f)
                except ParseError as e:
                    errors.append(f"{f} at {rev}: {e}")
                    continue
                items += its
                glue += gl
    unchanged, moved, changed, removed, added = compare(sides[0][0], sides[1][0])
    print(f"Pure-move check of {len(changed_files)} changed Rust file(s), {base}..{head}")
    print(
        f"  {unchanged} unchanged, {len(moved)} moved, {len(changed)} changed, "
        f"{len(added)} added, {len(removed)} removed"
    )
    for o, n in sorted(moved, key=lambda p: (p[0].file, p[0].start)):
        print(f"  moved    {o.kind} {label(o)} -> {label(n)} ({span(o)} -> {span(n)})")
    print_glue(sides)
    for o, n in changed:
        print(f"  CHANGED  {n.kind} {label(o)} -> {label(n)}")
        if o.norm == n.norm:
            print("      same text, under a different impl header")
        if not enforced:
            continue
        a, b = o.norm.split("\n"), n.norm.split("\n")
        diff = list(difflib.unified_diff(a, b, span(o), span(n), n=1, lineterm=""))
        for line in diff[:40]:
            print("      " + line)
        if len(diff) > 40:
            print(f"      ... {len(diff) - 40} more diff line(s)")
    for it in removed:
        print(f"  REMOVED  {it.kind} {label(it)} ({span(it)})")
    for it in added:
        print(f"  ADDED    {it.kind} {label(it)} ({span(it)})")
    for e in errors:
        print(f"  UNPARSED {e}")
    if not (changed or removed or added or errors):
        print("Pure move: every changed item moved byte for byte.")
        return 0
    print("Not a pure move: the items above changed, or have no identical twin.")
    if not enforced:
        print("Not enforced, so no diffs: no commit in the range declares `pure-move: true`.")
        return 0
    return 1


def list_items(path):
    with open(path, encoding="utf-8") as f:
        items, _ = parse(f.read(), path)
    for it in items:
        name, lines = "::".join([*it.mods, it.name]), it.end - it.start + 1
        print(f"{it.kind}\t{name}\t{it.ctx or '-'}\t{it.start + 1}-{it.end + 1}\t{lines}")
    return 0


def main(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    if_declared = "--if-declared" in args
    args = [a for a in args if a != "--if-declared"]
    if len(args) == 2 and args[0] == "list":
        return list_items(args[1])
    if len(args) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    return check(*args, if_declared)


if __name__ == "__main__":
    sys.exit(main())
