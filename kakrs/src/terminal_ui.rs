//! Port of terminal_ui.hh/cc
//!
//! The C++ TerminalUI is a Singleton so that its signal handlers can reach
//! it. Here signal handlers only touch async-signal-safe state (atomic flags
//! and `EventManager::force_signal`), the actual work (resize, hangup and
//! suspend handling) is then done from the stdin watcher callback, which is
//! forced to run by the signal.
//!
//! All the state lives behind an `Rc`, the stdin watcher callback holds a
//! `Weak` to it so that it stays valid even if the `TerminalUI` gets dropped
//! while a key callback is running.

use crate::diff::{for_each_diff, DiffOp};
use crate::display_buffer::{DisplayAtom, DisplayBuffer, DisplayLine, DisplayLineList};
use crate::event_manager::{set_signal_handler, EventManager, EventMode, FDWatcher, FdEvents};
use crate::exception::{Error, Result};
use crate::face::{merge_faces, Attribute, Color, Face};
use crate::file::{fd_readable, write_fd};
use crate::keys::{alt, ctrl, encode_coord, key, named, resize, shift, Key, Modifiers, MouseButton};
use crate::string_utils::{char_at, column_length, str_to_int_ifp, string_of_cp_cols, substr_columns};
use crate::unicode::{codepoint_width, is_word_default};
use crate::units::{Codepoint, ColumnCount, DisplayCoord, LineCount};
use crate::user_interface::{
    InfoStyle, MenuStyle, OnKeyCallback, OnPasteCallback, StatusStyle, UiOptions, UserInterface,
};
use crate::utf8;
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};

