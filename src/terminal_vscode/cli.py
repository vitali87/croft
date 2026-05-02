from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path
from typing import Annotated

import typer
from loguru import logger

from .app import TerminalVSCode


SETUP_FONT_NAME = "MesloLGSNerdFontMono-Regular"
SETUP_FONT_SIZE = 13

OSASCRIPT_SET_FONT = (
    'tell application "Terminal"\n'
    '    set the font name of the default settings to "{name}"\n'
    '    set the font size of the default settings to {size}\n'
    '    set the font name of the startup settings to "{name}"\n'
    '    set the font size of the startup settings to {size}\n'
    'end tell'
)

SUBCOMMANDS = {"setup-terminal"}


def launch_app(path: Path) -> None:
    logger.remove()
    TerminalVSCode(root=path.resolve()).run()


def cmd_open(
    path: Annotated[
        Path,
        typer.Argument(
            help="Workspace folder to open",
            exists=True,
            file_okay=False,
            dir_okay=True,
            resolve_path=True,
        ),
    ] = Path.cwd(),
) -> None:
    """Open the three-pane workspace at PATH (default: current directory)."""
    launch_app(path)


def cmd_setup_terminal(
    font: Annotated[
        str, typer.Option(help="Nerd Font PostScript name to apply")
    ] = SETUP_FONT_NAME,
    size: Annotated[int, typer.Option(help="Font size in points")] = SETUP_FONT_SIZE,
    yes: Annotated[
        bool,
        typer.Option("--yes", "-y", help="Skip the confirmation prompt"),
    ] = False,
) -> None:
    """Set macOS Terminal.app's default profile font to a Nerd Font so file icons render.

    Only modifies the *default* profile (the one used by new windows). Existing
    custom profiles are not touched.
    """
    if not shutil.which("osascript"):
        typer.secho(
            "osascript not available; this command is macOS-only.",
            fg=typer.colors.RED,
        )
        raise typer.Exit(code=1)
    typer.echo(
        f"This will set Terminal.app's default profile font to '{font}' at {size}pt."
    )
    typer.echo("Existing custom profiles are not modified.")
    if not yes:
        confirm = typer.confirm("Apply this change?", default=False)
        if not confirm:
            typer.echo("Aborted.")
            raise typer.Exit(code=1)
    script = OSASCRIPT_SET_FONT.format(name=font, size=size)
    result = subprocess.run(
        ["osascript", "-e", script], capture_output=True, text=True
    )
    if result.returncode != 0:
        typer.secho(f"AppleScript failed: {result.stderr.strip()}", fg=typer.colors.RED)
        raise typer.Exit(code=1)
    typer.secho(
        f"Set Terminal.app default profile font to {font} at {size}pt.",
        fg=typer.colors.GREEN,
    )
    typer.echo(
        "Quit Terminal.app entirely (cmd+Q) and reopen it for the change to take effect."
    )


def run() -> None:
    argv = sys.argv[1:]
    if argv and argv[0] in SUBCOMMANDS:
        sub_app = typer.Typer(add_completion=False)
        if argv[0] == "setup-terminal":
            sub_app.command()(cmd_setup_terminal)
        sub_app(argv[1:])
        return
    open_app = typer.Typer(add_completion=False)
    open_app.command()(cmd_open)
    open_app(argv)


if __name__ == "__main__":
    run()
