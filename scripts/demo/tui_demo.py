#!/usr/bin/env python3
"""Record croft in a terminal as a GIF for a pull request description.

Drives the real binary inside a sized tmux session with a throwaway HOME, so
nothing touches your own config, and snapshots the screen after each step.
The frames are replayed into xterm.js in headless Chromium (render.mjs) with a
Nerd Font, so croft's icons draw, then written out as one GIF with an optional
PNG still. See "Show it" in CONTRIBUTING.md; examples/settings_editor.py is a
complete scenario.

Needs tmux, node, Pillow (`pip install pillow`) and a Chromium that
playwright-core can launch (`npx playwright install chromium`, or point
CROFT_DEMO_CHROMIUM at one). The first run installs render.mjs's locked npm
packages and downloads a pinned, hash-checked font into ~/.cache/croft-demo.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.request
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "croft-demo"
# Pinned, and checked before use: the frames of a published recording are
# rendered with it.
FONT_URL = "https://github.com/ryanoasis/nerd-fonts/releases/download/v3.4.0/DejaVuSansMono.tar.xz"
FONT_SHA256 = "0e58ff9c1f9378922b7f324fdba953929d88d61b36aedd80ee43964567b226cc"
FONTS = ("DejaVuSansMNerdFontMono-Regular.ttf", "DejaVuSansMNerdFontMono-Bold.ttf")


class Demo:
    """One recording: start it, script keys and snapshots, then `save`.

    Use it as a context manager, or call `close`, so croft and the temporary
    HOME go away even when the scenario raises.
    """

    def __init__(
        self,
        binary: str | Path,
        cols: int = 150,
        rows: int = 36,
        user_config: dict | None = None,
        workspace_config: dict | None = None,
        files: dict[str, str] | None = None,
    ):
        self.binary = Path(binary).resolve()
        self.cols, self.rows = cols, rows
        # A tmux server of its own: a shared session name let one recording
        # kill, or capture, another one or an unrelated session.
        self.socket = f"croft-demo-{uuid.uuid4().hex[:12]}"
        # Under /tmp and short: croft's unix sockets live under the cache dir,
        # and an AF_UNIX path is capped near 104 bytes. The path also shows
        # in status messages ("saved to ..."), so short reads better.
        self.home = Path(tempfile.mkdtemp(prefix="cd-", dir="/tmp"))
        self.project = self.home / "proj"
        self.frames: list[tuple[str, int]] = []
        config = {"suppress_terminal_warning": True, **(user_config or {})}
        _write_json(self.home / ".config/croft/config.json", config)
        if workspace_config is not None:
            _write_json(self.project / ".croft/config.json", workspace_config)
        for rel, text in (files or {"README.md": "# demo\n"}).items():
            path = self.project / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)

    def __enter__(self) -> Demo:
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def start(self, settle: float = 4.0) -> None:
        """Launch croft in the project and give it `settle` seconds to draw."""
        env = {
            "HOME": str(self.home),
            "XDG_CACHE_HOME": str(self.home / ".cache"),
            "COLORTERM": "truecolor",
        }
        cmd = " ".join(f"{k}={_q(v)}" for k, v in env.items())
        self._tmux(
            "-f", "/dev/null", "new-session", "-d", "-s", "demo",
            "-x", str(self.cols), "-y", str(self.rows),
            "-e", "TERM=xterm-256color", "-e", "PS1=$ ",
            f"cd {_q(str(self.project))} && env -u XDG_CONFIG_HOME {cmd} {_q(str(self.binary))} .",
        )
        self._tmux("set", "-as", "terminal-features", ",*:RGB")
        time.sleep(settle)

    def key(self, *names: str) -> None:
        """tmux key names: Enter, Tab, BSpace, Escape, Up, C-p, M-x, F5."""
        self._tmux("send-keys", "-t", "demo", *names)

    def ctrl_shift(self, letter: str) -> None:
        """Ctrl+Shift+<letter>, sent as CSI-u because tmux cannot spell it."""
        self._tmux("send-keys", "-t", "demo", "-l", f"\x1b[{ord(letter.lower())};6u")

    def type(self, text: str, per_char_ms: int = 90, hold_ms: int = 900) -> None:
        """Type `text` a character at a time, one frame per character."""
        for i, c in enumerate(text):
            self._tmux("send-keys", "-t", "demo", "-l", c)
            self.snap(per_char_ms if i < len(text) - 1 else hold_ms)

    def snap(self, hold_ms: int) -> None:
        """Capture the screen as the next frame, held for `hold_ms`."""
        time.sleep(0.35)
        text = self._tmux("capture-pane", "-p", "-e", "-t", "demo")
        self.frames.append((text, hold_ms))

    def save(self, gif: str | Path, still: str | Path | None = None, still_frame: int = -1) -> None:
        """Stop croft, render the frames and write the GIF (and PNG still)."""
        self.close()
        work = Path(tempfile.mkdtemp(prefix="croft-demo-frames-"))
        try:
            index = []
            for i, (text, ms) in enumerate(self.frames):
                (work / f"{i:04}.ans").write_text(text)
                index.append({"file": f"{i:04}.ans", "ms": ms})
            (work / "index.json").write_text(json.dumps(index))
            _ensure_node_deps()
            fonts = _ensure_fonts()
            subprocess.run(
                ["node", str(HERE / "render.mjs"), str(work), f"{self.cols}x{self.rows}", str(fonts)],
                check=True,
            )
            _write_gif(work, index, Path(gif))
            if still is not None:
                shutil.copy(work / index[still_frame]["file"].replace(".ans", ".png"), still)
        finally:
            shutil.rmtree(work, ignore_errors=True)

    def close(self) -> None:
        """Stop this recording's tmux server and delete the temporary HOME."""
        subprocess.run(["tmux", "-L", self.socket, "kill-server"], capture_output=True)
        shutil.rmtree(self.home, ignore_errors=True)

    def _tmux(self, *args: str) -> str:
        """Run tmux against this recording's own server; return stdout."""
        return subprocess.run(
            ["tmux", "-L", self.socket, *args], check=True, capture_output=True, text=True
        ).stdout


