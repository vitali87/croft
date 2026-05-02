from __future__ import annotations

from typing import NamedTuple


class Icon(NamedTuple):
    glyph: str
    color: str


FOLDER_CLOSED = Icon("\uea83", "#dcb67a")
FOLDER_OPEN = Icon("\ueaf7", "#dcb67a")
DEFAULT_FILE = Icon("\ueae5", "#cccccc")


NAME_ICONS: dict[str, Icon] = {
    ".gitignore": Icon("\uefce", "#e8274b"),
    ".gitattributes": Icon("\uefce", "#e8274b"),
    ".gitmodules": Icon("\uefce", "#e8274b"),
    ".python-version": Icon("\ue235", "#3572a5"),
    "pyproject.toml": Icon("\ue235", "#3572a5"),
    "uv.lock": Icon("\uea75", "#519aba"),
    "poetry.lock": Icon("\uea75", "#519aba"),
    "package.json": Icon("\ued0d", "#cbcb41"),
    "package-lock.json": Icon("\uea75", "#519aba"),
    "tsconfig.json": Icon("\ued0d", "#519aba"),
    "dockerfile": Icon("\uebc1", "#384d54"),
    "makefile": Icon("\ueb2c", "#cc3e44"),
    "readme.md": Icon("\uf48a", "#519aba"),
    "license": Icon("\ueb12", "#cccccc"),
    ".env": Icon("\uea71", "#faf743"),
    ".dockerignore": Icon("\uebc1", "#384d54"),
}


EXT_ICONS: dict[str, Icon] = {
    ".py": Icon("\ue235", "#3572a5"),
    ".pyi": Icon("\ue235", "#3572a5"),
    ".pyc": Icon("\ue235", "#3572a5"),
    ".ipynb": Icon("\ue235", "#f37726"),
    ".js": Icon("\ue74e", "#cbcb41"),
    ".mjs": Icon("\ue74e", "#cbcb41"),
    ".cjs": Icon("\ue74e", "#cbcb41"),
    ".jsx": Icon("\ue7ba", "#519aba"),
    ".ts": Icon("\ue628", "#519aba"),
    ".tsx": Icon("\ue7ba", "#519aba"),
    ".html": Icon("\ue736", "#e44d26"),
    ".htm": Icon("\ue736", "#e44d26"),
    ".css": Icon("\ue749", "#42a5f5"),
    ".scss": Icon("\ue603", "#cc6699"),
    ".sass": Icon("\ue603", "#cc6699"),
    ".json": Icon("\ued0d", "#cbcb41"),
    ".jsonc": Icon("\ued0d", "#cbcb41"),
    ".md": Icon("\uf48a", "#519aba"),
    ".markdown": Icon("\uf48a", "#519aba"),
    ".yaml": Icon("\ue6a8", "#cb171e"),
    ".yml": Icon("\ue6a8", "#cb171e"),
    ".toml": Icon("\ue6b2", "#9c4221"),
    ".sql": Icon("\uebc1", "#dad8d8"),
    ".sh": Icon("\uebca", "#4d5a5e"),
    ".bash": Icon("\uebca", "#4d5a5e"),
    ".zsh": Icon("\uebca", "#4d5a5e"),
    ".fish": Icon("\uebca", "#4d5a5e"),
    ".go": Icon("\ue627", "#519aba"),
    ".rs": Icon("\ue7a8", "#dea584"),
    ".java": Icon("\ue738", "#cc3e44"),
    ".kt": Icon("\ue634", "#7f52ff"),
    ".kts": Icon("\ue634", "#7f52ff"),
    ".c": Icon("\ue61e", "#599eff"),
    ".h": Icon("\ue61e", "#a074c4"),
    ".cpp": Icon("\ue61d", "#519aba"),
    ".hpp": Icon("\ue61d", "#a074c4"),
    ".cs": Icon("\ue648", "#596706"),
    ".rb": Icon("\ue739", "#cc342d"),
    ".php": Icon("\ue73d", "#a074c4"),
    ".swift": Icon("\ue755", "#e37933"),
    ".lua": Icon("\ue620", "#000080"),
    ".vim": Icon("\ue7c5", "#019833"),
    ".xml": Icon("\ueabe", "#e37933"),
    ".svg": Icon("\ueabe", "#ffb13b"),
    ".png": Icon("\ueb1c", "#a074c4"),
    ".jpg": Icon("\ueb1c", "#a074c4"),
    ".jpeg": Icon("\ueb1c", "#a074c4"),
    ".gif": Icon("\ueb1c", "#a074c4"),
    ".webp": Icon("\ueb1c", "#a074c4"),
    ".ico": Icon("\ueb1c", "#a074c4"),
    ".pdf": Icon("\ueaeb", "#b30b00"),
    ".zip": Icon("\ueaf1", "#cca700"),
    ".tar": Icon("\ueaf1", "#cca700"),
    ".gz": Icon("\ueaf1", "#cca700"),
    ".lock": Icon("\uea75", "#519aba"),
    ".log": Icon("\ueb1f", "#dad8d8"),
    ".txt": Icon("\uea7d", "#cccccc"),
    ".csv": Icon("\ueb6e", "#7cb342"),
    ".tsv": Icon("\ueb6e", "#7cb342"),
    ".env": Icon("\uea71", "#faf743"),
    ".tcss": Icon("\ue749", "#42a5f5"),
}


def for_path_name(name: str, suffix: str) -> Icon:
    lower = name.lower()
    if lower in NAME_ICONS:
        return NAME_ICONS[lower]
    return EXT_ICONS.get(suffix.lower(), DEFAULT_FILE)
