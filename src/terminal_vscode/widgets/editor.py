from __future__ import annotations

from pathlib import Path

from textual.widgets import TextArea

from .. import constants as cs


def detect_language(path: Path) -> str | None:
    suffix = path.suffix.lower()
    lang = cs.EXT_TO_LANG.get(suffix)
    return lang.value if lang else None


def is_probably_binary(data: bytes) -> bool:
    if not data:
        return False
    if b"\x00" in data[:4096]:
        return True
    text_chars = bytes(range(32, 127)) + b"\n\r\t\b\f"
    sample = data[:4096]
    nontext = sum(1 for b in sample if b not in text_chars)
    return nontext / len(sample) > 0.30


class EditorWidget(TextArea):
    DEFAULT_CSS = """
    EditorWidget {
        background: $surface;
    }
    """

    def __init__(self, **kwargs) -> None:
        super().__init__(
            "",
            language=None,
            theme="vscode_dark",
            show_line_numbers=True,
            **kwargs,
        )
        self.current_path: Path | None = None
        self._original_text: str = ""

    @property
    def is_dirty(self) -> bool:
        return self.text != self._original_text

    def open_file(self, path: Path) -> tuple[bool, str]:
        try:
            size = path.stat().st_size
        except OSError as exc:
            return False, cs.STATUS_ERROR.format(error=exc)
        if size > cs.MAX_FILE_BYTES:
            return False, cs.STATUS_TOO_LARGE.format(size=size)
        try:
            raw = path.read_bytes()
        except OSError as exc:
            return False, cs.STATUS_ERROR.format(error=exc)
        if is_probably_binary(raw):
            return False, cs.STATUS_BINARY
        text = raw.decode("utf-8", errors="replace")
        lang = detect_language(path)
        self.load_text(text)
        try:
            self.language = lang
        except Exception:
            self.language = None
        self.current_path = path
        self._original_text = text
        return True, cs.STATUS_LOADED.format(path=path)

    def save_file(self) -> tuple[bool, str]:
        if self.current_path is None:
            return False, cs.STATUS_ERROR.format(error="No file loaded")
        try:
            self.current_path.write_text(self.text, encoding="utf-8")
        except OSError as exc:
            return False, cs.STATUS_ERROR.format(error=exc)
        self._original_text = self.text
        return True, cs.STATUS_SAVED.format(path=self.current_path)
