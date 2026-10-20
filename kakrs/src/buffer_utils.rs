//! Port of buffer_utils.hh/cc

use crate::buffer::{
    string_data, Buffer, BufferFlags, BufferIterator, ByteOrderMark, EolFormat, FinalEol, HistoryId, HistoryNode,
    Modification, ModificationType, StringDataPtr, HISTORY_ID_INVALID,
};
use crate::buffer_manager::BufferManager;
use crate::changes::ForwardChangesTracker;
use crate::event_manager::{EventMode, FDWatcher, FdEvents};
use crate::exception::{Error, Result};
use crate::file::{
    create_file, file_access_error, file_exists, get_fs_status, last_error_str, murmur3, open_temp_file,
    parse_filename, real_path, st_mtim, BufferedWriter, FsStatus, MappedFile,
};
use crate::hook_manager::Hook;
use crate::option_types::EnumDesc;
use crate::selection::{selection_to_string, ColumnType, Selection};
use crate::unicode::{codepoint_width, is_word_default};
use crate::units::{BufferCoord, BufferRange, ByteCount, CharCount, ColumnCount, DisplayCoord, LineCount};
use crate::utf8;
use crate::value::{get_free_value_id, ValueId};
use std::cell::Cell;
use std::rc::{Rc, Weak};

pub fn content(buffer: &Buffer, range: &Selection) -> Vec<u8> {
    buffer.string(range.min(), buffer.char_next(range.max()))
}

pub fn erase(buffer: &Buffer, range: &Selection) -> Result<BufferCoord> {
    buffer.erase(range.min(), buffer.char_next(range.max()))
}

pub fn char_length_sel(buffer: &Buffer, range: &Selection) -> CharCount {
    char_length(buffer, range.min(), buffer.char_next(range.max()))
}

pub fn char_length(buffer: &Buffer, begin: BufferCoord, end: BufferCoord) -> CharCount {
    let lines = buffer.lines();
    utf8::it_distance(&BufferIterator::new(&lines, begin), &BufferIterator::new(&lines, end))
}

pub fn column_length(buffer: &Buffer, begin: BufferCoord, end: BufferCoord) -> ColumnCount {
    let lines = buffer.lines();
    utf8::it_column_distance(&BufferIterator::new(&lines, begin), &BufferIterator::new(&lines, end))
}

pub fn is_bol(coord: BufferCoord) -> bool {
    coord.column == 0
}

pub fn is_eol(buffer: &Buffer, coord: BufferCoord) -> bool {
    buffer.is_end(coord) || buffer.line_length(coord.line) == coord.column + 1
}

fn codepoint_before(lines: &[StringDataPtr], coord: BufferCoord) -> u32 {
    let it = BufferIterator::new(lines, coord);
    let begin = BufferIterator::begin(lines);
    let prev = utf8::it_previous(&it, &begin);
    utf8::it_codepoint(&prev, &it)
}

fn codepoint_at(lines: &[StringDataPtr], coord: BufferCoord) -> u32 {
    let it = BufferIterator::new(lines, coord);
    let end = BufferIterator::end(lines);
    utf8::it_codepoint(&it, &end)
}

pub fn is_bow(buffer: &Buffer, coord: BufferCoord) -> bool {
    let lines = buffer.lines();
    if coord == BufferCoord::new(0, 0) {
        return is_word_default(codepoint_at(&lines, coord));
    }
    !is_word_default(codepoint_before(&lines, coord)) && is_word_default(codepoint_at(&lines, coord))
}

pub fn is_eow(buffer: &Buffer, coord: BufferCoord) -> bool {
    if buffer.is_end(coord) || coord == BufferCoord::new(0, 0) {
        return false;
    }
    let lines = buffer.lines();
    is_word_default(codepoint_before(&lines, coord)) && !is_word_default(codepoint_at(&lines, coord))
}

pub fn replace_ranges(buffer: &Buffer, ranges: &mut [BufferRange], strings: &[Vec<u8>]) -> Result<()> {
    let mut changes_tracker = ForwardChangesTracker::default();
    let mut timestamp = buffer.timestamp();
    for (index, range) in ranges.iter_mut().enumerate() {
        range.begin = changes_tracker.get_new_coord_tolerant(range.begin);
        range.end = changes_tracker.get_new_coord_tolerant(range.end);
        let s: &[u8] = if strings.is_empty() { b"" } else { &strings[index.min(strings.len() - 1)] };
        *range = buffer.replace(range.begin, range.end, s)?;
        changes_tracker.update_from_buffer(buffer, &mut timestamp);
    }
    Ok(())
}

