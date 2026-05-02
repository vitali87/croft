from __future__ import annotations

from typing import NamedTuple


class Icon(NamedTuple):
    glyph: str
    color: str


FOLDER_CLOSED = Icon("\u25b8", "#dcb67a")
FOLDER_OPEN = Icon("\u25be", "#dcb67a")
DEFAULT_FILE = Icon("\u25cf", "#888888")


NAME_ICONS: dict[str, Icon] = {
    ".gitignore": Icon("\u25cf", "#e8274b"),
    ".gitattributes": Icon("\u25cf", "#e8274b"),
    ".gitmodules": Icon("\u25cf", "#e8274b"),
    ".python-version": Icon("\u25cf", "#3572a5"),
    "pyproject.toml": Icon("\u25cf", "#3572a5"),
    "uv.lock": Icon("\u25cf", "#519aba"),
    "poetry.lock": Icon("\u25cf", "#519aba"),
    "package.json": Icon("\u25cf", "#cbcb41"),
    "package-lock.json": Icon("\u25cf", "#519aba"),
    "tsconfig.json": Icon("\u25cf", "#519aba"),
    "dockerfile": Icon("\u25cf", "#384d54"),
    "makefile": Icon("\u25cf", "#cc3e44"),
    "readme.md": Icon("\u25cf", "#519aba"),
    "license": Icon("\u25cf", "#cccccc"),
    ".env": Icon("\u25cf", "#faf743"),
    ".dockerignore": Icon("\u25cf", "#384d54"),
}


EXT_ICONS: dict[str, Icon] = {
    ".py": Icon("\u25cf", "#3572a5"),
    ".pyi": Icon("\u25cf", "#3572a5"),
    ".pyc": Icon("\u25cf", "#3572a5"),
    ".ipynb": Icon("\u25cf", "#f37726"),
    ".js": Icon("\u25cf", "#cbcb41"),
    ".mjs": Icon("\u25cf", "#cbcb41"),
    ".cjs": Icon("\u25cf", "#cbcb41"),
    ".jsx": Icon("\u25cf", "#519aba"),
    ".ts": Icon("\u25cf", "#519aba"),
    ".tsx": Icon("\u25cf", "#519aba"),
    ".html": Icon("\u25cf", "#e44d26"),
    ".htm": Icon("\u25cf", "#e44d26"),
    ".css": Icon("\u25cf", "#42a5f5"),
    ".scss": Icon("\u25cf", "#cc6699"),
    ".sass": Icon("\u25cf", "#cc6699"),
    ".json": Icon("\u25cf", "#cbcb41"),
    ".jsonc": Icon("\u25cf", "#cbcb41"),
    ".md": Icon("\u25cf", "#519aba"),
    ".markdown": Icon("\u25cf", "#519aba"),
    ".yaml": Icon("\u25cf", "#cb171e"),
    ".yml": Icon("\u25cf", "#cb171e"),
    ".toml": Icon("\u25cf", "#9c4221"),
    ".sql": Icon("\u25cf", "#dad8d8"),
    ".sh": Icon("\u25cf", "#4d5a5e"),
    ".bash": Icon("\u25cf", "#4d5a5e"),
    ".zsh": Icon("\u25cf", "#4d5a5e"),
    ".fish": Icon("\u25cf", "#4d5a5e"),
    ".go": Icon("\u25cf", "#519aba"),
    ".rs": Icon("\u25cf", "#dea584"),
    ".java": Icon("\u25cf", "#cc3e44"),
    ".kt": Icon("\u25cf", "#7f52ff"),
    ".kts": Icon("\u25cf", "#7f52ff"),
    ".c": Icon("\u25cf", "#599eff"),
    ".h": Icon("\u25cf", "#a074c4"),
    ".cpp": Icon("\u25cf", "#519aba"),
    ".hpp": Icon("\u25cf", "#a074c4"),
    ".cs": Icon("\u25cf", "#596706"),
    ".rb": Icon("\u25cf", "#cc342d"),
    ".php": Icon("\u25cf", "#a074c4"),
    ".swift": Icon("\u25cf", "#e37933"),
    ".lua": Icon("\u25cf", "#000080"),
    ".vim": Icon("\u25cf", "#019833"),
    ".xml": Icon("\u25cf", "#e37933"),
    ".svg": Icon("\u25cf", "#ffb13b"),
    ".png": Icon("\u25cf", "#a074c4"),
    ".jpg": Icon("\u25cf", "#a074c4"),
    ".jpeg": Icon("\u25cf", "#a074c4"),
    ".gif": Icon("\u25cf", "#a074c4"),
    ".webp": Icon("\u25cf", "#a074c4"),
    ".ico": Icon("\u25cf", "#a074c4"),
    ".pdf": Icon("\u25cf", "#b30b00"),
    ".zip": Icon("\u25cf", "#cca700"),
    ".tar": Icon("\u25cf", "#cca700"),
    ".gz": Icon("\u25cf", "#cca700"),
    ".lock": Icon("\u25cf", "#519aba"),
    ".log": Icon("\u25cf", "#dad8d8"),
    ".txt": Icon("\u25cf", "#cccccc"),
    ".csv": Icon("\u25cf", "#7cb342"),
    ".tsv": Icon("\u25cf", "#7cb342"),
    ".env": Icon("\u25cf", "#faf743"),
    ".tcss": Icon("\u25cf", "#42a5f5"),
}


def for_path_name(name: str, suffix: str) -> Icon:
    lower = name.lower()
    if lower in NAME_ICONS:
        return NAME_ICONS[lower]
    return EXT_ICONS.get(suffix.lower(), DEFAULT_FILE)
