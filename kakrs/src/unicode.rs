//! Port of unicode.hh

use crate::units::{Codepoint, ColumnCount};

#[allow(non_camel_case_types)]
type wchar_t = libc::wchar_t;
#[allow(non_camel_case_types)]
type wint_t = i32;

unsafe extern "C" {
    fn wcwidth(c: wchar_t) -> libc::c_int;
    fn iswalnum(c: wint_t) -> libc::c_int;
    fn iswlower(c: wint_t) -> libc::c_int;
    fn iswupper(c: wint_t) -> libc::c_int;
    fn towlower(c: wint_t) -> wint_t;
    fn towupper(c: wint_t) -> wint_t;
}

#[inline]
pub fn is_eol(c: Codepoint) -> bool {
    c == '\n' as u32
}

#[inline]
pub fn is_horizontal_blank(c: Codepoint) -> bool {
    // Characters considered whitespace by ECMA Regex Spec minus vertical tab
    matches!(
        c,
        0x09 | 0x0C | 0x20 | 0xA0 | 0xFEFF | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000
    )
}

#[inline]
pub fn is_blank(c: Codepoint) -> bool {
    // Characters considered Line Terminators by ECMA Regex Spec plus vertical tab
    c == '\n' as u32 || c == '\r' as u32 || c == 0x0B || c == 0x2028 || c == 0x2029 || is_horizontal_blank(c)
}

#[inline]
pub fn is_basic_alpha(c: Codepoint) -> bool {
    (c >= 'a' as u32 && c <= 'z' as u32) || (c >= 'A' as u32 && c <= 'Z' as u32)
}

#[inline]
pub fn is_basic_digit(c: Codepoint) -> bool {
    c >= '0' as u32 && c <= '9' as u32
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WordType {
    Word,
    #[allow(clippy::upper_case_acronyms)]
    WORD,
}

pub const DEFAULT_EXTRA_WORD_CHARS: &[Codepoint] = &['_' as u32];

#[inline]
pub fn is_alnum(c: Codepoint) -> bool {
    if c < 128 {
        is_basic_alpha(c) || is_basic_digit(c)
    } else {
        unsafe { iswalnum(c as wint_t) != 0 }
    }
}

#[inline]
pub fn is_word(c: Codepoint, extra_word_chars: &[Codepoint]) -> bool {
    if is_alnum(c) {
        return true;
    }
    extra_word_chars.contains(&c)
}

#[inline]
pub fn is_word_type(word_type: WordType, c: Codepoint, extra_word_chars: &[Codepoint]) -> bool {
    match word_type {
        WordType::Word => is_word(c, extra_word_chars),
        WordType::WORD => !is_blank(c),
    }
}

#[inline]
pub fn is_word_default(c: Codepoint) -> bool {
    is_word(c, DEFAULT_EXTRA_WORD_CHARS)
}

#[inline]
pub fn is_punctuation(c: Codepoint, extra_word_chars: &[Codepoint]) -> bool {
    !(is_word(c, extra_word_chars) || is_blank(c))
}

#[inline]
pub fn is_identifier(c: Codepoint) -> bool {
    is_basic_alpha(c) || is_basic_digit(c) || c == '_' as u32 || c == '-' as u32
}

#[inline]
pub fn codepoint_width(c: Codepoint) -> ColumnCount {
    if c < 0x80 {
        return 1;
    }
    let width = unsafe { wcwidth(c as wchar_t) };
    if width >= 0 { width } else { 1 }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CharCategories {
    Blank,
    EndOfLine,
    Word,
    Punctuation,
}

#[inline]
pub fn categorize(word_type: WordType, c: Codepoint, extra_word_chars: &[Codepoint]) -> CharCategories {
    if is_eol(c) {
        return CharCategories::EndOfLine;
    }
    if is_horizontal_blank(c) {
        return CharCategories::Blank;
    }
    if word_type == WordType::WORD || is_word(c, extra_word_chars) {
        return CharCategories::Word;
    }
    CharCategories::Punctuation
}

#[inline]
pub fn to_lower_byte(c: u8) -> u8 {
    c.to_ascii_lowercase()
}

#[inline]
pub fn to_upper_byte(c: u8) -> u8 {
    c.to_ascii_uppercase()
}

#[inline]
pub fn to_lower(cp: Codepoint) -> Codepoint {
    if cp < 128 {
        (cp as u8).to_ascii_lowercase() as Codepoint
    } else {
        unsafe { towlower(cp as wint_t) as Codepoint }
    }
}

#[inline]
pub fn to_upper(cp: Codepoint) -> Codepoint {
    if cp < 128 {
        (cp as u8).to_ascii_uppercase() as Codepoint
    } else {
        unsafe { towupper(cp as wint_t) as Codepoint }
    }
}

#[inline]
pub fn is_lower(cp: Codepoint) -> bool {
    if cp < 128 {
        (cp as u8).is_ascii_lowercase()
    } else {
        unsafe { iswlower(cp as wint_t) != 0 }
    }
}

#[inline]
pub fn is_upper(cp: Codepoint) -> bool {
    if cp < 128 {
        (cp as u8).is_ascii_uppercase()
    } else {
        unsafe { iswupper(cp as wint_t) != 0 }
    }
}
