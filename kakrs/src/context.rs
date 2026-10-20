//! Port of context.hh/cc and local_scope.hh

use crate::alias_registry::AliasRegistry;
use crate::buffer::Buffer;
use crate::buffer_manager::BufferManager;
use crate::client::Client;
use crate::display_buffer::DisplayLine;
use crate::exception::{Error, Result};
use crate::face::FaceRegistry;
use crate::hook_manager::{Hook, HookManager};
use crate::input_handler::InputHandler;
use crate::keymap_manager::KeymapManager;
use crate::option_manager::OptionManager;
use crate::register_manager::RegisterManager;
use crate::scope::{GlobalScope, Scope};
use crate::selection::{Selection, SelectionList};
use crate::units::ColumnCount;
use crate::user_interface::StatusStyle;
use crate::utils::NestedBool;
use crate::window::Window;
use std::cell::{Cell, RefCell, RefMut};
use std::rc::{Rc, Weak};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Backward = -1,
    Forward = 1,
}

crate::kak_flags! {
    pub struct ContextFlags: u32 {
        const None = 0;
        const Draft = 1;
    }
}

#[derive(Clone, Default, PartialEq)]
pub struct JumpList {
    jumps: Vec<SelectionList>,
    current: usize,
}

impl JumpList {
    pub fn push(&mut self, jump: SelectionList, index: Option<usize>) {
        if let Some(index) = index {
            self.current = index;
            debug_assert!(self.current <= self.jumps.len());
        }
        if self.current != self.jumps.len() {
            self.jumps.truncate(self.current + 1);
        }
        self.jumps.retain(|j| *j != jump);
        self.jumps.push(jump);
        self.current = self.jumps.len();
    }

    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn get_as_list(&self) -> &[SelectionList] {
        &self.jumps
    }

    pub fn forget_buffer(&mut self, buffer: &Buffer) {
        let mut i = 0;
        while i < self.jumps.len() {
            if std::ptr::eq(self.jumps[i].buffer().as_ref(), buffer) {
                if i < self.current {
                    self.current -= 1;
                } else if i == self.current {
                    self.current = self.jumps.len() - 1;
                }
                self.jumps.remove(i);
            } else {
                i += 1;
            }
        }
    }
}

pub type LastSelectFunc = Rc<dyn Fn(&Context) -> Result<()>>;

const HISTORY_INVALID: usize = usize::MAX;

#[derive(Clone)]
struct SelectionHistoryNode {
    selections: SelectionList,
    parent: usize,
    redo_child: usize,
}

impl SelectionHistoryNode {
    fn new(selections: SelectionList, parent: usize) -> Self {
        SelectionHistoryNode { selections, parent, redo_child: HISTORY_INVALID }
    }
}

#[derive(Default)]
struct SelectionHistory {
    history: Vec<SelectionHistoryNode>,
    history_id: usize,
    staging: Option<SelectionHistoryNode>,
    in_edition: i32,
}

impl SelectionHistory {
    fn empty(&self) -> bool {
        self.history.is_empty() && self.staging.is_none()
    }

    fn initialize(&mut self, selections: SelectionList) {
        debug_assert!(self.empty());
        self.history = vec![SelectionHistoryNode::new(selections, HISTORY_INVALID)];
        self.history_id = 0;
    }

    fn current_selections_mut(&mut self) -> &mut SelectionList {
        match &mut self.staging {
            Some(s) => &mut s.selections,
            None => &mut self.history[self.history_id].selections,
        }
    }

    fn begin_edition(&mut self) {
        if self.in_edition == 0 {
            let mut sels = self.current_selections_mut().clone();
            sels.update(true);
            self.staging = Some(SelectionHistoryNode::new(sels, self.history_id));
        }
        self.in_edition += 1;
    }

