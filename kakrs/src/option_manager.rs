//! Port of option_manager.hh/cc

use crate::completion::{complete, CandidateList};
use crate::context::Context;
use crate::exception::{Error, Result};
use crate::option_types::OptionType;
use crate::string_utils::Quoting;
use crate::unicode::{is_basic_alpha, is_basic_digit};
use indexmap::IndexMap;
use std::any::Any;
use std::cell::{Ref, RefCell, RefMut};
use std::rc::{Rc, Weak};

crate::kak_flags! {
    pub struct OptionFlags: u32 {
        const None = 0;
        const Hidden = 1;
    }
}

pub struct OptionDesc {
    pub name: Vec<u8>,
    pub docstring: Vec<u8>,
    pub flags: OptionFlags,
}

/// Type erased option value (C++ TypedOption<T> virtual interface)
pub trait OptionValue: Any {
    fn get_as_string(&self, quoting: Quoting) -> Vec<u8>;
    fn get_as_strings(&self) -> Vec<Vec<u8>>;
    fn get_desc_string(&self) -> Vec<u8>;
    fn parse_strings(&self, strs: &[Vec<u8>]) -> Result<Box<dyn OptionValue>>;
    fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool>;
    fn remove_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool>;
    fn update(&mut self, context: &Context) -> Result<()>;
    fn same_value(&self, other: &dyn OptionValue) -> bool;
    fn clone_box(&self) -> Box<dyn OptionValue>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn type_name(&self) -> Vec<u8>;
}

impl<T: OptionType> OptionValue for T {
    fn get_as_string(&self, quoting: Quoting) -> Vec<u8> {
        self.to_string_q(quoting)
    }
    fn get_as_strings(&self) -> Vec<Vec<u8>> {
        self.to_strings()
    }
    fn get_desc_string(&self) -> Vec<u8> {
        self.desc_string()
    }
    fn parse_strings(&self, strs: &[Vec<u8>]) -> Result<Box<dyn OptionValue>> {
        Ok(Box::new(T::from_strings(strs)?))
    }
    fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        OptionType::add_from_strings(self, strs)
    }
    fn remove_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        OptionType::remove_from_strings(self, strs)
    }
    fn update(&mut self, context: &Context) -> Result<()> {
        OptionType::update(self, context)
    }
    fn same_value(&self, other: &dyn OptionValue) -> bool {
        other.as_any().downcast_ref::<T>().is_some_and(|o| o == self)
    }
    fn clone_box(&self) -> Box<dyn OptionValue> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn type_name(&self) -> Vec<u8> {
        T::type_name()
    }
}

pub type Validator = fn(&dyn Any) -> Result<()>;

/// Port of the C++ Option class
pub struct Opt {
    manager: Weak<OptionManager>,
    desc: Rc<OptionDesc>,
    value: RefCell<Box<dyn OptionValue>>,
    validator: Option<Validator>,
}

impl Opt {
    pub fn name(&self) -> &[u8] {
        &self.desc.name
    }
    pub fn docstring(&self) -> &[u8] {
        &self.desc.docstring
    }
    pub fn flags(&self) -> OptionFlags {
        self.desc.flags
    }
    pub fn manager(&self) -> Option<Rc<OptionManager>> {
        self.manager.upgrade()
    }
    fn manager_ptr(&self) -> *const OptionManager {
        self.manager.as_ptr()
    }

    fn type_error<T: OptionType>(&self) -> Error {
        rt_err!("option '{}' is not of type '{}'", self.name(), T::type_name())
    }

    pub fn is_of_type<T: OptionType>(&self) -> bool {
        self.value.borrow().as_any().is::<T>()
    }

