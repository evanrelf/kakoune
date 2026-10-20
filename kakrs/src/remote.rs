//! Port of remote.hh/cc
//!
//! The C++ `disconnected` exception maps to `ErrorKind::Disconnected`.
//!
//! Objects whose watcher callbacks need to reach back into them (RemoteUI,
//! RemoteClient, Server::Accepter) keep their state in an `Rc`, the callbacks
//! only hold a `Weak` reference to it.

use crate::buffer::BufferFlags;
use crate::buffer_utils::{create_fifo_buffer, generate_buffer_name, AutoScroll};
use crate::client_manager::ClientManager;
use crate::command_manager::{CommandManager, ShellContext};
use crate::context::Context;
use crate::debug::{write_to_debug_buffer, EnvVarMap};
use crate::display_buffer::{DisplayAtom, DisplayBuffer, DisplayLine, DisplayLineList};
use crate::event_manager::{EventMode, FDWatcher, FdEvents, WatcherRef};
use crate::exception::{Error, ErrorKind, Result};
use crate::face::{Attribute, Color, Face};
use crate::file::{cstr, fd_readable, fd_writable, file_exists, getenv, last_error_str, make_directory, tmpdir, write};
use crate::keys::{Key, Modifiers};
use crate::unicode::is_identifier;
use crate::units::{BufferCoord, ColumnCount, DisplayCoord};
use crate::user_interface::{
    InfoStyle, MenuStyle, OnKeyCallback, OnPasteCallback, StatusStyle, UiOptions, UserInterface,
};
use crate::utils::OnScopeEnd;
use indexmap::IndexMap;
use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::CStr;
use std::hash::Hash;
use std::rc::{Rc, Weak};
use std::sync::OnceLock;

fn disconnected(what: impl Into<Vec<u8>>) -> Error {
    Error::new(ErrorKind::Disconnected, what)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum MessageType {
    Unknown,
    Connect,
    Command,
    MenuShow,
    MenuSelect,
    MenuHide,
    InfoShow,
    InfoHide,
    Draw,
    DrawStatus,
    Refresh,
    SetOptions,
    Exit,
    Key,
    Paste,
}

impl MessageType {
    fn from_u8(val: u8) -> MessageType {
        use MessageType::*;
        const TYPES: [MessageType; 15] = [
            Unknown, Connect, Command, MenuShow, MenuSelect, MenuHide, InfoShow, InfoHide, Draw, DrawStatus,
            Refresh, SetOptions, Exit, Key, Paste,
        ];
        TYPES.get(val as usize).copied().unwrap_or(Unknown)
    }
}

type RemoteBuffer = Vec<u8>;

/// Serialization of a value, using the same layout as the C++ code (native
/// endianness, raw copies of trivially copyable types)
trait WriteField {
    fn write_field(&self, buffer: &mut RemoteBuffer);
}

/// Deserialization of a value, see WriteField
trait ReadField: Sized {
    fn read_field(reader: &mut MsgReader) -> Result<Self>;
}

macro_rules! impl_raw_field {
    ($($t:ty),*) => {
        $(
            impl WriteField for $t {
                fn write_field(&self, buffer: &mut RemoteBuffer) {
                    buffer.extend_from_slice(&self.to_ne_bytes());
                }
            }
            impl ReadField for $t {
                fn read_field(reader: &mut MsgReader) -> Result<Self> {
                    let bytes = reader.read_raw(size_of::<$t>())?;
                    Ok(<$t>::from_ne_bytes(bytes.try_into().unwrap()))
                }
            }
        )*
    };
}
// i32 covers int, ByteCount, ColumnCount and LineCount
impl_raw_field!(i32, u32);

impl WriteField for bool {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        buffer.push(*self as u8);
    }
}

impl ReadField for bool {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        Ok(reader.read_raw(1)?[0] != 0)
    }
}

// Strings are written as their ByteCount length followed by their content
impl WriteField for [u8] {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        (self.len() as i32).write_field(buffer);
        buffer.extend_from_slice(self);
    }
}

impl WriteField for Vec<u8> {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.as_slice().write_field(buffer);
    }
}

impl ReadField for Vec<u8> {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let length = i32::read_field(reader)?;
        if length > 0 {
            return Ok(reader.read_raw(length as usize)?.to_vec());
        }
        Ok(Vec::new())
    }
}

// Arrays are written as their uint32_t size followed by their elements
impl<T: WriteField> WriteField for [T] {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        (self.len() as u32).write_field(buffer);
        for val in self {
            val.write_field(buffer);
        }
    }
}

impl<T: WriteField> WriteField for Vec<T> {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.as_slice().write_field(buffer);
    }
}

impl<T: ReadField> ReadField for Vec<T> {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let size = u32::read_field(reader)?;
        // do not trust the received size for allocating
        let mut res = Vec::with_capacity((size as usize).min(reader.remaining()));
        for _ in 0..size {
            res.push(T::read_field(reader)?);
        }
        Ok(res)
    }
}

impl<K: WriteField, V: WriteField> WriteField for IndexMap<K, V> {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        (self.len() as u32).write_field(buffer);
        for (key, value) in self {
            key.write_field(buffer);
            value.write_field(buffer);
        }
    }
}

impl<K: ReadField + Hash + Eq, V: ReadField> ReadField for IndexMap<K, V> {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let size = u32::read_field(reader)?;
        let mut res = IndexMap::new();
        res.reserve((size as usize).min(reader.remaining()));
        for _ in 0..size {
            let key = K::read_field(reader)?;
            let val = V::read_field(reader)?;
            res.insert(key, val);
        }
        Ok(res)
    }
}

impl<T: WriteField> WriteField for Option<T> {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.is_some().write_field(buffer);
        if let Some(val) = self {
            val.write_field(buffer);
        }
    }
}

impl<T: ReadField> ReadField for Option<T> {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        if !bool::read_field(reader)? {
            return Ok(None);
        }
        Ok(Some(T::read_field(reader)?))
    }
}

impl WriteField for Color {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        buffer.push(self.a);
        if self.is_rgb() {
            buffer.extend_from_slice(&[self.r, self.g, self.b]);
        }
    }
}

impl ReadField for Color {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let mut res = Color::DEFAULT;
        res.a = reader.read_raw(1)?[0];
        if res.is_rgb() {
            let rgb = reader.read_raw(3)?;
            (res.r, res.g, res.b) = (rgb[0], rgb[1], rgb[2]);
        }
        Ok(res)
    }
}

// Face is trivially copyable in C++, and hence is written as a raw copy of
// the struct, in which colors take 4 bytes { color/alpha, r, g, b }
fn write_raw_color(color: &Color, buffer: &mut RemoteBuffer) {
    buffer.extend_from_slice(&[color.a, color.r, color.g, color.b]);
}

fn read_raw_color(reader: &mut MsgReader) -> Result<Color> {
    let bytes = reader.read_raw(4)?;
    Ok(Color { a: bytes[0], r: bytes[1], g: bytes[2], b: bytes[3] })
}

impl WriteField for Face {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        write_raw_color(&self.fg, buffer);
        write_raw_color(&self.bg, buffer);
        self.attributes.0.write_field(buffer);
        write_raw_color(&self.underline, buffer);
    }
}

