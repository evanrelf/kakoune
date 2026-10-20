//! STUB: partial port of normal.hh/cc, to be completed

use crate::buffer::Buffer;
use crate::context::Context;
use crate::exception::Result;
use crate::keys::Key;
use crate::units::BufferCoord;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct NormalParams {
    pub count: i32,
    pub reg: u8,
}

#[derive(Clone, Copy)]
pub struct NormalCmd {
    pub docstring: &'static str,
    pub func: fn(&Context, NormalParams) -> Result<()>,
}

pub fn get_normal_command(_key: Key) -> Option<NormalCmd> {
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PasteMode {
    Append,
    Insert,
    Replace,
}

pub fn paste_pos(buffer: &Buffer, min: BufferCoord, max: BufferCoord, mode: PasteMode, linewise: bool) -> BufferCoord {
    match mode {
        PasteMode::Append => {
            if buffer.is_end(max) {
                return max;
            }
            if linewise { BufferCoord::line(buffer.line_count().min(max.line + 1)) } else { buffer.char_next(max) }
        }
        PasteMode::Insert => {
            if linewise { BufferCoord::line(min.line) } else { min }
        }
        PasteMode::Replace => BufferCoord::default(),
    }
}
