#!/usr/bin/env python3
"""Record croft in a terminal as a GIF for a pull request description.

Drives the real binary inside a sized tmux session with a throwaway HOME, so
nothing touches your own config, and snapshots the screen after each step.
See "Show it" in CONTRIBUTING.md; examples/settings_editor.py is a complete
scenario. Two ways to draw the frames:

* ghostty (the default on macOS when Ghostty.app is installed): croft runs
  directly in a new window of your running Ghostty, with your Ghostty
  config, so it sees a real Ghostty (fonts, Kitty images, keys). Keys go in
  through Ghostty's AppleScript `send key`, and each snapshot is a
  screenshot of that window. Needs Ghostty 1.4 or newer (older ones
  drop typed characters, so they record with xterm); macOS asks once
  for Screen Recording and Automation permission. Leave the window alone
  while it records.
* xterm (everywhere else): croft runs in a sized tmux session and the frames
  are replayed into xterm.js in headless Chromium (render.mjs) with a Nerd
  Font, so croft's icons draw. Needs tmux, node and a Chromium that
  playwright-core can launch (`npx playwright install chromium`, or point
  CROFT_DEMO_CHROMIUM at one). The first run installs render.mjs's locked
  npm packages and downloads a pinned, hash-checked font into
  ~/.cache/croft-demo.

CROFT_DEMO_TERMINAL=ghostty|xterm picks one. Both need Pillow
(`pip install pillow`).
"""

from __future__ import annotations

import hashlib
import json
import plistlib
import os
import shutil
import signal
import subprocess
import sys
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
GHOSTTY_FONT_SIZE = 18
GHOSTTY_APP = Path("/Applications/Ghostty.app/Contents/MacOS/ghostty")
GHOSTTY_PLIST = GHOSTTY_APP.parents[1] / "Info.plist"
# The first release whose `send key` delivers character keys.
GHOSTTY_MIN = (1, 4)
TERMINALS = ("ghostty", "xterm")


def ghostty_supported(version: str) -> bool:
    """Whether Ghostty `version` (e.g. "1.4.0") can drive a recording."""
    try:
        parts = tuple(int(p) for p in version.split(".")[:2])
    except ValueError:
        return False
    return parts >= GHOSTTY_MIN


def ghostty_version() -> str:
    """The installed Ghostty.app's version, or "" when it cannot be read."""
    try:
        with GHOSTTY_PLIST.open("rb") as f:
            return str(plistlib.load(f).get("CFBundleShortVersionString", ""))
    except (OSError, plistlib.InvalidFileException):
        return ""


def new_window(before: dict[str, int], after: dict[str, int]) -> tuple[str, int] | None:
    """The (CGWindowID, width in points) of the one window in `after` but not `before`."""
    new = [(w, width) for w, width in after.items() if w not in before]
    if len(new) > 1:
        raise RuntimeError("Ghostty opened more than one window; close the others and record again")
    return new[0] if new else None


def shot_scale(points: int, pixels: int) -> float:
    """The factor that brings a `pixels`-wide screenshot of a `points`-wide window to 1x."""
    if pixels <= 0:
        raise ValueError(f"screenshot is {pixels} pixels wide")
    return points / pixels


def choose_terminal(env: dict, platform: str, ghostty: Path | None, version: str = "") -> str:
    """The terminal that draws the frames: CROFT_DEMO_TERMINAL, else Ghostty 1.4+ on a Mac that has it."""
    asked = env.get("CROFT_DEMO_TERMINAL")
    if asked is not None and asked not in TERMINALS:
        raise ValueError(f"CROFT_DEMO_TERMINAL={asked!r}; expected one of {', '.join(TERMINALS)}")
    if asked == "xterm":
        return "xterm"
    if asked == "ghostty":
        if platform != "darwin":
            raise RuntimeError("CROFT_DEMO_TERMINAL=ghostty records a macOS window; use xterm here")
        if ghostty is None:
            raise RuntimeError(f"CROFT_DEMO_TERMINAL=ghostty, but Ghostty is not at {GHOSTTY_APP}")
        if not ghostty_supported(version):
            raise RuntimeError(f"CROFT_DEMO_TERMINAL=ghostty needs Ghostty 1.4 or newer; this is {version or 'unknown'}")
        return "ghostty"
    usable = platform == "darwin" and ghostty is not None and ghostty_supported(version)
    return "ghostty" if usable else "xterm"