fn fix_atom_text(s: &[u8]) -> Vec<u8> {
    let mut res = Vec::with_capacity(s.len());
    for &c in s {
        if c <= 0x1F {
            utf8::dump(&mut res, 0x2400 + c as Codepoint);
        } else {
            res.push(c);
        }
    }
    res
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
struct Atom {
    text: Vec<u8>,
    skip: ColumnCount,
    face: Face,
}

impl Atom {
    fn length(&self) -> ColumnCount {
        column_length(&self.text) + self.skip
    }

    fn resize(&mut self, mut size: ColumnCount) {
        let mut it = 0;
        while it != self.text.len() && size > 0 {
            size -= codepoint_width(utf8::read_codepoint(&self.text, &mut it));
        }

        if size < 0 {
            // possible if resizing to the middle of a double-width codepoint
            debug_assert!(size == -1);
            it = utf8::previous(&self.text, it, 0);
            self.skip = 1;
        } else {
            self.skip = size;
        }
        self.text.truncate(it);
    }
}

#[derive(Clone, Default, Debug)]
struct Line {
    atoms: Vec<Atom>,
}

impl Line {
    fn append(&mut self, text: &[u8], skip: ColumnCount, face: Face) {
        if let Some(back) = self.atoms.last_mut()
            && back.face == face
            && (back.skip == 0 || text.is_empty())
        {
            back.text.extend_from_slice(&fix_atom_text(text));
            back.skip += skip;
            return;
        }
        self.atoms.push(Atom { text: fix_atom_text(text), skip, face });
    }

    fn resize(&mut self, width: ColumnCount) {
        let mut it = 0;
        let mut column = 0;
        while it != self.atoms.len() && column < width {
            column += self.atoms[it].length();
            it += 1;
        }

        if column < width {
            let face = self.atoms.last().map_or(Face::default(), |a| a.face);
            self.append(&[], width - column, face);
        } else {
            self.atoms.truncate(it);
            if column > width {
                let back = self.atoms.last_mut().unwrap();
                back.resize(back.length() - (column - width));
            }
        }
    }

    /// Erase the columns [pos, pos+len), returns the index where the erased
    /// content was
    fn erase_range(&mut self, pos: ColumnCount, len: ColumnCount) -> usize {
        struct Pos {
            it: usize,
            column: ColumnCount,
        }
        let mut find_pos = 0;
        let mut find_it = 0;
        let mut find_col = |atoms: &[Atom], col: ColumnCount| -> Pos {
            while find_it != atoms.len() {
                let atom_len = atoms[find_it].length();
                if find_pos + atom_len >= col {
                    return Pos { it: find_it, column: col - find_pos };
                }
                find_pos += atom_len;
                find_it += 1;
            }
            Pos { it: find_it, column: 0 }
        };
        let begin = find_col(&self.atoms, pos);
        let end = find_col(&self.atoms, pos + len);

        let make_tail = |atom: &Atom, mut from: ColumnCount| -> Atom {
            let mut it = 0;
            while it != atom.text.len() && from > 0 {
                from -= codepoint_width(utf8::read_codepoint(&atom.text, &mut it));
            }

            if from < 0 {
                // can happen if tail starts in the middle of a double-width codepoint
                debug_assert!(from == -1);
                let mut text = b" ".to_vec();
                text.extend_from_slice(&atom.text[it..]);
                return Atom { text, skip: atom.skip, face: atom.face };
            }
            Atom { text: atom.text[it..].to_vec(), skip: atom.skip - from, face: atom.face }
        };

        if begin.it == self.atoms.len() {
            return begin.it;
        }

        if begin.it == end.it {
            let tail = make_tail(&self.atoms[begin.it], end.column);
            self.atoms[begin.it].resize(begin.column);
            if !(tail.text.is_empty() && tail.skip == 0) {
                self.atoms.insert(begin.it + 1, tail);
            }
            return begin.it + 1;
        }

        self.atoms[begin.it].resize(begin.column);
        let mut end_it = end.it;
        if end.column > 0 {
            if end.column == self.atoms[end_it].length() {
                end_it += 1;
            } else {
                self.atoms[end_it] = make_tail(&self.atoms[end_it], end.column);
            }
        }
        self.atoms.drain(begin.it + 1..end_it);
        begin.it + 1
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Rect {
    pub pos: DisplayCoord,
    pub size: DisplayCoord,
}

#[derive(Default)]
struct Window {
    pos: DisplayCoord,
    size: DisplayCoord,
    lines: Option<Vec<Line>>,
}

impl Window {
    /// C++ `explicit operator bool`
    fn is_some(&self) -> bool {
        self.lines.is_some()
    }

    fn rect(&self) -> Rect {
        Rect { pos: self.pos, size: self.size }
    }

    fn create(&mut self, p: DisplayCoord, s: DisplayCoord) {
        debug_assert!(p.line >= 0 && p.column >= 0);
        debug_assert!(s.line >= 0 && s.column >= 0);
        self.pos = p;
        self.size = s;
        self.lines = Some(vec![Line::default(); s.line.max(0) as usize]);
    }

    fn destroy(&mut self) {
        self.pos = DisplayCoord::default();
        self.size = DisplayCoord::default();
        self.lines = None;
    }

    fn blit(&mut self, target: &mut Window) {
        debug_assert!(self.pos.line < target.size.line);
        let Some(lines) = self.lines.as_mut() else { return };
        let Some(target_lines) = target.lines.as_mut() else { return };
        let mut line_index = self.pos.line;
        for line in lines.iter_mut().take(self.size.line as usize) {
            line.resize(self.size.column);
            let target_line = &mut target_lines[line_index as usize];
            target_line.resize(target.size.column);
            let it = target_line.erase_range(self.pos.column, self.size.column);
            target_line.atoms.splice(it..it, line.atoms.iter().cloned());
            line_index += 1;
            if line_index == target.size.line {
                break;
            }
        }
    }

    fn draw(&mut self, mut pos: DisplayCoord, atoms: &[DisplayAtom], default_face: &Face) {
        if pos.line >= self.size.line {
            // We might receive an out of date draw command after a resize
            return;
        }
        let size_column = self.size.column;
        let Some(lines) = self.lines.as_mut() else { return };
        let line = &mut lines[pos.line as usize];

        line.resize(pos.column);
        for atom in atoms {
            let content = atom.content();
            if content.is_empty() {
                continue;
            }

            let face = merge_faces(default_face, &atom.face);
            if content.last() == Some(&b'\n') {
                line.append(&content[..content.len() - 1], 1, face);
            } else {
                line.append(&content, 0, face);
            }
            pos.column += column_length(&content);
        }

        if pos.column < size_column {
            line.append(&[], size_column - pos.column, *default_face);
        }
    }
}

/// Buffered atomic writer to stdout, flushed on drop
struct Writer {
    buffer: Vec<u8>,
}

impl Writer {
    const SIZE: usize = 4096;

    fn new() -> Writer {
        Writer { buffer: Vec::with_capacity(Self::SIZE) }
    }

    fn write(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let write_len = data.len().min(Self::SIZE - self.buffer.len());
            self.buffer.extend_from_slice(&data[..write_len]);
            if self.buffer.len() == Self::SIZE {
                self.flush();
            }
            data = &data[write_len..];
        }
    }

    fn flush(&mut self) {
        // Write errors are ignored, a closed terminal is detected as a hangup
        let _ = write_fd(libc::STDOUT_FILENO, &self.buffer, true);
        self.buffer.clear();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if !self.buffer.is_empty() {
            self.flush();
        }
    }
}

macro_rules! format_with {
    ($writer:expr, $($args:tt)*) => {
        $writer.write(&kformat!($($args)*))
    };
}

#[derive(Default)]
struct Screen {
    win: Window,
    hashes: Vec<u64>,
    active_face: Face,
}

fn set_face(active_face: &mut Face, face: &Face, writer: &mut Writer) {
    const FG_TABLE: [i32; 17] = [39, 30, 31, 32, 33, 34, 35, 36, 37, 90, 91, 92, 93, 94, 95, 96, 97];
    const BG_TABLE: [i32; 17] = [49, 40, 41, 42, 43, 44, 45, 46, 47, 100, 101, 102, 103, 104, 105, 106, 107];
    const UL_TABLE: [i32; 17] = [0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    const ATTR_TABLE: [&str; 10] = ["0", "4", "4:3", "21", "7", "5", "1", "2", "3", "9"];

    let set_color = |writer: &mut Writer, fg: bool, color: &Color, join: bool| {
        if join {
            writer.write(b";");
        }
        if color.is_rgb() {
            format_with!(writer, "{};2;{};{};{}", if fg { 38 } else { 48 }, color.r, color.g, color.b);
        } else {
            format_with!(writer, "{}", (if fg { FG_TABLE } else { BG_TABLE })[color.a as usize]);
        }
    };

    if *active_face == *face {
        return;
    }

    writer.write(b"\x1b[");
    let mut join = false;
    if face.attributes != active_face.attributes {
        for (i, attr) in ATTR_TABLE.iter().enumerate() {
            if face.attributes.has(Attribute(1 << i)) {
                format_with!(writer, ";{}", attr);
            }
        }
        active_face.fg = Color::DEFAULT;
        active_face.bg = Color::DEFAULT;
        active_face.underline = Color::DEFAULT;
        join = true;
    }
    if active_face.fg != face.fg {
        set_color(writer, true, &face.fg, join);
        join = true;
    }
    if active_face.bg != face.bg {
        set_color(writer, false, &face.bg, join);
        join = true;
    }
    if active_face.underline != face.underline {
        if join {
            writer.write(b";");
        }
        if face.underline != Color::DEFAULT {
            if face.underline.is_rgb() {
                format_with!(writer, "58:2::{}:{}:{}", face.underline.r, face.underline.g, face.underline.b);
            } else {
                format_with!(writer, "58:5:{}", UL_TABLE[face.underline.a as usize]);
            }
        } else {
            writer.write(b"59");
        }
    }
    writer.write(b"m");

    *active_face = *face;
}

impl Screen {
    fn output(&mut self, force: bool, synchronized: bool, writer: &mut Writer) {
        let Some(lines) = self.win.lines.as_ref() else { return };
        let size_line = self.win.size.line;

        if force {
            self.hashes.iter_mut().for_each(|h| *h = 0);
            writer.write(b"\x1b[m");
            self.active_face = Face::default();
        }

        let hash_line = |line: &Line| -> u64 {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            line.atoms.hash(&mut hasher);
            (hasher.finish() << 1) | 1 // ensure non-zero
        };

        let active_face = &mut self.active_face;
        let mut output_line = |line: &Line, writer: &mut Writer| {
            let mut pending_move: ColumnCount = 0;
            for Atom { text, skip, face } in &line.atoms {
                if text.is_empty() && *skip == 0 {
                    continue;
                }

                if pending_move != 0 {
                    format_with!(writer, "\x1b[{}C", pending_move);
                    pending_move = 0;
                }
                set_face(active_face, face, writer);
                writer.write(text);
                if *skip > 3 && face.attributes == Attribute::Normal {
                    writer.write(b"\x1b[K");
                    pending_move = *skip;
                } else if *skip > 0 {
                    writer.write(&string_of_cp_cols(' ' as Codepoint, *skip));
                }
            }
        };

        if synchronized {
            writer.write(b"\x1b[?2026h"); // begin synchronized update

            #[derive(Default)]
            struct Change {
                keep: i32,
                add: i32,
                del: i32,
            }
            let mut changes = vec![Change::default()];
            let new_hashes: Vec<u64> = lines.iter().take(size_line as usize).map(hash_line).collect();
            let hashes = &self.hashes;
            for_each_diff(
                size_line,
                size_line,
                |i, j| hashes[i as usize] == new_hashes[j as usize],
                |op, len| match op {
                    DiffOp::Keep => changes.push(Change { keep: len, add: 0, del: 0 }),
                    DiffOp::Add => changes.last_mut().unwrap().add += len,
                    DiffOp::Remove => changes.last_mut().unwrap().del += len,
                },
            );
            self.hashes = new_hashes;

            let mut line = 0;
            for change in &changes {
                line += change.keep;
                let del = change.del - change.add;
                if del > 0 {
                    format_with!(writer, "\x1b[{}H\x1b[{}M", line + 1, del);
                    line -= del;
                }
                line += change.del;
            }

            line = 0;
            for change in &changes {
                line += change.keep;
                for i in 0..change.add {
                    let add = change.add - change.del;
                    if i == 0 && add > 0 {
                        format_with!(writer, "\x1b[{}H\x1b[{}L", line + 1, add);
                    } else {
                        format_with!(writer, "\x1b[{}H", line + 1);
                    }

                    output_line(&lines[line as usize], writer);
                    line += 1;
                }
            }

            writer.write(b"\x1b[?2026l"); // end synchronized update
        } else {
            for line in 0..size_line {
                let hash = hash_line(&lines[line as usize]);
                if hash == self.hashes[line as usize] {
                    continue;
                }
                self.hashes[line as usize] = hash;

                format_with!(writer, "\x1b[{}H", line + 1);
                output_line(&lines[line as usize], writer);
            }
        }
    }
}

const ASSISTANT_CAT: &[&str] = &[
    r"  ___            ",
    r" (__ \           ",
    r"   / /          ╭",
    r"  .' '·.        │",
    r" '      ”       │",
    r" ╰       /\_/|  │",
    r"  | .         \ │",
    r"  ╰_J`    | | | ╯",
    r"      ' \__- _/  ",
    r"      \_\   \_\  ",
    r"                 ",
];

const ASSISTANT_CLIPPY: &[&str] = &[
    " ╭──╮   ",
    " │  │   ",
    " @  @  ╭",
    " ││ ││ │",
    " ││ ││ ╯",
    " │╰─╯│  ",
    " ╰───╯  ",
    "        ",
];

const ASSISTANT_DILBERT: &[&str] = &[
    r"  დოოოოოდ   ",
    r"  |     |   ",
    r"  |     |  ╭",
    r"  |-ᱛ ᱛ-|  │",
    r" Ͼ   ∪   Ͽ │",
    r"  |     |  ╯",
    r" ˏ`-.ŏ.-´ˎ  ",
    r"     @      ",
    r"      @     ",
    r"            ",
];

fn sq<T: std::ops::Mul<Output = T> + Copy>(x: T) -> T {
    x * x
}

static RESIZE_PENDING: AtomicBool = AtomicBool::new(false);
static TERMINAL_HUNGUP: AtomicBool = AtomicBool::new(false);
static SUSPEND_PENDING: AtomicBool = AtomicBool::new(false);

extern "C" fn resize_signal_handler(_: libc::c_int) {
    RESIZE_PENDING.store(true, Ordering::SeqCst);
    EventManager::force_signal(0);
}

extern "C" fn hangup_signal_handler(_: libc::c_int) {
    TERMINAL_HUNGUP.store(true, Ordering::SeqCst);
    EventManager::force_signal(0);
}

/// The C++ handler calls TerminalUI::instance().suspend() directly from the
/// signal handler, here we defer it to the stdin watcher.
extern "C" fn suspend_signal_handler(_: libc::c_int) {
    SUSPEND_PENDING.store(true, Ordering::SeqCst);
    EventManager::force_signal(0);
}

fn handler_ptr(handler: extern "C" fn(libc::c_int)) -> libc::sighandler_t {
    handler as libc::sighandler_t
}

fn write_stdout(data: &[u8]) {
    unsafe { libc::write(libc::STDOUT_FILENO, data.as_ptr() as *const libc::c_void, data.len()) };
}

#[derive(Clone, Copy, Default)]
struct Synchronized {
    queried: bool,
    supported: bool,
    set: bool,
    requested: bool,
}

impl Synchronized {
    fn get(&self) -> bool {
        if self.set { self.requested } else { self.supported }
    }
}

struct Menu {
    win: Window,
    items: Vec<DisplayLine>,
    fg: Face,
    bg: Face,
    anchor: DisplayCoord,
    style: MenuStyle,
    selected_item: i32,
    first_item: i32,
    columns: i32,
}

struct Info {
    win: Window,
    title: DisplayLine,
    content: DisplayLineList,
    face: Face,
    anchor: DisplayCoord,
    style: InfoStyle,
}

const DEFAULT_SHIFT_FUNCTION_KEY: i32 = 12;

struct State {
    window: Window,
    screen: Screen,

    dimensions: DisplayCoord,
    original_termios: libc::termios,

    menu: Menu,
    info: Info,

    cursor_pos: DisplayCoord,

    stdin_watcher: Option<FDWatcher>,
    paste_buffer: Option<Vec<u8>>,
    /// Completed paste, waiting to be passed to on_paste once the state is
    /// not borrowed anymore
    pending_paste: Option<Vec<u8>>,

    status_on_top: bool,
    assistant: &'static [&'static str],

    mouse_enabled: bool,
    wheel_scroll_amount: i32,
    mouse_state: i32,

    shift_function_key: i32,

    set_title: bool,
    title: Option<Vec<u8>>,

    synchronized: Synchronized,

    padding_char: Codepoint,
    padding_fill: bool,
    cursor_native: bool,

    dirty: bool,

    resize_pending: bool,

    status_len: ColumnCount,
    status_pos: ColumnCount,
    status_cursor_pos: ColumnCount,
    info_max_width: ColumnCount,
}

type KeyCallback = Rc<dyn Fn(Key) -> Result<()>>;
type PasteCallback = Rc<dyn Fn(&[u8]) -> Result<()>>;

struct Inner {
    state: RefCell<State>,
    on_key: RefCell<Option<KeyCallback>>,
    on_paste: RefCell<Option<PasteCallback>>,
}

pub struct TerminalUI {
    inner: Rc<Inner>,
}

fn get_char() -> Option<u8> {
    if !fd_readable(libc::STDIN_FILENO) {
        return None;
    }

    let mut c: u8 = 0;
    if unsafe { libc::read(libc::STDIN_FILENO, &mut c as *mut u8 as *mut libc::c_void, 1) } == 1 {
        return Some(c);
    }

    TERMINAL_HUNGUP.store(true, Ordering::SeqCst);
    None
}

const fn control(c: u8) -> Codepoint {
    (c & 0o37) as Codepoint
}

fn convert(c: Codepoint, verase: Codepoint) -> Codepoint {
    if c == control(b'm') {
        return named::RETURN;
    }
    if c == control(b'i') {
        return named::TAB;
    }
    if c == ' ' as Codepoint {
        return named::SPACE;
    }
    if c == verase {
        return named::BACKSPACE;
    }
    if c == 127 {
        // when it's not backspace
        return named::DELETE;
    }
    if c == 27 {
        return named::ESCAPE;
    }
    c
}

fn parse_key(c: u8, verase: Codepoint) -> Key {
    let cp = convert(c as Codepoint, verase);
    if cp > 255 {
        return Key::new(cp);
    }
    // Special case: you can type NUL with Ctrl-2 or Ctrl-Shift-2 or
    // Ctrl-Backtick, but the most straightforward way is Ctrl-Space.
    if c == 0 {
        return ctrl(Key::new(named::SPACE));
    }
    // Represent Ctrl-letter combinations in lower-case, to be clear
    // that Shift is not involved.
    if c < 27 {
        return ctrl(Key::new(c as Codepoint - 1 + 'a' as Codepoint));
    }
    // Represent Ctrl-symbol combinations in "upper-case", as they are
    // traditionally-rendered.
    // Note that Escape is handled elsewhere.
    if c < 32 {
        return ctrl(Key::new(c as Codepoint - 1 + 'A' as Codepoint));
    }

    // Read the remaining bytes of the utf8 sequence, missing bytes read as 0
    let next = || get_char().unwrap_or(0) as Codepoint;
    let byte = c as Codepoint;
    let cp = if byte & 0x80 == 0 {
        byte
    } else if byte & 0xE0 == 0xC0 {
        ((byte & 0x1F) << 6) | (next() & 0x3F)
    } else if byte & 0xF0 == 0xE0 {
        let cp = ((byte & 0x0F) << 12) | ((next() & 0x3F) << 6);
        cp | (next() & 0x3F)
    } else if byte & 0xF8 == 0xF0 {
        let mut cp = ((byte & 0x0F) << 18) | ((next() & 0x3F) << 12);
        cp |= (next() & 0x3F) << 6;
        cp | (next() & 0x3F)
    } else {
        byte
    };
    Key::new(cp)
}

fn parse_mask(mask: i32) -> Modifiers {
    let mut m = Modifiers::None;
    if mask & 1 != 0 {
        m |= Modifiers::Shift;
    }
    if mask & 2 != 0 {
        m |= Modifiers::Alt;
    }
    if mask & 4 != 0 {
        m |= Modifiers::Control;
    }
    m
}

fn parse_ss3() -> Option<Key> {
    let mut raw_mask: i32 = 0;
    let mut code = b'0';
    loop {
        raw_mask = raw_mask.wrapping_mul(10).wrapping_add((code - b'0') as i32);
        code = get_char().unwrap_or(0xff);
        if !code.is_ascii_digit() {
            break;
        }
    }
    let m = parse_mask((raw_mask - 1).max(0));

    let k = |c: Codepoint| Some(Key::with_mod(m, c));
    match code {
        b' ' => k(named::SPACE),
        b'A' => k(named::UP),
        b'B' => k(named::DOWN),
        b'C' => k(named::RIGHT),
        b'D' => k(named::LEFT),
        b'F' => k(named::END),
        b'H' => k(named::HOME),
        b'I' => k(named::TAB),
        b'M' => k(named::RETURN),
        b'P' => k(named::F1),
        b'Q' => k(named::F2),
        b'R' => k(named::F3),
        b'S' => k(named::F4),
        b'X' => k('=' as Codepoint),
        b'j' => k('*' as Codepoint),
        b'k' => k('+' as Codepoint),
        b'l' => k(',' as Codepoint),
        b'm' => k('-' as Codepoint),
        b'n' => k('.' as Codepoint),
        b'o' => k('/' as Codepoint),
        b'p' => k('0' as Codepoint),
        b'q' => k('1' as Codepoint),
        b'r' => k('2' as Codepoint),
        b's' => k('3' as Codepoint),
        b't' => k('4' as Codepoint),
        b'u' => k('5' as Codepoint),
        b'v' => k('6' as Codepoint),
        b'w' => k('7' as Codepoint),
        b'x' => k('8' as Codepoint),
        b'y' => k('9' as Codepoint),
        _ => None,
    }
}

fn div_round_up(a: i32, b: i32) -> i32 {
    (a - 1) / b + 1
}

fn height_limit(style: MenuStyle) -> LineCount {
    match style {
        MenuStyle::Inline => 10,
        MenuStyle::Prompt => 10,
        MenuStyle::Search => 3,
    }
}

fn compute_pos(anchor: DisplayCoord, size: DisplayCoord, rect: Rect, to_avoid: Rect, mut prefer_above: bool) -> DisplayCoord {
    let mut pos = DisplayCoord::default();
    if prefer_above {
        pos = anchor - DisplayCoord::new(size.line, 0);
        if pos.line < 0 {
            prefer_above = false;
        }
    }
    let rect_end = rect.pos + rect.size;
    if !prefer_above {
        pos = anchor + DisplayCoord::new(1, 0);
        if pos.line + size.line >= rect_end.line {
            pos.line = rect.pos.line.max(anchor.line - size.line);
        }
    }
    if pos.column + size.column >= rect_end.column {
        pos.column = rect.pos.column.max(rect_end.column - size.column);
    }

    if to_avoid.size != DisplayCoord::default() {
        let to_avoid_end = to_avoid.pos + to_avoid.size;

        let end = pos + size;

        // check intersection
        if !(end.line < to_avoid.pos.line
            || end.column < to_avoid.pos.column
            || pos.line > to_avoid_end.line
            || pos.column > to_avoid_end.column)
        {
            pos.line = to_avoid.pos.line.min(anchor.line) - size.line;
            // if above does not work, try below
            if pos.line < 0 {
                pos.line = to_avoid_end.line.max(anchor.line);
            }
        }
    }

    pos
}

/// C++ calls is_word on each char, bytes >= 0x80 (negative chars) are never
/// word characters.
fn is_word_byte(c: u8) -> bool {
    c < 0x80 && is_word_default(c as Codepoint)
}

fn wrap_lines(lines: &DisplayLineList, max_width: ColumnCount) -> DisplayLineList {
    let mut result = DisplayLineList::new();
    for line in lines {
        let mut line = line.clone();
        let mut column = 0;
        let mut it = 0;
        while it != line.len() {
            let length = line.atoms()[it].length();
            column += length;
            if column > max_width {
                let full = line.atoms()[it].content().into_owned();
                let mut content = substr_columns(&full, 0, length - (column - max_width));
                if let Some(pos) = content.iter().rposition(|&c| !is_word_byte(c)) {
                    content = &content[..pos + 1];
                }

                if !content.is_empty() {
                    it = line.split_at_column(it, column_length(content)) + 1;
                }
                result.push(line.extract(0, it));
                it = 0;
                column = 0;
            } else {
                it += 1;
            }
        }
        result.push(line);
    }
    result
}

/// Draw helpers used by info_show (the C++ draw_atoms variadic lambda)
fn draw_str(win: &mut Window, pos: &mut DisplayCoord, s: Vec<u8>, face: &Face) {
    let len = column_length(&s);
    win.draw(*pos, &[DisplayAtom::text(s, Face::default())], face);
    pos.column += len;
}

fn draw_line(win: &mut Window, pos: &mut DisplayCoord, line: &DisplayLine, face: &Face) {
    win.draw(*pos, line.atoms(), face);
    pos.column += line.length();
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

impl State {
    fn content_line_offset(&self) -> LineCount {
        if self.status_on_top { 1 } else { 0 }
    }

    fn set_resize_pending(&mut self) {
        self.resize_pending = true;
        EventManager::force_signal(0);
    }

    fn set_raw_mode(&self) {
        let mut attr = self.original_termios;
        attr.c_iflag &= !(libc::IGNBRK | libc::BRKINT | libc::PARMRK | libc::ISTRIP | libc::INLCR | libc::IGNCR | libc::ICRNL | libc::IXON);
        attr.c_oflag &= !libc::OPOST;
        attr.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
        attr.c_lflag |= libc::NOFLSH;
        attr.c_cflag &= !(libc::CSIZE | libc::PARENB);
        attr.c_cflag |= libc::CS8;
        attr.c_cc[libc::VMIN] = 0;
        attr.c_cc[libc::VTIME] = 0;

        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &attr) };
    }

    fn redraw(&mut self, force: bool) {
        self.window.blit(&mut self.screen.win);

        if self.menu.columns != 0 || self.menu.win.pos.column > self.status_len {
            self.menu.win.blit(&mut self.screen.win);
        }

        self.info.win.blit(&mut self.screen.win);

        let mut writer = Writer::new();
        self.screen.output(force, self.synchronized.get(), &mut writer);

        let mut set_cursor_pos = |c: DisplayCoord| {
            format_with!(writer, "\x1b[{};{}H", c.line + 1, c.column + 1);
        };
        if self.status_cursor_pos > 0 {
            set_cursor_pos(DisplayCoord::new(
                if self.status_on_top { 0 } else { self.dimensions.line },
                self.status_cursor_pos,
            ));
        } else {
            set_cursor_pos(self.cursor_pos + DisplayCoord::new(self.content_line_offset(), 0));
        }
    }

    fn refresh(&mut self, force: bool) {
        if self.dirty || force {
            self.redraw(force);
        }
        self.dirty = false;
    }

    fn draw(&mut self, display_buffer: &DisplayBuffer, cursor_pos: DisplayCoord, default_face: &Face, padding_face: &Face) {
        self.check_resize(false);

        let dim = self.dimensions;
        let line_offset = self.content_line_offset();
        let mut line_index = line_offset;
        for line in display_buffer.lines() {
            self.window.draw(DisplayCoord::new(line_index, 0), line.atoms(), default_face);
            line_index += 1;
        }

        let face = merge_faces(default_face, padding_face);

        let padding = [DisplayAtom::text(
            string_of_cp_cols(self.padding_char, if self.padding_fill { dim.column } else { 1 }),
            Face::default(),
        )];

        while line_index < dim.line + line_offset {
            self.window.draw(DisplayCoord::new(line_index, 0), &padding, &face);
            line_index += 1;
        }

        self.cursor_pos = cursor_pos;

        self.dirty = true;
    }

    fn draw_status(&mut self, prompt: &DisplayLine, content: &DisplayLine, cursor_pos: ColumnCount, mode_line: &DisplayLine, default_face: &Face) {
        let status_line_pos = if self.status_on_top { 0 } else { self.dimensions.line };

        let prompt_len = prompt.length();
        let status_width = self.dimensions.column - prompt_len;
        if cursor_pos < self.status_pos {
            self.status_pos = cursor_pos;
        }
        if cursor_pos >= self.status_pos + status_width {
            self.status_pos = cursor_pos + 1 - status_width;
        }

        self.status_cursor_pos = if cursor_pos >= 0 { prompt_len + cursor_pos } else { -1 };

        let mut trimmed_content = content.clone();
        trimmed_content.trim(self.status_pos, status_width);
        self.window.draw(DisplayCoord::new(status_line_pos, 0), prompt.atoms(), default_face);
        self.window.draw(DisplayCoord::new(status_line_pos, prompt_len), trimmed_content.atoms(), default_face);

        let mode_len = mode_line.length();
        self.status_len = prompt_len + trimmed_content.length();
        let remaining = self.dimensions.column - self.status_len;
        if mode_len < remaining {
            let col = self.dimensions.column - mode_len;
            self.window.draw(DisplayCoord::new(status_line_pos, col), mode_line.atoms(), default_face);
        } else if remaining > 2 {
            let mut trimmed_mode_line = mode_line.clone();
            trimmed_mode_line.trim(mode_len + 2 - remaining, remaining - 2);
            trimmed_mode_line.insert(0, DisplayAtom::text("…", Face::default()));
            debug_assert!(trimmed_mode_line.length() == remaining - 1);

            let col = self.dimensions.column - remaining + 1;
            self.window.draw(DisplayCoord::new(status_line_pos, col), trimmed_mode_line.atoms(), default_face);
        }

        if self.set_title {
            let mut writer = Writer::new();
            writer.write(b"\x1b]2;");
            let write_escaped = |writer: &mut Writer, s: &[u8]| {
                let mut it = 0;
                while it != s.len() {
                    let c = s[it];
                    writer.write(&[if (0x20..=0x7e).contains(&c) { c } else { b'?' }]);
                    it = utf8::next(s, it);
                }
            };
            match &self.title {
                None => {
                    for atom in mode_line {
                        write_escaped(&mut writer, &atom.content());
                    }
                    writer.write(b" - Kakoune");
                }
                Some(title) => write_escaped(&mut writer, title),
            }
            writer.write(b"\x07");
        }

        self.dirty = true;
    }

    fn check_resize(&mut self, force: bool) {
        if !force && !RESIZE_PENDING.load(Ordering::SeqCst) {
            return;
        }

        RESIZE_PENDING.store(false, Ordering::SeqCst);

        let fd = unsafe { libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return;
        }

        let mut terminal_size = DisplayCoord::new(24, 80);
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            terminal_size = DisplayCoord::new(ws.ws_row as LineCount, ws.ws_col as ColumnCount);
        }
        unsafe { libc::close(fd) };

        let info = self.info.win.is_some();
        let menu = self.menu.win.is_some();
        if self.window.is_some() {
            self.window.destroy();
        }
        if info {
            self.info.win.destroy();
        }
        if menu {
            self.menu.win.destroy();
        }

        self.window.create(DisplayCoord::default(), terminal_size);
        self.screen.win.create(DisplayCoord::default(), terminal_size);
        self.screen.hashes = vec![0; terminal_size.line as usize];
        debug_assert!(self.window.is_some());

        self.dimensions = terminal_size - DisplayCoord::new(1, 0);

        if menu {
            let items = std::mem::take(&mut self.menu.items);
            self.menu_show(&items, self.menu.anchor, self.menu.fg, self.menu.bg, self.menu.style);
        }
        if info {
            let (title, content) = (self.info.title.clone(), self.info.content.clone());
            self.info_show(&title, &content, self.info.anchor, self.info.face, self.info.style);
        }

        self.set_resize_pending();
    }

    fn get_next_key(&mut self) -> Option<Key> {
        if TERMINAL_HUNGUP.load(Ordering::SeqCst) {
            set_signal_handler(libc::SIGWINCH, libc::SIG_DFL);
            set_signal_handler(libc::SIGHUP, libc::SIG_IGN);
            if self.window.is_some() {
                self.window.destroy();
            }
            if let Some(watcher) = &self.stdin_watcher {
                watcher.disable();
            }
            return None;
        }

        self.check_resize(false);

        if self.resize_pending {
            self.resize_pending = false;
            return Some(resize(self.dimensions));
        }

        let c = get_char()?;
        let verase = self.original_termios.c_cc[libc::VERASE] as Codepoint;

        if c == 27 {
            if let Some(next) = get_char() {
                if next == b'[' {
                    // potential CSI
                    return Some(self.parse_csi().unwrap_or(alt(key('['))));
                }
                if next == b'O' {
                    // potential SS3
                    return Some(parse_ss3().unwrap_or(alt(key('O'))));
                }
                return Some(alt(parse_key(next, verase)));
            } else if self.paste_buffer.is_none() {
                return Some(Key::new(named::ESCAPE));
            }
        }

        if let Some(paste_buffer) = &mut self.paste_buffer {
            let paste_convert = |c: u8| match c {
                b'\r' => b'\n',
                c => c,
            };

            paste_buffer.push(paste_convert(c));
            return Some(Key::new(named::INVALID));
        }
        Some(parse_key(c, verase))
    }

    fn parse_csi(&mut self) -> Option<Key> {
        let next_char = || get_char().unwrap_or(0xff);
        let verase = self.original_termios.c_cc[libc::VERASE] as Codepoint;
        let mut params = [[0i32; 4]; 16];
        let mut c = next_char();
        let mut private_mode = 0u8;
        if matches!(c, b'?' | b'<' | b'=' | b'>') {
            private_mode = c;
            c = next_char();
        }
        let mut count = 0;
        let mut subcount = 0;
        while count < 16 && (0x30..=0x3f).contains(&c) {
            if c.is_ascii_digit() {
                let p = &mut params[count][subcount];
                *p = p.wrapping_mul(10).wrapping_add((c - b'0') as i32);
            } else if c == b':' && subcount < 3 {
                subcount += 1;
            } else if c == b';' {
                count += 1;
                subcount = 0;
            } else {
                return None;
            }
            c = next_char();
        }
        if c != b'$' && !(0x40..=0x7e).contains(&c) {
            return None;
        }

        let mouse_scroll = |m: Modifiers, coord: Codepoint, down: bool, amount: i32| -> Key {
            let amount = if down { amount } else { -amount };
            Key::with_mod(m | Modifiers::Scroll | Modifiers((amount << 16) as u32), coord)
        };

        let masked_key = |mut key: Codepoint, shifted_key: Codepoint| -> Key {
            let mask = (params[1][0] - 1).max(0);
            let mut modifiers = parse_mask(mask);
            if shifted_key != 0 && modifiers.has(Modifiers::Shift) {
                modifiers.remove(Modifiers::Shift);
                key = shifted_key;
            }
            Key::with_mod(modifiers, key)
        };
        let mk = |key: Codepoint| Some(masked_key(key, 0));
        let p00 = params[0][0];

        match c {
            b'$' => {
                if private_mode == b'?' && next_char() == b'y' {
                    // DECRPM
                    if p00 == 2026 {
                        self.synchronized.supported = params[1][0] == 1 || params[1][0] == 2;
                    }
                    return Some(Key::new(named::INVALID));
                }
                match p00 {
                    // rxvt style
                    23 | 24 => Some(Key::with_mod(Modifiers::Shift, named::F11 + (p00 - 23) as Codepoint)),
                    _ => None,
                }
            }
            b'A' => mk(named::UP),
            b'B' => mk(named::DOWN),
            b'C' => mk(named::RIGHT),
            b'D' => mk(named::LEFT),
            b'E' => mk('5' as Codepoint),  // Numeric keypad 5
            b'F' => mk(named::END),        // PC/xterm style
            b'H' => mk(named::HOME),       // PC/xterm style
            b'P' => mk(named::F1),
            b'Q' => mk(named::F2),
            b'R' => mk(named::F3),
            b'S' => mk(named::F4),
            b'~' => match p00 {
                1 => mk(named::HOME), // VT220/tmux style
                2 => mk(named::INSERT),
                3 => mk(named::DELETE),
                4 => mk(named::END), // VT220/tmux style
                5 => mk(named::PAGE_UP),
                6 => mk(named::PAGE_DOWN),
                7 => mk(named::HOME), // rxvt style
                8 => mk(named::END),  // rxvt style
                11..=15 => mk(named::F1 + (p00 - 11) as Codepoint),
                17..=21 => mk(named::F6 + (p00 - 17) as Codepoint),
                23 | 24 => mk(named::F11 + (p00 - 23) as Codepoint),
                // rxvt style
                25 | 26 => Some(Key::with_mod(Modifiers::Shift, named::F3 + (p00 - 25) as Codepoint)),
                27 => mk(convert(params[2][0] as Codepoint, verase)),
                // rxvt style
                28 | 29 => Some(Key::with_mod(Modifiers::Shift, named::F5 + (p00 - 28) as Codepoint)),
                31 | 32 => Some(Key::with_mod(Modifiers::Shift, named::F7 + (p00 - 31) as Codepoint)),
                33 | 34 => Some(Key::with_mod(Modifiers::Shift, named::F9 + (p00 - 33) as Codepoint)),
                200 => {
                    self.paste_buffer = Some(Vec::new());
                    Some(Key::new(named::INVALID))
                }
                201 => {
                    if let Some(paste_buffer) = self.paste_buffer.take() {
                        // on_paste is called by the stdin watcher once the
                        // state is released
                        self.pending_paste = Some(paste_buffer);
                    }
                    Some(Key::new(named::INVALID))
                }
                _ => None,
            },
            b'u' => {
                let key = match p00 {
                    // Treat numpad keys the same as their non-numpad counterparts. Could add a numpad modifier here.
                    57399 => '0' as Codepoint,
                    57400 => '1' as Codepoint,
                    57401 => '2' as Codepoint,
                    57402 => '3' as Codepoint,
                    57403 => '4' as Codepoint,
                    57404 => '5' as Codepoint,
                    57405 => '6' as Codepoint,
                    57406 => '7' as Codepoint,
                    57407 => '8' as Codepoint,
                    57408 => '9' as Codepoint,
                    57409 => '.' as Codepoint,
                    57410 => '/' as Codepoint,
                    57411 => '*' as Codepoint,
                    57412 => '-' as Codepoint,
                    57413 => '+' as Codepoint,
                    57414 => named::RETURN,
                    57415 => '=' as Codepoint,
                    57417 => named::LEFT,
                    57418 => named::RIGHT,
                    57419 => named::UP,
                    57420 => named::DOWN,
                    57421 => named::PAGE_UP,
                    57422 => named::PAGE_DOWN,
                    57423 => named::HOME,
                    57424 => named::END,
                    57425 => named::INSERT,
                    57426 => named::DELETE,
                    _ => convert(p00 as Codepoint, verase),
                };
                Some(masked_key(key, convert(params[0][1] as Codepoint, verase)))
            }
            b'Z' => Some(shift(Key::new(named::TAB))),
            b'I' => Some(Key::new(named::FOCUS_IN)),
            b'O' => Some(Key::new(named::FOCUS_OUT)),
            b'M' | b'm' => {
                let sgr = private_mode == b'<';
                if !sgr && c != b'M' {
                    return None;
                }

                let b: Codepoint = if sgr { p00 as Codepoint } else { (next_char() as i32 - 32) as Codepoint };
                let x = (if sgr { params[1][0] } else { next_char() as i32 - 32 }) - 1;
                let y = (if sgr { params[2][0] } else { next_char() as i32 - 32 }) - 1;
                let coord = encode_coord(DisplayCoord::new(y - self.content_line_offset(), x));
                let m = parse_mask(((b >> 2) & 0x7) as i32);
                let code = b & 0x43;
                match code {
                    0..=2 => {
                        let button = match code {
                            0 => MouseButton::Left,
                            1 => MouseButton::Middle,
                            _ => MouseButton::Right,
                        };
                        return Some(self.mouse_button(m, button, coord, c == b'm'));
                    }
                    3 => {
                        if sgr {
                            return None;
                        }
                        let guess = if self.mouse_state == 0 { -1 } else { self.mouse_state.trailing_zeros() as i32 };
                        if (0..3).contains(&guess) {
                            let button = match guess {
                                0 => MouseButton::Left,
                                1 => MouseButton::Middle,
                                _ => MouseButton::Right,
                            };
                            return Some(self.mouse_button(m, button, coord, true));
                        }
                    }
                    64 => return Some(mouse_scroll(m, coord, false, self.wheel_scroll_amount)),
                    65 => return Some(mouse_scroll(m, coord, true, self.wheel_scroll_amount)),
                    _ => {}
                }
                Some(Key::with_mod(Modifiers::MousePos, coord))
            }
            _ => None,
        }
    }

    fn mouse_button(&mut self, mut m: Modifiers, button: MouseButton, coord: Codepoint, release: bool) -> Key {
        let mask = 1 << (button as i32);
        if !release {
            m |= if self.mouse_state & mask != 0 { Modifiers::MousePos } else { Modifiers::MousePress };
            self.mouse_state |= mask;
        } else {
            m |= Modifiers::MouseRelease;
            self.mouse_state &= !mask;
        }
        Key::with_mod(m | Key::to_modifier(button), coord)
    }

    fn draw_menu(&mut self) {
        // menu show may have not created the window if it did not fit.
        // so be tolerant.
        if !self.menu.win.is_some() {
            return;
        }

        let Menu { win, items, fg, bg, selected_item, first_item, columns, .. } = &mut self.menu;
        let item_count = items.len() as i32;
        if *columns == 0 {
            let win_width = win.size.column - 4;
            debug_assert!(win.size.line == 1);
            let mut pos: ColumnCount = 0;

            win.draw(DisplayCoord::new(0, 0), &[DisplayAtom::text(if *first_item > 0 { "< " } else { "" }, Face::default())], bg);

            let mut i = *first_item;
            while i < item_count && pos < win_width {
                let item = &items[i as usize];
                let item_width = item.length();
                let face = if i == *selected_item { *fg } else { *bg };
                win.draw(DisplayCoord::new(0, pos + 2), item.atoms(), &face);
                if pos + item_width >= win_width {
                    win.draw(DisplayCoord::new(0, win_width + 2), &[DisplayAtom::text("…", Face::default())], bg);
                }
                pos += item_width + 1;
                i += 1;
            }

            if i != item_count {
                win.draw(DisplayCoord::new(0, win_width + 3), &[DisplayAtom::text(">", Face::default())], bg);
            }

            self.dirty = true;
            return;
        }

        let menu_lines = div_round_up(item_count, *columns);
        let win_height = win.size.line;
        debug_assert!(win_height <= menu_lines);

        let column_width = (win.size.column - 1) / *columns;

        let mark_height = div_round_up(sq(win_height), menu_lines).min(win_height);

        let menu_cols = div_round_up(item_count, win.size.line);
        let first_col = *first_item / win.size.line;

        let mark_line = (win_height - mark_height) * first_col / 1.max(menu_cols - *columns);

        for line in 0..win_height {
            for col in 0..*columns {
                let item_idx = (first_col + col) * win.size.line + line;
                let face = if item_idx < item_count && item_idx == *selected_item { *fg } else { *bg };
                let atoms: &[DisplayAtom] = if item_idx < item_count { items[item_idx as usize].atoms() } else { &[] };
                win.draw(DisplayCoord::new(line, col * column_width), atoms, &face);
            }
            let is_mark = line >= mark_line && line < mark_line + mark_height;
            let size_column = win.size.column;
            win.draw(
                DisplayCoord::new(line, size_column - 1),
                &[DisplayAtom::text(if is_mark { "█" } else { "░" }, Face::default())],
                bg,
            );
        }
        self.dirty = true;
    }

    fn menu_show(&mut self, items: &[DisplayLine], mut anchor: DisplayCoord, fg: Face, bg: Face, style: MenuStyle) {
        if self.menu.win.is_some() {
            self.menu.win.destroy();
            self.dirty = true;
        }

        self.menu.fg = fg;
        self.menu.bg = bg;
        self.menu.style = style;
        self.menu.anchor = anchor;

        if self.dimensions.column <= 2 {
            return;
        }

        let item_count = items.len() as i32;
        self.menu.items.clear(); // make sure it is empty
        self.menu.items.reserve(items.len());
        let longest = items.iter().map(|i| i.length()).fold(1, ColumnCount::max);

        let max_width = self.dimensions.column - 1;
        let is_inline = style == MenuStyle::Inline;
        let is_search = style == MenuStyle::Search;
        self.menu.columns = if is_search {
            0
        } else if is_inline {
            1
        } else {
            (max_width / (longest + 1)).max(1)
        };

        let max_height = height_limit(style).min(anchor.line.max(self.dimensions.line - anchor.line - 1));
        let height = if is_search { 1 } else { max_height.min(div_round_up(item_count, self.menu.columns)) };

        if height == 0 {
            return;
        }

        let maxlen = if self.menu.columns > 1 && item_count > 1 { max_width / self.menu.columns - 1 } else { max_width };

        for item in items {
            let mut item = item.clone();
            item.trim(0, maxlen);
            debug_assert!(item.length() <= maxlen);
            self.menu.items.push(item);
        }

        if is_inline {
            anchor.line += self.content_line_offset();
        }

        let mut line = anchor.line + 1;
        let mut column = 0.max(anchor.column.min(self.dimensions.column - longest - 1));
        if is_search {
            line = if self.status_on_top { 0 } else { self.dimensions.line };
            column = self.dimensions.column / 2;
        } else if !is_inline {
            line = if self.status_on_top { 1 } else { self.dimensions.line - height };
        } else if line + height > self.dimensions.line && anchor.line >= height {
            line = anchor.line - height;
        }

        let width = if is_search {
            self.dimensions.column - self.dimensions.column / 2
        } else if is_inline {
            (longest + 1).min(self.dimensions.column)
        } else {
            self.dimensions.column
        };
        self.menu.win.create(DisplayCoord::new(line, column), DisplayCoord::new(height, width));
        self.menu.selected_item = item_count;
        self.menu.first_item = 0;

        self.draw_menu();

        if self.info.win.is_some() {
            let (title, content) = (self.info.title.clone(), self.info.content.clone());
            self.info_show(&title, &content, self.info.anchor, self.info.face, self.info.style);
        }
    }

    fn menu_select(&mut self, selected: i32) {
        let item_count = self.menu.items.len() as i32;
        if selected < 0 || selected >= item_count {
            self.menu.selected_item = -1;
            self.menu.first_item = 0;
        } else if self.menu.columns == 0 {
            // Do not columnize
            self.menu.selected_item = selected;
            let width = self.menu.win.size.column - 3;
            let mut first = 0;
            let mut item_col: ColumnCount = 0;
            for i in 0..=selected {
                let item_width = self.menu.items[i as usize].length() + 1;
                if item_col + item_width > width {
                    first = i;
                    item_col = item_width;
                } else {
                    item_col += item_width;
                }
            }
            self.menu.first_item = first;
        } else {
            self.menu.selected_item = selected;
            let size_line = self.menu.win.size.line;
            let menu_cols = div_round_up(item_count, size_line);
            let first_col = self.menu.first_item / size_line;
            let selected_col = self.menu.selected_item / size_line;
            if selected_col < first_col {
                self.menu.first_item = selected_col * size_line;
            }
            if selected_col >= first_col + self.menu.columns {
                self.menu.first_item = selected_col.min(menu_cols - self.menu.columns) * size_line;
            }
        }
        self.draw_menu();
    }

    fn menu_hide(&mut self) {
        if !self.menu.win.is_some() {
            return;
        }

        self.menu.items.clear();
        self.menu.win.destroy();
        self.dirty = true;

        // Recompute info as it does not have to avoid the menu anymore
        if self.info.win.is_some() {
            let (title, content) = (self.info.title.clone(), self.info.content.clone());
            self.info_show(&title, &content, self.info.anchor, self.info.face, self.info.style);
        }
    }

    fn info_show(&mut self, title: &DisplayLine, content: &DisplayLineList, mut anchor: DisplayCoord, face: Face, style: InfoStyle) {
        self.info_hide();

        self.info.title = title.clone();
        self.info.content = content.clone();
        self.info.anchor = anchor;
        self.info.face = face;
        self.info.style = style;

        let assistant = self.assistant;
        let framed = style == InfoStyle::Prompt || style == InfoStyle::Modal;
        let assisted = style == InfoStyle::Prompt && !assistant.is_empty();
        let assistant_width = if assisted { column_length(assistant[0].as_bytes()) } else { 0 };

        let menu_rect = self.menu.win.rect();
        let mut max_size = self.dimensions;
        if style == InfoStyle::MenuDoc {
            max_size.column = (self.dimensions.column - (menu_rect.pos.column + menu_rect.size.column)).max(menu_rect.pos.column);
        } else if style != InfoStyle::Modal {
            max_size.line -= menu_rect.size.line;
        }

        let max_content_width = (if self.info_max_width > 0 { max_size.column.min(self.info_max_width) } else { max_size.column })
            - (if framed { 4 } else { 0 })
            - assistant_width;
        if max_content_width <= 0 {
            return;
        }

        let compute_size = |lines: &DisplayLineList| -> DisplayCoord {
            DisplayCoord::new(lines.len() as LineCount, lines.iter().fold(0, |c, l| c.max(l.length())))
        };

        let mut content_size = compute_size(content);
        let wrap = content_size.column > max_content_width;
        let mut wrapped_content = DisplayLineList::new();
        if wrap {
            wrapped_content = wrap_lines(content, max_content_width);
            content_size = compute_size(&wrapped_content);
        }
        let lines: &DisplayLineList = if wrap { &wrapped_content } else { content };
        let lines_count = lines.len() as LineCount;

        let mut size = DisplayCoord::new(content_size.line, content_size.column.max(title.length() + if framed { 2 } else { 0 }));
        if framed {
            size += DisplayCoord::new(2, 4);
        }
        if assisted {
            size = DisplayCoord::new((assistant.len() as LineCount - 1).max(size.line), size.column + assistant_width);
        }
        size = DisplayCoord::new(max_size.line.min(size.line), max_size.column.min(size.column));

        if (framed && size.line < 3) || size.line <= 0 {
            return;
        }

        let rect = Rect { pos: DisplayCoord::new(self.content_line_offset(), 0), size: self.dimensions };
        if style == InfoStyle::Prompt {
            anchor = DisplayCoord::new(if self.status_on_top { 0 } else { self.dimensions.line }, self.dimensions.column - 1);
            anchor = compute_pos(anchor, size, rect, menu_rect, style == InfoStyle::InlineAbove);
        } else if style == InfoStyle::Modal {
            let half = |c: DisplayCoord| DisplayCoord::new(c.line / 2, c.column / 2);
            anchor = rect.pos + half(rect.size) - half(size);
        } else if style == InfoStyle::MenuDoc {
            let right_max_width = self.dimensions.column - (menu_rect.pos.column + menu_rect.size.column);
            let left_max_width = menu_rect.pos.column;
            anchor.line = menu_rect.pos.line;
            if size.column <= right_max_width || right_max_width >= left_max_width {
                anchor.column = menu_rect.pos.column + menu_rect.size.column;
            } else {
                anchor.column = menu_rect.pos.column - size.column;
            }
        } else {
            anchor = compute_pos(anchor, size, rect, menu_rect, style == InfoStyle::InlineAbove);
            anchor.line += self.content_line_offset();
        }

        let win = &mut self.info.win;
        win.create(anchor, size);
        for line in 0..size.line {
            let mut pos = DisplayCoord::new(line, 0);

            const DASH: Codepoint = '─' as Codepoint;
            const DOTTED_DASH: Codepoint = '┄' as Codepoint;
            if assisted {
                let assistant_size = assistant.len() as LineCount;
                let assistant_top_margin = (size.line - assistant_size + 1) / 2;
                let assistant_line = if line >= assistant_top_margin {
                    assistant[(line - assistant_top_margin).min(assistant_size - 1) as usize]
                } else {
                    assistant[(assistant_size - 1) as usize]
                };

                draw_str(win, &mut pos, assistant_line.as_bytes().to_vec(), &face);
            }
            if !framed {
                draw_line(win, &mut pos, &lines[line as usize], &face);
            } else if line == 0 {
                if title.atoms().is_empty() || content_size.column < 2 {
                    let dashes = string_of_cp_cols(DASH, content_size.column);
                    draw_str(win, &mut pos, concat(&["╭─".as_bytes(), &dashes, "─╮".as_bytes()]), &face);
                } else {
                    let mut trimmed_title = title.clone();
                    trimmed_title.trim(0, content_size.column - 2);
                    let dash_count = content_size.column - trimmed_title.length() - 2;
                    let left = string_of_cp_cols(DASH, dash_count / 2);
                    let right = string_of_cp_cols(DASH, dash_count - dash_count / 2);
                    draw_str(win, &mut pos, concat(&["╭─".as_bytes(), &left, "┤".as_bytes()]), &face);
                    draw_line(win, &mut pos, &trimmed_title, &face);
                    draw_str(win, &mut pos, concat(&["├".as_bytes(), &right, "─╮".as_bytes()]), &face);
                }
            } else if line < size.line - 1 && line <= lines_count {
                let mut info_line = lines[(line - 1) as usize].clone();
                let trimmed = info_line.trim(0, content_size.column);
                let padding = content_size.column - info_line.length();
                draw_str(win, &mut pos, "│ ".as_bytes().to_vec(), &face);
                draw_line(win, &mut pos, &info_line, &face);
                pos.column += padding;
                draw_str(win, &mut pos, (if trimmed { "…│" } else { " │" }).as_bytes().to_vec(), &face);
            } else if line == (lines_count + 1).min(size.line - 1) {
                let dashes = string_of_cp_cols(if line > lines_count { DASH } else { DOTTED_DASH }, content_size.column);
                draw_str(win, &mut pos, "╰─".as_bytes().to_vec(), &face);
                draw_str(win, &mut pos, dashes, &face);
                draw_str(win, &mut pos, "─╯".as_bytes().to_vec(), &face);
            }
        }
        self.dirty = true;
    }

    fn info_hide(&mut self) {
        if !self.info.win.is_some() {
            return;
        }
        self.info.win.destroy();
        self.dirty = true;
    }

    fn enable_mouse(&mut self, enabled: bool) {
        if enabled == self.mouse_enabled {
            return;
        }

        self.mouse_enabled = enabled;
        if enabled {
            write_stdout(concat!(
                "\x1b[?1006h", // force SGR mode
                "\x1b[?1000h", // enable mouse
                "\x1b[?1002h", // force enable report mouse position
            ).as_bytes());
        } else {
            write_stdout(concat!("\x1b[?1002l", "\x1b[?1000l", "\x1b[?1006l").as_bytes());
        }
    }

    fn set_ui_options(&mut self, options: &UiOptions) {
        let find = |name: &str| -> Option<&[u8]> { options.get(name.as_bytes()).map(|v| v.as_slice()) };

        let assistant = find("terminal_assistant").unwrap_or(b"clippy");
        match assistant {
            b"clippy" => self.assistant = ASSISTANT_CLIPPY,
            b"cat" => self.assistant = ASSISTANT_CAT,
            b"dilbert" => self.assistant = ASSISTANT_DILBERT,
            b"none" | b"off" => self.assistant = &[],
            _ => {}
        }

        let to_bool = |s: &[u8]| s == b"yes" || s == b"true";

        self.status_on_top = find("terminal_status_on_top").map(to_bool).unwrap_or(false);
        self.set_title = find("terminal_set_title").map(to_bool).unwrap_or(true);
        self.title = find("terminal_title").map(|s| s.to_vec());

        let synchronized = find("terminal_synchronized").map(to_bool);
        self.synchronized.set = synchronized.is_some();
        self.synchronized.requested = synchronized.unwrap_or(false);
        if !self.synchronized.queried && !self.synchronized.set {
            write_stdout(b"\x1b[?2026$p");
            self.synchronized.queried = true;
        }

        self.shift_function_key =
            find("terminal_shift_function_key").and_then(str_to_int_ifp).unwrap_or(DEFAULT_SHIFT_FUNCTION_KEY);

        self.enable_mouse(find("terminal_enable_mouse").map(to_bool).unwrap_or(true));
        self.wheel_scroll_amount = find("terminal_wheel_scroll_amount").and_then(str_to_int_ifp).unwrap_or(3);

        self.padding_char = find("terminal_padding_char")
            .map(|s| if column_length(s) < 1 { ' ' as Codepoint } else { char_at(s, 0) })
            .unwrap_or('~' as Codepoint);
        self.padding_fill = find("terminal_padding_fill").map(to_bool).unwrap_or(false);

        let new_cursor_native = find("terminal_cursor_native").map(to_bool).unwrap_or(false);
        if new_cursor_native != self.cursor_native {
            self.cursor_native = new_cursor_native;
            write_stdout(if self.cursor_native { b"\x1b[?25h" } else { b"\x1b[?25l" });
        }

        self.info_max_width = find("terminal_info_max_width").and_then(str_to_int_ifp).unwrap_or(0);
    }
}

