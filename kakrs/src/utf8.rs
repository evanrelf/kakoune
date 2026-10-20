//! Port of utf8.hh and utf8_iterator.hh
//!
//! Two flavours are provided: functions operating on byte slices with
//! `usize` positions, and generic functions operating on any `ByteIt`
//! (such as buffer iterators).

use crate::unicode::codepoint_width;
use crate::units::{CharCount, Codepoint, ColumnCount};

/// A bidirectional iterator over bytes, the equivalent of the C++ iterator
/// concept used by the utf8 templates.
pub trait ByteIt: Clone + PartialEq + PartialOrd {
    fn get(&self) -> u8;
    fn inc(&mut self);
    fn dec(&mut self);
}

#[inline]
pub fn is_character_start(c: u8) -> bool {
    (c & 0xC0) != 0x80
}

#[inline]
fn read_multibyte<F: FnMut() -> Option<u8>>(byte: u8, mut next: F) -> Codepoint {
    let byte = byte as u32;
    if (byte & 0xE0) == 0xC0 {
        // 110xxxxx
        match next() {
            None => byte,
            Some(b) => ((byte & 0x1F) << 6) | (b as u32 & 0x3F),
        }
    } else if (byte & 0xF0) == 0xE0 {
        // 1110xxxx
        let Some(b1) = next() else { return byte };
        let cp = ((byte & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6);
        match next() {
            None => cp,
            Some(b2) => cp | (b2 as u32 & 0x3F),
        }
    } else if (byte & 0xF8) == 0xF0 {
        // 11110xxx
        let Some(b1) = next() else { return byte };
        let mut cp = ((byte & 0x0F) << 18) | ((b1 as u32 & 0x3F) << 12);
        let Some(b2) = next() else { return cp };
        cp |= (b2 as u32 & 0x3F) << 6;
        match next() {
            None => cp,
            Some(b3) => cp | (b3 as u32 & 0x3F),
        }
    } else {
        byte
    }
}

// ---------------------------------------------------------------------------
// Slice based functions

/// Read codepoint at pos and advance pos past it. Returns u32::MAX (-1) at end.
#[inline]
pub fn read_codepoint(s: &[u8], pos: &mut usize) -> Codepoint {
    if *pos >= s.len() {
        return u32::MAX;
    }
    let byte = s[*pos];
    *pos += 1;
    if byte & 0x80 == 0 {
        return byte as u32;
    }
    read_multibyte(byte, || {
        if *pos < s.len() {
            let b = s[*pos];
            *pos += 1;
            Some(b)
        } else {
            None
        }
    })
}

#[inline]
pub fn codepoint_at(s: &[u8], pos: usize) -> Codepoint {
    let mut p = pos;
    read_codepoint(s, &mut p)
}

/// Codepoint of the first character of s
#[inline]
pub fn codepoint(s: &[u8]) -> Codepoint {
    codepoint_at(s, 0)
}

pub fn codepoint_size_of_byte(byte: u8) -> usize {
    if byte & 0x80 == 0 {
        1
    } else if byte & 0xE0 == 0xC0 {
        2
    } else if byte & 0xF0 == 0xE0 {
        3
    } else if byte & 0xF8 == 0xF0 {
        4
    } else {
        1
    }
}

pub fn codepoint_size(cp: Codepoint) -> usize {
    if cp <= 0x7F {
        1
    } else if cp <= 0x7FF {
        2
    } else if cp <= 0xFFFF {
        3
    } else if cp <= 0x10FFFF {
        4
    } else {
        0
    }
}

#[inline]
pub fn next(s: &[u8], mut pos: usize) -> usize {
    if pos < s.len() {
        pos += 1;
    }
    while pos < s.len() && !is_character_start(s[pos]) {
        pos += 1;
    }
    pos
}

/// returns pos if it points to a character first byte, or else next character first byte
pub fn finish(s: &[u8], mut pos: usize) -> usize {
    while pos < s.len() && (s[pos] & 0xC0) == 0x80 {
        pos += 1;
    }
    pos
}

/// previous character start, not going before begin
#[inline]
pub fn previous(s: &[u8], mut pos: usize, begin: usize) -> usize {
    if pos != begin {
        pos -= 1;
    }
    while pos != begin && !is_character_start(s[pos]) {
        pos -= 1;
    }
    pos
}

/// advance by d characters (may be negative)
pub fn advance(s: &[u8], mut pos: usize, mut d: CharCount) -> usize {
    if pos == s.len() {
        return pos;
    }
    if d < 0 {
        // Note: the C++ code uses `end` as the begin sentinel here, which
        // means it can go back to the start of the string.
        while d != 0 {
            d += 1;
            pos = previous(s, pos, 0);
            if pos == 0 {
                break;
            }
        }
    } else if d > 0 {
        while pos != s.len() && d != 0 {
            d -= 1;
            pos = next(s, pos);
        }
    }
    pos
}

/// advance by d columns (may be negative)
pub fn advance_columns(s: &[u8], mut pos: usize, mut d: ColumnCount) -> usize {
    if pos == s.len() {
        return pos;
    }
    if d < 0 {
        while d < 0 {
            let cur = pos;
            pos = previous(s, pos, 0);
            d += codepoint_width(codepoint(&s[pos..cur]));
            if pos == 0 {
                break;
            }
        }
    } else if d > 0 {
        let begin = pos;
        while pos != s.len() && d > 0 {
            d -= codepoint_width(read_codepoint(s, &mut pos));
            if pos != s.len() && d < 0 {
                pos = previous(s, pos, begin);
            }
        }
    }
    pos
}

/// character count of s
pub fn distance(s: &[u8]) -> CharCount {
    s.iter().filter(|&&c| is_character_start(c)).count() as CharCount
}

/// column count of s
pub fn column_distance(s: &[u8]) -> ColumnCount {
    let mut pos = 0;
    let mut dist = 0;
    while pos < s.len() {
        dist += codepoint_width(read_codepoint(s, &mut pos));
    }
    dist
}

/// returns the position of the first byte of the character pos is into
pub fn character_start(s: &[u8], mut pos: usize, begin: usize) -> usize {
    while pos != begin && pos < s.len() && !is_character_start(s[pos]) {
        pos -= 1;
    }
    pos
}

/// codepoint before pos, u32::MAX if pos <= begin
pub fn prev_codepoint(s: &[u8], pos: usize, begin: usize) -> Codepoint {
    if pos <= begin {
        return u32::MAX;
    }
    let start = character_start(s, pos - 1, begin);
    codepoint(&s[start..pos])
}

pub fn dump(out: &mut Vec<u8>, cp: Codepoint) {
    if cp <= 0x7F {
        out.push(cp as u8);
    } else if cp <= 0x7FF {
        out.push((0xC0 | (cp >> 6)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    } else if cp <= 0xFFFF {
        out.push((0xE0 | (cp >> 12)) as u8);
        out.push((0x80 | ((cp >> 6) & 0x3F)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    } else if cp <= 0x10FFFF {
        out.push((0xF0 | (cp >> 18)) as u8);
        out.push((0x80 | ((cp >> 12) & 0x3F)) as u8);
        out.push((0x80 | ((cp >> 6) & 0x3F)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    }
}

pub fn encode(cp: Codepoint) -> Vec<u8> {
    let mut v = Vec::with_capacity(4);
    dump(&mut v, cp);
    v
}

/// Iterate over the codepoints of a byte slice
pub fn codepoints(s: &[u8]) -> impl Iterator<Item = Codepoint> + '_ {
    let mut pos = 0;
    std::iter::from_fn(move || {
        if pos < s.len() {
            Some(read_codepoint(s, &mut pos))
        } else {
            None
        }
    })
}

/// Iterate over (byte_position, codepoint) pairs of a byte slice
pub fn char_indices(s: &[u8]) -> impl Iterator<Item = (usize, Codepoint)> + '_ {
    let mut pos = 0;
    std::iter::from_fn(move || {
        if pos < s.len() {
            let start = pos;
            Some((start, read_codepoint(s, &mut pos)))
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Generic iterator based functions

pub fn it_read_codepoint<I: ByteIt>(it: &mut I, end: &I) -> Codepoint {
    if it == end {
        return u32::MAX;
    }
    let byte = it.get();
    it.inc();
    if byte & 0x80 == 0 {
        return byte as u32;
    }
    read_multibyte(byte, || {
        if it != end {
            let b = it.get();
            it.inc();
            Some(b)
        } else {
            None
        }
    })
}

#[inline]
pub fn it_codepoint<I: ByteIt>(it: &I, end: &I) -> Codepoint {
    let mut it = it.clone();
    it_read_codepoint(&mut it, end)
}

#[inline]
pub fn it_to_next<I: ByteIt>(it: &mut I, end: &I) {
    if it != end {
        it.inc();
    }
    while it != end && !is_character_start(it.get()) {
        it.inc();
    }
}

#[inline]
pub fn it_next<I: ByteIt>(it: &I, end: &I) -> I {
    let mut it = it.clone();
    it_to_next(&mut it, end);
    it
}

pub fn it_finish<I: ByteIt>(it: &I, end: &I) -> I {
    let mut it = it.clone();
    while &it != end && (it.get() & 0xC0) == 0x80 {
        it.inc();
    }
    it
}

#[inline]
pub fn it_to_previous<I: ByteIt>(it: &mut I, begin: &I) {
    if it != begin {
        it.dec();
    }
    while it != begin && !is_character_start(it.get()) {
        it.dec();
    }
}

#[inline]
pub fn it_previous<I: ByteIt>(it: &I, begin: &I) -> I {
    let mut it = it.clone();
    it_to_previous(&mut it, begin);
    it
}

pub fn it_character_start<I: ByteIt>(it: &I, begin: &I) -> I {
    let mut it = it.clone();
    while &it != begin && !is_character_start(it.get()) {
        it.dec();
    }
    it
}

pub fn it_prev_codepoint<I: ByteIt>(it: &I, begin: &I) -> Codepoint {
    if it <= begin {
        return u32::MAX;
    }
    let mut prev = it.clone();
    prev.dec();
    let start = it_character_start(&prev, begin);
    it_codepoint(&start, it)
}

/// returns the character count between begin and end
pub fn it_distance<I: ByteIt>(begin: &I, end: &I) -> CharCount {
    let mut it = begin.clone();
    let mut dist = 0;
    while &it != end {
        if is_character_start(it.get()) {
            dist += 1;
        }
        it.inc();
    }
    dist
}

/// returns the column count between begin and end
pub fn it_column_distance<I: ByteIt>(begin: &I, end: &I) -> ColumnCount {
    let mut it = begin.clone();
    let mut dist = 0;
    while &it != end {
        dist += codepoint_width(it_read_codepoint(&mut it, end));
    }
    dist
}

/// Port of utf8::iterator: iterates on codepoints over a byte iterator.
#[derive(Clone, Debug)]
pub struct Utf8Iter<I: ByteIt> {
    pub it: I,
    pub begin: I,
    pub end: I,
}

impl<I: ByteIt> Utf8Iter<I> {
    pub fn new(it: I, begin: I, end: I) -> Self {
        Utf8Iter { it, begin, end }
    }
    #[inline]
    pub fn inc(&mut self) {
        it_to_next(&mut self.it, &self.end);
    }
    #[inline]
    pub fn dec(&mut self) {
        it_to_previous(&mut self.it, &self.begin);
    }
    pub fn add(&self, count: CharCount) -> Self {
        let mut r = self.clone();
        r.advance(count);
        r
    }
    pub fn sub(&self, count: CharCount) -> Self {
        let mut r = self.clone();
        r.advance(-count);
        r
    }
    pub fn advance(&mut self, count: CharCount) {
        if count < 0 {
            for _ in 0..-count {
                self.dec();
            }
        } else {
            for _ in 0..count {
                self.inc();
            }
        }
    }
    #[inline]
    pub fn get(&self) -> Codepoint {
        it_codepoint(&self.it, &self.end)
    }
    #[inline]
    pub fn read(&mut self) -> Codepoint {
        it_read_codepoint(&mut self.it, &self.end)
    }
    #[inline]
    pub fn base(&self) -> &I {
        &self.it
    }
    pub fn at_end(&self) -> bool {
        self.it == self.end
    }
    pub fn at_begin(&self) -> bool {
        self.it == self.begin
    }
    /// character distance from other to self
    pub fn distance_from(&self, other: &Self) -> CharCount {
        it_distance(&other.it, &self.it)
    }
}

impl<I: ByteIt> PartialEq for Utf8Iter<I> {
    fn eq(&self, o: &Self) -> bool {
        self.it == o.it
    }
}

impl<I: ByteIt> PartialOrd for Utf8Iter<I> {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        self.it.partial_cmp(&o.it)
    }
}

/// A ByteIt over a byte slice
#[derive(Clone, Copy, Debug)]
pub struct SliceIt<'a> {
    pub s: &'a [u8],
    pub pos: usize,
}

impl<'a> SliceIt<'a> {
    pub fn new(s: &'a [u8], pos: usize) -> Self {
        SliceIt { s, pos }
    }
}

impl PartialEq for SliceIt<'_> {
    fn eq(&self, o: &Self) -> bool {
        self.pos == o.pos
    }
}

impl PartialOrd for SliceIt<'_> {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        self.pos.partial_cmp(&o.pos)
    }
}

impl ByteIt for SliceIt<'_> {
    #[inline]
    fn get(&self) -> u8 {
        self.s[self.pos]
    }
    #[inline]
    fn inc(&mut self) {
        self.pos += 1;
    }
    #[inline]
    fn dec(&mut self) {
        self.pos -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for cp in [0x41u32, 0xE9, 0x20AC, 0x1F600] {
            let e = encode(cp);
            assert_eq!(codepoint(&e), cp);
            assert_eq!(e.len(), codepoint_size(cp));
        }
        let s = "aé€😀".as_bytes();
        assert_eq!(distance(s), 4);
        assert_eq!(next(s, 1), 3);
        assert_eq!(previous(s, 3, 0), 1);
        assert_eq!(advance(s, 0, 2), 3);
    }
}