    fn end_edition(&mut self) {
        debug_assert!(self.in_edition > 0);
        self.in_edition -= 1;
        if self.in_edition > 0 {
            return;
        }
        let staging = self.staging.take().expect("staging selections");
        if self.history_id != HISTORY_INVALID && self.history[self.history_id].selections == staging.selections {
            // No change, except maybe the index of the main selection.
            // Update timestamp to potentially improve interaction with content undo.
            let node = &mut self.history[self.history_id];
            node.selections.force_timestamp(staging.selections.timestamp());
            node.selections.set_main_index(staging.selections.main_index());
        } else {
            self.history_id = self.history.len();
            self.history.push(staging);
        }
    }

    fn forget_buffer(&mut self, buffer: &Buffer) {
        let mut new_ids = Vec::with_capacity(self.history.len());
        let mut bias = 0;
        for node in &self.history {
            if std::ptr::eq(node.selections.buffer().as_ref(), buffer) {
                new_ids.push(HISTORY_INVALID);
                bias += 1;
            } else {
                new_ids.push(new_ids.len() - bias);
            }
        }
        let new_id = |old: usize| if old == HISTORY_INVALID { HISTORY_INVALID } else { new_ids[old] };
        self.history.retain(|node| !std::ptr::eq(node.selections.buffer().as_ref(), buffer));
        for node in &mut self.history {
            node.parent = new_id(node.parent);
            node.redo_child = new_id(node.redo_child);
        }
        self.history_id = new_id(self.history_id);
        if let Some(staging) = &mut self.staging {
            staging.parent = new_id(staging.parent);
        }
    }
}

/// A Context is used to access non singleton objects for various services
/// in commands.
///
/// The Context object links a Client, a Window, an InputHandler and a
/// SelectionList. It may represent an interactive user window, a hook
/// execution or a macro replay context.
pub struct Context {
    flags: ContextFlags,
    input_handler: Option<Weak<InputHandler>>,
    window: RefCell<Option<Rc<Window>>>,
    client: RefCell<Option<Weak<Client>>>,
    local_scopes: RefCell<Vec<Scope>>,
    selection_history: RefCell<SelectionHistory>,
    /// Buffer of the current selections, used when the selection history is
    /// borrowed (for example while iterating on selections with a callback
    /// that queries the context buffer).
    buffer_cache: RefCell<Option<Rc<Buffer>>>,
    name: RefCell<Vec<u8>>,
    jump_list: RefCell<JumpList>,
    last_select: RefCell<Option<LastSelectFunc>>,
    hooks_disabled: NestedBool,
    keymaps_disabled: NestedBool,
    edition_level: Cell<i32>,
    edition_timestamp: Cell<usize>,
    pub ensure_cursor_visible: Cell<bool>,
}

impl Context {
    pub fn new(input_handler: Weak<InputHandler>, selections: SelectionList, flags: ContextFlags, name: Vec<u8>) -> Self {
        let mut history = SelectionHistory::default();
        history.initialize(selections);
        Context {
            flags,
            input_handler: Some(input_handler),
            window: RefCell::new(None),
            client: RefCell::new(None),
            local_scopes: RefCell::new(Vec::new()),
            selection_history: RefCell::new(history),
            buffer_cache: RefCell::new(None),
            name: RefCell::new(name),
            jump_list: RefCell::new(JumpList::default()),
            last_select: RefCell::new(None),
            hooks_disabled: NestedBool::default(),
            keymaps_disabled: NestedBool::default(),
            edition_level: Cell::new(0),
            edition_timestamp: Cell::new(0),
            ensure_cursor_visible: Cell::new(true),
        }
    }

    /// Context(EmptyContextFlag)
    pub fn new_empty() -> Self {
        Context {
            flags: ContextFlags::None,
            input_handler: None,
            window: RefCell::new(None),
            client: RefCell::new(None),
            local_scopes: RefCell::new(Vec::new()),
            selection_history: RefCell::new(SelectionHistory::default()),
            buffer_cache: RefCell::new(None),
            name: RefCell::new(Vec::new()),
            jump_list: RefCell::new(JumpList::default()),
            last_select: RefCell::new(None),
            hooks_disabled: NestedBool::default(),
            keymaps_disabled: NestedBool::default(),
            edition_level: Cell::new(0),
            edition_timestamp: Cell::new(0),
            ensure_cursor_visible: Cell::new(true),
        }
    }

