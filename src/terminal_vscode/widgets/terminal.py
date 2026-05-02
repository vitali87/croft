from __future__ import annotations

import asyncio
import fcntl
import os
import pty
import shlex
import signal
import struct
import termios
from typing import ClassVar

import pyte
from rich.console import RenderableType
from rich.style import Style
from rich.text import Text
from textual import events
from textual.widget import Widget

from .. import constants as cs


KEY_MAP: dict[str, bytes] = {
    "enter": b"\r",
    "tab": b"\t",
    "shift+tab": b"\x1b[Z",
    "backspace": b"\x7f",
    "delete": b"\x1b[3~",
    "escape": b"\x1b",
    "up": b"\x1b[A",
    "down": b"\x1b[B",
    "right": b"\x1b[C",
    "left": b"\x1b[D",
    "home": b"\x1b[H",
    "end": b"\x1b[F",
    "pageup": b"\x1b[5~",
    "pagedown": b"\x1b[6~",
    "insert": b"\x1b[2~",
    "f1": b"\x1bOP",
    "f2": b"\x1bOQ",
    "f3": b"\x1bOR",
    "f4": b"\x1bOS",
    "f5": b"\x1b[15~",
    "f6": b"\x1b[17~",
    "f7": b"\x1b[18~",
    "f8": b"\x1b[19~",
    "f9": b"\x1b[20~",
    "f10": b"\x1b[21~",
    "f11": b"\x1b[23~",
    "f12": b"\x1b[24~",
    "ctrl+up": b"\x1b[1;5A",
    "ctrl+down": b"\x1b[1;5B",
    "ctrl+right": b"\x1b[1;5C",
    "ctrl+left": b"\x1b[1;5D",
    "alt+left": b"\x1bb",
    "alt+right": b"\x1bf",
    "space": b" ",
}


PYTE_COLOR_NAMES: dict[str, str] = {
    "black": "black",
    "red": "red",
    "green": "green",
    "brown": "yellow",
    "yellow": "yellow",
    "blue": "blue",
    "magenta": "magenta",
    "cyan": "cyan",
    "white": "white",
    "brightblack": "bright_black",
    "brightred": "bright_red",
    "brightgreen": "bright_green",
    "brightbrown": "bright_yellow",
    "brightyellow": "bright_yellow",
    "brightblue": "bright_blue",
    "brightmagenta": "bright_magenta",
    "brightcyan": "bright_cyan",
    "brightwhite": "bright_white",
}


def _color_to_rich(color: str) -> str | None:
    if color == "default":
        return None
    if len(color) == 6 and all(c in "0123456789abcdefABCDEF" for c in color):
        return f"#{color}"
    return PYTE_COLOR_NAMES.get(color, None)


def _build_style(char: pyte.screens.Char) -> Style:
    return Style(
        color=_color_to_rich(char.fg),
        bgcolor=_color_to_rich(char.bg),
        bold=char.bold,
        italic=char.italics,
        underline=char.underscore,
        strike=char.strikethrough,
        reverse=char.reverse,
        blink=char.blink,
    )


def _resolve_shell() -> str:
    return os.environ.get("SHELL") or cs.DEFAULT_SHELL_FALLBACK