    pub fn try_get<T: OptionType>(&self) -> Result<Ref<'_, T>> {
        let v = self.value.borrow();
        if !v.as_any().is::<T>() {
            return Err(self.type_error::<T>());
        }
        Ok(Ref::map(v, |v| v.as_any().downcast_ref::<T>().unwrap()))
    }

    /// get with a known type, panics if the type is wrong
    pub fn get<T: OptionType>(&self) -> Ref<'_, T> {
        match self.try_get::<T>() {
            Ok(v) => v,
            Err(e) => panic!("{}", e),
        }
    }

    pub fn try_get_mutable<T: OptionType>(&self) -> Result<RefMut<'_, T>> {
        let v = self.value.borrow_mut();
        if !v.as_any().is::<T>() {
            return Err(self.type_error::<T>());
        }
        Ok(RefMut::map(v, |v| v.as_any_mut().downcast_mut::<T>().unwrap()))
    }

    pub fn set<T: OptionType>(&self, val: T, notify: bool) -> Result<()> {
        if !self.is_of_type::<T>() {
            return Err(self.type_error::<T>());
        }
        if let Some(validator) = self.validator {
            validator(&val)?;
        }
        let changed = {
            let mut v = self.value.borrow_mut();
            let cur = v.as_any_mut().downcast_mut::<T>().unwrap();
            if *cur != val {
                *cur = val;
                true
            } else {
                false
            }
        };
        if changed && notify {
            self.notify()?;
        }
        Ok(())
    }

    fn notify(&self) -> Result<()> {
        if let Some(manager) = self.manager.upgrade() {
            manager.on_option_changed(self)?;
        }
        Ok(())
    }

    pub fn get_as_string(&self, quoting: Quoting) -> Vec<u8> {
        self.value.borrow().get_as_string(quoting)
    }
    pub fn get_as_strings(&self) -> Vec<Vec<u8>> {
        self.value.borrow().get_as_strings()
    }
    pub fn get_desc_string(&self) -> Vec<u8> {
        self.value.borrow().get_desc_string()
    }
    pub fn type_name(&self) -> Vec<u8> {
        self.value.borrow().type_name()
    }

    pub fn set_from_strings(&self, strs: &[Vec<u8>]) -> Result<()> {
        let new_value = self.value.borrow().parse_strings(strs)?;
        if let Some(validator) = self.validator {
            validator(new_value.as_any())?;
        }
        let changed = {
            let mut v = self.value.borrow_mut();
            if !v.same_value(new_value.as_ref()) {
                *v = new_value;
                true
            } else {
                false
            }
        };
        if changed {
            self.notify()?;
        }
        Ok(())
    }

    pub fn add_from_strings(&self, strs: &[Vec<u8>]) -> Result<()> {
        let changed = self.value.borrow_mut().add_from_strings(strs)?;
        if changed {
            self.notify()?;
        }
        Ok(())
    }

    pub fn remove_from_strings(&self, strs: &[Vec<u8>]) -> Result<()> {
        let changed = self.value.borrow_mut().remove_from_strings(strs)?;
        if changed {
            self.notify()?;
        }
        Ok(())
    }

    pub fn update(&self, context: &Context) -> Result<()> {
        self.value.borrow_mut().update(context)
    }

    pub fn has_same_value(&self, other: &Opt) -> bool {
        if std::ptr::eq(self, other) {
            return true;
        }
        self.value.borrow().same_value(other.value.borrow().as_ref())
    }

    fn clone_for(&self, manager: &Rc<OptionManager>) -> Rc<Opt> {
        Rc::new(Opt {
            manager: Rc::downgrade(manager),
            desc: self.desc.clone(),
            value: RefCell::new(self.value.borrow().clone_box()),
            validator: self.validator,
        })
    }
}

pub trait OptionWatcher {
    fn on_option_changed(&self, option: &Opt) -> Result<()>;
}

pub struct OptionManager {
    options: RefCell<IndexMap<Vec<u8>, Rc<Opt>>>,
    parent: RefCell<Option<Rc<OptionManager>>>,
    watchers: RefCell<Vec<Weak<dyn OptionWatcher>>>,
    self_weak: Weak<OptionManager>,
}

fn option_not_found(name: &[u8]) -> Error {
    rt_err!("option not found: '{}'. Use declare-option first", name)
}

impl OptionManager {
    pub fn new_root() -> Rc<OptionManager> {
        Rc::new_cyclic(|w| OptionManager {
            options: RefCell::new(IndexMap::new()),
            parent: RefCell::new(None),
            watchers: RefCell::new(Vec::new()),
            self_weak: w.clone(),
        })
    }