# tmux key names (what scenarios pass to `key`) as Ghostty physical keys.
_GHOSTTY_KEYS = {
    "Enter": "enter", "Tab": "tab", "BSpace": "backspace", "Escape": "escape",
    "Space": "space", "Up": "arrowUp", "Down": "arrowDown", "Left": "arrowLeft",
    "Right": "arrowRight", "Home": "home", "End": "end", "PageUp": "pageUp",
    "PPage": "pageUp", "PageDown": "pageDown", "NPage": "pageDown", "DC": "delete",
    **{f"F{n}": f"f{n}" for n in range(1, 13)},
}
_MODIFIERS = {"C": "control", "M": "option", "S": "shift"}
# The US layout: each printable character as (key, shifted).
_US = {
    **{c: (c, False) for c in "abcdefghijklmnopqrstuvwxyz"},
    **{c.upper(): (c, True) for c in "abcdefghijklmnopqrstuvwxyz"},
    **{str(n): (f"digit{n}", False) for n in range(10)},
    **{c: (f"digit{n}", True) for n, c in zip("1234567890", "!@#$%^&*()")},
    " ": ("space", False),
    **{c: (k, False) for c, k in zip("-=[]\\;',./`", (
        "minus", "equal", "bracketLeft", "bracketRight", "backslash", "semicolon",
        "quote", "comma", "period", "slash", "backquote"))},
    **{c: (k, True) for c, k in zip('_+{}|:"<>?~', (
        "minus", "equal", "bracketLeft", "bracketRight", "backslash", "semicolon",
        "quote", "comma", "period", "slash", "backquote"))},
}


def ghostty_char(char: str) -> tuple[str, str]:
    """The Ghostty key and modifiers that type `char` on a US layout."""
    if char not in _US:
        raise ValueError(f"no US-layout key types {char!r}")
    key, shifted = _US[char]
    return key, "shift" if shifted else ""