class TerminalWidget(Widget, can_focus=True):
    DEFAULT_CSS = """
    TerminalWidget {
        background: black;
        color: white;
    }
    """

    BINDINGS: ClassVar[list] = []

    def __init__(self, *, cwd: str | None = None, **kwargs) -> None:
        super().__init__(**kwargs)
        self.shell = _resolve_shell()
        self.cwd = cwd or os.getcwd()
        self.master_fd: int | None = None
        self.pid: int | None = None
        self.vt: pyte.Screen = pyte.Screen(80, 24)
        self.stream: pyte.ByteStream = pyte.ByteStream(self.vt)
        self._reader_added = False
        self._closed = False

    def on_mount(self) -> None:
        self._spawn()

    def on_unmount(self) -> None:
        self._cleanup()

    def _spawn(self) -> None:
        pid, master_fd = pty.fork()
        if pid == 0:
            try:
                os.chdir(self.cwd)
            except OSError:
                pass
            env = os.environ.copy()
            env["TERM"] = "xterm-256color"
            env["COLORTERM"] = "truecolor"
            argv = shlex.split(self.shell)
            os.execvpe(argv[0], argv, env)
        self.pid = pid
        self.master_fd = master_fd
        flags = fcntl.fcntl(master_fd, fcntl.F_GETFL)
        fcntl.fcntl(master_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
        self._set_winsize(self.vt.lines, self.vt.columns)
        loop = asyncio.get_running_loop()
        loop.add_reader(master_fd, self._on_readable)
        self._reader_added = True

    def _cleanup(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._reader_added and self.master_fd is not None:
            try:
                asyncio.get_running_loop().remove_reader(self.master_fd)
            except (RuntimeError, ValueError):
                pass
        if self.pid is not None:
            try:
                os.kill(self.pid, signal.SIGHUP)
            except OSError:
                pass
            try:
                os.waitpid(self.pid, os.WNOHANG)
            except OSError:
                pass
        if self.master_fd is not None:
            try:
                os.close(self.master_fd)
            except OSError:
                pass

    def _on_readable(self) -> None:
        if self.master_fd is None:
            return
        try:
            data = os.read(self.master_fd, cs.PTY_READ_CHUNK)
        except (BlockingIOError, InterruptedError):
            return
        except OSError:
            self._cleanup()
            self.refresh()
            return
        if not data:
            self._cleanup()
            self.refresh()
            return
        self.stream.feed(data)
        self.refresh()

    def _set_winsize(self, rows: int, cols: int) -> None:
        if self.master_fd is None:
            return
        winsize = struct.pack("HHHH", rows, cols, 0, 0)
        try:
            fcntl.ioctl(self.master_fd, termios.TIOCSWINSZ, winsize)
        except OSError:
            pass

    def on_resize(self, event: events.Resize) -> None:
        cols = max(1, event.size.width)
        rows = max(1, event.size.height)
        self.vt.resize(rows, cols)
        self._set_winsize(rows, cols)
        self.refresh()

    def _send(self, data: bytes) -> None:
        if self.master_fd is None or self._closed:
            return
        try:
            os.write(self.master_fd, data)
        except OSError:
            pass

    def on_key(self, event: events.Key) -> None:
        if self._closed:
            return
        event.stop()
        event.prevent_default()
        key = event.key
        if key in KEY_MAP:
            self._send(KEY_MAP[key])
            return
        if key.startswith("ctrl+") and len(key) == 6:
            ch = key[-1].lower()
            if "a" <= ch <= "z":
                self._send(bytes([ord(ch) - ord("a") + 1]))
                return
            if ch == "@":
                self._send(b"\x00")
                return
            if ch == "\\":
                self._send(b"\x1c")
                return
            if ch == "]":
                self._send(b"\x1d")
                return
        if key.startswith("alt+") and event.character:
            self._send(b"\x1b" + event.character.encode("utf-8"))
            return
        if event.character is not None:
            self._send(event.character.encode("utf-8"))

    def render(self) -> RenderableType:
        text = Text(no_wrap=True, overflow="crop")
        cursor = self.vt.cursor
        cursor_visible = not getattr(cursor, "hidden", False) and self.has_focus
        cols = self.vt.columns
        rows = self.vt.lines
        for y in range(rows):
            row = self.vt.buffer[y]
            for x in range(cols):
                ch = row[x]
                data = ch.data if ch.data else " "
                style = _build_style(ch)
                if cursor_visible and x == cursor.x and y == cursor.y:
                    style = style + Style(reverse=True)
                text.append(data, style=style)
            if y < rows - 1:
                text.append("\n")
        return text