def _write_gif(work: Path, index: list[dict], out: Path) -> None:
    """Merge identical frames and write them as one looping GIF."""
    from PIL import Image

    imgs, durations = [], []
    for f in index:
        im = Image.open(work / f["file"].replace(".ans", ".png")).convert("RGB")
        if imgs and im.tobytes() == imgs[-1].tobytes():
            durations[-1] += f["ms"]
            continue
        imgs.append(im)
        durations.append(f["ms"])
    # One palette for every frame, so colours do not flicker between them,
    # built from ALL the frames: a first frame's palette cannot hold a colour
    # that only appears later, like a modal's accent. NEAREST keeps each
    # sampled pixel an exact colour from the recording.
    w, h = imgs[0].size
    sample = Image.new("RGB", (w // 2, (h // 2) * len(imgs)))
    for i, im in enumerate(imgs):
        sample.paste(im.resize((w // 2, h // 2), Image.Resampling.NEAREST), (0, (h // 2) * i))
    palette = sample.quantize(colors=256, method=Image.Quantize.MEDIANCUT)
    frames = [im.quantize(palette=palette, dither=Image.Dither.NONE) for im in imgs]
    tmp = out.with_name(out.name + ".part")
    frames[0].save(
        tmp, format="GIF", save_all=True, append_images=frames[1:], duration=durations,
        loop=0, optimize=True, disposal=1,
    )
    tmp.replace(out)
    print(f"{out}: {len(frames)} frames, {sum(durations) / 1000:.1f}s, {out.stat().st_size // 1024} KiB")


def _ensure_node_deps() -> None:
    """Install render.mjs's packages from the lockfile on first use."""
    if not (HERE / "node_modules/@xterm/xterm").exists():
        subprocess.run(["npm", "ci", "--silent"], cwd=HERE, check=True)


def _ensure_fonts() -> Path:
    """Download the pinned Nerd Font once, refusing an archive that does not match its hash."""
    fonts = CACHE / "fonts"
    if not all((fonts / f).exists() for f in FONTS):
        fonts.mkdir(parents=True, exist_ok=True)
        archive = CACHE / "nerd-font.tar.xz"
        urllib.request.urlretrieve(FONT_URL, archive)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != FONT_SHA256:
            archive.unlink()
            raise RuntimeError(f"{FONT_URL} has sha256 {digest}, expected {FONT_SHA256}")
        with tarfile.open(archive) as tar:
            for f in FONTS:
                tar.extract(f, fonts, filter="data")
        archive.unlink()
    return fonts


def _write_json(path: Path, value: dict) -> None:
    """Write `value` as indented JSON, creating parent directories."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def _q(s: str) -> str:
    """Single-quote `s` for the shell tmux runs the command in."""
    return "'" + s.replace("'", "'\\''") + "'"
