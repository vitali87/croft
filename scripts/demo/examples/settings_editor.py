#!/usr/bin/env python3
"""The recording in #675: Preferences: Open Settings (UI), end to end.

    cargo build && python3 scripts/demo/examples/settings_editor.py target/debug/croft out.gif
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from tui_demo import Demo  # noqa: E402


def main(binary: str, gif: str) -> None:
    """Record the scenario from `binary` into `gif`."""
    with Demo(
        binary,
        user_config={"auto_save": False, "theme": "dark"},
        workspace_config={"copy_on_select": True},
        files={"src/main.rs": 'fn main() {\n    println!("hello");\n}\n'},
    ) as d:
        record(d, gif)


def record(d: Demo, gif: str) -> None:
    """The steps: open the editor, flip a bool, set a number, use the workspace."""
    d.start()
    d.snap(1200)

    d.ctrl_shift("p")
    d.snap(500)
    d.type("open settings ui")
    d.key("Enter")
    d.snap(2200)

    # Search, then Enter flips a bool into the user layer.
    d.type("auto save", hold_ms=1400)
    d.key("Enter")
    d.snap(2200)

    # A number: Enter asks for it and writes it.
    d.key(*["BSpace"] * len("auto save"))
    d.snap(250)
    d.type("scrollback", hold_ms=1200)
    d.key("Enter")
    d.snap(1500)
    d.key("BSpace")
    d.snap(300)
    d.type("5000", hold_ms=1000)
    d.key("Enter")
    d.snap(2200)

    # Tab: write to the workspace layer instead.
    d.key(*["BSpace"] * len("scrollback"))
    d.key("Tab")
    d.snap(1800)
    d.type("format on save", hold_ms=1200)
    d.key("Enter")
    d.snap(2200)

    # A key a repository may not set is refused for the workspace.
    d.key(*["BSpace"] * len("format on save"))
    d.snap(250)
    d.type("sidebar auto hide", hold_ms=1000)
    d.key("Enter")
    d.snap(2800)

    d.key("Escape")
    d.snap(1500)
    d.save(gif)


if __name__ == "__main__":
    main(*sys.argv[1:3])
