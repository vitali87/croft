//! What the pickers share under the mouse. Quick Open, the Command Palette,
//! both symbol pickers and the process, directory, branch and list pickers
//! each show a list of rows, and in all eight the wheel moves the selection,
//! a press outside closes the picker and a left press on a row takes it, as
//! VS Code's quick pick does. [`answer_mouse`] is that rule; each picker
//! says only how it closes and what taking a row does.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

/// A picker's list as the mouse sees it.
pub trait PickerRows {
    /// The picker's outer rectangle, as last drawn.
    fn last_rect(&self) -> Rect;
    /// Move the selection one row down, stopping at the last row.
    fn select_next(&mut self);
    /// Move the selection one row up, stopping at the first row.
    fn select_prev(&mut self);
    /// The row drawn at screen row `y`, if a row is drawn there.
    fn row_index_at(&self, y: u16) -> Option<usize>;
    /// Make row `idx` the selected one.
    fn select(&mut self, idx: usize);
}

/// What a mouse event asks of a picker, once [`answer_mouse`] has moved its
/// selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMouse {
    /// Nothing more: the wheel moved the selection, or the event asks for
    /// nothing.
    Stay,
    /// A left or right press outside the picker: close it.
    Close,
    /// A left press on a row, which is now the selected one: take it.
    Accept,
}

/// The rows one notch of the wheel moves the selection.
const WHEEL_ROWS: usize = 3;

/// Answer `m` over an open `picker`: the wheel moves the selection three
/// rows, a left or right press outside the picker asks to close it, and a
/// left press on a row selects that row and asks to take it.
pub fn answer_mouse(picker: &mut impl PickerRows, m: MouseEvent) -> PickerMouse {
    let inside = picker.last_rect().contains(Position::new(m.column, m.row));
    match m.kind {
        MouseEventKind::ScrollDown => {
            for _ in 0..WHEEL_ROWS {
                picker.select_next();
            }
            PickerMouse::Stay
        }
        MouseEventKind::ScrollUp => {
            for _ in 0..WHEEL_ROWS {
                picker.select_prev();
            }
            PickerMouse::Stay
        }
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
            if !inside =>
        {
            PickerMouse::Close
        }
        MouseEventKind::Down(MouseButton::Left) => match picker.row_index_at(m.row) {
            Some(idx) => {
                picker.select(idx);
                PickerMouse::Accept
            }
            None => PickerMouse::Stay,
        },
        _ => PickerMouse::Stay,
    }
}

/// [`PickerRows`] for each picker, through its own list methods and its
/// `last_rect` and `selected` fields.
macro_rules! picker_rows {
    ($($picker:ty),* $(,)?) => {$(
        impl PickerRows for $picker {
            fn last_rect(&self) -> Rect {
                self.last_rect
            }
            fn select_next(&mut self) {
                <$picker>::select_next(self)
            }
            fn select_prev(&mut self) {
                <$picker>::select_prev(self)
            }
            fn row_index_at(&self, y: u16) -> Option<usize> {
                <$picker>::row_index_at(self, y)
            }
            fn select(&mut self, idx: usize) {
                self.selected = idx;
            }
        }
    )*};
}

picker_rows!(
    crate::widgets::file_finder::FileFinder,
    crate::widgets::command_palette::CommandPalette,
    crate::widgets::symbol_picker::SymbolPicker,
    crate::widgets::workspace_symbols::WorkspaceSymbolPicker,
    crate::widgets::process_picker::ProcessPicker,
    crate::widgets::zoxide_jump::ZoxideJump,
    crate::widgets::branch_picker::BranchPicker,
    crate::widgets::list_picker::ListPicker,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    /// Ten rows drawn from screen row 3 down, inside a box at rows 0..20.
    struct Rows {
        selected: usize,
    }

    const COUNT: usize = 10;
    const LIST_TOP: u16 = 3;

    impl PickerRows for Rows {
        fn last_rect(&self) -> Rect {
            Rect::new(10, 0, 40, 20)
        }
        fn select_next(&mut self) {
            self.selected = (self.selected + 1).min(COUNT - 1);
        }
        fn select_prev(&mut self) {
            self.selected = self.selected.saturating_sub(1);
        }
        fn row_index_at(&self, y: u16) -> Option<usize> {
            let idx = usize::from(y.checked_sub(LIST_TOP)?);
            (idx < COUNT).then_some(idx)
        }
        fn select(&mut self, idx: usize) {
            self.selected = idx;
        }
    }

    fn event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn the_wheel_moves_the_selection_three_rows_and_stops_at_the_ends() {
        let mut rows = Rows { selected: 0 };
        let down = event(MouseEventKind::ScrollDown, 20, 5);
        assert_eq!(answer_mouse(&mut rows, down), PickerMouse::Stay);
        assert_eq!(rows.selected, 3);
        for _ in 0..5 {
            answer_mouse(&mut rows, down);
        }
        assert_eq!(rows.selected, COUNT - 1, "the last row stops the wheel");
        let up = event(MouseEventKind::ScrollUp, 20, 5);
        assert_eq!(answer_mouse(&mut rows, up), PickerMouse::Stay);
        assert_eq!(rows.selected, COUNT - 4);
        for _ in 0..5 {
            answer_mouse(&mut rows, up);
        }
        assert_eq!(rows.selected, 0, "the first row stops the wheel");
    }

    #[test]
    fn the_wheel_moves_the_selection_wherever_the_pointer_is() {
        let mut rows = Rows { selected: 0 };
        let outside = event(MouseEventKind::ScrollDown, 0, 30);
        assert_eq!(answer_mouse(&mut rows, outside), PickerMouse::Stay);
        assert_eq!(rows.selected, 3);
    }

    #[test]
    fn a_left_or_right_press_outside_closes_the_picker() {
        for button in [MouseButton::Left, MouseButton::Right] {
            for (column, row) in [(9, 5), (50, 5), (20, 20), (0, 0)] {
                let mut rows = Rows { selected: 2 };
                let press = event(MouseEventKind::Down(button), column, row);
                assert_eq!(
                    answer_mouse(&mut rows, press),
                    PickerMouse::Close,
                    "{button:?} press at {column},{row}"
                );
                assert_eq!(rows.selected, 2, "closing moves no selection");
            }
        }
    }

    #[test]
    fn a_left_press_on_a_row_selects_and_takes_it() {
        let mut rows = Rows { selected: 0 };
        let press = event(MouseEventKind::Down(MouseButton::Left), 20, LIST_TOP + 4);
        assert_eq!(answer_mouse(&mut rows, press), PickerMouse::Accept);
        assert_eq!(rows.selected, 4);
    }

    #[test]
    fn a_press_inside_but_on_no_row_asks_for_nothing() {
        // The query field, above the rows; then below the last row.
        for row in [1, LIST_TOP + COUNT as u16] {
            let mut rows = Rows { selected: 2 };
            let press = event(MouseEventKind::Down(MouseButton::Left), 20, row);
            assert_eq!(answer_mouse(&mut rows, press), PickerMouse::Stay);
            assert_eq!(rows.selected, 2);
        }
    }

    #[test]
    fn a_right_press_inside_and_every_other_event_ask_for_nothing() {
        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            let mut rows = Rows { selected: 2 };
            let press = event(kind, 20, LIST_TOP + 4);
            assert_eq!(
                answer_mouse(&mut rows, press),
                PickerMouse::Stay,
                "{kind:?}"
            );
            assert_eq!(rows.selected, 2, "{kind:?}");
        }
    }
}
