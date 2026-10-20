//! Port of the parts of utils.hh that are still useful in Rust

use std::cell::Cell;

/// A counted boolean, set while at least one setter is alive
#[derive(Default, Debug)]
pub struct NestedBool {
    count: Cell<i32>,
}

impl NestedBool {
    pub fn set(&self) {
        self.count.set(self.count.get() + 1);
    }
    pub fn unset(&self) {
        debug_assert!(self.count.get() > 0);
        self.count.set(self.count.get() - 1);
    }
    pub fn is_set(&self) -> bool {
        self.count.get() > 0
    }
}

/// Sets a NestedBool for the duration of its lifetime
pub struct ScopedSetBool<'a> {
    nested_bool: &'a NestedBool,
    condition: bool,
}

impl<'a> ScopedSetBool<'a> {
    pub fn new(nested_bool: &'a NestedBool, condition: bool) -> Self {
        if condition {
            nested_bool.set();
        }
        ScopedSetBool { nested_bool, condition }
    }
    pub fn set(nested_bool: &'a NestedBool) -> Self {
        Self::new(nested_bool, true)
    }
}

impl Drop for ScopedSetBool<'_> {
    fn drop(&mut self) {
        if self.condition {
            self.nested_bool.unset();
        }
    }
}

/// Run a closure when going out of scope
pub struct OnScopeEnd<F: FnOnce()> {
    func: Option<F>,
}

impl<F: FnOnce()> OnScopeEnd<F> {
    pub fn new(func: F) -> Self {
        OnScopeEnd { func: Some(func) }
    }
    pub fn cancel(&mut self) {
        self.func = None;
    }
}

impl<F: FnOnce()> Drop for OnScopeEnd<F> {
    fn drop(&mut self) {
        if let Some(f) = self.func.take() {
            f();
        }
    }
}

/// Restore a Cell value when going out of scope
pub struct ScopedValue<'a, T: Copy> {
    cell: &'a Cell<T>,
    saved: T,
}

impl<'a, T: Copy> ScopedValue<'a, T> {
    pub fn new(cell: &'a Cell<T>, value: T) -> Self {
        let saved = cell.replace(value);
        ScopedValue { cell, saved }
    }
}

impl<T: Copy> Drop for ScopedValue<'_, T> {
    fn drop(&mut self) {
        self.cell.set(self.saved);
    }
}
