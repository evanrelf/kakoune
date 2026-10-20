//! STUB: to be replaced by the port of highlighters.hh/cc

use crate::exception::Result;
use crate::highlighter_group::HighlighterGroup;

pub fn setup_builtin_highlighters(_group: &HighlighterGroup) -> Result<()> {
    Ok(())
}

// STUB items until the full port lands
use crate::highlighter::HighlightPass;
use crate::option_types::{PrefixedListType, TimestampedList};
use std::rc::Rc;

pub fn register_highlighters() {}

thread_local! {
    static SHARED: Rc<HighlighterGroup> = Rc::new(HighlighterGroup::new(HighlightPass::All));
}

pub fn shared_highlighters() -> Rc<HighlighterGroup> {
    SHARED.with(|s| s.clone())
}

pub type LineAndSpecList = crate::option_types::PrefixedList<i32, Vec<u8>>;
pub type RangeAndStringList = TimestampedList<Vec<u8>>;
impl PrefixedListType for LineAndSpecList {
    fn type_name() -> Vec<u8> {
        b"line-specs".to_vec()
    }
}
impl PrefixedListType for RangeAndStringList {
    fn type_name() -> Vec<u8> {
        b"range-specs".to_vec()
    }
}