impl ReadField for Face {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let fg = read_raw_color(reader)?;
        let bg = read_raw_color(reader)?;
        let attributes = Attribute(u32::read_field(reader)?);
        let underline = read_raw_color(reader)?;
        Ok(Face { fg, bg, attributes, underline })
    }
}

impl WriteField for BufferCoord {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.line.write_field(buffer);
        self.column.write_field(buffer);
    }
}

impl ReadField for BufferCoord {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let line = i32::read_field(reader)?;
        let column = i32::read_field(reader)?;
        Ok(BufferCoord::new(line, column))
    }
}

impl WriteField for DisplayCoord {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.line.write_field(buffer);
        self.column.write_field(buffer);
    }
}

impl ReadField for DisplayCoord {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let line = i32::read_field(reader)?;
        let column = i32::read_field(reader)?;
        Ok(DisplayCoord::new(line, column))
    }
}

impl WriteField for Key {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.modifiers.0.write_field(buffer);
        self.key.write_field(buffer);
    }
}

impl ReadField for Key {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let modifiers = Modifiers(u32::read_field(reader)?);
        let key = u32::read_field(reader)?;
        Ok(Key::with_mod(modifiers, key))
    }
}

// enum classes without explicit underlying type are int sized
macro_rules! impl_enum_field {
    ($t:ty, [$($variant:ident),*]) => {
        impl WriteField for $t {
            fn write_field(&self, buffer: &mut RemoteBuffer) {
                (*self as i32).write_field(buffer);
            }
        }
        impl ReadField for $t {
            fn read_field(reader: &mut MsgReader) -> Result<Self> {
                const VALUES: &[$t] = &[$(<$t>::$variant),*];
                let val = i32::read_field(reader)?;
                VALUES.get(val as usize).copied().ok_or_else(|| disconnected("invalid message received"))
            }
        }
    };
}
impl_enum_field!(MenuStyle, [Prompt, Search, Inline]);
impl_enum_field!(InfoStyle, [Prompt, Inline, InlineAbove, InlineBelow, MenuDoc, Modal]);
impl_enum_field!(StatusStyle, [Status, Command, Search, Prompt]);

impl WriteField for DisplayAtom {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.content().write_field(buffer);
        self.face.write_field(buffer);
    }
}

impl ReadField for DisplayAtom {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let content = Vec::<u8>::read_field(reader)?;
        Ok(DisplayAtom::text(content, Face::read_field(reader)?))
    }
}

impl WriteField for DisplayLine {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.atoms().write_field(buffer);
    }
}

impl ReadField for DisplayLine {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        Ok(DisplayLine::new(Vec::<DisplayAtom>::read_field(reader)?))
    }
}

impl WriteField for DisplayBuffer {
    fn write_field(&self, buffer: &mut RemoteBuffer) {
        self.lines().write_field(buffer);
    }
}

impl ReadField for DisplayBuffer {
    fn read_field(reader: &mut MsgReader) -> Result<Self> {
        let mut db = DisplayBuffer::default();
        *db.lines_mut() = Vec::<DisplayLine>::read_field(reader)?;
        Ok(db)
    }
}

/// Appends a message to a buffer, the message size gets patched in when
/// the writer is dropped
struct MsgWriter<'a> {
    buffer: &'a mut RemoteBuffer,
    start: usize,
}

impl<'a> MsgWriter<'a> {
    fn new(buffer: &'a mut RemoteBuffer, type_: MessageType) -> Self {
        let start = buffer.len();
        buffer.push(type_ as u8);
        0u32.write_field(buffer); // message size, to be patched on write
        MsgWriter { buffer, start }
    }

    fn write<T: WriteField + ?Sized>(&mut self, val: &T) -> &mut Self {
        val.write_field(self.buffer);
        self
    }
}

impl Drop for MsgWriter<'_> {
    fn drop(&mut self) {
        let count = (self.buffer.len() - self.start) as u32;
        let pos = self.start + size_of::<MessageType>();
        self.buffer[pos..pos + size_of::<u32>()].copy_from_slice(&count.to_ne_bytes());
    }
}

const HEADER_SIZE: u32 = (size_of::<MessageType>() + size_of::<u32>()) as u32;

// Large enough and suitably aligned to hold CMSG_SPACE(sizeof(int)) bytes
#[repr(C, align(8))]
struct FdBuf([u8; 64]);

fn cmsg_space_int() -> usize {
    unsafe { libc::CMSG_SPACE(size_of::<i32>() as u32) as usize }
}

fn cmsg_len_int() -> usize {
    unsafe { libc::CMSG_LEN(size_of::<i32>() as u32) as usize }
}

struct MsgReader {
    stream: Vec<u8>,
    ancillary_fd: Option<i32>,
    write_pos: u32,
    read_pos: u32,
}

impl MsgReader {
    fn new() -> Self {
        MsgReader { stream: Vec::new(), ancillary_fd: None, write_pos: 0, read_pos: HEADER_SIZE }
    }

    fn read_available(&mut self, sock: i32) -> Result<()> {
        if self.write_pos < HEADER_SIZE {
            self.stream.resize(HEADER_SIZE as usize, 0);
            self.read_from_socket(sock, (HEADER_SIZE - self.write_pos) as usize)?;
            if self.write_pos == HEADER_SIZE {
                if self.size() < HEADER_SIZE {
                    return Err(disconnected("invalid message received"));
                }
                self.stream.resize(self.size() as usize, 0);
            }
        } else {
            self.read_from_socket(sock, (self.size() - self.write_pos) as usize)?;
        }
        Ok(())
    }

    fn ready(&self) -> bool {
        self.write_pos >= HEADER_SIZE && self.write_pos == self.size()
    }

    fn size(&self) -> u32 {
        debug_assert!(self.write_pos >= HEADER_SIZE);
        let pos = size_of::<MessageType>();
        u32::from_ne_bytes(self.stream[pos..pos + size_of::<u32>()].try_into().unwrap())
    }

    fn type_(&self) -> MessageType {
        debug_assert!(self.write_pos >= HEADER_SIZE);
        MessageType::from_u8(self.stream[0])
    }

    /// C++ MsgReader::read(char* buffer, size_t size)
    fn read_raw(&mut self, size: usize) -> Result<&[u8]> {
        let begin = self.read_pos as usize;
        if begin + size > self.stream.len() {
            return Err(disconnected("tried to read after message end"));
        }
        self.read_pos += size as u32;
        Ok(&self.stream[begin..begin + size])
    }

    fn remaining(&self) -> usize {
        self.stream.len().saturating_sub(self.read_pos as usize)
    }

    fn read<T: ReadField>(&mut self) -> Result<T> {
        T::read_field(self)
    }

    fn ancillary_fd(&mut self) -> Option<i32> {
        self.ancillary_fd.take()
    }

    fn reset(&mut self) {
        self.stream.clear();
        self.write_pos = 0;
        self.read_pos = HEADER_SIZE;
        self.close_ancillary_fd();
    }

    fn close_ancillary_fd(&mut self) {
        if let Some(fd) = self.ancillary_fd.take() {
            unsafe { libc::close(fd) };
        }
    }

