"""Tests for scripts/demo/tui_demo.py's terminal choice and Ghostty launch."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "demo"))

import tui_demo  # noqa: E402

GHOSTTY = Path("/Applications/Ghostty.app/Contents/MacOS/ghostty")


class ChooseTerminal(unittest.TestCase):
    def test_macos_with_ghostty_records_in_ghostty(self):
        self.assertEqual(tui_demo.choose_terminal({}, "darwin", GHOSTTY), "ghostty")

    def test_without_ghostty_falls_back_to_xterm_js(self):
        self.assertEqual(tui_demo.choose_terminal({}, "darwin", None), "xterm")
        self.assertEqual(tui_demo.choose_terminal({}, "linux", None), "xterm")

    def test_env_can_force_xterm_js_on_a_mac_with_ghostty(self):
        env = {"CROFT_DEMO_TERMINAL": "xterm"}
        self.assertEqual(tui_demo.choose_terminal(env, "darwin", GHOSTTY), "xterm")

    def test_unknown_terminal_is_refused(self):
        with self.assertRaisesRegex(ValueError, "CROFT_DEMO_TERMINAL"):
            tui_demo.choose_terminal({"CROFT_DEMO_TERMINAL": "kitty"}, "darwin", GHOSTTY)

    def test_ghostty_asked_for_but_missing_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, "Ghostty"):
            tui_demo.choose_terminal({"CROFT_DEMO_TERMINAL": "ghostty"}, "darwin", None)

    def test_ghostty_off_macos_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, "macOS"):
            tui_demo.choose_terminal({"CROFT_DEMO_TERMINAL": "ghostty"}, "linux", GHOSTTY)


class GhosttyKeys(unittest.TestCase):
    def test_tmux_key_names_become_ghostty_physical_keys(self):
        cases = {
            "Enter": ("enter", ""),
            "BSpace": ("backspace", ""),
            "Escape": ("escape", ""),
            "Down": ("arrowDown", ""),
            "PageUp": ("pageUp", ""),
            "F5": ("f5", ""),
            "C-p": ("p", "control"),
            "C-End": ("end", "control"),
            "M-x": ("x", "option"),
            "V": ("v", "shift"),
        }
        for name, want in cases.items():
            self.assertEqual(tui_demo.ghostty_key(name), want, name)

    def test_typed_characters_use_the_us_layout(self):
        cases = {
            "a": ("a", ""),
            "T": ("t", "shift"),
            "1": ("digit1", ""),
            "(": ("digit9", "shift"),
            ":": ("semicolon", "shift"),
            ".": ("period", ""),
            ">": ("period", "shift"),
            " ": ("space", ""),
            "[": ("bracketLeft", ""),
        }
        for char, want in cases.items():
            self.assertEqual(tui_demo.ghostty_char(char), want, repr(char))

    def test_a_key_tmux_names_but_this_map_does_not_is_refused(self):
        with self.assertRaisesRegex(ValueError, "Insert"):
            tui_demo.ghostty_key("Insert")

    def test_a_character_with_no_key_on_the_us_layout_is_refused(self):
        with self.assertRaisesRegex(ValueError, "é"):
            tui_demo.ghostty_char("é")


class Prompt(unittest.TestCase):
    def test_every_shell_croft_may_start_prints_a_bare_prompt(self):
        d = tui_demo.Demo("/bin/true")
        try:
            for rc in (".bash_profile", ".zshrc"):
                self.assertIn("'$ '", (d.home / rc).read_text(), rc)
        finally:
            d.close()


if __name__ == "__main__":
    unittest.main()
