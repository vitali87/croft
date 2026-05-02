from __future__ import annotations

from pathlib import Path
from typing import Annotated

import typer
from loguru import logger

from .app import TerminalVSCode

app = typer.Typer(add_completion=False, help=__doc__ or "")


@app.command()
def main(
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
    logger.remove()
    TerminalVSCode(root=path).run()


def run() -> None:
    app()


if __name__ == "__main__":
    run()