    fn read_from_socket(&mut self, sock: i32, size: usize) -> Result<()> {
        debug_assert!(self.write_pos as usize + size <= self.stream.len());
        let mut io = libc::iovec {
            iov_base: unsafe { self.stream.as_mut_ptr().add(self.write_pos as usize) } as *mut libc::c_void,
            iov_len: size,
        };
        let mut fdbuf = FdBuf([0; 64]);

        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut io;
        msg.msg_iovlen = 1;
        msg.msg_control = fdbuf.0.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_space_int() as _;

        let res = unsafe { libc::recvmsg(sock, &mut msg, 0) };
        if res <= 0 {
            return Err(disconnected(kformat!("socket read failed: {}", last_error_str())));
        }

        self.write_pos += res as u32;

        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            if !cmsg.is_null()
                && (*cmsg).cmsg_level == libc::SOL_SOCKET
                && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                && (*cmsg).cmsg_len as usize == cmsg_len_int()
            {
                self.close_ancillary_fd();
                let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const i32);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                self.ancillary_fd = Some(fd);
            }
        }
        Ok(())
    }
}

impl Drop for MsgReader {
    fn drop(&mut self) {
        self.close_ancillary_fd();
    }
}

/// Send as much of buffer as possible without blocking, returns true if
/// everything was sent. The ancillary fd, if any, is passed with SCM_RIGHTS.
fn send_data(fd: i32, buffer: &mut RemoteBuffer, ancillary_fd: Option<i32>) -> Result<bool> {
    while !buffer.is_empty() && fd_writable(fd) {
        let mut io = libc::iovec { iov_base: buffer.as_mut_ptr() as *mut libc::c_void, iov_len: buffer.len() };
        let mut fdbuf = FdBuf([0; 64]);

        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut io;
        msg.msg_iovlen = 1;
        if let Some(ancillary_fd) = ancillary_fd {
            msg.msg_control = fdbuf.0.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = cmsg_space_int() as _;

            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&msg);
                (*cmsg).cmsg_len = cmsg_len_int() as _;
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                std::ptr::write_unaligned(libc::CMSG_DATA(cmsg) as *mut i32, ancillary_fd);
            }
        }

        let res = unsafe { libc::sendmsg(fd, &msg, 0) };
        if res <= 0 {
            return Err(disconnected(kformat!("socket write failed: {}", last_error_str())));
        }
        buffer.drain(..res as usize);
    }
    Ok(buffer.is_empty())
}

fn add_events(watcher: &FDWatcher, events: FdEvents) {
    watcher.set_events(watcher.events() | events);
}

fn remove_events(watcher: &FDWatcher, events: FdEvents) {
    watcher.set_events(watcher.events() & !events);
}

// callbacks are reference counted so that they can be called without
// holding a borrow, they may reenter the ui
type SharedOnKey = Rc<dyn Fn(Key) -> Result<()>>;
type SharedOnPaste = Rc<dyn Fn(&[u8]) -> Result<()>>;

/// State shared between the RemoteUI, its socket watcher callback and the
/// client on_exit callback
struct RemoteUIState {
    socket_watcher: FDWatcher,
    reader: RefCell<MsgReader>,
    dimensions: Cell<DisplayCoord>,
    on_key: RefCell<Option<SharedOnKey>>,
    on_paste: RefCell<Option<SharedOnPaste>>,
    send_buffer: RefCell<RemoteBuffer>,
}

/// User interface running on the server, forwarding everything to a
/// RemoteClient through a socket
struct RemoteUI {
    state: Rc<RemoteUIState>,
}

impl RemoteUI {
    fn new(socket: i32, dimensions: DisplayCoord) -> RemoteUI {
        let state = Rc::new_cyclic(|weak: &Weak<RemoteUIState>| {
            let weak = weak.clone();
            RemoteUIState {
                socket_watcher: FDWatcher::new(
                    socket,
                    FdEvents::Read | FdEvents::Write,
                    EventMode::Urgent,
                    move |watcher: &WatcherRef, events, _| match weak.upgrade() {
                        Some(state) => state.handle_socket(watcher, events),
                        None => Ok(()),
                    },
                ),
                reader: RefCell::new(MsgReader::new()),
                dimensions: Cell::new(dimensions),
                on_key: RefCell::new(None),
                on_paste: RefCell::new(None),
                send_buffer: RefCell::new(Vec::new()),
            }
        });
        write_to_debug_buffer(&kformat!("remote client connected: {}", state.socket_watcher.fd()));
        RemoteUI { state }
    }
}

impl RemoteUIState {
    fn handle_socket(&self, watcher: &WatcherRef, events: FdEvents) -> Result<()> {
        let sock = watcher.fd();
        let res = (|| -> Result<()> {
            if events.has(FdEvents::Write) && send_data(sock, &mut self.send_buffer.borrow_mut(), None)? {
                remove_events(&self.socket_watcher, FdEvents::Write);
            }

            while events.has(FdEvents::Read) && fd_readable(sock) {
                let mut reader = self.reader.borrow_mut();
                reader.read_available(sock)?;

                if !reader.ready() {
                    continue;
                }

                match reader.type_() {
                    MessageType::Key => {
                        let key = reader.read::<Key>()?;
                        reader.reset();
                        drop(reader);
                        if key.modifiers == Modifiers::Resize {
                            self.dimensions.set(key.coord());
                        }
                        let on_key = self.on_key.borrow().clone();
                        if let Some(on_key) = on_key {
                            on_key(key)?;
                        }
                    }
                    MessageType::Paste => {
                        let content = reader.read::<Vec<u8>>()?;
                        reader.reset();
                        drop(reader);
                        let on_paste = self.on_paste.borrow().clone();
                        if let Some(on_paste) = on_paste {
                            on_paste(&content)?;
                        }
                    }
                    _ => {
                        watcher.close_fd();
                        return Ok(());
                    }
                }
            }
            Ok(())
        })();
        match res {
            Err(err) if err.kind == ErrorKind::Disconnected => {
                write_to_debug_buffer(&kformat!("Error while transfering remote messages: {}", err.what()));
                watcher.close_fd();
                Ok(())
            }
            res => res,
        }
    }

    fn send_message(&self, type_: MessageType, write: impl FnOnce(&mut MsgWriter)) {
        {
            let mut buffer = self.send_buffer.borrow_mut();
            let mut msg = MsgWriter::new(&mut buffer, type_);
            write(&mut msg);
        }
        add_events(&self.socket_watcher, FdEvents::Write);
    }

    fn exit(&self, status: i32) {
        self.send_message(MessageType::Exit, |msg| {
            msg.write(&status);
        });
    }
}

impl Drop for RemoteUI {
    fn drop(&mut self) {
        let watcher = &self.state.socket_watcher;
        // Try to send the remaining data if possible, as it might contain the desired exit status
        if watcher.fd() != -1 {
            let _ = send_data(watcher.fd(), &mut self.state.send_buffer.borrow_mut(), None);
        }

        write_to_debug_buffer(&kformat!("remote client disconnected: {}", watcher.fd()));
        watcher.close_fd();
    }
}

impl UserInterface for RemoteUI {
    fn is_ok(&self) -> bool {
        self.state.socket_watcher.fd() != -1
    }

