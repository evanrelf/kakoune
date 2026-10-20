//! Port of client_manager.hh/cc

use crate::buffer::{Buffer, BufferFlags};
use crate::buffer_manager::BufferManager;
use crate::client::{Client, OnExitCallback};
use crate::command_manager::{CommandManager, ShellContext};
use crate::completion::{complete, CandidateList};
use crate::debug::EnvVarMap;
use crate::display_buffer::DisplayLine;
use crate::exception::Result;
use crate::hook_manager::Hook;
use crate::selection::{Selection, SelectionList};
use crate::units::BufferCoord;
use crate::user_interface::UserInterface;
use crate::window::Window;
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

pub struct WindowAndSelections {
    pub window: Rc<Window>,
    pub selections: SelectionList,
}

pub struct ClientManager {
    clients: RefCell<Vec<Rc<Client>>>,
    client_trash: RefCell<Vec<Rc<Client>>>,
    free_windows: RefCell<Vec<WindowAndSelections>>,
    window_trash: RefCell<Vec<Rc<Window>>>,
}

thread_local! {
    static CLIENT_MANAGER: OnceCell<&'static ClientManager> = const { OnceCell::new() };
}

impl ClientManager {
    pub fn create() -> &'static ClientManager {
        let cm: &'static ClientManager = Box::leak(Box::new(ClientManager {
            clients: RefCell::new(Vec::new()),
            client_trash: RefCell::new(Vec::new()),
            free_windows: RefCell::new(Vec::new()),
            window_trash: RefCell::new(Vec::new()),
        }));
        CLIENT_MANAGER.with(|c| {
            let _ = c.set(cm);
        });
        cm
    }

    pub fn instance() -> &'static ClientManager {
        CLIENT_MANAGER.with(|c| *c.get().expect("ClientManager not created"))
    }

    pub fn has_instance() -> bool {
        CLIENT_MANAGER.with(|c| c.get().is_some())
    }

    pub fn clients(&self) -> Vec<Rc<Client>> {
        self.clients.borrow().clone()
    }

    pub fn empty(&self) -> bool {
        self.clients.borrow().is_empty()
    }

    pub fn count(&self) -> usize {
        self.clients.borrow().len()
    }

    pub fn clear(&self, disconnect_clients: bool) {
        if disconnect_clients {
            loop {
                let first = self.clients.borrow().first().cloned();
                match first {
                    Some(client) => {
                        let _ = self.remove_client(&client, true, 0);
                    }
                    None => break,
                }
            }
        } else {
            let clients = std::mem::take(&mut *self.clients.borrow_mut());
            drop(clients);
        }
        let trash = std::mem::take(&mut *self.client_trash.borrow_mut());
        drop(trash);
        let free_windows = std::mem::take(&mut *self.free_windows.borrow_mut());
        for window in &free_windows {
            let _ = window.window.run_hook_in_own_context(Hook::WinClose, &window.window.buffer().name(), Vec::new());
        }
        drop(free_windows);
        let window_trash = std::mem::take(&mut *self.window_trash.borrow_mut());
        drop(window_trash);
    }

    fn generate_name(&self) -> Vec<u8> {
        let mut i = 0;
        loop {
            let name = kformat!("client{}", i);
            if !self.client_name_exists(&name) {
                return name;
            }
            i += 1;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_client(
        &self,
        ui: Box<dyn UserInterface>,
        pid: i32,
        name: Vec<u8>,
        env_vars: EnvVarMap,
        init_cmds: &[u8],
        init_buffer: &[u8],
        init_coord: Option<BufferCoord>,
        on_exit: OnExitCallback,
    ) -> Result<Option<Rc<Client>>> {
        let buffer = if init_buffer.is_empty() { None } else { BufferManager::instance().get_buffer_ifp(init_buffer) };
        let buffer = match buffer {
            Some(b) => b,
            None => BufferManager::instance().get_first_buffer()?,
        };

        buffer.add_flags(BufferFlags::Locked);
        let mut unlock = Some(crate::utils::OnScopeEnd::new(|| buffer.remove_flags(BufferFlags::Locked)));

        let ws = self.get_free_window(&buffer)?;
        let name = if name.is_empty() { self.generate_name() } else { name };
        let client = Client::new(ui, ws.window, ws.selections, pid, env_vars, name, on_exit)?;
        self.clients.borrow_mut().push(client.clone());

        let context = client.context();
        if context.buffer().name() == b"*scratch*" {
            context.print_status(DisplayLine::from_str_face(
                "This *scratch* buffer won't be automatically saved",
                context.faces().face("Information"),
            ));
        }

        if let Some(init_coord) = init_coord {
            *context.selections_write_only() =
                SelectionList::new(buffer.clone(), Selection::new(context.buffer().clamp(init_coord)));
            context.window().center_line(init_coord.line);
        }

        unlock.take();

        let res = (|| -> Result<()> {
            context.hooks().run_hook(Hook::ClientCreate, &context.name(), context)?;
            CommandManager::instance().execute(init_cmds, context, &ShellContext::default())
        })();
        match res {
            Ok(()) => {}
            Err(error) if error.is_runtime() => {
                context.print_status(DisplayLine::from_str_face(error.what().to_vec(), context.faces().face("Error")));
                context.hooks().run_hook(Hook::RuntimeError, error.what(), context)?;
            }
            Err(error) => return Err(error),
        }

        // Do not return the client if it already got moved to the trash
        let alive = self.clients.borrow().iter().any(|c| Rc::ptr_eq(c, &client));
        Ok(if alive { Some(client) } else { None })
    }

    pub fn process_pending_inputs(&self) -> Result<bool> {
        let mut processed_some_input = false;
        loop {
            let mut had_input = false;
            // Use index based iteration as clients might get mutated during
            // client input processing
            let mut i = 0;
            loop {
                let client = match self.clients.borrow().get(i) {
                    Some(c) => c.clone(),
                    None => break,
                };
                if !client.is_ui_ok() {
                    self.remove_client(&client, false, -1)?;
                    continue;
                }
                had_input = client.process_pending_inputs()? || had_input;
                processed_some_input |= had_input;
                i += 1;
            }
            if !had_input {
                break;
            }
        }
        Ok(processed_some_input)
    }

    pub fn has_pending_inputs(&self) -> bool {
        self.clients.borrow().iter().any(|c| c.has_pending_inputs())
    }

    pub fn remove_client(&self, client: &Rc<Client>, graceful: bool, status: i32) -> Result<()> {
        let removed = {
            let mut clients = self.clients.borrow_mut();
            let Some(pos) = clients.iter().position(|c| Rc::ptr_eq(c, client)) else {
                return Ok(());
            };
            clients.remove(pos)
        };
        self.client_trash.borrow_mut().push(removed);

        let context = client.context();
        let res = context.hooks().run_hook(Hook::ClientClose, &context.name(), context);
        client.exit(status);

        if !graceful && self.clients.borrow().is_empty() {
            BufferManager::instance().backup_modified_buffers();
        }
        res
    }

    pub fn get_free_window(&self, buffer: &Rc<Buffer>) -> Result<WindowAndSelections> {
        let found = {
            let mut free_windows = self.free_windows.borrow_mut();
            free_windows
                .iter()
                .rposition(|ws| Rc::ptr_eq(ws.window.buffer(), buffer))
                .map(|pos| free_windows.remove(pos))
        };
        match found {
            None => Ok(WindowAndSelections {
                window: Window::new(buffer)?,
                selections: SelectionList::new(buffer.clone(), Selection::default()),
            }),
            Some(mut res) => {
                res.selections.update(true);
                Ok(res)
            }
        }
    }

    pub fn add_free_window(&self, window: Rc<Window>, selections: SelectionList) {
        let alive = BufferManager::instance().buffers().iter().any(|b| Rc::ptr_eq(b, window.buffer()));
        if !alive {
            self.window_trash.borrow_mut().push(window);
            return;
        }
        window.clear_display_buffer();
        self.free_windows.borrow_mut().push(WindowAndSelections { window, selections });
    }

    pub fn ensure_no_client_uses_buffer(&self, buffer: &Buffer) -> Result<()> {
        for client in self.clients() {
            client.context().forget_buffer(buffer)?;
        }

        let removed_windows: Vec<Rc<Window>> = {
            let mut free_windows = self.free_windows.borrow_mut();
            let mut removed = Vec::new();
            free_windows.retain(|ws| {
                if !std::ptr::eq(ws.window.buffer().as_ref(), buffer) {
                    return true;
                }
                removed.push(ws.window.clone());
                false
            });
            removed
        };
        for removed_window in removed_windows {
            removed_window.run_hook_in_own_context(Hook::WinClose, &removed_window.buffer().name(), Vec::new())?;
            self.window_trash.borrow_mut().push(removed_window);
        }
        Ok(())
    }

    pub fn clear_window_trash(&self) {
        let trash = std::mem::take(&mut *self.window_trash.borrow_mut());
        drop(trash);
    }

    pub fn clear_client_trash(&self) {
        let trash = std::mem::take(&mut *self.client_trash.borrow_mut());
        drop(trash);
    }

    pub fn client_name_exists(&self, name: &[u8]) -> bool {
        self.get_client_ifp(name).is_some()
    }

    pub fn get_client_ifp(&self, name: &[u8]) -> Option<Rc<Client>> {
        self.clients.borrow().iter().find(|c| c.context().name() == name).cloned()
    }

    pub fn get_client(&self, name: &[u8]) -> Result<Rc<Client>> {
        self.get_client_ifp(name).ok_or_else(|| rt_err!("no such client: '{}'", name))
    }

    pub fn redraw_clients(&self) -> Result<()> {
        for client in self.clients() {
            client.redraw_ifn()?;
        }
        Ok(())
    }

    pub fn complete_client_name(&self, prefix: &[u8], cursor_pos: usize) -> CandidateList {
        let names: Vec<Vec<u8>> = self.clients.borrow().iter().map(|c| c.context().name()).collect();
        complete(prefix, cursor_pos, &names)
    }
}