pub fn get_column(buffer: &Buffer, tabstop: ColumnCount, coord: BufferCoord) -> ColumnCount {
    let line = buffer.line(coord.line);
    let mut col = 0;
    let mut it = 0usize;
    while it != line.len() && coord.column > it as ByteCount {
        if line[it] == b'\t' {
            col = (col / tabstop + 1) * tabstop;
            it += 1;
        } else {
            col += codepoint_width(utf8::read_codepoint(&line, &mut it));
        }
    }
    col
}

/// column_length(buffer, tabstop, line)
pub fn column_length_line(buffer: &Buffer, tabstop: ColumnCount, line: LineCount) -> ColumnCount {
    get_column(buffer, tabstop, BufferCoord::new(line, i32::MAX))
}

pub fn get_byte_to_column(buffer: &Buffer, tabstop: ColumnCount, coord: DisplayCoord) -> ByteCount {
    let line = buffer.line(coord.line);
    let mut col = 0;
    let mut it = 0usize;
    while it != line.len() && coord.column > col {
        if line[it] == b'\t' {
            col = (col / tabstop + 1) * tabstop;
            if col > coord.column {
                // the target column was in the tab
                break;
            }
            it += 1;
        } else {
            let mut next = it;
            col += codepoint_width(utf8::read_codepoint(&line, &mut next));
            if col > coord.column {
                // the target column was in the char
                break;
            }
            it = next;
        }
    }
    it as ByteCount
}

pub fn parse_lines(data: &[u8], eolformat: EolFormat, expected_line_count: usize) -> Result<Vec<StringDataPtr>> {
    let mut lines = Vec::with_capacity(expected_line_count);
    let end = data.len();
    let mut pos = 0;
    while pos < end {
        if lines.len() >= i32::MAX as usize {
            return Err(Error::runtime("too many lines"));
        }
        let eol = memchr::memchr(b'\n', &data[pos..]).map(|p| p + pos).unwrap_or(end);
        if eol - pos >= i32::MAX as usize {
            return Err(Error::runtime("line is too long"));
        }
        let line_end = eol - if eolformat == EolFormat::Crlf && eol != end { 1 } else { 0 };
        lines.push(string_data(&[&data[pos..line_end.max(pos)], b"\n"]));
        pos = eol + 1;
    }
    if lines.is_empty() {
        lines.push(Rc::from(&b"\n"[..]));
    }
    Ok(lines)
}

pub fn create_buffer_from_string(name: Vec<u8>, flags: BufferFlags, data: &[u8]) -> Result<Rc<Buffer>> {
    BufferManager::instance().create_buffer(
        name,
        flags,
        parse_lines(data, EolFormat::Lf, 0)?,
        ByteOrderMark::None,
        EolFormat::Lf,
        FinalEol::Present,
        FsStatus::default(),
    )
}

pub struct ParsedFile {
    pub lines: Vec<StringDataPtr>,
    pub bom: ByteOrderMark,
    pub eolformat: EolFormat,
    pub finaleol: FinalEol,
    pub fs_status: FsStatus,
}

pub fn parse_file(filename: &[u8]) -> Result<ParsedFile> {
    let file = MappedFile::new(&parse_filename(filename, b""))?;
    let data = &file.data[..];
    let mut pos = 0;
    let end = data.len();
    let mut bom = ByteOrderMark::None;
    if data.len() >= 3 && &data[..3] == b"\xEF\xBB\xBF" {
        bom = ByteOrderMark::Utf8;
        pos += 3;
    }

    let mut line_count = 1;
    let mut has_crlf = false;
    let mut has_lf = false;
    for it in memchr::memchr_iter(b'\n', &data[pos..]) {
        let it = it + pos;
        if it != pos && data[it - 1] == b'\r' {
            has_crlf = true;
        } else {
            has_lf = true;
        }
        line_count += 1;
    }
    let eolformat = if has_crlf && !has_lf { EolFormat::Crlf } else { EolFormat::Lf };
    let finaleol = if pos == end {
        FinalEol::IfNotEmpty
    } else if data[end - 1] == b'\n' {
        FinalEol::Present
    } else {
        FinalEol::Missing
    };

    let fs_status = FsStatus { timestamp: st_mtim(&file.st), file_size: file.st.st_size, hash: murmur3(data) };
    Ok(ParsedFile { lines: parse_lines(&data[pos..], eolformat, line_count)?, bom, eolformat, finaleol, fs_status })
}