impl Inner {
    fn on_key(&self) -> Option<KeyCallback> {
        self.on_key.borrow().clone()
    }

    fn handle_pending_suspend(&self) -> Result<()> {
        if SUSPEND_PENDING.swap(false, Ordering::SeqCst) {
            return self.suspend();
        }
        Ok(())
    }

    /// stdin watcher callback
    fn on_stdin(&self) -> Result<()> {
        self.handle_pending_suspend()?;

        if self.on_key.borrow().is_none() {
            return Ok(());
        }

        loop {
            let key = self.state.borrow_mut().get_next_key();
            let paste = self.state.borrow_mut().pending_paste.take();
            if let Some(paste) = paste {
                let on_paste = self.on_paste.borrow().clone();
                if let Some(on_paste) = on_paste {
                    on_paste(&paste)?;
                }
            }
            let Some(key) = key else { break };

            if key == ctrl(self::key('z')) {
                unsafe { libc::kill(0, libc::SIGTSTP) }; // We suspend at this line
                self.handle_pending_suspend()?;
            } else if key != Key::new(named::INVALID) {
                // on_key is cleared if the TerminalUI got dropped by a previous key
                let Some(on_key) = self.on_key() else { break };
                on_key(key)?;
            }
        }
        Ok(())
    }

    fn suspend(&self) -> Result<()> {
        let mut result = Ok(());
        if let Some(on_key) = self.on_key() {
            result = on_key(Key::new(named::FOCUS_OUT));
        }

        {
            let mut state = self.state.borrow_mut();
            let mouse_enabled = state.mouse_enabled;
            state.enable_mouse(false);
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &state.original_termios) };
            TerminalUI::restore_terminal();