    pub fn new(parent: &Rc<OptionManager>) -> Rc<OptionManager> {
        let res = Rc::new_cyclic(|w| OptionManager {
            options: RefCell::new(IndexMap::new()),
            parent: RefCell::new(Some(parent.clone())),
            watchers: RefCell::new(Vec::new()),
            self_weak: w.clone(),
        });
        parent.register_watcher(Rc::downgrade(&res) as Weak<dyn OptionWatcher>);
        res
    }

    fn self_rc(&self) -> Rc<OptionManager> {
        self.self_weak.upgrade().expect("option manager is alive")
    }

    pub fn parent(&self) -> Option<Rc<OptionManager>> {
        self.parent.borrow().clone()
    }

    pub fn reparent(&self, parent: &Rc<OptionManager>) {
        let me = self.self_weak.clone() as Weak<dyn OptionWatcher>;
        if let Some(old) = self.parent.borrow().as_ref() {
            old.unregister_watcher(&me);
        }
        *self.parent.borrow_mut() = Some(parent.clone());
        parent.register_watcher(me);
    }

    /// Unregister from parent, to be called when the owner goes away
    pub fn detach(&self) {
        let me = self.self_weak.clone() as Weak<dyn OptionWatcher>;
        if let Some(old) = self.parent.borrow().as_ref() {
            old.unregister_watcher(&me);
        }
    }

    pub fn register_watcher(&self, watcher: Weak<dyn OptionWatcher>) {
        self.watchers.borrow_mut().push(watcher);
    }

    pub fn unregister_watcher(&self, watcher: &Weak<dyn OptionWatcher>) {
        let mut watchers = self.watchers.borrow_mut();
        if let Some(pos) = watchers.iter().position(|w| Weak::ptr_eq(w, watcher)) {
            watchers.remove(pos);
        }
    }

    /// operator[]
    pub fn get(&self, name: &[u8]) -> Result<Rc<Opt>> {
        if let Some(opt) = self.options.borrow().get(name) {
            return Ok(opt.clone());
        }
        match self.parent() {
            Some(parent) => parent.get(name),
            None => Err(option_not_found(name)),
        }
    }

    /// Access a builtin option that is known to exist
    pub fn opt(&self, name: &str) -> Rc<Opt> {
        match self.get(name.as_bytes()) {
            Ok(o) => o,
            Err(e) => panic!("{}", e),
        }
    }

    /// Get a copy of the value of a builtin option of known type
    pub fn val<T: OptionType>(&self, name: &str) -> T {
        self.opt(name).get::<T>().clone()
    }

    pub fn get_local_option(&self, name: &[u8]) -> Result<Rc<Opt>> {
        if let Some(opt) = self.options.borrow().get(name) {
            return Ok(opt.clone());
        }
        match self.parent() {
            Some(parent) => {
                let clone = parent.get(name)?.clone_for(&self.self_rc());
                self.options.borrow_mut().insert(clone.name().to_vec(), clone.clone());
                Ok(clone)
            }
            None => Err(option_not_found(name)),
        }
    }

    pub fn has_local_option(&self, name: &[u8]) -> bool {
        self.options.borrow().contains_key(name)
    }

    pub fn unset_option(&self, name: &[u8]) -> Result<()> {
        let parent = self.parent().expect("cannot unset option on global manager");
        let local = self.options.borrow().get(name).cloned();
        if let Some(local) = local {
            let parent_option = parent.get(name)?;
            let changed = !parent_option.has_same_value(&local);
            self.options.borrow_mut().swap_remove(name);
            if changed {
                self.on_option_changed(&parent_option)?;
            }
        }
        Ok(())
    }

    pub fn on_option_changed(&self, option: &Opt) -> Result<()> {
        // if parent option changed, but we overrided it, it's like nothing happened
        if !std::ptr::eq(option.manager_ptr(), self) && self.options.borrow().contains_key(option.name()) {
            return Ok(());
        }
        // The watcher list might get mutated during calls to on_option_changed
        let watchers = self.watchers.borrow().clone();
        for watcher in watchers {
            let still_registered = self.watchers.borrow().iter().any(|w| Weak::ptr_eq(w, &watcher));
            if !still_registered {
                continue;
            }
            if let Some(w) = watcher.upgrade() {
                w.on_option_changed(option)?;
            }
        }
        Ok(())
    }

