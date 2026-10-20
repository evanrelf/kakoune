//! Port of keymap_manager.hh/cc

use crate::exception::Result;
use crate::keys::{Key, KeyList};
use crate::unicode::is_identifier;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct KeymapMode(pub u8);

#[allow(non_upper_case_globals)]
impl KeymapMode {
    pub const None: KeymapMode = KeymapMode(0);
    pub const Normal: KeymapMode = KeymapMode(1);
    pub const Insert: KeymapMode = KeymapMode(2);
    pub const Prompt: KeymapMode = KeymapMode(3);
    pub const Menu: KeymapMode = KeymapMode(4);
    pub const Goto: KeymapMode = KeymapMode(5);
    pub const View: KeymapMode = KeymapMode(6);
    pub const User: KeymapMode = KeymapMode(7);
    pub const Object: KeymapMode = KeymapMode(8);
    pub const Combine: KeymapMode = KeymapMode(9);
    pub const FirstUserMode: KeymapMode = KeymapMode(10);
}

#[derive(Clone, Debug, Default)]
pub struct KeymapInfo {
    pub keys: KeyList,
    pub docstring: Vec<u8>,
    pub atomic: bool,
}

#[derive(Default)]
pub struct KeymapManager {
    parent: RefCell<Option<Rc<KeymapManager>>>,
    mapping: RefCell<IndexMap<(Key, KeymapMode), KeymapInfo>>,
    user_modes: RefCell<Vec<Vec<u8>>>,
}

impl KeymapManager {
    pub fn new(parent: &Rc<KeymapManager>) -> KeymapManager {
        KeymapManager { parent: RefCell::new(Some(parent.clone())), ..Default::default() }
    }

    pub fn reparent(&self, parent: &Rc<KeymapManager>) {
        *self.parent.borrow_mut() = Some(parent.clone());
    }

    fn parent(&self) -> Option<Rc<KeymapManager>> {
        self.parent.borrow().clone()
    }

    pub fn map_key(&self, key: Key, mode: KeymapMode, mapping: KeyList, docstring: Vec<u8>, atomic: bool) {
        self.mapping.borrow_mut().insert((key, mode), KeymapInfo { keys: mapping, docstring, atomic });
    }

    pub fn unmap_key(&self, key: Key, mode: KeymapMode) {
        self.mapping.borrow_mut().shift_remove(&(key, mode));
    }

    pub fn unmap_keys(&self, mode: KeymapMode) {
        self.mapping.borrow_mut().retain(|(_, m), _| *m != mode);
    }

    pub fn get_mapping(&self, key: Key, mode: KeymapMode) -> Option<KeymapInfo> {
        if let Some(info) = self.mapping.borrow().get(&(key, mode)) {
            return Some(info.clone());
        }
        self.parent().and_then(|p| p.get_mapping(key, mode))
    }

    pub fn get_mapped_keys(&self, mode: KeymapMode) -> KeyList {
        let mut res = match self.parent() {
            Some(p) => p.get_mapped_keys(mode),
            None => KeyList::new(),
        };
        for (k, m) in self.mapping.borrow().keys() {
            if *m == mode && !res.contains(k) {
                res.push(*k);
            }
        }
        res
    }

    pub fn user_modes(&self) -> Vec<Vec<u8>> {
        match self.parent() {
            Some(p) => p.user_modes(),
            None => self.user_modes.borrow().clone(),
        }
    }

    fn push_user_mode(&self, name: Vec<u8>) {
        match self.parent() {
            Some(p) => p.push_user_mode(name),
            None => self.user_modes.borrow_mut().push(name),
        }
    }

    pub fn add_user_mode(&self, user_mode_name: Vec<u8>) -> Result<()> {
        let modes: [&[u8]; 9] =
            [b"normal", b"insert", b"prompt", b"menu", b"goto", b"view", b"user", b"object", b"combine"];
        if modes.contains(&user_mode_name.as_slice()) {
            crate::throw!("'{}' is already a regular mode", user_mode_name);
        }
        if self.user_modes().contains(&user_mode_name) {
            crate::throw!("user mode '{}' already defined", user_mode_name);
        }
        if !user_mode_name.iter().all(|&c| is_identifier(c as u32)) {
            crate::throw!("invalid mode name: '{}'", user_mode_name);
        }
        self.push_user_mode(user_mode_name);
        Ok(())
    }
}
