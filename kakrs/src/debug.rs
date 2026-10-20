//! Port of debug.hh/cc, profile.hh and env_vars.hh/cc

use crate::buffer::BufferFlags;
use crate::buffer_manager::BufferManager;
use crate::option_types::EnumDesc;
use indexmap::IndexMap;
use std::time::{Duration, Instant};

crate::kak_flags! {
    pub struct DebugFlags: u32 {
        const None     = 0;
        const Hooks    = 1 << 0;
        const Shell    = 1 << 1;
        const Profile  = 1 << 2;
        const Keys     = 1 << 3;
        const Commands = 1 << 4;
    }
}

impl EnumDesc for DebugFlags {
    fn desc() -> &'static [(Self, &'static str)] {
        &[
            (DebugFlags::Hooks, "hooks"),
            (DebugFlags::Shell, "shell"),
            (DebugFlags::Profile, "profile"),
            (DebugFlags::Keys, "keys"),
            (DebugFlags::Commands, "commands"),
        ]
    }
}
crate::flags_option!(DebugFlags);

pub fn write_to_debug_buffer(s: &[u8]) {
    if !BufferManager::has_instance() {
        let _ = crate::file::write_fd(2, s, true);
        let _ = crate::file::write_fd(2, b"\n", true);
        return;
    }
    const DEBUG_BUFFER_NAME: &[u8] = b"*debug*";
    // Try to ensure we keep an empty line at the end of the debug buffer
    // where the user can put its cursor to scroll with new messages
    let eol_back = s.last() == Some(&b'\n');
    if let Some(buffer) = BufferManager::instance().get_buffer_ifp(DEBUG_BUFFER_NAME) {
        let mut flags = buffer.flags();
        flags.remove(BufferFlags::ReadOnly);
        buffer.set_flags(flags);
        let content = if eol_back { s.to_vec() } else { kformat!("{}\n", s) };
        let _ = buffer.insert(buffer.back_coord(), &content);
        let mut flags = buffer.flags();
        flags.insert(BufferFlags::ReadOnly);
        buffer.set_flags(flags);
    } else {
        let line = kformat!("{}{}", s, if eol_back { "\n" } else { "\n\n" });
        let _ = crate::buffer_utils::create_buffer_from_string(
            DEBUG_BUFFER_NAME.to_vec(),
            BufferFlags::NoUndo | BufferFlags::Debug | BufferFlags::ReadOnly,
            &line,
        );
    }
}

/// Port of ProfileScope: call callback with the elapsed duration when dropped
pub struct ProfileScope<F: FnMut(Duration)> {
    start: Option<Instant>,
    callback: F,
}

impl<F: FnMut(Duration)> ProfileScope<F> {
    pub fn new(debug_flags: DebugFlags, callback: F, active: bool) -> Self {
        let active = active && debug_flags.has(DebugFlags::Profile);
        ProfileScope { start: if active { Some(Instant::now()) } else { None }, callback }
    }
}

impl<F: FnMut(Duration)> Drop for ProfileScope<F> {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            (self.callback)(start.elapsed());
        }
    }
}

pub type EnvVarMap = IndexMap<Vec<u8>, Vec<u8>>;

pub fn get_env_vars() -> EnvVarMap {
    use std::os::unix::ffi::OsStrExt;
    let mut env_vars = EnvVarMap::new();
    for (k, v) in std::env::vars_os() {
        env_vars.insert(k.as_bytes().to_vec(), v.as_bytes().to_vec());
    }
    env_vars
}
