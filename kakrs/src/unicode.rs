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

/// [FAITHFUL] '\r' and '\v' are blanks (is_blank) but neither end of lines
/// nor horizontal blanks, so they categorize as Punctuation (or Word for WORD)
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

/// Kakoune calls setlocale(LC_ALL, "") at startup, wcwidth/iswalnum/towlower
/// depend on it. Tests relying on non-ASCII classification call this first.
#[cfg(test)]
pub(crate) fn init_test_locale() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        if libc::setlocale(libc::LC_ALL, c"en_US.UTF-8".as_ptr()).is_null() {
            libc::setlocale(libc::LC_ALL, c"C.UTF-8".as_ptr());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    /// Any u32: valid scalar values, surrogates, out of range values and
    /// values with the sign bit set (as produced by decoding invalid utf8).
    fn any_codepoint() -> impl hegel::PrintableGenerator<Codepoint> {
        hegel::one_of!(
            gs::integers::<u32>().max_value(0x7F),
            gs::integers::<u32>().max_value(0x10FFFF),
            gs::integers::<u32>(),
        )
    }

    // ECMA WhiteSpace + LineTerminator (plus vertical tab) is Unicode's
    // White_Space property, minus U+0085 (NEL), plus U+FEFF (ZWNBSP).
    #[hegel::test(test_cases = 1000)]
    fn is_blank_is_ecma_whitespace(tc: TestCase) {
        let c = tc.draw(any_codepoint());
        let expected = c == 0xFEFF || (c != 0x85 && char::from_u32(c).is_some_and(char::is_whitespace));
        assert_eq!(is_blank(c), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn horizontal_blank_is_blank_minus_vertical(tc: TestCase) {
        let c = tc.draw(any_codepoint());
        let vertical = c == '\n' as u32 || c == '\r' as u32 || c == 0x0B;
        assert_eq!(is_horizontal_blank(c), is_blank(c) && !vertical);
    }

    #[hegel::test(test_cases = 1000)]
    fn ascii_classification_matches_std(tc: TestCase) {
        let c = tc.draw(gs::integers::<u8>().max_value(0x7F));
        let cp = c as Codepoint;
        assert_eq!(is_basic_alpha(cp), c.is_ascii_alphabetic());
        assert_eq!(is_basic_digit(cp), c.is_ascii_digit());
        assert_eq!(is_alnum(cp), c.is_ascii_alphanumeric());
        assert_eq!(is_lower(cp), c.is_ascii_lowercase());
        assert_eq!(is_upper(cp), c.is_ascii_uppercase());
        assert_eq!(is_identifier(cp), c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
    }

    #[hegel::test(test_cases = 1000)]
    fn ascii_case_mapping_matches_std(tc: TestCase) {
        let c = tc.draw(gs::integers::<u8>().max_value(0x7F));
        assert_eq!(to_lower(c as Codepoint), c.to_ascii_lowercase() as Codepoint);
        assert_eq!(to_upper(c as Codepoint), c.to_ascii_uppercase() as Codepoint);
        assert_eq!(to_lower_byte(c) as Codepoint, to_lower(c as Codepoint));
        assert_eq!(to_upper_byte(c) as Codepoint, to_upper(c as Codepoint));
    }

    // The byte versions only touch ASCII letters, whatever the byte.
    #[hegel::test(test_cases = 1000)]
    fn byte_case_mapping_only_touches_ascii_letters(tc: TestCase) {
        let c = tc.draw(gs::integers::<u8>());
        let lower = to_lower_byte(c);
        if c.is_ascii_uppercase() {
            assert_eq!(lower, c + 32);
        } else {
            assert_eq!(lower, c);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn codepoint_width_is_0_1_or_2(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(any_codepoint());
        let w = codepoint_width(c);
        assert!((0..=2).contains(&w), "width of {c:#x} is {w}");
        if c < 0x80 {
            assert_eq!(w, 1);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn cjk_ideographs_are_wide(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(gs::integers::<u32>().min_value(0x4E00).max_value(0x9FFF));
        assert_eq!(codepoint_width(c), 2);
    }

    #[hegel::test(test_cases = 1000)]
    fn combining_diacritics_are_zero_width(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(gs::integers::<u32>().min_value(0x300).max_value(0x36F));
        assert_eq!(codepoint_width(c), 0);
    }

    #[hegel::test(test_cases = 1000)]
    fn extra_word_chars_are_words(tc: TestCase) {
        init_test_locale();
        let extra = tc.draw(gs::vecs(any_codepoint()).max_size(5));
        let c = if !extra.is_empty() && tc.draw(gs::booleans()) {
            tc.draw(gs::sampled_from(extra.clone()))
        } else {
            tc.draw(any_codepoint())
        };
        assert_eq!(is_word(c, &extra), is_word(c, &[]) || extra.contains(&c));
        assert_eq!(is_word(c, &[]), is_alnum(c));
    }

    #[hegel::test(test_cases = 1000)]
    fn word_type_word_is_not_blank(tc: TestCase) {
        let c = tc.draw(any_codepoint());
        assert_eq!(is_word_type(WordType::WORD, c, &[]), !is_blank(c));
    }

    #[hegel::test(test_cases = 1000)]
    fn categorize_end_of_line_and_blank(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(any_codepoint());
        let word_type = tc.draw(gs::sampled_from(vec![WordType::Word, WordType::WORD]));
        let cat = categorize(word_type, c, DEFAULT_EXTRA_WORD_CHARS);
        assert_eq!(cat == CharCategories::EndOfLine, is_eol(c));
        assert_eq!(cat == CharCategories::Blank, is_horizontal_blank(c));
    }

    #[hegel::test(test_cases = 1000)]
    fn categorize_word_agrees_with_is_word(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(any_codepoint());
        let cat = categorize(WordType::Word, c, DEFAULT_EXTRA_WORD_CHARS);
        if !is_eol(c) && !is_horizontal_blank(c) {
            assert_eq!(cat == CharCategories::Word, is_word_default(c));
        }
        // [FAITHFUL] '\r' and '\v' are blanks for is_punctuation but neither
        // eol nor horizontal blanks for categorize, so they categorize as
        // Punctuation.
        if cat == CharCategories::Punctuation {
            assert!(is_punctuation(c, DEFAULT_EXTRA_WORD_CHARS) || c == '\r' as u32 || c == 0x0B);
        }
        assert_ne!(categorize(WordType::WORD, c, DEFAULT_EXTRA_WORD_CHARS), CharCategories::Punctuation);
    }
}