            unsafe {
                let current = set_signal_handler(libc::SIGTSTP, libc::SIG_DFL);
                let mut unblock_sigtstp: libc::sigset_t = std::mem::zeroed();
                let mut old_mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut unblock_sigtstp);
                libc::sigaddset(&mut unblock_sigtstp, libc::SIGTSTP);
                libc::sigprocmask(libc::SIG_UNBLOCK, &unblock_sigtstp, &mut old_mask);

                libc::raise(libc::SIGTSTP); // suspend here

                set_signal_handler(libc::SIGTSTP, current);
                libc::sigprocmask(libc::SIG_SETMASK, &old_mask, std::ptr::null_mut());
            }

            TerminalUI::setup_terminal();
            state.check_resize(true);
            state.set_raw_mode();
            state.enable_mouse(mouse_enabled);

            state.refresh(true);
        }

        if let Some(on_key) = self.on_key() {
            let res = on_key(Key::new(named::FOCUS_IN));
            if result.is_ok() {
                result = res;
            }
        }
        result
    }
}

impl TerminalUI {
    /// Requires the EventManager to be created. Sets up the terminal (alternate
    /// screen, raw mode, mouse) and installs SIGWINCH, SIGHUP and SIGTSTP
    /// handlers, which are restored on drop.
    pub fn new() -> Result<TerminalUI> {
        if unsafe { libc::isatty(1) } == 0 {
            return Err(Error::runtime("stdout is not a tty"));
        }

        let mut original_termios: libc::termios = unsafe { std::mem::zeroed() };
        unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original_termios) };

        let state = State {
            window: Window::default(),
            screen: Screen::default(),
            dimensions: DisplayCoord::default(),
            original_termios,
            menu: Menu {
                win: Window::default(),
                items: Vec::new(),
                fg: Face::default(),
                bg: Face::default(),
                anchor: DisplayCoord::default(),
                style: MenuStyle::Prompt,
                selected_item: 0,
                first_item: 0,
                columns: 1,
            },
            info: Info {
                win: Window::default(),
                title: DisplayLine::default(),
                content: DisplayLineList::new(),
                face: Face::default(),
                anchor: DisplayCoord::default(),
                style: InfoStyle::Prompt,
            },
            cursor_pos: DisplayCoord::default(),
            stdin_watcher: None,
            paste_buffer: None,
            pending_paste: None,
            status_on_top: false,
            assistant: ASSISTANT_CLIPPY,
            mouse_enabled: false,
            wheel_scroll_amount: 3,
            mouse_state: 0,
            shift_function_key: DEFAULT_SHIFT_FUNCTION_KEY,
            set_title: true,
            title: None,
            synchronized: Synchronized::default(),
            padding_char: '~' as Codepoint,
            padding_fill: false,
            cursor_native: false,
            dirty: false,
            resize_pending: false,
            status_len: 0,
            status_pos: 0,
            status_cursor_pos: 0,
            info_max_width: 0,
        };

        let inner = Rc::new(Inner { state: RefCell::new(state), on_key: RefCell::new(None), on_paste: RefCell::new(None) });

        let weak: Weak<Inner> = Rc::downgrade(&inner);
        let watcher = FDWatcher::new(libc::STDIN_FILENO, FdEvents::Read, EventMode::Urgent, move |_, _, _| {
            match weak.upgrade() {
                Some(inner) => inner.on_stdin(),
                None => Ok(()),
            }
        });

        {
            let mut state = inner.state.borrow_mut();
            state.stdin_watcher = Some(watcher);

            Self::setup_terminal();
            state.set_raw_mode();
            state.enable_mouse(true);

            set_signal_handler(libc::SIGWINCH, handler_ptr(resize_signal_handler));
            set_signal_handler(libc::SIGHUP, handler_ptr(hangup_signal_handler));
            set_signal_handler(libc::SIGTSTP, handler_ptr(suspend_signal_handler));

            state.check_resize(true);
            state.redraw(false);
        }

        Ok(TerminalUI { inner })
    }

    /// Suspend the process (SIGTSTP), restoring the terminal while suspended.
    /// Normally triggered by <c-z> or by receiving SIGTSTP.
    pub fn suspend(&self) -> Result<()> {
        self.inner.suspend()
    }

    /// async-signal-safe
    pub fn setup_terminal() {
        write_stdout(concat!(
            "\x1b[?1049h", // enable alternative screen buffer
            "\x1b[?1004h", // enable focus notify
            "\x1b[>4;1m",  // request CSI u style key reporting
            "\x1b[>5u",    // kitty progressive enhancement - report shifted key codes
            "\x1b[22t",    // save the current window title
            "\x1b[?25l",   // hide cursor
            "\x1b=",       // set application keypad mode, so the keypad keys send unique codes
            "\x1b[?2004h", // force enable bracketed-paste events
        ).as_bytes());
    }

    /// async-signal-safe
    pub fn restore_terminal() {
        write_stdout(concat!(
            "\x1b>",
            "\x1b[?25h",
            "\x1b[23t",
            "\x1b[<u",
            "\x1b[>4;0m",
            "\x1b[?1004l",
            "\x1b[?1049l",
            "\x1b[?2004l",
            "\x1b[m", // set the terminal output back to default colours and style
        ).as_bytes());
    }
}