def ghostty_key(name: str) -> tuple[str, str]:
    """The Ghostty key and modifiers for a tmux key name such as `C-p` or `Down`."""
    mods = []
    while len(name) > 2 and name[1] == "-" and name[0] in _MODIFIERS:
        mods.append(_MODIFIERS[name[0]])
        name = name[2:]
    if name in _GHOSTTY_KEYS:
        key = _GHOSTTY_KEYS[name]
    elif len(name) == 1 and name in _US:
        key, shifted = _US[name]
        if shifted:
            mods.append("shift")
    else:
        raise ValueError(f"no Ghostty key for tmux key {name!r}")
    return key, ",".join(mods)


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
        found = GHOSTTY_APP if GHOSTTY_APP.exists() else None
        version = ghostty_version() if found else ""
        self.terminal = choose_terminal(dict(os.environ), sys.platform, found, version)
        self.window = ""  # Ghostty's AppleScript id for the window
        self.window_id = ""  # its CGWindowID, for screencapture
        self.window_width = 0  # its width in points, to undo Retina scaling
        self.shots = self.home / ".shots"
        config = {"suppress_terminal_warning": True, **(user_config or {})}
        _write_json(self.home / ".config/croft/config.json", config)
        if workspace_config is not None:
            _write_json(self.project / ".croft/config.json", workspace_config)
        # croft starts bash as a login shell, which reads /etc/profile; on a
        # system whose profile sets PS1 that beats the PS1 passed in `start`.
        # croft starts $SHELL; zsh's default prompt would print the host name.
        (self.home / ".bash_profile").write_text("PS1='$ '\n")
        (self.home / ".zshrc").write_text("unset RPROMPT\nPROMPT='$ '\n")
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
            # No "update available" card in the middle of a recording.
            "CROFT_NO_UPDATE_CHECK": "1",
        }
        # The scene's HOME has no toolchain; keep using yours.
        for var, default in (("RUSTUP_HOME", ".rustup"), ("CARGO_HOME", ".cargo")):
            real = os.environ.get(var) or str(Path.home() / default)
            if Path(real).is_dir():
                env[var] = real
        if self.terminal == "ghostty":
            self._open_ghostty(env)
            time.sleep(settle)
            return
        cmd = " ".join(f"{k}={_q(v)}" for k, v in env.items())
        self._tmux(
            "-f", "/dev/null", "new-session", "-d", "-s", "demo",
            "-x", str(self.cols), "-y", str(self.rows),
            "-e", "TERM=xterm-256color", "-e", "PS1=$ ",
            f"cd {_q(str(self.project))} && env -u XDG_CONFIG_HOME {cmd} {_q(str(self.binary))} .",
        )
        self._tmux("set", "-as", "terminal-features", ",*:RGB")
        time.sleep(settle)

    def _open_ghostty(self, env: dict[str, str]) -> None:
        """Run croft in a new window of the running Ghostty and find that window."""
        # zsh reads $ZDOTDIR/.zshrc, so the scene's prompt wins over yours.
        # Your PATH, since ZDOTDIR also skips the profile that would set it,
        # and system files such as nix-darwin's /etc/zshenv reset it.
        path = os.environ.get("PATH", "")
        env = {**env, "ZDOTDIR": str(self.home), "PATH": path}
        with (self.home / ".zshrc").open("a") as rc:
            rc.write(f"export PATH={_q(path)}\n")
        run = self.home / ".run.sh"
        run.write_text(
            f"echo $$ > {_q(str(self.home / '.pid'))}\n"
            f"cd {_q(str(self.project))} && exec env -u XDG_CONFIG_HOME "
            + " ".join(f"{k}={_q(v)}" for k, v in env.items())
            + f" {_q(str(self.binary))} .\n"
        )
        before = _ghostty_window_ids()
        self.window = _osascript(
            'tell application "Ghostty"',
            "set cfg to new surface configuration",
            f"set command of cfg to {_as(f'/bin/sh {run}')}",
            f"set font size of cfg to {GHOSTTY_FONT_SIZE}",
            "return id of (new window with configuration cfg)",
            "end tell",
        )
        for _ in range(100):
            time.sleep(0.1)
            found = new_window(before, _ghostty_window_ids())
            if found:
                self.window_id, self.window_width = found
                break
        else:
            raise RuntimeError("Ghostty opened no window to record")
        self.shots.mkdir()

    def _ghostty_send(self, keys: list[tuple[str, str]]) -> None:
        """Send (key, modifiers) presses to the recording's Ghostty window in one go."""
        target = f"terminal 1 of (first window whose id is {_as(self.window)})"
        lines = ['tell application "Ghostty"']
        for key, mods in keys:
            m = f" modifiers {_as(mods)}" if mods else ""
            lines.append(f"send key {_as(key)}{m} to {target}")
        _osascript(*lines, "end tell")

    def key(self, *names: str) -> None:
        """tmux key names: Enter, Tab, BSpace, Escape, Up, C-p, M-x, F5."""
        if self.terminal == "ghostty":
            self._ghostty_send([ghostty_key(n) for n in names])
            return
        self._tmux("send-keys", "-t", "demo", *names)

    def ctrl_shift(self, letter: str) -> None:
        """Ctrl+Shift+<letter>, sent as CSI-u because tmux cannot spell it."""
        if self.terminal == "ghostty":
            self._ghostty_send([(letter.lower(), "control,shift")])
            return
        self._tmux("send-keys", "-t", "demo", "-l", f"\x1b[{ord(letter.lower())};6u")

    def type(self, text: str, per_char_ms: int = 90, hold_ms: int = 900) -> None:
        """Type `text` a character at a time, one frame per character."""
        for i, c in enumerate(text):
            self.send_text(c)
            self.snap(per_char_ms if i < len(text) - 1 else hold_ms)

    def send_text(self, text: str) -> None:
        """Type `text` in one go, without a frame."""
        if self.terminal == "ghostty":
            self._ghostty_send([ghostty_char(c) for c in text])
            return
        self._tmux("send-keys", "-t", "demo", "-l", text)

    def snap(self, hold_ms: int) -> None:
        """Capture the screen as the next frame, held for `hold_ms`."""
        time.sleep(0.35)
        if self.terminal == "ghostty":
            shot = self.shots / f"{len(self.frames):04}.png"
            subprocess.run(["screencapture", "-x", "-o", "-l", self.window_id, str(shot)], check=True)
            self.frames.append((str(shot), hold_ms))
            return
        # -N keeps each row's trailing blanks: a pane painted to its right
        # edge with no border glyph there (Croft Loch's padding) otherwise
        # loses its background and replays on the terminal default.
        text = self._tmux("capture-pane", "-p", "-e", "-N", "-t", "demo")
        self.frames.append((text, hold_ms))

    def save(self, gif: str | Path, still: str | Path | None = None, still_frame: int = -1) -> None:
        """Stop croft, render the frames and write the GIF (and PNG still)."""
        if self.terminal == "ghostty":
            self._close_ghostty()
            try:
                index = [{"file": s, "ms": ms} for s, ms in self.frames]
                from PIL import Image

                with Image.open(self.frames[0][0]) as first:
                    scale = shot_scale(self.window_width, first.width)
                _write_gif(Path("/"), index, Path(gif), scale=scale)
                if still is not None:
                    shutil.copy(self.frames[still_frame][0], still)
            finally:
                self.close()
            return
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
        """Stop croft (its Ghostty window, or tmux server) and delete the temporary HOME."""
        try:
            self._close_ghostty()
            if self.terminal == "xterm":
                subprocess.run(["tmux", "-L", self.socket, "kill-server"], capture_output=True)
        finally:
            shutil.rmtree(self.home, ignore_errors=True)

    def _close_ghostty(self) -> None:
        """Stop croft and close its Ghostty window, if this recording opened one."""
        if not self.window:
            return
        pid = self.home / ".pid"
        if pid.exists():
            try:
                os.kill(int(pid.read_text()), signal.SIGTERM)
            except (ProcessLookupError, ValueError):
                pass
            time.sleep(0.5)
        _osascript(
            'tell application "Ghostty"',
            f"close window (first window whose id is {_as(self.window)})",
            "end tell",
            check=False,
        )
        self.window = ""

    def _tmux(self, *args: str) -> str:
        """Run tmux against this recording's own server; return stdout."""
        return subprocess.run(
            ["tmux", "-L", self.socket, *args], check=True, capture_output=True, text=True
        ).stdout


