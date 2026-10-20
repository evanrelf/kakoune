//! STUB: to be replaced by the port of insert_completer.hh/cc

use crate::exception::Result;
use crate::input_handler::InputHandler;
use crate::keys::Key;
use std::rc::{Rc, Weak};

pub struct InsertCompleter {
    input_handler: Weak<InputHandler>,
}

impl InsertCompleter {
    pub fn new(input_handler: &Rc<InputHandler>) -> Self {
        InsertCompleter { input_handler: Rc::downgrade(input_handler) }
    }
    pub fn select(&self, _index: i32, _relative: bool, _record_key: &mut dyn FnMut(Key)) -> Result<()> {
        Ok(())
    }
    pub fn update(&self, _allow_implicit: bool) -> Result<()> {
        Ok(())
    }
    pub fn try_accept(&self) -> Result<()> {
        Ok(())
    }
    pub fn reset(&self) {}
    pub fn explicit_file_complete(&self) -> Result<()> {
        Ok(())
    }
    pub fn explicit_word_buffer_complete(&self) -> Result<()> {
        Ok(())
    }
    pub fn explicit_word_all_complete(&self) -> Result<()> {
        Ok(())
    }
    pub fn explicit_line_buffer_complete(&self) -> Result<()> {
        Ok(())
    }
    pub fn explicit_line_all_complete(&self) -> Result<()> {
        Ok(())
    }
    pub fn has_candidate_selected(&self) -> bool {
        false
    }
}