impl Drop for TerminalUI {
    fn drop(&mut self) {
        // Make a running stdin watcher callback stop dispatching keys
        *self.inner.on_key.borrow_mut() = None;
        *self.inner.on_paste.borrow_mut() = None;

        let watcher = {
            let mut state = self.inner.state.borrow_mut();
            if !TERMINAL_HUNGUP.load(Ordering::SeqCst) {
                state.enable_mouse(false);
                Self::restore_terminal();
                unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &state.original_termios) };
            }
            state.stdin_watcher.take()
        };
        set_signal_handler(libc::SIGWINCH, libc::SIG_DFL);
        set_signal_handler(libc::SIGHUP, libc::SIG_IGN);
        set_signal_handler(libc::SIGTSTP, libc::SIG_DFL);
        drop(watcher);
    }
}

impl UserInterface for TerminalUI {
    fn is_ok(&self) -> bool {
        self.inner.state.borrow().window.is_some()
    }

    fn menu_show(&self, choices: &[DisplayLine], anchor: DisplayCoord, fg: Face, bg: Face, style: MenuStyle) {
        self.inner.state.borrow_mut().menu_show(choices, anchor, fg, bg, style)
    }

    fn menu_select(&self, selected: i32) {
        self.inner.state.borrow_mut().menu_select(selected)
    }