def _ghostty_window_ids() -> dict[str, int]:
    """Ghostty's on-screen windows: CGWindowID to width in points."""
    script = """
ObjC.import('CoreGraphics');
function run() {
  const list = ObjC.castRefToObject(
    $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0));
  const out = [];
  for (let i = 0; i < list.count; i++) {
    const w = list.objectAtIndex(i);
    if (ObjC.unwrap(w.objectForKey('kCGWindowOwnerName')) === 'Ghostty'
        && ObjC.unwrap(w.objectForKey('kCGWindowLayer')) === 0) {
      const bounds = ObjC.deepUnwrap(w.objectForKey('kCGWindowBounds'));
      out.push(ObjC.unwrap(w.objectForKey('kCGWindowNumber')) + ':' + Math.round(bounds.Width));
    }
  }
  return out.join(' ');
}
"""
    out = subprocess.run(["osascript", "-l", "JavaScript", "-e", script], capture_output=True, text=True, check=False)
    if out.returncode != 0:
        raise RuntimeError(f"could not list Ghostty's windows: {out.stderr.strip()}")
    return {w: int(width) for w, width in (item.split(":") for item in out.stdout.split())}


def _osascript(*lines: str, check: bool = True) -> str:
    """Run AppleScript `lines`; return what it returns."""
    args = ["osascript"]
    for line in lines:
        args += ["-e", line]
    out = subprocess.run(args, capture_output=True, text=True, check=False)
    if check and out.returncode != 0:
        raise RuntimeError(f"osascript failed: {out.stderr.strip()}")
    return out.stdout.strip()


def _as(s: str) -> str:
    """`s` as an AppleScript string literal."""
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _write_gif(work: Path, index: list[dict], out: Path, scale: float = 1.0) -> None:
    """Merge identical frames and write them as one looping GIF."""
    from PIL import Image

    imgs, durations = [], []
    for f in index:
        im = Image.open(work / f["file"].replace(".ans", ".png")).convert("RGB")
        if scale != 1.0:
            # A Retina screenshot is two pixels a point; halve it so the GIF
            # stays the size of the window on screen.
            im = im.resize((round(im.width * scale), round(im.height * scale)), Image.Resampling.LANCZOS)
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