    fn menu_show(&self, choices: &[DisplayLine], anchor: DisplayCoord, fg: Face, bg: Face, style: MenuStyle) {
        self.state.send_message(MessageType::MenuShow, |msg| {
            msg.write(choices).write(&anchor).write(&fg).write(&bg).write(&style);
        });
    }

    fn menu_select(&self, selected: i32) {
        self.state.send_message(MessageType::MenuSelect, |msg| {
            msg.write(&selected);
        });
    }

    fn menu_hide(&self) {
        self.state.send_message(MessageType::MenuHide, |_| {});
    }

    fn info_show(&self, title: &DisplayLine, content: &DisplayLineList, anchor: DisplayCoord, face: Face, style: InfoStyle) {
        self.state.send_message(MessageType::InfoShow, |msg| {
            msg.write(title).write(content).write(&anchor).write(&face).write(&style);
        });
    }

    fn info_hide(&self) {
        self.state.send_message(MessageType::InfoHide, |_| {});
    }

    fn draw(
        &self,
        display_buffer: &DisplayBuffer,
        cursor_pos: DisplayCoord,
        default_face: &Face,
        padding_face: &Face,
        widget_columns: ColumnCount,
    ) {
        self.state.send_message(MessageType::Draw, |msg| {
            msg.write(display_buffer).write(&cursor_pos).write(default_face).write(padding_face).write(&widget_columns);
        });
    }

    fn draw_status(
        &self,
        prompt: &DisplayLine,
        content: &DisplayLine,
        cursor_pos: ColumnCount,
        mode_line: &DisplayLine,
        default_face: &Face,
        style: StatusStyle,
    ) {
        self.state.send_message(MessageType::DrawStatus, |msg| {
            msg.write(prompt).write(content).write(&cursor_pos).write(mode_line).write(default_face).write(&style);
        });
    }

    fn refresh(&self, force: bool) {
        self.state.send_message(MessageType::Refresh, |msg| {
            msg.write(&force);
        });
    }

    fn dimensions(&self) -> DisplayCoord {
        self.state.dimensions.get()
    }

    fn set_on_key(&self, callback: OnKeyCallback) {
        *self.state.on_key.borrow_mut() = Some(Rc::from(callback));
    }

    fn set_on_paste(&self, callback: OnPasteCallback) {
        *self.state.on_paste.borrow_mut() = Some(Rc::from(callback));
    }

    fn set_ui_options(&self, options: &UiOptions) {
        self.state.send_message(MessageType::SetOptions, |msg| {
            msg.write(options);
        });
    }
}

pub fn get_user_name() -> Vec<u8> {
    unsafe {
        let pw = libc::getpwuid(libc::geteuid());
        if !pw.is_null() {
            return CStr::from_ptr((*pw).pw_name).to_bytes().to_vec();
        }
    }
    getenv("USER").unwrap_or_default()
}

pub fn session_directory() -> &'static [u8] {
    static SESSION_DIR: OnceLock<Vec<u8>> = OnceLock::new();
    SESSION_DIR.get_or_init(|| {
        let xdg_runtime_dir = getenv("XDG_RUNTIME_DIR").unwrap_or_default();
        if !xdg_runtime_dir.is_empty() {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let path = cstr(&xdg_runtime_dir);
            if unsafe { libc::stat(path.as_ptr(), &mut st) } == 0 && st.st_uid == unsafe { libc::geteuid() } {
                return kformat!("{}/kakoune", xdg_runtime_dir);
            } else {
                write_to_debug_buffer(b"XDG_RUNTIME_DIR does not exist or not owned by current user, using tmpdir");
            }
        }
        kformat!("{}/kakoune-{}", tmpdir(), get_user_name())
    })
}

fn sun_path_size() -> usize {
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    size_of_val(&addr.sun_path)
}

pub fn session_path(session: &[u8], assume_valid: bool) -> Result<Vec<u8>> {
    if !assume_valid && !session.iter().all(|&c| is_identifier(c as u32)) {
        throw!("invalid session name: '{}'", session);
    }
    let path = kformat!("{}/{}", session_directory(), session);
    if !assume_valid && path.len() + 1 > sun_path_size() {
        throw!("socket path too long: '{}'", path);
    }
    Ok(path)
}

/// returns the socket address and its path
fn session_addr(session: &[u8]) -> Result<(libc::sockaddr_un, Vec<u8>)> {
    let path = session_path(session, false)?;
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as _;
    for (dst, &src) in addr.sun_path.iter_mut().zip(path.iter()) {
        *dst = src as libc::c_char;
    }
    Ok((addr, path))
}

fn unix_socket() -> i32 {
    unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) }
}

fn connect_to(session: &[u8]) -> Result<i32> {
    let sock = unix_socket();
    unsafe { libc::fcntl(sock, libc::F_SETFD, libc::FD_CLOEXEC) };
    let (addr, path) = match session_addr(session) {
        Ok(res) => res,
        Err(err) => {
            unsafe { libc::close(sock) };
            return Err(err);
        }
    };
    let res = unsafe {
        libc::connect(sock, &addr as *const _ as *const libc::sockaddr, sun_path_size() as libc::socklen_t)
    };
    if res == -1 {
        unsafe { libc::close(sock) };
        return Err(disconnected(kformat!("connect to {} failed", path)));
    }
    Ok(sock)
}

pub fn check_session(session: &[u8]) -> Result<bool> {
    let sock = unix_socket();
    let _close_sock = OnScopeEnd::new(|| unsafe {
        libc::close(sock);
    });
    let (addr, _) = session_addr(session)?;
    Ok(unsafe { libc::connect(sock, &addr as *const _ as *const libc::sockaddr, sun_path_size() as libc::socklen_t) }
        != -1)
}

struct RemoteClientState {
    // declared first so that it is dropped before the ui, as in C++
    socket_watcher: OnceCell<FDWatcher>,
    reader: RefCell<MsgReader>,
    send_buffer: RefCell<RemoteBuffer>,
    exit_status: Cell<Option<i32>>,
    ui: Box<dyn UserInterface>,
}

/// A remote client handle communication between a client running on the server
/// and a user interface running on the local process.
pub struct RemoteClient {
    state: Rc<RemoteClientState>,
}

