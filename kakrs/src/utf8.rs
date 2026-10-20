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
    // C++ passes the lead byte as a (signed) char to the invalid policy, so
    // invalid bytes are sign extended.
    let invalid = byte as i8 as Codepoint;
    let byte = byte as u32;
    if (byte & 0xE0) == 0xC0 {
        // 110xxxxx
        match next() {
            None => invalid,
            Some(b) => ((byte & 0x1F) << 6) | (b as u32 & 0x3F),
        }
    } else if (byte & 0xF0) == 0xE0 {
        // 1110xxxx
        let Some(b1) = next() else { return invalid };
        let cp = ((byte & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6);
        match next() {
            None => cp,
            Some(b2) => cp | (b2 as u32 & 0x3F),
        }
    } else if (byte & 0xF8) == 0xF0 {
        // 11110xxx
        let Some(b1) = next() else { return invalid };
        let mut cp = ((byte & 0x0F) << 18) | ((b1 as u32 & 0x3F) << 12);
        let Some(b2) = next() else { return cp };
        cp |= (b2 as u32 & 0x3F) << 6;
        match next() {
            None => cp,
            Some(b3) => cp | (b3 as u32 & 0x3F),
        }
    } else {
        invalid
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

/// Iterate over the codepoints of a byte slice, like a C++ utf8::iterator
/// range: decode at each position, then move to the next character start.
/// On invalid utf8 this differs from repeated read_codepoint: stray
/// continuation bytes are skipped ("\x80\x80" yields one codepoint), and a
/// truncated sequence does not swallow the following character ("\xC3a"
/// yields two).
pub fn codepoints(s: &[u8]) -> impl Iterator<Item = Codepoint> + '_ {
    char_indices(s).map(|(_, cp)| cp)
}

/// Iterate over (byte_position, codepoint) pairs of a byte slice, with
/// utf8::iterator semantics (see codepoints)
pub fn char_indices(s: &[u8]) -> impl Iterator<Item = (usize, Codepoint)> + '_ {
    let mut pos = 0;
    std::iter::from_fn(move || {
        if pos < s.len() {
            let start = pos;
            pos = next(s, pos);
            Some((start, codepoint_at(s, start)))
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

    use crate::unicode::init_test_locale;
    use hegel::{Generator as _, TestCase};
    use hegel::generators as gs;

    /// Byte strings mixing ascii, valid multibyte sequences and stray
    /// lead/continuation bytes.
    fn any_bytes() -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(
            gs::binary(),
            gs::text().map(String::into_bytes),
            gs::vecs(gs::sampled_from(vec![b'a', 0x80, 0xBF, 0xC3, 0xA9, 0xE2, 0x82, 0xAC, 0xF0, 0x9F, 0xF8, 0xFF])),
        )
    }

    fn valid_text() -> impl hegel::PrintableGenerator<String> {
        hegel::one_of!(
            gs::text(),
            gs::vecs(gs::sampled_from(vec!['a', 'é', '€', '😀', '\u{300}', '中', '\0', '\t'])).map(|v| v.into_iter().collect()),
        )
    }

    fn boundaries(s: &str) -> Vec<usize> {
        s.char_indices().map(|(i, _)| i).chain(std::iter::once(s.len())).collect()
    }

    /// C++ decodes invalid bytes through a (signed) char, sign extending them
    fn sign_extend(b: u8) -> Codepoint {
        b as i8 as i32 as u32
    }

    #[hegel::test(test_cases = 1000)]
    fn encode_decode_roundtrip(tc: TestCase) {
        let cp = tc.draw(gs::integers::<u32>().max_value(0x10FFFF));
        assert_eq!(codepoint(&encode(cp)), cp);
    }

    #[hegel::test(test_cases = 1000)]
    fn encoded_size_agrees(tc: TestCase) {
        let cp = tc.draw(gs::integers::<u32>().max_value(0x10FFFF));
        let e = encode(cp);
        assert_eq!(e.len(), codepoint_size(cp));
        assert_eq!(codepoint_size_of_byte(e[0]), e.len());
        assert_eq!(distance(&e), 1);
    }

    #[hegel::test(test_cases = 1000)]
    fn encode_matches_std(tc: TestCase) {
        let c = tc.draw(gs::characters());
        let mut buf = [0u8; 4];
        assert_eq!(encode(c as u32), c.encode_utf8(&mut buf).as_bytes());
    }

    #[hegel::test(test_cases = 1000)]
    fn encode_out_of_range_is_empty(tc: TestCase) {
        let cp = tc.draw(gs::integers::<u32>().min_value(0x110000));
        assert!(encode(cp).is_empty());
        assert_eq!(codepoint_size(cp), 0);
    }

    #[hegel::test(test_cases = 1000)]
    fn codepoints_of_valid_text_match_std(tc: TestCase) {
        let s = tc.draw(valid_text());
        let expected: Vec<(usize, u32)> = s.char_indices().map(|(i, c)| (i, c as u32)).collect();
        assert_eq!(char_indices(s.as_bytes()).collect::<Vec<_>>(), expected);
    }

    // codepoints() is a utf8::iterator range: one element per step of next()
    // from the beginning, decoded at that position
    #[hegel::test(test_cases = 1000)]
    fn codepoints_follow_iterator_steps(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let mut expected = Vec::new();
        let mut it = SliceIt::new(&s, 0);
        let end = SliceIt::new(&s, s.len());
        while it != end {
            expected.push((it.pos, it_codepoint(&it, &end)));
            it_to_next(&mut it, &end);
        }
        assert_eq!(char_indices(&s).collect::<Vec<_>>(), expected);
        assert_eq!(codepoints(&s).collect::<Vec<_>>(), expected.iter().map(|(_, c)| *c).collect::<Vec<_>>());
    }

    #[test]
    fn codepoints_on_invalid_utf8() {
        assert_eq!(codepoints(b"\x80\x80").collect::<Vec<_>>(), vec![0xFFFFFF80]);
        assert_eq!(codepoints(b"\xC3a").collect::<Vec<_>>(), vec![0xE1, 'a' as u32]);
    }

    #[hegel::test(test_cases = 1000)]
    fn distance_of_valid_text_is_char_count(tc: TestCase) {
        let s = tc.draw(valid_text());
        assert_eq!(distance(s.as_bytes()), s.chars().count() as CharCount);
    }

    // Stray bytes decode like the C++ code does, through a signed char
    #[hegel::test(test_cases = 1000)]
    fn invalid_lead_byte_decodes_sign_extended(tc: TestCase) {
        let b = tc.draw(hegel::one_of!(
            gs::integers::<u8>().min_value(0x80).max_value(0xBF),
            gs::integers::<u8>().min_value(0xF8)
        ));
        let rest = tc.draw(gs::binary().max_size(4));
        let mut s = vec![b];
        s.extend(&rest);
        assert_eq!(codepoint(&s), sign_extend(b));
    }

    #[hegel::test(test_cases = 1000)]
    fn truncated_lead_byte_at_end_decodes_sign_extended(tc: TestCase) {
        let b = tc.draw(gs::integers::<u8>().min_value(0x80));
        assert_eq!(codepoint(&[b]), sign_extend(b));
    }

    #[hegel::test(test_cases = 1000)]
    fn read_codepoint_advances_by_sequence_length(tc: TestCase) {
        let s = tc.draw(any_bytes());
        tc.assume(!s.is_empty());
        let mut pos = 0;
        read_codepoint(&s, &mut pos);
        let expected = codepoint_size_of_byte(s[0]).min(s.len());
        // an invalid lead byte not at the end consumes only itself
        assert_eq!(pos, expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn next_moves_to_next_character_start(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        let n = next(&s, pos);
        if pos == s.len() {
            assert_eq!(n, pos);
        } else {
            assert!(n > pos);
            assert!(n == s.len() || is_character_start(s[n]));
            assert!(s[pos + 1..n].iter().all(|&c| !is_character_start(c)));
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn previous_moves_to_previous_character_start(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        let p = previous(&s, pos, 0);
        if pos == 0 {
            assert_eq!(p, 0);
        } else {
            assert!(p < pos);
            assert!(p == 0 || is_character_start(s[p]));
            assert!(s[p + 1..pos].iter().all(|&c| !is_character_start(c)));
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn previous_undoes_next_on_valid_text(tc: TestCase) {
        let s = tc.draw(valid_text());
        let b = boundaries(&s);
        let i = tc.draw(gs::integers::<usize>().max_value(b.len() - 1));
        if b[i] != s.len() {
            assert_eq!(next(s.as_bytes(), b[i]), b[i + 1]);
            assert_eq!(previous(s.as_bytes(), b[i + 1], 0), b[i]);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn finish_skips_continuation_bytes(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        let f = finish(&s, pos);
        let expected = (pos..s.len()).find(|&i| is_character_start(s[i])).unwrap_or(s.len());
        assert_eq!(f, expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn character_start_is_last_start_at_or_before(tc: TestCase) {
        let s = tc.draw(any_bytes());
        tc.assume(!s.is_empty());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len() - 1));
        let expected = (0..=pos).rev().find(|&i| is_character_start(s[i])).unwrap_or(0);
        assert_eq!(character_start(&s, pos, 0), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn distance_counts_next_steps(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let mut pos = finish(&s, 0);
        let mut steps = 0;
        while pos != s.len() {
            pos = next(&s, pos);
            steps += 1;
        }
        assert_eq!(distance(&s), steps);
    }

    #[hegel::test(test_cases = 1000)]
    fn advance_moves_by_characters_on_valid_text(tc: TestCase) {
        let s = tc.draw(valid_text());
        let b = boundaries(&s);
        let count = b.len() as i64 - 1;
        let i = tc.draw(gs::integers::<usize>().max_value(b.len() - 1));
        let d = tc.draw(gs::integers::<i32>().min_value(-(count as i32) - 2).max_value(count as i32 + 2));
        let res = advance(s.as_bytes(), b[i], d);
        if b[i] == s.len() {
            // C++ quirk: advancing from the end returns the end, even backward
            assert_eq!(res, s.len());
        } else {
            let target = (i as i64 + d as i64).clamp(0, count);
            assert_eq!(res, b[target as usize]);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn prev_codepoint_on_valid_text(tc: TestCase) {
        let s = tc.draw(valid_text());
        let b = boundaries(&s);
        let i = tc.draw(gs::integers::<usize>().max_value(b.len() - 1));
        let expected = if i == 0 { u32::MAX } else { s[b[i - 1]..b[i]].chars().next().unwrap() as u32 };
        assert_eq!(prev_codepoint(s.as_bytes(), b[i], 0), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn column_distance_is_sum_of_widths(tc: TestCase) {
        init_test_locale();
        let s = tc.draw(any_bytes());
        // C++ column_distance steps with read_codepoint
        let mut pos = 0;
        let mut expected = 0;
        while pos < s.len() {
            expected += codepoint_width(read_codepoint(&s, &mut pos));
        }
        assert_eq!(column_distance(&s), expected);
    }

    /// Display column of each character boundary of valid text
    fn columns(s: &str) -> Vec<ColumnCount> {
        let mut res = vec![0];
        for c in s.chars() {
            res.push(res.last().unwrap() + codepoint_width(c as u32));
        }
        res
    }

    // Moving forward by d columns stops at the first boundary reaching the
    // target column, or at the start of the character spanning it (unless
    // that character is the last one, where it stops at the end).
    #[hegel::test(test_cases = 1000)]
    fn advance_columns_forward(tc: TestCase) {
        init_test_locale();
        let s = tc.draw(valid_text());
        let (b, col) = (boundaries(&s), columns(&s));
        let i = tc.draw(gs::integers::<usize>().max_value(b.len() - 1));
        let d = tc.draw(gs::integers::<i32>().min_value(1).max_value(col.last().unwrap() + 3));
        let res = advance_columns(s.as_bytes(), b[i], d);
        let expected = if b[i] == s.len() {
            s.len()
        } else {
            let target = col[i] + d;
            match (i + 1..b.len()).find(|&k| col[k] >= target) {
                None => s.len(),
                Some(k) if col[k] == target || k == b.len() - 1 => b[k],
                Some(k) => b[k - 1],
            }
        };
        assert_eq!(res, expected);
    }

    // Moving backward by d columns stops at the last boundary at or before the
    // target column (or the beginning).
    #[hegel::test(test_cases = 1000)]
    fn advance_columns_backward(tc: TestCase) {
        init_test_locale();
        let s = tc.draw(valid_text());
        let (b, col) = (boundaries(&s), columns(&s));
        let i = tc.draw(gs::integers::<usize>().max_value(b.len() - 1));
        let d = tc.draw(gs::integers::<i32>().min_value(-col.last().unwrap() - 3).max_value(-1));
        let res = advance_columns(s.as_bytes(), b[i], d);
        let expected = if b[i] == s.len() {
            s.len()
        } else {
            let target = col[i] + d;
            (0..i).rev().find(|&k| col[k] <= target).map(|k| b[k]).unwrap_or(0)
        };
        assert_eq!(res, expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn advance_by_zero_is_identity(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        assert_eq!(advance(&s, pos, 0), pos);
        assert_eq!(advance_columns(&s, pos, 0), pos);
    }

    // The generic iterator functions must agree with the slice ones
    fn it(s: &[u8], pos: usize) -> SliceIt<'_> {
        SliceIt::new(s, pos)
    }

    #[hegel::test(test_cases = 1000)]
    fn it_read_codepoint_agrees(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        let (mut p1, mut p2) = (pos, it(&s, pos));
        assert_eq!(it_read_codepoint(&mut p2, &it(&s, s.len())), read_codepoint(&s, &mut p1));
        assert_eq!(p2.pos, p1);
    }

    #[hegel::test(test_cases = 1000)]
    fn it_next_agrees(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        assert_eq!(it_next(&it(&s, pos), &it(&s, s.len())).pos, next(&s, pos));
    }

    #[hegel::test(test_cases = 1000)]
    fn it_previous_agrees(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        assert_eq!(it_previous(&it(&s, pos), &it(&s, 0)).pos, previous(&s, pos, 0));
    }

    #[hegel::test(test_cases = 1000)]
    fn it_finish_agrees(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        assert_eq!(it_finish(&it(&s, pos), &it(&s, s.len())).pos, finish(&s, pos));
    }

    #[hegel::test(test_cases = 1000)]
    fn it_character_start_agrees(tc: TestCase) {
        let s = tc.draw(any_bytes());
        tc.assume(!s.is_empty());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len() - 1));
        assert_eq!(it_character_start(&it(&s, pos), &it(&s, 0)).pos, character_start(&s, pos, 0));
    }

    #[hegel::test(test_cases = 1000)]
    fn it_prev_codepoint_agrees(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        assert_eq!(it_prev_codepoint(&it(&s, pos), &it(&s, 0)), prev_codepoint(&s, pos, 0));
    }

    #[hegel::test(test_cases = 1000)]
    fn it_distances_agree(tc: TestCase) {
        init_test_locale();
        let s = tc.draw(any_bytes());
        let (begin, end) = (it(&s, 0), it(&s, s.len()));
        assert_eq!(it_distance(&begin, &end), distance(&s));
        assert_eq!(it_column_distance(&begin, &end), column_distance(&s));
    }

    #[hegel::test(test_cases = 1000)]
    fn utf8_iter_advance_is_repeated_steps(tc: TestCase) {
        let s = tc.draw(any_bytes());
        let pos = tc.draw(gs::integers::<usize>().max_value(s.len()));
        let n = tc.draw(gs::integers::<i32>().min_value(-20).max_value(20));
        let start = Utf8Iter::new(it(&s, pos), it(&s, 0), it(&s, s.len()));
        let mut stepped = start.clone();
        for _ in 0..n.abs() {
            if n < 0 { stepped.dec() } else { stepped.inc() }
        }
        assert_eq!(start.add(n).it.pos, stepped.it.pos);
        assert_eq!(start.sub(-n).it.pos, stepped.it.pos);
    }

    #[hegel::test(test_cases = 1000)]
    fn utf8_iter_distance_from_counts_steps_on_valid_text(tc: TestCase) {
        let s = tc.draw(valid_text());
        let b = boundaries(&s);
        let i = tc.draw(gs::integers::<usize>().max_value(b.len() - 1));
        let j = tc.draw(gs::integers::<usize>().min_value(i).max_value(b.len() - 1));
        let bytes = s.as_bytes();
        let mk = |p| Utf8Iter::new(it(bytes, p), it(bytes, 0), it(bytes, bytes.len()));
        assert_eq!(mk(b[j]).distance_from(&mk(b[i])), (j - i) as CharCount);
        assert_eq!(mk(b[i]).add((j - i) as CharCount).it.pos, b[j]);
        assert_eq!(mk(b[i]).get(), codepoint_at(bytes, b[i]));
    }
}
