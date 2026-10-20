//! Port of input_handler.hh/cc
//!
//! Input modes are reference counted trait objects held on the input
//! handler's mode stack. The current mode is cloned out of the stack before
//! a key is dispatched to it, so that it stays alive if it gets popped while
//! handling the key (the C++ code uses a RefPtr keep_alive for that).

use crate::command_manager::{expand, CommandParser, ShellContext};
use crate::completion::{complete_filename, CandidateList, CompletionFlags, Completions, FilenameFlags};
use crate::context::{Context, ContextFlags, ScopedEdition};
use crate::debug::write_to_debug_buffer;
use crate::display_buffer::{DisplayAtom, DisplayLine};
use crate::event_manager::{TimePoint, Timer, TimerRef};
use crate::exception::{Error, Result};
use crate::face::{Face, FaceRegistry};
use crate::format::Cp;
use crate::hook_manager::Hook;
use crate::insert_completer::InsertCompleter;
use crate::keymap_manager::KeymapMode;
use crate::keys::{alt, ctrl, key_to_str, named, shift, Key, Modifiers, MouseButton};
use crate::normal::{get_normal_command, paste_pos, NormalParams, PasteMode};
use crate::option_types::EnumDesc;
use crate::ranked_match::RankedMatch;
use crate::regex::Regex;
use crate::register_manager::{Register, RegisterManager};
use crate::selection::{insert_at, Selection, SelectionList};
use crate::string_utils::{char_length, column_length, join, prefix_match, quoter, string_of_cp, substr_chars, Quoting};
use crate::unicode::{is_horizontal_blank, is_punctuation, is_word_type, to_upper, WordType, DEFAULT_EXTRA_WORD_CHARS};
use crate::units::{BufferCoord, CharCount, Codepoint, ColumnCount, DisplayCoord, LineCount};
use crate::user_interface::{InfoStyle, MenuStyle, StatusStyle};
use crate::utf8;
use crate::utils::ScopedSetBool;
use crate::word_db::{for_n_best, get_word_db};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptEvent {
    Change,
    Abort,
    Validate,
}

pub type PromptCallback = Rc<dyn Fn(&[u8], PromptEvent, &Context) -> Result<()>>;
pub type PromptCompleter = Rc<dyn Fn(&Context, &[u8], usize) -> Completions>;
pub type KeyCallback = Rc<dyn Fn(Key, &Context) -> Result<()>>;
pub type IdleCallback = Rc<dyn Fn(&TimerRef) -> Result<()>>;

crate::kak_flags! {
    pub struct PromptFlags: u32 {
        const None = 0;
        const Password = 1 << 0;
        const DropHistoryEntriesWithBlankPrefix = 1 << 1;
        const Search = 1 << 2;
        const Command = 1 << 3;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InsertMode {
    Insert,
    Append,
    Replace,
    InsertAtLineBegin,
    AppendAtLineEnd,
    OpenLineBelow,
    OpenLineAbove,
}

#[derive(Clone, Default)]
pub struct ModeInfo {
    pub display_line: DisplayLine,
    pub normal_params: Option<NormalParams>,
}

crate::kak_flags! {
    pub struct AutoInfo: u32 {
        const None = 0;
        const Command = 1 << 0;
        const OnKey   = 1 << 1;
        const Normal  = 1 << 2;
    }
}
impl EnumDesc for AutoInfo {
    fn desc() -> &'static [(Self, &'static str)] {
        &[(AutoInfo::Command, "command"), (AutoInfo::OnKey, "onkey"), (AutoInfo::Normal, "normal")]
    }
}
crate::flags_option!(AutoInfo);

crate::kak_flags! {
    pub struct AutoComplete: u32 {
        const None = 0;
        const Insert = 0b01;
        const Prompt = 0b10;
    }
}
impl EnumDesc for AutoComplete {
    fn desc() -> &'static [(Self, &'static str)] {
        &[(AutoComplete::Insert, "insert"), (AutoComplete::Prompt, "prompt")]
    }
}
crate::flags_option!(AutoComplete);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnHiddenCursor {
    PreserveSelections,
    MoveCursor,
    MoveCursorAndAnchor,
}

/// State used to repeat the last insertion
struct Insertion {
    recording: i32,
    repeating: bool,
    mode: InsertMode,
    keys: Vec<Key>,
    disable_hooks: bool,
    count: i32,
}

pub trait InputMode: Any {
    fn input_handler(&self) -> Rc<InputHandler>;
    fn on_key(&self, key: Key) -> Result<()>;
    fn paste(&self, content: &[u8]) -> Result<()> {
        default_paste(self.input_handler().context(), content)
    }
    fn on_raw_key(&self) {}
    fn on_enabled(&self, _from_pop: bool) -> Result<()> {
        Ok(())
    }
    fn on_disabled(&self, _from_push: bool) -> Result<()> {
        Ok(())
    }
    fn refresh_ifn(&self) {}
    fn take_pending_count(&self) -> u32 {
        1
    }
    fn mode_info(&self) -> ModeInfo;
    fn keymap_mode(&self) -> KeymapMode;
    fn name(&self) -> Vec<u8>;
    /// Release resources tied to the context, called when the input handler is destroyed
    fn teardown(&self, _context: &Context) {}
    fn as_any(&self) -> &dyn Any;
}

fn is_mode<T: 'static>(mode: &dyn InputMode) -> bool {
    mode.as_any().is::<T>()
}

fn same_mode(a: &Rc<dyn InputMode>, b: &dyn InputMode) -> bool {
    std::ptr::eq(Rc::as_ptr(a) as *const (), b as *const dyn InputMode as *const ())
}

fn default_paste(context: &Context, content: &[u8]) -> Result<()> {
    let res = (|| -> Result<()> {
        let buffer = context.buffer();
        let linewise = content.last() == Some(&b'\n');
        let _edition = ScopedEdition::new(context);
        let _selection_edition = crate::context::ScopedSelectionEdition::new(context);
        context.selections().for_each(
            |_, sel| {
                let (min, max) = sel.min_max_mut();
                let range = buffer.insert(paste_pos(&buffer, *min, *max, PasteMode::Insert, linewise), content)?;
                *min = range.begin;
                *max = if range.end > range.begin { buffer.char_prev(range.end) } else { range.begin };
                Ok(())
            },
            false,
        )
    })();
    match res {
        Ok(()) => Ok(()),
        Err(error) if error.is_runtime() => {
            write_to_debug_buffer(&kformat!("Error: {}", error.what()));
            context.print_status(DisplayLine::from_str_face(error.what().to_vec(), context.faces().face("Error")));
            context.hooks().run_hook(Hook::RuntimeError, error.what(), context)
        }
        Err(error) => Err(error),
    }
}

fn get_idle_timeout(context: &Context) -> Duration {
    Duration::from_millis(context.options().val::<i32>("idle_timeout").max(0) as u64)
}

fn get_fs_check_timeout(context: &Context) -> Duration {
    Duration::from_millis(context.options().val::<i32>("fs_check_timeout").max(0) as u64)
}

fn now_plus(d: Duration) -> TimePoint {
    Some(Instant::now() + d)
}

fn is_draft(context: &Context) -> bool {
    context.flags().has(ContextFlags::Draft)
}

/// Port of MouseHandler
#[derive(Default)]
struct MouseHandler {
    dragging: Cell<bool>,
    dragging_valid: Cell<bool>,
    anchor: Cell<BufferCoord>,
}

impl MouseHandler {
    fn start_dragging(&self, context: &Context) {
        self.stop_dragging(context);
        let valid = !is_draft(context) && context.has_buffer();
        if valid {
            context.begin_selection_edition();
        }
        self.dragging_valid.set(valid);
        self.dragging.set(true);
    }

    fn stop_dragging(&self, context: &Context) {
        if self.dragging.get() {
            if self.dragging_valid.get() {
                context.end_selection_edition();
            }
            self.dragging.set(false);
            self.dragging_valid.set(false);
        }
    }

