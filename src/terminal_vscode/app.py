from __future__ import annotations

from pathlib import Path
from typing import ClassVar

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.widgets import DirectoryTree, Footer, Header, Static

from . import constants as cs
from .widgets.editor import EditorWidget
from .widgets.file_tree import FileTree
from .widgets.terminal import TerminalWidget


class TerminalVSCode(App[None]):
    CSS_PATH = "styles.tcss"
    TITLE = cs.APP_TITLE

    BINDINGS: ClassVar[list[Binding]] = [
        Binding("ctrl+s", "save", "Save", priority=True),
        Binding("ctrl+q", "quit", "Quit", priority=True),
        Binding("f6", "cycle_focus", "Focus next pane", priority=True),
        Binding("ctrl+b", "toggle_tree", "Toggle file tree", priority=True),
    ]

    def __init__(self, root: Path) -> None:
        super().__init__()
        self.root = root.resolve()
        self.sub_title = str(self.root)

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield FileTree(str(self.root), id=cs.ID_FILE_TREE)
        with Vertical(id=cs.ID_RIGHT):
            yield Static(cs.PANE_TITLE_EDITOR_EMPTY, id=cs.ID_TAB_LABEL)
            yield EditorWidget(id=cs.ID_EDITOR)
            yield Static(cs.PANE_TITLE_TERMINAL, classes=cs.CLASS_PANE_TITLE)
            yield TerminalWidget(cwd=str(self.root), id=cs.ID_TERMINAL)
        yield Static(cs.STATUS_READY, id=cs.ID_STATUS_BAR)
        yield Footer()

    def on_mount(self) -> None:
        tree = self.query_one(f"#{cs.ID_FILE_TREE}", DirectoryTree)
        tree.focus()

    def on_directory_tree_file_selected(
        self, event: DirectoryTree.FileSelected
    ) -> None:
        editor = self.query_one(f"#{cs.ID_EDITOR}", EditorWidget)
        path = Path(event.path)
        ok, message = editor.open_file(path)
        self._set_status(message)
        label = self.query_one(f"#{cs.ID_TAB_LABEL}", Static)
        if ok:
            label.update(str(path.relative_to(self.root)) if path.is_relative_to(self.root) else str(path))
            editor.focus()
        else:
            label.update(cs.PANE_TITLE_EDITOR_EMPTY)

    def action_save(self) -> None:
        editor = self.query_one(f"#{cs.ID_EDITOR}", EditorWidget)
        if editor.current_path is None:
            self._set_status(cs.STATUS_ERROR.format(error="No file loaded"))
            return
        ok, message = editor.save_file()
        self._set_status(message)
        if ok:
            label = self.query_one(f"#{cs.ID_TAB_LABEL}", Static)
            path = editor.current_path
            label.update(str(path.relative_to(self.root)) if path.is_relative_to(self.root) else str(path))

    def action_cycle_focus(self) -> None:
        order = [cs.ID_FILE_TREE, cs.ID_EDITOR, cs.ID_TERMINAL]
        focused = self.focused
        focused_id = getattr(focused, "id", None)
        if focused_id in order:
            idx = (order.index(focused_id) + 1) % len(order)
        else:
            idx = 0
        self.query_one(f"#{order[idx]}").focus()

    def action_toggle_tree(self) -> None:
        tree = self.query_one(f"#{cs.ID_FILE_TREE}", DirectoryTree)
        tree.display = not tree.display

    def _set_status(self, message: str) -> None:
        self.query_one(f"#{cs.ID_STATUS_BAR}", Static).update(message)
