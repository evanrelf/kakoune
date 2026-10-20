//! Shared setup for unit tests that need the global singletons.
//!
//! Singletons are thread local and libtest runs each test on its own thread,
//! so every test gets a fresh set.

use crate::buffer::{Buffer, BufferFlags, ByteOrderMark, EolFormat, FinalEol, StringDataPtr};
use crate::buffer_manager::BufferManager;
use crate::register_manager::RegisterManager;
use crate::scope::GlobalScope;
use crate::units::{BufferCoord, ByteCount, LineCount};
use hegel::generators as gs;
use hegel::Generator as _;
use std::rc::Rc;

/// Create the global scope with every builtin option, plus the register and
/// buffer managers. Idempotent within a thread.
pub fn setup_globals() {
    if GlobalScope::has_instance() {
        return;
    }
    GlobalScope::create();
    RegisterManager::create();
    BufferManager::create();
    crate::kak_main::register_options().unwrap();
}

/// Split `content` into buffer lines, each ending with '\n'. A missing final
/// newline is added, as when loading a file.
pub fn make_lines(content: &[u8]) -> Vec<StringDataPtr> {
    let mut lines: Vec<StringDataPtr> = content
        .split_inclusive(|&c| c == b'\n')
        .map(|l| if l.last() == Some(&b'\n') { Rc::from(l) } else { Rc::from([l, b"\n"].concat()) })
        .collect();
    if lines.is_empty() {
        lines.push(Rc::from(&b"\n"[..]));
    }
    lines
}

/// A scratch buffer (not registered with the BufferManager) holding `content`
pub fn make_buffer(content: &[u8]) -> Rc<Buffer> {
    setup_globals();
    Buffer::new(
        b"test".to_vec(),
        BufferFlags::None,
        make_lines(content),
        ByteOrderMark::None,
        EolFormat::Lf,
        FinalEol::Present,
        Default::default(),
    )
    .unwrap()
}

/// The whole buffer content as one string
pub fn buffer_content(buffer: &Buffer) -> Vec<u8> {
    buffer.lines().concat()
}

/// Valid utf8 text made of a few ascii, multibyte and newline pieces
pub fn text() -> impl hegel::PrintableGenerator<Vec<u8>> {
    gs::vecs(gs::sampled_from(vec!["a", "b", " ", "\n", "\n", "é", "€", "\t"]))
        .max_size(30)
        .map(|v| v.concat().into_bytes())
}

/// Coordinate of byte offset `offset` in `content`, end_coord for its length
pub fn coord_of(content: &[u8], offset: usize) -> BufferCoord {
    let before = &content[..offset];
    let line = before.iter().filter(|&&c| c == b'\n').count();
    let line_start = before.iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1);
    BufferCoord::new(line as LineCount, (offset - line_start) as ByteCount)
}

/// Byte offset of `coord` in `content`
pub fn offset_of(content: &[u8], coord: BufferCoord) -> usize {
    let line_start: usize = content.split_inclusive(|&c| c == b'\n').take(coord.line as usize).map(|l| l.len()).sum();
    line_start + coord.column as usize
}
