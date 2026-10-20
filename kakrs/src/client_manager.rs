//! STUB: to be replaced by the port of client_manager.hh/cc
use crate::buffer::Buffer;
use crate::exception::Result;

pub struct ClientManager {}

impl ClientManager {
    pub fn has_instance() -> bool {
        false
    }
    pub fn instance() -> &'static ClientManager {
        todo!()
    }
    pub fn clear(&self, _disconnect_clients: bool) {
        todo!()
    }
    pub fn ensure_no_client_uses_buffer(&self, _buffer: &Buffer) -> Result<()> {
        todo!()
    }
}