    pub fn has_buffer(&self) -> bool {
        match self.selection_history.try_borrow() {
            Ok(history) => !history.empty(),
            // borrowed for selection access, so there are selections
            Err(_) => true,
        }
    }

    pub fn buffer(&self) -> Rc<Buffer> {
        self.try_buffer().expect("no buffer in context")
    }

    pub fn try_buffer(&self) -> Result<Rc<Buffer>> {
        match self.selection_history.try_borrow_mut() {
            Ok(mut history) => {
                if history.empty() {
                    return Err(Error::runtime("no buffer in context"));
                }
                let buffer = history.current_selections_mut().buffer().clone();
                *self.buffer_cache.borrow_mut() = Some(buffer.clone());
                Ok(buffer)
            }
            Err(_) => self.buffer_cache.borrow().clone().ok_or_else(|| Error::runtime("no buffer in context")),
        }
    }

    pub fn has_window(&self) -> bool {
        self.window.borrow().is_some()
    }

    pub fn window(&self) -> Rc<Window> {
        self.try_window().expect("no window in context")
    }

    pub fn try_window(&self) -> Result<Rc<Window>> {
        self.window.borrow().clone().ok_or_else(|| Error::runtime("no window in context"))
    }

    pub fn has_client(&self) -> bool {
        self.client.borrow().as_ref().is_some_and(|c| c.strong_count() > 0)
    }

    pub fn client(&self) -> Rc<Client> {
        self.try_client().expect("no client in context")
    }

    pub fn try_client(&self) -> Result<Rc<Client>> {
        self.client.borrow().as_ref().and_then(|c| c.upgrade()).ok_or_else(|| Error::runtime("no client in context"))
    }

    pub fn has_input_handler(&self) -> bool {
        self.input_handler.as_ref().is_some_and(|i| i.strong_count() > 0)
    }

    pub fn input_handler(&self) -> Rc<InputHandler> {
        self.try_input_handler().expect("no input handler in context")
    }

    pub fn try_input_handler(&self) -> Result<Rc<InputHandler>> {
        self.input_handler
            .as_ref()
            .and_then(|i| i.upgrade())
            .ok_or_else(|| Error::runtime("no input handler in context"))
    }