    fn menu_hide(&self) {
        self.inner.state.borrow_mut().menu_hide()
    }

    fn info_show(&self, title: &DisplayLine, content: &DisplayLineList, anchor: DisplayCoord, face: Face, style: InfoStyle) {
        self.inner.state.borrow_mut().info_show(title, content, anchor, face, style)
    }

    fn info_hide(&self) {
        self.inner.state.borrow_mut().info_hide()
    }

    fn draw(&self, display_buffer: &DisplayBuffer, cursor_pos: DisplayCoord, default_face: &Face, padding_face: &Face, _widget_columns: ColumnCount) {
        self.inner.state.borrow_mut().draw(display_buffer, cursor_pos, default_face, padding_face)
    }

    fn draw_status(&self, prompt: &DisplayLine, content: &DisplayLine, cursor_pos: ColumnCount, mode_line: &DisplayLine, default_face: &Face, _style: StatusStyle) {
        self.inner.state.borrow_mut().draw_status(prompt, content, cursor_pos, mode_line, default_face)
    }

    fn dimensions(&self) -> DisplayCoord {
        self.inner.state.borrow().dimensions
    }

    fn refresh(&self, force: bool) {
        self.inner.state.borrow_mut().refresh(force)
    }

    fn set_on_key(&self, callback: OnKeyCallback) {
        *self.inner.on_key.borrow_mut() = Some(Rc::from(callback));
        EventManager::force_signal(0);
    }

