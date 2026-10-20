//! STUB: partial port of normal.hh/cc, to be completed

use crate::buffer::Buffer;
use crate::context::Context;
use crate::exception::Result;
use crate::keys::Key;
use crate::keymap_manager::KeymapMode;
use crate::keys::key_to_str;
use crate::string_utils::{column_length, join, string_of_cp_cols};
use crate::units::BufferCoord;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct NormalParams {
    pub count: i32,
    pub reg: u8,
}

#[derive(Clone, Copy)]
pub struct NormalCmd {
    pub docstring: &'static str,
    pub func: fn(&Context, NormalParams) -> Result<()>,
}

pub fn get_normal_command(_key: Key) -> Option<NormalCmd> {
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PasteMode {
    Append,
    Insert,
    Replace,
}

pub fn paste_pos(buffer: &Buffer, min: BufferCoord, max: BufferCoord, mode: PasteMode, linewise: bool) -> BufferCoord {
    match mode {
        PasteMode::Append => {
            if buffer.is_end(max) {
                return max;
            }
            if linewise { BufferCoord::line(buffer.line_count().min(max.line + 1)) } else { buffer.char_next(max) }
        }
        PasteMode::Insert => {
            if linewise { BufferCoord::line(min.line) } else { min }
        }
        PasteMode::Replace => BufferCoord::default(),
    }
}

pub struct KeyInfo {
    pub keys: Vec<Key>,
    pub docstring: &'static str,
}

pub fn build_autoinfo_for_mapping(context: &Context, mode: KeymapMode, built_ins: &[KeyInfo]) -> Vec<u8> {
    let keymaps = context.keymaps();

    let mut descs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for built_in in built_ins {
        let keys = join(
            built_in.keys.iter().filter(|k| keymaps.get_mapping(**k, mode).is_none()).map(|k| key_to_str(*k)),
            b',',
            false,
        );
        if !keys.is_empty() {
            descs.push((keys, built_in.docstring.as_bytes().to_vec()));
        }
    }

    for key in keymaps.get_mapped_keys(mode) {
        let Some(mapping) = keymaps.get_mapping(key, mode) else { continue };
        let docstring = mapping.docstring;
        if mapping.keys.is_empty() && docstring.is_empty() {
            continue;
        }
        if let Some(elem) = descs.iter_mut().find(|e| e.1 == docstring) {
            elem.0.push(b',');
            elem.0.extend(key_to_str(key));
        } else {
            descs.push((key_to_str(key), docstring));
        }
    }

    let max_len = descs.iter().map(|(k, _)| column_length(k)).max().unwrap_or(0);

    let mut res = Vec::new();
    for (keys, docstring) in &descs {
        res.extend(kformat!("{}:{}{}\n", keys, string_of_cp_cols(' ' as u32, max_len - column_length(keys) + 1), docstring));
    }
    res
}
