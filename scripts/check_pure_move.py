#!/usr/bin/env python3
"""Parse Rust files into items, so a change can be proven to only move code.

Splitting an 86k-line file moves thousands of lines, and nobody can review
that by reading it. Every line of a file is either part of an item - each
free item, and each item inside an `impl` or inline `mod` block - or glue
between items. An item's text is the item plus the docs, attributes and `//`
comments directly above it, with the common indentation and the visibility
on its signature line (moves widen it) normalised away. A method also keeps
the code of its `impl` header, so `fmt` in `impl Display for X` and `fmt` in
`impl Debug for X` are two items. Glue is `use` and `mod x;` declarations,
`impl` and `mod` block headers (their docs and comments too) and closing
braces, blank lines and `//!` docs.

Usage:
  check_pure_move.py list <file.rs>
      Print every item in a file: kind, name, impl header, lines, line count.
"""

import bisect
import os
import re
import sys
from collections import namedtuple

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
STRUCT = re.compile(r"->|=>|[(\[{)\]};<>]")
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
BLANK, COMMENT, DOC, INNER, CODE = range(5)


class ParseError(Exception):
    pass


def lex(text):
    """`text` with every string, char literal and comment blanked to spaces
    (newlines kept, so lines and columns still line up); what each line that
    starts inside a literal or a block comment starts inside: "string",
    "comment", or "inner" for a `/*!` doc; and the (start, end) offsets of
    every literal and comment. Re-indenting a line inside a string changes
    the string, so it is never dedented."""
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
    return "".join(pieces), starts_in, spans


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


WHITESPACE = re.compile(r"\s+")


def collapse(text, segments, spans):
    """The `(start, end)` offset ranges of `text`, each stripped and joined by
    a space, with every run of whitespace outside a literal or comment shrunk
    to one space. Whitespace inside a string is part of its value
    (`#[path = "a  b.rs"]` names another file than `"a b.rs"`), so it is
    kept as written."""
    parts = []
    for a, b in segments:
        out, pos = [], a
        k = max(bisect.bisect_right(spans, (a, float("inf"))) - 1, 0)
        while k < len(spans) and spans[k][0] < b:
            s, e = spans[k]
            if e > a:
                out.append(WHITESPACE.sub(" ", text[pos : max(s, a)]))
                out.append(text[max(s, a) : min(e, b)])
                pos = min(e, b)
            k += 1
        out.append(WHITESPACE.sub(" ", text[pos:b]))
        if part := "".join(out).strip():
            parts.append(part)
    return " ".join(parts)


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
    its body's `{` or None, line of that body's `}` or None, column just past
    the token that ended it). It ends at the first line end at depth 0 after
    a `;` at depth 0 or, unless `semi_only`, after the body closes. Before
    the body, `<` and `>` nest too, so the braces of a const generic
    argument (`impl Tr for Foo<{ 1 }> {`) are not taken for the body."""
    depth, angle, body, close, done, stop = 0, 0, None, None, False, None
    for k in range(j, hi):
        for m in STRUCT.finditer(clines[k], col if k == j else 0):
            c = m.group()
            if c in ("->", "=>"):
                continue
            if c in "<>":
                if depth == 0 and body is None and not done:
                    angle = angle + 1 if c == "<" else max(angle - 1, 0)
                continue
            if c == ";":
                if not done and depth == 0:
                    done, stop = True, (k, m.end())
            elif c in "([{":
                if c == "{" and depth == 0 and angle == 0 and body is None and not done:
                    body = (k, m.start())
                depth += 1
            else:
                depth -= 1
                if depth < 0:
                    raise ParseError(f"line {k + 1}: unbalanced {c!r}")
                if c == "}" and depth == 0 and body and not semi_only and close is None and not done:
                    close, done, stop = k, True, (k, m.end())
        if done and depth == 0:
            return k, body, close, stop[1] if stop[0] == k else 0
    raise ParseError(f"line {j + 1}: item never ends")


def parse(text, file=""):
    """Every item in `text` and every line of glue between them. Together
    they cover each line exactly once; a file that cannot be read that way
    raises ParseError rather than being half-checked."""
    code, starts_in, spans = lex(text)
    lines, clines = text.split("\n"), code.split("\n")
    offsets = [0]
    for ln in lines:
        offsets.append(offsets[-1] + len(ln) + 1)

    def line_range(k, upto=None):
        """Line `k` as an offset range of `text`, up to column `upto`."""
        return offsets[k], offsets[k] + (len(lines[k]) if upto is None else upto)

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
                name = " ".join(" ".join(clines[i : end + 1]).split())
                item("attr", name, mods, ctx, start, i, 0, end)
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
            end, body, close, stop = extent(clines, j, col, hi, kind in SEMI_ONLY)
            if clines[end][stop:].strip():
                # Rust allows it, rustfmt never writes it, and reading the
                # rest of the line as part of this item would hide the next.
                raise ParseError(f"line {end + 1}: another item starts where one ends")
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
                header = [line_range(k) for k in range(start, body[0]) if kinds[k] == CODE]
                head = collapse(text, [*header, line_range(body[0], body[1])], spans)
                written = "\n".join(ln.strip() for ln in lines[start : body[0] + 1])
                glue.append(Glue(file, "header", start, body[0], written))
                if kind == "impl":
                    walk(body[0] + 1, close, mods, head)
                else:
                    walk(body[0] + 1, close, (*mods, name), ctx)
                glue.append(Glue(file, "close", close, close, lines[close].strip()))
            elif kind == "use" or (kind == "mod" and body is None):
                written = collapse(text, [line_range(k) for k in range(start, end + 1)], spans)
                glue.append(Glue(file, kind, start, end, written))
            else:
                item(kind, name, mods, ctx, start, j, col, end)
            i = end + 1
        if lead is not None:
            item("comment", lines[lead].strip()[:60], mods, ctx, lead, lead, 0, hi - 1)

    first = 0
    if lines[0].startswith("#!") and not INNER_ATTR.match(clines[0]):
        # A shebang line, which Rust skips; `#![` is an inner attribute.
        glue.append(Glue(file, "shebang", 0, 0, lines[0]))
        first = 1
    walk(first, len(lines), (), "")
    return items, glue


def list_items(path):
    with open(path, encoding="utf-8") as f:
        items, _ = parse(f.read(), path)
    for it in items:
        name, lines = "::".join([*it.mods, it.name]), it.end - it.start + 1
        print(f"{it.kind}\t{name}\t{it.ctx or '-'}\t{it.start + 1}-{it.end + 1}\t{lines}")
    return 0


def main(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) == 2 and args[0] == "list":
        return list_items(args[1])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
