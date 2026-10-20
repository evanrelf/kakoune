//! Port of highlighter.hh/cc
//!
//! A Highlighter is a function which mutates a DisplayBuffer in order to
//! change the visual representation of a file. It could be changing text
//! color, adding information text (line numbering for example) or replacing
//! buffer content (folding for example)

use crate::completion::Completions;
use crate::context::Context;
use crate::debug::write_to_debug_buffer;
use crate::display_buffer::DisplayBuffer;
use crate::exception::{Error, Result};
use crate::parameters_parser::ParameterDesc;
use crate::units::{BufferRange, ColumnCount, DisplayCoord, LineCount};
use indexmap::IndexMap;
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

crate::kak_flags! {
    pub struct HighlightPass: u32 {
        const Replace = 1 << 0;
        const Wrap = 1 << 1;
        const Move = 1 << 2;
        const Colorize = 1 << 3;
        const All = 0b1111;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplaySetup {
    pub first_line: LineCount,
    pub line_count: LineCount,
    pub first_column: ColumnCount,
    pub widget_columns: ColumnCount,
    /// Offset of line and columns that must remain visible around cursor
    pub scroll_offset: DisplayCoord,
}

pub struct HighlightContext<'a> {
    pub context: &'a Context,
    pub setup: &'a DisplaySetup,
    pub pass: HighlightPass,
    pub disabled_ids: &'a [Vec<u8>],
}

impl<'a> HighlightContext<'a> {
    pub fn with_disabled(&self, disabled_ids: &'a [Vec<u8>]) -> HighlightContext<'a> {
        HighlightContext { context: self.context, setup: self.setup, pass: self.pass, disabled_ids }
    }
    pub fn with_pass(&self, pass: HighlightPass) -> HighlightContext<'a> {
        HighlightContext { context: self.context, setup: self.setup, pass, disabled_ids: self.disabled_ids }
    }
}

fn no_children() -> Error {
    Error::runtime("this highlighter does not hold children")
}

pub trait Highlighter {
    fn passes(&self) -> HighlightPass;
    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) -> Result<()>;
    fn do_compute_display_setup(&self, _context: &HighlightContext, _setup: &mut DisplaySetup) -> Result<()> {
        Ok(())
    }
    fn has_children(&self) -> bool {
        false
    }
    fn get_child(&self, _path: &[u8]) -> Result<Rc<dyn Highlighter>> {
        Err(no_children())
    }
    fn add_child(&self, _name: Vec<u8>, _hl: Rc<dyn Highlighter>, _override: bool) -> Result<()> {
        Err(no_children())
    }
    fn remove_child(&self, _id: &[u8]) -> Result<()> {
        Err(no_children())
    }
    fn complete_child(&self, _path: &[u8], _cursor_pos: usize, _group: bool) -> Result<Completions> {
        Err(no_children())
    }
    fn fill_unique_ids(&self, _unique_ids: &mut Vec<Vec<u8>>) {}
}

/// Highlighter::highlight: run if the pass matches, errors go to the debug buffer
pub fn highlight(hl: &dyn Highlighter, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) {
    if context.pass.has(hl.passes()) {
        if let Err(error) = hl.do_highlight(context, display_buffer, range) {
            if error.is_runtime() {
                write_to_debug_buffer(&kformat!("Error while highlighting: {}", error.what()));
            }
        }
    }
}

/// Highlighter::compute_display_setup
pub fn compute_display_setup(hl: &dyn Highlighter, context: &HighlightContext, setup: &mut DisplaySetup) {
    if context.pass.has(hl.passes()) {
        let _ = hl.do_compute_display_setup(context, setup);
    }
}

pub type HighlighterParameters<'a> = &'a [Vec<u8>];
pub type HighlighterFactory = fn(HighlighterParameters, Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>>;

pub struct HighlighterDesc {
    pub docstring: &'static str,
    pub params: ParameterDesc,
}

pub struct HighlighterFactoryAndDescription {
    pub factory: HighlighterFactory,
    pub description: Rc<HighlighterDesc>,
}

pub struct HighlighterRegistry {
    map: RefCell<IndexMap<Vec<u8>, Rc<HighlighterFactoryAndDescription>>>,
}

thread_local! {
    static HIGHLIGHTER_REGISTRY: OnceCell<&'static HighlighterRegistry> = const { OnceCell::new() };
}

impl HighlighterRegistry {
    pub fn instance() -> &'static HighlighterRegistry {
        HIGHLIGHTER_REGISTRY.with(|r| {
            *r.get_or_init(|| Box::leak(Box::new(HighlighterRegistry { map: RefCell::new(IndexMap::new()) })))
        })
    }
    pub fn insert(&self, name: &str, factory: HighlighterFactory, description: HighlighterDesc) {
        self.map.borrow_mut().insert(
            name.as_bytes().to_vec(),
            Rc::new(HighlighterFactoryAndDescription { factory, description: Rc::new(description) }),
        );
    }
    pub fn get(&self, name: &[u8]) -> Option<Rc<HighlighterFactoryAndDescription>> {
        self.map.borrow().get(name).cloned()
    }
    pub fn names(&self) -> Vec<Vec<u8>> {
        self.map.borrow().keys().cloned().collect()
    }
}
