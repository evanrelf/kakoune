//! Port of keys.hh/keys.cc

use crate::exception::{Error, Result};
use crate::format::Cp;
use crate::string_utils::{char_length, str_to_int};
use crate::unicode::{is_basic_alpha, to_lower, to_upper};
use crate::units::{Codepoint, DisplayCoord};
use crate::utf8;

crate::kak_flags! {
    pub struct Modifiers: u32 {
        const None    = 0;
        const Control = 1 << 0;
        const Alt     = 1 << 1;
        const Shift   = 1 << 2;
        const MousePress     = 1 << 3;
        const MouseRelease   = 1 << 4;
        const MousePos       = 1 << 5;
        const MouseButtonMask= 0b11 << 6;
        const Scroll     = 1 << 8;
        const Resize     = 1 << 9;
        const MenuSelect = 1 << 10;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
}

/// Named keys use the UTF-16 surrogate pairs range
pub mod named {
    use crate::units::Codepoint;
    pub const BACKSPACE: Codepoint = 0xD800;
    pub const DELETE: Codepoint = 0xD801;
    pub const ESCAPE: Codepoint = 0xD802;
    pub const RETURN: Codepoint = 0xD803;
    pub const UP: Codepoint = 0xD804;
    pub const DOWN: Codepoint = 0xD805;
    pub const LEFT: Codepoint = 0xD806;
    pub const RIGHT: Codepoint = 0xD807;
    pub const PAGE_UP: Codepoint = 0xD808;
    pub const PAGE_DOWN: Codepoint = 0xD809;
    pub const HOME: Codepoint = 0xD80A;
    pub const END: Codepoint = 0xD80B;
    pub const INSERT: Codepoint = 0xD80C;
    pub const TAB: Codepoint = 0xD80D;
    pub const SPACE: Codepoint = 0xD80E;
    pub const F1: Codepoint = 0xD80F;
    pub const F2: Codepoint = 0xD810;
    pub const F3: Codepoint = 0xD811;
    pub const F4: Codepoint = 0xD812;
    pub const F5: Codepoint = 0xD813;
    pub const F6: Codepoint = 0xD814;
    pub const F7: Codepoint = 0xD815;
    pub const F8: Codepoint = 0xD816;
    pub const F9: Codepoint = 0xD817;
    pub const F10: Codepoint = 0xD818;
    pub const F11: Codepoint = 0xD819;
    pub const F12: Codepoint = 0xD81A;
    pub const FOCUS_IN: Codepoint = 0xD81B;
    pub const FOCUS_OUT: Codepoint = 0xD81C;
    pub const INVALID: Codepoint = 0xD81D;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Key {
    pub modifiers: Modifiers,
    pub key: Codepoint,
}

impl PartialOrd for Key {
    fn partial_cmp(&self, o: &Key) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Key {
    fn cmp(&self, o: &Key) -> std::cmp::Ordering {
        self.val().cmp(&o.val())
    }
}

impl From<Codepoint> for Key {
    fn from(key: Codepoint) -> Key {
        Key::new(key)
    }
}
impl From<char> for Key {
    fn from(key: char) -> Key {
        Key::new(key as Codepoint)
    }
}

impl Key {
    pub const fn new(key: Codepoint) -> Key {
        Key { modifiers: Modifiers::None, key }
    }
    pub const fn with_mod(modifiers: Modifiers, key: Codepoint) -> Key {
        Key { modifiers, key }
    }
    pub const fn val(&self) -> u64 {
        ((self.modifiers.0 as u64) << 32) | self.key as u64
    }
    pub const fn coord(&self) -> DisplayCoord {
        DisplayCoord::new(((self.key & 0xFFFF0000) as i32) >> 16, (self.key & 0x0000FFFF) as i32)
    }
    pub fn mouse_button(&self) -> MouseButton {
        match (self.modifiers.0 & Modifiers::MouseButtonMask.0) >> 6 {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            _ => MouseButton::Right,
        }
    }
    pub fn scroll_amount(&self) -> i32 {
        (self.modifiers.0 as i32) >> 16
    }
    pub fn to_modifier(button: MouseButton) -> Modifiers {
        Modifiers(((button as u32) << 6) & Modifiers::MouseButtonMask.0)
    }
    pub fn codepoint(&self) -> Option<Codepoint> {
        if *self == Key::new(named::RETURN) {
            return Some('\n' as u32);
        }
        if *self == Key::new(named::TAB) {
            return Some('\t' as u32);
        }
        if *self == Key::new(named::SPACE) || *self == shift(Key::new(named::SPACE)) {
            return Some(' ' as u32);
        }
        if *self == Key::new(named::ESCAPE) {
            return Some(0x1B);
        }
        if self.modifiers == Modifiers::None && self.key > 27 && (self.key < 0xD800 || self.key > 0xDFFF) {
            return Some(self.key);
        }
        None
    }
}

pub type KeyList = Vec<Key>;

pub const fn shift(key: Key) -> Key {
    Key { modifiers: Modifiers(key.modifiers.0 | Modifiers::Shift.0), key: key.key }
}
pub const fn alt(key: Key) -> Key {
    Key { modifiers: Modifiers(key.modifiers.0 | Modifiers::Alt.0), key: key.key }
}
pub const fn ctrl(key: Key) -> Key {
    Key { modifiers: Modifiers(key.modifiers.0 | Modifiers::Control.0), key: key.key }
}
pub const fn key(c: char) -> Key {
    Key::new(c as Codepoint)
}
pub const fn encode_coord(coord: DisplayCoord) -> Codepoint {
    ((coord.line << 16) | (coord.column & 0x0000FFFF)) as Codepoint
}
pub const fn resize(dim: DisplayCoord) -> Key {
    Key { modifiers: Modifiers::Resize, key: encode_coord(dim) }
}

fn key_parse_error(msg: Vec<u8>) -> Error {
    Error::runtime(msg)
}

fn canonicalize_ifn(mut key: Key) -> Result<Key> {
    if key.key > 0 && key.key < 27 {
        key.modifiers = Modifiers::Control;
        key.key = key.key - 1 + 'a' as u32;
    }
    if key.modifiers.has(Modifiers::Shift) {
        if is_basic_alpha(key.key) {
            // Shift + ASCII letters is just the uppercase letter.
            key.modifiers.remove(Modifiers::Shift);
            key.key = to_upper(key.key);
        } else if key.key < 0xD800 || key.key > 0xDFFF {
            // Shift + any other printable character is not allowed.
            return Err(key_parse_error(kformat!(
                "Shift modifier only works on special keys and lowercase ASCII, not '{}'",
                Cp(key.key)
            )));
        }
    }
    Ok(key)
}

pub const KEY_NAME_MAP: &[(&str, Codepoint)] = &[
    ("ret", named::RETURN),
    ("space", named::SPACE),
    ("tab", named::TAB),
    ("lt", '<' as u32),
    ("gt", '>' as u32),
    ("backspace", named::BACKSPACE),
    ("esc", named::ESCAPE),
    ("up", named::UP),
    ("down", named::DOWN),
    ("left", named::LEFT),
    ("right", named::RIGHT),
    ("pageup", named::PAGE_UP),
    ("pagedown", named::PAGE_DOWN),
    ("home", named::HOME),
    ("end", named::END),
    ("ins", named::INSERT),
    ("del", named::DELETE),
    ("plus", '+' as u32),
    ("minus", '-' as u32),
    ("semicolon", ';' as u32),
    ("percent", '%' as u32),
    ("quote", '\'' as u32),
    ("dquote", '"' as u32),
    ("focus_in", named::FOCUS_IN),
    ("focus_out", named::FOCUS_OUT),
];

pub fn parse_keys(s: &[u8]) -> Result<KeyList> {
    let mut result = KeyList::new();
    let end = s.len();
    let mut it = 0;
    while it < end {
        let cp = utf8::codepoint_at(s, it);
        if cp != '<' as u32 {
            let converted = match cp {
                0x0A => named::RETURN,
                0x0D => named::RETURN,
                0x08 => named::BACKSPACE,
                0x09 => named::TAB,
                0x20 => named::SPACE,
                0x1B => named::ESCAPE,
                c => c,
            };
            result.push(Key::new(converted));
            it = utf8::next(s, it);
            continue;
        }
        // find '>' codepoint
        let mut end_it = it;
        while end_it < end && s[end_it] != b'>' {
            end_it = utf8::next(s, end_it);
        }
        if end_it == end {
            result.push(Key::new(cp));
            it = utf8::next(s, it);
            continue;
        }
        let mut modifier = Modifiers::None;
        let full_desc = &s[it..end_it + 1];
        let mut desc = &s[it + 1..end_it];
        while let Some(dash) = desc.iter().position(|&c| c == b'-') {
            if dash != 1 {
                return Err(key_parse_error(kformat!("unable to parse modifier in '{}'", full_desc)));
            }
            match to_lower(desc[0] as u32) as u8 {
                b'c' => modifier |= Modifiers::Control,
                b'a' => modifier |= Modifiers::Alt,
                b's' => modifier |= Modifiers::Shift,
                _ => return Err(key_parse_error(kformat!("unable to parse modifier in '{}'", full_desc))),
            }
            desc = &desc[dash + 1..];
        }
        if let Some((_, k)) = KEY_NAME_MAP.iter().find(|(name, _)| name.as_bytes() == desc) {
            result.push(canonicalize_ifn(Key::with_mod(modifier, *k))?);
        } else if char_length(desc) == 1 {
            result.push(canonicalize_ifn(Key::with_mod(modifier, utf8::codepoint(desc)))?);
        } else if !desc.is_empty() && desc[0] == b'F' && desc.len() <= 3 {
            let val = str_to_int(&desc[1..])?;
            if (1..=12).contains(&val) {
                result.push(Key::with_mod(modifier, named::F1 + (val - 1) as u32));
            } else {
                return Err(key_parse_error(kformat!("only F1 through F12 are supported, not '{}'", desc)));
            }
        } else {
            let mut msg = b"unable to parse ".to_vec();
            msg.extend_from_slice(full_desc);
            return Err(key_parse_error(msg));
        }
        it = end_it + 1;
    }
    Ok(result)
}

pub fn button_to_str(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Middle => "middle",
        MouseButton::Right => "right",
    }
}

pub fn str_to_button(s: &[u8]) -> Result<MouseButton> {
    match s {
        b"left" => Ok(MouseButton::Left),
        b"middle" => Ok(MouseButton::Middle),
        b"right" => Ok(MouseButton::Right),
        _ => Err(rt_err!("invalid mouse button name {}", s)),
    }
}

pub fn key_to_str(key: Key) -> Vec<u8> {
    let coord = key.coord() + DisplayCoord::new(1, 1);
    let mut named = true;
    let mut res: Vec<u8>;
    if key.modifiers.has(Modifiers::MousePos) {
        res = kformat!("mouse:move:{}.{}", coord.line, coord.column);
    } else if key.modifiers.has(Modifiers::MousePress) {
        res = kformat!("mouse:press:{}:{}.{}", button_to_str(key.mouse_button()), coord.line, coord.column);
    } else if key.modifiers.has(Modifiers::MouseRelease) {
        res = kformat!("mouse:release:{}:{}.{}", button_to_str(key.mouse_button()), coord.line, coord.column);
    } else if key.modifiers.has(Modifiers::Scroll) {
        res = kformat!("scroll:{}:{}.{}", key.scroll_amount(), coord.line, coord.column);
    } else if key.modifiers.has(Modifiers::Resize) {
        res = kformat!("resize:{}.{}", coord.line, coord.column);
    } else if let Some((name, _)) = KEY_NAME_MAP.iter().find(|(_, k)| *k == key.key) {
        res = name.as_bytes().to_vec();
    } else if key.key >= named::F1 && key.key <= named::F12 {
        res = kformat!("F{}", (key.key - named::F1 + 1) as i32);
    } else {
        named = false;
        res = utf8::encode(key.key);
    }
    if key.modifiers.has(Modifiers::Shift) {
        res.splice(0..0, b"s-".iter().copied());
        named = true;
    }
    if key.modifiers.has(Modifiers::Alt) {
        res.splice(0..0, b"a-".iter().copied());
        named = true;
    }
    if key.modifiers.has(Modifiers::Control) {
        res.splice(0..0, b"c-".iter().copied());
        named = true;
    }
    if named {
        res.insert(0, b'<');
        res.push(b'>');
    }
    res
}

impl crate::format::FormatParam for Key {
    fn format_param(&self) -> std::borrow::Cow<'_, [u8]> {
        std::borrow::Cow::Owned(key_to_str(*self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keys() {
        let keys = vec![
            Key::new(named::SPACE),
            key('c'),
            Key::new(named::UP),
            alt(key('j')),
            ctrl(key('r')),
            shift(Key::new(named::UP)),
            ctrl(key('[')),
            ctrl(key('\\')),
            ctrl(key(']')),
            ctrl(key('_')),
        ];
        let mut s = Vec::new();
        for k in &keys {
            s.extend(key_to_str(*k));
        }
        assert_eq!(parse_keys(&s).unwrap(), keys);
        assert_eq!(parse_keys(b"a<c-a-b>c").unwrap(), vec![key('a'), ctrl(alt(key('b'))), key('c')]);
        assert_eq!(parse_keys(b"x").unwrap(), vec![key('x')]);
        assert_eq!(parse_keys(b"<x>").unwrap(), vec![key('x')]);
        assert_eq!(parse_keys(b"<s-x>").unwrap(), vec![key('X')]);
        assert_eq!(parse_keys(b"<s-X>").unwrap(), vec![key('X')]);
        assert_eq!(parse_keys(b"<X>").unwrap(), vec![key('X')]);
        assert_eq!(parse_keys(b"<s-up>").unwrap(), vec![shift(Key::new(named::UP))]);
        assert_eq!(parse_keys(b"<s-tab>").unwrap(), vec![shift(Key::new(named::TAB))]);
        assert_eq!(parse_keys(b"\n").unwrap(), vec![Key::new(named::RETURN)]);
        assert_eq!(key_to_str(shift(Key::new(named::TAB))), b"<s-tab>");
        for bad in ["<-x>", "<xy-z>", "<x-y>", "<s-/>", "<s-ë>", "<s-lt>", "<f99>", "<backtab>", "<invalidkey>"] {
            assert!(parse_keys(bad.as_bytes()).is_err(), "{}", bad);
        }
    }
}
