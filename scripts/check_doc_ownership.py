#!/usr/bin/env python3
"""Flag functions that lost their doc comment between two revisions.

Rust attaches a `///` block to whatever item FOLLOWS it, so inserting a new
`fn` between an existing function and its doc silently hands that prose to the
newcomer. Nothing reports it: the build stays green, clippy is silent, and the
rendered rustdoc is confidently wrong rather than absent (#314).

The snapshot is undetectable - the reassigned prose is well formed and sits
above a plausible function - but the DIFF is not. A function that had a doc
comment and now has none is the exact fingerprint the insertion leaves behind,
and it is what both known instances did.

Deliberate removals are rare and are declared: put
`doc-removal: <path>::<item key>` in a commit message on the branch, where
the key is the one the gate's own error names: a bare name for a free item
(`src/foo.rs::bar`), or the enclosing impl header for a method
(`src/foo.rs::Foo::new`, `src/foo.rs::Display for Foo::fmt`). The path
qualifier keeps one declared removal from excusing a same-named item
elsewhere.

Usage: check_doc_ownership.py <base-rev> <head-rev>
"""

import re
import subprocess
import sys

# Every item kind a doc block can sit above. `fn` alone was the original
# scope and it let the same failure through twice in one day on `const`
# declarations: a doc block does not care what follows it, so neither can
# this. `impl` and bare `mod` are deliberately absent - an `impl` block's
# name is not unique enough to key on, and its methods are matched as `fn`
# in their own right.
ITEM = re.compile(
    # A same-line attribute (`#[cfg(test)] const A: u8 = 1;`) is unusual but
    # legal, and rustfmt leaves it alone inside macro bodies. Skipping it here
    # costs nothing and stops the item from being invisible.
    r"^\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?"
    # Rust's own qualifier order: default, const, async, unsafe, extern. The
    # optional `const` here is the one in `const fn`, and it has to sit where
    # Rust puts it: trailing the others made `pub const unsafe fn f()` match
    # nothing at all, so the function was invisible to the gate rather than
    # merely undocumented. The `const A: u8` declaration is a branch below,
    # reached when this optional one is not taken.
    r"(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?"
    # `r#` belongs to the name it prefixes. Without it here, `r#type` and
    # `r#match` in one impl both key as `r`, and the "any documented" reading
    # then hides a capture on either behind the other's surviving prose -
    # the #405 collision the impl qualifier exists to prevent, one level down.
    r"(?:extern\s+\"[^\"]*\"\s+)?(?:"
    r"fn\s+((?:r#)?[A-Za-z_]\w*)"
    r"|const\s+((?:r#)?[A-Za-z_]\w*)\s*:"
    r"|static\s+(?:mut\s+)?((?:r#)?[A-Za-z_]\w*)\s*:"
    r"|struct\s+((?:r#)?[A-Za-z_]\w*)"
    r"|enum\s+((?:r#)?[A-Za-z_]\w*)"
    r"|union\s+((?:r#)?[A-Za-z_]\w*)"
    r"|trait\s+((?:r#)?[A-Za-z_]\w*)"
    r"|type\s+((?:r#)?[A-Za-z_]\w*)"
    r"|macro_rules!\s+((?:r#)?[A-Za-z_]\w*)"
    r")"
)


# The one git failure this checker tolerates, in the two spellings git uses:
# the path is simply not present in that revision, because the branch added or
# deleted the file. Anything else - a bad revision, a malformed object, a
# broken repository - is a failure to report, not an empty file to accept.
MISSING_PATH = re.compile(
    r"fatal: path .*(?:does not exist in|exists on disk, but not in)", re.MULTILINE
)


# The head of an `impl` or `trait` block. Its methods are keyed
# `<header>::<name>` so that `new` in two impl blocks is two items, not one
# merged key whose "any documented" reading hides a capture on either
# (#405). The whole header is the key rather than the type alone: `impl
# Display for X` and `impl Debug for X` both define `fmt`. A trait's own
# method declarations get the same treatment, keyed `trait Foo::id`, since
# two traits declaring `id` are the same shape. Generics and lifetimes are
# stripped so a branch that reformats them does not move every method to a
# new key.
BLOCK_HEADER = re.compile(
    r"^\s*(?:(?:pub(?:\([^)]*\))?\s+)?trait|(?:unsafe\s+)?impl)\b(?P<rest>.*)$"
)

