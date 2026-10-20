//! Port of user_interface.hh

use crate::display_buffer::{DisplayBuffer, DisplayLine, DisplayLineList};
use crate::face::Face;
use crate::keys::Key;
use crate::units::{ColumnCount, DisplayCoord};
use indexmap::IndexMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuStyle {
    Prompt,
    Search,
    Inline,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InfoStyle {
    Prompt,
    Inline,
    InlineAbove,
    InlineBelow,
    MenuDoc,
    Modal,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StatusStyle {
    #[default]
    Status,
    Command,
    Search,
    Prompt,
}

/// Key callbacks may fail, for example with a cancel error, which then
/// propagates out of the event loop
pub type OnKeyCallback = Box<dyn Fn(Key) -> crate::exception::Result<()>>;
pub type OnPasteCallback = Box<dyn Fn(&[u8]) -> crate::exception::Result<()>>;
pub type UiOptions = IndexMap<Vec<u8>, Vec<u8>>;

pub trait UserInterface {
    fn is_ok(&self) -> bool;
    fn menu_show(&self, choices: &[DisplayLine], anchor: DisplayCoord, fg: Face, bg: Face, style: MenuStyle);
    fn menu_select(&self, selected: i32);
    fn menu_hide(&self);
    fn info_show(&self, title: &DisplayLine, content: &DisplayLineList, anchor: DisplayCoord, face: Face, style: InfoStyle);
    fn info_hide(&self);
    fn draw(&self, display_buffer: &DisplayBuffer, cursor_pos: DisplayCoord, default_face: &Face, padding_face: &Face, widget_columns: ColumnCount);
    fn draw_status(&self, prompt: &DisplayLine, content: &DisplayLine, cursor_pos: ColumnCount, mode_line: &DisplayLine, default_face: &Face, style: StatusStyle);
    fn dimensions(&self) -> DisplayCoord;
    fn refresh(&self, force: bool);
    fn set_on_key(&self, callback: OnKeyCallback);
    fn set_on_paste(&self, callback: OnPasteCallback);
    fn set_ui_options(&self, options: &UiOptions);
}