    /// Options of grand parent, parent and self, children overriding parents
    pub fn flatten_options(&self) -> Vec<Rc<Opt>> {
        let parent = self.parent();
        let grand_parent = parent.as_ref().and_then(|p| p.parent());
        let mut layers = Vec::new();
        if let Some(gp) = &grand_parent {
            layers.push(gp.options.borrow().clone());
        }
        if let Some(p) = &parent {
            layers.push(p.options.borrow().clone());
        }
        layers.push(self.options.borrow().clone());
        crate::face::merge_layers(layers).into_iter().map(|(_, v)| v).collect()
    }

    pub(crate) fn insert_option(&self, opt: Rc<Opt>) {
        self.options.borrow_mut().insert(opt.name().to_vec(), opt);
    }

    pub(crate) fn find_local(&self, name: &[u8]) -> Option<Rc<Opt>> {
        self.options.borrow().get(name).cloned()
    }
}

impl OptionWatcher for OptionManager {
    fn on_option_changed(&self, option: &Opt) -> Result<()> {
        OptionManager::on_option_changed(self, option)
    }
}

pub struct OptionsRegistry {
    global_manager: Rc<OptionManager>,
    descs: RefCell<Vec<Rc<OptionDesc>>>,
}

fn is_option_identifier(c: u8) -> bool {
    is_basic_alpha(c as u32) || is_basic_digit(c as u32) || c == b'_'
}

impl OptionsRegistry {
    pub fn new(global_manager: Rc<OptionManager>) -> Self {
        OptionsRegistry { global_manager, descs: RefCell::new(Vec::new()) }
    }

    pub fn declare_option<T: OptionType>(
        &self,
        name: &[u8],
        docstring: &[u8],
        value: T,
        flags: OptionFlags,
    ) -> Result<Rc<Opt>> {
        self.declare_option_checked(name, docstring, value, flags, None)
    }

    pub fn declare_option_checked<T: OptionType>(
        &self,
        name: &[u8],
        docstring: &[u8],
        value: T,
        flags: OptionFlags,
        validator: Option<Validator>,
    ) -> Result<Rc<Opt>> {
        if !name.iter().all(|&c| is_option_identifier(c)) {
            return Err(rt_err!("name '{}' contains char out of [a-zA-Z0-9_]", name));
        }
        if let Some(existing) = self.global_manager.find_local(name) {
            if existing.is_of_type::<T>() && existing.flags() == flags {
                return Ok(existing);
            }
            return Err(rt_err!("option '{}' already declared with different type or flags", name));
        }
        let type_name = T::type_name();
        let doc = if docstring.is_empty() {
            kformat!("[{}]", type_name)
        } else {
            kformat!("[{}] - {}", type_name, docstring)
        };
        let desc = Rc::new(OptionDesc { name: name.to_vec(), docstring: doc, flags });
        self.descs.borrow_mut().push(desc.clone());
        let opt = Rc::new(Opt {
            manager: Rc::downgrade(&self.global_manager),
            desc,
            value: RefCell::new(Box::new(value)),
            validator,
        });
        self.global_manager.insert_option(opt.clone());
        Ok(opt)
    }

    pub fn option_desc(&self, name: &[u8]) -> Option<Rc<OptionDesc>> {
        self.descs.borrow().iter().find(|d| d.name == name).cloned()
    }

    pub fn option_exists(&self, name: &[u8]) -> bool {
        self.option_desc(name).is_some()
    }

    pub fn complete_option_name(&self, prefix: &[u8], cursor_pos: usize) -> CandidateList {
        let descs = self.descs.borrow();
        let names: Vec<&[u8]> =
            descs.iter().filter(|d| !d.flags.has(OptionFlags::Hidden)).map(|d| d.name.as_slice()).collect();
        complete(prefix, cursor_pos, names)
    }
}
