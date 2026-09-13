use std::io;

use warp_terminal::event::ExitReason;
use warp_terminal::local_tty::event_loop::ActiveTerminal;
use warp_terminal::model::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, LineClearMode, Mode,
    ProcessorInput, StandardCharset, TabulationClearMode,
};
use warp_terminal::model::index::VisibleRow;
use warp_terminal::model::selection::ScrollDelta;
use warp_terminal::model::{KeyboardModes, KeyboardModesApplyBehavior};
use warpui_core::color::ColorU;

use super::peer::append_terminal;

#[derive(Default)]
pub struct Capture {
    pub bytes: Vec<u8>,
    pub exited: bool,
}

impl ActiveTerminal for Capture {
    fn exit(&mut self, _: ExitReason) {
        self.exited = true;
    }
}

impl Handler for Capture {
    fn on_finish_byte_processing(&mut self, input: &ProcessorInput<'_>) {
        append_terminal(&mut self.bytes, input.bytes());
    }

    fn input(&mut self, _: char) {}
    fn set_title(&mut self, _: Option<String>) {}
    fn set_cursor_style(&mut self, _: Option<CursorStyle>) {}
    fn set_cursor_shape(&mut self, _: CursorShape) {}
    fn goto(&mut self, _: VisibleRow, _: usize) {}
    fn goto_line(&mut self, _: VisibleRow) {}
    fn goto_col(&mut self, _: usize) {}
    fn insert_blank(&mut self, _: usize) {}
    fn move_up(&mut self, _: usize) {}
    fn move_down(&mut self, _: usize) {}
    fn identify_terminal<W: io::Write>(&mut self, _: &mut W, _: Option<char>) {}
    fn report_xtversion<W: io::Write>(&mut self, _: &mut W) {}
    fn device_status<W: io::Write>(&mut self, _: &mut W, _: usize) {}
    fn move_forward(&mut self, _: usize) {}
    fn move_backward(&mut self, _: usize) {}
    fn move_down_and_cr(&mut self, _: usize) {}
    fn move_up_and_cr(&mut self, _: usize) {}
    fn put_tab(&mut self, _: u16) {}
    fn backspace(&mut self) {}
    fn carriage_return(&mut self) {}
    fn linefeed(&mut self) -> ScrollDelta {
        ScrollDelta::zero()
    }
    fn bell(&mut self) {}
    fn substitute(&mut self) {}
    fn newline(&mut self) {}
    fn set_horizontal_tabstop(&mut self) {}
    fn scroll_up(&mut self, _: usize) -> ScrollDelta {
        ScrollDelta::zero()
    }
    fn scroll_down(&mut self, _: usize) -> ScrollDelta {
        ScrollDelta::zero()
    }
    fn insert_blank_lines(&mut self, _: usize) -> ScrollDelta {
        ScrollDelta::zero()
    }
    fn delete_lines(&mut self, _: usize) -> ScrollDelta {
        ScrollDelta::zero()
    }
    fn erase_chars(&mut self, _: usize) {}
    fn delete_chars(&mut self, _: usize) {}
    fn move_backward_tabs(&mut self, _: u16) {}
    fn move_forward_tabs(&mut self, _: u16) {}
    fn save_cursor_position(&mut self) {}
    fn restore_cursor_position(&mut self) {}
    fn clear_line(&mut self, _: LineClearMode) {}
    fn clear_screen(&mut self, _: ClearMode) {}
    fn clear_tabs(&mut self, _: TabulationClearMode) {}
    fn reset_state(&mut self) {}
    fn reverse_index(&mut self) -> ScrollDelta {
        ScrollDelta::zero()
    }
    fn terminal_attribute(&mut self, _: Attr) {}
    fn set_mode(&mut self, _: Mode) {}
    fn unset_mode(&mut self, _: Mode) {}
    fn set_keyboard_enhancement_flags(&mut self, _: KeyboardModes, _: KeyboardModesApplyBehavior) {}
    fn push_keyboard_enhancement_flags(&mut self, _: KeyboardModes) {}
    fn pop_keyboard_enhancement_flags(&mut self, _: u16) {}
    fn query_keyboard_enhancement_flags<W: io::Write>(&mut self, _: &mut W) {}
    fn set_scrolling_region(&mut self, _: usize, _: Option<usize>) {}
    fn set_keypad_application_mode(&mut self) {}
    fn unset_keypad_application_mode(&mut self) {}
    fn set_active_charset(&mut self, _: CharsetIndex) {}
    fn configure_charset(&mut self, _: CharsetIndex, _: StandardCharset) {}
    fn set_color(&mut self, _: usize, _: ColorU) {}
    fn dynamic_color_sequence<W: io::Write>(&mut self, _: &mut W, _: u8, _: usize, _: &str) {}
    fn reset_color(&mut self, _: usize) {}
    fn clipboard_store(&mut self, _: u8, _: &[u8]) {}
    fn clipboard_load(&mut self, _: u8, _: &str) {}
    fn decaln(&mut self) {}
    fn push_title(&mut self) {}
    fn pop_title(&mut self) {}
    fn text_area_size_pixels<W: io::Write>(&mut self, _: &mut W) {}
    fn text_area_size_chars<W: io::Write>(&mut self, _: &mut W) {}
}