    /// Returns the (updated) selections. The returned borrow must not be held
    /// across calls that may access this context's selections.
    pub fn selections(&self) -> RefMut<'_, SelectionList> {
        self.selections_upd(true)
    }

    pub fn try_selections(&self) -> Result<RefMut<'_, SelectionList>> {
        if !self.has_buffer() {
            return Err(Error::runtime("no selections in context"));
        }
        Ok(self.selections_upd(true))
    }

    pub fn selections_upd(&self, update: bool) -> RefMut<'_, SelectionList> {
        let history = self.selection_history.borrow_mut();
        if history.empty() {
            panic!("no selections in context");
        }
        let mut sels = RefMut::map(history, |h| h.current_selections_mut());
        *self.buffer_cache.borrow_mut() = Some(sels.buffer().clone());
        if update {
            sels.update(true);
        }
        sels
    }

    /// Return potentially out of date selections
    pub fn selections_write_only(&self) -> RefMut<'_, SelectionList> {
        self.selections_upd(false)
    }

    pub fn set_selections(&self, selections: SelectionList) {
        *self.selections_write_only() = selections;
    }

    pub fn selections_content(&self) -> Vec<Vec<u8>> {
        let sels = self.selections();
        let buf = sels.buffer().clone();
        sels.iter().map(|sel| buf.string(sel.min(), buf.char_next(sel.max()))).collect()
    }

    pub fn end_selection_edition(&self) {
        self.selection_history.borrow_mut().end_edition();
    }

    pub fn undo_selection_change(&self, direction: Direction) -> Result<()> {
        let backward = direction == Direction::Backward;
        if self.selection_history.borrow().in_edition > 0 {
            return Err(Error::runtime("selection undo is only supported at top-level"));
        }
        let old_selections = self.selections().clone();
        loop {
            let (next, destination_buffer) = {
                let history = self.selection_history.borrow();
                let node = &history.history[history.history_id];
                let next = if backward { node.parent } else { node.redo_child };
                if next == HISTORY_INVALID {
                    return Err(Error::runtime(if backward {
                        "no selection change to undo"
                    } else {
                        "no selection change to redo"
                    }));
                }
                (next, history.history[next].selections.buffer().clone())
            };
            let select_next = || {
                let mut history = self.selection_history.borrow_mut();
                let previous_id = history.history_id;
                history.history_id = next;
                if backward {
                    history.history[next].redo_child = previous_id;
                }
            };
            if Rc::ptr_eq(&destination_buffer, &self.buffer()) {
                select_next();
            } else {
                self.change_buffer(&destination_buffer, Some(&select_next))?;
            }
            if *self.selections() != old_selections {
                break;
            }
        }
        Ok(())
    }

    pub fn change_buffer(&self, buffer: &Rc<Buffer>, set_selections: Option<&dyn Fn()>) -> Result<()> {
        if self.has_buffer() && Rc::ptr_eq(buffer, &self.buffer()) {
            return Ok(());
        }
        if self.has_buffer() && self.edition_level.get() > 0 {
            self.buffer().commit_undo_group();
        }

        let client = self.try_client().ok();
        let is_client_context = client.as_ref().is_some_and(|c| std::ptr::eq(c.context(), self));
        if let (Some(client), true) = (client, is_client_context) {
            // Hook contexts may set the client but not be the client's context
            client.info_hide(false);
            client.menu_hide();
            client.change_buffer(buffer, set_selections)?;
        } else {
            *self.window.borrow_mut() = None;
            if self.selection_history.borrow().empty() {
                self.selection_history
                    .borrow_mut()
                    .initialize(SelectionList::new(buffer.clone(), Selection::default()));
            } else {
                let _selection_edition = ScopedSelectionEdition::new(self);
                *self.selections_write_only() = SelectionList::new(buffer.clone(), Selection::default());
            }
            if let Some(front) = self.local_scopes.borrow().first() {
                front.reparent(buffer.scope());
            }
        }

        if let Ok(input_handler) = self.try_input_handler() {
            input_handler.reset_normal_mode()?;
        }
        Ok(())
    }

    pub fn forget_buffer(&self, buffer: &Buffer) -> Result<()> {
        self.jump_list.borrow_mut().forget_buffer(buffer);

        if std::ptr::eq(self.buffer().as_ref(), buffer) {
            if self.is_editing() {
                if let Ok(input_handler) = self.try_input_handler() {
                    input_handler.reset_normal_mode()?;
                }
            }
            let target = match self.last_buffer() {
                Some(b) => b,
                None => BufferManager::instance().get_first_buffer()?,
            };
            self.change_buffer(&target, None)?;
        }
        self.selection_history.borrow_mut().forget_buffer(buffer);
        Ok(())
    }

    pub fn set_client(&self, client: &Rc<Client>) {
        debug_assert!(!self.has_client());
        *self.client.borrow_mut() = Some(Rc::downgrade(client));
    }

    pub fn set_window(&self, window: &Rc<Window>) {
        *self.window.borrow_mut() = Some(window.clone());
        if let Some(front) = self.local_scopes.borrow().first() {
            front.reparent(window.scope());
        }
    }

    /// The current scope: local scope, window, buffer, or global
    pub fn scope(&self) -> Scope {
        self.scope_allow_local(true)
    }

    pub fn scope_allow_local(&self, allow_local: bool) -> Scope {
        if allow_local {
            if let Some(local) = self.local_scopes.borrow().last() {
                return local.clone();
            }
        }
        if let Some(window) = self.window.borrow().as_ref() {
            return window.scope().clone();
        }
        if self.has_buffer() {
            return self.buffer().scope().clone();
        }
        GlobalScope::instance().scope().clone()
    }

    pub fn local_scope(&self) -> Option<Scope> {
        self.local_scopes.borrow().last().cloned()
    }

    pub fn options(&self) -> Rc<OptionManager> {
        self.scope().options().clone()
    }
    pub fn hooks(&self) -> Rc<HookManager> {
        self.scope().hooks().clone()
    }
    pub fn keymaps(&self) -> Rc<KeymapManager> {
        self.scope().keymaps().clone()
    }
    pub fn aliases(&self) -> Rc<AliasRegistry> {
        self.scope().aliases().clone()
    }
    pub fn faces(&self) -> Rc<FaceRegistry> {
        self.scope().faces().clone()
    }
    pub fn faces_allow_local(&self, allow_local: bool) -> Rc<FaceRegistry> {
        self.scope_allow_local(allow_local).faces().clone()
    }

    pub fn print_status_full(&self, prompt: DisplayLine, content: DisplayLine, cursor_pos: ColumnCount, style: StatusStyle) {
        if let Ok(client) = self.try_client() {
            client.print_status(prompt, content, cursor_pos, style);
        }
    }

    pub fn print_status(&self, status: DisplayLine) {
        self.print_status_full(DisplayLine::default(), status, -1, StatusStyle::Status);
    }

    pub fn main_sel_register_value(&self, reg: &[u8]) -> Result<Vec<u8>> {
        let index = if self.has_buffer() { self.selections_upd(false).main_index() } else { 0 };
        RegisterManager::instance().get(reg)?.get_main(self, index)
    }

    pub fn name(&self) -> Vec<u8> {
        self.name.borrow().clone()
    }

    pub fn set_name(&self, name: Vec<u8>) -> Result<()> {
        let old_name = std::mem::replace(&mut *self.name.borrow_mut(), name.clone());
        self.hooks().run_hook(Hook::ClientRenamed, &kformat!("{}:{}", old_name, name), self)
    }

    pub fn is_editing(&self) -> bool {
        self.edition_level.get() != 0
    }

    pub fn disable_undo_handling(&self) {
        self.edition_level.set(-1);
    }

    pub fn hooks_disabled(&self) -> &NestedBool {
        &self.hooks_disabled
    }

    pub fn keymaps_disabled(&self) -> &NestedBool {
        &self.keymaps_disabled
    }

    pub fn flags(&self) -> ContextFlags {
        self.flags
    }

    pub fn jump_list(&self) -> RefMut<'_, JumpList> {
        self.jump_list.borrow_mut()
    }

    pub fn push_jump(&self, force: bool) {
        if force || !self.flags.has(ContextFlags::Draft) {
            let sels = self.selections().clone();
            self.jump_list.borrow_mut().push(sels, None);
        }
    }

    /// JumpList::forward
    pub fn jump_forward(&self, count: i32) -> Result<SelectionList> {
        let res = {
            let mut jl = self.jump_list.borrow_mut();
            let count = count as usize;
            if jl.current != jl.jumps.len() && jl.current + count < jl.jumps.len() {
                jl.current += count;
                let cur = jl.current;
                let res = &mut jl.jumps[cur];
                res.update(true);
                Some((res.clone(), cur, jl.jumps.len()))
            } else {
                None
            }
        };
        match res {
            Some((sels, cur, len)) => {
                self.print_status(DisplayLine::from_str_face(
                    kformat!("jumped to #{} ({})", cur, len - 1),
                    self.faces().face("Information"),
                ));
                Ok(sels)
            }
            None => Err(Error::runtime("no next jump")),
        }
    }

    /// JumpList::backward
    pub fn jump_backward(&self, count: i32) -> Result<SelectionList> {
        let current = self.selections().clone();
        let (sels, cur, len) = {
            let mut jl = self.jump_list.borrow_mut();
            let should_push_current = jl.current == jl.jumps.len() || jl.jumps[jl.current] != current;
            if should_push_current {
                jl.push(current, None);
            }
            let steps = count + if should_push_current { 1 } else { 0 };
            if (jl.current as i32) - steps < 0 {
                return Err(Error::runtime("no previous jump"));
            }
            jl.current -= steps as usize;
            let cur = jl.current;
            let res = &mut jl.jumps[cur];
            res.update(true);
            (res.clone(), cur, jl.jumps.len())
        };
        self.print_status(DisplayLine::from_str_face(
            kformat!("jumped to #{} ({})", cur, len - 1),
            self.faces().face("Information"),
        ));
        Ok(sels)
    }

    pub fn set_last_select(&self, func: LastSelectFunc) {
        *self.last_select.borrow_mut() = Some(func);
    }

    pub fn repeat_last_select(&self) -> Result<()> {
        let f = self.last_select.borrow().clone();
        match f {
            Some(f) => f(self),
            None => Ok(()),
        }
    }

    pub fn last_buffer(&self) -> Option<Rc<Buffer>> {
        let jl = self.jump_list.borrow();
        let jump_list = jl.get_as_list();
        if jump_list.is_empty() {
            return None;
        }
        let this_buffer = self.buffer();
        let predicate = |sels: &&SelectionList| !Rc::ptr_eq(sels.buffer(), &this_buffer);
        let current = jl.current_index();
        let start = current.saturating_sub(1);
        if let Some(next) = jump_list[start..].iter().find(predicate) {
            return Some(next.buffer().clone());
        }
        jump_list[..current.min(jump_list.len())].iter().rev().find(predicate).map(|s| s.buffer().clone())
    }

    pub fn begin_edition(&self) {
        let level = self.edition_level.get();
        if level >= 0 {
            if level == 0 {
                self.edition_timestamp.set(self.buffer().timestamp());
            }
            self.edition_level.set(level + 1);
        }
    }

    pub fn end_edition(&self) {
        let level = self.edition_level.get();
        if level < 0 {
            return;
        }
        debug_assert!(level != 0);
        if level == 1 && self.buffer().timestamp() != self.edition_timestamp.get() {
            self.buffer().commit_undo_group();
        }
        self.edition_level.set(level - 1);
    }

    pub fn begin_selection_edition(&self) {
        self.selection_history.borrow_mut().begin_edition();
    }

    fn selection_history_begin_edition(&self) {
        self.selection_history.borrow_mut().begin_edition();
    }

    fn selection_history_end_edition(&self) {
        self.selection_history.borrow_mut().end_edition();
    }
}

