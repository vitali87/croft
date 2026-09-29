# croft Live Run instrumenter (see src/live_run.rs).
#
# Runs one Python file with its source read from stdin (the editor buffer,
# unsaved edits included) and writes a JSON report of what every line did:
# the values its assignments produced, what it printed, what it returned,
# whether it ran at all, and where the run died.
#
# argv: <target path> <report path> <budget seconds>
#
# Values come from an AST rewrite rather than a line tracer: a record call is
# spliced after each assignment, so a hot loop pays one call per assignment
# instead of a trace callback per line. Line coverage alone uses
# sys.monitoring (3.12+, events disabled after the first hit) or, on older
# interpreters, a trace function scoped to the target file's frames.

import ast
import io
import json
import os
import reprlib
import signal
import sys
import time

TARGET, REPORT, BUDGET = sys.argv[1], sys.argv[2], float(sys.argv[3])
MAX_RECORDS = 200_000
MAX_SEEN = 3
MAX_OUT_CHARS = 400
MAX_STDOUT = 64 * 1024

_repr = reprlib.Repr()
_repr.maxstring = 80
_repr.maxother = 80
_repr.maxlist = _repr.maxtuple = _repr.maxset = _repr.maxfrozenset = 8
_repr.maxdict = 6
_repr.maxlevel = 3

# (line, label) -> [count, [first values], last value], in first-hit order
slots = {}
# line -> [text tail, write count]
outputs = {}
stdout_all = []
stdout_len = 0
records = 0
covered = set()
_SKIP_TYPES = (type(os), type(len), type(lambda: 0), type)


def _show(value):
    try:
        cls = type(value)
        # The default `<__main__.Point object at 0x7f...>` says nothing a
        # reader wants; the instance's fields do.
        if cls.__repr__ is object.__repr__ and isinstance(getattr(value, "__dict__", None), dict):
            fields = ", ".join("%s=%s" % (k, _repr.repr(v))
                               for k, v in list(vars(value).items())[:6] if not k.startswith("_"))
            return "%s(%s)" % (cls.__name__, fields)
        return _repr.repr(value)
    except BaseException as exc:  # a __repr__ that raises is still a value
        return "<repr failed: %s>" % type(exc).__name__


class _Shown(str):
    """A value already rendered, as opposed to a raw str still to render."""


class _Deferred:
    __slots__ = ("value",)

    def __init__(self, value):
        self.value = value


# Values that cannot change after the line that produced them, so holding
# the object and rendering it once at the end is exact, and a hot loop pays
# a dict update instead of a repr per iteration.
_SCALARS = frozenset((int, float, complex, bool, str, bytes, type(None)))


def _hold(value):
    return value if type(value) in _SCALARS else _Shown(_show(value))


def _render(held):
    if isinstance(held, _Shown):
        return str(held)
    if isinstance(held, _Deferred):
        return _show(held.value)
    return _show(held)


def _note(line, label, value):
    global records
    records += 1
    key = (line, label)
    slot = slots.get(key)
    if slot is None:
        held = _hold(value)
        slots[key] = [1, [held], held]
        return
    slot[0] += 1
    if records > MAX_RECORDS and type(value) not in _SCALARS:
        # Past the budget a repr per hit is the cost that matters, so keep a
        # reference and render it once at the end. A stale "last" would be a
        # lie; a late render of a mutable object is the lesser one.
        slot[2] = _Deferred(value)
        return
    held = _hold(value)
    if len(slot[1]) < MAX_SEEN:
        slot[1].append(held)
    slot[2] = held


def __lv_rec__(line, pairs):
    for label, value in pairs:
        if isinstance(value, _SKIP_TYPES) and label.isidentifier():
            continue
        _note(line, label, value)


def __lv_ret__(line, value):
    _note(line, "↩", value)
    return value


def __lv_expr__(line, value):
    if value is not None:
        _note(line, "→", value)
    return value


def __lv_call__(line, label, receiver, value):
    # Arguments evaluate left to right, so `receiver` is the object the
    # method ran on; a None result is the mutator convention (append, sort,
    # update), and then the receiver's new state is the interesting value.
    if value is None:
        if not isinstance(receiver, _SKIP_TYPES):
            _note(line, label, receiver)
    else:
        _note(line, "\u2192", value)
    return value


def _load(node):
    node = ast.parse(ast.unparse(node), mode="eval").body
    return node


def _labels(target):
    """(label, load-expression) pairs for an assignment target."""
    if isinstance(target, ast.Name):
        return [(target.id, ast.Name(target.id, ast.Load()))]
    if isinstance(target, (ast.Tuple, ast.List)):
        out = []
        for elt in target.elts:
            out.extend(_labels(elt))
        return out
    if isinstance(target, ast.Starred):
        return _labels(target.value)
    if isinstance(target, (ast.Attribute, ast.Subscript)):
        # Re-reading `obj.attr` / `d[k]` is what the line just wrote; a call
        # inside the target could have side effects, so fall back to the root.
        if not any(isinstance(n, (ast.Call, ast.Await, ast.Yield, ast.NamedExpr))
                   for n in ast.walk(target)):
            return [(ast.unparse(target), _load(target))]
        root = target
        while isinstance(root, (ast.Attribute, ast.Subscript)):
            root = root.value
        if isinstance(root, ast.Name):
            return [(root.id, ast.Name(root.id, ast.Load()))]
    return []


