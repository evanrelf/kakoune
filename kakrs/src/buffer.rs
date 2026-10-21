//! Port of buffer.hh, buffer.inl.hh and buffer.cc

use crate::buffer_manager::BufferManager;
use crate::buffer_utils::{get_byte_to_column, get_column};
use crate::diff::{for_each_diff, DiffOp};
use crate::exception::{Error, Result};
use crate::file::{compact_path, file_exists, parse_filename, real_path, FsStatus};
use crate::hook_manager::Hook;
use crate::input_handler::InputHandler;
use crate::option_manager::{Opt, OptionWatcher};
use crate::option_types::EnumDesc;
use crate::scope::{GlobalScope, Scope};
use crate::selection::{max_column, Selection, SelectionList};
use crate::units::{BufferCoord, BufferCoordAndTarget, BufferRange, ByteCount, CharCount, ColumnCount, LineCount};
use crate::utf8;
use crate::value::ValueMap;
use std::cell::{Cell, Ref, RefCell};
use std::rc::{Rc, Weak};
use std::time::SystemTime;

/// Immutable shared line storage (C++ StringDataPtr). Lines always end with '\n'.
pub type StringDataPtr = Rc<[u8]>;

pub fn string_data(parts: &[&[u8]]) -> StringDataPtr {
    let mut v = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
    for p in parts {
        v.extend_from_slice(p);
    }
    Rc::from(v)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EolFormat {
    Lf,
    Crlf,
}
impl EnumDesc for EolFormat {
    fn desc() -> &'static [(Self, &'static str)] {
        &[(EolFormat::Lf, "lf"), (EolFormat::Crlf, "crlf")]
    }
}
crate::enum_option!(EolFormat);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ByteOrderMark {
    None,
    Utf8,
}
impl EnumDesc for ByteOrderMark {
    fn desc() -> &'static [(Self, &'static str)] {
        &[(ByteOrderMark::None, "none"), (ByteOrderMark::Utf8, "utf8")]
    }
}
crate::enum_option!(ByteOrderMark);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FinalEol {
    Present,
    Missing,
    IfNotEmpty,
}
impl EnumDesc for FinalEol {
    fn desc() -> &'static [(Self, &'static str)] {
        &[(FinalEol::Present, "present"), (FinalEol::Missing, "missing"), (FinalEol::IfNotEmpty, "ifnotempty")]
    }
}
crate::enum_option!(FinalEol);

crate::kak_flags! {
    pub struct BufferFlags: u32 {
        const None     = 0;
        const File     = 1 << 0;
        const New      = 1 << 1;
        const Fifo     = 1 << 2;
        const NoUndo   = 1 << 3;
        const NoHooks  = 1 << 4;
        const Debug    = 1 << 5;
        const ReadOnly = 1 << 6;
        const NoBufSetOption = 1 << 7;
        const Locked = 1 << 8;
    }
}

