//! Port of client.hh/cc

use crate::buffer::{Buffer, BufferFlags};
use crate::buffer_utils::reload_file_buffer;
use crate::client_manager::ClientManager;
use crate::command_manager::{expand_with, ShellContext};
use crate::context::{Context, ContextFlags, ScopedSelectionEdition};
use crate::debug::{write_to_debug_buffer, DebugFlags, EnvVarMap};
use crate::display_buffer::{parse_display_line, DisplayLine, DisplayLineBuiltins, DisplayLineList};
use crate::event_manager::{set_signal_handler, EventMode, Timer};
use crate::exception::{Error, Result};
use crate::face::Face;
use crate::file::{get_fs_status, get_fs_timestamp, murmur3, MappedFile, INVALID_TIME};
use crate::hook_manager::Hook;
use crate::input_handler::InputHandler;
use crate::keymap_manager::KeymapMode;
use crate::keys::{ctrl, named, Key, Modifiers};
use crate::option_manager::{Opt, OptionWatcher};
use crate::option_types::EnumDesc;
use crate::scope::GlobalScope;
use crate::selection::SelectionList;
use crate::string_utils::escape;
use crate::units::{BufferCoord, ColumnCount, DisplayCoord};
use crate::user_interface::{InfoStyle, MenuStyle, StatusStyle, UiOptions, UserInterface};
use crate::window::Window;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Autoreload {
    Yes,
    No,
    Ask,
}

impl EnumDesc for Autoreload {
    fn desc() -> &'static [(Self, &'static str)] {
        &[
            (Autoreload::Yes, "yes"),
            (Autoreload::No, "no"),
            (Autoreload::Ask, "ask"),
            (Autoreload::Yes, "true"),
            (Autoreload::No, "false"),
        ]
    }
}
crate::enum_option!(Autoreload);

mod pending {
    pub const MENU_SHOW: u32 = 1 << 0;
    pub const MENU_SELECT: u32 = 1 << 1;
    pub const MENU_HIDE: u32 = 1 << 2;
    pub const INFO_SHOW: u32 = 1 << 3;
    pub const INFO_HIDE: u32 = 1 << 4;
    pub const STATUS_LINE: u32 = 1 << 5;
    pub const DRAW: u32 = 1 << 6;
    pub const REFRESH: u32 = 1 << 7;
}

const PENDING_CLEAR_INFO: u32 = 0b01;
const PENDING_CLEAR_STATUS_LINE: u32 = 0b10;

#[derive(Clone, Default)]
struct Menu {
    items: Vec<DisplayLine>,
    anchor: BufferCoord,
    ui_anchor: Option<DisplayCoord>,
    style: Option<MenuStyle>,
    selected: i32,
}

#[derive(Clone, Default)]
struct Info {
    title: DisplayLine,
    content: DisplayLineList,
    anchor: BufferCoord,
    ui_anchor: Option<DisplayCoord>,
    style: Option<InfoStyle>,
}

#[derive(Default)]
struct ClientState {
    status_prompt: DisplayLine,
    status_content: DisplayLine,
    status_cursor_pos: ColumnCount,
    status_style: StatusStyle,
    mode_line: DisplayLine,
    ui_pending: u32,
    pending_clear: u32,
    menu: Menu,
    info: Info,
    pending_keys: Vec<Key>,
    buffer_reload_dialog_opened: bool,
}

pub type OnExitCallback = Box<dyn Fn(i32)>;

pub struct Client {
    ui: Box<dyn UserInterface>,
    window: RefCell<Rc<Window>>,
    pid: i32,
    on_exit: OnExitCallback,
    env_vars: EnvVarMap,
    input_handler: Rc<InputHandler>,
    state: RefCell<ClientState>,
    self_weak: Weak<Client>,
}