impl RemoteClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: &[u8],
        name: &[u8],
        ui: Box<dyn UserInterface>,
        pid: i32,
        env_vars: &EnvVarMap,
        init_command: &[u8],
        init_coord: Option<BufferCoord>,
        stdin_fd: Option<i32>,
    ) -> Result<RemoteClient> {
        let sock = connect_to(session)?;

        let mut send_buffer = RemoteBuffer::new();
        {
            let mut msg = MsgWriter::new(&mut send_buffer, MessageType::Connect);
            msg.write(&pid).write(name).write(init_command).write(&init_coord).write(&ui.dimensions()).write(env_vars);
        }
        send_data(sock, &mut send_buffer, stdin_fd)?;

        let state = Rc::new(RemoteClientState {
            socket_watcher: OnceCell::new(),
            reader: RefCell::new(MsgReader::new()),
            send_buffer: RefCell::new(send_buffer),
            exit_status: Cell::new(None),
            ui,
        });

        let weak = Rc::downgrade(&state);
        state.ui.set_on_key(Box::new(move |key: Key| {
            if let Some(state) = weak.upgrade() {
                state.send_message(MessageType::Key, |msg| {
                    msg.write(&key);
                });
            }
            Ok(())
        }));
        let weak = Rc::downgrade(&state);
        state.ui.set_on_paste(Box::new(move |content: &[u8]| {
            if let Some(state) = weak.upgrade() {
                state.send_message(MessageType::Paste, |msg| {
                    msg.write(content);
                });
            }
            Ok(())
        }));

        let weak = Rc::downgrade(&state);
        let watcher = FDWatcher::new(
            sock,
            FdEvents::Read | FdEvents::Write,
            EventMode::Urgent,
            move |watcher: &WatcherRef, events, _| match weak.upgrade() {
                Some(state) => state.handle_socket(watcher, events),
                None => Ok(()),
            },
        );
        let _ = state.socket_watcher.set(watcher);

        Ok(RemoteClient { state })
    }

    pub fn is_ui_ok(&self) -> bool {
        self.state.ui.is_ok()
    }

    pub fn exit_status(&self) -> Option<i32> {
        self.state.exit_status.get()
    }
}

impl RemoteClientState {
    fn send_message(&self, type_: MessageType, write: impl FnOnce(&mut MsgWriter)) {
        {
            let mut buffer = self.send_buffer.borrow_mut();
            let mut msg = MsgWriter::new(&mut buffer, type_);
            write(&mut msg);
        }
        if let Some(watcher) = self.socket_watcher.get() {
            add_events(watcher, FdEvents::Write);
        }
    }