def _is_literal(node):
    try:
        ast.literal_eval(node)
    except (ValueError, TypeError, SyntaxError, MemoryError, RecursionError):
        return False
    return True


def _rec_stmt(line, pairs):
    call = ast.Call(
        ast.Name("__lv_rec__", ast.Load()),
        [ast.Constant(line),
         ast.Tuple([ast.Tuple([ast.Constant(l), e], ast.Load()) for l, e in pairs], ast.Load())],
        [],
    )
    return ast.Try(
        body=[ast.Expr(call)],
        handlers=[ast.ExceptHandler(ast.Name("Exception", ast.Load()), None, [ast.Pass()])],
        orelse=[],
        finalbody=[],
    )


def _prepend(body, stmt):
    # A docstring must stay the first statement or it stops being one.
    at = 1 if (body and isinstance(body[0], ast.Expr)
               and isinstance(body[0].value, ast.Constant)
               and isinstance(body[0].value.value, str)) else 0
    body.insert(at, stmt)


class Instrument(ast.NodeTransformer):
    def _after(self, node, targets):
        self.generic_visit(node)
        pairs = []
        for t in targets:
            pairs.extend(_labels(t))
        return [node, _rec_stmt(node.lineno, pairs)] if pairs else node

    def visit_Assign(self, node):
        if _is_literal(node.value):
            # `total = 0` already says what `total` is; echoing it is noise.
            self.generic_visit(node)
            return node
        return self._after(node, node.targets)

    def visit_AugAssign(self, node):
        return self._after(node, [node.target])

    def visit_AnnAssign(self, node):
        if node.value is None or _is_literal(node.value):
            self.generic_visit(node)
            return node
        return self._after(node, [node.target])

    def _loop(self, node):
        self.generic_visit(node)
        pairs = _labels(node.target)
        if pairs:
            node.body.insert(0, _rec_stmt(node.lineno, pairs))
        return node

    visit_For = visit_AsyncFor = _loop

    def _with(self, node):
        self.generic_visit(node)
        pairs = []
        for item in node.items:
            if item.optional_vars is not None:
                pairs.extend(_labels(item.optional_vars))
        if pairs:
            node.body.insert(0, _rec_stmt(node.lineno, pairs))
        return node

    visit_With = visit_AsyncWith = _with

    def _func(self, node):
        self.generic_visit(node)
        a = node.args
        names = [x.arg for x in a.posonlyargs + a.args + a.kwonlyargs]
        names += [x.arg for x in (a.vararg, a.kwarg) if x is not None]
        names = [n for n in names if n not in ("self", "cls")]
        if names:
            pairs = [(n, ast.Name(n, ast.Load())) for n in names]
            _prepend(node.body, _rec_stmt(node.lineno, pairs))
        return node

    visit_FunctionDef = visit_AsyncFunctionDef = _func

    def visit_Return(self, node):
        self.generic_visit(node)
        if node.value is not None:
            node.value = ast.Call(ast.Name("__lv_ret__", ast.Load()),
                                  [ast.Constant(node.lineno), node.value], [])
        return node

    def visit_Expr(self, node):
        self.generic_visit(node)
        value = node.value
        if isinstance(value, ast.Constant):
            return node
        if (isinstance(value, ast.Call) and isinstance(value.func, ast.Attribute)
                and isinstance(value.func.value, ast.Name)):
            # `items.append(x)` changed `items`: show what it holds now.
            name = value.func.value.id
            node.value = ast.Call(ast.Name("__lv_call__", ast.Load()),
                                  [ast.Constant(node.lineno), ast.Constant(name),
                                   ast.Name(name, ast.Load()), value], [])
            return node
        node.value = ast.Call(ast.Name("__lv_expr__", ast.Load()),
                              [ast.Constant(node.lineno), value], [])
        return node


class Capture(io.TextIOBase):
    """stdout/stderr that remembers which target line wrote each chunk."""

    def writable(self):
        return True

    def write(self, s):
        global stdout_len
        if not isinstance(s, str):
            s = str(s)
        if stdout_len < MAX_STDOUT:
            stdout_all.append(s[: MAX_STDOUT - stdout_len])
            stdout_len += len(s)
        frame = sys._getframe(1)
        while frame is not None and frame.f_code.co_filename != TARGET:
            frame = frame.f_back
        if frame is None:
            return len(s)
        slot = outputs.setdefault(frame.f_lineno, ["", 0])
        slot[0] = (slot[0] + s)[-MAX_OUT_CHARS:]
        slot[1] += s.count("\n")
        return len(s)


class LiveTimeout(BaseException):
    pass