# Everything on a CODE line that is not block structure, in the order a
# left-to-right scan meets it: a complete raw string (`r#"{"#` is not a
# `"..."` and its braces are content), a complete ordinary string, a char
# literal (exactly one, possibly escaped, character between quotes, which
# is what keeps a lifetime `'a` from opening one), a raw string that runs
# past its line, an ordinary one that does (a `"...\` continuation), and a
# `//` comment. `format!("{x}")` is on most lines that matter, and one
# miscounted brace keys every item after it to the wrong block.
TOKEN = re.compile(
    r'(?P<raw>b?r(?P<hashes>#*)"(?:(?!"(?P=hashes)).)*"(?P=hashes))'
    r'|(?P<plain>b?"(?:\\.|[^"\\])*")'
    r"|(?P<char>b?'(?:\\.|[^'\\])')"
    r'|(?P<open_raw>b?r(?P<open_hashes>#*)")'
    r'|(?P<open_plain>")'
    r"|(?P<comment>//)"
)

# The first `"` not escaped by a backslash: where an ordinary string that
# ran past its line ends.
UNESCAPED_QUOTE = re.compile(r'(?<!\\)(?:\\\\)*"')


def strip_generics(text):
    """Drop every `<...>` group, nested ones included."""
    out, depth = [], 0
    for ch in text:
        if ch == "<":
            depth += 1
        elif ch == ">" and depth > 0:
            depth -= 1
        elif depth == 0:
            out.append(ch)
    return "".join(out)


def block_key(kind, header):
    """`impl<T> fmt::Display for Foo<T> where T: X {` -> `fmt::Display for Foo`;
    `trait Foo: Bar {` -> `trait Foo`.

    Everything from the first `where` or `{` on is the block's body or
    bounds, not its identity. A trait keeps its keyword so `trait Foo` and an
    inherent `impl Foo` on a same-named type stay distinct.
    """
    rest = strip_generics(header)
    rest = re.split(r"\bwhere\b|\{|:", rest, maxsplit=1)[0] if kind == "trait" else re.split(r"\bwhere\b|\{", rest, maxsplit=1)[0]
    rest = " ".join(rest.split())
    return f"trait {rest}" if kind == "trait" else rest


class BlockTracker:
    """Which `impl`/`trait` block each line of a file is in.

    Fed CODE lines in order. Counts braces after removing literals and a
    trailing `//` comment; a string that runs past its line puts the tracker
    in "content" mode until the terminator, so a multi-line raw string full
    of JSON braces is invisible to it. A header whose `{` comes later (a
    `where` clause, a rustfmt-wrapped `impl<T>\n Trait for X<T>\n{`) is
    accumulated until that brace opens. A body-less `impl Eq for X {}` opens
    and closes on one line and must not leak its key onto the next block.

    The counter is line-based and therefore fallible in principle; the
    invariants a caller can check are `depth == 0` and no open blocks at
    EOF, and the test-suite asserts both over every file in `src/`. Depth is
    floored at zero so one miscount cannot corrupt every later block.
    """

    def __init__(self):
        self.depth = 0
        # (depth the block opened at, key or None for a `mod`/`fn` body).
        self.blocks = []
        # Header text accumulated while waiting for its opening brace.
        self.pending = None
        # Terminator of a string that ran past its line, or None.
        self.open_string = None

    def scope(self):
        """The innermost impl/trait key, or None. A block with no key (a
        nested `fn` or `mod`) is transparent: it still lives in the impl."""
        return next((k for _, k in reversed(self.blocks) if k), None)

    def _code_of(self, line):
        """The line with literals and comments removed, tracking strings
        that span lines."""
        if self.open_string is not None:
            if self.open_string == '"':
                m = UNESCAPED_QUOTE.search(line)
                if not m:
                    return ""
                line = line[m.end():]
            else:
                at = line.find(self.open_string)
                if at < 0:
                    return ""
                line = line[at + len(self.open_string):]
            self.open_string = None
        out = []
        pos = 0
        for m in TOKEN.finditer(line):
            out.append(line[pos:m.start()])
            pos = m.end()
            if m.group("comment"):
                return "".join(out)
            if m.group("open_raw"):
                self.open_string = '"' + m.group("open_hashes")
                return "".join(out)
            if m.group("open_plain"):
                self.open_string = '"'
                return "".join(out)
        out.append(line[pos:])
        return "".join(out)

    def feed(self, line):
        header = BLOCK_HEADER.match(line) if self.open_string is None else None
        code = self._code_of(line)
        if header:
            kind = "trait" if "trait" in header.group(0)[: header.start("rest")] else "impl"
            self.pending = (kind, header.group("rest"))
        elif self.pending is not None and "{" not in code:
            # Still inside a wrapped header: `impl<T>` / `Trait for X<T>`.
            self.pending = (self.pending[0], self.pending[1] + " " + line.strip())
        delta = code.count("{") - code.count("}")
        if "{" in code:
            if delta > 0:
                key = block_key(*self.pending) if self.pending else None
                self.blocks.append((self.depth, key))
            # Opened and closed on one line (`impl Eq for X {}`): the header
            # is spent either way.
            self.pending = None
        self.depth = max(self.depth + delta, 0)
        while self.blocks and self.blocks[-1][0] >= self.depth:
            self.blocks.pop()


