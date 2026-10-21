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
        // [DEVIATION] a mask value of 3 is not a valid C++ MouseButton
        // (to_string asserts then throws logic_error), it maps to Right here
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
        // [FAITHFUL] the modifiers are replaced, not extended (C++ only
        // asserts they are None), so "<s-\x1a>" or "<a-\x01>" lose them
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
        // find '>' codepoint ([FAITHFUL] as C++ std::find on a utf8
        // iterator, which compares decoded codepoints: an invalid "\xC0>"
        // decodes to '>')
        let mut end_it = it;
        while end_it < end && utf8::codepoint_at(s, end_it) != '>' as u32 {
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
            // [FAITHFUL] raw characters are not converted to named keys here,
            // so "< >" and "<\x1b>" give keys that print back as " " and
            // "\x1b", which then parse as <space> and <esc>
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
        // C++ sets it = end_it and the loop increment moves to the next
        // character start, skipping stray continuation bytes after '>'
        it = utf8::next(s, end_it);
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

    use hegel::generators as gs;
    use hegel::{Generator as _, TestCase};

    fn is_surrogate(cp: Codepoint) -> bool {
        (0xD800..=0xDFFF).contains(&cp)
    }

    fn any_key_codepoint() -> impl hegel::PrintableGenerator<Codepoint> {
        hegel::one_of!(
            gs::sampled_from(KEY_NAME_MAP.iter().map(|(_, k)| *k).collect::<Vec<_>>()),
            gs::integers::<u32>().min_value(named::F1).max_value(named::F12),
            gs::integers::<u32>().min_value(0xD800).max_value(0xDFFF),
            gs::integers::<u32>().max_value(0x7F),
            gs::integers::<u32>().max_value(0x10FFFF),
        )
    }

    fn any_modifiers() -> impl hegel::PrintableGenerator<Modifiers> {
        gs::integers::<u32>().max_value(7).map(|bits| {
            let mut m = Modifiers::None;
            for (bit, flag) in [(1, Modifiers::Control), (2, Modifiers::Alt), (4, Modifiers::Shift)] {
                if bits & bit != 0 {
                    m |= flag;
                }
            }
            m
        })
    }

    /// Keys that parse_keys can produce, i.e. that canonicalize_ifn leaves
    /// unchanged and that are not converted from raw characters.
    fn is_canonical(k: Key) -> bool {
        let unmodified = k.modifiers == Modifiers::None;
        k.key <= 0x10FFFF
            && !(1..27).contains(&k.key)
            && !(unmodified && (k.key == 0x1B || k.key == ' ' as u32))
            && (!k.modifiers.has(Modifiers::Shift) || is_surrogate(k.key))
    }

    fn canonical_key() -> impl hegel::PrintableGenerator<Key> {
        hegel::compose!(|tc| {
            let modifiers = tc.draw(any_modifiers());
            let key = tc.draw(any_key_codepoint());
            Key::with_mod(modifiers, key)
        })
        .filter(|k| is_canonical(*k))
    }

    #[hegel::test(test_cases = 1000)]
    fn parse_inverts_key_to_str(tc: TestCase) {
        let k = tc.draw(canonical_key());
        assert_eq!(parse_keys(&key_to_str(k)).unwrap(), vec![k]);
    }

    #[hegel::test(test_cases = 1000)]
    fn parse_inverts_key_to_str_on_sequences(tc: TestCase) {
        let keys = tc.draw(gs::vecs(canonical_key()).max_size(8));
        let s: Vec<u8> = keys.iter().flat_map(|k| key_to_str(*k)).collect();
        assert_eq!(parse_keys(&s).unwrap(), keys);
    }

    fn key_text() -> impl hegel::PrintableGenerator<String> {
        hegel::one_of!(
            gs::text(),
            gs::vecs(gs::sampled_from(vec![
                "<", ">", "-", "c-", "a-", "s-", "C-", "x", "X", "é", "F1", "F12", "ret", "lt", "gt", "space", "\n", "\t",
                " ", "\u{1b}", "\r", "\u{8}", "\u{1}", "\u{D7FF}",
            ]))
            .map(|v| v.concat()),
        )
    }

    // Printing the keys parsed from a string and parsing them again gives the
    // same keys back, except that "< >" and "<\x1b>" parse to raw space and
    // escape keys, which print unbracketed and so re-parse as <space> and
    // <esc> ([FAITHFUL] C++ behaviour).
    #[hegel::test(test_cases = 1000)]
    fn key_to_str_parse_is_idempotent(tc: TestCase) {
        let s = tc.draw(key_text());
        if let Ok(keys) = parse_keys(s.as_bytes()) {
            let printed: Vec<u8> = keys.iter().flat_map(|k| key_to_str(*k)).collect();
            let normalized: Vec<Key> = keys
                .iter()
                .map(|&k| match (k.modifiers, k.key) {
                    (Modifiers::None, 0x20) => Key::new(named::SPACE),
                    (Modifiers::None, 0x1B) => Key::new(named::ESCAPE),
                    _ => k,
                })
                .collect();
            assert_eq!(parse_keys(&printed).unwrap(), normalized);
        }
    }

    // As C++ steps a utf8 iterator past the closing '>', stray continuation
    // bytes following a bracketed key are skipped.
    #[hegel::test(test_cases = 1000)]
    fn continuation_bytes_after_bracketed_key_are_skipped(tc: TestCase) {
        let k = tc.draw(canonical_key().filter(|k| key_to_str(*k).starts_with(b"<")));
        let junk = tc.draw(gs::vecs(gs::integers::<u8>().min_value(0x80).max_value(0xBF)).max_size(3));
        let rest = tc.draw(key_text());
        let s = [key_to_str(k), junk, rest.clone().into_bytes()].concat();
        let mut expected = vec![k];
        expected.extend(parse_keys(rest.as_bytes()).unwrap_or_default());
        if let Ok(rest_keys) = parse_keys(rest.as_bytes()) {
            assert_eq!(parse_keys(&s).unwrap(), [vec![k], rest_keys].concat());
        }
    }

    // The closing '>' is searched for as a decoded codepoint: the invalid
    // sequence "\xC0>" decodes as '>' and so closes the key name.
    #[hegel::test(test_cases = 1000)]
    fn overlong_closing_bracket_closes_key_name(tc: TestCase) {
        let name = tc.draw(gs::sampled_from(KEY_NAME_MAP.iter().map(|(n, _)| *n).collect::<Vec<_>>()));
        let s = format!("<{name}").into_bytes();
        let expected = [parse_keys(&[&s[..], b">"].concat()).unwrap(), vec![key('>')]].concat();
        assert_eq!(parse_keys(&[&s[..], b"\xC0>"].concat()).unwrap(), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn text_without_angle_bracket_is_one_key_per_codepoint(tc: TestCase) {
        let s = tc.draw(key_text().filter(|s| !s.contains('<')));
        let expected: Vec<Key> = s
            .chars()
            .map(|c| {
                Key::new(match c {
                    '\n' | '\r' => named::RETURN,
                    '\u{8}' => named::BACKSPACE,
                    '\t' => named::TAB,
                    ' ' => named::SPACE,
                    '\u{1b}' => named::ESCAPE,
                    c => c as Codepoint,
                })
            })
            .collect();
        assert_eq!(parse_keys(s.as_bytes()).unwrap(), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn unclosed_angle_bracket_is_literal(tc: TestCase) {
        let s = tc.draw(key_text().filter(|s| !s.contains(['<', '>'])));
        let parsed = parse_keys(format!("{s}<").as_bytes()).unwrap();
        assert_eq!(parsed.last(), Some(&key('<')));
    }

    // Control codes 1..=26 are excluded: canonicalize_ifn turns them into
    // <c-a>..<c-z> *replacing* the modifiers before checking shift, so
    // "<s-\x1a>" is <c-z> ([FAITHFUL] C++ behaviour, it asserts no modifiers
    // in debug builds).
    #[hegel::test(test_cases = 1000)]
    fn shift_only_applies_to_letters_and_special_keys(tc: TestCase) {
        let c = tc.draw(gs::characters().filter(|&c| {
            !c.is_ascii_alphabetic() && !matches!(c, '<' | '>' | '-') && !(1..27).contains(&(c as u32))
        }));
        let k = format!("<s-{c}>");
        assert!(parse_keys(k.as_bytes()).is_err(), "{k}");
    }

    #[hegel::test(test_cases = 1000)]
    fn shift_letter_is_uppercase_letter(tc: TestCase) {
        let c = tc.draw(gs::characters().filter(|c| c.is_ascii_alphabetic()));
        let m = tc.draw(gs::sampled_from(vec!["", "c-", "a-", "c-a-"]));
        let expected = Key::with_mod(parse_keys(format!("<{m}x>").as_bytes()).unwrap()[0].modifiers, c.to_ascii_uppercase() as u32);
        assert_eq!(parse_keys(format!("<{m}s-{c}>").as_bytes()).unwrap(), vec![expected]);
    }

    #[hegel::test(test_cases = 1000)]
    fn function_keys_outside_1_to_12_are_errors(tc: TestCase) {
        let n = tc.draw(hegel::one_of!(gs::just(0), gs::integers::<i32>().min_value(13).max_value(99)));
        assert!(parse_keys(format!("<F{n}>").as_bytes()).is_err());
        let ok = tc.draw(gs::integers::<u32>().min_value(1).max_value(12));
        assert_eq!(parse_keys(format!("<F{ok}>").as_bytes()).unwrap(), vec![Key::new(named::F1 + ok - 1)]);
    }

    #[hegel::test(test_cases = 1000)]
    fn multi_char_modifier_is_an_error(tc: TestCase) {
        let m = tc.draw(gs::text().alphabet("cas").min_size(2).max_size(3));
        assert!(parse_keys(format!("<{m}-x>").as_bytes()).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn unknown_modifier_is_an_error(tc: TestCase) {
        let m = tc.draw(gs::characters().filter(|c| !matches!(c, 'c' | 'a' | 's' | 'C' | 'A' | 'S' | '>' | '-')));
        assert!(parse_keys(format!("<{m}-x>").as_bytes()).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn ctrl_letters_canonicalize_from_control_codes(tc: TestCase) {
        let c = tc.draw(gs::integers::<u8>().min_value(b'a').max_value(b'z'));
        let raw = [b'<', c - b'a' + 1, b'>'];
        assert_eq!(parse_keys(&raw).unwrap(), vec![ctrl(key(c as char))]);
    }

    #[hegel::test(test_cases = 1000)]
    fn key_codepoint_of_parsed_printable_char(tc: TestCase) {
        let c = tc.draw(gs::characters().filter(|&c| c as u32 > 27 && c != '<'));
        let keys = parse_keys(c.to_string().as_bytes()).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].codepoint(), Some(c as u32));
    }

    #[hegel::test(test_cases = 1000)]
    fn key_codepoint_is_none_for_modified_keys(tc: TestCase) {
        let k = tc.draw(canonical_key());
        if k.modifiers.has(Modifiers::Control | Modifiers::Alt) {
            assert_eq!(k.codepoint(), None);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn coord_roundtrips_through_resize_key(tc: TestCase) {
        let line = tc.draw(gs::integers::<i32>().min_value(i16::MIN as i32).max_value(i16::MAX as i32));
        let column = tc.draw(gs::integers::<i32>().min_value(0).max_value(0xFFFF));
        let coord = DisplayCoord::new(line, column);
        assert_eq!(resize(coord).coord(), coord);
    }

    #[hegel::test(test_cases = 1000)]
    fn mouse_button_roundtrips_through_modifiers(tc: TestCase) {
        let b = tc.draw(gs::sampled_from(vec![MouseButton::Left, MouseButton::Middle, MouseButton::Right]));
        let k = Key::with_mod(Key::to_modifier(b) | Modifiers::MousePress, 0);
        assert_eq!(k.mouse_button(), b);
    }

    #[hegel::test(test_cases = 1000)]
    fn mouse_button_name_roundtrip(tc: TestCase) {
        let b = tc.draw(gs::sampled_from(vec![MouseButton::Left, MouseButton::Middle, MouseButton::Right]));
        assert_eq!(str_to_button(button_to_str(b).as_bytes()).unwrap(), b);
        let other = tc.draw(gs::binary().filter(|s| !matches!(&s[..], b"left" | b"middle" | b"right")));
        assert!(str_to_button(&other).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn key_ordering_is_val_ordering(tc: TestCase) {
        let a = tc.draw(canonical_key());
        let b = tc.draw(canonical_key());
        assert_eq!(a.cmp(&b), a.val().cmp(&b.val()));
        assert_eq!(a == b, a.val() == b.val());
    }
}
