#!/usr/bin/env python3
"""Regenerate croft web's icon font (#342): src/web/page/icons.woff2.

    pip install fonttools brotli
    curl -LO https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.4.0/patched-fonts/Meslo/S/Regular/MesloLGSNerdFontMono-Regular.ttf
    python3 scripts/web_icon_font.py MesloLGSNerdFontMono-Regular.ttf

Explorer icons, the activity bar fallbacks and status glyphs are Private Use
Area code points from the Nerd Fonts patch. A browser viewer has no Nerd Font
installed, so the page carries a subset holding exactly the PUA code points
croft's source uses: every `\\u{...}` escape and literal character in
src/**/*.rs in U+E000..U+F8FF or the supplementary PUA planes. Rerun after
adding an icon; scripts/tests/test_web_page.py fails while the font lacks
one the source uses.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src" / "web" / "page" / "icons.woff2"


def is_pua(cp: int) -> bool:
    return 0xE000 <= cp <= 0xF8FF or cp >= 0xF0000


def used_codepoints(src: Path = ROOT / "src") -> set[int]:
    cps: set[int] = set()
    for path in src.rglob("*.rs"):
        text = path.read_text(errors="ignore")
        for m in re.finditer(r"\\u\{([0-9A-Fa-f]{4,6})\}", text):
            cp = int(m.group(1), 16)
            if is_pua(cp):
                cps.add(cp)
        cps.update(ord(ch) for ch in text if is_pua(ord(ch)))
    return cps


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    from fontTools import subset
    from fontTools.ttLib import TTFont

    font = TTFont(argv[1])
    have = font.getBestCmap()
    wanted = used_codepoints()
    missing = sorted(cp for cp in wanted if cp not in have)
    options = subset.Options()
    options.flavor = "woff2"
    options.layout_features = []
    options.name_IDs = []
    options.notdef_outline = False
    options.hinting = False
    sub = subset.Subsetter(options)
    sub.populate(unicodes=sorted(wanted - set(missing)))
    sub.subset(font)
    font["name"].setName("croft-icons", 1, 3, 1, 0x409)
    font.flavor = "woff2"
    font.save(OUT)
    print(f"{OUT.relative_to(ROOT)}: {len(wanted) - len(missing)} glyphs, {OUT.stat().st_size} bytes")
    if missing:
        print("not in this font: " + ", ".join(f"U+{cp:04X}" for cp in missing))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
