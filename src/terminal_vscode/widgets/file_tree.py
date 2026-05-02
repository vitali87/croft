from __future__ import annotations

from pathlib import Path

from rich.style import Style
from rich.text import Text
from textual.widgets import DirectoryTree
from textual.widgets._directory_tree import DirEntry
from textual.widgets._tree import TreeNode

from .. import icons as ic


class FileTree(DirectoryTree):
    ICON_NODE = "▸ "
    ICON_NODE_EXPANDED = "▾ "

    def render_label(
        self,
        node: TreeNode[DirEntry],
        base_style: Style,
        style: Style,
    ) -> Text:
        node_label = node._label.copy()
        node_label.stylize(style)

        if node.data is None:
            return node_label

        path = Path(node.data.path)
        is_dir = path.is_dir()

        if is_dir:
            chev = self.ICON_NODE_EXPANDED if node.is_expanded else self.ICON_NODE
            icon = ic.FOLDER_OPEN if node.is_expanded else ic.FOLDER_CLOSED
            label = Text.assemble(
                Text(chev, style=Style(color="#888888")),
                Text(icon.glyph + " ", style=Style(color=icon.color)),
                node_label,
            )
        else:
            icon = ic.for_path_name(path.name, path.suffix)
            label = Text.assemble(
                Text("  ", style=base_style),
                Text(icon.glyph + " ", style=Style(color=icon.color)),
                node_label,
            )
        return label
