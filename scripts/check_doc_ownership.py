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
    r"(?:extern\s+\"[^\"]*\"\s+)?(?:"
    r"fn\s+([A-Za-z_]\w*)"
    r"|const\s+([A-Za-z_]\w*)\s*:"
    r"|static\s+(?:mut\s+)?([A-Za-z_]\w*)\s*:"
    r"|struct\s+([A-Za-z_]\w*)"
    r"|enum\s+([A-Za-z_]\w*)"
    r"|union\s+([A-Za-z_]\w*)"
    r"|trait\s+([A-Za-z_]\w*)"
    r"|type\s+([A-Za-z_]\w*)"
    r"|macro_rules!\s+([A-Za-z_]\w*)"
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


# What a line is, for the backward scan. Computed in one FORWARD pass,
# because both multi-line constructs (an attribute spanning lines, a `/** */`
# doc block) can only be recognised by reading downward: from below, `))]` and
# `*/` are indistinguishable from code.
# Lines a doc comment on this repo can never be MEANT for. Deliberately a
# short allowlist of shapes that have actually stranded prose rather than an
# attempt to enumerate everything: a false accusation here is worse than a
# miss, because a gate that cries wolf stops being read. A plain `use` is
# the one observed in the wild (#436).
#
# `pub use` is excluded on purpose. Documenting a re-export is legitimate
# Rust and rustdoc renders it, so flagging one would fail a correct PR —
# and a private `use`, which cannot appear in the docs at all, is the only
# form that has ever taken another item's prose here.
NEVER_DOCUMENTED = re.compile(r"^\s*(?:use|extern\s+crate)\s")

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


def main():
    base, head = sys.argv[1], sys.argv[2]
    declared = git("log", f"{base}..{head}", "--format=%B")
    # File-qualified: `doc-removal: src/foo.rs::bar`. Keyed on the bare name,
    # one deliberate removal of `new` would excuse every `new` in every
    # changed file, including an accidental loss elsewhere in the same branch.
    exempt = {
        (path, name)
        for path, name in re.findall(
            r"doc-removal:\s*([\w./-]+\.rs)::((?:(?:trait )?[A-Za-z_][\w:]*(?: for [A-Za-z_][\w:]*)?::)?[A-Za-z_]\w*)",
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
                        and not any(l[0] == f and l[1] == name for l in losses)
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
    orphans = []
    for f in changed:
        text = git("show", f"{head}:{f}", allow_missing_path=True)
        if not text:
            continue
        for line_no, first, follower in orphaned_docs(text):
            orphans.append((f, line_no, first, follower))
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
    if losses or orphans:
        if losses:
            print(f"\n{len(losses)} item(s) lost documentation.", file=sys.stderr)
        if orphans:
            print(f"{len(orphans)} doc block(s) document nothing.", file=sys.stderr)
        return 1
    print(f"No documentation lost across {len(changed)} changed Rust file(s).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
