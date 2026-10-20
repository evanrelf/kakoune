//! Port of register_manager.hh/cc

use crate::completion::{complete, CandidateList};
use crate::context::Context;
use crate::exception::{Error, Result};
use crate::format::Cp;
use crate::hook_manager::Hook;
use crate::unicode::to_lower;
use crate::units::Codepoint;
use crate::utils::NestedBool;
use indexmap::IndexMap;
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

pub type RestoreInfo = Vec<Vec<u8>>;

pub trait Register {
    fn set(&self, context: &Context, values: &[Vec<u8>], restoring: bool) -> Result<()>;
    /// Never empty: an empty register returns a single empty string
    fn get(&self, context: &Context) -> Result<Vec<Vec<u8>>>;
    fn get_main(&self, context: &Context, main_index: usize) -> Result<Vec<u8>>;
    fn modified_hook_disabled(&self) -> &NestedBool;

    fn save(&self, context: &Context) -> Result<RestoreInfo> {
        self.get(context)
    }
    fn restore(&self, context: &Context, info: &RestoreInfo) -> Result<()> {
        self.set(context, info, true)
    }
}

fn non_empty(content: &[Vec<u8>]) -> Vec<Vec<u8>> {
    if content.is_empty() { vec![Vec::new()] } else { content.to_vec() }
}

/// static value register, which can be modified
pub struct StaticRegister {
    name: Vec<u8>,
    content: RefCell<Vec<Vec<u8>>>,
    disable_modified_hook: NestedBool,
}

impl StaticRegister {
    pub fn new(name: &str) -> Self {
        StaticRegister { name: name.as_bytes().to_vec(), content: RefCell::new(Vec::new()), disable_modified_hook: NestedBool::default() }
    }
}

impl Register for StaticRegister {
    fn set(&self, context: &Context, values: &[Vec<u8>], _restoring: bool) -> Result<()> {
        *self.content.borrow_mut() = values.to_vec();
        if !self.disable_modified_hook.is_set() {
            context.hooks().run_hook(Hook::RegisterModified, &self.name, context)?;
        }
        Ok(())
    }
    fn get(&self, _context: &Context) -> Result<Vec<Vec<u8>>> {
        Ok(non_empty(&self.content.borrow()))
    }
    fn get_main(&self, context: &Context, main_index: usize) -> Result<Vec<u8>> {
        let content = self.get(context)?;
        Ok(content[main_index.min(content.len() - 1)].clone())
    }
    fn modified_hook_disabled(&self) -> &NestedBool {
        &self.disable_modified_hook
    }
}

pub type RegisterGetter = Box<dyn Fn(&Context) -> Result<Vec<Vec<u8>>>>;
pub type RegisterSetter = Box<dyn Fn(&Context, &[Vec<u8>]) -> Result<()>>;

/// Dynamic value register, use its getter to get its value when needed.
pub struct DynamicRegister {
    disable_modified_hook: NestedBool,
    getter: RegisterGetter,
    setter: RegisterSetter,
}

impl Register for DynamicRegister {
    fn set(&self, context: &Context, values: &[Vec<u8>], _restoring: bool) -> Result<()> {
        (self.setter)(context, values)
    }
    fn get(&self, context: &Context) -> Result<Vec<Vec<u8>>> {
        Ok(non_empty(&(self.getter)(context)?))
    }
    fn get_main(&self, context: &Context, main_index: usize) -> Result<Vec<u8>> {
        let content = self.get(context)?;
        Ok(content[main_index.min(content.len() - 1)].clone())
    }
    fn modified_hook_disabled(&self) -> &NestedBool {
        &self.disable_modified_hook
    }
}

pub fn make_dyn_reg(getter: impl Fn(&Context) -> Result<Vec<Vec<u8>>> + 'static) -> Rc<dyn Register> {
    Rc::new(DynamicRegister {
        disable_modified_hook: NestedBool::default(),
        getter: Box::new(getter),
        setter: Box::new(|_, _| Err(Error::runtime("this register is not assignable"))),
    })
}

pub fn make_dyn_reg_with_setter(
    getter: impl Fn(&Context) -> Result<Vec<Vec<u8>>> + 'static,
    setter: impl Fn(&Context, &[Vec<u8>]) -> Result<()> + 'static,
) -> Rc<dyn Register> {
    Rc::new(DynamicRegister {
        disable_modified_hook: NestedBool::default(),
        getter: Box::new(getter),
        setter: Box::new(setter),
    })
}

/// Register that is used to store some kind prompt history
pub struct HistoryRegister {
    inner: StaticRegister,
}

