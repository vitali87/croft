"""Tests for scripts/demo/tui_demo.py's terminal choice and Ghostty launch."""

from __future__ import annotations

import sys
import unittest
from unittest import mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "demo"))

import tui_demo  # noqa: E402

GHOSTTY = Path("/Applications/Ghostty.app/Contents/MacOS/ghostty")


class ChooseTerminal(unittest.TestCase):
    def test_macos_with_ghostty_records_in_ghostty(self):
        self.assertEqual(tui_demo.choose_terminal({}, "darwin", GHOSTTY, "1.4.0"), "ghostty")

    def test_without_ghostty_falls_back_to_xterm_js(self):
        self.assertEqual(tui_demo.choose_terminal({}, "darwin", None), "xterm")
        self.assertEqual(tui_demo.choose_terminal({}, "linux", None), "xterm")

    def test_env_can_force_xterm_js_on_a_mac_with_ghostty(self):
        env = {"CROFT_DEMO_TERMINAL": "xterm"}
        self.assertEqual(tui_demo.choose_terminal(env, "darwin", GHOSTTY, "1.4.0"), "xterm")

    def test_unknown_terminal_is_refused(self):
        with self.assertRaisesRegex(ValueError, "CROFT_DEMO_TERMINAL"):
            tui_demo.choose_terminal({"CROFT_DEMO_TERMINAL": "kitty"}, "darwin", GHOSTTY, "1.4.0")

    def test_ghostty_asked_for_but_missing_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, "Ghostty"):
            tui_demo.choose_terminal({"CROFT_DEMO_TERMINAL": "ghostty"}, "darwin", None)

    def test_a_ghostty_older_than_1_4_falls_back_to_xterm_js(self):
        self.assertEqual(tui_demo.choose_terminal({}, "darwin", GHOSTTY, "1.3.1"), "xterm")

    def test_ghostty_asked_for_but_older_than_1_4_is_refused(self):
        env = {"CROFT_DEMO_TERMINAL": "ghostty"}
        with self.assertRaisesRegex(RuntimeError, "1.4"):
            tui_demo.choose_terminal(env, "darwin", GHOSTTY, "1.3.1")

    def test_ghostty_versions_compare_numerically(self):
        for version, ok in (("1.4.0", True), ("1.10.2", True), ("2.0", True),
                            ("1.3.9", False), ("0.9", False), ("", False), ("tip", False)):
            self.assertEqual(tui_demo.ghostty_supported(version), ok, version)

    def test_ghostty_off_macos_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, "macOS"):
            tui_demo.choose_terminal({"CROFT_DEMO_TERMINAL": "ghostty"}, "linux", GHOSTTY, "1.4.0")


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

    def test_a_tmux_key_name_with_no_ghostty_key_is_refused(self):
        with self.assertRaisesRegex(ValueError, "Insert"):
            tui_demo.ghostty_key("Insert")

    def test_a_character_with_no_key_on_the_us_layout_is_refused(self):
        with self.assertRaisesRegex(ValueError, "é"):
            tui_demo.ghostty_char("é")


class RecordedWindow(unittest.TestCase):
    WINDOWS = [("7", "croft  test", 900), ("9", "croft-demo-ab12", 1440), ("11", "zsh", 800)]

    def test_the_window_with_the_recordings_title_is_recorded(self):
        self.assertEqual(tui_demo.window_titled(self.WINDOWS, "croft-demo-ab12"), ("9", 1440))

    def test_an_unrelated_new_window_is_never_recorded(self):
        self.assertIsNone(tui_demo.window_titled(self.WINDOWS[:1] + self.WINDOWS[2:], "croft-demo-ab12"))

    def test_two_windows_with_the_recordings_title_are_refused(self):
        twins = self.WINDOWS + [("13", "croft-demo-ab12", 1440)]
        with self.assertRaisesRegex(RuntimeError, "more than one"):
            tui_demo.window_titled(twins, "croft-demo-ab12")


class ShotScale(unittest.TestCase):
    def test_a_retina_shot_is_halved_and_a_1x_shot_kept(self):
        self.assertEqual(tui_demo.shot_scale(1440, 2880), 0.5)
        self.assertEqual(tui_demo.shot_scale(1440, 1440), 1.0)

    def test_a_shot_with_no_width_is_refused(self):
        with self.assertRaises(ValueError):
            tui_demo.shot_scale(1440, 0)


class Close(unittest.TestCase):
    def test_a_ghostty_recording_never_runs_tmux(self):
        d = tui_demo.Demo("/bin/true")
        d.terminal = "ghostty"
        with mock.patch.object(tui_demo.subprocess, "run", side_effect=FileNotFoundError("tmux")) as run:
            d.close()
        run.assert_not_called()
        self.assertFalse(d.home.exists())

    def test_an_xterm_recording_stops_its_tmux_server(self):
        d = tui_demo.Demo("/bin/true")
        d.terminal = "xterm"
        with mock.patch.object(tui_demo.subprocess, "run") as run:
            d.close()
        self.assertEqual(run.call_args.args[0][:3], ["tmux", "-L", d.socket])
        self.assertFalse(d.home.exists())


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