impl Client {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ui: Box<dyn UserInterface>,
        window: Rc<Window>,
        selections: SelectionList,
        pid: i32,
        env_vars: EnvVarMap,
        name: Vec<u8>,
        on_exit: OnExitCallback,
    ) -> Result<Rc<Client>> {
        let client = Rc::new_cyclic(|w: &Weak<Client>| {
            let input_handler = InputHandler::new(selections, ContextFlags::None, name);
            Client {
                ui,
                window: RefCell::new(window.clone()),
                pid,
                on_exit,
                env_vars,
                input_handler,
                // [DEVIATION] the C++ leaves m_status_cursor_pos uninitialized
                // until the first print_status; it reads as 0 in practice.
                state: RefCell::new(ClientState { status_cursor_pos: 0, ..Default::default() }),
                self_weak: w.clone(),
            }
        });

        window.set_client(Some(&client));
        client.context().set_client(&client);
        client.context().set_window(&window);

        window.set_dimensions(client.ui.dimensions());
        window.options().register_watcher(client.self_weak.clone() as Weak<dyn OptionWatcher>);

        client.ui.set_ui_options(&window.options().val::<UiOptions>("ui_options"));
        let weak = client.self_weak.clone();
        client.ui.set_on_key(Box::new(move |key: Key| {
            let Some(client) = weak.upgrade() else { return Ok(()) };
            client.on_key(key)
        }));
        let weak = client.self_weak.clone();
        client.ui.set_on_paste(Box::new(move |content: &[u8]| {
            let Some(client) = weak.upgrade() else { return Ok(()) };
            client.context().input_handler().paste(content)
        }));

        window.hooks().run_hook(Hook::WinDisplay, &window.buffer().name(), client.context())?;

        client.force_redraw(false);
        Ok(client)
    }

    fn on_key(&self, key: Key) -> Result<()> {
        if key == ctrl(crate::keys::key('c')) {
            let prev_handler = set_signal_handler(libc::SIGINT, libc::SIG_IGN);
            unsafe { libc::killpg(libc::getpgrp(), libc::SIGINT) };
            set_signal_handler(libc::SIGINT, prev_handler);
        } else if key == ctrl(crate::keys::key('g')) {
            self.state.borrow_mut().pending_keys.clear();
            self.print_status(
                DisplayLine::default(),
                DisplayLine::from_str_face("operation cancelled", self.context().faces().face("Error")),
                -1,
                StatusStyle::Status,
            );
            return Err(Error::cancel());
        } else if key.modifiers.has(Modifiers::Resize) {
            self.window().set_dimensions(key.coord());
            self.force_redraw(true);
        } else {
            self.state.borrow_mut().pending_keys.push(key);
        }
        Ok(())
    }

    pub fn rc(&self) -> Rc<Client> {
        self.self_weak.upgrade().expect("client is alive")
    }

    pub fn weak(&self) -> Weak<Client> {
        self.self_weak.clone()
    }

    pub fn context(&self) -> &Context {
        self.input_handler.context()
    }

    pub fn input_handler(&self) -> &Rc<InputHandler> {
        &self.input_handler
    }

    pub fn window(&self) -> Rc<Window> {
        self.window.borrow().clone()
    }

    pub fn ui(&self) -> &dyn UserInterface {
        self.ui.as_ref()
    }

    pub fn is_ui_ok(&self) -> bool {
        self.ui.is_ok()
    }

    pub fn has_pending_inputs(&self) -> bool {
        !self.state.borrow().pending_keys.is_empty()
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn exit(&self, status: i32) {
        (self.on_exit)(status);
    }

    pub fn process_pending_inputs(&self) -> Result<bool> {
        let debug_keys = self.context().options().val::<DebugFlags>("debug").has(DebugFlags::Keys);
        self.window().run_resize_hook_ifn()?;
        // steal keys as we might receive new keys while handling them.
        let keys = std::mem::take(&mut self.state.borrow_mut().pending_keys);
        for key in &keys {
            let key = *key;
            let res = (|| -> Result<()> {
                if debug_keys {
                    write_to_debug_buffer(&kformat!("Client '{}' got key '{}'", self.context().name(), key));
                }

                let context = self.context();
                if key == Key::new(named::FOCUS_IN) {
                    context.hooks().run_hook(Hook::FocusIn, &context.name(), context)?;
                } else if key == Key::new(named::FOCUS_OUT) {
                    context.hooks().run_hook(Hook::FocusOut, &context.name(), context)?;
                } else {
                    context.ensure_cursor_visible.set(true);
                    self.input_handler.handle_key(key, false)?;
                }

                context.hooks().run_hook(Hook::RawKey, &crate::keys::key_to_str(key), context)
            })();
            match res {
                Ok(()) => {}
                Err(error) if error.is_runtime() => {
                    let context = self.context();
                    write_to_debug_buffer(&kformat!("Error: {}", error.what()));
                    context.print_status_full(
                        DisplayLine::default(),
                        DisplayLine::from_str_face(error.what().to_vec(), context.faces().face("Error")),
                        -1,
                        StatusStyle::Status,
                    );
                    context.hooks().run_hook(Hook::RuntimeError, error.what(), context)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(!keys.is_empty())
    }

    pub fn print_status(&self, prompt: DisplayLine, content: DisplayLine, cursor_pos: ColumnCount, style: StatusStyle) {
        let mut state = self.state.borrow_mut();
        state.status_prompt = prompt;
        state.status_content = content;
        state.status_cursor_pos = cursor_pos;
        state.status_style = style;
        state.ui_pending |= pending::STATUS_LINE;
        state.pending_clear &= !PENDING_CLEAR_STATUS_LINE;
    }

    pub fn dimensions(&self) -> DisplayCoord {
        self.ui.dimensions()
    }

    fn generate_context_info(&self) -> Vec<u8> {
        let context = self.context();
        let buffer = context.buffer();
        let mut s = Vec::new();
        if buffer.is_modified() {
            s.extend_from_slice(b"[+]");
        }
        if self.input_handler.is_recording() {
            s.extend(kformat!("[recording ({})]", crate::format::Cp(self.input_handler.recording_reg())));
        }
        if context.hooks_disabled().is_set() {
            s.extend_from_slice(b"[no-hooks]");
        }
        let flags = buffer.flags();
        if !flags.has(BufferFlags::File | BufferFlags::Debug) {
            s.extend_from_slice(b"[scratch]");
        }
        if flags.has(BufferFlags::New) {
            s.extend_from_slice(b"[new file]");
        }
        if flags.has(BufferFlags::Fifo) {
            s.extend_from_slice(b"[fifo]");
        }
        if flags.has(BufferFlags::Debug) {
            s.extend_from_slice(b"[debug]");
        }
        if flags.has(BufferFlags::ReadOnly) {
            s.extend_from_slice(b"[readonly]");
        }
        s
    }

    fn generate_mode_line(&self) -> DisplayLine {
        let context = self.context();
        let res = (|| -> Result<DisplayLine> {
            let mode_info = self.input_handler.mode_info();
            let modelinefmt = context.options().val::<Vec<u8>>("modelinefmt");
            let mut atoms = DisplayLineBuiltins::new();
            atoms.insert(b"mode_info".to_vec(), mode_info.display_line);
            atoms.insert(
                b"context_info".to_vec(),
                DisplayLine::from_str_face(self.generate_context_info(), context.faces().face("Information")),
            );
            let mut shell_context = ShellContext::default();
            let (reg, count) = match &mode_info.normal_params {
                Some(p) => (
                    if p.reg == 0 { Vec::new() } else { vec![p.reg] },
                    p.count.to_string().into_bytes(),
                ),
                None => (Vec::new(), Vec::new()),
            };
            shell_context.env_vars.insert(b"register".to_vec(), reg);
            shell_context.env_vars.insert(b"count".to_vec(), count);
            let expanded = expand_with(&modelinefmt, context, &shell_context, &|s| escape(&s, b"{", b'\\'))?;
            parse_display_line(&expanded, &context.faces(), &atoms)
        })();
        match res {
            Ok(line) => line,
            Err(err) => {
                write_to_debug_buffer(&kformat!("Error while parsing modelinefmt: {}", err.what()));
                DisplayLine::from_str_face("modelinefmt error, see *debug* buffer", context.faces().face("Error"))
            }
        }
    }

    pub fn change_buffer(&self, buffer: &Rc<Buffer>, set_selections: Option<&dyn Fn()>) -> Result<()> {
        if self.state.borrow().buffer_reload_dialog_opened {
            self.close_buffer_reload_dialog()?;
        }

        if self.context().buffer().flags().has(BufferFlags::Locked) {
            return Err(Error::runtime("Changing buffer is not allowed while current buffer is locked"));
        }

        buffer.add_flags(BufferFlags::Locked);
        let _unlock = crate::utils::OnScopeEnd::new(|| buffer.remove_flags(BufferFlags::Locked));

        let client_manager = ClientManager::instance();
        let ws = client_manager.get_free_window(buffer)?;

        let old_window = self.window();
        let me = self.self_weak.clone() as Weak<dyn OptionWatcher>;
        old_window.options().unregister_watcher(&me);
        old_window.set_client(None);
        let old_selections = self.context().selections().clone();
        client_manager.add_free_window(old_window, old_selections);

        *self.window.borrow_mut() = ws.window.clone();
        let window = ws.window;
        window.set_client_weak(self.self_weak.clone());
        window.options().register_watcher(me);

        match set_selections {
            Some(f) => f(),
            None => {
                let _selection_edition = ScopedSelectionEdition::new(self.context());
                *self.context().selections_write_only() = ws.selections;
            }
        }

        self.context().set_window(&window);
        window.set_dimensions(self.ui.dimensions());

        self.ui.set_ui_options(&window.options().val::<UiOptions>("ui_options"));

        window.hooks().run_hook(Hook::WinDisplay, &buffer.name(), self.context())?;

        self.force_redraw(true);
        Ok(())
    }

    pub fn redraw_ifn(&self) -> Result<()> {
        let context = self.context();
        let window = context.window();
        if window.needs_redraw(context) {
            self.state.borrow_mut().ui_pending |= pending::DRAW;
        }

        let faces = context.faces();
        if self.state.borrow().ui_pending & pending::DRAW != 0 {
            let display_buffer = window.update_display_buffer(context)?;
            let cursor = context.selections().main().cursor().coord;
            drop(display_buffer);
            let cursor_pos = window.display_coord(cursor).unwrap_or_default();
            self.ui.draw(
                &window.display_buffer(),
                cursor_pos,
                &faces.face("Default"),
                &faces.face("BufferPadding"),
                window.last_display_setup().widget_columns,
            );
        }

        {
            let mut guard = self.state.borrow_mut();
            let state = &mut *guard;
            let update_menu_anchor = (state.ui_pending & pending::DRAW != 0)
                && (state.ui_pending & pending::MENU_HIDE == 0)
                && !state.menu.items.is_empty()
                && state.menu.style == Some(MenuStyle::Inline);
            if (state.ui_pending & pending::MENU_SHOW != 0) || update_menu_anchor {
                let anchor = if state.menu.style == Some(MenuStyle::Inline) {
                    window.display_coord(state.menu.anchor)
                } else {
                    Some(DisplayCoord::default())
                };
                if state.ui_pending & pending::MENU_SHOW == 0 && state.menu.ui_anchor != anchor {
                    state.ui_pending |= if anchor.is_some() { pending::MENU_SHOW | pending::MENU_SELECT } else { pending::MENU_HIDE };
                }
                state.menu.ui_anchor = anchor;
            }

            if state.ui_pending & pending::MENU_SHOW != 0 {
                if let Some(anchor) = state.menu.ui_anchor {
                    self.ui.menu_show(
                        &state.menu.items,
                        anchor,
                        faces.face("MenuForeground"),
                        faces.face("MenuBackground"),
                        state.menu.style.unwrap_or(MenuStyle::Prompt),
                    );
                }
            }
            if state.ui_pending & pending::MENU_SELECT != 0 && state.menu.ui_anchor.is_some() {
                self.ui.menu_select(state.menu.selected);
            }
            if state.ui_pending & pending::MENU_HIDE != 0 {
                self.ui.menu_hide();
            }

            let is_inline_info = state.info.style.is_some_and(is_inline);
            let update_info_anchor = (state.ui_pending & pending::DRAW != 0)
                && (state.ui_pending & pending::INFO_HIDE == 0)
                && !state.info.content.is_empty()
                && is_inline_info;
            if (state.ui_pending & pending::INFO_SHOW != 0) || update_info_anchor {
                let anchor = if is_inline_info { window.display_coord(state.info.anchor) } else { Some(DisplayCoord::default()) };
                if state.ui_pending & pending::MENU_SHOW == 0 && state.info.ui_anchor != anchor {
                    state.ui_pending |= if anchor.is_some() { pending::INFO_SHOW } else { pending::INFO_HIDE };
                }
                state.info.ui_anchor = anchor;
            }

            if state.ui_pending & pending::INFO_SHOW != 0 {
                if let Some(anchor) = state.info.ui_anchor {
                    let style = state.info.style.unwrap_or(InfoStyle::Prompt);
                    let face = faces.face(if is_inline(style) || style == InfoStyle::MenuDoc {
                        "InlineInformation"
                    } else {
                        "Information"
                    });
                    self.ui.info_show(&state.info.title, &state.info.content, anchor, face, style);
                }
            }
            if state.ui_pending & pending::INFO_HIDE != 0 {
                self.ui.info_hide();
            }
        }

        // This needs to be done *after* update_display_buffer as ithe mode line may rely on it to
        // compute whether selections are visible.
        let mode_line = self.generate_mode_line();
        {
            let mut state = self.state.borrow_mut();
            if mode_line.atoms() != state.mode_line.atoms() {
                state.ui_pending |= pending::STATUS_LINE;
                state.mode_line = mode_line;
            }

            if state.ui_pending & pending::STATUS_LINE != 0 {
                self.ui.draw_status(
                    &state.status_prompt,
                    &state.status_content,
                    state.status_cursor_pos,
                    &state.mode_line,
                    &faces.face("StatusLine"),
                    state.status_style,
                );
            }

            if state.ui_pending != 0 {
                self.ui.refresh(state.ui_pending & pending::REFRESH != 0);
            }
            state.ui_pending = 0;
        }
        Ok(())
    }

    pub fn force_redraw(&self, full: bool) {
        let mut state = self.state.borrow_mut();
        if full {
            let menu = if state.menu.items.is_empty() { pending::MENU_HIDE } else { pending::MENU_SHOW | pending::MENU_SELECT };
            let info = if state.info.content.is_empty() { pending::INFO_HIDE } else { pending::INFO_SHOW };
            state.ui_pending |= pending::REFRESH | pending::DRAW | pending::STATUS_LINE | menu | info;
        } else {
            state.ui_pending |= pending::DRAW;
        }
    }

    fn reload_buffer(&self) -> Result<()> {
        let context = self.context();
        let buffer = context.buffer();
        match reload_file_buffer(&buffer) {
            Ok(()) => {
                context.print_status(DisplayLine::from_str_face(
                    kformat!("'{}' reloaded", buffer.display_name()),
                    context.faces().face("Information"),
                ));
                self.window().hooks().run_hook(Hook::BufReload, &buffer.name(), context)?;
            }
            Err(error) if error.is_runtime() => {
                context.print_status(DisplayLine::from_str_face(
                    kformat!("error while reloading buffer: '{}'", error.what()),
                    context.faces().face("Error"),
                ));
                buffer.set_fs_status(get_fs_status(&buffer.filename())?);
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn on_buffer_reload_key(&self, key: Key) -> Result<()> {
        let context = self.context();
        let buffer = context.buffer();

        let set_autoreload = |autoreload: Autoreload| -> Result<()> {
            let mut option = context.options().get(b"autoreload")?;
            // Do not touch global autoreload, set it at least at buffer level
            if option.manager().is_some_and(|m| Rc::ptr_eq(&m, GlobalScope::instance().options())) {
                option = context.buffer().options().get_local_option(b"autoreload")?;
            }
            option.set(autoreload, true)
        };

        if key == crate::keys::key('y') || key == crate::keys::key('Y') || key == Key::new(named::RETURN) {
            self.reload_buffer()?;
            if key == crate::keys::key('Y') {
                set_autoreload(Autoreload::Yes)?;
            }
        } else if key == crate::keys::key('n') || key == crate::keys::key('N') || key == Key::new(named::ESCAPE) {
            // reread timestamp in case the file was modified again
            buffer.set_fs_status(get_fs_status(&buffer.filename())?);
            self.print_status(
                DisplayLine::default(),
                DisplayLine::from_str_face(
                    kformat!("'{}' kept", buffer.display_name()),
                    context.faces().face("Information"),
                ),
                -1,
                StatusStyle::Status,
            );
            if key == crate::keys::key('N') {
                set_autoreload(Autoreload::No)?;
            }
        } else {
            self.print_status(
                DisplayLine::default(),
                DisplayLine::from_str_face(kformat!("'{}' is not a valid choice", key), context.faces().face("Error")),
                -1,
                StatusStyle::Status,
            );
            let weak = self.self_weak.clone();
            self.input_handler.on_next_key(
                b"buffer-reload",
                KeymapMode::None,
                Rc::new(move |key, _| match weak.upgrade() {
                    Some(c) => c.on_buffer_reload_key(key),
                    None => Ok(()),
                }),
                None,
            )?;
            return Ok(());
        }

        for client in ClientManager::instance().clients() {
            if Rc::ptr_eq(&client.context().buffer(), &buffer) && client.state.borrow().buffer_reload_dialog_opened {
                client.close_buffer_reload_dialog()?;
            }
        }
        Ok(())
    }

    fn close_buffer_reload_dialog(&self) -> Result<()> {
        // Reset first as this might check for reloading.
        self.input_handler.reset_normal_mode()?;
        self.state.borrow_mut().buffer_reload_dialog_opened = false;
        self.info_hide(true);
        Ok(())
    }

    pub fn check_if_buffer_needs_reloading(&self) -> Result<()> {
        if self.state.borrow().buffer_reload_dialog_opened {
            return Ok(());
        }

        let context = self.context();
        let buffer = context.buffer();
        let reload = context.options().val::<Autoreload>("autoreload");
        if !buffer.flags().has(BufferFlags::File) || reload == Autoreload::No {
            return Ok(());
        }

        let res = (|| -> Result<()> {
            let filename = buffer.filename();
            let ts = get_fs_timestamp(&filename);
            let status = buffer.fs_status();
            if ts == INVALID_TIME || ts == status.timestamp {
                return Ok(());
            }
            {
                let fd = MappedFile::new(&filename)?;
                if fd.st.st_size == status.file_size && murmur3(&fd.data) == status.hash {
                    return Ok(());
                }
            }

            if reload == Autoreload::Ask {
                self.info_show_str(
                    &kformat!("reload '{}' ?", buffer.display_name()),
                    &kformat!(
                        "'{}' was modified externally\n y, <ret>: reload | n, <esc>: keep\n Y: always reload | N: always keep\n",
                        buffer.display_name()
                    ),
                    BufferCoord::default(),
                    InfoStyle::Modal,
                );
                self.state.borrow_mut().buffer_reload_dialog_opened = true;
                let weak = self.self_weak.clone();
                self.input_handler.on_next_key(
                    b"buffer-reload",
                    KeymapMode::None,
                    Rc::new(move |key, _| match weak.upgrade() {
                        Some(c) => c.on_buffer_reload_key(key),
                        None => Ok(()),
                    }),
                    None,
                )?;
            } else {
                self.reload_buffer()?;
            }
            Ok(())
        })();
        match res {
            Ok(()) => Ok(()),
            Err(error) if error.is_runtime() => {
                write_to_debug_buffer(&kformat!(
                    "Error while checking if buffer {} changed: {}",
                    buffer.filename(),
                    error.what()
                ));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub fn get_env_var(&self, name: &[u8]) -> Option<&[u8]> {
        self.env_vars.get(name).map(|v| v.as_slice())
    }

    pub fn env_vars(&self) -> &EnvVarMap {
        &self.env_vars
    }

    pub fn menu_show(&self, choices: Vec<DisplayLine>, anchor: BufferCoord, style: MenuStyle) {
        let mut state = self.state.borrow_mut();
        state.menu = Menu { items: choices, anchor, ui_anchor: None, style: Some(style), selected: -1 };
        state.ui_pending |= pending::MENU_SHOW;
        state.ui_pending &= !pending::MENU_HIDE;
    }

    pub fn menu_select(&self, selected: i32) {
        let mut state = self.state.borrow_mut();
        state.menu.selected = selected;
        state.ui_pending |= pending::MENU_SELECT;
        state.ui_pending &= !pending::MENU_HIDE;
    }

    pub fn menu_hide(&self) {
        let mut state = self.state.borrow_mut();
        state.menu = Menu::default();
        state.ui_pending |= pending::MENU_HIDE;
        state.ui_pending &= !(pending::MENU_SHOW | pending::MENU_SELECT);
    }

    pub fn info_show(&self, title: DisplayLine, content: DisplayLineList, anchor: BufferCoord, style: InfoStyle) {
        let mut state = self.state.borrow_mut();
        if state.info.style == Some(InfoStyle::Modal) {
            // We already have a modal info opened, do not touch it.
            return;
        }
        state.info = Info { title, content, anchor, ui_anchor: None, style: Some(style) };
        state.ui_pending |= pending::INFO_SHOW;
        state.ui_pending &= !pending::INFO_HIDE;
        state.pending_clear &= !PENDING_CLEAR_INFO;
    }

    pub fn info_show_str(&self, title: &[u8], content: &[u8], anchor: BufferCoord, style: InfoStyle) {
        let content = if content.last() == Some(&b'\n') { &content[..content.len() - 1] } else { content };
        let title = if title.is_empty() { DisplayLine::default() } else { DisplayLine::from_str_face(title, Face::default()) };
        let lines = crate::string_utils::split(content, b'\n')
            .into_iter()
            .map(|s| DisplayLine::from_str_face(crate::string_utils::replace(s, b"\t", b" "), Face::default()))
            .collect();
        self.info_show(title, lines, anchor, style);
    }

    pub fn info_hide(&self, even_modal: bool) {
        let mut state = self.state.borrow_mut();
        if !even_modal && state.info.style == Some(InfoStyle::Modal) {
            return;
        }
        state.info = Info::default();
        state.ui_pending |= pending::INFO_HIDE;
        state.ui_pending &= !pending::INFO_SHOW;
    }

    pub fn info_pending(&self) -> bool {
        self.state.borrow().ui_pending & pending::INFO_SHOW != 0
    }

    pub fn status_line_pending(&self) -> bool {
        self.state.borrow().ui_pending & pending::STATUS_LINE != 0
    }

    pub fn schedule_clear(&self) {
        let mut state = self.state.borrow_mut();
        if state.ui_pending & pending::INFO_SHOW == 0 {
            state.pending_clear |= PENDING_CLEAR_INFO;
        }
        if state.ui_pending & pending::STATUS_LINE == 0 {
            state.pending_clear |= PENDING_CLEAR_STATUS_LINE;
        }
    }

    pub fn clear_pending(&self) {
        let pending_clear = self.state.borrow().pending_clear;
        if pending_clear & PENDING_CLEAR_STATUS_LINE != 0 {
            self.print_status(DisplayLine::default(), DisplayLine::default(), -1, StatusStyle::Status);
        }
        if pending_clear & PENDING_CLEAR_INFO != 0 {
            self.info_hide(false);
        }
        self.state.borrow_mut().pending_clear = 0;
    }

    fn status_snapshot(&self) -> (DisplayLine, DisplayLine, ColumnCount, StatusStyle) {
        let state = self.state.borrow();
        (state.status_prompt.clone(), state.status_content.clone(), state.status_cursor_pos, state.status_style)
    }
}

impl OptionWatcher for Client {
    fn on_option_changed(&self, option: &Opt) -> Result<()> {
        if option.name() == b"ui_options" {
            self.ui.set_ui_options(&option.get::<UiOptions>());
        }
        // a highlighter might depend on the option, so we need to redraw
        self.state.borrow_mut().ui_pending |= pending::DRAW;
        Ok(())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let window = self.window();
        let me = self.self_weak.clone() as Weak<dyn OptionWatcher>;
        window.options().unregister_watcher(&me);
        window.set_client(None);
        if ClientManager::has_instance() && self.context().has_buffer() {
            // Do not move the selections here, as we need them to be valid
            // in order to correctly destroy the input handler
            let selections = self.context().selections_upd(false).clone();
            ClientManager::instance().add_free_window(window, selections);
        }
    }
}

fn is_inline(style: InfoStyle) -> bool {
    matches!(style, InfoStyle::Inline | InfoStyle::InlineAbove | InfoStyle::InlineBelow)
}

const WAIT_TIMEOUT: Duration = Duration::from_secs(1);

type PreviousStatus = (DisplayLine, DisplayLine, ColumnCount, StatusStyle);

/// Displays a status message while a long operation is running
pub struct BusyIndicator<'a> {
    context: &'a Context,
    _timer: Timer,
    previous_status: Rc<RefCell<Option<PreviousStatus>>>,
}

impl<'a> BusyIndicator<'a> {
    pub fn new(context: &'a Context, status_message: Box<dyn Fn(Duration) -> DisplayLine>, wait_time: Instant) -> Self {
        let previous_status: Rc<RefCell<Option<PreviousStatus>>> = Rc::new(RefCell::new(None));
        let client = if context.has_client() { Some(context.client().weak()) } else { None };
        let prev = previous_status.clone();
        let timer = Timer::new(
            Some(wait_time + WAIT_TIMEOUT),
            move |timer| {
                let Some(client) = client.as_ref().and_then(|c| c.upgrade()) else { return Ok(()) };
                let now = Instant::now();
                timer.set_next_date(Some(now + WAIT_TIMEOUT));
                if prev.borrow().is_none() {
                    *prev.borrow_mut() = Some(client.status_snapshot());
                }
                client.print_status(
                    DisplayLine::default(),
                    status_message(Duration::from_secs((now - wait_time).as_secs())),
                    -1,
                    StatusStyle::Status,
                );
                client.redraw_ifn()
            },
            EventMode::Urgent,
        );
        BusyIndicator { context, _timer: timer, previous_status }
    }

    /// Do not restore the previous status (C++ skips it when unwinding due to an exception)
    pub fn skip_restore(&self) {
        self.previous_status.borrow_mut().take();
    }
}

impl Drop for BusyIndicator<'_> {
    fn drop(&mut self) {
        let previous = self.previous_status.borrow_mut().take();
        if let Some((prompt, content, cursor_pos, style)) = previous {
            // restore the status line
            self.context.print_status_full(prompt, content, cursor_pos, style);
            if let Ok(client) = self.context.try_client() {
                let _ = client.redraw_ifn();
            }
        }
    }
}
