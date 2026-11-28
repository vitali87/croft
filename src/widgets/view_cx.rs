//! What a side-bar or panel view draws with this frame (#2062, card "Non-UI
//! code out of `widgets/`", step 2): the theme, whether the view has the
//! keyboard, and the cell under the mouse pointer. The app builds one when it
//! draws and hands it to the view's `draw`, so no view keeps a copy that can
//! go stale between the frames that set it.

use crate::theme::Theme;

/// The drawing inputs every side-bar and panel view shares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewCx {
    /// The active theme.
    pub theme: Theme,
    /// Whether the view has the keyboard.
    pub focused: bool,
    /// The cell under the mouse pointer, when it is over croft.
    pub pointer: Option<(u16, u16)>,
}
