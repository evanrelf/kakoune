//! Port of buffer_manager.hh/cc

use crate::buffer::{Buffer, BufferFlags, ByteOrderMark, EolFormat, FinalEol, StringDataPtr};
use crate::buffer_utils::write_buffer_to_backup_file;
use crate::client_manager::ClientManager;
use crate::exception::{Error, Result};
use crate::file::{parse_filename, real_path, FsStatus};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

pub struct BufferManager {
    buffers: RefCell<Vec<Rc<Buffer>>>,
    buffer_trash: RefCell<Vec<Rc<Buffer>>>,
}

thread_local! {
    static BUFFER_MANAGER: OnceCell<&'static BufferManager> = const { OnceCell::new() };
}

impl BufferManager {
    pub fn create() -> &'static BufferManager {
        let bm: &'static BufferManager =
            Box::leak(Box::new(BufferManager { buffers: RefCell::new(Vec::new()), buffer_trash: RefCell::new(Vec::new()) }));
        BUFFER_MANAGER.with(|b| {
            let _ = b.set(bm);
        });
        bm
    }

    pub fn instance() -> &'static BufferManager {
        BUFFER_MANAGER.with(|b| *b.get().expect("BufferManager not created"))
    }

    pub fn has_instance() -> bool {
        BUFFER_MANAGER.with(|b| b.get().is_some())
    }

    /// Port of the destructor: unregister all buffers
    pub fn shutdown(&self) {
        // Move buffers to avoid running BufClose with buffers remaining in that list
        let buffers = std::mem::take(&mut *self.buffers.borrow_mut());
        for buffer in &buffers {
            let _ = buffer.on_unregistered();
        }
        // Make sure not clients exists
        if ClientManager::has_instance() {
            ClientManager::instance().clear(true);
        }
        for buffer in buffers {
            buffer.values().clear();
        }
        self.clear_buffer_trash();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_buffer(
        &self,
        name: Vec<u8>,
        flags: BufferFlags,
        lines: Vec<StringDataPtr>,
        bom: ByteOrderMark,
        eolformat: EolFormat,
        finaleol: FinalEol,
        fs_status: FsStatus,
    ) -> Result<Rc<Buffer>> {
        if self.get_buffer_ifp(&name).is_some() {
            return Err(Error::runtime("buffer name is already in use"));
        }
        let buffer = Buffer::new(name, flags, lines, bom, eolformat, finaleol, fs_status)?;
        self.buffers.borrow_mut().push(buffer.clone());
        buffer.on_registered()?;
        if self.buffer_trash.borrow().iter().any(|b| Rc::ptr_eq(b, &buffer)) {
            return Err(Error::runtime("buffer got removed during its creation"));
        }
        Ok(buffer)
    }

    pub fn delete_buffer(&self, buffer: &Buffer) -> Result<()> {
        if buffer.flags().has(BufferFlags::Locked) {
            return Err(Error::runtime("Trying to delete a locked buffer"));
        }
        let removed = {
            let mut buffers = self.buffers.borrow_mut();
            // we might be trying to recursively delete this buffer
            let Some(pos) = buffers.iter().position(|b| std::ptr::eq(b.as_ref(), buffer)) else {
                return Ok(());
            };
            buffers.remove(pos)
        };
        self.buffer_trash.borrow_mut().push(removed);
        if ClientManager::has_instance() {
            ClientManager::instance().ensure_no_client_uses_buffer(buffer)?;
        }
        buffer.on_unregistered()
    }

    pub fn buffers(&self) -> Vec<Rc<Buffer>> {
        self.buffers.borrow().clone()
    }

    pub fn count(&self) -> usize {
        self.buffers.borrow().len()
    }

    pub fn get_buffer_ifp(&self, name: &[u8]) -> Option<Rc<Buffer>> {
        let filename = real_path(&parse_filename(name, b""));
        self.buffers
            .borrow()
            .iter()
            .find(|b| b.name() == name || (b.flags().has(BufferFlags::File) && b.filename() == filename))
            .cloned()
    }

    pub fn get_buffer(&self, name: &[u8]) -> Result<Rc<Buffer>> {
        self.get_buffer_ifp(name).ok_or_else(|| rt_err!("no such buffer '{}'", name))
    }

    pub fn get_buffer_matching_ifp(&self, filter: impl Fn(&Buffer) -> bool) -> Option<Rc<Buffer>> {
        self.buffers.borrow().iter().rev().find(|b| filter(b)).cloned()
    }

    pub fn get_buffer_matching(&self, filter: impl Fn(&Buffer) -> bool) -> Result<Rc<Buffer>> {
        self.get_buffer_matching_ifp(filter).ok_or_else(|| Error::runtime("no buffer found"))
    }

    pub fn get_first_buffer(&self) -> Result<Rc<Buffer>> {
        let all_debug = self.buffers.borrow().iter().all(|b| b.flags().has(BufferFlags::Debug));
        if all_debug {
            self.create_buffer(
                b"*scratch*".to_vec(),
                BufferFlags::None,
                vec![Rc::from(&b"\n"[..])],
                ByteOrderMark::None,
                EolFormat::Lf,
                FinalEol::Present,
                FsStatus::default(),
            )?;
        }
        Ok(self.buffers.borrow().last().unwrap().clone())
    }

    pub fn backup_modified_buffers(&self) {
        for buf in self.buffers() {
            if buf.flags().has(BufferFlags::File) && buf.is_modified() && !buf.flags().has(BufferFlags::ReadOnly) {
                write_buffer_to_backup_file(&buf);
            }
        }
    }

    pub fn clear_buffer_trash(&self) {
        let trash = std::mem::take(&mut *self.buffer_trash.borrow_mut());
        // values may hold watchers whose destruction needs the buffer alive
        for buffer in &trash {
            buffer.values().clear();
        }
        drop(trash);
    }

    pub fn arrange_buffers(&self, names: &[Vec<u8>], to_back: bool) -> Result<()> {
        let mut buffers = self.buffers.borrow_mut();
        let mut indices: Vec<usize> = Vec::new();
        for name in names {
            let filename = real_path(&parse_filename(name, b""));
            let Some(index) = buffers
                .iter()
                .position(|b| b.display_name() == *name || (b.flags().has(BufferFlags::File) && b.filename() == filename))
            else {
                return Err(rt_err!("no such buffer '{}'", name));
            };
            if indices.contains(&index) {
                return Err(rt_err!("buffer '{}' appears more than once", name));
            }
            indices.push(index);
        }
        let mut res: Vec<Rc<Buffer>> = indices.iter().map(|&i| buffers[i].clone()).collect();
        for (i, b) in buffers.iter().enumerate() {
            if !indices.contains(&i) {
                res.push(b.clone());
            }
        }
        if to_back {
            res.rotate_left(indices.len());
        }
        *buffers = res;
        Ok(())
    }

    pub fn make_latest(&self, buffer: &Buffer) {
        let mut buffers = self.buffers.borrow_mut();
        if let Some(pos) = buffers.iter().position(|b| std::ptr::eq(b.as_ref(), buffer)) {
            let b = buffers.remove(pos);
            buffers.push(b);
        }
    }
}