    fn handle_socket(&self, watcher: &WatcherRef, events: FdEvents) -> Result<()> {
        let sock = watcher.fd();
        if events.has(FdEvents::Write)
            && send_data(sock, &mut self.send_buffer.borrow_mut(), None)?
            && let Some(watcher) = self.socket_watcher.get()
        {
            remove_events(watcher, FdEvents::Write);
        }

        // The ui methods might process urgent events, which can reenter this
        // callback while the reader is borrowed, it then counts as ready so
        // that no further message is read until the current one is handled.
        let reader_ready = || self.reader.try_borrow().map_or(true, |reader| reader.ready());

        while events.has(FdEvents::Read) && !reader_ready() && fd_readable(sock) {
            let mut reader = self.reader.borrow_mut();
            reader.read_available(sock)?;

            if !reader.ready() {
                continue;
            }

            let res = self.handle_message(&mut reader, watcher);
            reader.reset();
            if res? {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Dispatch a received message to the ui, arguments are read in order.
    /// Returns true if the connection got closed.
    fn handle_message(&self, reader: &mut MsgReader, watcher: &WatcherRef) -> Result<bool> {
        let ui = &*self.ui;
        match reader.type_() {
            MessageType::MenuShow => {
                let choices = reader.read::<Vec<DisplayLine>>()?;
                let anchor = reader.read::<DisplayCoord>()?;
                let fg = reader.read::<Face>()?;
                let bg = reader.read::<Face>()?;
                let style = reader.read::<MenuStyle>()?;
                ui.menu_show(&choices, anchor, fg, bg, style);
            }
            MessageType::MenuSelect => ui.menu_select(reader.read::<i32>()?),
            MessageType::MenuHide => ui.menu_hide(),
            MessageType::InfoShow => {
                let title = reader.read::<DisplayLine>()?;
                let content = reader.read::<DisplayLineList>()?;
                let anchor = reader.read::<DisplayCoord>()?;
                let face = reader.read::<Face>()?;
                let style = reader.read::<InfoStyle>()?;
                ui.info_show(&title, &content, anchor, face, style);
            }
            MessageType::InfoHide => ui.info_hide(),
            MessageType::Draw => {
                let display_buffer = reader.read::<DisplayBuffer>()?;
                let cursor_pos = reader.read::<DisplayCoord>()?;
                let default_face = reader.read::<Face>()?;
                let padding_face = reader.read::<Face>()?;
                let widget_columns = reader.read::<ColumnCount>()?;
                ui.draw(&display_buffer, cursor_pos, &default_face, &padding_face, widget_columns);
            }
            MessageType::DrawStatus => {
                let prompt = reader.read::<DisplayLine>()?;
                let content = reader.read::<DisplayLine>()?;
                let cursor_pos = reader.read::<ColumnCount>()?;
                let mode_line = reader.read::<DisplayLine>()?;
                let default_face = reader.read::<Face>()?;
                let style = reader.read::<StatusStyle>()?;
                ui.draw_status(&prompt, &content, cursor_pos, &mode_line, &default_face, style);
            }
            MessageType::Refresh => ui.refresh(reader.read::<bool>()?),
            MessageType::SetOptions => ui.set_ui_options(&reader.read::<UiOptions>()?),
            MessageType::Exit => {
                self.exit_status.set(Some(reader.read::<i32>()?));
                watcher.close_fd();
                return Ok(true);
            }
            _ => debug_assert!(false, "unexpected message received"),
        }
        Ok(false)
    }
}

pub fn send_command(session: &[u8], command: &[u8]) -> Result<()> {
    let sock = connect_to(session)?;
    let _close_sock = OnScopeEnd::new(|| unsafe {
        libc::close(sock);
    });
    let mut buffer = RemoteBuffer::new();
    {
        let mut msg = MsgWriter::new(&mut buffer, MessageType::Command);
        msg.write(command);
    }
    write(sock, &buffer)
}

// A client accepter handle a connection until it closes or a nul byte is
// recieved. Everything recieved before is considered to be a command.
//
// * When a nul byte is recieved, the socket is handed to a new Client along
//   with the command.
// * When the connection is closed, the command is run in an empty context.
struct Accepter {
    socket_watcher: FDWatcher,
    reader: RefCell<MsgReader>,
}

impl Accepter {
    fn new(socket: i32) -> Rc<Accepter> {
        Rc::new_cyclic(|weak: &Weak<Accepter>| {
            let weak = weak.clone();
            Accepter {
                socket_watcher: FDWatcher::new(socket, FdEvents::Read, EventMode::Normal, move |_, _, mode| {
                    match weak.upgrade() {
                        Some(accepter) => accepter.handle_available_input(mode),
                        None => Ok(()),
                    }
                }),
                reader: RefCell::new(MsgReader::new()),
            }
        })
    }

    fn handle_available_input(self: &Rc<Self>, mode: EventMode) -> Result<()> {
        let sock = self.socket_watcher.fd();
        let res = (|| -> Result<()> {
            let mut reader = self.reader.borrow_mut();
            while !reader.ready() && fd_readable(sock) {
                reader.read_available(sock)?;
            }

            if mode != EventMode::Normal || !reader.ready() {
                return Ok(());
            }

            match reader.type_() {
                MessageType::Connect => {
                    let pid = reader.read::<i32>()?;
                    let name = reader.read::<Vec<u8>>()?;
                    let init_cmds = reader.read::<Vec<u8>>()?;
                    let init_coord = reader.read::<Option<BufferCoord>>()?;
                    let dimensions = reader.read::<DisplayCoord>()?;
                    let env_vars = reader.read::<EnvVarMap>()?;
                    let stdin_fd = reader.ancillary_fd();
                    drop(reader);

                    if let Some(stdin_fd) = stdin_fd {
                        create_fifo_buffer(
                            generate_buffer_name(b"*stdin-{}*")?,
                            stdin_fd,
                            BufferFlags::None,
                            AutoScroll::NotInitially,
                        )?;
                    }
                    let ui = RemoteUI::new(sock, dimensions);
                    let ui_state = Rc::downgrade(&ui.state);
                    ClientManager::instance().create_client(
                        Box::new(ui),
                        pid,
                        name,
                        env_vars,
                        &init_cmds,
                        b"",
                        init_coord,
                        Box::new(move |status: i32| {
                            if let Some(ui_state) = ui_state.upgrade() {
                                ui_state.exit(status);
                            }
                        }),
                    )?;

                    Server::instance().remove_accepter(self);
                }
                MessageType::Command => {
                    let command = reader.read::<Vec<u8>>()?;
                    drop(reader);
                    if !command.is_empty() {
                        let context = Context::new_empty();
                        match CommandManager::instance().execute(&command, &context, &ShellContext::default()) {
                            Ok(()) => {}
                            Err(e) if e.is_runtime() => {
                                write_to_debug_buffer(&kformat!("error running command '{}': {}", command, e.what()));
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    unsafe { libc::close(sock) };
                    Server::instance().remove_accepter(self);
                }
                _ => {
                    drop(reader);
                    write_to_debug_buffer(b"invalid introduction message received");
                    unsafe { libc::close(sock) };
                    Server::instance().remove_accepter(self);
                }
            }
            Ok(())
        })();
        match res {
            Err(err) if err.kind == ErrorKind::Disconnected => {
                write_to_debug_buffer(&kformat!("accepting connection failed: {}", err.what()));
                unsafe { libc::close(sock) };
                Server::instance().remove_accepter(self);
                Ok(())
            }
            res => res,
        }
    }
}

pub struct Server {
    session: RefCell<Vec<u8>>,
    is_daemon: Cell<bool>,
    listener: RefCell<Option<FDWatcher>>,
    accepters: RefCell<Vec<Rc<Accepter>>>,
}

thread_local! {
    static SERVER: Cell<Option<&'static Server>> = const { Cell::new(None) };
}

impl Server {
    /// Create the server and register it as the singleton instance.
    /// `Server::destroy` must be called where the C++ Server would be destroyed.
    pub fn create(session_name: Vec<u8>, is_daemon: bool) -> Result<&'static Server> {
        debug_assert!(!Self::has_instance());
        let server: &'static Server = Box::leak(Box::new(Server {
            session: RefCell::new(session_name),
            is_daemon: Cell::new(is_daemon),
            listener: RefCell::new(None),
            accepters: RefCell::new(Vec::new()),
        }));

        let listen_sock = unix_socket();
        unsafe { libc::fcntl(listen_sock, libc::F_SETFD, libc::FD_CLOEXEC) };
        let (addr, path) = session_addr(&server.session.borrow())?;

        make_directory(session_directory(), 0o711)?;

        // Do not give any access to the socket to other users by default
        let old_mask = unsafe { libc::umask(0o077) };
        let _restore_mask = OnScopeEnd::new(|| unsafe {
            libc::umask(old_mask);
        });

        if unsafe {
            libc::bind(
                listen_sock,
                &addr as *const _ as *const libc::sockaddr,
                size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        } == -1
        {
            throw!("unable to bind listen socket '{}': {}", path, last_error_str());
        }

        if unsafe { libc::listen(listen_sock, 4) } == -1 {
            throw!("unable to listen on socket '{}': {}", path, last_error_str());
        }

        let accepter = move |watcher: &WatcherRef, _, _| -> Result<()> {
            let mut client_addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            let mut client_addr_len = size_of::<libc::sockaddr_un>() as libc::socklen_t;
            let sock = unsafe {
                libc::accept(watcher.fd(), &mut client_addr as *mut _ as *mut libc::sockaddr, &mut client_addr_len)
            };
            if sock == -1 {
                throw!("accept failed");
            }
            unsafe { libc::fcntl(sock, libc::F_SETFD, libc::FD_CLOEXEC) };

            server.accepters.borrow_mut().push(Accepter::new(sock));
            Ok(())
        };
        *server.listener.borrow_mut() = Some(FDWatcher::new(listen_sock, FdEvents::Read, EventMode::Urgent, accepter));

        SERVER.with(|s| s.set(Some(server)));
        Ok(server)
    }

    pub fn instance() -> &'static Server {
        SERVER.with(|s| s.get()).expect("Server not created")
    }

    pub fn has_instance() -> bool {
        SERVER.with(|s| s.get().is_some())
    }

    /// Port of ~Server: close the session if still open and unregister the instance
    pub fn destroy() {
        let Some(server) = SERVER.with(|s| s.take()) else { return };
        if server.listener.borrow().is_some() {
            server.close_session(true);
        }
        let accepters = std::mem::take(&mut *server.accepters.borrow_mut());
        drop(accepters);
    }

    pub fn session(&self) -> Vec<u8> {
        self.session.borrow().clone()
    }

    pub fn rename_session(&self, name: &[u8]) -> Result<bool> {
        let old_socket_file = session_path(&self.session.borrow(), true)?;
        let new_socket_file = session_path(name, false)?;

        if file_exists(&new_socket_file) {
            return Ok(false);
        }

        let (old, new) = (cstr(&old_socket_file), cstr(&new_socket_file));
        if unsafe { libc::rename(old.as_ptr(), new.as_ptr()) } != 0 {
            return Ok(false);
        }

        *self.session.borrow_mut() = name.to_vec();
        Ok(true)
    }

    pub fn close_session(&self, do_unlink: bool) {
        if do_unlink && let Ok(socket_file) = session_path(&self.session.borrow(), true) {
            let socket_file = cstr(&socket_file);
            unsafe { libc::unlink(socket_file.as_ptr()) };
        }
        let listener = self.listener.borrow_mut().take();
        if let Some(listener) = listener {
            listener.close_fd();
        }
    }

    pub fn negotiating(&self) -> bool {
        !self.accepters.borrow().is_empty()
    }

    pub fn daemonize(&self) {
        self.is_daemon.set(true);
    }

    pub fn is_daemon(&self) -> bool {
        self.is_daemon.get()
    }

    fn remove_accepter(&self, accepter: &Rc<Accepter>) {
        let mut accepters = self.accepters.borrow_mut();
        let it = accepters.iter().position(|a| Rc::ptr_eq(a, accepter));
        debug_assert!(it.is_some());
        if let Some(it) = it {
            let removed = accepters.remove(it);
            drop(accepters);
            drop(removed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_manager::EventManager;
    use crate::face::NamedColor;

    fn socketpair() -> (i32, i32) {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    fn read_message(sock: i32) -> MsgReader {
        let mut reader = MsgReader::new();
        while !reader.ready() {
            reader.read_available(sock).unwrap();
        }
        reader
    }

    #[test]
    fn message_layout() {
        let mut buffer = RemoteBuffer::new();
        {
            let mut msg = MsgWriter::new(&mut buffer, MessageType::Key);
            msg.write(&Key::with_mod(Modifiers::Control, 'a' as u32));
        }
        let mut expected = vec![MessageType::Key as u8];
        expected.extend_from_slice(&13u32.to_ne_bytes());
        expected.extend_from_slice(&Modifiers::Control.0.to_ne_bytes());
        expected.extend_from_slice(&('a' as u32).to_ne_bytes());
        assert_eq!(buffer, expected);

        buffer.clear();
        {
            let mut msg = MsgWriter::new(&mut buffer, MessageType::Command);
            msg.write(b"echo".as_slice()).write(&Some(BufferCoord::new(1, 2))).write(&Color::rgb(1, 2, 3));
        }
        let mut expected = vec![MessageType::Command as u8];
        expected.extend_from_slice(&26u32.to_ne_bytes());
        expected.extend_from_slice(&4i32.to_ne_bytes());
        expected.extend_from_slice(b"echo");
        expected.push(1);
        expected.extend_from_slice(&1i32.to_ne_bytes());
        expected.extend_from_slice(&2i32.to_ne_bytes());
        expected.extend_from_slice(&[255, 1, 2, 3]);
        assert_eq!(buffer, expected);
    }

    #[test]
    fn round_trip() {
        let (a, b) = socketpair();
        let face = Face::with_attr(NamedColor::Red, Color::rgb(10, 20, 30), Attribute::Bold | Attribute::Italic);
        let line = DisplayLine::new(vec![
            DisplayAtom::text("hello", face),
            DisplayAtom::text("", Face::default()),
            DisplayAtom::text(" world", Face::new(NamedColor::Default, NamedColor::Blue)),
        ]);
        let mut display_buffer = DisplayBuffer::default();
        *display_buffer.lines_mut() = vec![line.clone(), DisplayLine::default(), line.clone()];
        let mut options = UiOptions::new();
        options.insert(b"key".to_vec(), b"value".to_vec());
        options.insert(b"empty".to_vec(), Vec::new());

        let mut buffer = RemoteBuffer::new();
        {
            let mut msg = MsgWriter::new(&mut buffer, MessageType::Draw);
            msg.write(&display_buffer).write(&DisplayCoord::new(3, 4)).write(&face).write(&Face::default()).write(&7);
        }
        {
            let mut msg = MsgWriter::new(&mut buffer, MessageType::SetOptions);
            msg.write(&options);
        }
        {
            let mut msg = MsgWriter::new(&mut buffer, MessageType::DrawStatus);
            msg.write(&line).write(&line).write(&-1).write(&line).write(&face).write(&StatusStyle::Prompt);
        }
        {
            MsgWriter::new(&mut buffer, MessageType::MenuHide);
        }

        // pass a pipe read end as ancillary data
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        assert!(send_data(a, &mut buffer, Some(pipe[0])).unwrap());

        let mut reader = read_message(b);
        assert_eq!(reader.type_(), MessageType::Draw);
        let db = reader.read::<DisplayBuffer>().unwrap();
        assert_eq!(db.lines().len(), display_buffer.lines().len());
        for (read, written) in db.lines().iter().zip(display_buffer.lines()) {
            assert_eq!(read.atoms(), written.atoms());
        }
        assert_eq!(reader.read::<DisplayCoord>().unwrap(), DisplayCoord::new(3, 4));
        assert_eq!(reader.read::<Face>().unwrap(), face);
        assert_eq!(reader.read::<Face>().unwrap(), Face::default());
        assert_eq!(reader.read::<ColumnCount>().unwrap(), 7);
        assert_eq!(reader.remaining(), 0);
        assert_eq!(reader.read::<i32>().unwrap_err().what(), b"tried to read after message end");
        let fd = reader.ancillary_fd().expect("ancillary fd");
        assert_eq!(unsafe { libc::write(pipe[1], b"x".as_ptr() as *const libc::c_void, 1) }, 1);
        let mut c = 0u8;
        assert_eq!(unsafe { libc::read(fd, &mut c as *mut u8 as *mut libc::c_void, 1) }, 1);
        assert_eq!(c, b'x');
        unsafe {
            libc::close(fd);
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }

        // reading only the available message size
        reader.reset();
        while !reader.ready() {
            reader.read_available(b).unwrap();
        }
        assert_eq!(reader.type_(), MessageType::SetOptions);
        assert_eq!(reader.read::<UiOptions>().unwrap(), options);

        let mut reader = read_message(b);
        assert_eq!(reader.type_(), MessageType::DrawStatus);
        assert_eq!(reader.read::<DisplayLine>().unwrap().atoms(), line.atoms());
        assert_eq!(reader.read::<DisplayLine>().unwrap().atoms(), line.atoms());
        assert_eq!(reader.read::<ColumnCount>().unwrap(), -1);
        assert_eq!(reader.read::<DisplayLine>().unwrap().atoms(), line.atoms());
        assert_eq!(reader.read::<Face>().unwrap(), face);
        assert_eq!(reader.read::<StatusStyle>().unwrap(), StatusStyle::Prompt);

        let reader = read_message(b);
        assert_eq!(reader.type_(), MessageType::MenuHide);
        assert_eq!(reader.size(), HEADER_SIZE);

        unsafe { libc::close(a) };
        let mut reader = MsgReader::new();
        let err = reader.read_available(b).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Disconnected);
        assert!(err.what().starts_with(b"socket read failed: "));
        unsafe { libc::close(b) };
    }

    #[test]
    fn invalid_message_size() {
        let (a, b) = socketpair();
        let mut buffer = vec![MessageType::Key as u8];
        buffer.extend_from_slice(&2u32.to_ne_bytes());
        assert!(send_data(a, &mut buffer, None).unwrap());
        let mut reader = MsgReader::new();
        let err = reader.read_available(b).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Disconnected);
        assert_eq!(err.what(), b"invalid message received");
        unsafe {
            libc::close(a);
            libc::close(b);
        }
    }

    #[test]
    fn session_paths() {
        assert_eq!(session_path(b"foo bar", false).unwrap_err().what(), b"invalid session name: 'foo bar'");
        assert_eq!(session_path("sé".as_bytes(), false).unwrap_err().what(), "invalid session name: 'sé'".as_bytes());
        assert_eq!(session_path(b"my-session_1", false).unwrap(), kformat!("{}/my-session_1", session_directory()));
        let long = vec![b'a'; 200];
        let err = session_path(&long, false).unwrap_err();
        assert_eq!(err.what(), kformat!("socket path too long: '{}/{}'", session_directory(), long));
        assert!(session_path(&long, true).is_ok());
    }

    struct TestUI {
        dimensions: DisplayCoord,
        log: Rc<RefCell<Vec<String>>>,
        on_key: Rc<RefCell<Option<OnKeyCallback>>>,
    }

    impl UserInterface for TestUI {
        fn is_ok(&self) -> bool {
            true
        }
        fn menu_show(&self, choices: &[DisplayLine], anchor: DisplayCoord, _: Face, _: Face, style: MenuStyle) {
            self.log.borrow_mut().push(format!("menu_show {} {:?} {:?}", choices.len(), anchor, style));
        }
        fn menu_select(&self, selected: i32) {
            self.log.borrow_mut().push(format!("menu_select {selected}"));
        }
        fn menu_hide(&self) {
            self.log.borrow_mut().push("menu_hide".into());
        }
        fn info_show(&self, _: &DisplayLine, content: &DisplayLineList, _: DisplayCoord, _: Face, style: InfoStyle) {
            self.log.borrow_mut().push(format!("info_show {} {:?}", content.len(), style));
        }
        fn info_hide(&self) {
            self.log.borrow_mut().push("info_hide".into());
        }
        fn draw(&self, db: &DisplayBuffer, cursor_pos: DisplayCoord, _: &Face, _: &Face, widget_columns: ColumnCount) {
            self.log.borrow_mut().push(format!("draw {} {:?} {}", db.lines().len(), cursor_pos, widget_columns));
        }
        fn draw_status(&self, _: &DisplayLine, _: &DisplayLine, cursor_pos: ColumnCount, _: &DisplayLine, _: &Face, style: StatusStyle) {
            self.log.borrow_mut().push(format!("draw_status {cursor_pos} {style:?}"));
        }
        fn dimensions(&self) -> DisplayCoord {
            self.dimensions
        }
        fn refresh(&self, force: bool) {
            self.log.borrow_mut().push(format!("refresh {force}"));
        }
        fn set_on_key(&self, callback: OnKeyCallback) {
            *self.on_key.borrow_mut() = Some(callback);
        }
        fn set_on_paste(&self, _: OnPasteCallback) {}
        fn set_ui_options(&self, options: &UiOptions) {
            self.log.borrow_mut().push(format!("set_ui_options {}", options.len()));
        }
    }

    // RemoteUI on one end of a socket pair, the RemoteClient socket handling
    // on the other end, both driven by the event manager
    #[test]
    fn remote_ui_to_client() {
        if !EventManager::has_instance() {
            EventManager::create();
        }
        let (server_sock, client_sock) = socketpair();
        let log = Rc::new(RefCell::new(Vec::new()));
        let client_on_key: Rc<RefCell<Option<OnKeyCallback>>> = Rc::new(RefCell::new(None));
        let state = Rc::new(RemoteClientState {
            socket_watcher: OnceCell::new(),
            reader: RefCell::new(MsgReader::new()),
            send_buffer: RefCell::new(Vec::new()),
            exit_status: Cell::new(None),
            ui: Box::new(TestUI { dimensions: DisplayCoord::new(24, 80), log: log.clone(), on_key: client_on_key.clone() }),
        });
        let weak = Rc::downgrade(&state);
        state.ui.set_on_key(Box::new(move |key: Key| {
            if let Some(state) = weak.upgrade() {
                state.send_message(MessageType::Key, |msg| {
                    msg.write(&key);
                });
            }
            Ok(())
        }));
        let weak = Rc::downgrade(&state);
        let _ = state.socket_watcher.set(FDWatcher::new(
            client_sock,
            FdEvents::Read | FdEvents::Write,
            EventMode::Urgent,
            move |watcher: &WatcherRef, events, _| match weak.upgrade() {
                Some(state) => state.handle_socket(watcher, events),
                None => Ok(()),
            },
        ));

        let ui = RemoteUI::new(server_sock, DisplayCoord::new(10, 20));
        let keys = Rc::new(RefCell::new(Vec::new()));
        let keys_clone = keys.clone();
        ui.set_on_key(Box::new(move |key| {
            keys_clone.borrow_mut().push(key);
            Ok(())
        }));

        let mut db = DisplayBuffer::default();
        *db.lines_mut() = vec![DisplayLine::default(); 3];
        ui.draw(&db, DisplayCoord::new(1, 2), &Face::default(), &Face::default(), 5);
        ui.menu_show(&[DisplayLine::default()], DisplayCoord::new(0, 0), Face::default(), Face::default(), MenuStyle::Inline);
        ui.menu_select(2);
        ui.info_show(&DisplayLine::default(), &vec![], DisplayCoord::new(0, 0), Face::default(), InfoStyle::Modal);
        ui.draw_status(&DisplayLine::default(), &DisplayLine::default(), 3, &DisplayLine::default(), &Face::default(), StatusStyle::Command);
        ui.set_ui_options(&UiOptions::new());
        ui.refresh(true);

        // send keys from the client ui
        {
            let cb = client_on_key.borrow();
            let cb = cb.as_ref().unwrap();
            cb(Key::new('x' as u32)).unwrap();
            cb(crate::keys::resize(DisplayCoord::new(30, 40))).unwrap();
        }

        ui.state.exit(3);

        for _ in 0..20 {
            if state.exit_status.get().is_some() && keys.borrow().len() == 2 {
                break;
            }
            EventManager::instance()
                .try_handle_next_events(EventMode::Normal, None, Some(std::time::Duration::from_millis(10)))
                .unwrap();
        }
        assert_eq!(state.exit_status.get(), Some(3));
        assert_eq!(state.socket_watcher.get().unwrap().fd(), -1);
        assert_eq!(*keys.borrow(), vec![Key::new('x' as u32), crate::keys::resize(DisplayCoord::new(30, 40))]);
        assert_eq!(ui.dimensions(), DisplayCoord::new(30, 40));
        assert_eq!(
            *log.borrow(),
            vec![
                "draw 3 DisplayCoord { line: 1, column: 2 } 5".to_string(),
                "menu_show 1 DisplayCoord { line: 0, column: 0 } Inline".to_string(),
                "menu_select 2".to_string(),
                "info_show 0 Modal".to_string(),
                "draw_status 3 Command".to_string(),
                "set_ui_options 0".to_string(),
                "refresh true".to_string(),
            ]
        );
        assert!(ui.is_ok());
        drop(ui);
    }

    #[test]
    fn server_session() {
        if !EventManager::has_instance() {
            EventManager::create();
        }
        let session = kformat!("kakrs-remote-test-{}", std::process::id());
        let renamed = kformat!("{}-renamed", session);
        assert!(!check_session(&session).unwrap());
        assert_eq!(
            send_command(&session, b"").unwrap_err().what(),
            kformat!("connect to {} failed", session_path(&session, false).unwrap())
        );
        assert_eq!(check_session(b"a/b").unwrap_err().what(), b"invalid session name: 'a/b'");

        let server = Server::create(session.clone(), false).unwrap();
        assert!(Server::has_instance());
        assert!(check_session(&session).unwrap());
        let process = |count| {
            for _ in 0..count {
                EventManager::instance()
                    .try_handle_next_events(EventMode::Normal, None, Some(std::time::Duration::from_millis(5)))
                    .unwrap();
            }
        };
        process(5);
        // empty commands do not need a command manager
        send_command(&session, b"").unwrap();
        process(1);
        assert!(server.negotiating());
        process(5);
        assert!(!server.negotiating());

        assert!(server.rename_session(&renamed).unwrap());
        assert_eq!(server.session(), renamed);
        assert!(check_session(&renamed).unwrap());
        assert!(!check_session(&session).unwrap());
        assert_eq!(server.rename_session(b"bad name").unwrap_err().what(), b"invalid session name: 'bad name'");

        Server::destroy();
        assert!(!Server::has_instance());
        assert!(!file_exists(&session_path(&renamed, false).unwrap()));
    }
}
