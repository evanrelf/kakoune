//! STUB: to be replaced by the port of input_handler.hh/cc
use crate::context::{Context, ContextFlags};
use crate::exception::Result;
use crate::selection::SelectionList;
use std::rc::Rc;

pub struct InputHandler {
    context: Context,
}

impl InputHandler {
    pub fn new(selections: SelectionList, flags: ContextFlags, name: Vec<u8>) -> Rc<InputHandler> {
        Rc::new_cyclic(|w| InputHandler { context: Context::new(w.clone(), selections, flags, name) })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn reset_normal_mode(&self) -> Result<()> {
        todo!()
    }
}