    fn handle_key(&self, key: Key, context: &Context) -> Result<bool> {
        if !context.has_window() {
            return Ok(false);
        }

        let buffer = context.buffer();

        // bits above these potentially store additional information
        const MASK: u32 = 0x7FF;
        let modifiers =
            Modifiers::Control.0 | Modifiers::Alt.0 | Modifiers::Shift.0 | Modifiers::MouseButtonMask.0 | !MASK;
        let window = context.window();
        match key.modifiers.0 & !modifiers {
            m if m == Modifiers::MousePress.0 => match key.mouse_button() {
                MouseButton::Right => {
                    self.stop_dragging(context);
                    let Some(cursor) = window.buffer_coord(key.coord()) else {
                        context.ensure_cursor_visible.set(false);
                        return Ok(true);
                    };
                    let _selection_edition = crate::context::ScopedSelectionEdition::new(context);
                    let mut selections = context.selections();
                    if key.modifiers.has(Modifiers::Control) {
                        let anchor = selections[0].anchor();
                        selections.assign(vec![Selection::from_range(anchor, cursor)]);
                    } else {
                        let anchor = selections.main().anchor();
                        *selections.main_mut() = Selection::from_range(anchor, cursor);
                    }
                    selections.sort_and_merge_overlapping();
                    Ok(true)
                }
                MouseButton::Left => {
                    self.start_dragging(context);
                    let Some(anchor) = window.buffer_coord(key.coord()) else {
                        context.ensure_cursor_visible.set(false);
                        return Ok(true);
                    };
                    self.anchor.set(anchor);
                    if !key.modifiers.has(Modifiers::Control) {
                        *context.selections_write_only() = SelectionList::new(buffer.clone(), Selection::new(anchor));
                    } else {
                        let mut selections = context.selections();
                        let main = selections.size();
                        selections.push(Selection::new(anchor));
                        selections.set_main_index(main);
                        selections.sort_and_merge_overlapping();
                    }
                    Ok(true)
                }
                _ => Ok(true),
            },
            m if m == Modifiers::MouseRelease.0 => {
                let cursor = window.buffer_coord(key.coord());
                let (true, Some(cursor)) = (self.dragging.get(), cursor) else {
                    context.ensure_cursor_visible.set(false);
                    return Ok(true);
                };
                {
                    let mut selections = context.selections();
                    *selections.main_mut() = Selection::from_range(buffer.clamp(self.anchor.get()), cursor);
                    selections.sort_and_merge_overlapping();
                }
                self.stop_dragging(context);
                Ok(true)
            }
            m if m == Modifiers::MousePos.0 => {
                let cursor = window.buffer_coord(key.coord());
                let (true, Some(cursor)) = (self.dragging.get(), cursor) else {
                    context.ensure_cursor_visible.set(false);
                    return Ok(true);
                };
                let mut selections = context.selections();
                *selections.main_mut() = Selection::from_range(buffer.clamp(self.anchor.get()), cursor);
                selections.sort_and_merge_overlapping();
                Ok(true)
            }
            m if m == Modifiers::Scroll.0 => {
                scroll_window(
                    context,
                    key.scroll_amount(),
                    if self.dragging.get() { OnHiddenCursor::MoveCursor } else { OnHiddenCursor::PreserveSelections },
                )?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

pub const REGISTER_DOC: &str = "Special registers:\n\
[0-9]: selections capture group\n\
%:     buffer name\n\
.:     selection contents\n\
#:     selection index\n\
_:     null register\n\
\":     default yank/paste register\n\
@:     default macro register\n\
/:     default search register\n\
^:     default mark register\n\
|:     default shell command register\n\
::     last entered command\n";

// ---------------------------------------------------------------------------
// Normal mode

#[derive(Clone, Copy, PartialEq, Eq)]
enum NormalState {
    Normal,
    SingleCommand,
    PopOnEnabled,
}

pub struct Normal {
    input_handler: Weak<InputHandler>,
    self_weak: Weak<Normal>,
    params: Cell<NormalParams>,
    hooks_disabled: Cell<bool>,
    in_on_key: Cell<i32>,
    idle_timer: Timer,
    fs_check_timer: Timer,
    mouse_handler: MouseHandler,
    state: Cell<NormalState>,
}

impl Normal {
    fn new(input_handler: &Rc<InputHandler>, single_command: bool) -> Rc<Normal> {
        let draft = is_draft(input_handler.context());
        Rc::new_cyclic(|w: &Weak<Normal>| {
            let idle_timer = if draft {
                Timer::none()
            } else {
                let w = w.clone();
                Timer::new(
                    None,
                    move |_| {
                        // hook could trigger pop_mode()
                        let Some(this) = w.upgrade() else { return Ok(()) };
                        let ih = this.input_handler();
                        let context = ih.context();
                        if let Ok(client) = context.try_client() {
                            client.clear_pending();
                        }
                        context.hooks().run_hook(Hook::NormalIdle, b"", context)
                    },
                    crate::event_manager::EventMode::Normal,
                )
            };
            let fs_check_timer = if draft {
                Timer::none()
            } else {
                let w = w.clone();
                Timer::new(
                    None,
                    move |timer| {
                        let Some(this) = w.upgrade() else { return Ok(()) };
                        let ih = this.input_handler();
                        let context = ih.context();
                        if let Ok(client) = context.try_client() {
                            client.check_if_buffer_needs_reloading()?;
                        }
                        timer.set_next_date(now_plus(get_fs_check_timeout(context)));
                        Ok(())
                    },
                    crate::event_manager::EventMode::Normal,
                )
            };
            Normal {
                input_handler: Rc::downgrade(input_handler),
                self_weak: w.clone(),
                params: Cell::new(NormalParams { count: 0, reg: 0 }),
                hooks_disabled: Cell::new(false),
                in_on_key: Cell::new(0),
                idle_timer,
                fs_check_timer,
                mouse_handler: MouseHandler::default(),
                state: Cell::new(if single_command { NormalState::SingleCommand } else { NormalState::Normal }),
            }
        })
    }

    fn enabled(&self) -> bool {
        self.input_handler().is_current_mode(self)
    }

    pub fn set_params(&self, params: NormalParams) {
        self.params.set(params);
    }

    fn handle_key_impl(&self, ih: &Rc<InputHandler>, mut key: Key, should_clear: &mut bool, do_restore_hooks: &mut bool) -> Result<()> {
        let context = ih.context();
        let transient = is_draft(context);

        if self.mouse_handler.handle_key(key, context)? {
            *should_clear = true;
            if !transient {
                self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
            }
        } else if let Some(cp) = key.codepoint().filter(|cp| (b'0' as u32..=b'9' as u32).contains(cp)) {
            let mut params = self.params.get();
            let new_val = params.count as i64 * 10 + (cp - b'0' as u32) as i64;
            if new_val > i32::MAX as i64 {
                context.print_status(DisplayLine::from_str_face("parameter overflowed", context.faces().face("Error")));
            } else {
                params.count = new_val as i32;
                self.params.set(params);
            }
        } else if key == Key::new(named::BACKSPACE) {
            let mut params = self.params.get();
            params.count /= 10;
            self.params.set(params);
        } else if key == crate::keys::key('\\') {
            if !self.hooks_disabled.get() {
                self.hooks_disabled.set(true);
                context.hooks_disabled().set();
            }
        } else if key == crate::keys::key('"') {
            let mode_weak = self.self_weak.clone();
            on_next_key_with_autoinfo(
                context,
                b"register",
                KeymapMode::None,
                Rc::new(move |key: Key, context: &Context| {
                    let Some(cp) = key.codepoint() else { return Ok(()) };
                    if key == Key::new(named::ESCAPE) {
                        return Ok(());
                    }
                    if cp <= 127 {
                        if let Some(mode) = mode_weak.upgrade() {
                            let mut params = mode.params.get();
                            params.reg = cp as u8;
                            mode.params.set(params);
                        }
                    } else {
                        context.print_status(DisplayLine::from_str_face(
                            kformat!("invalid register '{}'", Cp(cp)),
                            context.faces().face("Error"),
                        ));
                    }
                    Ok(())
                }),
                b"enter target register".to_vec(),
                REGISTER_DOC.as_bytes().to_vec(),
            )?;
        } else {
            *should_clear = true;

            // Hack to parse keys sent by terminals using the 8th bit to mark the
            // meta key. In normal mode, give priority to a potential alt-key than
            // the accentuated character.
            if key.key >= 127 && key.key < 256 {
                key.modifiers |= Modifiers::Alt;
                key.key &= 0x7f;
            }

            *do_restore_hooks = true;
            let res = if let Some(command) = get_normal_command(key) {
                let autoinfo = context.options().val::<AutoInfo>("autoinfo");
                if autoinfo.has(AutoInfo::Normal) {
                    if let Ok(client) = context.try_client() {
                        client.info_show_str(&key_to_str(key), command.docstring.as_bytes(), BufferCoord::default(), InfoStyle::Prompt);
                    }
                }

                // reset params now to be reentrant
                let params = self.params.replace(NormalParams { count: 0, reg: 0 });
                (command.func)(context, params)
            } else {
                self.params.set(NormalParams { count: 0, reg: 0 });
                Ok(())
            };

            if self.state.get() == NormalState::SingleCommand && self.enabled() {
                ih.pop_mode(self)?;
            } else if self.state.get() == NormalState::SingleCommand {
                self.state.set(NormalState::PopOnEnabled);
            }
            res?;
        }

        context.hooks().run_hook(Hook::NormalKey, &key_to_str(key), context)?;
        if *should_clear && !transient {
            if let Ok(client) = context.try_client() {
                client.schedule_clear();
            }
        }
        if self.enabled() && !transient {
            // The hook might have changed mode
            self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
        }
        Ok(())
    }
}

impl InputMode for Normal {
    fn input_handler(&self) -> Rc<InputHandler> {
        self.input_handler.upgrade().expect("input handler is alive")
    }

    fn on_enabled(&self, _from_pop: bool) -> Result<()> {
        let ih = self.input_handler();
        if self.state.get() == NormalState::PopOnEnabled {
            return ih.pop_mode(self);
        }
        let context = ih.context();
        if !is_draft(context) {
            if let Ok(client) = context.try_client() {
                client.check_if_buffer_needs_reloading()?;
            }
            self.fs_check_timer.set_next_date(now_plus(get_fs_check_timeout(context)));
            self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
        }
        if self.hooks_disabled.get() && self.in_on_key.get() == 0 {
            context.hooks_disabled().unset();
            self.hooks_disabled.set(false);
        }
        Ok(())
    }

    fn on_disabled(&self, from_push: bool) -> Result<()> {
        self.idle_timer.disable();
        self.fs_check_timer.disable();
        if !from_push && self.hooks_disabled.get() {
            self.input_handler().context().hooks_disabled().unset();
            self.hooks_disabled.set(false);
        }
        Ok(())
    }

    fn on_key(&self, key: Key) -> Result<()> {
        let ih = self.input_handler();
        self.in_on_key.set(self.in_on_key.get() + 1);
        let mut should_clear = false;
        let mut do_restore_hooks = false;
        let res = self.handle_key_impl(&ih, key, &mut should_clear, &mut do_restore_hooks);
        self.in_on_key.set(self.in_on_key.get() - 1);
        if self.hooks_disabled.get() && self.enabled() && do_restore_hooks {
            ih.context().hooks_disabled().unset();
            self.hooks_disabled.set(false);
        }
        res
    }

    fn mode_info(&self) -> ModeInfo {
        let ih = self.input_handler();
        let context = ih.context();
        let info_face = context.faces().face("StatusLineInfo");
        let value_face = context.faces().face("StatusLineValue");

        let (size, main_index, hidden_count) = {
            let sels = context.selections();
            let window = context.try_window().ok();
            let hidden = sels
                .iter()
                .filter(|sel| window.as_ref().is_some_and(|w| w.display_coord(sel.cursor().coord).is_none()))
                .count();
            (sels.size(), sels.main_index(), hidden)
        };

        let mut atoms = Vec::new();
        if size == 1 {
            atoms.push(DisplayAtom::text(kformat!("{} sel", size), info_face));
            if hidden_count != 0 {
                atoms.push(DisplayAtom::text(" (hidden)", info_face));
            }
        } else {
            atoms.push(DisplayAtom::text(kformat!("{} sels ({})", size, main_index + 1), info_face));
            if hidden_count != 0 {
                atoms.push(DisplayAtom::text(kformat!(" ({} hidden)", hidden_count), info_face));
            }
        }
        let params = self.params.get();
        if params.count != 0 {
            atoms.push(DisplayAtom::text(" param=", info_face));
            atoms.push(DisplayAtom::text(params.count.to_string(), value_face));
        }
        if params.reg != 0 {
            atoms.push(DisplayAtom::text(" reg=", info_face));
            atoms.push(DisplayAtom::text(vec![params.reg], value_face));
        }
        ModeInfo { display_line: DisplayLine::new(atoms), normal_params: Some(params) }
    }

    fn paste(&self, content: &[u8]) -> Result<()> {
        let ih = self.input_handler();
        let context = ih.context();
        default_paste(context, content)?;
        if !is_draft(context) {
            if let Ok(client) = context.try_client() {
                client.schedule_clear();
            }
            self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
        }
        Ok(())
    }

    fn take_pending_count(&self) -> u32 {
        let mut params = self.params.get();
        let count = params.count.max(1) as u32;
        params.count = 0;
        self.params.set(params);
        count
    }

    fn keymap_mode(&self) -> KeymapMode {
        KeymapMode::Normal
    }

    fn name(&self) -> Vec<u8> {
        b"normal".to_vec()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Line editing

fn cp_at(line: &[u8], pos: CharCount) -> Codepoint {
    crate::string_utils::char_at(line, pos)
}

fn to_next_word_begin(word_type: WordType, pos: &mut CharCount, line: &[u8]) {
    let len = char_length(line);
    if *pos == len {
        return;
    }
    let ew = DEFAULT_EXTRA_WORD_CHARS;
    if word_type == WordType::Word && is_punctuation(cp_at(line, *pos), ew) {
        while *pos != len && is_punctuation(cp_at(line, *pos), ew) {
            *pos += 1;
        }
    } else if is_word_type(word_type, cp_at(line, *pos), ew) {
        while *pos != len && is_word_type(word_type, cp_at(line, *pos), ew) {
            *pos += 1;
        }
    }
    while *pos != len && is_horizontal_blank(cp_at(line, *pos)) {
        *pos += 1;
    }
}

fn to_next_word_end(word_type: WordType, pos: &mut CharCount, line: &[u8]) {
    let len = char_length(line);
    if *pos + 1 >= len {
        return;
    }
    let ew = DEFAULT_EXTRA_WORD_CHARS;
    *pos += 1;
    while *pos != len && is_horizontal_blank(cp_at(line, *pos)) {
        *pos += 1;
    }
    if word_type == WordType::Word && is_punctuation(cp_at(line, *pos), ew) {
        while *pos != len && is_punctuation(cp_at(line, *pos), ew) {
            *pos += 1;
        }
    } else if is_word_type(word_type, cp_at(line, *pos), ew) {
        while *pos != len && is_word_type(word_type, cp_at(line, *pos), ew) {
            *pos += 1;
        }
    }
    *pos -= 1;
}

fn to_prev_word_begin(word_type: WordType, pos: &mut CharCount, line: &[u8]) {
    if *pos == 0 {
        return;
    }
    let ew = DEFAULT_EXTRA_WORD_CHARS;
    *pos -= 1;
    while *pos != 0 && is_horizontal_blank(cp_at(line, *pos)) {
        *pos -= 1;
    }
    if word_type == WordType::Word && is_punctuation(cp_at(line, *pos), ew) {
        while *pos != 0 && is_punctuation(cp_at(line, *pos), ew) {
            *pos -= 1;
        }
        if !is_punctuation(cp_at(line, *pos), ew) {
            *pos += 1;
        }
    } else if is_word_type(word_type, cp_at(line, *pos), ew) {
        while *pos != 0 && is_word_type(word_type, cp_at(line, *pos), ew) {
            *pos -= 1;
        }
        if !is_word_type(word_type, cp_at(line, *pos), ew) {
            *pos += 1;
        }
    }
}

fn concat3(a: &[u8], b: &[u8], c: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(a.len() + b.len() + c.len());
    v.extend_from_slice(a);
    v.extend_from_slice(b);
    v.extend_from_slice(c);
    v
}

struct LineEditor {
    cursor_pos: CharCount,
    line: Vec<u8>,
    empty_text: Vec<u8>,
    clipboard: Vec<u8>,
    faces: Rc<FaceRegistry>,
}

impl LineEditor {
    fn new(faces: Rc<FaceRegistry>) -> Self {
        LineEditor { cursor_pos: 0, line: Vec::new(), empty_text: Vec::new(), clipboard: Vec::new(), faces }
    }

    fn handle_key(&mut self, key: Key) {
        let erase_move = |this: &mut LineEditor, move_func: fn(WordType, &mut CharCount, &[u8]), word_type: WordType| {
            let mut old_pos = this.cursor_pos;
            move_func(word_type, &mut this.cursor_pos, &this.line.clone());
            if this.cursor_pos > old_pos {
                std::mem::swap(&mut this.cursor_pos, &mut old_pos);
            }
            this.clipboard = substr_chars(&this.line, this.cursor_pos, old_pos - this.cursor_pos).to_vec();
            this.line = concat3(substr_chars(&this.line, 0, this.cursor_pos), substr_chars(&this.line, old_pos, -1), b"");
        };

        let len = char_length(&self.line);
        if key == Key::new(named::LEFT) || key == ctrl(crate::keys::key('b')) {
            if self.cursor_pos > 0 {
                self.cursor_pos -= 1;
            }
        } else if key == Key::new(named::RIGHT) || key == ctrl(crate::keys::key('f')) {
            if self.cursor_pos < len {
                self.cursor_pos += 1;
            }
        } else if key == Key::new(named::HOME) || key == ctrl(crate::keys::key('a')) {
            self.cursor_pos = 0;
        } else if key == Key::new(named::END) || key == ctrl(crate::keys::key('e')) {
            self.cursor_pos = len;
        } else if key == Key::new(named::BACKSPACE)
            || key == shift(Key::new(named::BACKSPACE))
            || key == ctrl(crate::keys::key('h'))
        {
            if self.cursor_pos != 0 {
                self.line = concat3(
                    substr_chars(&self.line, 0, self.cursor_pos - 1),
                    substr_chars(&self.line, self.cursor_pos, -1),
                    b"",
                );
                self.cursor_pos -= 1;
            }
        } else if key == Key::new(named::DELETE) || key == ctrl(crate::keys::key('d')) {
            if self.cursor_pos != len {
                self.line = concat3(
                    substr_chars(&self.line, 0, self.cursor_pos),
                    substr_chars(&self.line, self.cursor_pos + 1, -1),
                    b"",
                );
            }
        } else if key == alt(crate::keys::key('f')) {
            let line = self.line.clone();
            to_next_word_begin(WordType::Word, &mut self.cursor_pos, &line);
        } else if key == alt(crate::keys::key('F')) {
            let line = self.line.clone();
            to_next_word_begin(WordType::WORD, &mut self.cursor_pos, &line);
        } else if key == alt(crate::keys::key('b')) {
            let line = self.line.clone();
            to_prev_word_begin(WordType::Word, &mut self.cursor_pos, &line);
        } else if key == alt(crate::keys::key('B')) {
            let line = self.line.clone();
            to_prev_word_begin(WordType::WORD, &mut self.cursor_pos, &line);
        } else if key == alt(crate::keys::key('e')) {
            let line = self.line.clone();
            to_next_word_end(WordType::Word, &mut self.cursor_pos, &line);
        } else if key == alt(crate::keys::key('E')) {
            let line = self.line.clone();
            to_next_word_end(WordType::WORD, &mut self.cursor_pos, &line);
        } else if key == ctrl(crate::keys::key('k')) {
            self.clipboard = substr_chars(&self.line, self.cursor_pos, -1).to_vec();
            self.line = substr_chars(&self.line, 0, self.cursor_pos).to_vec();
        } else if key == ctrl(crate::keys::key('u')) {
            self.clipboard = substr_chars(&self.line, 0, self.cursor_pos).to_vec();
            self.line = substr_chars(&self.line, self.cursor_pos, -1).to_vec();
            self.cursor_pos = 0;
        } else if key == ctrl(crate::keys::key('w')) || key == alt(Key::new(named::BACKSPACE)) {
            erase_move(self, to_prev_word_begin, WordType::Word);
        } else if key == ctrl(crate::keys::key('W')) {
            erase_move(self, to_prev_word_begin, WordType::WORD);
        } else if key == alt(crate::keys::key('d')) {
            erase_move(self, to_next_word_begin, WordType::Word);
        } else if key == alt(crate::keys::key('D')) {
            erase_move(self, to_next_word_begin, WordType::WORD);
        } else if key == ctrl(crate::keys::key('y')) {
            let clipboard = self.clipboard.clone();
            self.line = concat3(
                substr_chars(&self.line, 0, self.cursor_pos),
                &clipboard,
                substr_chars(&self.line, self.cursor_pos, -1),
            );
            self.cursor_pos += char_length(&clipboard);
        } else if let Some(cp) = key.codepoint() {
            self.insert_cp(cp);
        }
    }

    fn insert_cp(&mut self, cp: Codepoint) {
        self.line = concat3(
            substr_chars(&self.line, 0, self.cursor_pos),
            &utf8::encode(cp),
            substr_chars(&self.line, self.cursor_pos, -1),
        );
        self.cursor_pos += 1;
    }

    fn insert(&mut self, s: &[u8]) {
        self.insert_from(self.cursor_pos, s);
    }

    fn insert_from(&mut self, start: CharCount, s: &[u8]) {
        self.line = concat3(substr_chars(&self.line, 0, start), s, substr_chars(&self.line, self.cursor_pos, -1));
        self.cursor_pos = start + char_length(s);
    }

    fn reset(&mut self, line: Vec<u8>, empty_text: &[u8]) {
        self.line = line;
        self.empty_text = empty_text.to_vec();
        self.cursor_pos = char_length(&self.line);
    }

    fn line(&self) -> &[u8] {
        &self.line
    }

    fn cursor_pos(&self) -> CharCount {
        self.cursor_pos
    }

    fn cursor_display_column(&self) -> ColumnCount {
        column_length(substr_chars(&self.line, 0, self.cursor_pos))
    }

    fn build_display_line(&self, _in_width: ColumnCount) -> DisplayLine {
        let empty = self.line.is_empty();
        let s: &[u8] = if empty { &self.empty_text } else { &self.line };
        let line_face = self.faces.face(if empty { "StatusLineInfo" } else { "StatusLine" });
        let cursor_face = self.faces.face("StatusCursor");
        if self.cursor_pos == char_length(s) {
            return DisplayLine::new(vec![DisplayAtom::text(s.to_vec(), line_face), DisplayAtom::text(" ", cursor_face)]);
        }
        DisplayLine::new(vec![
            DisplayAtom::text(substr_chars(s, 0, self.cursor_pos).to_vec(), line_face),
            DisplayAtom::text(substr_chars(s, self.cursor_pos, 1).to_vec(), cursor_face),
            DisplayAtom::text(substr_chars(s, self.cursor_pos + 1, -1).to_vec(), line_face),
        ])
    }
}

fn get_raw_codepoint(key: Key) -> Option<Codepoint> {
    if let Some(cp) = key.codepoint() {
        return Some(cp);
    }
    if key.modifiers == Modifiers::Control
        && ((key.key >= '@' as u32 && key.key <= '_' as u32) || (key.key >= 'a' as u32 && key.key <= 'z' as u32))
    {
        return Some(to_upper(key.key) - '@' as u32);
    }
    None
}

fn byte_count_to(s: &[u8], count: CharCount) -> usize {
    utf8::advance(s, 0, count)
}

fn char_count_to(s: &[u8], count: usize) -> CharCount {
    utf8::distance(&s[..count.min(s.len())])
}

// ---------------------------------------------------------------------------
// Prompt mode

struct PromptState {
    completer: Option<PromptCompleter>,
    explicit_completer: Option<PromptCompleter>,
    prompt_face: Face,
    completions: Completions,
    current_completion: i32,
    prefix_in_completions: bool,
    prefix: Vec<u8>,
    empty_text: Vec<u8>,
    line_editor: LineEditor,
    line_changed: bool,
    was_interactive: bool,
    current_history: i32,
    auto_complete: bool,
    refresh_completion_pending: bool,
}

pub struct Prompt {
    input_handler: Weak<InputHandler>,
    self_weak: Weak<Prompt>,
    callback: PromptCallback,
    prompt: Vec<u8>,
    flags: PromptFlags,
    history: Rc<dyn Register>,
    state: RefCell<PromptState>,
    idle_timer: Timer,
}

impl Prompt {
    #[allow(clippy::too_many_arguments)]
    fn new(
        ih: &Rc<InputHandler>,
        prompt: &[u8],
        initstr: Vec<u8>,
        emptystr: Vec<u8>,
        face: Face,
        flags: PromptFlags,
        history_register: u8,
        completer: Option<PromptCompleter>,
        callback: PromptCallback,
    ) -> Result<Rc<Prompt>> {
        let context = ih.context();
        let history = RegisterManager::instance().get_cp(history_register as Codepoint)?;
        let auto_complete = context.options().val::<AutoComplete>("autocomplete").has(AutoComplete::Prompt);
        let draft = is_draft(context);
        // This prompt may outlive local scopes so ignore local faces.
        let mut line_editor = LineEditor::new(context.faces_allow_local(false));
        line_editor.reset(initstr, &emptystr);
        Ok(Rc::new_cyclic(|w: &Weak<Prompt>| {
            let idle_timer = if draft {
                Timer::none()
            } else {
                let w = w.clone();
                Timer::new(
                    None,
                    move |_| {
                        // hook or callback could trigger pop_mode()
                        let Some(this) = w.upgrade() else { return Ok(()) };
                        let (auto_complete, pending) = {
                            let s = this.state.borrow();
                            (s.auto_complete, s.refresh_completion_pending)
                        };
                        if auto_complete && pending {
                            this.refresh_completions();
                        }
                        let ih = this.input_handler();
                        let context = ih.context();
                        let changed = std::mem::replace(&mut this.state.borrow_mut().line_changed, false);
                        if changed {
                            let line = this.state.borrow().line_editor.line().to_vec();
                            (this.callback)(&line, PromptEvent::Change, context)?;
                        }
                        context.hooks().run_hook(Hook::PromptIdle, b"", context)
                    },
                    crate::event_manager::EventMode::Normal,
                )
            };
            Prompt {
                input_handler: Rc::downgrade(ih),
                self_weak: w.clone(),
                callback,
                prompt: prompt.to_vec(),
                flags,
                history,
                state: RefCell::new(PromptState {
                    completer,
                    explicit_completer: None,
                    prompt_face: face,
                    completions: Completions::default(),
                    current_completion: -1,
                    prefix_in_completions: false,
                    prefix: Vec::new(),
                    empty_text: emptystr,
                    line_editor,
                    line_changed: false,
                    was_interactive: false,
                    current_history: -1,
                    auto_complete,
                    refresh_completion_pending: true,
                }),
                idle_timer,
            }
        }))
    }

    fn enabled(&self) -> bool {
        self.input_handler().is_current_mode(self)
    }

    fn line(&self) -> Vec<u8> {
        self.state.borrow().line_editor.line().to_vec()
    }

    fn can_auto_insert_completion(&self) -> bool {
        let s = self.state.borrow();
        let line = s.line_editor.line();
        let has_completions = !s.completions.candidates.is_empty();
        let completion_selected = s.current_completion != -1;
        let cursor_byte = byte_count_to(line, s.line_editor.cursor_pos());
        let text_entered = s.completions.start != cursor_byte;
        let at_end = cursor_byte == line.len();
        s.completions.flags.has(CompletionFlags::Menu)
            && has_completions
            && !completion_selected
            && at_end
            && (!s.completions.flags.has(CompletionFlags::NoEmpty) || text_entered)
    }

    fn insert_first_completion(&self) {
        let mut s = self.state.borrow_mut();
        let start = char_count_to(s.line_editor.line(), s.completions.start);
        let completion = s.completions.candidates[0].clone();
        s.line_editor.insert_from(start, &completion);
    }

    fn set_line_changed(&self) {
        let mut s = self.state.borrow_mut();
        s.line_changed = true;
        s.refresh_completion_pending = true;
    }

    fn use_explicit_completer(&self, completer: fn(&Context, &[u8]) -> CandidateList) {
        self.state.borrow_mut().explicit_completer = Some(Rc::new(move |context: &Context, content: &[u8], cursor_pos: usize| {
            let mut last_token = None;
            let mut parser = CommandParser::new(&content[..cursor_pos.min(content.len())]);
            while let Ok(Some(token)) = parser.read_token(false) {
                last_token = Some(token);
            }
            if let Some(t) = &last_token {
                if t.pos + t.content.len() < cursor_pos {
                    last_token = None;
                }
            }
            let token_start = last_token.as_ref().map_or(cursor_pos, |t| t.pos);
            let token_content = last_token.as_ref().map_or(&[][..], |t| t.content.as_slice());
            Completions::with(token_start, cursor_pos, completer(context, token_content), CompletionFlags::None)
        }));
    }

    fn refresh_completions(&self) {
        let completer = {
            let mut s = self.state.borrow_mut();
            s.refresh_completion_pending = false;
            match s.explicit_completer.clone().or_else(|| s.completer.clone()) {
                Some(c) => c,
                None => return,
            }
        };
        let ih = self.input_handler();
        let context = ih.context();
        let (line, cursor) = {
            let mut s = self.state.borrow_mut();
            s.current_completion = -1;
            let line = s.line_editor.line().to_vec();
            let cursor = byte_count_to(&line, s.line_editor.cursor_pos());
            (line, cursor)
        };
        let completions = completer(context, &line, cursor);
        self.state.borrow_mut().completions = completions;
        let Ok(client) = context.try_client() else { return };
        if self.state.borrow().completions.candidates.is_empty() {
            client.menu_hide();
            return;
        }

        self.show_completions();
        let mut s = self.state.borrow_mut();
        let menu = s.completions.flags.has(CompletionFlags::Menu);
        if menu {
            client.menu_select(0);
        }
        let start = s.completions.start.min(line.len());
        let end = s.completions.end.max(start).min(line.len());
        let prefix = line[start..end].to_vec();
        s.prefix_in_completions = !menu && !s.completions.candidates.contains(&prefix);
        if s.prefix_in_completions {
            s.current_completion = s.completions.candidates.len() as i32;
            s.completions.candidates.push(prefix);
        }
    }

    fn show_completions(&self) {
        let ih = self.input_handler();
        let Ok(client) = ih.context().try_client() else { return };
        let items: Vec<DisplayLine> = self
            .state
            .borrow()
            .completions
            .candidates
            .iter()
            .map(|c| DisplayLine::from_str_face(c.clone(), Face::default()))
            .collect();
        let menu_style = if self.flags.has(PromptFlags::Search) { MenuStyle::Search } else { MenuStyle::Prompt };
        client.menu_show(items, BufferCoord::default(), menu_style);
    }

    fn clear_completions(&self) {
        let mut s = self.state.borrow_mut();
        s.current_completion = -1;
        s.completions.candidates.clear();
    }

    fn display(&self) {
        let ih = self.input_handler();
        let context = ih.context();
        let Ok(client) = context.try_client() else { return };
        let width = client.dimensions().column - column_length(&self.prompt);
        let s = self.state.borrow();
        let display_line = if self.flags.has(PromptFlags::Password) {
            DisplayLine::default()
        } else {
            s.line_editor.build_display_line(width)
        };
        let status_style = if self.flags.has(PromptFlags::Search) {
            StatusStyle::Search
        } else if self.flags.has(PromptFlags::Command) {
            StatusStyle::Command
        } else {
            StatusStyle::Prompt
        };
        let cursor = s.line_editor.cursor_display_column();
        let prompt_face = s.prompt_face;
        drop(s);
        context.print_status_full(
            DisplayLine::from_str_face(self.prompt.clone(), prompt_face),
            display_line,
            cursor,
            status_style,
        );
    }

    pub fn set_prompt_face(&self, face: Face) {
        let changed = {
            let mut s = self.state.borrow_mut();
            if face != s.prompt_face {
                s.prompt_face = face;
                true
            } else {
                false
            }
        };
        if changed {
            self.display();
        }
    }

    fn history_push(&self, entry: &[u8]) -> Result<()> {
        let was_interactive = self.state.borrow().was_interactive;
        if entry.is_empty()
            || !was_interactive
            || (self.flags.has(PromptFlags::DropHistoryEntriesWithBlankPrefix) && is_horizontal_blank(entry[0] as u32))
        {
            return Ok(());
        }
        let ih = self.input_handler();
        self.history.set(ih.context(), &[entry.to_vec()], false)
    }

    fn on_key_impl(&self, ih: &Rc<InputHandler>, key: Key) -> Result<()> {
        let context = ih.context();
        let is_backspace = key == Key::new(named::BACKSPACE)
            || key == shift(Key::new(named::BACKSPACE))
            || key == ctrl(crate::keys::key('h'));

        if key == Key::new(named::RETURN) {
            if self.can_auto_insert_completion() {
                self.insert_first_completion();
            }
            let line = self.line();
            self.history_push(&line)?;
            context.print_status(DisplayLine::default());
            if let Ok(client) = context.try_client() {
                client.menu_hide();
            }

            // Maintain hooks disabled in callback if they were before pop_mode
            let _disable_hooks = ScopedSetBool::new(context.hooks_disabled(), context.hooks_disabled().is_set());
            ih.pop_mode(self)?;
            // call callback after pop_mode so that callback
            // may change the mode
            return (self.callback)(&line, PromptEvent::Validate, context);
        } else if key == Key::new(named::ESCAPE) || key == ctrl(crate::keys::key('c')) || (is_backspace && self.line().is_empty()) {
            let line = self.line();
            self.history_push(&line)?;
            context.print_status(DisplayLine::default());
            if let Ok(client) = context.try_client() {
                client.menu_hide();
            }

            // Maintain hooks disabled in callback if they were before pop_mode
            let _disable_hooks = ScopedSetBool::new(context.hooks_disabled(), context.hooks_disabled().is_set());
            ih.pop_mode(self)?;
            return (self.callback)(&line, PromptEvent::Abort, context);
        } else if key == ctrl(crate::keys::key('r')) {
            let weak = self.self_weak.clone();
            on_next_key_with_autoinfo(
                context,
                b"register",
                KeymapMode::None,
                Rc::new(move |mut key: Key, context: &Context| {
                    let Some(this) = weak.upgrade() else { return Ok(()) };
                    let joined = key.modifiers.has(Modifiers::Alt);
                    let quoted = key.modifiers.has(Modifiers::Control);
                    key.modifiers.remove(Modifiers::Alt | Modifiers::Control);

                    let Some(cp) = key.codepoint() else { return Ok(()) };
                    if key == Key::new(named::ESCAPE) {
                        return Ok(());
                    }

                    let q = quoter(if quoted { Quoting::Kakoune } else { Quoting::Raw });
                    let text = if joined {
                        let values = RegisterManager::instance().get_cp(cp)?.get(context)?;
                        join(values.iter().map(|v| q(v)), b' ', false)
                    } else {
                        q(&context.main_sel_register_value(&utf8::encode(cp))?)
                    };
                    this.state.borrow_mut().line_editor.insert(&text);
                    this.display();
                    this.set_line_changed();
                    Ok(())
                }),
                b"enter register name".to_vec(),
                REGISTER_DOC.as_bytes().to_vec(),
            )?;
            self.display();
            return Ok(());
        } else if key == ctrl(crate::keys::key('v')) {
            let weak = self.self_weak.clone();
            on_next_key_with_autoinfo(
                context,
                b"raw-key",
                KeymapMode::None,
                Rc::new(move |key: Key, _context: &Context| {
                    let Some(this) = weak.upgrade() else { return Ok(()) };
                    if let Some(cp) = get_raw_codepoint(key) {
                        this.state.borrow_mut().line_editor.insert_cp(cp);
                        this.display();
                        this.set_line_changed();
                    }
                    Ok(())
                }),
                b"raw insert".to_vec(),
                b"enter key to insert".to_vec(),
            )?;
            self.display();
            return Ok(());
        } else if key == Key::new(named::UP) || key == ctrl(crate::keys::key('p')) {
            let history = self.history.get(context)?;
            {
                let mut s = self.state.borrow_mut();
                s.current_history = (history.len() as i32 - 1).min(s.current_history);
                if s.current_history == -1 {
                    s.prefix = s.line_editor.line().to_vec();
                }
                let start = (s.current_history + 1) as usize;
                let prefix = s.prefix.clone();
                if let Some(offset) = history[start.min(history.len())..].iter().position(|h| prefix_match(h, &prefix)) {
                    s.current_history = (start + offset) as i32;
                    let empty = s.empty_text.clone();
                    s.line_editor.reset(history[start + offset].clone(), &empty);
                }
            }
            self.clear_completions();
            self.state.borrow_mut().refresh_completion_pending = true;
        } else if key == Key::new(named::DOWN) || key == ctrl(crate::keys::key('n')) {
            let history = self.history.get(context)?;
            let mut s = self.state.borrow_mut();
            s.current_history = (history.len() as i32 - 1).min(s.current_history);
            if s.current_history >= 0 {
                let prefix = s.prefix.clone();
                let found = history[..s.current_history as usize].iter().rposition(|h| prefix_match(h, &prefix));
                s.current_history = match found {
                    Some(i) => i as i32,
                    None => -1,
                };
                let empty = s.empty_text.clone();
                let text = match found {
                    Some(i) => history[i].clone(),
                    None => prefix,
                };
                s.line_editor.reset(text, &empty);
                drop(s);
                self.clear_completions();
                self.state.borrow_mut().refresh_completion_pending = true;
            }
        } else if key == Key::new(named::TAB) || key == shift(Key::new(named::TAB)) || key.modifiers == Modifiers::MenuSelect {
            let (auto_complete, pending) = {
                let s = self.state.borrow();
                (s.auto_complete, s.refresh_completion_pending)
            };
            if auto_complete && pending {
                self.refresh_completions();
            }
            if self.state.borrow().completions.candidates.is_empty() {
                // manual completion, we need to ask our completer for completions
                self.refresh_completions();
                let s = self.state.borrow();
                let n = s.completions.candidates.len();
                if (!s.prefix_in_completions && n > 1) || n > 2 {
                    return Ok(());
                }
            }

            if self.state.borrow().completions.candidates.is_empty() {
                return Ok(());
            }

            let reverse = key == shift(Key::new(named::TAB));
            let (current, completion) = {
                let mut s = self.state.borrow_mut();
                let n = s.completions.candidates.len() as i32;
                if key.modifiers == Modifiers::MenuSelect {
                    s.current_completion = (key.key as i32).clamp(0, n - 1);
                } else if !reverse {
                    s.current_completion += 1;
                    if s.current_completion >= n {
                        s.current_completion = 0;
                    }
                } else {
                    s.current_completion -= 1;
                    if s.current_completion < 0 {
                        s.current_completion = n - 1;
                    }
                }
                (s.current_completion, s.completions.candidates[s.current_completion as usize].clone())
            };
            if let Ok(client) = context.try_client() {
                client.menu_select(current);
            }
            let mut s = self.state.borrow_mut();
            let start = char_count_to(s.line_editor.line(), s.completions.start);
            s.line_editor.insert_from(start, &completion);

            // when we have only one completion candidate, make next tab complete
            // from the new content.
            let n = s.completions.candidates.len();
            if n == 1 || (s.prefix_in_completions && n == 2) {
                s.current_completion = -1;
                s.completions.candidates.clear();
                s.refresh_completion_pending = true;
            }
        } else if key == ctrl(crate::keys::key('x')) {
            let weak = self.self_weak.clone();
            on_next_key_with_autoinfo(
                context,
                b"explicit-completion",
                KeymapMode::None,
                Rc::new(move |key: Key, _context: &Context| {
                    let Some(this) = weak.upgrade() else { return Ok(()) };
                    this.state.borrow_mut().explicit_completer = None;

                    if key.key == 'f' as u32 {
                        this.use_explicit_completer(|context, token| {
                            let ignored = context.options().val::<Regex>("ignored_files");
                            complete_filename(token, &ignored, token.len(), FilenameFlags::Expand)
                        });
                    } else if key.key == 'w' as u32 {
                        this.use_explicit_completer(|context, token| {
                            let mut candidates = CandidateList::new();
                            let words = get_word_db(&context.buffer()).find_matching(token);
                            let matches: Vec<(Rc<[u8]>, crate::word_db::RankedMatchInfo)> = words;
                            let token_letters = crate::ranked_match::used_letters(token);
                            for_n_best(
                                matches,
                                100,
                                |a, b| {
                                    RankedMatch::with_letters(&a.0, a.1.letters, token, token_letters)
                                        .less(&RankedMatch::with_letters(&b.0, b.1.letters, token, token_letters))
                                },
                                |m| {
                                    candidates.push(m.0.to_vec());
                                    true
                                },
                            );
                            candidates
                        });
                    }

                    if this.state.borrow().explicit_completer.is_some() {
                        this.refresh_completions();
                    }
                    Ok(())
                }),
                b"enter completion type".to_vec(),
                b"f: filename\nw: buffer word\n".to_vec(),
            )?;
        } else if key == ctrl(crate::keys::key('o')) {
            let auto_complete = {
                let mut s = self.state.borrow_mut();
                s.explicit_completer = None;
                s.auto_complete = !s.auto_complete;
                s.auto_complete
            };
            if auto_complete {
                self.refresh_completions();
            } else if let Ok(client) = context.try_client() {
                self.clear_completions();
                client.menu_hide();
            }
        } else if key == alt(crate::keys::key('!')) {
            let line = self.line();
            match expand(&line, context, &ShellContext::default()) {
                Ok(expanded) => {
                    let mut s = self.state.borrow_mut();
                    let empty = s.empty_text.clone();
                    s.line_editor.reset(expanded, &empty);
                }
                Err(error) if error.is_runtime() => {
                    context.print_status(DisplayLine::from_str_face(error.what().to_vec(), context.faces().face("Error")));
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        } else if key == alt(crate::keys::key(';')) {
            let mode = Normal::new(ih, true);
            ih.push_mode(mode)?;
            return Ok(());
        } else {
            if (key == Key::new(named::SPACE) || key == shift(Key::new(named::SPACE)))
                && !self.state.borrow().completions.flags.has(CompletionFlags::Quoted) // if token is quoted, this space does not end it
                && self.can_auto_insert_completion()
            {
                self.insert_first_completion();
            }
            self.state.borrow_mut().line_editor.handle_key(key);
            self.clear_completions();
            self.state.borrow_mut().refresh_completion_pending = true;
        }

        self.display();
        self.state.borrow_mut().line_changed = true;
        if self.enabled() && !is_draft(context) {
            // The callback might have disabled us
            self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
        }
        Ok(())
    }
}

impl InputMode for Prompt {
    fn input_handler(&self) -> Rc<InputHandler> {
        self.input_handler.upgrade().expect("input handler is alive")
    }

    fn on_key(&self, key: Key) -> Result<()> {
        let ih = self.input_handler();
        self.on_key_impl(&ih, key)
    }

    fn refresh_ifn(&self) {
        let explicit_completion_selected = {
            let s = self.state.borrow();
            s.current_completion != -1
                && (!s.prefix_in_completions || s.current_completion as usize != s.completions.candidates.len() - 1)
        };
        let ih = self.input_handler();
        let context = ih.context();
        if !self.enabled() || is_draft(context) || explicit_completion_selected {
            return;
        }

        let next_date = Instant::now() + get_idle_timeout(context);
        if self.idle_timer.next_date().is_none_or(|d| next_date < d) {
            self.idle_timer.set_next_date(Some(next_date));
        }
        self.state.borrow_mut().refresh_completion_pending = true;
    }

    fn paste(&self, content: &[u8]) -> Result<()> {
        self.state.borrow_mut().line_editor.insert(content);
        self.clear_completions();
        self.state.borrow_mut().refresh_completion_pending = true;
        self.display();
        self.state.borrow_mut().line_changed = true;
        let ih = self.input_handler();
        if !is_draft(ih.context()) {
            self.idle_timer.set_next_date(now_plus(get_idle_timeout(ih.context())));
        }
        Ok(())
    }

    fn on_raw_key(&self) {
        self.state.borrow_mut().was_interactive = true;
    }

    fn mode_info(&self) -> ModeInfo {
        let ih = self.input_handler();
        ModeInfo {
            display_line: DisplayLine::from_str_face("prompt", ih.context().faces().face("StatusLineMode")),
            normal_params: None,
        }
    }

    fn keymap_mode(&self) -> KeymapMode {
        KeymapMode::Prompt
    }

    fn name(&self) -> Vec<u8> {
        b"prompt".to_vec()
    }

    fn on_enabled(&self, from_pop: bool) -> Result<()> {
        self.display();
        let ih = self.input_handler();
        let context = ih.context();
        if from_pop {
            if let Ok(client) = context.try_client() {
                if !self.state.borrow().completions.candidates.is_empty() {
                    self.show_completions();
                    let s = self.state.borrow();
                    let menu = s.completions.flags.has(CompletionFlags::Menu);
                    if s.current_completion != -1 {
                        client.menu_select(s.current_completion);
                    } else if menu {
                        client.menu_select(0);
                    }
                }
            }
        }
        if !is_draft(context) {
            self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
        }
        Ok(())
    }

    fn on_disabled(&self, from_push: bool) -> Result<()> {
        let ih = self.input_handler();
        let context = ih.context();
        if !from_push {
            context.print_status(DisplayLine::default());
        }
        self.idle_timer.disable();
        if let Ok(client) = context.try_client() {
            client.menu_hide();
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// NextKey mode

pub struct NextKey {
    input_handler: Weak<InputHandler>,
    name: Vec<u8>,
    callback: KeyCallback,
    keymap_mode: KeymapMode,
    _idle_timer: Timer,
}

impl InputMode for NextKey {
    fn input_handler(&self) -> Rc<InputHandler> {
        self.input_handler.upgrade().expect("input handler is alive")
    }

    fn on_key(&self, key: Key) -> Result<()> {
        let ih = self.input_handler();
        let context = ih.context();
        // maintain hooks disabled in the callback if they were before pop_mode
        let _disable_hooks = ScopedSetBool::new(context.hooks_disabled(), context.hooks_disabled().is_set());
        ih.pop_mode(self)?;
        (self.callback)(key, context)
    }

    fn mode_info(&self) -> ModeInfo {
        let ih = self.input_handler();
        ModeInfo {
            display_line: DisplayLine::from_str_face("enter key", ih.context().faces().face("StatusLineMode")),
            normal_params: None,
        }
    }

    fn keymap_mode(&self) -> KeymapMode {
        self.keymap_mode
    }

    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Insert mode

pub struct Insert {
    input_handler: Weak<InputHandler>,
    self_weak: Weak<Insert>,
    /// guards released when the mode is destroyed: edition, selection edition, disable hooks
    guards_active: Cell<bool>,
    edition_active: bool,
    selection_edition_active: bool,
    disable_hooks_active: bool,
    completer: InsertCompleter,
    records_last_insert: bool,
    restore_cursor: bool,
    auto_complete: Cell<bool>,
    idle_timer: Timer,
    mouse_handler: MouseHandler,
}

impl Insert {
    fn new(ih: &Rc<InputHandler>, mode: InsertMode, count: i32, records_last_insert: bool) -> Result<Rc<Insert>> {
        let context = ih.context();
        let draft = is_draft(context);

        // ScopedEdition, ScopedSelectionEdition and ScopedSetBool equivalents
        let edition_active = context.has_buffer();
        if edition_active {
            context.begin_edition();
        }
        let selection_edition_active = !draft && context.has_buffer();
        if selection_edition_active {
            context.begin_selection_edition();
        }
        let disable_hooks_active = context.hooks_disabled().is_set();
        if disable_hooks_active {
            context.hooks_disabled().set();
        }

        let insert = Rc::new_cyclic(|w: &Weak<Insert>| {
            let idle_timer = if draft {
                Timer::none()
            } else {
                let w = w.clone();
                Timer::new(
                    None,
                    move |_| {
                        // hook could trigger pop_mode()
                        let Some(this) = w.upgrade() else { return Ok(()) };
                        let ih = this.input_handler();
                        let context = ih.context();
                        if let Ok(client) = context.try_client() {
                            client.clear_pending();
                        }
                        this.completer.update(this.auto_complete.get())?;
                        context.hooks().run_hook(Hook::InsertIdle, b"", context)
                    },
                    crate::event_manager::EventMode::Normal,
                )
            };
            Insert {
                input_handler: Rc::downgrade(ih),
                self_weak: w.clone(),
                guards_active: Cell::new(true),
                edition_active,
                selection_edition_active,
                disable_hooks_active,
                completer: InsertCompleter::new(ih),
                records_last_insert,
                restore_cursor: mode == InsertMode::Append,
                auto_complete: Cell::new(
                    context.options().val::<AutoComplete>("autocomplete").has(AutoComplete::Insert),
                ),
                idle_timer,
                mouse_handler: MouseHandler::default(),
            }
        });

        context.buffer().throw_if_read_only()?;

        if records_last_insert {
            let mut last_insert = ih.last_insert.borrow_mut();
            last_insert.recording += 1;
            last_insert.mode = mode;
            last_insert.keys.clear();
            last_insert.disable_hooks = context.hooks_disabled().is_set();
            last_insert.count = count;
        }
        insert.prepare(context, mode, count)?;
        Ok(insert)
    }

    fn release_guards(&self, context: &Context) {
        if !self.guards_active.replace(false) {
            return;
        }
        self.mouse_handler.stop_dragging(context);
        if self.disable_hooks_active {
            context.hooks_disabled().unset();
        }
        if self.selection_edition_active {
            context.end_selection_edition();
        }
        if self.edition_active {
            context.end_edition();
        }
    }

    fn enabled(&self) -> bool {
        self.input_handler().is_current_mode(self)
    }

    fn move_chars(&self, context: &Context, offset: CharCount) {
        let buffer = context.buffer();
        let mut selections = context.selections();
        for sel in selections.iter_mut() {
            let cursor = buffer.offset_coord_chars(sel.cursor().coord, offset);
            sel.set(cursor, cursor);
        }
        selections.sort_and_merge_overlapping();
    }

    fn move_lines(&self, context: &Context, offset: LineCount) {
        let buffer = context.buffer();
        let tabstop = context.options().val::<i32>("tabstop");
        let mut selections = context.selections();
        for sel in selections.iter_mut() {
            let cursor = buffer.offset_coord_lines(sel.cursor(), offset, tabstop);
            sel.set_anchor(cursor.coord);
            sel.set_cursor(cursor);
        }
        selections.sort_and_merge_overlapping();
    }

    fn insert_strings(&self, context: &Context, strings: &[Vec<u8>]) -> Result<()> {
        let buffer = context.buffer();
        context.selections().for_each(
            |index, sel| {
                let pos = sel.cursor().coord;
                insert_at(&buffer, sel, pos, &strings[index.min(strings.len() - 1)])?;
                Ok(())
            },
            false,
        )
    }

    fn insert_cp(&self, context: &Context, key: Codepoint) -> Result<()> {
        let s = utf8::encode(key);
        self.insert_strings(context, std::slice::from_ref(&s))?;
        context.hooks().run_hook(Hook::InsertChar, &s, context)
    }

    fn prepare(&self, context: &Context, mode: InsertMode, count: i32) -> Result<()> {
        let buffer = context.buffer();
        match mode {
            InsertMode::Insert => {
                let mut selections = context.selections();
                for sel in selections.iter_mut() {
                    let (min, max) = (sel.min(), sel.max());
                    sel.set(max, min);
                }
            }
            InsertMode::Replace => {
                context.selections().erase()?;
            }
            InsertMode::Append => {
                let mut selections = context.selections();
                for sel in selections.iter_mut() {
                    sel.set(sel.min(), buffer.char_next(sel.max()));
                    if sel.cursor().coord == buffer.end_coord() {
                        buffer.insert(buffer.end_coord(), b"\n")?;
                    }
                }
            }
            InsertMode::AppendAtLineEnd => {
                let mut selections = context.selections();
                for sel in selections.iter_mut() {
                    let line = sel.max().line;
                    sel.set_pos(BufferCoord::new(line, buffer.line_length(line) - 1));
                }
            }
            InsertMode::OpenLineBelow | InsertMode::OpenLineAbove => {
                let below = mode == InsertMode::OpenLineBelow;
                let count = if count > 0 { count } else { 1 };
                let mut new_sels = Vec::new();
                let mut inserted_count = 0;
                let (sels, main_index) = {
                    let selections = context.selections();
                    (selections.as_slice().to_vec(), selections.main_index())
                };
                for sel in &sels {
                    let line = if below { sel.max().line + 1 } else { sel.min().line };
                    buffer.insert(BufferCoord::line(line + inserted_count), &string_of_cp('\n' as u32, count))?;
                    for i in 0..count {
                        new_sels.push(Selection::new(BufferCoord::line(line + inserted_count + i)));
                    }
                    inserted_count += count;
                }
                context.selections().set(new_sels, main_index * count as usize + count as usize - 1);
                context.hooks().run_hook(Hook::InsertChar, b"\n", context)?;
            }
            InsertMode::InsertAtLineBegin => {
                let mut selections = context.selections();
                for sel in selections.iter_mut() {
                    let mut pos = BufferCoord::line(sel.min().line);
                    let line = buffer.line(pos.line);
                    let mut col = 0;
                    while line[col] == b' ' || line[col] == b'\t' {
                        col += 1;
                    }
                    if line[col] != b'\n' {
                        pos.column = col as i32;
                    }
                    sel.set_pos(pos);
                }
            }
        }
        Ok(())
    }

    fn on_key_impl(&self, ih: &Rc<InputHandler>, key: Key) -> Result<()> {
        let context = ih.context();
        let buffer = context.buffer();
        let transient = is_draft(context);
        let mut update_completions = true;
        let mut moved = false;
        if self.mouse_handler.handle_key(key, context)? {
            if !transient {
                self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
            }
        } else if key == Key::new(named::ESCAPE) || key == ctrl(crate::keys::key('c')) {
            self.completer.try_reset()?;
            ih.pop_mode(self)?;
        } else if key == Key::new(named::BACKSPACE) || key == shift(Key::new(named::BACKSPACE)) {
            let (sels, main_cursor) = {
                let selections = context.selections();
                let sels: Vec<Selection> = selections
                    .iter()
                    .filter(|sel| sel.cursor().coord != BufferCoord::new(0, 0))
                    .map(|sel| Selection::new(buffer.char_prev(sel.cursor().coord)))
                    .collect();
                (sels, selections.main().cursor().coord)
            };
            let main_char = if main_cursor != BufferCoord::new(0, 0) {
                buffer.string(buffer.char_prev(main_cursor), main_cursor)
            } else {
                Vec::new()
            };
            if !sels.is_empty() {
                SelectionList::from_vec(buffer.clone(), sels).erase()?;
            }
            if !main_char.is_empty() {
                context.hooks().run_hook(Hook::InsertDelete, &main_char, context)?;
            }
            context.selections_write_only().update(false);
        } else if key == Key::new(named::DELETE) {
            let sels: Vec<Selection> = context.selections().iter().map(|sel| Selection::new(sel.cursor())).collect();
            SelectionList::from_vec(buffer.clone(), sels).erase()?;
        } else if key == Key::new(named::LEFT) {
            self.move_chars(context, -1);
            moved = true;
        } else if key == Key::new(named::RIGHT) {
            self.move_chars(context, 1);
            moved = true;
        } else if key == Key::new(named::UP) {
            self.move_lines(context, -1);
            moved = true;
        } else if key == Key::new(named::DOWN) {
            self.move_lines(context, 1);
            moved = true;
        } else if key == Key::new(named::HOME) {
            let mut selections = context.selections();
            for sel in selections.iter_mut() {
                let c = BufferCoord::line(sel.cursor().line);
                sel.set(c, c);
            }
            selections.sort_and_merge_overlapping();
        } else if key == Key::new(named::END) {
            let mut selections = context.selections();
            for sel in selections.iter_mut() {
                let line = sel.cursor().line;
                let c = buffer.clamp(BufferCoord::new(line, buffer.line_length(line)));
                sel.set(c, c);
            }
            selections.sort_and_merge_overlapping();
        } else if let Some(cp) = key.codepoint() {
            self.completer.try_accept()?;
            self.insert_cp(context, cp)?;
        } else if key == ctrl(crate::keys::key('r')) {
            self.completer.try_accept()?;
            let weak = self.self_weak.clone();
            on_next_key_with_autoinfo(
                context,
                b"register",
                KeymapMode::None,
                Rc::new(move |key: Key, context: &Context| {
                    let Some(this) = weak.upgrade() else { return Ok(()) };
                    let Some(cp) = key.codepoint() else { return Ok(()) };
                    if key == Key::new(named::ESCAPE) {
                        return Ok(());
                    }
                    let values = RegisterManager::instance().get_cp(cp)?.get(context)?;
                    this.insert_strings(context, &values)
                }),
                b"enter register name".to_vec(),
                REGISTER_DOC.as_bytes().to_vec(),
            )?;
            update_completions = false;
        } else if key == ctrl(crate::keys::key('n')) || key == ctrl(crate::keys::key('p')) || key.modifiers == Modifiers::MenuSelect {
            ih.drop_last_recorded_key();
            let relative = key.modifiers != Modifiers::MenuSelect;
            let index = if relative { if key == ctrl(crate::keys::key('n')) { 1 } else { -1 } } else { key.key as i32 };
            self.completer.select(index, relative, &mut |k| ih.record_key(k))?;
            update_completions = false;
        } else if key == ctrl(crate::keys::key('x')) {
            let weak = self.self_weak.clone();
            on_next_key_with_autoinfo(
                context,
                b"explicit-completion",
                KeymapMode::None,
                Rc::new(move |key: Key, _context: &Context| {
                    let Some(this) = weak.upgrade() else { return Ok(()) };
                    if key.key == 'f' as u32 {
                        this.completer.explicit_file_complete()?;
                    }
                    if key.key == 'w' as u32 {
                        this.completer.explicit_word_buffer_complete()?;
                    }
                    if key.key == 'W' as u32 {
                        this.completer.explicit_word_all_complete()?;
                    }
                    if key.key == 'l' as u32 {
                        this.completer.explicit_line_buffer_complete()?;
                    }
                    if key.key == 'L' as u32 {
                        this.completer.explicit_line_all_complete()?;
                    }
                    Ok(())
                }),
                b"enter completion type".to_vec(),
                b"f: filename\nw: word (current buffer)\nW: word (all buffers)\nl: line (current buffer)\nL: line (all buffers)\n"
                    .to_vec(),
            )?;
            update_completions = false;
        } else if key == ctrl(crate::keys::key('o')) {
            self.auto_complete.set(!self.auto_complete.get());
            self.completer.try_reset()?;
        } else if key == ctrl(crate::keys::key('u')) {
            buffer.commit_undo_group();
            context.print_status(DisplayLine::from_str_face(
                kformat!("committed change #{}", buffer.current_history_id()),
                context.faces().face("Information"),
            ));
        } else if key == ctrl(crate::keys::key('v')) {
            self.completer.try_accept()?;
            let weak = self.self_weak.clone();
            on_next_key_with_autoinfo(
                context,
                b"raw-insert",
                KeymapMode::None,
                Rc::new(move |key: Key, context: &Context| {
                    let Some(this) = weak.upgrade() else { return Ok(()) };
                    if let Some(cp) = get_raw_codepoint(key) {
                        this.insert_cp(context, cp)?;
                        context.hooks().run_hook(Hook::InsertKey, &key_to_str(key), context)?;
                        if this.enabled() && !transient {
                            this.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
                        }
                    }
                    Ok(())
                }),
                b"raw insert".to_vec(),
                b"enter key to insert".to_vec(),
            )?;
            update_completions = false;
        } else if key == alt(crate::keys::key(';')) {
            let mode = Normal::new(ih, true);
            ih.push_mode(mode)?;
            return Ok(());
        }

        context.hooks().run_hook(Hook::InsertKey, &key_to_str(key), context)?;
        if moved {
            context.hooks().run_hook(Hook::InsertMove, &key_to_str(key), context)?;
        }

        if update_completions && self.enabled() && !transient {
            // Hooks might have disabled us
            self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
        }
        Ok(())
    }
}

impl InputMode for Insert {
    fn input_handler(&self) -> Rc<InputHandler> {
        self.input_handler.upgrade().expect("input handler is alive")
    }

    fn on_enabled(&self, _from_pop: bool) -> Result<()> {
        let ih = self.input_handler();
        if !is_draft(ih.context()) {
            self.idle_timer.set_next_date(now_plus(get_idle_timeout(ih.context())));
        }
        Ok(())
    }

    fn on_disabled(&self, from_push: bool) -> Result<()> {
        self.idle_timer.disable();

        if !from_push {
            let ih = self.input_handler();
            if self.records_last_insert {
                ih.last_insert.borrow_mut().recording -= 1;
            }

            let context = ih.context();
            if self.restore_cursor {
                let buffer = context.buffer();
                let mut selections = context.selections();
                for sel in selections.iter_mut() {
                    if sel.cursor().coord > sel.anchor() && sel.cursor().coord > BufferCoord::new(0, 0) {
                        let c = buffer.char_prev(sel.cursor().coord);
                        sel.set_cursor(c);
                    }
                }
            }
        }
        Ok(())
    }

    fn on_key(&self, key: Key) -> Result<()> {
        let ih = self.input_handler();
        self.on_key_impl(&ih, key)
    }

    fn paste(&self, content: &[u8]) -> Result<()> {
        let ih = self.input_handler();
        let context = ih.context();
        self.completer.try_accept()?;
        self.insert_strings(context, &[content.to_vec()])?;
        self.idle_timer.set_next_date(now_plus(get_idle_timeout(context)));
        Ok(())
    }

    fn mode_info(&self) -> ModeInfo {
        let ih = self.input_handler();
        let context = ih.context();
        let (num_sel, main_index) = {
            let sels = context.selections();
            (sels.size(), sels.main_index())
        };
        ModeInfo {
            display_line: DisplayLine::new(vec![
                DisplayAtom::text("insert", context.faces().face("StatusLineMode")),
                DisplayAtom::text(" ", context.faces().face("StatusLine")),
                DisplayAtom::text(
                    if num_sel == 1 {
                        kformat!("{} sel", num_sel)
                    } else {
                        kformat!("{} sels ({})", num_sel, main_index + 1)
                    },
                    context.faces().face("StatusLineInfo"),
                ),
            ]),
            normal_params: None,
        }
    }

    fn keymap_mode(&self) -> KeymapMode {
        KeymapMode::Insert
    }

    fn name(&self) -> Vec<u8> {
        b"insert".to_vec()
    }

    fn teardown(&self, context: &Context) {
        self.release_guards(context);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for Insert {
    fn drop(&mut self) {
        if let Some(ih) = self.input_handler.upgrade() {
            self.release_guards(ih.context());
        }
    }
}

// ---------------------------------------------------------------------------
// InputHandler

pub struct InputHandler {
    context: Context,
    mode_stack: RefCell<Vec<Rc<dyn InputMode>>>,
    last_insert: RefCell<Insertion>,
    handle_key_level: Cell<i32>,
    recording_reg: Cell<Codepoint>,
    recorded_keys: RefCell<Vec<Key>>,
    recording_level: Cell<i32>,
    self_weak: Weak<InputHandler>,
}

impl InputHandler {
    pub fn new(selections: SelectionList, flags: ContextFlags, name: Vec<u8>) -> Rc<InputHandler> {
        let ih = Rc::new_cyclic(|w| InputHandler {
            context: Context::new(w.clone(), selections, flags, name),
            mode_stack: RefCell::new(Vec::new()),
            last_insert: RefCell::new(Insertion {
                recording: 0,
                repeating: false,
                mode: InsertMode::Insert,
                keys: Vec::new(),
                disable_hooks: false,
                count: 1,
            }),
            handle_key_level: Cell::new(0),
            recording_reg: Cell::new(0),
            recorded_keys: RefCell::new(Vec::new()),
            recording_level: Cell::new(-1),
            self_weak: w.clone(),
        });
        let normal = Normal::new(&ih, false);
        ih.mode_stack.borrow_mut().push(normal.clone());
        if let Err(e) = normal.on_enabled(false) {
            write_to_debug_buffer(&kformat!("error enabling normal mode: {}", e.what()));
        }
        ih
    }

    pub fn rc(&self) -> Rc<InputHandler> {
        self.self_weak.upgrade().expect("input handler is alive")
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn current_mode(&self) -> Rc<dyn InputMode> {
        self.mode_stack.borrow().last().expect("mode stack is never empty").clone()
    }

    fn is_current_mode(&self, mode: &dyn InputMode) -> bool {
        self.mode_stack.borrow().last().is_some_and(|m| same_mode(m, mode))
    }

    pub fn push_mode(&self, new_mode: Rc<dyn InputMode>) -> Result<()> {
        let prev = self.current_mode();
        let prev_name = prev.name();
        prev.on_disabled(true)?;
        self.mode_stack.borrow_mut().push(new_mode.clone());
        new_mode.on_enabled(false)?;
        self.context.hooks().run_hook(
            Hook::ModeChange,
            &kformat!("push:{}:{}", prev_name, new_mode.name()),
            &self.context,
        )
    }

    pub fn pop_mode(&self, mode: &dyn InputMode) -> Result<()> {
        debug_assert!(self.is_current_mode(mode));
        if self.mode_stack.borrow().len() <= 1 {
            return Ok(());
        }
        let prev_name = mode.name();
        self.current_mode().on_disabled(false)?;
        // Keep the popped mode alive until we are done
        let _keep_alive = self.mode_stack.borrow_mut().pop();
        let current = self.current_mode();
        current.on_enabled(true)?;
        self.context.hooks().run_hook(
            Hook::ModeChange,
            &kformat!("pop:{}:{}", prev_name, current.name()),
            &self.context,
        )
    }

    pub fn reset_normal_mode(&self) -> Result<()> {
        loop {
            if self.mode_stack.borrow().len() <= 1 {
                return Ok(());
            }
            let mode = self.current_mode();
            self.pop_mode(mode.as_ref())?;
        }
    }

    /// switch to insert mode
    pub fn insert(&self, mode: InsertMode, count: i32) -> Result<()> {
        let records = self.handle_key_level.get() <= 1;
        let insert = Insert::new(&self.rc(), mode, count, records)?;
        self.push_mode(insert)
    }

    /// repeat last insert mode key sequence
    pub fn repeat_last_insert(&self) -> Result<()> {
        let (keys, mode, count, disable_hooks) = {
            let last = self.last_insert.borrow();
            if last.keys.is_empty() {
                return Ok(());
            }
            if !is_mode::<Normal>(self.current_mode().as_ref()) || last.recording > 0 || last.repeating {
                return Err(Error::runtime("repeating last insert not available in this context"));
            }
            (last.keys.clone(), last.mode, last.count, last.disable_hooks)
        };

        let _disable_hooks = ScopedSetBool::new(self.context.hooks_disabled(), disable_hooks);
        self.last_insert.borrow_mut().repeating = true;
        let res = (|| -> Result<()> {
            let insert = Insert::new(&self.rc(), mode, count, false)?;
            self.push_mode(insert)?;
            for key in &keys {
                self.handle_key(*key, true)?;
            }
            Ok(())
        })();
        self.last_insert.borrow_mut().repeating = false;
        res
    }

    /// insert a string without affecting the mode stack
    pub fn paste(&self, content: &[u8]) -> Result<()> {
        self.current_mode().paste(content)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prompt(
        &self,
        prompt: &[u8],
        initstr: Vec<u8>,
        emptystr: Vec<u8>,
        prompt_face: Face,
        flags: PromptFlags,
        history_register: u8,
        completer: Option<PromptCompleter>,
        callback: PromptCallback,
    ) -> Result<()> {
        let mode = Prompt::new(&self.rc(), prompt, initstr, emptystr, prompt_face, flags, history_register, completer, callback)?;
        self.push_mode(mode)
    }

    pub fn set_prompt_face(&self, prompt_face: Face) {
        let current = self.current_mode();
        if let Some(prompt) = current.as_any().downcast_ref::<Prompt>() {
            prompt.set_prompt_face(prompt_face);
        }
    }

    /// execute callback on next keypress and returns to normal mode
    /// if callback does not change the mode itself
    pub fn on_next_key(
        &self,
        mode_name: &[u8],
        keymap_mode: KeymapMode,
        callback: KeyCallback,
        idle_callback: Option<IdleCallback>,
    ) -> Result<()> {
        let idle_timer = match idle_callback {
            Some(cb) => Timer::new(
                now_plus(get_idle_timeout(&self.context)),
                move |t| cb(t),
                crate::event_manager::EventMode::Normal,
            ),
            None => Timer::none(),
        };
        let mode = Rc::new(NextKey {
            input_handler: self.self_weak.clone(),
            name: kformat!("next-key[{}]", mode_name),
            callback,
            keymap_mode,
            _idle_timer: idle_timer,
        });
        self.push_mode(mode)
    }

    /// process the given key
    pub fn handle_key(&self, key: Key, synthesized: bool) -> Result<()> {
        if !is_valid(key) {
            return Ok(());
        }

        self.handle_key_level.set(self.handle_key_level.get() + 1);
        let res = self.handle_key_impl(key, synthesized);
        self.handle_key_level.set(self.handle_key_level.get() - 1);
        res
    }

    fn handle_key_impl(&self, key: Key, synthesized: bool) -> Result<()> {
        if !synthesized {
            self.current_mode().on_raw_key();
        }

        let process_key = |k: Key| -> Result<()> {
            self.record_key(k);
            // keep the mode alive while it handles the key
            let mode = self.current_mode();
            mode.on_key(k)
        };

        let keymap_mode = self.current_mode().keymap_mode();
        let keymaps = self.context.keymaps();
        let mapping = keymaps.get_mapping(key, keymap_mode);
        match mapping {
            Some(mapping) if !self.context.keymaps_disabled().is_set() => {
                // copy to allow reentrant unmap
                let keys = mapping.keys.clone();
                let count = if mapping.atomic { self.current_mode().take_pending_count() } else { 1 };
                for _ in 0..count {
                    for k in &keys {
                        process_key(*k)?;
                    }
                }
            }
            _ => process_key(key)?,
        }

        if self.handle_key_level.get() < self.recording_level.get() {
            write_to_debug_buffer(b"Macro recording started but not finished");
            self.recording_reg.set(0);
            self.recording_level.set(-1);
        }
        Ok(())
    }

    fn record_key(&self, key: Key) {
        let level = self.handle_key_level.get();
        {
            let mut last_insert = self.last_insert.borrow_mut();
            if last_insert.recording > 0 && level <= 1 {
                last_insert.keys.push(key);
            }
        }
        if self.is_recording() && level == self.recording_level.get() {
            self.recorded_keys.borrow_mut().push(key);
        }
    }

    fn drop_last_recorded_key(&self) {
        let level = self.handle_key_level.get();
        {
            let mut last_insert = self.last_insert.borrow_mut();
            if last_insert.recording > 0 && level <= 1 {
                last_insert.keys.pop();
            }
        }
        if self.is_recording() && level == self.recording_level.get() {
            self.recorded_keys.borrow_mut().pop();
        }
    }

    pub fn refresh_ifn(&self) {
        self.current_mode().refresh_ifn();
    }

    pub fn start_recording(&self, reg: Codepoint) {
        debug_assert!(self.recording_reg.get() == 0);
        self.recording_level.set(self.handle_key_level.get());
        self.recorded_keys.borrow_mut().clear();
        self.recording_reg.set(reg);
    }

    pub fn is_recording(&self) -> bool {
        self.recording_reg.get() != 0
    }

    pub fn recording_reg(&self) -> Codepoint {
        self.recording_reg.get()
    }

    pub fn stop_recording(&self) -> Result<()> {
        debug_assert!(self.recording_reg.get() != 0);
        let keys = {
            let mut recorded = self.recorded_keys.borrow_mut();
            if recorded.is_empty() {
                None
            } else {
                // Forget the key that got us to exit recording
                if self.handle_key_level.get() == self.recording_level.get() {
                    recorded.pop();
                }
                let mut keys = Vec::new();
                for key in recorded.iter() {
                    keys.extend(key_to_str(*key));
                }
                Some(keys)
            }
        };
        let reg = self.recording_reg.get();
        self.recording_reg.set(0);
        self.recording_level.set(-1);
        if let Some(keys) = keys {
            RegisterManager::instance().get_cp(reg)?.set(&self.context, &[keys], false)?;
        }
        Ok(())
    }

    pub fn mode_info(&self) -> ModeInfo {
        self.current_mode().mode_info()
    }

    /// Force an input handler into normal mode temporarily (ScopedForceNormal)
    pub fn force_normal(&self, params: NormalParams) -> Result<ScopedForceNormal> {
        let mut pushed = None;
        if self.mode_stack.borrow().len() != 1 {
            let mode = Normal::new(&self.rc(), false);
            self.push_mode(mode.clone())?;
            pushed = Some(mode);
        }
        let current = self.current_mode();
        if let Some(normal) = current.as_any().downcast_ref::<Normal>() {
            normal.set_params(params);
        }
        Ok(ScopedForceNormal { handler: self.rc(), mode: pushed })
    }
}

impl Drop for InputHandler {
    fn drop(&mut self) {
        let modes = std::mem::take(&mut *self.mode_stack.borrow_mut());
        for mode in modes.iter().rev() {
            mode.teardown(&self.context);
        }
    }
}

pub struct ScopedForceNormal {
    handler: Rc<InputHandler>,
    mode: Option<Rc<Normal>>,
}

impl Drop for ScopedForceNormal {
    fn drop(&mut self) {
        let Some(mode) = self.mode.take() else { return };
        let is_current = self.handler.is_current_mode(mode.as_ref());
        if is_current {
            let _ = self.handler.pop_mode(mode.as_ref());
        } else {
            let mut stack = self.handler.mode_stack.borrow_mut();
            if let Some(pos) = stack.iter().position(|m| same_mode(m, mode.as_ref())) {
                stack.remove(pos);
            }
        }
    }
}

fn is_valid(key: Key) -> bool {
    let valid_mods = Modifiers::Control | Modifiers::Alt | Modifiers::Shift;
    (key.modifiers.0 & !valid_mods.0) != 0 || key.key <= 0x10FFFF
}

pub fn should_show_info(mask: AutoInfo, context: &Context) -> bool {
    context.options().val::<AutoInfo>("autoinfo").has(mask) && context.has_client()
}

pub fn show_auto_info_ifn(title: &[u8], info: &[u8], mask: AutoInfo, context: &Context) -> bool {
    if !should_show_info(mask, context) {
        return false;
    }
    context.client().info_show_str(title, info, BufferCoord::default(), InfoStyle::Prompt);
    true
}

pub fn hide_auto_info_ifn(context: &Context, hide: bool) {
    if hide {
        if let Ok(client) = context.try_client() {
            client.info_hide(false);
        }
    }
}

pub fn on_next_key_with_autoinfo(
    context: &Context,
    mode_name: &[u8],
    keymap_mode: KeymapMode,
    cmd: KeyCallback,
    title: Vec<u8>,
    info: Vec<u8>,
) -> Result<()> {
    let ih = context.input_handler();
    let weak_ih = Rc::downgrade(&ih);
    ih.on_next_key(
        mode_name,
        keymap_mode,
        Rc::new(move |key: Key, context: &Context| {
            let hide = should_show_info(AutoInfo::OnKey, context);
            hide_auto_info_ifn(context, hide);
            cmd(key, context)
        }),
        Some(Rc::new(move |_: &TimerRef| {
            if let Some(ih) = weak_ih.upgrade() {
                show_auto_info_ifn(&title, &info, AutoInfo::OnKey, ih.context());
            }
            Ok(())
        })),
    )
}

pub fn scroll_window(context: &Context, offset: LineCount, on_hidden_cursor: OnHiddenCursor) -> Result<()> {
    let window = context.window();
    let buffer = context.buffer();
    let line_count = buffer.line_count();

    let mut win_pos = window.position();
    let win_dim = window.dimensions();

    if on_hidden_cursor == OnHiddenCursor::PreserveSelections {
        context.ensure_cursor_visible.set(false);
    }

    if (offset < 0 && win_pos.line == 0) || (offset > 0 && win_pos.line == line_count - 1) {
        return Ok(());
    }

    let max_offset = DisplayCoord::new((win_dim.line - 1) / 2, (win_dim.column - 1) / 2);
    let scrolloff = context.options().val::<DisplayCoord>("scrolloff").min(max_offset);

    win_pos.line = (win_pos.line + offset).clamp(0, line_count - 1);
    window.set_position(win_pos);
    if on_hidden_cursor != OnHiddenCursor::PreserveSelections {
        let _selection_edition = crate::context::ScopedSelectionEdition::new(context);
        let mut selections = context.selections();
        let main_selection = selections.main_mut();
        let anchor = main_selection.anchor();
        let cursor = main_selection.cursor();
        let cursor_off = win_pos.line - window.position().line;
        let line = (cursor.line + cursor_off).clamp(
            win_pos.line + scrolloff.line,
            win_pos.line + win_dim.line - 1 - scrolloff.line,
        );
        let tabstop = context.options().val::<i32>("tabstop");
        let new_cursor = buffer.offset_coord_lines(cursor, line - cursor.line, tabstop);
        let new_anchor = if on_hidden_cursor == OnHiddenCursor::MoveCursor { anchor } else { new_cursor.coord };
        *main_selection = Selection::from_range(new_anchor, new_cursor);
        selections.sort_and_merge_overlapping();
    }
    Ok(())
}

