from __future__ import annotations

from enum import StrEnum


class Lang(StrEnum):
    PYTHON = "python"
    JAVASCRIPT = "javascript"
    TYPESCRIPT = "typescript"
    HTML = "html"
    CSS = "css"
    JSON = "json"
    MARKDOWN = "markdown"
    YAML = "yaml"
    TOML = "toml"
    SQL = "sql"
    BASH = "bash"
    GO = "go"
    RUST = "rust"
    JAVA = "java"
    REGEX = "regex"
    XML = "xml"


EXT_TO_LANG: dict[str, Lang] = {
    ".py": Lang.PYTHON,
    ".pyi": Lang.PYTHON,
    ".js": Lang.JAVASCRIPT,
    ".jsx": Lang.JAVASCRIPT,
    ".mjs": Lang.JAVASCRIPT,
    ".cjs": Lang.JAVASCRIPT,
    ".ts": Lang.TYPESCRIPT,
    ".tsx": Lang.TYPESCRIPT,
    ".html": Lang.HTML,
    ".htm": Lang.HTML,
    ".css": Lang.CSS,
    ".scss": Lang.CSS,
    ".json": Lang.JSON,
    ".md": Lang.MARKDOWN,
    ".markdown": Lang.MARKDOWN,
    ".yaml": Lang.YAML,
    ".yml": Lang.YAML,
    ".toml": Lang.TOML,
    ".sql": Lang.SQL,
    ".sh": Lang.BASH,
    ".bash": Lang.BASH,
    ".zsh": Lang.BASH,
    ".go": Lang.GO,
    ".rs": Lang.RUST,
    ".java": Lang.JAVA,
    ".xml": Lang.XML,
}


APP_TITLE = "Terminal VS Code"
APP_SUBTITLE = "{path}"

ID_FILE_TREE = "file-tree"
ID_EDITOR = "editor"
ID_TERMINAL = "terminal"
ID_RIGHT = "right"
ID_HEADER_PATH = "header-path"
ID_STATUS_BAR = "status-bar"
ID_TAB_LABEL = "tab-label"

CLASS_PANE = "pane"
CLASS_PANE_TITLE = "pane-title"

PANE_TITLE_FILES = "EXPLORER"
PANE_TITLE_EDITOR_EMPTY = "EDITOR"
PANE_TITLE_TERMINAL = "TERMINAL"

EDITOR_PLACEHOLDER = "Select a file from the tree on the left to start editing."

MAX_FILE_BYTES = 5 * 1024 * 1024

DEFAULT_SHELL_FALLBACK = "/bin/bash"

PTY_READ_CHUNK = 4096
PTY_POLL_INTERVAL = 0.02

STATUS_READY = "Ready"
STATUS_LOADED = "Loaded {path}"
STATUS_SAVED = "Saved {path}"
STATUS_TOO_LARGE = "File too large to open ({size} bytes)"
STATUS_BINARY = "Binary file not displayed"
STATUS_ERROR = "Error: {error}"
STATUS_DIRTY = "● {path}"
STATUS_CLEAN = "  {path}"
