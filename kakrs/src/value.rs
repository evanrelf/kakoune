//! Port of value.hh: a type erased map of values attached to buffers

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ValueId(pub u32);

thread_local! {
    static NEXT_VALUE_ID: Cell<u32> = const { Cell::new(0) };
}

pub fn get_free_value_id() -> ValueId {
    NEXT_VALUE_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        ValueId(id)
    })
}

/// Values are shared (Rc) so that users can keep them alive while the map
/// is modified; mutable values should use interior mutability.
#[derive(Default)]
pub struct ValueMap {
    values: RefCell<Vec<(ValueId, Rc<dyn Any>)>>,
}

impl ValueMap {
    pub fn get<T: Any>(&self, id: ValueId) -> Option<Rc<T>> {
        let values = self.values.borrow();
        let v = values.iter().find(|(i, _)| *i == id)?.1.clone();
        v.downcast::<T>().ok()
    }

    pub fn get_or_insert_with<T: Any>(&self, id: ValueId, f: impl FnOnce() -> T) -> Rc<T> {
        if let Some(v) = self.get::<T>(id) {
            return v;
        }
        let v = Rc::new(f());
        self.insert(id, v.clone());
        v
    }

    pub fn insert(&self, id: ValueId, value: Rc<dyn Any>) {
        let old = {
            let mut values = self.values.borrow_mut();
            match values.iter_mut().find(|(i, _)| *i == id) {
                Some(entry) => Some(std::mem::replace(&mut entry.1, value)),
                None => {
                    values.push((id, value));
                    None
                }
            }
        };
        drop(old); // dropped outside of the borrow, destructors may access the map
    }

    pub fn contains(&self, id: ValueId) -> bool {
        self.values.borrow().iter().any(|(i, _)| *i == id)
    }

    pub fn erase(&self, id: ValueId) {
        let removed = {
            let mut values = self.values.borrow_mut();
            values.iter().position(|(i, _)| *i == id).map(|pos| values.swap_remove(pos))
        };
        drop(removed);
    }

    pub fn clear(&self) {
        let removed = std::mem::take(&mut *self.values.borrow_mut());
        drop(removed);
    }
}