pub type HistoryId = usize;
pub const HISTORY_ID_FIRST: HistoryId = 0;
pub const HISTORY_ID_INVALID: HistoryId = usize::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeType {
    Insert,
    Erase,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Change {
    pub type_: ChangeType,
    pub begin: BufferCoord,
    pub end: BufferCoord,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModificationType {
    Insert,
    Erase,
}

/// A Modification holds a single atomic modification to Buffer
#[derive(Clone, Debug)]
pub struct Modification {
    pub type_: ModificationType,
    pub coord: BufferCoord,
    pub content: StringDataPtr,
}

impl Modification {
    pub fn inverse(&self) -> Modification {
        Modification {
            type_: match self.type_ {
                ModificationType::Insert => ModificationType::Erase,
                ModificationType::Erase => ModificationType::Insert,
            },
            coord: self.coord,
            content: self.content.clone(),
        }
    }
}

pub type UndoGroup = Vec<Modification>;

#[derive(Clone, Debug)]
pub struct HistoryNode {
    pub parent: HistoryId,
    pub redo_child: HistoryId,
    pub committed: SystemTime,
    pub undo_group: UndoGroup,
}

impl HistoryNode {
    fn new(parent: HistoryId) -> Self {
        HistoryNode { parent, redo_child: HISTORY_ID_INVALID, committed: SystemTime::now(), undo_group: Vec::new() }
    }
}

/// Text content and its history. Buffer methods never call out to user
/// code while this is borrowed, so it lives in a single RefCell.
struct Content {
    lines: Vec<StringDataPtr>,
    history: Vec<HistoryNode>,
    history_id: HistoryId,
    last_save_history_id: HistoryId,
    current_undo_group: UndoGroup,
    changes: Vec<Change>,
}

/// A Buffer is a in-memory representation of a file
pub struct Buffer {
    scope: Scope,
    content: RefCell<Content>,
    filename: RefCell<Vec<u8>>,
    display_name: RefCell<Vec<u8>>,
    flags: Cell<BufferFlags>,
    fs_status: Cell<FsStatus>,
    values: ValueMap,
    self_weak: Weak<Buffer>,
}

impl Buffer {
    pub fn new(
        name: Vec<u8>,
        flags: BufferFlags,
        lines: Vec<StringDataPtr>,
        bom: ByteOrderMark,
        eolformat: EolFormat,
        finaleol: FinalEol,
        fs_status: FsStatus,
    ) -> Result<Rc<Buffer>> {
        let (filename, display_name) = if flags.has(BufferFlags::File) {
            let filename = real_path(&parse_filename(&name, b""));
            let display = compact_path(&filename)?;
            (filename, display)
        } else {
            (Vec::new(), name)
        };
        debug_assert!(lines.iter().all(|l| l.last() == Some(&b'\n')));
        let line_count = lines.len() as LineCount;
        let buffer = Rc::new_cyclic(|w| Buffer {
            scope: Scope::new(GlobalScope::instance().scope()),
            content: RefCell::new(Content {
                lines,
                history: vec![HistoryNode::new(HISTORY_ID_INVALID)],
                history_id: HISTORY_ID_FIRST,
                last_save_history_id: HISTORY_ID_FIRST,
                current_undo_group: Vec::new(),
                changes: vec![Change {
                    type_: ChangeType::Insert,
                    begin: BufferCoord::new(0, 0),
                    end: BufferCoord::line(line_count),
                }],
            }),
            filename: RefCell::new(filename),
            display_name: RefCell::new(display_name),
            flags: Cell::new(flags | BufferFlags::NoUndo),
            fs_status: Cell::new(fs_status),
            values: ValueMap::default(),
            self_weak: w.clone(),
        });
        let options = buffer.options();
        options.get_local_option(b"eolformat")?.set(eolformat, true)?;
        options.get_local_option(b"finaleol")?.set(finaleol, true)?;
        options.get_local_option(b"BOM")?.set(bom, true)?;
        options.get_local_option(b"readonly")?.set(flags.has(BufferFlags::ReadOnly), true)?;
        // now we may begin to record undo data
        if !flags.has(BufferFlags::NoUndo) {
            buffer.remove_flags(BufferFlags::NoUndo);
        }
        Ok(buffer)
    }

    pub fn rc(&self) -> Rc<Buffer> {
        self.self_weak.upgrade().expect("buffer is alive")
    }

    pub fn weak(&self) -> Weak<Buffer> {
        self.self_weak.clone()
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn options(&self) -> &Rc<crate::option_manager::OptionManager> {
        self.scope.options()
    }
    pub fn hooks(&self) -> &Rc<crate::hook_manager::HookManager> {
        self.scope.hooks()
    }

    pub fn flags(&self) -> BufferFlags {
        self.flags.get()
    }
    pub fn set_flags(&self, flags: BufferFlags) {
        self.flags.set(flags)
    }
    pub fn add_flags(&self, flags: BufferFlags) {
        self.flags.set(self.flags.get() | flags)
    }
    pub fn remove_flags(&self, flags: BufferFlags) {
        let mut f = self.flags.get();
        f.remove(flags);
        self.flags.set(f)
    }

    pub fn values(&self) -> &ValueMap {
        &self.values
    }

    pub fn on_registered(&self) -> Result<()> {
        // Ignore debug buffer, as it can be created in many
        // corner cases (including while destroying the BufferManager
        // if a BufClose hooks triggers writing to it).
        if self.flags().has(BufferFlags::Debug) {
            return Ok(());
        }
        self.options().register_watcher(self.self_weak.clone() as Weak<dyn OptionWatcher>);

        if self.flags().has(BufferFlags::NoHooks) {
            self.on_option_changed(&self.options().opt("readonly"))?;
            return Ok(());
        }

        self.add_flags(BufferFlags::NoBufSetOption);
        let name = self.name();
        self.run_hook_in_own_context(Hook::BufCreate, &name, Vec::new())?;
        if self.flags().has(BufferFlags::File) {
            if self.flags().has(BufferFlags::New) {
                self.run_hook_in_own_context(Hook::BufNewFile, &name, Vec::new())?;
            } else {
                self.run_hook_in_own_context(Hook::BufOpenFile, &name, Vec::new())?;
            }
        }
        self.remove_flags(BufferFlags::NoBufSetOption);

        for option in self.options().flatten_options() {
            self.on_option_changed(&option)?;
        }
        Ok(())
    }

    pub fn on_unregistered(&self) -> Result<()> {
        if self.flags().has(BufferFlags::Debug) {
            return Ok(());
        }
        let me = self.self_weak.clone() as Weak<dyn OptionWatcher>;
        self.options().unregister_watcher(&me);
        let name = self.name();
        self.run_hook_in_own_context(Hook::BufClose, &name, Vec::new())
    }

    pub fn set_name(&self, name: Vec<u8>) -> Result<bool> {
        let other = BufferManager::instance().get_buffer_ifp(&name);
        if other.as_ref().is_none_or(|o| std::ptr::eq(o.as_ref(), self)) {
            if self.flags().has(BufferFlags::File) {
                let filename = real_path(&name);
                *self.display_name.borrow_mut() = compact_path(&filename)?;
                let exists = file_exists(&filename);
                *self.filename.borrow_mut() = filename;
                if !exists {
                    self.add_flags(BufferFlags::New);
                    self.content.borrow_mut().last_save_history_id = HISTORY_ID_INVALID;
                }
            } else {
                self.filename.borrow_mut().clear();
                *self.display_name.borrow_mut() = name;
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub fn throw_if_read_only(&self) -> Result<()> {
        if self.flags().has(BufferFlags::ReadOnly) {
            return Err(Error::runtime("buffer is read-only"));
        }
        Ok(())
    }

    pub fn update_display_name(&self) -> Result<()> {
        if self.flags().has(BufferFlags::File) {
            let d = compact_path(&self.filename.borrow())?;
            *self.display_name.borrow_mut() = d;
        }
        Ok(())
    }

    /// name(): filename for file buffers, display name otherwise
    pub fn name(&self) -> Vec<u8> {
        if self.flags().has(BufferFlags::File) {
            self.filename.borrow().clone()
        } else {
            self.display_name.borrow().clone()
        }
    }
    pub fn filename(&self) -> Vec<u8> {
        self.filename.borrow().clone()
    }
    pub fn display_name(&self) -> Vec<u8> {
        self.display_name.borrow().clone()
    }

    // Content accessors --------------------------------------------------

    /// Borrow the buffer lines, for iterating. Must not be held across
    /// buffer modifications.
    pub fn lines(&self) -> Ref<'_, [StringDataPtr]> {
        Ref::map(self.content.borrow(), |c| c.lines.as_slice())
    }

    /// operator[](LineCount)
    pub fn line(&self, line: LineCount) -> StringDataPtr {
        self.content.borrow().lines[line as usize].clone()
    }

    pub fn line_length(&self, line: LineCount) -> ByteCount {
        self.content.borrow().lines[line as usize].len() as ByteCount
    }

    pub fn line_count(&self) -> LineCount {
        self.content.borrow().lines.len() as LineCount
    }

    pub fn timestamp(&self) -> usize {
        self.content.borrow().changes.len()
    }

    pub fn changes_since(&self, timestamp: usize) -> Vec<Change> {
        let content = self.content.borrow();
        if timestamp < content.changes.len() {
            content.changes[timestamp..].to_vec()
        } else {
            Vec::new()
        }
    }

    /// Run f with the changes since timestamp, without copying them
    pub fn with_changes_since<R>(&self, timestamp: usize, f: impl FnOnce(&[Change]) -> R) -> R {
        let content = self.content.borrow();
        if timestamp < content.changes.len() {
            f(&content.changes[timestamp..])
        } else {
            f(&[])
        }
    }

    pub fn byte_at(&self, c: BufferCoord) -> u8 {
        self.content.borrow().lines[c.line as usize][c.column as usize]
    }

    pub fn next(&self, coord: BufferCoord) -> BufferCoord {
        if coord.column < self.line_length(coord.line) - 1 {
            BufferCoord::new(coord.line, coord.column + 1)
        } else {
            BufferCoord::new(coord.line + 1, 0)
        }
    }

    pub fn prev(&self, coord: BufferCoord) -> BufferCoord {
        if coord.column == 0 {
            BufferCoord::new(coord.line - 1, self.line_length(coord.line - 1) - 1)
        } else {
            BufferCoord::new(coord.line, coord.column - 1)
        }
    }

    pub fn distance_lines(lines: &[StringDataPtr], begin: BufferCoord, end: BufferCoord) -> ByteCount {
        if begin > end {
            return -Self::distance_lines(lines, end, begin);
        }
        if begin.line == end.line {
            return end.column - begin.column;
        }
        let mut res = lines[begin.line as usize].len() as ByteCount - begin.column;
        for l in begin.line + 1..end.line {
            res += lines[l as usize].len() as ByteCount;
        }
        res + end.column
    }

    pub fn distance(&self, begin: BufferCoord, end: BufferCoord) -> ByteCount {
        Self::distance_lines(&self.content.borrow().lines, begin, end)
    }

    pub fn advance_lines(lines: &[StringDataPtr], coord: BufferCoord, mut count: ByteCount) -> BufferCoord {
        if count > 0 {
            let mut line = coord.line;
            count += coord.column;
            while count >= lines[line as usize].len() as ByteCount {
                count -= lines[line as usize].len() as ByteCount;
                line += 1;
                if line as usize == lines.len() {
                    return BufferCoord::line(line);
                }
            }
            BufferCoord::new(line, count)
        } else if count < 0 {
            let mut line = coord.line;
            count += coord.column;
            while count < 0 {
                line -= 1;
                if line < 0 {
                    return BufferCoord::new(0, 0);
                }
                count += lines[line as usize].len() as ByteCount;
            }
            BufferCoord::new(line, count)
        } else {
            coord
        }
    }

    pub fn advance(&self, coord: BufferCoord, count: ByteCount) -> BufferCoord {
        Self::advance_lines(&self.content.borrow().lines, coord, count)
    }

    pub fn char_next(&self, coord: BufferCoord) -> BufferCoord {
        let line = self.line(coord.line);
        let len = line.len() as ByteCount;
        if coord.column < len - 1 {
            let column = coord.column + utf8::codepoint_size_of_byte(line[coord.column as usize]) as ByteCount;
            if column >= len {
                // Handle invalid utf-8
                return BufferCoord::new(coord.line + 1, 0);
            }
            return BufferCoord::new(coord.line, column);
        }
        BufferCoord::new(coord.line + 1, 0)
    }

    pub fn char_prev(&self, coord: BufferCoord) -> BufferCoord {
        if coord.column == 0 {
            return BufferCoord::new(coord.line - 1, self.line_length(coord.line - 1) - 1);
        }
        let line = self.line(coord.line);
        let column = utf8::character_start(&line, coord.column as usize - 1, 0) as ByteCount;
        BufferCoord::new(coord.line, column)
    }

    pub fn back_coord(&self) -> BufferCoord {
        let content = self.content.borrow();
        let last = content.lines.len() - 1;
        BufferCoord::new(last as LineCount, content.lines[last].len() as ByteCount - 1)
    }

    pub fn end_coord(&self) -> BufferCoord {
        BufferCoord::line(self.line_count())
    }

    pub fn is_valid(&self, c: BufferCoord) -> bool {
        let content = self.content.borrow();
        let line_count = content.lines.len() as LineCount;
        (c.line >= 0 && c.column >= 0)
            && ((c.line < line_count && c.column < content.lines[c.line as usize].len() as ByteCount)
                || (c.line == line_count && c.column == 0))
    }

    pub fn is_end(&self, c: BufferCoord) -> bool {
        c >= self.end_coord()
    }

    /// returns nearest valid coordinates from given ones
    pub fn clamp(&self, mut coord: BufferCoord) -> BufferCoord {
        let back = self.back_coord();
        if coord > back {
            coord = back;
        }
        let max_col = (self.line_length(coord.line) - 1).max(0);
        coord.column = coord.column.clamp(0, max_col);
        coord
    }

    /// offset_coord(BufferCoord, CharCount)
    pub fn offset_coord_chars(&self, coord: BufferCoord, offset: CharCount) -> BufferCoord {
        let lines = self.lines();
        let it = BufferIterator::new(&lines, coord);
        let mut res = it;
        if offset < 0 {
            let begin = BufferIterator::new(&lines, BufferCoord::new(0, 0));
            let mut d = offset;
            while d != 0 && res != begin {
                utf8::it_to_previous(&mut res, &begin);
                d += 1;
            }
        } else {
            let end = BufferIterator::new(&lines, self.end_coord_with(&lines)).prev_byte();
            let mut d = offset;
            if res != end {
                while res != end && d != 0 {
                    utf8::it_to_next(&mut res, &end);
                    d -= 1;
                }
            }
        }
        res.coord()
    }

    fn end_coord_with(&self, lines: &[StringDataPtr]) -> BufferCoord {
        BufferCoord::line(lines.len() as LineCount)
    }

    /// offset_coord(BufferCoordAndTarget, LineCount offset, ColumnCount tabstop)
    pub fn offset_coord_lines(
        &self,
        coord: BufferCoordAndTarget,
        offset: LineCount,
        tabstop: ColumnCount,
    ) -> BufferCoordAndTarget {
        let column = if coord.target == -1 { get_column(self, tabstop, coord.coord()) } else { coord.target };
        let avoid_eol = coord.target < max_column();
        let line = (coord.line + offset).clamp(0, self.line_count() - 1);
        let max_byte = self.line_length(line) - 1;
        let max_col = get_column(self, tabstop, BufferCoord::new(line, max_byte));
        let final_column = column.min(max_col - if avoid_eol { 1 } else { 0 }).max(0);
        BufferCoordAndTarget::new(
            BufferCoord::new(
                line,
                max_byte.min(get_byte_to_column(self, tabstop, crate::units::DisplayCoord::new(line, final_column))),
            ),
            column,
            -1,
        )
    }

    pub fn string(&self, begin: BufferCoord, end: BufferCoord) -> Vec<u8> {
        let content = self.content.borrow();
        let mut res = Vec::new();
        let last_line = end.line.min(content.lines.len() as LineCount - 1);
        for line in begin.line..=last_line {
            let l = &content.lines[line as usize];
            let start = if line == begin.line { begin.column as usize } else { 0 };
            let count = if line == end.line { end.column as isize - start as isize } else { -1 };
            res.extend_from_slice(crate::string_utils::substr(l, start, count));
        }
        res
    }

    /// substr on a single line
    pub fn substr(&self, begin: BufferCoord, end: BufferCoord) -> Vec<u8> {
        debug_assert_eq!(begin.line, end.line);
        let content = self.content.borrow();
        let l = &content.lines[begin.line as usize];
        crate::string_utils::substr(l, begin.column as usize, (end.column - begin.column) as isize).to_vec()
    }

    // Modifications ------------------------------------------------------

    pub fn reload(
        &self,
        lines: Vec<StringDataPtr>,
        bom: ByteOrderMark,
        eolformat: EolFormat,
        finaleol: FinalEol,
        fs_status: FsStatus,
    ) -> Result<()> {
        let record_undo = !self.flags().has(BufferFlags::NoUndo);

        self.commit_undo_group();

        {
            let mut guard = self.content.borrow_mut();
            let c = &mut *guard;
            if !record_undo {
                // Erase history about to be invalidated history
                c.history_id = HISTORY_ID_FIRST;
                c.last_save_history_id = HISTORY_ID_FIRST;
                c.history = vec![HistoryNode::new(HISTORY_ID_INVALID)];

                let lc = c.lines.len() as LineCount;
                c.changes.push(Change { type_: ChangeType::Erase, begin: BufferCoord::new(0, 0), end: BufferCoord::line(lc) });
                c.lines = lines;
                let lc = c.lines.len() as LineCount;
                c.changes.push(Change { type_: ChangeType::Insert, begin: BufferCoord::new(0, 0), end: BufferCoord::line(lc) });
            } else {
                let mut diff = Vec::new();
                {
                    let old = &c.lines;
                    for_each_diff(
                        old.len() as i32,
                        lines.len() as i32,
                        |i, j| old[i as usize] == lines[j as usize],
                        |op, len| diff.push((op, len)),
                    );
                }

                // Rebuild the line list while recording modifications
                let old_lines = std::mem::take(&mut c.lines);
                let mut new_list: Vec<StringDataPtr> = Vec::with_capacity(lines.len());
                let mut read_it = 0usize;
                let mut new_it = 0usize;
                for (op, len) in diff {
                    let len = len as usize;
                    match op {
                        DiffOp::Keep => {
                            new_list.extend_from_slice(&old_lines[read_it..read_it + len]);
                            read_it += len;
                            new_it += len;
                        }
                        DiffOp::Add => {
                            let cur_line = new_list.len() as LineCount;
                            for line in 0..len {
                                c.current_undo_group.push(Modification {
                                    type_: ModificationType::Insert,
                                    coord: BufferCoord::line(cur_line + line as LineCount),
                                    content: lines[new_it + line].clone(),
                                });
                            }
                            c.changes.push(Change {
                                type_: ChangeType::Insert,
                                begin: BufferCoord::line(cur_line),
                                end: BufferCoord::line(cur_line + len as LineCount),
                            });
                            new_list.extend_from_slice(&lines[new_it..new_it + len]);
                            new_it += len;
                        }
                        DiffOp::Remove => {
                            let cur_line = new_list.len() as LineCount;
                            for line in (0..len).rev() {
                                c.current_undo_group.push(Modification {
                                    type_: ModificationType::Erase,
                                    coord: BufferCoord::line(cur_line + line as LineCount),
                                    content: old_lines[read_it + line].clone(),
                                });
                            }
                            read_it += len;
                            c.changes.push(Change {
                                type_: ChangeType::Erase,
                                begin: BufferCoord::line(cur_line),
                                end: BufferCoord::line(cur_line + len as LineCount),
                            });
                        }
                    }
                }
                c.lines = new_list;
            }
        }

        self.commit_undo_group();

        let options = self.options();
        options.get_local_option(b"eolformat")?.set(eolformat, true)?;
        options.get_local_option(b"finaleol")?.set(finaleol, true)?;
        options.get_local_option(b"BOM")?.set(bom, true)?;

        {
            let mut c = self.content.borrow_mut();
            c.last_save_history_id = c.history_id;
        }
        self.fs_status.set(fs_status);
        Ok(())
    }

    pub fn commit_undo_group(&self) {
        if self.flags().has(BufferFlags::NoUndo) {
            return;
        }
        let mut guard = self.content.borrow_mut();
        let c = &mut *guard;
        if c.current_undo_group.is_empty() {
            return;
        }
        let id = c.history.len();
        let mut node = HistoryNode::new(c.history_id);
        node.undo_group = std::mem::take(&mut c.current_undo_group);
        c.history.push(node);
        let cur = c.history_id;
        c.history[cur].redo_child = id;
        c.history_id = id;
    }

    pub fn undo(&self, mut count: usize) -> Result<bool> {
        self.throw_if_read_only()?;
        self.commit_undo_group();
        let mut guard = self.content.borrow_mut();
        let c = &mut *guard;
        if c.history[c.history_id].parent == HISTORY_ID_INVALID {
            return Ok(false);
        }
        while count != 0 && c.history[c.history_id].parent != HISTORY_ID_INVALID {
            count -= 1;
            let group = c.history[c.history_id].undo_group.clone();
            for modification in group.iter().rev() {
                apply_modification(c, &modification.inverse());
            }
            c.history_id = c.history[c.history_id].parent;
        }
        Ok(true)
    }

    pub fn redo(&self, mut count: usize) -> Result<bool> {
        self.throw_if_read_only()?;
        let mut guard = self.content.borrow_mut();
        let c = &mut *guard;
        if c.history[c.history_id].redo_child == HISTORY_ID_INVALID || !c.current_undo_group.is_empty() {
            return Ok(false);
        }
        while count != 0 && c.history[c.history_id].redo_child != HISTORY_ID_INVALID {
            count -= 1;
            c.history_id = c.history[c.history_id].redo_child;
            let group = c.history[c.history_id].undo_group.clone();
            for modification in &group {
                apply_modification(c, modification);
            }
        }
        Ok(true)
    }

    pub fn move_to(&self, id: HistoryId) -> Result<bool> {
        if id >= self.next_history_id() {
            return Ok(false);
        }
        self.throw_if_read_only()?;
        self.commit_undo_group();

        let mut guard = self.content.borrow_mut();
        let c = &mut *guard;

        let depth_of = |c: &Content, mut id: HistoryId| {
            let mut depth = 0;
            while c.history[id].parent != HISTORY_ID_INVALID {
                id = c.history[id].parent;
                depth += 1;
            }
            depth
        };
        let (mut a, mut b) = (c.history_id, id);
        let (mut depth_a, mut depth_b) = (depth_of(c, a), depth_of(c, b));
        while depth_a > depth_b {
            a = c.history[a].parent;
            depth_a -= 1;
        }
        while depth_b > depth_a {
            b = c.history[b].parent;
            depth_b -= 1;
        }
        while a != b {
            a = c.history[a].parent;
            b = c.history[b].parent;
        }
        let parent = a;

        // undo up to common parent
        let mut it = c.history_id;
        while it != parent {
            let group = c.history[it].undo_group.clone();
            for modification in group.iter().rev() {
                apply_modification(c, &modification.inverse());
            }
            it = c.history[it].parent;
        }

        // apply from parent
        let mut path = Vec::new();
        let mut it = id;
        while it != parent {
            path.push(it);
            it = c.history[it].parent;
        }
        for &node_id in path.iter().rev() {
            let node_parent = c.history[node_id].parent;
            c.history[node_parent].redo_child = node_id;
            let group = c.history[node_id].undo_group.clone();
            for modification in &group {
                apply_modification(c, modification);
            }
        }

        c.history_id = id;
        Ok(true)
    }

    pub fn current_history_id(&self) -> HistoryId {
        self.content.borrow().history_id
    }

    pub fn next_history_id(&self) -> HistoryId {
        self.content.borrow().history.len()
    }

    pub fn history(&self) -> Ref<'_, [HistoryNode]> {
        Ref::map(self.content.borrow(), |c| c.history.as_slice())
    }

    pub fn current_undo_group(&self) -> Ref<'_, [Modification]> {
        Ref::map(self.content.borrow(), |c| c.current_undo_group.as_slice())
    }

    pub fn insert(&self, pos: BufferCoord, content: &[u8]) -> Result<BufferRange> {
        self.throw_if_read_only()?;
        debug_assert!(self.is_valid(pos));
        if content.is_empty() {
            return Ok(BufferRange::new(pos, pos));
        }
        let real_content: StringDataPtr = if self.is_end(pos) && content.last() != Some(&b'\n') {
            string_data(&[content, b"\n"])
        } else {
            Rc::from(content)
        };
        let no_undo = self.flags().has(BufferFlags::NoUndo);
        let mut c = self.content.borrow_mut();
        if !no_undo {
            c.current_undo_group.push(Modification {
                type_: ModificationType::Insert,
                coord: pos,
                content: real_content.clone(),
            });
        }
        Ok(do_insert(&mut c, pos, &real_content))
    }

    pub fn erase(&self, begin: BufferCoord, mut end: BufferCoord) -> Result<BufferCoord> {
        self.throw_if_read_only()?;
        debug_assert!(self.is_valid(begin) && self.is_valid(end));
        // do not erase last \n except if we erase from the start of a line, and normalize
        // end coord
        if self.is_end(end) {
            end = if begin.column != 0 || begin == BufferCoord::new(0, 0) { self.prev(end) } else { self.end_coord() };
        }
        if begin >= end {
            // use >= to handle case where begin is {line_count}
            return Ok(begin);
        }
        let no_undo = self.flags().has(BufferFlags::NoUndo);
        let erased = if no_undo { None } else { Some(self.string(begin, end)) };
        let mut c = self.content.borrow_mut();
        if let Some(erased) = erased {
            c.current_undo_group.push(Modification {
                type_: ModificationType::Erase,
                coord: begin,
                content: Rc::from(erased),
            });
        }
        Ok(do_erase(&mut c, begin, end))
    }

    pub fn replace(&self, begin: BufferCoord, end: BufferCoord, content: &[u8]) -> Result<BufferRange> {
        self.throw_if_read_only()?;
        if self.string(begin, end) == content {
            return Ok(BufferRange::new(begin, end));
        }
        if self.is_end(end) && content.last() == Some(&b'\n') {
            let erased = self.erase(begin, self.back_coord())?;
            let pos = self.insert(erased, &content[..content.len() - 1])?.begin;
            return Ok(BufferRange::new(pos, self.end_coord()));
        }
        let erased = self.erase(begin, end)?;
        self.insert(erased, content)
    }

    // returns true if the buffer is in a different state than
    // the last time it was saved
    pub fn is_modified(&self) -> bool {
        let c = self.content.borrow();
        self.flags().has(BufferFlags::File)
            && (c.history_id != c.last_save_history_id || !c.current_undo_group.is_empty())
    }

    // notify the buffer that it was saved in the current state
    pub fn notify_saved(&self, status: FsStatus) {
        if !self.content.borrow().current_undo_group.is_empty() {
            self.commit_undo_group();
        }
        self.remove_flags(BufferFlags::New);
        {
            let mut c = self.content.borrow_mut();
            c.last_save_history_id = c.history_id;
        }
        self.fs_status.set(status);
    }

    pub fn set_fs_status(&self, status: FsStatus) {
        debug_assert!(self.flags().has(BufferFlags::File));
        self.fs_status.set(status);
    }

    pub fn fs_status(&self) -> FsStatus {
        self.fs_status.get()
    }

    pub fn run_hook_in_own_context(&self, hook: Hook, param: &[u8], client_name: Vec<u8>) -> Result<()> {
        if self.flags().has(BufferFlags::NoHooks) {
            return Ok(());
        }
        let hook_handler = InputHandler::new(
            SelectionList::new(self.rc(), Selection::new(BufferCoord::default())),
            crate::context::ContextFlags::Draft,
            client_name,
        );
        self.hooks().run_hook(hook, param, hook_handler.context())
    }

    pub fn last_modification_coord(&self) -> Option<BufferCoord> {
        let c = self.content.borrow();
        if c.history_id == HISTORY_ID_FIRST {
            return None;
        }
        c.history[c.history_id].undo_group.last().map(|m| m.coord)
    }

    pub fn debug_description(&self) -> Vec<u8> {
        let c = self.content.borrow();
        let content_size: usize = c.lines.iter().map(|l| l.len()).sum();
        let additional_size: usize = c
            .history
            .iter()
            .map(|h| std::mem::size_of::<HistoryNode>() + h.undo_group.len() * std::mem::size_of::<Modification>())
            .sum::<usize>()
            + c.changes.len() * std::mem::size_of::<Change>();
        let flags = self.flags();
        let file = if flags.has(BufferFlags::File) { kformat!("File ({}) ", self.filename()) } else { Vec::new() };
        drop(c);
        kformat!(
            "{}\nFlags: {}{}{}{}{}{}{}{}\nUsed mem: content={} additional={}\n",
            self.display_name(),
            file,
            if flags.has(BufferFlags::New) { "New " } else { "" },
            if flags.has(BufferFlags::Fifo) { "Fifo " } else { "" },
            if flags.has(BufferFlags::NoUndo) { "NoUndo " } else { "" },
            if flags.has(BufferFlags::NoHooks) { "NoHooks " } else { "" },
            if flags.has(BufferFlags::Debug) { "Debug " } else { "" },
            if flags.has(BufferFlags::ReadOnly) { "ReadOnly " } else { "" },
            if self.is_modified() { "Modified " } else { "" },
            content_size,
            additional_size
        )
    }
}

impl OptionWatcher for Buffer {
    fn on_option_changed(&self, option: &Opt) -> Result<()> {
        if self.flags().has(BufferFlags::NoBufSetOption) {
            return Ok(());
        }
        if option.name() == b"readonly" {
            if *option.get::<bool>() {
                self.add_flags(BufferFlags::ReadOnly);
            } else {
                self.remove_flags(BufferFlags::ReadOnly);
            }
        }
        self.run_hook_in_own_context(
            Hook::BufSetOption,
            &kformat!("{}={}", option.name(), option.get_desc_string()),
            Vec::new(),
        )
    }
}

impl PartialEq for Buffer {
    fn eq(&self, other: &Buffer) -> bool {
        std::ptr::eq(self, other)
    }
}

fn do_insert(c: &mut Content, pos: BufferCoord, content: &[u8]) -> BufferRange {
    if content.is_empty() {
        return BufferRange::new(pos, pos);
    }
    let line_count = c.lines.len() as LineCount;
    let at_end = pos >= BufferCoord::line(line_count);
    let append_lines = at_end && (c.lines.is_empty() || c.lines.last().unwrap().last() == Some(&b'\n'));

    let (prefix, suffix): (Vec<u8>, Vec<u8>) = {
        let prefix = if append_lines { Vec::new() } else { c.lines[pos.line as usize][..pos.column as usize].to_vec() };
        let suffix = if at_end { Vec::new() } else { c.lines[pos.line as usize][pos.column as usize..].to_vec() };
        (prefix, suffix)
    };

    let mut new_lines: Vec<StringDataPtr> = Vec::new();
    let mut start = 0usize;
    for i in 0..content.len() {
        if content[i] == b'\n' {
            let line = &content[start..i + 1];
            new_lines.push(if start == 0 { string_data(&[&prefix, line]) } else { Rc::from(line) });
            start = i + 1;
        }
    }
    if start == 0 {
        new_lines.push(string_data(&[&prefix, content, &suffix]));
    } else if start != content.len() || !suffix.is_empty() {
        new_lines.push(string_data(&[&content[start..], &suffix]));
    }

    let new_count = new_lines.len();
    let mut line_idx = pos.line as usize;
    let mut iter = new_lines.into_iter();
    if !append_lines {
        // replace first line with new first line
        c.lines[line_idx] = iter.next().unwrap();
        line_idx += 1;
    }
    c.lines.splice(line_idx..line_idx, iter);

    let last_line = pos.line + new_count as LineCount - 1;
    let end = if at_end {
        BufferCoord::line(c.lines.len() as LineCount)
    } else {
        BufferCoord::new(last_line, c.lines[last_line as usize].len() as ByteCount - suffix.len() as ByteCount)
    };

    c.changes.push(Change { type_: ChangeType::Insert, begin: pos, end });
    BufferRange::new(pos, end)
}

fn do_erase(c: &mut Content, begin: BufferCoord, end: BufferCoord) -> BufferCoord {
    if begin == end {
        return begin;
    }
    let line_count = c.lines.len() as LineCount;
    let prefix = &c.lines[begin.line as usize][..begin.column as usize];
    let suffix: &[u8] = if end.line == line_count { &[] } else { &c.lines[end.line as usize][end.column as usize..] };
    let new_line = if !prefix.is_empty() || !suffix.is_empty() { Some(string_data(&[prefix, suffix])) } else { None };

    c.lines.drain(begin.line as usize..end.line as usize);
    c.changes.push(Change { type_: ChangeType::Erase, begin, end });

    if let Some(new_line) = new_line {
        c.lines[begin.line as usize] = new_line;
    }
    begin
}

fn apply_modification(c: &mut Content, modification: &Modification) {
    let content = &modification.content;
    let coord = modification.coord;
    match modification.type_ {
        ModificationType::Insert => {
            do_insert(c, coord, content);
        }
        ModificationType::Erase => {
            let end = Buffer::advance_lines(&c.lines, coord, content.len() as ByteCount);
            do_erase(c, coord, end);
        }
    }
}

/// A BufferIterator permits to iterate over the bytes of a buffer
#[derive(Clone, Copy)]
pub struct BufferIterator<'a> {
    lines: &'a [StringDataPtr],
    coord: BufferCoord,
}

impl<'a> BufferIterator<'a> {
    pub fn new(lines: &'a [StringDataPtr], coord: BufferCoord) -> Self {
        BufferIterator { lines, coord }
    }
    pub fn begin(lines: &'a [StringDataPtr]) -> Self {
        BufferIterator { lines, coord: BufferCoord::new(0, 0) }
    }
    pub fn end(lines: &'a [StringDataPtr]) -> Self {
        BufferIterator { lines, coord: BufferCoord::line(lines.len() as LineCount) }
    }
    pub fn coord(&self) -> BufferCoord {
        self.coord
    }
    pub fn lines(&self) -> &'a [StringDataPtr] {
        self.lines
    }
    pub fn at(&self, coord: BufferCoord) -> Self {
        BufferIterator { lines: self.lines, coord }
    }
    pub fn add(&self, n: ByteCount) -> Self {
        BufferIterator { lines: self.lines, coord: Buffer::advance_lines(self.lines, self.coord, n) }
    }
    pub fn sub(&self, n: ByteCount) -> Self {
        self.add(-n)
    }
    pub fn prev_byte(&self) -> Self {
        let mut r = *self;
        crate::utf8::ByteIt::dec(&mut r);
        r
    }
    pub fn next_byte(&self) -> Self {
        let mut r = *self;
        crate::utf8::ByteIt::inc(&mut r);
        r
    }
    /// byte distance from other to self
    pub fn distance_from(&self, other: &Self) -> ByteCount {
        Buffer::distance_lines(self.lines, other.coord, self.coord)
    }
}

impl PartialEq for BufferIterator<'_> {
    fn eq(&self, o: &Self) -> bool {
        self.coord == o.coord
    }
}

