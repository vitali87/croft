//! The colours the pickers draw with on a boxed theme: Quick Open, the
//! Command Palette, both symbol pickers, the process, directory, branch and
//! list pickers, and the command history pop-up. Each is croft's dark
//! vocabulary and still passes through `Theme::ui` where it is used, so a
//! theme moves it as before. A theme's own selection colour is not here: it
//! is drawn as the theme gives it, and only [`SELECTION`] stands in when the
//! theme gives none.

use ratatui::style::Color;

/// The picker's border.
pub const BORDER: Color = Color::Rgb(0x4e, 0x9a, 0xff);

/// The picker's body, and the character under the query's caret.
pub const BODY: Color = Color::Rgb(0x16, 0x18, 0x1f);

/// The query, the rows, and the block caret.
pub const TEXT: Color = Color::Rgb(0xec, 0xef, 0xf4);

/// The `>` before the query, and the branch picker's Create row.
pub const PROMPT: Color = Color::Rgb(0x88, 0xc0, 0xd0);

/// An empty list's message, and the branch picker's `(remote)` mark.
pub const DIM: Color = Color::Rgb(0x7a, 0x82, 0x90);

/// A row's second column: a command's shortcut, a file's folder, a
/// symbol's detail.
pub const DETAIL: Color = Color::Rgb(0x8e, 0x95, 0xa4);

/// The second column on the selected row, and Go to Symbol's line prompt.
pub const DETAIL_BRIGHT: Color = Color::Rgb(0xa0, 0xb4, 0xd8);

/// The selected row's fill when the theme gives no selection colour.
pub const SELECTION: Color = Color::Rgb(0x1e, 0x3a, 0x6e);