def _alarm(signum, frame):
    raise LiveTimeout()


def _start_coverage():
    mon = getattr(sys, "monitoring", None)
    if mon is not None:
        tool = mon.COVERAGE_ID
        try:
            mon.use_tool_id(tool, "croft-live-run")
        except ValueError:
            return None

        def on_line(code, line):
            if code.co_filename == TARGET:
                covered.add(line)
            return mon.DISABLE

        mon.register_callback(tool, mon.events.LINE, on_line)
        mon.set_events(tool, mon.events.LINE)
        return lambda: mon.set_events(tool, 0)

    def local(frame, event, arg):
        if event == "line":
            covered.add(frame.f_lineno)
        return local

    def global_(frame, event, arg):
        if frame.f_code.co_filename == TARGET:
            covered.add(frame.f_lineno)
            return local
        return None

    sys.settrace(global_)
    return lambda: sys.settrace(None)


def _statement_lines(tree):
    # Statements that compile to no bytecode never report a line, so they
    # would read as "never ran" however often their code did: a function's
    # docstring (a constant, not a store) and global/nonlocal declarations.
    silent = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.body:
            first = node.body[0]
            if (isinstance(first, ast.Expr) and isinstance(first.value, ast.Constant)
                    and isinstance(first.value.value, str)):
                silent.add(id(first))
    lines = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.stmt) and id(node) not in silent \
                and not isinstance(node, (ast.Global, ast.Nonlocal)):
            lines.add(node.lineno)
    return sorted(lines)


def main():
    source = sys.stdin.read()
    sys.stdin = io.StringIO("")
    report = {"lines": [], "error": None, "stmts": [], "covered": None,
              "stdout": "", "ms": 0, "timed_out": False}
    started = time.perf_counter()
    real_out, real_err = sys.stdout, sys.stderr
    stop_cov = None
    try:
        try:
            tree = ast.parse(source, TARGET)
        except SyntaxError as exc:
            report["error"] = {"line": exc.lineno or 1,
                               "msg": "SyntaxError: %s" % exc.msg}
            return report
        report["stmts"] = _statement_lines(tree)
        tree = ast.fix_missing_locations(Instrument().visit(tree))
        code = compile(tree, TARGET, "exec")
        ns = {"__name__": "__main__", "__file__": TARGET, "__builtins__": __builtins__,
              "__lv_rec__": __lv_rec__, "__lv_ret__": __lv_ret__, "__lv_expr__": __lv_expr__,
              "__lv_call__": __lv_call__}
        sys.argv = [TARGET]
        sys.path[0] = os.path.dirname(TARGET)
        sys.stdout = sys.stderr = Capture()
        if hasattr(signal, "setitimer"):
            signal.signal(signal.SIGALRM, _alarm)
            signal.setitimer(signal.ITIMER_REAL, BUDGET)
        stop_cov = _start_coverage()
        try:
            exec(code, ns)
        except BaseException as exc:
            if stop_cov:
                stop_cov()
                stop_cov = None
            if hasattr(signal, "setitimer"):
                signal.setitimer(signal.ITIMER_REAL, 0)
            line = None
            tb = exc.__traceback__
            while tb is not None:
                if tb.tb_frame.f_code.co_filename == TARGET:
                    line = tb.tb_lineno
                tb = tb.tb_next
            if isinstance(exc, LiveTimeout):
                report["timed_out"] = True
                msg = "still running after %gs" % BUDGET
            elif isinstance(exc, SystemExit):
                if exc.code in (None, 0):
                    msg = None
                else:
                    msg = "SystemExit: %s" % (exc.code,)
            else:
                text = str(exc)
                msg = type(exc).__name__ + (": " + text if text else "")
            if msg is not None:
                report["error"] = {"line": line or 1, "msg": msg}
    finally:
        if stop_cov:
            stop_cov()
        if hasattr(signal, "setitimer"):
            signal.setitimer(signal.ITIMER_REAL, 0)
        sys.stdout, sys.stderr = real_out, real_err
    report["ms"] = round((time.perf_counter() - started) * 1000, 1)
    report["covered"] = sorted(l for l in covered if l > 0)
    report["stdout"] = "".join(stdout_all)
    lines = {}
    for (line, label), (n, seen, last) in slots.items():
        lines.setdefault(line, {}).setdefault("vals", []).append(
            {"k": label, "n": n, "seen": [_render(v) for v in seen], "last": _render(last)})
    for line, (text, count) in outputs.items():
        tail = [t for t in text.split("\n") if t.strip()]
        if tail:
            entry = lines.setdefault(line, {})
            entry["out"] = tail[-1][-120:]
            entry["outn"] = count
    report["lines"] = [dict(line=k, **v) for k, v in sorted(lines.items())]
    return report


if __name__ == "__main__":
    result = main()
    with open(REPORT, "w", encoding="utf-8") as fh:
        json.dump(result, fh)
    # User threads or atexit hooks must not hold the run open.
    os._exit(0)