def git(*args, allow_missing_path=False):
    """Run git, failing closed.

    A silent failure here is worse than a crash: an errored `git diff` yields
    an empty file list, the checker reports "no documentation lost" and exits
    zero, and the gate has passed by not running.

    `allow_missing_path` narrows that tolerance to exactly the expected case.
    Tolerating every non-zero exit would let an invalid revision or a
    malformed object read as empty content, which is the same fail-open bug
    one door further in. A probe whose exit status IS the answer does not go
    through here at all: see `exists_at`.
    """
    proc = subprocess.run(["git", *args], capture_output=True, text=True)
    if proc.returncode != 0:
        if allow_missing_path and MISSING_PATH.search(proc.stderr):
            return ""
        raise SystemExit(
            f"git {' '.join(args)} failed ({proc.returncode}): {proc.stderr.strip()}"
        )
    return proc.stdout


def exists_at(rev, path):
    """Whether `path` is present at `rev`.

    A probe: `git cat-file -e` prints nothing either way and answers with
    its exit status. Reading its stdout, as the added-file detection once
    did, made every changed file look branch-added: each took the pairwise
    commit walk (minutes on a real branch, for nothing) and a capture in an
    existing file was reported twice, once per loop.
    """
    proc = subprocess.run(
        ["git", "cat-file", "-e", f"{rev}:{path}"], capture_output=True, text=True
    )
    return proc.returncode == 0


def added_files(base, changed):
    """The changed files that do not exist at `base`: the branch added them,
    so their history lives only in its own commits."""
    return [f for f in changed if not exists_at(base, f)]


def is_doc(line):
    """A `///` outer doc comment, and not a `////` rule.

    Four or more slashes is an ordinary comment to rustc, so counting it as
    documentation invents losses that never happened.
    """
    s = line.lstrip()
    return s.startswith("///") and not s.startswith("////")


# Lines a doc comment on this repo can never be MEANT for. Deliberately a
# short allowlist of shapes that have actually stranded prose rather than an
# attempt to enumerate everything: a false accusation here is worse than a
# miss, because a gate that cries wolf stops being read. A plain `use` is
# the one observed in the wild (#436).
#
# Any VISIBILITY-QUALIFIED `use` is excluded, not just `pub use`.
# Documenting a re-export is legitimate Rust and rustdoc renders a `pub use`,
# so flagging one would fail a correct PR; `pub(crate) use` does not reach
# an external reader either, but a doc above one is a deliberate note rather
# than stranded prose. A bare private `use` is the only form that has ever
# taken another item's doc here, and the only one this treats as unable to
# hold prose.
NEVER_DOCUMENTED = re.compile(r"^\s*(?:use|extern\s+crate)\s")


# What a line is, for the backward scan. Computed in one FORWARD pass,
# because both multi-line constructs (an attribute spanning lines, a `/** */`
# doc block) can only be recognised by reading downward: from below, `))]` and
# `*/` are indistinguishable from code.
DOC, ATTR, SKIP, CODE = "doc", "attr", "skip", "code"


def classify(lines):
    """Label every line DOC, ATTR, SKIP (blank or ordinary comment) or CODE.

    Both multi-line shapes were false-ACCUSATION bugs rather than misses: an
    attribute spanning lines, or a `/** */` block doc, made a documented item
    read as undocumented and the gate reported a loss that had not happened.
    For a gate that is the worse direction, since a gate that cries wolf stops
    being read.
    """
    kinds = []
    attr_depth = 0
    in_block_doc = False
    in_block_comment = False
    for raw in lines:
        s = raw.strip()
        if in_block_doc:
            kinds.append(DOC)
            if "*/" in s:
                in_block_doc = False
            continue
        if in_block_comment:
            kinds.append(SKIP)
            if "*/" in s:
                in_block_comment = False
            continue
        if attr_depth > 0:
            kinds.append(ATTR)
            attr_depth += s.count("[") - s.count("]")
            continue
        # `/** ... */` is an outer doc comment lowering to the same `#[doc]`
        # attribute as `///`. `/*! */` is INNER (it documents the enclosing
        # item, not the next one) and `/***` is an ordinary comment, so
        # neither counts.
        if s.startswith("/**") and not s.startswith("/***"):
            kinds.append(DOC)
            if "*/" not in s[2:]:
                in_block_doc = True
            continue
        if s.startswith("/*"):
            kinds.append(SKIP)
            if "*/" not in s[2:]:
                in_block_comment = True
            continue
        if s.startswith("#["):
            kinds.append(ATTR)
            depth = s.count("[") - s.count("]")
            attr_depth = max(depth, 0)
            continue
        if is_doc(raw):
            kinds.append(DOC)
            continue
        if s == "" or s.startswith("//"):
            kinds.append(SKIP)
            continue
        kinds.append(CODE)
    return kinds