impl PartialOrd for BufferIterator<'_> {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        self.coord.partial_cmp(&o.coord)
    }
}

impl BufferIterator<'_> {
    /// Move forward line by line, `find` returning the offset of the target
    /// byte in the part of the line it is given, stopping at `end`
    #[inline]
    fn seek(&mut self, end: &Self, find: impl Fn(&[u8]) -> Option<usize>) {
        while self.coord < end.coord {
            let line = &self.lines[self.coord.line as usize];
            let stop = if self.coord.line == end.coord.line { end.coord.column as usize } else { line.len() };
            let start = self.coord.column as usize;
            if let Some(p) = find(&line[start..stop]) {
                self.coord.column = (start + p) as ByteCount;
                return;
            }
            if self.coord.line == end.coord.line {
                self.coord = end.coord;
                return;
            }
            self.coord = BufferCoord::line(self.coord.line + 1);
        }
    }
}

impl crate::utf8::ByteIt for BufferIterator<'_> {
    #[inline]
    fn get(&self) -> u8 {
        self.lines[self.coord.line as usize][self.coord.column as usize]
    }
    #[inline]
    fn inc(&mut self) {
        self.coord.column += 1;
        if self.coord.column as usize == self.lines[self.coord.line as usize].len() {
            self.coord.line += 1;
            self.coord.column = 0;
        }
    }
    #[inline]
    fn dec(&mut self) {
        if self.coord.column == 0 {
            self.coord.line -= 1;
            self.coord.column = self.lines[self.coord.line as usize].len() as ByteCount - 1;
        } else {
            self.coord.column -= 1;
        }
    }
    #[inline]
    fn seek_byte(&mut self, end: &Self, byte: u8) {
        self.seek(end, |s| memchr::memchr(byte, s));
    }
    #[inline]
    fn seek_in_map(&mut self, end: &Self, map: &[bool; 256]) {
        self.seek(end, |s| s.iter().position(|&b| map[b as usize]));
    }
}

