//! STUB: to be replaced by the port of command_manager.hh/cc
use crate::context::Context;
use crate::debug::EnvVarMap;
use crate::exception::Result;
use std::cell::OnceCell;

#[derive(Clone, Default)]
pub struct ShellContext {
    pub params: Vec<Vec<u8>>,
    pub env_vars: EnvVarMap,
}

pub struct CommandManager {}

thread_local! {
    static COMMAND_MANAGER: OnceCell<&'static CommandManager> = const { OnceCell::new() };
}

impl CommandManager {
    pub fn instance() -> &'static CommandManager {
        COMMAND_MANAGER.with(|c| *c.get_or_init(|| Box::leak(Box::new(CommandManager {}))))
    }
    pub fn execute(&self, _command_line: &[u8], _context: &Context, _shell_context: &ShellContext) -> Result<()> {
        todo!()
    }
    pub fn loaded_modules(&self) -> Vec<Vec<u8>> {
        todo!()
    }
}
