//! STUB: to be replaced by the port of window.hh/cc
use crate::scope::Scope;

pub struct Window {
    scope: Scope,
}

impl Window {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
}