def documented(text):
    """Map item key -> True when ANY definition under it carries a doc comment.

    Keyed by name because two revisions cannot be lined up by position, and
    a method's name is qualified by its enclosing `impl` or `trait` header
    (`Foo::new`, `Display for Foo::fmt`) because a bare name is not unique
    where it matters most: `new` across impl blocks is the common shape,
    and merging them let a capture on one hide behind the other's surviving
    prose (#405). Items outside any impl, and items in a `mod` or a `fn`
    body, keep the bare name, so every pre-existing key for those survives.
    Where one key still covers several definitions (a `cfg`-gated pair, say)
    "any documented" remains the deliberately conservative reading: the
    check fires only when every definition under that key has lost its
    prose. Under-reporting there is the safer direction for a gate that
    blocks merges; a mis-aligned key would turn it into a false accusation.

    `///` lowers to an outer `#[doc]` attribute, and an attribute is not
    detached from its item by blank lines or ordinary comments - verified
    against rustc, which warns `unused_doc_comments` when a doc really is
    orphaned and stays silent here, and against rustdoc, which renders the
    prose on the function in both shapes. So the backward scan steps over
    attributes, blanks and `//` comments alike; stopping at the first blank
    reported documentation as missing when rustc could see it perfectly well.
    """
    lines = text.splitlines()
    kinds = classify(lines)
    state = {}
    tracker = BlockTracker()
    for i, line in enumerate(lines):
        # The scope an item belongs to is the one in force BEFORE its own
        # line: `trait A {` is itself an item, keyed to whatever encloses
        # it, not to the block it opens.
        scope = tracker.scope()
        if kinds[i] == CODE:
            tracker.feed(line)
        m = ITEM.match(line)
        if not m:
            continue
        # Exactly one alternative captures per match.
        name = next(g for g in m.groups() if g)
        if scope:
            name = f"{scope}::{name}"
        j = i - 1
        while j >= 0 and kinds[j] in (ATTR, SKIP):
            j -= 1
        has_doc = j >= 0 and kinds[j] == DOC
        state[name] = state.get(name, False) or has_doc
    return state


def orphaned_docs(text):
    """Doc blocks in `text` that no item can be attached to.

    The diff-based check above cannot see a capture whose VICTIM is new on
    the branch: relative to the merge base that item does not exist, so it
    cannot have lost a doc it never had (#427). Nor can it see one where
    the capturing item is of a kind the `ITEM` regex does not model, such
    as a `use` (#436).

    Both are the same shape at HEAD, and it needs no base revision to see:
    a `///` block that is not immediately followed by something a doc can
    attach to. When an item is inserted between a doc and its subject, the
    ORIGINAL doc lands on the newcomer and the newcomer's own doc — or, for
    a `use`, nothing at all — is left with no item beneath it.

    Deliberately narrow. Only a doc block followed by another DOC block, or
    by a non-item line that ends the block's reach, is reported; a doc above
    an `impl`, a `mod`, a macro invocation or any other legal-but-unmodelled
    item is left alone, because reporting those would be the false accusation
    that stops a gate being read. The check answers "is this prose stranded",
    not "do I recognise what follows".

    KNOWN LIMITATION, and the reason #427 is only partly closed: this sees a
    capture solely through the doc it STRANDS. When the inserted item carries
    its own doc, nothing is stranded — the original prose has silently moved
    to the newcomer and both items look documented — and when the inserted
    item is a `mod`, an `impl` or a macro invocation, the doc above it is
    legal, so that case is deliberately unreportable here. All three remain
    invisible to a merge-base comparison too when the victim is new on the
    branch, which is what #427 filed. Catching them needs the commit-walk
    that issue also proposed, or a semantic check of whether prose describes
    the item beneath it; neither is in scope for this pass.
    """
    lines = text.splitlines()
    kinds = classify(lines)
    orphans = []
    i = 0
    while i < len(lines):
        if kinds[i] != DOC:
            i += 1
            continue
        start = i
        while i < len(lines) and kinds[i] == DOC:
            i += 1
        # Attributes and blank lines sit legally between a doc and its item.
        j = i
        while j < len(lines) and kinds[j] in (ATTR, SKIP):
            j += 1
        if j >= len(lines):
            # A doc block at end of file documents nothing.
            orphans.append((start + 1, lines[start].strip(), "end of file"))
            continue
        if kinds[j] == DOC:
            # Two doc blocks with no item between them: the first cannot
            # reach an item, because the second one gets there first. This
            # is what an insertion above a documented item leaves behind.
            orphans.append((start + 1, lines[start].strip(), "another doc block"))
            continue
        if NEVER_DOCUMENTED.match(lines[j]):
            # A line that syntactically cannot carry a doc comment. `use`
            # is the one that has actually happened (#436) — an import
            # dropped between a doc and its function takes prose that
            # rustdoc then renders against the import, and neither the
            # diff check nor `unused_doc_comments` says a word.
            orphans.append((start + 1, lines[start].strip(), lines[j].strip()))
    return orphans