pub fn open_file_buffer(filename: &[u8], flags: BufferFlags) -> Result<Rc<Buffer>> {
    let parsed = parse_file(filename)?;
    BufferManager::instance().create_buffer(
        filename.to_vec(),
        BufferFlags::File | flags,
        parsed.lines,
        parsed.bom,
        parsed.eolformat,
        parsed.finaleol,
        parsed.fs_status,
    )
}

pub fn open_or_create_file_buffer(filename: &[u8], flags: BufferFlags) -> Result<Rc<Buffer>> {
    let path = parse_filename(filename, b"");
    if file_exists(&path) {
        return open_file_buffer(filename, BufferFlags::File | flags);
    }
    create_buffer_from_string(filename.to_vec(), BufferFlags::File | BufferFlags::New, b"")
}

pub fn reload_file_buffer(buffer: &Buffer) -> Result<()> {
    debug_assert!(buffer.flags().has(BufferFlags::File));
    let parsed = parse_file(&buffer.filename())?;
    buffer.reload(parsed.lines, parsed.bom, parsed.eolformat, parsed.finaleol, parsed.fs_status)?;
    buffer.remove_flags(BufferFlags::New);
    Ok(())
}

crate::kak_flags! {
    pub struct WriteFlags: u32 {
        const None  = 0;
        const Force = 0b01;
        const Sync  = 0b10;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WriteMethod {
    Overwrite,
    Replace,
}
impl EnumDesc for WriteMethod {
    fn desc() -> &'static [(Self, &'static str)] {
        &[(WriteMethod::Overwrite, "overwrite"), (WriteMethod::Replace, "replace")]
    }
}
crate::enum_option!(WriteMethod);

pub fn write_buffer_to_fd(buffer: &Buffer, fd: i32, finaleol: Option<FinalEol>) -> Result<()> {
    let finaleol = finaleol.unwrap_or_else(|| buffer.options().val::<FinalEol>("finaleol"));
    let eolformat = buffer.options().val::<EolFormat>("eolformat");
    let eoldata: &[u8] = if eolformat == EolFormat::Crlf { b"\r\n" } else { b"\n" };

    let write_eol_at_eof = finaleol == FinalEol::Present
        || (finaleol == FinalEol::IfNotEmpty && (buffer.line_count() != 1 || &*buffer.line(0) != b"\n"));

    let mut writer = BufferedWriter::new(fd, false);
    if buffer.options().val::<ByteOrderMark>("BOM") == ByteOrderMark::Utf8 {
        writer.write(b"\xEF\xBB\xBF")?;
    }

    let lines: Vec<StringDataPtr> = buffer.lines().to_vec();
    let count = lines.len();
    for (i, linedata) in lines.iter().enumerate() {
        // end of lines are written according to eolformat but always
        // stored as \n
        writer.write(&linedata[..linedata.len() - 1])?;
        if write_eol_at_eof || i != count - 1 {
            writer.write(eoldata)?;
        }
    }
    writer.flush()
}

pub fn write_buffer_to_file(
    buffer: &Buffer,
    filename: &[u8],
    method: WriteMethod,
    flags: WriteFlags,
    finaleol: Option<FinalEol>,
) -> Result<()> {
    let zfilename = crate::file::cstr(filename);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };

    let mut replace = method == WriteMethod::Replace;
    let mut force = flags.has(WriteFlags::Force);

    if (replace || force)
        && (unsafe { libc::stat(zfilename.as_ptr(), &mut st) } != 0 || (st.st_mode & libc::S_IFMT) != libc::S_IFREG)
    {
        force = false;
        replace = false;
    }

    if force && unsafe { libc::chmod(zfilename.as_ptr(), st.st_mode | libc::S_IWUSR) } < 0 {
        return Err(rt_err!("unable to change file permissions: {}", last_error_str()));
    }

    let (fd, temp_filename) = if replace { open_temp_file(filename) } else { (create_file(filename), Vec::new()) };
    if fd == -1 {
        let saved_errno = crate::file::errno();
        if force {
            unsafe { libc::chmod(zfilename.as_ptr(), st.st_mode) };
        }
        return Err(file_access_error(filename, &crate::file::strerror(saved_errno)));
    }

    {
        let res = write_buffer_to_fd(buffer, fd, finaleol);
        if res.is_ok() && flags.has(WriteFlags::Sync) {
            unsafe { libc::fsync(fd) };
        }
        unsafe { libc::close(fd) };
        res?;
    }

    let ztemp = crate::file::cstr(&temp_filename);
    if replace && unsafe { libc::geteuid() } == 0 && unsafe { libc::chown(ztemp.as_ptr(), st.st_uid, st.st_gid) } < 0 {
        return Err(rt_err!("unable to set replacement file ownership: {}", last_error_str()));
    }
    if replace && unsafe { libc::chmod(ztemp.as_ptr(), st.st_mode) } < 0 {
        return Err(rt_err!("unable to set replacement file permissions: {}", last_error_str()));
    }
    if force && !replace && unsafe { libc::chmod(zfilename.as_ptr(), st.st_mode) } < 0 {
        return Err(rt_err!("unable to restore file permissions: {}", last_error_str()));
    }

    if replace && unsafe { libc::rename(ztemp.as_ptr(), zfilename.as_ptr()) } != 0 {
        if force {
            unsafe { libc::chmod(zfilename.as_ptr(), st.st_mode) };
        }
        return Err(Error::runtime("replacing file failed"));
    }

    if buffer.flags().has(BufferFlags::File) && real_path(filename) == real_path(&buffer.filename()) {
        buffer.notify_saved(get_fs_status(&real_path(filename))?);
    }
    Ok(())
}

