//! Port of alias_registry.hh/cc

use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

pub type AliasMap = IndexMap<Vec<u8>, Vec<u8>>;

#[derive(Default)]
pub struct AliasRegistry {
    parent: RefCell<Option<Rc<AliasRegistry>>>,
    aliases: RefCell<AliasMap>,
}

impl AliasRegistry {
    pub fn new(parent: &Rc<AliasRegistry>) -> AliasRegistry {
        AliasRegistry { parent: RefCell::new(Some(parent.clone())), aliases: RefCell::new(AliasMap::new()) }
    }

    pub fn reparent(&self, parent: &Rc<AliasRegistry>) {
        *self.parent.borrow_mut() = Some(parent.clone());
    }

    fn parent(&self) -> Option<Rc<AliasRegistry>> {
        self.parent.borrow().clone()
    }

    pub fn add_alias(&self, alias: Vec<u8>, command: Vec<u8>) {
        debug_assert!(!alias.is_empty());
        self.aliases.borrow_mut().insert(alias, command);
    }

    pub fn remove_alias(&self, alias: &[u8]) {
        self.aliases.borrow_mut().shift_remove(alias);
    }

    /// operator[]: empty if not found
    pub fn get(&self, alias: &[u8]) -> Vec<u8> {
        if let Some(c) = self.aliases.borrow().get(alias) {
            return c.clone();
        }
        match self.parent() {
            Some(p) => p.get(alias),
            None => Vec::new(),
        }
    }

    pub fn aliases_for(&self, command: &[u8]) -> Vec<Vec<u8>> {
        let mut res = match self.parent() {
            Some(p) => p.aliases_for(command),
            None => Vec::new(),
        };
        for (alias, cmd) in self.aliases.borrow().iter() {
            if cmd == command {
                res.push(alias.clone());
            }
        }
        res
    }

    pub fn flatten_aliases(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let parent = self.parent();
        let grand_parent = parent.as_ref().and_then(|p| p.parent());
        let mut layers = Vec::new();
        if let Some(gp) = &grand_parent {
            layers.push(gp.aliases.borrow().clone());
        }
        if let Some(p) = &parent {
            layers.push(p.aliases.borrow().clone());
        }
        layers.push(self.aliases.borrow().clone());
        crate::face::merge_layers(layers)
    }
}