def _doc_blocks(lines, kinds):
    """Every `///` block in the file, with the line it documents.

    Yields `(start, end, subject)`: the block spans `lines[start:end]`, and
    `subject` is the first line under it that a doc can attach to - blank
    lines, ordinary comments and attributes sit legally in between - or None
    when the block reaches the end of the file. The subject is the line's own
    text rather than a parsed item, because the captures that get through the
    other two passes are on kinds `ITEM` deliberately does not model: an enum
    variant is what #455 was filed for.
    """
    i = 0
    while i < len(lines):
        if kinds[i] != DOC:
            i += 1
            continue
        start = i
        while i < len(lines) and kinds[i] == DOC:
            i += 1
        j = i
        while j < len(lines) and kinds[j] in (ATTR, SKIP):
            j += 1
        # A block followed by ANOTHER doc block documents nothing, and the
        # head-only pass above reports exactly that. Handing the second
        # block's text back as a "subject" made one defect print two
        # annotations, the second of them saying the prose now describes a
        # `///` line, which is not an item and not something a reader can act
        # on. The two passes cannot now report the same block: this one needs
        # a real subject, and the other one fires only when there is none.
        subject = lines[j].strip() if j < len(lines) and kinds[j] == CODE else None
        yield start, i, subject


# A doc line carrying no prose of its own: the `///` separators a long block
# is full of, and the `*/` that closes a `/** */` one. They repeat, they move
# whenever anything near them moves, and they say nothing about which item
# they describe. The lookahead is what keeps `*/` out: the continuation-line
# branch matches its `*`, leaving `/` to read as prose.
DOC_PROSE = re.compile(r"^\s*(?:///|/\*\*+|\*)\s*(?!\*?/\s*$)(\S.*)$")


def doc_subjects(text):
    """Doc lines that occur exactly once in `text`, and what each sits above.

    Uniqueness is the whole guard against reading a coincidence as a move: a
    `/// The width, in cells.` that appears above three different fields has
    no single owner to have changed, so it is dropped rather than guessed at.
    """
    lines = text.splitlines()
    kinds = classify(lines)
    subjects = {}
    seen = {}
    for start, end, subject in _doc_blocks(lines, kinds):
        for k in range(start, end):
            doc = lines[k].strip()
            seen[doc] = seen.get(doc, 0) + 1
            subjects[doc] = (subject, k + 1)
    return {
        doc: where
        for doc, where in subjects.items()
        if seen[doc] == 1 and DOC_PROSE.match(doc)
    }


def documented_subjects(text):
    """Every line that some doc block reaches, unique or not."""
    lines = text.splitlines()
    return {
        subject
        for _start, _end, subject in _doc_blocks(lines, classify(lines))
        if subject
    }


def line_counts(text):
    """How often each stripped line occurs, for the "still there, and only
    once" test the report depends on."""
    counts = {}
    for line in text.splitlines():
        stripped = line.strip()
        counts[stripped] = counts.get(stripped, 0) + 1
    return counts


def reassigned_docs(before, after):
    """Doc lines that changed the item they sit above, leaving it bare (#455).

    The two passes above see a capture only through prose that is STRANDED -
    a doc block with nothing a doc can attach to under it. A newcomer that
    brings its own doc strands nothing: rustfmt leaves the stolen line and
    the thief's own contiguous, they read as one ordinary two-line block, and
    at HEAD the file is indistinguishable from one where somebody wrote a
    two-line doc. That is why #455 could not be closed in the snapshot; the
    signal only exists in the diff, where the same line used to sit above a
    different item.

    Three conditions have to hold together, and each one is a false-accusation
    class removed rather than a nicety:

    * the doc line is unchanged and unique on both sides, so there is exactly
      one thing it can be talking about;
    * its old subject is still in the file, exactly once, and now has no doc
      block of its own - prose that merely moved between two documented items
      cost nobody their documentation;
    * the line it landed on is NEW. Moving a doc DOWN onto an item that was
      already there is how a capture gets repaired, and a gate that fails the
      fix as well as the bug is one people route around.

    Returns `(line at head, the doc line, the old subject, the new one)`.
    """
    before_map = doc_subjects(before)
    after_map = doc_subjects(after)
    if not before_map or not after_map:
        return []
    before_lines = line_counts(before)
    after_lines = line_counts(after)
    documented = documented_subjects(after)
    found = []
    for doc, (old, _at) in before_map.items():
        moved = after_map.get(doc)
        if not old or not moved:
            continue
        new, line_no = moved
        if not new or new == old:
            continue
        if after_lines.get(old, 0) != 1 or old in documented:
            continue
        if before_lines.get(new, 0):
            continue
        found.append((line_no, doc, old, new))
    # One insertion is one defect. Every line of a block sits above the same
    # item, so a three-line doc reported per line names the same victim and
    # the same thief three times; the block's FIRST line is the one a reader
    # recognises, and it is the lowest line number.
    per_subject = {}
    for line_no, doc, old, new in sorted(found):
        per_subject.setdefault(old, (line_no, doc, old, new))
    return sorted(per_subject.values())