impl HistoryRegister {
    pub fn new(name: &str) -> Self {
        HistoryRegister { inner: StaticRegister::new(name) }
    }
}

impl Register for HistoryRegister {
    fn set(&self, context: &Context, values: &[Vec<u8>], restoring: bool) -> Result<()> {
        const SIZE_LIMIT: usize = 1000;
        if restoring {
            self.inner.set(context, values, true)?;
            self.inner.content.borrow_mut().retain(|s| !s.is_empty());
            return Ok(());
        }
        {
            let mut content = self.inner.content.borrow_mut();
            for entry in values.iter().rev() {
                content.retain(|s| s != entry);
                content.insert(0, entry.clone());
            }
            content.truncate(SIZE_LIMIT);
        }
        if !self.inner.disable_modified_hook.is_set() {
            context.hooks().run_hook(Hook::RegisterModified, &self.inner.name, context)?;
        }
        Ok(())
    }
    fn get(&self, context: &Context) -> Result<Vec<Vec<u8>>> {
        self.inner.get(context)
    }
    fn get_main(&self, _context: &Context, _main_index: usize) -> Result<Vec<u8>> {
        Ok(self.inner.content.borrow().first().cloned().unwrap_or_default())
    }
    fn modified_hook_disabled(&self) -> &NestedBool {
        &self.inner.disable_modified_hook
    }
}

pub struct NullRegister {
    disable_modified_hook: NestedBool,
}

impl Register for NullRegister {
    fn set(&self, _: &Context, _: &[Vec<u8>], _: bool) -> Result<()> {
        Ok(())
    }
    fn get(&self, _: &Context) -> Result<Vec<Vec<u8>>> {
        Ok(vec![Vec::new()])
    }
    fn get_main(&self, _: &Context, _: usize) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
    fn modified_hook_disabled(&self) -> &NestedBool {
        &self.disable_modified_hook
    }
}

pub fn null_register() -> Rc<dyn Register> {
    Rc::new(NullRegister { disable_modified_hook: NestedBool::default() })
}

pub const REG_NAMES: &[(&str, char)] = &[
    ("slash", '/'),
    ("dquote", '"'),
    ("pipe", '|'),
    ("caret", '^'),
    ("arobase", '@'),
    ("percent", '%'),
    ("dot", '.'),
    ("hash", '#'),
    ("underscore", '_'),
    ("colon", ':'),
];

pub struct RegisterManager {
    registers: RefCell<IndexMap<Codepoint, Rc<dyn Register>>>,
}

thread_local! {
    static REGISTER_MANAGER: OnceCell<&'static RegisterManager> = const { OnceCell::new() };
}

impl RegisterManager {
    pub fn create() -> &'static RegisterManager {
        let rm: &'static RegisterManager = Box::leak(Box::new(RegisterManager { registers: RefCell::new(IndexMap::new()) }));
        REGISTER_MANAGER.with(|r| {
            let _ = r.set(rm);
        });
        rm
    }

    pub fn instance() -> &'static RegisterManager {
        REGISTER_MANAGER.with(|r| *r.get().expect("RegisterManager not created"))
    }

    pub fn has_instance() -> bool {
        REGISTER_MANAGER.with(|r| r.get().is_some())
    }

    /// operator[](StringView)
    pub fn get(&self, reg: &[u8]) -> Result<Rc<dyn Register>> {
        if reg.len() == 1 {
            return self.get_cp(reg[0] as i8 as i32 as u32);
        }
        match REG_NAMES.iter().find(|(n, _)| n.as_bytes() == reg) {
            Some((_, c)) => self.get_cp(*c as u32),
            None => Err(rt_err!("no such register: '{}'", reg)),
        }
    }

    /// operator[](Codepoint)
    pub fn get_cp(&self, c: Codepoint) -> Result<Rc<dyn Register>> {
        let c = to_lower(c);
        self.registers.borrow().get(&c).cloned().ok_or_else(|| rt_err!("no such register: '{}'", Cp(c)))
    }

    pub fn add_register(&self, c: Codepoint, reg: Rc<dyn Register>) {
        let prev = self.registers.borrow_mut().insert(c, reg);
        debug_assert!(prev.is_none());
    }

    pub fn complete_register_name(&self, prefix: &[u8], cursor_pos: usize) -> CandidateList {
        complete(prefix, cursor_pos, REG_NAMES.iter().map(|(n, _)| n.as_bytes()))
    }

    pub fn registers(&self) -> Vec<(Codepoint, Rc<dyn Register>)> {
        self.registers.borrow().iter().map(|(k, v)| (*k, v.clone())).collect()
    }
}
