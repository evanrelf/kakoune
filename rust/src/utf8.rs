//! UTF-8 decoding over byte positions, mirroring `src/utf8.hh` with its
//! default `InvalidPolicy::Pass`. Invalid UTF-8 decodes to the same values as
//! in the C++, so code ported from it ranks and compares identically.

use crate::unicode::Codepoint;
use std::ffi::c_char;

/// What the C++ returns when reading past the end: `-1` as a `Codepoint`.
pub const END: Codepoint = Codepoint::MAX;

/// Converts a byte the way C++ converts a `char` to a `Codepoint`, which
/// sign-extends wherever `char` is signed.
pub fn char_to_codepoint(byte: u8) -> Codepoint {
    byte as c_char as Codepoint
}

pub fn is_character_start(byte: u8) -> bool {
    byte & 0xC0 != 0x80
}

/// Decodes the character starting at `*pos` and moves `*pos` past it.
/// Continuation bytes are consumed according to the lead byte without being
/// checked.
pub fn read_codepoint(s: &[u8], pos: &mut usize) -> Codepoint {
    let Some(&byte) = s.get(*pos) else {
        return END;
    };
    *pos += 1;
    if byte & 0x80 == 0 {
        return byte.into();
    }

    if *pos == s.len() {
        return char_to_codepoint(byte);
    }

    // Reads a continuation byte's payload. A truncated sequence returns the
    // bits read so far.
    let mut next = || {
        let b = *s.get(*pos)?;
        *pos += 1;
        Some(Codepoint::from(b & 0x3F))
    };
    let lead = Codepoint::from(byte);
    if byte & 0xE0 == 0xC0 {
        (lead & 0x1F) << 6 | next().unwrap()
    } else if byte & 0xF0 == 0xE0 {
        let cp = (lead & 0x0F) << 12 | next().unwrap() << 6;
        next().map_or(cp, |b| cp | b)
    } else if byte & 0xF8 == 0xF0 {
        let cp = (lead & 0x0F) << 18 | next().unwrap() << 12;
        let Some(b) = next() else { return cp };
        let cp = cp | b << 6;
        next().map_or(cp, |b| cp | b)
    } else {
        char_to_codepoint(byte)
    }
}

/// Decodes the character starting at `pos`.
pub fn codepoint(s: &[u8], mut pos: usize) -> Codepoint {
    read_codepoint(s, &mut pos)
}

/// The start of the character after the one at `pos`.
pub fn next(s: &[u8], mut pos: usize) -> usize {
    if pos < s.len() {
        pos += 1;
    }
    while pos < s.len() && !is_character_start(s[pos]) {
        pos += 1;
    }
    pos
}

/// The start of the character containing `pos`, stopping at `begin`.
pub fn character_start(s: &[u8], mut pos: usize, begin: usize) -> usize {
    while pos != begin && !is_character_start(s[pos]) {
        pos -= 1;
    }
    pos
}

/// The character ending just before `pos`, decoded without reading past
/// `pos`.
pub fn prev_codepoint(s: &[u8], pos: usize) -> Codepoint {
    if pos == 0 {
        return END;
    }
    codepoint(&s[..pos], character_start(s, pos - 1, 0))
}

/// Iterates over the characters of `s` the way `utf8::iterator` does:
/// decoding from each character start, then skipping to the next one.
pub fn codepoints(s: &[u8]) -> impl Iterator<Item = Codepoint> + Clone + '_ {
    let mut pos = 0;
    std::iter::from_fn(move || {
        (pos < s.len()).then(|| {
            let cp = codepoint(s, pos);
            pos = next(s, pos);
            cp
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_valid_utf8() {
        let s = "aé€😀".as_bytes();
        let cps: Vec<_> = codepoints(s).collect();
        assert_eq!(cps, ['a', 'é', '€', '😀'].map(Codepoint::from));
        let mut pos = 1;
        assert_eq!(read_codepoint(s, &mut pos), 'é'.into());
        assert_eq!(pos, 3);
        assert_eq!(prev_codepoint(s, 3), 'é'.into());
        assert_eq!(prev_codepoint(s, 0), END);
        assert_eq!(character_start(s, 4, 0), 3);
    }

    #[test]
    fn decodes_invalid_utf8_like_cpp() {
        // A truncated sequence keeps the bits read so far
        assert_eq!(codepoint(b"\xE2\x82", 0), 0x2080);
        // A lone lead byte converts like a C++ `char`
        assert_eq!(codepoint(b"\xC3", 0), char_to_codepoint(0xC3));
        // Continuation bytes aren't checked
        let mut pos = 0;
        assert_eq!(read_codepoint(b"\xC3a", &mut pos), 0xE1);
        assert_eq!(pos, 2);
        // ...but iterating moves to the next character start
        let cps: Vec<_> = codepoints(b"\xC3a").collect();
        assert_eq!(cps, [0xE1, 'a'.into()]);
        // An invalid lead byte only consumes itself
        let mut pos = 0;
        assert_eq!(read_codepoint(b"\x80a", &mut pos), char_to_codepoint(0x80));
        assert_eq!(pos, 1);
    }
}