# The leading name on a subject line. `r#` is part of the name, not a
# prefix to skip: `r#type` and `r#match` are different items, and
# capturing `r` for both would let one declared removal excuse the other.
SUBJECT_KEY = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:#\[[^\]]*\]\s*)*((?:r#)?[A-Za-z_]\w*)")


# Words that lead an item rather than name one.
ITEM_KEYWORDS = frozenset(
    {
        "mod",
        "impl",
        "trait",
        "enum",
        "struct",
        "union",
        "type",
        "const",
        "static",
        "fn",
        # Without the bang: `SUBJECT_KEY` captures `\w*`, which stops before it.
        "macro_rules",
        "pub",
        "unsafe",
        "async",
        "default",
        "extern",
    }
)


def subject_key(line):
    """The name to declare a deliberate removal against, for a subject line
    `ITEM` does not model.

    #455 was filed for an enum variant, and a variant is not an item this
    file parses. Without a key derived from the line itself, the branch would
    have a merge-blocking check and no way to declare a removal against it,
    which is the one thing that turns a gate into an obstacle.
    """
    # An impl or trait header keys the way the loss pass already keys its
    # methods, so one declaration cannot cover two impls of one type: `impl
    # fmt::Display for Foo {` is `Display for Foo`, not `fmt`. Taking the
    # first word gave `fmt` to both a Display and a Debug impl, and one
    # `doc-removal: a.rs::fmt` excused either.
    header = BLOCK_HEADER.match(line)
    if header:
        kind = "trait" if "trait" in header.group(0)[: header.start("rest")] else "impl"
        return block_key(kind, header.group("rest")) or None

    # EVERY leading keyword, not one. `pub async unsafe extern "C" fn bar(`
    # walks four before the name, and stopping after the first keyed it as
    # `unsafe`. The bound is a guard against a pathological line rather than
    # a real limit: Rust has no item with eight qualifiers.
    rest = line
    for _ in range(8):
        m = SUBJECT_KEY.match(rest)
        if not m:
            return None
        name = m.group(1)
        if name not in ITEM_KEYWORDS:
            return name
        rest = rest[m.end():]
        if name == "extern":
            # `extern "C" fn bar(`: the ABI is a string literal, which the
            # name pattern cannot step over.
            rest = re.sub(r'^\s*"[^"]*"', "", rest)
        elif name == "macro_rules":
            # The bang belongs to the keyword, and `\w` stops before it.
            rest = rest.lstrip().removeprefix("!")
        rest = rest.lstrip()
    return None


def git_ok(*args):
    """Whether a git command succeeds, for a question rather than an answer."""
    return (
        subprocess.run(
            ("git",) + args, capture_output=True, text=True, check=False
        ).returncode
        == 0
    )


def item_name(line):
    """The bare item name a subject line declares, or None.

    Only used to tell the two passes apart: when the diff pass and the loss
    pass have found the same capture, the loss pass owns the report, and one
    defect must not print two annotations.
    """
    m = ITEM.match(line)
    return next((g for g in m.groups() if g), None) if m else None