pub fn write_buffer_to_backup_file(buffer: &Buffer) {
    let (fd, _) = open_temp_file(&buffer.filename());
    if fd >= 0 {
        let _ = write_buffer_to_fd(buffer, fd, None);
        unsafe { libc::close(fd) };
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AutoScroll {
    No,
    NotInitially,
    Yes,
}

thread_local! {
    static FIFO_WATCHER_ID: ValueId = get_free_value_id();
}

pub fn fifo_watcher_id() -> ValueId {
    FIFO_WATCHER_ID.with(|id| *id)
}

/// Reads a fifo into a buffer, stored in the buffer values
struct FifoWatcher {
    watcher: FDWatcher,
    buffer: Weak<Buffer>,
}

struct FifoState {
    buffer: Weak<Buffer>,
    scroll: AutoScroll,
    had_trailing_newline: Cell<bool>,
}

impl Drop for FifoWatcher {
    fn drop(&mut self) {
        self.watcher.close_fd();
        if let Some(buffer) = self.buffer.upgrade() {
            let _ = buffer.run_hook_in_own_context(Hook::BufCloseFifo, b"", Vec::new());
            if !buffer.values().contains(fifo_watcher_id()) {
                buffer.remove_flags(BufferFlags::Fifo | BufferFlags::NoUndo);
            }
        }
    }
}

impl FifoState {
    fn read_fifo(&self, fifo: i32) -> Result<()> {
        let Some(buffer) = self.buffer.upgrade() else { return Ok(()) };
        const BUFFER_SIZE: usize = 2048;
        // if we read data slower than it arrives in the fifo, limiting the
        // iteration number allows us to go back go back to the event loop and
        // handle other events sources (such as input)
        const MAX_LOOP: usize = 1024;
        let mut closed = false;
        let mut loop_count = 0;
        let mut data = [0u8; BUFFER_SIZE];
        let mut insert_begin: Option<BufferCoord> = None;

        {
            let saved_flags = buffer.flags();
            buffer.remove_flags(BufferFlags::ReadOnly);
            let res = (|| -> Result<()> {
                loop {
                    let count = unsafe { libc::read(fifo, data.as_mut_ptr() as *mut libc::c_void, BUFFER_SIZE) };
                    if count <= 0 {
                        closed = true;
                        break;
                    }
                    let count = count as usize;

                    let mut pos = buffer.back_coord();
                    let is_first = pos == BufferCoord::new(0, 0);
                    if (self.scroll == AutoScroll::No && (is_first || self.had_trailing_newline.get()))
                        || (self.scroll == AutoScroll::NotInitially && is_first)
                    {
                        pos = buffer.next(pos);
                    }

                    let inserted_range = buffer.insert(pos, &data[..count])?;
                    if insert_begin.is_none() {
                        insert_begin = Some(inserted_range.begin);
                    }
                    pos = inserted_range.end;

                    let have_trailing_newline = data[count - 1] == b'\n';
                    if self.scroll != AutoScroll::Yes {
                        if is_first {
                            buffer.erase(BufferCoord::new(0, 0), buffer.next(BufferCoord::new(0, 0)))?;
                            if let Some(b) = insert_begin.as_mut() {
                                b.line -= 1;
                            }
                            if self.scroll == AutoScroll::NotInitially && have_trailing_newline {
                                buffer.insert(buffer.end_coord(), b"\n")?;
                            }
                        } else if self.scroll == AutoScroll::No
                            && !self.had_trailing_newline.get()
                            && have_trailing_newline
                        {
                            buffer.erase(buffer.prev(pos), pos)?;
                        }
                    }
                    self.had_trailing_newline.set(have_trailing_newline);

                    loop_count += 1;
                    if !(loop_count < MAX_LOOP && crate::file::fd_readable(fifo)) {
                        break;
                    }
                }
                Ok(())
            })();
            buffer.set_flags(saved_flags);
            res?;
        }

        if let Some(insert_begin) = insert_begin {
            let insert_back = if self.had_trailing_newline.get() && self.scroll == AutoScroll::No {
                buffer.back_coord()
            } else {
                buffer.prev(buffer.back_coord())
            };
            buffer.run_hook_in_own_context(
                Hook::BufReadFifo,
                &selection_to_string(ColumnType::Byte, &buffer, &Selection::from_range(insert_begin, insert_back), -1),
                Vec::new(),
            )?;
        }

        if closed {
            buffer.values().erase(fifo_watcher_id()); // will delete the watcher
        }
        Ok(())
    }
}

pub fn create_fifo_buffer(name: Vec<u8>, fd: i32, flags: BufferFlags, scroll: AutoScroll) -> Result<Rc<Buffer>> {
    let buffer_manager = BufferManager::instance();
    let buffer = match buffer_manager.get_buffer_ifp(&name) {
        Some(buffer) => {
            buffer.add_flags(BufferFlags::NoUndo | flags);
            buffer.values().clear();
            buffer.reload(
                vec![Rc::from(&b"\n"[..])],
                ByteOrderMark::None,
                EolFormat::Lf,
                FinalEol::Present,
                FsStatus::default(),
            )?;
            buffer_manager.make_latest(&buffer);
            buffer
        }
        None => buffer_manager.create_buffer(
            name,
            flags | BufferFlags::Fifo | BufferFlags::NoUndo,
            vec![Rc::from(&b"\n"[..])],
            ByteOrderMark::None,
            EolFormat::Lf,
            FinalEol::Present,
            FsStatus::default(),
        )?,
    };

    let state = Rc::new(FifoState { buffer: buffer.weak(), scroll, had_trailing_newline: Cell::new(false) });
    let watcher = FDWatcher::new(fd, FdEvents::Read, EventMode::Normal, move |w, _, mode| {
        if mode == EventMode::Normal {
            if let Err(err) = state.read_fifo(w.fd()) {
                crate::debug::write_to_debug_buffer(&kformat!("error reading fifo: {}", err.what()));
            }
        }
    });
    buffer.values().insert(fifo_watcher_id(), Rc::new(FifoWatcher { watcher, buffer: buffer.weak() }));
    buffer.set_flags(flags | BufferFlags::Fifo | BufferFlags::NoUndo);
    buffer.run_hook_in_own_context(Hook::BufOpenFifo, &buffer.name(), Vec::new())?;
    Ok(buffer)
}

fn history_id_to_string(id: HistoryId) -> Vec<u8> {
    if id == HISTORY_ID_INVALID { b"-".to_vec() } else { id.to_string().into_bytes() }
}

fn modification_as_string(modification: &Modification) -> Vec<u8> {
    kformat!(
        "{}{}.{}|{}",
        if modification.type_ == ModificationType::Insert { "+" } else { "-" },
        modification.coord.line,
        modification.coord.column,
        modification.content
    )
}

pub fn history_as_strings(history: &[HistoryNode]) -> Vec<Vec<u8>> {
    let mut res = Vec::new();
    for node in history {
        let seconds = node.committed.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        res.push(history_id_to_string(node.parent));
        res.push(seconds.to_string().into_bytes());
        res.push(history_id_to_string(node.redo_child));
        for modification in &node.undo_group {
            res.push(modification_as_string(modification));
        }
    }
    res
}

pub fn undo_group_as_strings(undo_group: &[Modification]) -> Vec<Vec<u8>> {
    undo_group.iter().map(modification_as_string).collect()
}

pub fn generate_buffer_name(pattern: &[u8]) -> Result<Vec<u8>> {
    let buffer_manager = BufferManager::instance();
    let mut i = 0;
    loop {
        let name = crate::format::try_format(pattern, &[i.to_string().as_bytes()])?;
        if buffer_manager.get_buffer_ifp(&name).is_none() {
            return Ok(name);
        }
        i += 1;
    }
}
