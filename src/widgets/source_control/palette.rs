//! The panel's colours and glyphs, and the width a row's action icons take.

pub(super) const INPUT_PROMPT_RGB: (u8, u8, u8) = (0x6c, 0x7d, 0x9c);

pub(super) const BUTTON_BG_RGB: (u8, u8, u8) = (0x09, 0x67, 0xb8);

pub(super) const BUTTON_FG_RGB: (u8, u8, u8) = (0xff, 0xff, 0xff);

pub(super) const SECTION_HEADER_RGB: (u8, u8, u8) = (0xcc, 0xcc, 0xcc);

pub(super) const ADDITIONS_RGB: (u8, u8, u8) = (0xa3, 0xbe, 0x8c);

pub(super) const DELETIONS_RGB: (u8, u8, u8) = (0xe7, 0x70, 0x70);

pub const DISCARD_GLYPH: char = '\u{21b6}';

pub const STAGE_GLYPH: char = '+';

/// The minus sign (U+2212), used for the staged-row "unstage" affordance.
/// Mirrors the plain `+` STAGE_GLYPH rather than a codicon so the two
/// inline actions read as a matched pair.
pub const UNSTAGE_GLYPH: char = '\u{2212}';

pub(super) const ROW_ACTIONS_WIDTH: u16 = 4;
