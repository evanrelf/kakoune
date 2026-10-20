//! Port of highlighter_group.hh/cc

use crate::completion::{complete, offset_pos, CompletionFlags, Completions};
use crate::display_buffer::DisplayBuffer;
use crate::exception::{Error, ErrorKind, Result};
use crate::highlighter::{compute_display_setup, highlight, DisplaySetup, HighlightContext, HighlightPass, Highlighter};
use crate::units::BufferRange;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

pub fn child_not_found(msg: Vec<u8>) -> Error {
    Error::new(ErrorKind::ChildNotFound, msg)
}

pub struct HighlighterGroup {
    passes: HighlightPass,
    highlighters: RefCell<IndexMap<Vec<u8>, Rc<dyn Highlighter>>>,
}

impl HighlighterGroup {
    pub fn new(passes: HighlightPass) -> Self {
        HighlighterGroup { passes, highlighters: RefCell::new(IndexMap::new()) }
    }

    fn children(&self) -> Vec<Rc<dyn Highlighter>> {
        self.highlighters.borrow().values().cloned().collect()
    }
}

impl Highlighter for HighlighterGroup {
    fn passes(&self) -> HighlightPass {
        self.passes
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) -> Result<()> {
        for hl in self.children() {
            highlight(hl.as_ref(), context, display_buffer, range);
        }
        Ok(())
    }

    fn do_compute_display_setup(&self, context: &HighlightContext, setup: &mut DisplaySetup) -> Result<()> {
        for hl in self.children() {
            compute_display_setup(hl.as_ref(), context, setup);
        }
        Ok(())
    }

    fn has_children(&self) -> bool {
        true
    }

    fn fill_unique_ids(&self, unique_ids: &mut Vec<Vec<u8>>) {
        for hl in self.children() {
            hl.fill_unique_ids(unique_ids);
        }
    }

    fn add_child(&self, name: Vec<u8>, hl: Rc<dyn Highlighter>, override_: bool) -> Result<()> {
        if (hl.passes() & self.passes) != hl.passes() {
            return Err(Error::runtime("cannot add that highlighter to this group, passes don't match"));
        }
        let mut highlighters = self.highlighters.borrow_mut();
        if let Some(existing) = highlighters.get_mut(&name) {
            if !override_ {
                return Err(rt_err!("duplicate id: '{}'", name));
            }
            *existing = hl;
            return Ok(());
        }
        highlighters.insert(name, hl);
        Ok(())
    }

    fn remove_child(&self, id: &[u8]) -> Result<()> {
        let removed = self.highlighters.borrow_mut().shift_remove(id);
        match removed {
            Some(_) => Ok(()),
            None => Err(child_not_found(kformat!("no such id: '{}'", id))),
        }
    }

    fn get_child(&self, path: &[u8]) -> Result<Rc<dyn Highlighter>> {
        let sep = path.iter().position(|&c| c == b'/');
        let id = &path[..sep.unwrap_or(path.len())];
        let child = self.highlighters.borrow().get(id).cloned();
        let Some(child) = child else {
            return Err(child_not_found(kformat!("no such id: '{}'", id)));
        };
        match sep {
            None => Ok(child),
            Some(sep) => child.get_child(&path[sep + 1..]),
        }
    }

    fn complete_child(&self, path: &[u8], cursor_pos: usize, group: bool) -> Result<Completions> {
        if let Some(sep) = path.iter().position(|&c| c == b'/') {
            let offset = sep + 1;
            let hl = self.get_child(&path[..sep])?;
            return Ok(offset_pos(hl.complete_child(&path[offset..], cursor_pos - offset, group)?, offset));
        }
        let names: Vec<Vec<u8>> = self
            .highlighters
            .borrow()
            .iter()
            .filter(|(_, hl)| !group || hl.has_children())
            .map(|(k, hl)| if hl.has_children() { kformat!("{}/", k) } else { k.clone() })
            .collect();
        let candidates = complete(path, cursor_pos, &names);
        let flags = if group { CompletionFlags::None } else { CompletionFlags::Menu };
        Ok(Completions::with(0, 0, candidates, flags))
    }
}

/// The highlighters of a scope, delegating to the parent scope's ones
pub struct Highlighters {
    parent: RefCell<Option<Rc<Highlighters>>>,
    group: Rc<HighlighterGroup>,
}

impl Highlighters {
    pub fn new_root() -> Self {
        Highlighters { parent: RefCell::new(None), group: Rc::new(HighlighterGroup::new(HighlightPass::All)) }
    }

    pub fn new(parent: &Rc<Highlighters>) -> Self {
        Highlighters { parent: RefCell::new(Some(parent.clone())), group: Rc::new(HighlighterGroup::new(HighlightPass::All)) }
    }

    pub fn reparent(&self, parent: &Rc<Highlighters>) {
        *self.parent.borrow_mut() = Some(parent.clone());
    }

    pub fn group(&self) -> &Rc<HighlighterGroup> {
        &self.group
    }

    pub fn highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) {
        let mut disabled_ids: Vec<Vec<u8>> = context.disabled_ids.to_vec();
        self.group.fill_unique_ids(&mut disabled_ids);
        let parent = self.parent.borrow().clone();
        if let Some(parent) = parent {
            parent.highlight(&context.with_disabled(&disabled_ids), display_buffer, range);
        }
        highlight(self.group.as_ref(), context, display_buffer, range);
    }

    pub fn compute_display_setup(&self, context: &HighlightContext, setup: &mut DisplaySetup) {
        let mut disabled_ids: Vec<Vec<u8>> = context.disabled_ids.to_vec();
        self.group.fill_unique_ids(&mut disabled_ids);
        let parent = self.parent.borrow().clone();
        if let Some(parent) = parent {
            parent.compute_display_setup(&context.with_disabled(&disabled_ids), setup);
        }
        compute_display_setup(self.group.as_ref(), context, setup);
    }
}