def main():
    base, head = sys.argv[1], sys.argv[2]
    declared = git("log", f"{base}..{head}", "--format=%B")
    # File-qualified: `doc-removal: src/foo.rs::bar`. Keyed on the bare name,
    # one deliberate removal of `new` would excuse every `new` in every
    # changed file, including an accidental loss elsewhere in the same branch.
    exempt = {
        (path, name)
        for path, name in re.findall(
            # `r#` belongs to the name it prefixes: `r#type` and `r#match` are
            # different items, and a pattern stopping at `r` would read both
            # declarations as the same one.
            # Three shapes, because the gate prints three. A bare name
            # (`bar`); a method under its block (`Foo::new`, `Display for
            # Foo::fmt`); and, since the diff pass may report an impl HEADER
            # as the victim, that header on its own (`fmt::Display for Foo`).
            # A declaration the error tells you to write has to parse back.
            r"doc-removal:\s*([\w./-]+\.rs)::"
            r"((?:trait )?(?:r#)?[A-Za-z_][\w:]*(?: for (?:r#)?[A-Za-z_][\w:]*)?"
            r"(?:::(?:r#)?[A-Za-z_]\w*)?)",
            declared,
        )
    }
    changed = [
        f for f in git("diff", "--name-only", base, head).splitlines()
        if f.endswith(".rs")
    ]
    losses = []
    # A file the branch ADDS has no base version, so `documented(before)` is
    # empty and no loss can ever be reported in it: a doc captured within the
    # branch is invisible to a merge-base comparison. Those files are checked
    # commit-by-commit instead, which is the only place their history exists.
    added = added_files(base, changed)
    if added:
        # What matters is the state at HEAD. A doc captured in one commit and
        # restored in a later one is not a loss: the branch is fine, and
        # reporting it blocks a PR whose head is correct. Without this the
        # pairwise pass reports every intermediate state a branch passed
        # through, which is exactly the false accusation that makes a gate
        # untrustworthy.
        head_state = {f: documented(git("show", f"{head}:{f}", allow_missing_path=True)) for f in added}
        commits = git("log", f"{base}..{head}", "--format=%H", "--reverse").split()
        for older, newer in zip(commits, commits[1:]):
            for f in added:
                before = documented(git("show", f"{older}:{f}", allow_missing_path=True))
                after = documented(git("show", f"{newer}:{f}", allow_missing_path=True))
                for name, had_doc in before.items():
                    if (
                        had_doc
                        and name in after
                        and not after[name]
                        and (f, name) not in exempt
                        and not head_state[f].get(name, False)
                        and not any(loss[0] == f and loss[1] == name for loss in losses)
                    ):
                        # Name the commit the doc was last seen at. Saying
                        # "at {base}" would be wrong for a file the branch
                        # ADDED: it does not exist there, and a reader sent
                        # to that revision finds nothing.
                        losses.append((f, name, older))
    for f in changed:
        # A path can legitimately be absent on one side: the branch added the
        # file, or deleted it. That is the one git failure this tolerates -
        # `allow_missing_path` exists for exactly it, and not passing it here
        # made the gate abort on every PR that adds a Rust file, which is how
        # it failed on the first one that did.
        before = documented(git("show", f"{base}:{f}", allow_missing_path=True))
        after = documented(git("show", f"{head}:{f}", allow_missing_path=True))
        for name, had_doc in before.items():
            if had_doc and name in after and not after[name] and (f, name) not in exempt:
                losses.append((f, name, base))
    # The head-only pass (#427, #436): a capture the diff cannot see because
    # its victim is new on the branch, or because the capturing item is of a
    # kind `ITEM` does not model. Runs on the same changed files, needs no
    # base, and reports separately so the two failure shapes stay legible.
    # The diff pass (#455): a doc line that changed the item it sits above.
    # Runs per file over the revisions that actually touched it - the merge
    # base, then each commit on the branch - so a capture made in one commit
    # of a branch is seen even though the merge base predates both items.
    # Every candidate is then re-tested against HEAD, because a capture that
    # a later commit repaired is not a defect in the branch being merged.
    orphans = []
    reassigned = []
    for f in changed:
        head_text = git("show", f"{head}:{f}", allow_missing_path=True)
        if not head_text:
            continue
        for line_no, first, follower in orphaned_docs(head_text):
            orphans.append((f, line_no, first, follower))
        # `--no-merges`, and the omission is not a shortcut. A merge's first
        # parent is the branch tip, so the pair `(M^, M)` is everything the
        # OTHER side brought in, replayed as though this branch had written
        # it - and this repo's convention is to merge main into a branch that
        # needs it, so that is the common shape rather than an exotic one. The
        # declaration that excused such a change on main is invisible here
        # too, because the commit-message scan starts above the merge base, so
        # the report would name an escape hatch the reader cannot use.
        #
        # What it COSTS, stated rather than waved at: a capture made by the
        # resolution itself, when the thief is a line the other side already
        # had. The base-to-head pair does not cover that one, because "the
        # line the prose landed on is new" is false for a line main brought
        # in. A resolution that invents a new thief line IS still caught.
        # `test_a_capture_inside_a_merge_resolution_is_a_known_gap` pins that
        # boundary, and an earlier version of this comment claimed the pair
        # covered it, which was wrong.
        touched = git(
            "log", f"{base}..{head}", "--no-merges", "--format=%H", "--reverse", "--", f
        ).split()
        # Each commit against its OWN first parent, not against the previous
        # entry in the log. Path limiting simplifies history before `--reverse`
        # orders it, so two adjacent entries need not be parent and child, and
        # comparing them reads a doc as having moved between states that were
        # never one edit apart.
        #
        # The base-to-head pair is kept as well, and it is not redundant: the
        # per-commit walk asks whether the line the prose landed on is new IN
        # THAT PAIR, so a capture split across two commits - the thief added
        # above the block in one, moved under it in the next - is invisible to
        # every pair but this one. It is also the pair that caught the real
        # #454 instance.
        pairs = [(git("show", f"{base}:{f}", allow_missing_path=True), head_text)]
        for rev in touched:
            # A root commit has no `rev^`, which is a git failure rather than
            # a missing path, so it would abort instead of being skipped.
            # Unreachable under CI's full-depth checkout, reachable in a
            # shallow clone running the documented local invocation.
            if not git_ok("rev-parse", "--verify", f"{rev}^"):
                continue
            pairs.append(
                (
                    git("show", f"{rev}^:{f}", allow_missing_path=True),
                    git("show", f"{rev}:{f}", allow_missing_path=True),
                )
            )
        candidates = {}
        for older, newer in pairs:
            if not older or not newer or older == newer:
                continue
            for _line, doc, old, new in reassigned_docs(older, newer):
                # FIRST owner wins. A doc captured twice on one branch
                # (A -> B -> C) would otherwise be reported as having left B,
                # which is the thief from the first move; the item that
                # actually lost its documentation is A, and a reader who
                # repairs the item the message names leaves A bare.
                candidates.setdefault(doc, (old, new))
        if not candidates:
            continue
        losses_keys = [(loss[0], loss[1]) for loss in losses]
        at_head = doc_subjects(head_text)
        counts = line_counts(head_text)
        has_doc = documented_subjects(head_text)
        for doc, (old, new) in candidates.items():
            here = at_head.get(doc)
            if not here or not here[0] or here[0] == old:
                continue
            if counts.get(old, 0) != 1 or old in has_doc:
                continue
            # The same capture, when its victim is an item `ITEM` models and
            # was documented at the base, is already reported as a loss. One
            # defect, one annotation: the loss message is the older and more
            # precise of the two, so it keeps the report. The declared-removal
            # exemption travels with it for the same reason.
            modelled = item_name(old)
            if modelled and any(
                path == f and (name == modelled or name.endswith(f"::{modelled}"))
                for path, name in losses_keys
            ):
                continue
            # The declared-removal hatch reaches this pass too. Keyed on the
            # modelled name where there is one, and otherwise on the leading
            # name of the subject line - an enum variant has no other key, and
            # a variant is what #455 was filed for.
            key = modelled or subject_key(old)
            if key and any(
                path == f and (name == key or name.endswith(f"::{key}"))
                for path, name in exempt
            ):
                continue
            reassigned.append((f, here[1], doc, old, here[0], key))

    for f, name, at in losses:
        print(
            f"::error file={f}::`{name}` had a doc comment at {at[:12]} and has none now. "
            "A doc block above it was most likely captured by an item inserted between the two "
            "(#314), which hands one item's prose to another with nothing failing. Restore it, "
            "or declare the removal with `doc-removal: " + f"{f}::{name}` in a commit message."
        )
    for f, line_no, first, follower in orphans:
        print(
            f"::error file={f},line={line_no}::this doc block documents nothing: "
            f"the next thing after it is {follower!r}, which no doc comment can "
            "attach to (#314/#427/#436). An item inserted above a documented one "
            "takes that item's prose and strands its own, and neither the diff "
            "check nor rustc reports it. Move the inserted item above the block, "
            f"or give the block an item. First line: {first!r}"
        )
    for f, line_no, doc, old, new, key in reassigned:
        declare = (
            f" Or declare the removal with `doc-removal: {f}::{key}` in a commit message."
            if key
            else ""
        )
        print(
            f"::error file={f},line={line_no}::this doc comment described {old!r} "
            f"before this branch and describes {new!r} now, leaving {old!r} with no "
            "documentation at all (#455). An item inserted directly above a documented "
            "one takes its prose: nothing is stranded, so the head-only pass cannot see "
            "it, and the rendered docs are confidently wrong rather than absent. Move "
            f"the new item above the doc block, or give it a doc of its own. Line: {doc!r}"
            f"{declare}"
        )
    if losses or orphans or reassigned:
        if losses:
            print(f"\n{len(losses)} item(s) lost documentation.", file=sys.stderr)
        if orphans:
            print(f"{len(orphans)} doc block(s) document nothing.", file=sys.stderr)
        if reassigned:
            print(f"{len(reassigned)} doc comment(s) changed owner.", file=sys.stderr)
        return 1
    print(
        f"No documentation lost or stranded across {len(changed)} changed Rust file(s)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
