//! STUB: to be replaced by the port of client.hh/cc
use crate::buffer::Buffer;
use crate::context::Context;
use crate::display_buffer::DisplayLine;
use crate::exception::Result;
use crate::units::ColumnCount;
use crate::user_interface::StatusStyle;
use std::rc::Rc;

pub struct Client {}

impl Client {
    pub fn context(&self) -> &Context {
        todo!()
    }
    pub fn info_hide(&self, _even_modal: bool) {
        todo!()
    }
    pub fn menu_hide(&self) {
        todo!()
    }
    pub fn change_buffer(&self, _buffer: &Rc<Buffer>, _set_selections: Option<&dyn Fn()>) -> Result<()> {
        todo!()
    }
    pub fn print_status(&self, _prompt: DisplayLine, _content: DisplayLine, _cursor_pos: ColumnCount, _style: StatusStyle) {
        todo!()
    }
}