    fn set_on_paste(&self, callback: OnPasteCallback) {
        *self.inner.on_paste.borrow_mut() = Some(Rc::from(callback));
    }

    fn set_ui_options(&self, options: &UiOptions) {
        self.inner.state.borrow_mut().set_ui_options(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(atoms: &[(&str, ColumnCount)]) -> Line {
        let mut l = Line::default();
        for (text, skip) in atoms {
            l.atoms.push(Atom { text: text.as_bytes().to_vec(), skip: *skip, face: Face::default() });
        }
        l
    }

    fn texts(l: &Line) -> Vec<(String, ColumnCount)> {
        l.atoms.iter().map(|a| (String::from_utf8(a.text.clone()).unwrap(), a.skip)).collect()
    }

    #[test]
    fn test_line_resize_and_erase() {
        let mut l = line(&[("abc", 0), ("def", 2)]);
        l.resize(4);
        assert_eq!(texts(&l), vec![("abc".into(), 0), ("d".into(), 0)]);
        l.resize(6);
        assert_eq!(texts(&l), vec![("abc".into(), 0), ("d".into(), 2)]);

        let mut l = line(&[("abcdef", 0)]);
        let it = l.erase_range(2, 2);
        assert_eq!(it, 1);
        assert_eq!(texts(&l), vec![("ab".into(), 0), ("ef".into(), 0)]);

        let mut l = line(&[("ab", 0), ("cd", 0), ("ef", 0)]);
        let it = l.erase_range(1, 4);
        assert_eq!(it, 1);
        assert_eq!(texts(&l), vec![("a".into(), 0), ("f".into(), 0)]);

        // double width codepoint cut in the middle (needs a locale where it is double width)
        if codepoint_width(0x6F22) != 2 {
            return;
        }
        let mut l = line(&[("a漢b", 0)]);
        let it = l.erase_range(1, 1);
        assert_eq!(texts(&l), vec![("a".into(), 0), (" b".into(), 0)]);
        assert_eq!(it, 1);
    }

    #[test]
    fn test_synchronized_output() {
        let mut screen = Screen::default();
        screen.win.create(DisplayCoord::default(), DisplayCoord::new(3, 5));
        screen.hashes = vec![0; 3];
        let set_lines = |screen: &mut Screen, texts: [&str; 3]| {
            for (i, t) in texts.iter().enumerate() {
                screen.win.lines.as_mut().unwrap()[i] = line(&[(t, 0)]);
            }
        };
        let mut writer = Writer::new();
        set_lines(&mut screen, ["a", "b", "c"]);
        screen.output(false, true, &mut writer);
        assert_eq!(writer.buffer, b"\x1b[?2026h\x1b[1Ha\x1b[2Hb\x1b[3Hc\x1b[?2026l");
        writer.buffer.clear();

        // scroll down by one line: insert a line at the top, drop the last
        set_lines(&mut screen, ["z", "a", "b"]);
        screen.output(false, true, &mut writer);
        assert_eq!(writer.buffer, b"\x1b[?2026h\x1b[3H\x1b[1M\x1b[1H\x1b[1Lz\x1b[?2026l");
        writer.buffer.clear();
    }

    #[test]
    fn test_fix_atom_text() {
        assert_eq!(fix_atom_text(b"a\tb"), "a\u{2409}b".as_bytes());
    }

    #[test]
    fn test_set_face() {
        let mut active = Face::default();
        let mut writer = Writer::new();
        let face = Face::with_attr(Color::rgb(1, 2, 3), crate::face::NamedColor::Red, Attribute::Bold);
        set_face(&mut active, &face, &mut writer);
        assert_eq!(writer.buffer, b"\x1b[;1;38;2;1;2;3;41m");
        writer.buffer.clear();
    }
}