/// Port of ScopedEdition
pub struct ScopedEdition<'a> {
    context: &'a Context,
    active: bool,
}

impl<'a> ScopedEdition<'a> {
    pub fn new(context: &'a Context) -> Self {
        let active = context.has_buffer();
        if active {
            context.begin_edition();
        }
        ScopedEdition { context, active }
    }
    pub fn context(&self) -> &'a Context {
        self.context
    }
}

impl Drop for ScopedEdition<'_> {
    fn drop(&mut self) {
        if self.active {
            self.context.end_edition();
        }
    }
}

/// Port of ScopedSelectionEdition
pub struct ScopedSelectionEdition<'a> {
    context: &'a Context,
    valid: bool,
}

impl<'a> ScopedSelectionEdition<'a> {
    pub fn new(context: &'a Context) -> Self {
        let valid = !context.flags().has(ContextFlags::Draft) && context.has_buffer();
        if valid {
            context.selection_history_begin_edition();
        }
        ScopedSelectionEdition { context, valid }
    }
}

impl Drop for ScopedSelectionEdition<'_> {
    fn drop(&mut self) {
        if self.valid {
            self.context.selection_history_end_edition();
        }
    }
}

/// Port of LocalScope: pushes a new scope on the context for its lifetime
pub struct LocalScope<'a> {
    context: &'a Context,
    scope: Scope,
}

impl<'a> LocalScope<'a> {
    pub fn new(context: &'a Context) -> Self {
        let scope = Scope::new(&context.scope());
        context.local_scopes.borrow_mut().push(scope.clone());
        LocalScope { context, scope }
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
}

impl Drop for LocalScope<'_> {
    fn drop(&mut self) {
        self.context.local_scopes.borrow_mut().pop();
    }
}
