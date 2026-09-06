"""Tests for scripts/check_arch_content.py: ARCHITECTURE.md's reasoning gate.

The tree in that file carries each module's design rationale, and a rewrite
can cut a line's reasoning away while leaving the row and its identifiers
exactly where they were. A row-level or identifier-level check reads that as
a pass, which is the failure this script exists to catch and which it was
caught by twice while being written: first with a threshold set above the
real losses, then with a parse that collapsed repeated basenames.

So the cases below are the two blind spots, not a coverage sweep. The
same-basename case is the one worth having: `registry.rs` appears four times
in this tree and `mod.rs` three, so a dict keyed on the filename keeps only
the last of each and reports the wrong path - or nothing at all.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import check_arch_content as gate  # noqa: E402


def doc(tree_rows, sections=()):
    """An ARCHITECTURE.md-shaped document: prose, the layout fence, sections."""
    body = ["# croft architecture", "", "Some preamble.", "", "## Project layout", "", "```"]
    body += tree_rows
    body += ["```", ""]
    for name in sections:
        body += [f"### {name}", "", "Why it is this way.", ""]
    return "\n".join(body)


class TreeParse(unittest.TestCase):
    def test_keys_on_path_so_repeated_basenames_stay_distinct(self):
        parsed = gate.tree_descriptions(
            doc(
                [
                    "src/",
                    "├── output.rs            the in-process output bus",
                    "└── widgets/",
                    "    └── output.rs        the panel group's OUTPUT tab",
                ]
            )
        )
        self.assertEqual(
            sorted(parsed),
            ["src/output.rs", "src/widgets/output.rs"],
        )
        self.assertEqual(parsed["src/output.rs"], "the in-process output bus")

    def test_anchors_on_the_heading_not_a_line_number(self):
        """A fenced example above the tree must not become the tree."""
        text = doc(["src/", "└── a.rs                 does a thing"])
        text = text.replace(
            "Some preamble.",
            "Some preamble:\n\n```bash\ncroft --version\n```\n",
        )
        parsed = gate.tree_descriptions(text)
        self.assertEqual(list(parsed), ["src/a.rs"])


class Gutting(unittest.TestCase):
    """Whether a shrunken description is reported depends on having a home."""

    LONG = "x" * 400
    ROW = "src/", "└── a.rs                 "

    def gutted(self, sections=()):
        before = doc([self.ROW[0], self.ROW[1] + self.LONG])
        after = doc([self.ROW[0], self.ROW[1] + "short"], sections)
        return gate.compare(before, after)

    def test_gutted_row_with_no_section_is_reported(self):
        shrunk, dropped = self.gutted()
        self.assertEqual([n for n, _, _ in shrunk], ["src/a.rs"])
        self.assertEqual(dropped, [])

    def test_gutted_row_with_a_section_is_accepted(self):
        self.assertEqual(self.gutted(sections=["a.rs"])[0], [])

    def test_a_section_named_by_full_path_also_counts(self):
        self.assertEqual(self.gutted(sections=["src/a.rs"])[0], [])

    def test_only_the_gutted_one_of_two_same_named_rows_is_reported(self):
        rows = [
            "src/",
            "├── registry.rs          " + self.LONG,
            "└── dap/",
            "    └── registry.rs      " + self.LONG,
        ]
        after = list(rows)
        after[3] = "    └── registry.rs      short"
        shrunk, _ = gate.compare(doc(rows), doc(after))
        self.assertEqual([n for n, _, _ in shrunk], ["src/dap/registry.rs"])

    def test_a_dropped_row_is_reported(self):
        before = doc(["src/", "├── a.rs                 does a thing", "└── b.rs   and b"])
        after = doc(["src/", "└── b.rs   and b"])
        self.assertEqual(gate.compare(before, after)[1], ["src/a.rs"])


class Threshold(unittest.TestCase):
    """The bound is a measured value; these pin which side of it a shrink lands."""

    def shrink_by(self, n):
        keep = 500 - n
        before = doc(["src/", "└── a.rs                 " + "x" * 500])
        after = doc(["src/", "└── a.rs                 " + "x" * keep])
        return gate.compare(before, after)[0]

    def test_just_over_the_threshold_is_reported(self):
        self.assertEqual(len(self.shrink_by(gate.SHRINK_THRESHOLD + 1)), 1)

    def test_just_under_the_threshold_is_not(self):
        self.assertEqual(self.shrink_by(gate.SHRINK_THRESHOLD - 1), [])


if __name__ == "__main__":
    unittest.main()
