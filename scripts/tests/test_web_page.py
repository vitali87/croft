"""croft web's page (#342): the terminal model's tests, and icon coverage.

The model's tests are JavaScript (web_term_test.js) and run under node when
node is on PATH, as it is on CI's runners. The icon check needs fontTools,
the same tool that builds the font, and is skipped without it.
"""

import shutil
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

import web_icon_font  # noqa: E402


class TerminalModel(unittest.TestCase):
    @unittest.skipUnless(shutil.which("node"), "node is not installed")
    def test_the_terminal_model_passes_its_tests(self):
        r = subprocess.run(
            ["node", str(ROOT / "scripts" / "tests" / "web_term_test.js")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


class IconFont(unittest.TestCase):
    # Code points croft uses that the Nerd Font it subsets does not carry.
    NOT_IN_NERD_FONTS = {0xF841}

    def test_the_icon_font_covers_every_icon_croft_draws(self):
        try:
            from fontTools.ttLib import TTFont
        except ImportError:
            self.skipTest("fontTools is not installed")
        font = TTFont(ROOT / "src" / "web" / "page" / "icons.woff2")
        have = set(font.getBestCmap())
        missing = web_icon_font.used_codepoints() - have - self.NOT_IN_NERD_FONTS
        self.assertFalse(
            missing,
            "rerun scripts/web_icon_font.py; the font lacks "
            + ", ".join(f"U+{cp:04X}" for cp in sorted(missing)),
        )

    def test_used_codepoints_finds_escapes_and_literals(self):
        tmp = Path(self.id().replace(".", "_"))
        src = ROOT / "target" / "web_icon_font_test" / tmp
        src.mkdir(parents=True, exist_ok=True)
        self.addCleanup(shutil.rmtree, src.parent, ignore_errors=True)
        (src / "a.rs").write_text("let a = '\\u{ea83}'; let b = '\ue5ff'; let c = '\\u{41}';\n")
        self.assertEqual(web_icon_font.used_codepoints(src), {0xEA83, 0xE5FF})


if __name__ == "__main__":
    unittest.main()
