//! Character classification, mirroring `src/unicode.hh`. ASCII is classified
//! here; anything above it falls back to the same locale-dependent libc
//! functions as the C++, called through C++ so the conversions match.

#[cxx::bridge(namespace = "Kakoune::rs")]
mod ffi {
    unsafe extern "C++" {
        include!("rust_bridge.hh");

        fn locale_is_alnum(c: u32) -> bool;
        fn locale_is_lower(c: u32) -> bool;
        fn locale_is_upper(c: u32) -> bool;
        fn locale_to_lower(c: u32) -> u32;
    }
}

/// A Unicode scalar value, or whatever Kakoune decoded from invalid UTF-8.
pub type Codepoint = u32;

/// The extra word characters `is_word` uses by default in the C++.
pub const DEFAULT_EXTRA_WORD_CHARS: &[Codepoint] = &['_' as Codepoint];

pub fn is_word(c: Codepoint, extra_word_chars: &[Codepoint]) -> bool {
    let alnum = match u8::try_from(c) {
        Ok(b) if b.is_ascii() => b.is_ascii_alphanumeric(),
        _ => ffi::locale_is_alnum(c),
    };
    alnum || extra_word_chars.contains(&c)
}

pub fn is_lower(c: Codepoint) -> bool {
    match u8::try_from(c) {
        Ok(b) if b.is_ascii() => b.is_ascii_lowercase(),
        _ => ffi::locale_is_lower(c),
    }
}

pub fn is_upper(c: Codepoint) -> bool {
    match u8::try_from(c) {
        Ok(b) if b.is_ascii() => b.is_ascii_uppercase(),
        _ => ffi::locale_is_upper(c),
    }
}

pub fn to_lower(c: Codepoint) -> Codepoint {
    match u8::try_from(c) {
        Ok(b) if b.is_ascii() => b.to_ascii_lowercase().into(),
        _ => ffi::locale_to_lower(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_ascii() {
        assert!(is_word('a' as Codepoint, &[]));
        assert!(is_word('7' as Codepoint, &[]));
        assert!(!is_word('_' as Codepoint, &[]));
        assert!(is_word('_' as Codepoint, DEFAULT_EXTRA_WORD_CHARS));
        assert!(is_lower('a' as Codepoint) && !is_lower('A' as Codepoint));
        assert!(is_upper('A' as Codepoint) && !is_upper('a' as Codepoint));
        assert_eq!(to_lower('Q' as Codepoint), 'q' as Codepoint);
        assert_eq!(to_lower('-' as Codepoint), '-' as Codepoint);
    }
}