impl std::fmt::Debug for BufferIterator<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BufferIterator({:?})", self.coord)
    }
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Buffer({})", String::from_utf8_lossy(&self.display_name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{buffer_content, coord_of, make_buffer, make_lines, setup_globals, text};
    use crate::utf8::ByteIt;
    use hegel::generators as gs;
    use hegel::{Generator as _, TestCase};

    fn new_buffer(lines: &[&str]) -> Rc<Buffer> {
        setup_globals();
        let lines = lines.iter().map(|l| Rc::from(l.as_bytes())).collect();
        Buffer::new(
            b"test".to_vec(),
            BufferFlags::None,
            lines,
            ByteOrderMark::None,
            EolFormat::Lf,
            FinalEol::Present,
            Default::default(),
        )
        .unwrap()
    }

    #[test]
    fn test_buffer() {
        let _empty_buffer = new_buffer(&["\n"]);

        let buffer = new_buffer(&["allo ?\n", "mais que fais la police\n", " hein ?\n", " youpi\n"]);
        assert_eq!(buffer.line_count(), 4);

        {
            let lines = buffer.lines();
            let mut pos = BufferIterator::begin(&lines);
            assert_eq!(pos.get(), b'a');
            pos = pos.add(6);
            assert_eq!(pos.coord(), BufferCoord::new(0, 6));
            pos.inc();
            assert_eq!(pos.coord(), BufferCoord::new(1, 0));
            pos = pos.prev_byte();
            assert_eq!(pos.coord(), BufferCoord::new(0, 6));
            pos = pos.add(1);
            assert_eq!(pos.coord(), BufferCoord::new(1, 0));
        }
        buffer.insert(BufferCoord::new(1, 0), b"tchou kanaky\n").unwrap();
        assert_eq!(buffer.line_count(), 5);
        {
            let lines = buffer.lines();
            let pos2 = BufferIterator::end(&lines).sub(9);
            assert_eq!(pos2.get(), b'?');
        }

        let s = buffer.string(BufferCoord::new(4, 1), buffer.next(BufferCoord::new(4, 5)));
        assert_eq!(s, b"youpi");

        // check insert at end behaviour: auto add end of line if necessary
        let pos = buffer.prev(buffer.end_coord());
        buffer.insert(pos, b"tchou").unwrap();
        assert_eq!(buffer.string(pos, buffer.end_coord()), b"tchou\n");

        let pos = buffer.prev(buffer.end_coord());
        buffer.insert(buffer.end_coord(), b"kanaky\n").unwrap();
        assert_eq!(buffer.string(buffer.next(pos), buffer.end_coord()), b"kanaky\n");

        buffer.commit_undo_group();
        buffer.erase(buffer.next(pos), buffer.end_coord()).unwrap();
        buffer.insert(buffer.end_coord(), b"mutch\n").unwrap();
        buffer.commit_undo_group();
        buffer.undo(1).unwrap();
        assert_eq!(buffer.string(buffer.advance(buffer.end_coord(), -7), buffer.end_coord()), b"kanaky\n");
        buffer.redo(1).unwrap();
        assert_eq!(buffer.string(buffer.advance(buffer.end_coord(), -6), buffer.end_coord()), b"mutch\n");
    }

    #[test]
    fn test_undo() {
        let buffer = new_buffer(&["allo ?\n", "mais que fais la police\n", " hein ?\n", " youpi\n"]);
        let pos = buffer.end_coord();
        buffer.insert(pos, b"kanaky\n").unwrap(); // change 1
        buffer.commit_undo_group();
        buffer.erase(pos, buffer.end_coord()).unwrap(); // change 2
        buffer.commit_undo_group();
        buffer.insert(BufferCoord::line(2), b"tchou\n").unwrap(); // change 3
        buffer.commit_undo_group();
        buffer.undo(1).unwrap();
        buffer.insert(BufferCoord::line(2), b"mutch\n").unwrap(); // change 4
        buffer.commit_undo_group();
        buffer.erase(BufferCoord::new(2, 1), BufferCoord::new(2, 5)).unwrap(); // change 5
        buffer.commit_undo_group();
        buffer.undo(2).unwrap();
        buffer.redo(2).unwrap();
        buffer.undo(1).unwrap();
        buffer.replace(BufferCoord::line(2), buffer.end_coord(), b"foo").unwrap(); // change 6
        buffer.commit_undo_group();

        let check = |expected: &[&str]| {
            assert_eq!(buffer.line_count() as usize, expected.len());
            for (i, line) in expected.iter().enumerate() {
                assert_eq!(&buffer.line(i as LineCount)[..], line.as_bytes());
            }
        };
        check(&["allo ?\n", "mais que fais la police\n", "foo\n"]);

        buffer.move_to(3).unwrap();
        check(&["allo ?\n", "mais que fais la police\n", "tchou\n", " hein ?\n", " youpi\n"]);

        buffer.move_to(4).unwrap();
        check(&["allo ?\n", "mais que fais la police\n", "mutch\n", " hein ?\n", " youpi\n"]);

        buffer.move_to(HISTORY_ID_FIRST).unwrap();
        check(&["allo ?\n", "mais que fais la police\n", " hein ?\n", " youpi\n"]);
        assert!(!buffer.undo(1).unwrap());

        buffer.move_to(5).unwrap();
        assert!(!buffer.redo(1).unwrap());

        buffer.move_to(6).unwrap();
        assert!(!buffer.redo(1).unwrap());
    }

    // Properties ------------------------------------------------------------

    fn check_lines_invariant(buffer: &Buffer) {
        let lines = buffer.lines();
        assert!(!lines.is_empty());
        for line in lines.iter() {
            assert_eq!(line.last(), Some(&b'\n'));
            assert!(!line[..line.len() - 1].contains(&b'\n'));
        }
    }

    #[derive(Debug, Clone)]
    enum Edit {
        Insert(usize, Vec<u8>),
        Erase(usize, usize),
        Replace(usize, usize, Vec<u8>),
        Commit,
    }

    fn edit() -> impl hegel::PrintableGenerator<Edit> {
        let offset = || gs::integers::<usize>().max_value(200);
        hegel::one_of!(
            gs::tuples!(offset(), text()).map(|(o, t)| Edit::Insert(o, t)),
            gs::tuples!(offset(), offset()).map(|(a, b)| Edit::Erase(a, b)),
            gs::tuples!(offset(), offset(), text()).map(|(a, b, t)| Edit::Replace(a, b, t)),
            gs::just(Edit::Commit),
        )
    }

    /// Apply `edit` to the buffer, with offsets wrapped into the current
    /// content, and to a byte model following the documented Buffer contract.
    /// Returns the updated model.
    fn apply_edit(buffer: &Buffer, model: &[u8], edit: &Edit) -> Vec<u8> {
        let len = model.len();
        let mut res = model.to_vec();
        match edit {
            Edit::Insert(o, t) => {
                let o = o % (len + 1);
                let pos = coord_of(model, o);
                let range = buffer.insert(pos, t).unwrap();
                if t.is_empty() {
                    assert_eq!(range, BufferRange::new(pos, pos));
                    return res;
                }
                // inserting at the end adds a missing final end of line
                let mut real = t.clone();
                if o == len && real.last() != Some(&b'\n') {
                    real.push(b'\n');
                }
                res.splice(o..o, real.iter().copied());
                assert_eq!(range, BufferRange::new(pos, coord_of(&res, o + real.len())));
            }
            Edit::Erase(a, b) => {
                let (a, b) = (a % (len + 1), b % (len + 1));
                let (a, b) = (a.min(b), a.max(b));
                let begin = coord_of(model, a);
                // the final end of line is only erased when erasing whole lines
                let b = if b == len && (begin.column != 0 || a == 0) { len - 1 } else { b };
                let pos = buffer.erase(begin, coord_of(model, b.max(a))).unwrap();
                assert_eq!(pos, begin);
                if a < b {
                    res.drain(a..b);
                }
            }
            Edit::Replace(a, b, t) => {
                let (a, b) = (a % (len + 1), b % (len + 1));
                let (a, b) = (a.min(b), a.max(b));
                let begin = coord_of(model, a);
                buffer.replace(begin, coord_of(model, b), t).unwrap();
                // replace is erase then insert, except that replacing up to
                // the end with a string ending in eol keeps a single final eol
                let mut erased = model.to_vec();
                let mut ins = t.clone();
                if b == len && t.last() == Some(&b'\n') {
                    if a < len - 1 {
                        erased.drain(a..len - 1);
                    }
                    ins.pop();
                } else {
                    let e = if b == len && (begin.column != 0 || a == 0) { len - 1 } else { b };
                    if a < e {
                        erased.drain(a..e);
                    }
                }
                if model[a..b] == t[..] {
                    return res;
                }
                let o = a.min(erased.len());
                if !ins.is_empty() && o == erased.len() && ins.last() != Some(&b'\n') {
                    ins.push(b'\n');
                }
                erased.splice(o..o, ins);
                res = erased;
            }
            Edit::Commit => buffer.commit_undo_group(),
        }
        res
    }

    #[hegel::test(test_cases = 500)]
    fn edits_follow_byte_model(tc: TestCase) {
        let initial = tc.draw(text());
        let buffer = make_buffer(&initial);
        let mut model = buffer_content(&buffer);
        for edit in tc.draw(gs::vecs(edit()).max_size(15)) {
            model = apply_edit(&buffer, &model, &edit);
            check_lines_invariant(&buffer);
            assert_eq!(buffer_content(&buffer), model, "after {edit:?}");
        }
    }

    #[hegel::test(test_cases = 500)]
    fn undo_all_then_redo_all_roundtrips(tc: TestCase) {
        let initial = tc.draw(text());
        let buffer = make_buffer(&initial);
        let mut model = buffer_content(&buffer);
        // content after each history node
        let mut states = vec![model.clone()];
        for edit in tc.draw(gs::vecs(edit()).max_size(15)) {
            model = apply_edit(&buffer, &model, &edit);
            if matches!(edit, Edit::Commit) {
                states.push(model.clone());
            }
        }
        buffer.commit_undo_group();
        states.push(model.clone());
        states.dedup();

        // every non empty undo group is one step
        while buffer.undo(1).unwrap() {
            check_lines_invariant(&buffer);
        }
        assert_eq!(buffer_content(&buffer), states[0]);
        assert_eq!(buffer.current_history_id(), HISTORY_ID_FIRST);
        while buffer.redo(1).unwrap() {
            check_lines_invariant(&buffer);
        }
        assert_eq!(buffer_content(&buffer), model);
    }

    #[hegel::test(test_cases = 500)]
    fn undo_steps_visit_committed_states(tc: TestCase) {
        let initial = tc.draw(text());
        let buffer = make_buffer(&initial);
        let mut model = buffer_content(&buffer);
        let mut states = vec![model.clone()];
        for edit in tc.draw(gs::vecs(edit()).max_size(15)) {
            let had_group = !buffer.current_undo_group().is_empty();
            model = apply_edit(&buffer, &model, &edit);
            if matches!(edit, Edit::Commit) && had_group {
                states.push(model.clone());
            }
        }
        if !buffer.current_undo_group().is_empty() {
            buffer.commit_undo_group();
            states.push(model.clone());
        }
        for expected in states.iter().rev().skip(1) {
            assert!(buffer.undo(1).unwrap());
            assert_eq!(&buffer_content(&buffer), expected);
        }
        assert!(!buffer.undo(1).unwrap());
    }

    #[hegel::test(test_cases = 1000)]
    fn coordinate_arithmetic_matches_offsets(tc: TestCase) {
        let initial = tc.draw(text());
        let buffer = make_buffer(&initial);
        let content = buffer_content(&buffer);
        let len = content.len();
        let a = tc.draw(gs::integers::<usize>().max_value(len));
        let b = tc.draw(gs::integers::<usize>().min_value(a).max_value(len));
        let (ca, cb) = (coord_of(&content, a), coord_of(&content, b));
        assert!(buffer.is_valid(ca) && buffer.is_valid(cb));
        assert_eq!(buffer.is_end(cb), b == len);
        assert_eq!(buffer.distance(ca, cb), (b - a) as ByteCount);
        assert_eq!(buffer.distance(cb, ca), -((b - a) as ByteCount));
        assert_eq!(buffer.advance(ca, (b - a) as ByteCount), cb);
        assert_eq!(buffer.advance(cb, -((b - a) as ByteCount)), ca);
        assert_eq!(buffer.string(ca, cb), &content[a..b]);
        if a < len {
            assert_eq!(buffer.next(ca), coord_of(&content, a + 1));
            assert_eq!(buffer.byte_at(ca), content[a]);
        }
        if a > 0 {
            assert_eq!(buffer.prev(ca), coord_of(&content, a - 1));
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn char_next_and_prev_step_over_codepoints(tc: TestCase) {
        let initial = tc.draw(text());
        let buffer = make_buffer(&initial);
        let content = buffer_content(&buffer);
        // text() is valid utf8, so codepoint starts are std char boundaries
        let s = std::str::from_utf8(&content).unwrap();
        let starts: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([content.len()]).collect();
        let i = tc.draw(gs::integers::<usize>().max_value(starts.len() - 2));
        let (here, next) = (coord_of(&content, starts[i]), coord_of(&content, starts[i + 1]));
        assert_eq!(buffer.char_next(here), next);
        if starts[i + 1] < content.len() {
            assert_eq!(buffer.char_prev(next), here);
        }
    }

    #[hegel::test(test_cases = 500)]
    fn changes_since_replays_to_same_line_count(tc: TestCase) {
        // Each change's line delta (insert adds end-begin lines, erase
        // removes them) sums to the total line count difference.
        let initial = tc.draw(text());
        let buffer = make_buffer(&initial);
        let timestamp = buffer.timestamp();
        let lines_before = buffer.line_count();
        let mut model = buffer_content(&buffer);
        for edit in tc.draw(gs::vecs(edit()).max_size(15)) {
            model = apply_edit(&buffer, &model, &edit);
        }
        let delta: LineCount = buffer
            .changes_since(timestamp)
            .iter()
            .map(|c| match c.type_ {
                ChangeType::Insert => c.end.line - c.begin.line,
                ChangeType::Erase => c.begin.line - c.end.line,
            })
            .sum();
        assert_eq!(lines_before + delta, buffer.line_count());
        assert_eq!(buffer.changes_since(buffer.timestamp()), vec![]);
    }

    #[hegel::test(test_cases = 1000)]
    fn seeks_match_stepping_byte_by_byte(tc: TestCase) {
        let buffer = make_buffer(&tc.draw(text()));
        let content = buffer_content(&buffer);
        let a = tc.draw(gs::integers::<usize>().max_value(content.len()));
        let b = tc.draw(gs::integers::<usize>().min_value(a).max_value(content.len()));
        let byte = tc.draw(gs::sampled_from(vec![b'a', b' ', b'\n', 0xC3, 0x82, b'z']));
        let mut map = [false; 256];
        for m in tc.draw(gs::vecs(gs::integers::<u8>()).max_size(4)) {
            map[m as usize] = true;
        }
        let lines = buffer.lines();
        let (begin, end) = (BufferIterator::new(&lines, coord_of(&content, a)), BufferIterator::new(&lines, coord_of(&content, b)));

        let mut expected = begin;
        while expected != end && expected.get() != byte {
            expected.inc();
        }
        let mut it = begin;
        it.seek_byte(&end, byte);
        assert_eq!(it.coord(), expected.coord());

        let mut expected = begin;
        while expected != end && !map[expected.get() as usize] {
            expected.inc();
        }
        let mut it = begin;
        it.seek_in_map(&end, &map);
        assert_eq!(it.coord(), expected.coord());
    }

    #[test]
    fn make_lines_adds_final_eol() {
        assert_eq!(make_lines(b"").len(), 1);
        assert_eq!(&make_lines(b"a")[0][..], b"a\n");
    }
}
