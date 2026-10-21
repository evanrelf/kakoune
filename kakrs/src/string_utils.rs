//! Port of string_utils.hh/cc, string.hh helpers and the split views of ranges.hh

use crate::exception::{Error, Result};
use crate::unicode::{codepoint_width, is_blank, is_eol, is_horizontal_blank};
use crate::units::{Codepoint, ColumnCount};
use crate::utf8;

pub type KString = Vec<u8>;

/// String(cp, count)
pub fn string_of_cp(cp: Codepoint, count: i32) -> Vec<u8> {
    let mut res = Vec::new();
    for _ in 0..count.max(0) {
        utf8::dump(&mut res, cp);
    }
    res
}

/// String(cp, ColumnCount)
pub fn string_of_cp_cols(cp: Codepoint, count: ColumnCount) -> Vec<u8> {
    let cp_count = count / codepoint_width(cp).max(1);
    string_of_cp(cp, cp_count)
}

/// StringView::substr(ColumnCount from, ColumnCount length)
pub fn substr_columns(s: &[u8], from: ColumnCount, length: ColumnCount) -> &[u8] {
    let beg = utf8::advance_columns(s, 0, from);
    let end = if length >= 0 { utf8::advance_columns(s, beg, length) } else { s.len() };
    &s[beg..end.max(beg)]
}

/// StringView::substr(CharCount from, CharCount length)
pub fn substr_chars(s: &[u8], from: i32, length: i32) -> &[u8] {
    let beg = utf8::advance(s, 0, from);
    let end = if length >= 0 { utf8::advance(s, beg, length) } else { s.len() };
    &s[beg..end.max(beg)]
}

/// StringView::substr(ByteCount from, ByteCount length = -1)
pub fn substr(s: &[u8], from: usize, length: isize) -> &[u8] {
    // [DEVIATION] C++ asserts from <= length (out of range access otherwise),
    // clamp instead
    let from = from.min(s.len());
    let max_length = s.len() - from;
    let len = if length >= 0 && (length as usize) < max_length { length as usize } else { max_length };
    &s[from..from + len]
}

pub fn char_length(s: &[u8]) -> i32 {
    utf8::distance(s)
}

pub fn column_length(s: &[u8]) -> ColumnCount {
    utf8::column_distance(s)
}

/// byte_count_to(ColumnCount)
pub fn byte_count_to_column(s: &[u8], count: ColumnCount) -> usize {
    utf8::advance_columns(s, 0, count)
}

/// byte_count_to(CharCount)
pub fn byte_count_to_char(s: &[u8], count: i32) -> usize {
    utf8::advance(s, 0, count)
}

/// the Codepoint at char index pos (StringView::operator[](CharCount))
pub fn char_at(s: &[u8], pos: i32) -> Codepoint {
    utf8::codepoint_at(s, utf8::advance(s, 0, pos))
}

pub fn trim_indent(s: &[u8]) -> Result<Vec<u8>> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut s = s;
    if s[0] == b'\n' {
        s = &s[1..];
    }
    while let Some(&last) = s.last() {
        // [FAITHFUL] C++ converts the (signed) char to a Codepoint, so bytes
        // >= 0x80 (such as the 0xA0 ending "à") are never blanks.
        if is_blank(last as i8 as i32 as Codepoint) {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    // C++ walks a utf8::iterator, whose increment moves to the next
    // character start (skipping any stray continuation bytes)
    let mut it = 0;
    while it < s.len() && is_horizontal_blank(utf8::codepoint_at(s, it)) {
        it = utf8::next(s, it);
    }
    let indent = &s[..it];
    let mut res = Vec::new();
    for line in split_after(s, b'\n') {
        if line == b"\n" {
            res.extend_from_slice(line);
        } else if !line.starts_with(indent) {
            return Err(Error::runtime("inconsistent indentation in the string"));
        } else {
            res.extend_from_slice(&line[indent.len()..]);
        }
    }
    Ok(res)
}

pub fn escape(s: &[u8], characters: &[u8], escape: u8) -> Vec<u8> {
    let mut res = Vec::with_capacity(s.len());
    for &c in s {
        if characters.contains(&c) {
            res.push(escape);
        }
        res.push(c);
    }
    res
}

pub fn unescape(s: &[u8], characters: &[u8], escape: u8) -> Vec<u8> {
    let mut res = Vec::with_capacity(s.len());
    let end = s.len();
    let mut it = 0;
    while it != end {
        let next = s[it..].iter().position(|&c| c == escape).map(|p| p + it).unwrap_or(end);
        if next != end && next + 1 != end && characters.contains(&s[next + 1]) {
            res.extend_from_slice(&s[it..next]);
            res.push(s[next + 1]);
            it = next + 2;
        } else {
            let upto = if next == end { next } else { next + 1 };
            res.extend_from_slice(&s[it..upto]);
            it = upto;
        }
    }
    res
}

pub fn unescape_chars(s: &[u8], character: u8, escape: u8) -> Vec<u8> {
    unescape(s, &[character, escape], escape)
}

pub fn indent(s: &[u8], indent: &[u8]) -> Vec<u8> {
    let mut res = Vec::with_capacity(s.len());
    let mut was_eol = true;
    for &c in s {
        if was_eol {
            res.extend_from_slice(indent);
        }
        res.push(c);
        was_eol = is_eol(c as Codepoint);
    }
    res
}

/// [FAITHFUL] an empty substr loops forever with a non empty s, as in C++
/// (std::search of an empty needle matches at the current position).
pub fn replace(s: &[u8], substr: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut res = Vec::new();
    let mut it = 0;
    while it < s.len() {
        let m = find_subslice(&s[it..], substr).map(|p| p + it).unwrap_or(s.len());
        res.extend_from_slice(&s[it..m]);
        if m == s.len() {
            break;
        }
        res.extend_from_slice(replacement);
        it = m + substr.len();
    }
    res
}

pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// [DEVIATION] `size - column_length` overflows for sizes near i32::MIN: UB
/// in C++, a panic in debug builds here (same in right_pad)
pub fn left_pad(s: &[u8], size: ColumnCount, c: Codepoint) -> Vec<u8> {
    let mut res = string_of_cp_cols(c, (size - column_length(s)).max(0));
    res.extend_from_slice(substr_columns(s, 0, size));
    res
}

pub fn right_pad(s: &[u8], size: ColumnCount, c: Codepoint) -> Vec<u8> {
    let mut res = substr_columns(s, 0, size).to_vec();
    res.extend(string_of_cp_cols(c, (size - column_length(s)).max(0)));
    res
}

/// join(container, char joiner, bool esc_joiner)
///
/// [FAITHFUL] the joiner is only added once the result is non empty, so
/// leading empty elements vanish (join(["", "a"]) == "a").
pub fn join<I, S>(container: I, joiner: u8, esc_joiner: bool) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u8]>,
{
    let to_escape = [joiner, b'\\'];
    let mut res = Vec::new();
    for s in container {
        if !res.is_empty() {
            res.push(joiner);
        }
        if esc_joiner {
            res.extend(escape(s.as_ref(), &to_escape, b'\\'));
        } else {
            res.extend_from_slice(s.as_ref());
        }
    }
    res
}

/// join(container, StringView joiner)
///
/// [FAITHFUL] leading empty elements vanish, as in join.
pub fn join_str<I, S>(container: I, joiner: &[u8]) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u8]>,
{
    let mut res = Vec::new();
    for s in container {
        if !res.is_empty() {
            res.extend_from_slice(joiner);
        }
        res.extend_from_slice(s.as_ref());
    }
    res
}

#[inline]
pub fn prefix_match(s: &[u8], prefix: &[u8]) -> bool {
    s.starts_with(prefix)
}

pub fn subsequence_match(s: &[u8], subseq: &[u8]) -> bool {
    let mut it = 0;
    for &c in subseq {
        if it == s.len() {
            return false;
        }
        while s[it] != c {
            it += 1;
            if it == s.len() {
                return false;
            }
        }
        it += 1;
    }
    true
}

pub fn expand_tabs(line: &[u8], tabstop: ColumnCount, mut col: ColumnCount) -> Vec<u8> {
    let mut res = Vec::with_capacity(line.len());
    let mut it = 0;
    while it < line.len() {
        if line[it] == b'\t' {
            // [DEVIATION] a zero tabstop divides by zero: UB in C++, a panic here
            let end_col = (col / tabstop + 1) * tabstop;
            res.extend(std::iter::repeat_n(b' ', (end_col - col) as usize));
            col = end_col;
            it += 1;
        } else {
            let beg = it;
            let cp = utf8::read_codepoint(line, &mut it);
            res.extend_from_slice(&line[beg..it]);
            col += codepoint_width(cp);
        }
    }
    res
}

pub fn str_to_int_ifp(s: &[u8]) -> Option<i32> {
    let negative = !s.is_empty() && s[0] == b'-';
    let s = if negative { &s[1..] } else { s };
    if s.is_empty() {
        return None;
    }
    let mut res: u32 = 0;
    for &c in s {
        if !c.is_ascii_digit() {
            return None;
        }
        res = res.wrapping_mul(10).wrapping_add((c - b'0') as u32);
    }
    Some(if negative { (res as i32).wrapping_neg() } else { res as i32 })
}

pub fn str_to_int(s: &[u8]) -> Result<i32> {
    match str_to_int_ifp(s) {
        Some(v) => Ok(v),
        None => {
            let mut msg = s.to_vec();
            msg.extend_from_slice(b" is not a number");
            Err(Error::runtime(msg))
        }
    }
}

pub fn double_up(s: &[u8], characters: &[u8]) -> Vec<u8> {
    let mut res = Vec::with_capacity(s.len());
    for &c in s {
        res.push(c);
        if characters.contains(&c) {
            res.push(c);
        }
    }
    res
}

pub fn quote(s: &[u8]) -> Vec<u8> {
    let mut res = vec![b'\''];
    res.extend(double_up(s, b"'"));
    res.push(b'\'');
    res
}

pub fn shell_quote(s: &[u8]) -> Vec<u8> {
    let mut res = vec![b'\''];
    res.extend(replace(s, b"'", b"'\\''"));
    res.push(b'\'');
    res
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quoting {
    Raw,
    Kakoune,
    Shell,
}

pub const QUOTING_DESC: &[(Quoting, &str)] =
    &[(Quoting::Raw, "raw"), (Quoting::Kakoune, "kakoune"), (Quoting::Shell, "shell")];

pub fn quoter(quoting: Quoting) -> fn(&[u8]) -> Vec<u8> {
    match quoting {
        Quoting::Kakoune => quote,
        Quoting::Shell => shell_quote,
        Quoting::Raw => |s: &[u8]| s.to_vec(),
    }
}

/// Port of the `split` view: splits on separator, with optional escaper
/// (escaped separators do not split, escapes are kept in the output).
/// An empty input yields no element.
pub fn split_escaped(s: &[u8], separator: u8, escaper: Option<u8>) -> Vec<&[u8]> {
    let mut res = Vec::new();
    let end = s.len();
    if end == 0 {
        return res;
    }
    let mut pos = 0;
    let mut escaped = false;
    let mut sep = pos;
    while sep != end && (escaped || s[sep] != separator) {
        escaped = escaper.is_some() && !escaped && Some(s[sep]) == escaper;
        sep += 1;
    }
    loop {
        res.push(&s[pos..sep]);
        if sep == end {
            break;
        }
        pos = sep + 1;
        // [FAITHFUL] when the separator is the escaper, it escapes the
        // character following it (C++ ranges.hh SplitView::advance)
        let mut escaped = escaper.is_some() && Some(s[sep]) == escaper;
        sep = pos;
        while sep != end {
            if !escaped && s[sep] == separator {
                break;
            }
            escaped = escaper.is_some() && !escaped && Some(s[sep]) == escaper;
            sep += 1;
        }
    }
    res
}

pub fn split(s: &[u8], separator: u8) -> Vec<&[u8]> {
    split_escaped(s, separator, None)
}

/// split, keeping the separator at the end of each element
pub fn split_after(s: &[u8], separator: u8) -> Vec<&[u8]> {
    let mut res = Vec::new();
    let end = s.len();
    let mut pos = 0;
    while pos != end {
        let sep = s[pos..].iter().position(|&c| c == separator).map(|p| p + pos);
        match sep {
            Some(sep) => {
                res.push(&s[pos..sep + 1]);
                pos = sep + 1;
            }
            None => {
                res.push(&s[pos..]);
                pos = end;
            }
        }
    }
    res
}

/// Equivalent of `str | split<StringView>(sep, '\\') | transform(unescape<sep, '\\'>) | static_gather<error, N>()`
///
/// Like static_gather, the error receives the index at which gathering
/// failed: k - 1 when only k < n elements are available, max(n, 1) when there
/// are too many. static_gather dereferences the begin iterator even for an
/// empty input, which yields a single empty element.
pub fn split_unescape_n(s: &[u8], separator: u8, n: usize, err: impl FnOnce(usize) -> Error) -> Result<Vec<Vec<u8>>> {
    let mut parts = split_escaped(s, separator, Some(b'\\'));
    if parts.is_empty() {
        parts.push(&s[..0]);
    }
    if parts.len() < n {
        return Err(err(parts.len() - 1));
    }
    if parts.len() > n.max(1) {
        return Err(err(n.max(1)));
    }
    Ok(parts.into_iter().take(n).map(|p| unescape(p, &[separator, b'\\'], b'\\')).collect())
}

pub fn to_string_i(v: impl std::fmt::Display) -> Vec<u8> {
    v.to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string() {
        assert_eq!(trim_indent(b" ").unwrap(), b"");
        assert_eq!(trim_indent(b"no-indent").unwrap(), b"no-indent");
        assert_eq!(trim_indent(b"\nno-indent").unwrap(), b"no-indent");
        assert_eq!(trim_indent(b"\n  indent\n  indent").unwrap(), b"indent\nindent");
        assert_eq!(trim_indent(b"\n  indent\n    indent").unwrap(), b"indent\n  indent");
        assert_eq!(trim_indent(b"\n  indent\n  indent\n   ").unwrap(), b"indent\nindent");
        assert!(trim_indent(b"\n  indent\nno-indent").is_err());
        assert_eq!(escape(br"\youpi:matin:tchou\:", b":\\", b'\\'), br"\\youpi\:matin\:tchou\\\:");
        assert_eq!(unescape(br"\\youpi\:matin\:tchou\\\:", b":\\", b'\\'), br"\youpi:matin:tchou\:");
        assert!(prefix_match(b"tchou kanaky", b"tchou"));
        assert!(!prefix_match(b"tchou kanaky", b"c"));
        assert!(subsequence_match(b"tchou kanaky", b"tknky"));
        assert!(subsequence_match(b"tchou kanaky", b"knk"));
        assert!(!subsequence_match(b"tchou kanaky", b"tchou  kanaky"));
        assert_eq!(str_to_int(b"5").unwrap(), 5);
        assert_eq!(str_to_int(i32::MAX.to_string().as_bytes()).unwrap(), i32::MAX);
        assert_eq!(str_to_int(i32::MIN.to_string().as_bytes()).unwrap(), i32::MIN);
        assert_eq!(str_to_int(b"00").unwrap(), 0);
        assert_eq!(str_to_int(b"-0").unwrap(), 0);
        assert_eq!(double_up(br#"'foo%"bar"'"#, b"'\"%"), br#"''foo%%""bar""''"#);
        assert_eq!(replace(b"tchou/tcha/tchi", b"/", b"!!"), b"tchou!!tcha!!tchi");
    }

    #[test]
    fn test_split() {
        assert_eq!(split(b"a:b", b':'), vec![&b"a"[..], b"b"]);
        assert_eq!(split(b"a:", b':'), vec![&b"a"[..], b""]);
        assert_eq!(split(b":a", b':'), vec![&b""[..], b"a"]);
        assert!(split(b"", b':').is_empty());
        assert_eq!(split_escaped(br"a\:b:c", b':', Some(b'\\')), vec![&br"a\:b"[..], b"c"]);
        assert_eq!(split_after(b"a\nb\n", b'\n'), vec![&b"a\n"[..], b"b\n"]);
        assert_eq!(split_after(b"a\nb", b'\n'), vec![&b"a\n"[..], b"b"]);
    }

    use crate::unicode::init_test_locale;
    use hegel::generators as gs;
    use hegel::{Generator as _, TestCase};

    /// Bytes drawn either freely or from a small alphabet rich in separators
    /// and escapes, so that interesting interactions are frequent.
    fn bytes_over(alphabet: &[u8]) -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(gs::binary(), gs::vecs(gs::sampled_from(alphabet.to_vec())))
    }

    fn special_bytes() -> impl hegel::PrintableGenerator<Vec<u8>> {
        bytes_over(b"ab:|=\\' \n\t")
    }

    fn separator() -> impl hegel::PrintableGenerator<u8> {
        hegel::one_of!(gs::sampled_from(b":|= \n".to_vec()), gs::integers::<u8>().filter(|&c| c != b'\\'))
    }

    #[hegel::test(test_cases = 1000)]
    fn unescape_inverts_escape(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let mut chars = tc.draw(gs::vecs(gs::sampled_from(b":|=\\'".to_vec())).max_size(4));
        let esc = tc.draw(gs::sampled_from(b"\\%".to_vec()));
        // as in every Kakoune use, the escape character is escaped too
        chars.push(esc);
        assert_eq!(unescape(&escape(&s, &chars, esc), &chars, esc), s);
    }

    #[hegel::test(test_cases = 1000)]
    fn escape_prefixes_each_special_char(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let chars = tc.draw(bytes_over(b":|=\\'"));
        let esc = tc.draw(gs::integers::<u8>());
        let mut expected = Vec::new();
        for &c in &s {
            if chars.contains(&c) {
                expected.push(esc);
            }
            expected.push(c);
        }
        assert_eq!(escape(&s, &chars, esc), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn unescape_without_escape_char_is_identity(tc: TestCase) {
        let esc = tc.draw(gs::sampled_from(b"\\%".to_vec()));
        let s = tc.draw(special_bytes().filter(move |s| !s.contains(&esc)));
        let chars = tc.draw(special_bytes());
        assert_eq!(unescape(&s, &chars, esc), s);
    }

    /// [FAITHFUL] join() only adds the joiner once the result is non empty,
    /// so leading empty elements vanish.
    fn skip_leading_empty(parts: &[Vec<u8>]) -> &[Vec<u8>] {
        let first = parts.iter().position(|p| !p.is_empty()).unwrap_or(parts.len());
        &parts[first..]
    }

    #[hegel::test(test_cases = 1000)]
    fn join_str_is_std_join_without_leading_empties(tc: TestCase) {
        let parts = tc.draw(gs::vecs(special_bytes()));
        let joiner = tc.draw(special_bytes());
        assert_eq!(join_str(&parts, &joiner), skip_leading_empty(&parts).join(&joiner[..]));
    }

    #[hegel::test(test_cases = 1000)]
    fn join_unescaped_is_std_join_without_leading_empties(tc: TestCase) {
        let parts = tc.draw(gs::vecs(special_bytes()));
        let sep = tc.draw(separator());
        assert_eq!(join(&parts, sep, false), skip_leading_empty(&parts).join(&sep));
    }

    #[hegel::test(test_cases = 1000)]
    fn split_escaped_unescape_inverts_join(tc: TestCase) {
        let parts = tc.draw(gs::vecs(special_bytes()).min_size(1));
        let sep = tc.draw(separator());
        let joined = join(&parts, sep, true);
        let split: Vec<Vec<u8>> =
            split_escaped(&joined, sep, Some(b'\\')).into_iter().map(|p| unescape(p, &[sep, b'\\'], b'\\')).collect();
        // [FAITHFUL] leading empty elements are lost by join
        assert_eq!(split, skip_leading_empty(&parts));
    }

    #[hegel::test(test_cases = 1000)]
    fn split_matches_std_split(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let sep = tc.draw(separator());
        let expected: Vec<&[u8]> = if s.is_empty() { vec![] } else { s.split(|&c| c == sep).collect() };
        assert_eq!(split(&s, sep), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn split_escaped_parts_rejoin_to_input(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let sep = tc.draw(hegel::one_of!(separator(), gs::just(b'\\')));
        let parts = split_escaped(&s, sep, Some(b'\\'));
        assert_eq!(parts.join(&sep), s);
    }

    // A separator splits iff it is not escaped. When the separator is the
    // escaper itself, a separator also escapes the following character
    // ([FAITHFUL] C++ ranges.hh SplitView quirk), which this model reproduces.
    #[hegel::test(test_cases = 1000)]
    fn split_escaped_splits_on_unescaped_separators_only(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let sep = tc.draw(hegel::one_of!(separator(), gs::just(b'\\')));
        let parts = split_escaped(&s, sep, Some(b'\\'));
        let mut expected_splits = 0;
        let mut escaped = false;
        for &c in &s {
            if !escaped && c == sep {
                expected_splits += 1;
            }
            escaped = !escaped && c == b'\\';
        }
        let expected_parts = if s.is_empty() { 0 } else { expected_splits + 1 };
        assert_eq!(parts.len(), expected_parts);
    }

    #[hegel::test(test_cases = 1000)]
    fn split_after_keeps_separators(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let sep = tc.draw(separator());
        let parts = split_after(&s, sep);
        assert_eq!(parts.concat(), s);
        for (i, p) in parts.iter().enumerate() {
            assert!(!p.is_empty());
            let body = if i + 1 < parts.len() {
                assert_eq!(p.last(), Some(&sep));
                &p[..p.len() - 1]
            } else if p.last() == Some(&sep) {
                &p[..p.len() - 1]
            } else {
                &p[..]
            };
            assert!(!body.contains(&sep));
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn replace_matches_bstr(tc: TestCase) {
        use bstr::ByteSlice;
        let s = tc.draw(special_bytes());
        // [FAITHFUL] an empty pattern makes replace loop forever, as in C++
        let pattern = tc.draw(gs::vecs(gs::sampled_from(b"ab:'".to_vec())).min_size(1).max_size(3));
        let replacement = tc.draw(special_bytes());
        assert_eq!(replace(&s, &pattern, &replacement), s.replace(&pattern, &replacement));
    }

    #[hegel::test(test_cases = 1000)]
    fn str_to_int_inverts_to_string(tc: TestCase) {
        let i = tc.draw(gs::integers::<i32>());
        assert_eq!(str_to_int(i.to_string().as_bytes()).unwrap(), i);
    }

    #[hegel::test(test_cases = 1000)]
    fn str_to_int_ignores_leading_zeros(tc: TestCase) {
        let i = tc.draw(gs::integers::<i32>().min_value(0));
        let zeros = tc.draw(gs::integers::<usize>().max_value(5));
        let neg = tc.draw(gs::booleans());
        let s = format!("{}{}{}", if neg { "-" } else { "" }, "0".repeat(zeros), i);
        assert_eq!(str_to_int_ifp(s.as_bytes()), Some(if neg { i.wrapping_neg() } else { i }));
    }

    // Overflow wraps modulo 2^32 like the C++ unsigned accumulator
    #[hegel::test(test_cases = 1000)]
    fn str_to_int_wraps_on_overflow(tc: TestCase) {
        let digits = tc.draw(gs::vecs(gs::integers::<u8>().max_value(9)).min_size(1).max_size(30));
        let s: Vec<u8> = digits.iter().map(|d| b'0' + d).collect();
        let expected = digits.iter().fold(0u128, |acc, &d| (acc * 10 + d as u128) % (1u128 << 32)) as u32 as i32;
        assert_eq!(str_to_int_ifp(&s), Some(expected));
    }

    #[hegel::test(test_cases = 1000)]
    fn str_to_int_rejects_non_digits(tc: TestCase) {
        let s = tc.draw(special_bytes().filter(|s| {
            let body = s.strip_prefix(b"-").unwrap_or(s);
            body.is_empty() || !body.iter().all(u8::is_ascii_digit)
        }));
        assert_eq!(str_to_int_ifp(&s), None);
        assert_eq!(str_to_int(&s).unwrap_err().what(), [&s[..], b" is not a number"].concat());
    }

    #[hegel::test(test_cases = 1000)]
    fn subsequence_match_is_subsequence(tc: TestCase) {
        let s = tc.draw(bytes_over(b"abc"));
        let sub = tc.draw(gs::vecs(gs::sampled_from(b"abc".to_vec())).max_size(5));
        let mut it = s.iter();
        let expected = sub.iter().all(|c| it.any(|x| x == c));
        assert_eq!(subsequence_match(&s, &sub), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn subsequence_match_accepts_subsequences(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let keep = tc.draw(gs::vecs(gs::booleans()).min_size(s.len()).max_size(s.len()));
        let sub: Vec<u8> = s.iter().zip(&keep).filter(|(_, k)| **k).map(|(c, _)| *c).collect();
        assert!(subsequence_match(&s, &sub));
    }

    #[hegel::test(test_cases = 1000)]
    fn double_up_doubles_each_special_char(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let chars = tc.draw(bytes_over(b"'%\""));
        let expected: Vec<u8> =
            s.iter().flat_map(|&c| if chars.contains(&c) { vec![c, c] } else { vec![c] }).collect();
        assert_eq!(double_up(&s, &chars), expected);
    }

    /// Kakoune single quoted string parsing: '' is a literal quote
    fn kak_unquote(s: &[u8]) -> Option<Vec<u8>> {
        let inner = s.strip_prefix(b"'")?.strip_suffix(b"'")?;
        let mut res = Vec::new();
        let mut i = 0;
        while i < inner.len() {
            if inner[i] == b'\'' {
                if inner.get(i + 1) != Some(&b'\'') {
                    return None;
                }
                i += 1;
            }
            res.push(inner[i]);
            i += 1;
        }
        Some(res)
    }

    /// POSIX shell word parsing restricted to single quotes and backslashes
    fn sh_unquote(s: &[u8]) -> Option<Vec<u8>> {
        let mut res = Vec::new();
        let mut i = 0;
        while i < s.len() {
            match s[i] {
                b'\'' => {
                    let end = s[i + 1..].iter().position(|&c| c == b'\'')? + i + 1;
                    res.extend_from_slice(&s[i + 1..end]);
                    i = end + 1;
                }
                b'\\' => {
                    res.push(*s.get(i + 1)?);
                    i += 2;
                }
                c if c.is_ascii_whitespace() => return None,
                c => {
                    res.push(c);
                    i += 1;
                }
            }
        }
        Some(res)
    }

    #[hegel::test(test_cases = 1000)]
    fn quote_is_kakoune_single_quoting(tc: TestCase) {
        let s = tc.draw(special_bytes());
        assert_eq!(kak_unquote(&quote(&s)), Some(s));
    }

    #[hegel::test(test_cases = 1000)]
    fn shell_quote_is_posix_single_quoting(tc: TestCase) {
        let s = tc.draw(special_bytes());
        assert_eq!(sh_unquote(&shell_quote(&s)), Some(s));
    }

    #[hegel::test(test_cases = 1000)]
    fn indent_prefixes_every_line(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let ind = tc.draw(bytes_over(b" \t>"));
        let expected: Vec<u8> = split_after(&s, b'\n').iter().flat_map(|l| [&ind[..], l].concat()).collect();
        assert_eq!(indent(&s, &ind), expected);
    }

    fn line_text() -> impl hegel::PrintableGenerator<String> {
        gs::vecs(gs::sampled_from(vec!["a", "à", " ", "\t", "é", "-"]))
            .min_size(1)
            .map(|v| v.concat())
            .filter(|l| !l.starts_with([' ', '\t']) && !l.ends_with([' ', '\t']))
    }

    /// Indentation: horizontal blanks, including multibyte ones, possibly
    /// followed by stray continuation bytes (which the C++ utf8 iterator
    /// skips over when moving to the next character).
    fn indentation() -> impl hegel::PrintableGenerator<Vec<u8>> {
        gs::vecs(gs::sampled_from(vec![&b" "[..], b"\t", b"\t\x80", b"\xe2\x80\xa8", b"\xc2\xa0", b" \xbf\xbf"]))
            .map(|v| v.concat())
    }

    #[hegel::test(test_cases = 1000)]
    fn trim_indent_removes_common_indent(tc: TestCase) {
        let lines = tc.draw(gs::vecs(line_text()).min_size(1).max_size(5));
        let ind = tc.draw(indentation());
        // [FAITHFUL] trailing blanks are trimmed byte per byte (signed char),
        // so only ascii ones
        let trailing = tc.draw(gs::optional(gs::text().alphabet(" \t\n")));
        let mut text = b"\n".to_vec();
        text.extend(join_str(lines.iter().map(|l| [&ind[..], l.as_bytes()].concat()), b"\n"));
        if let Some(trailing) = trailing {
            text.push(b'\n');
            text.extend_from_slice(trailing.as_bytes());
        }
        assert_eq!(trim_indent(&text).unwrap(), lines.join("\n").as_bytes());
    }

    #[hegel::test(test_cases = 1000)]
    fn trim_indent_rejects_inconsistent_indent(tc: TestCase) {
        let first = tc.draw(line_text());
        let second = tc.draw(line_text());
        let ind = tc.draw(indentation().filter(|i| !i.is_empty()));
        let text = [&b"\n"[..], &ind, first.as_bytes(), b"\n", second.as_bytes()].concat();
        assert!(trim_indent(&text).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn expand_tabs_reaches_tab_stops(tc: TestCase) {
        let line = tc.draw(bytes_over(b"ab \t"));
        let line: Vec<u8> = line.into_iter().filter(|c| c.is_ascii_graphic() || *c == b' ' || *c == b'\t').collect();
        // [DEVIATION] tabstop 0 divides by zero (UB in C++, a panic here); starting columns are never
        // negative in Kakoune (C++ integer division would round towards 0)
        let tabstop = tc.draw(gs::integers::<i32>().min_value(1).max_value(16));
        let col = tc.draw(gs::integers::<i32>().min_value(0).max_value(100));
        let mut expected = Vec::new();
        let mut c = col;
        for &b in &line {
            if b == b'\t' {
                let n = tabstop - c % tabstop;
                expected.extend(std::iter::repeat_n(b' ', n as usize));
                c += n;
            } else {
                expected.push(b);
                c += 1;
            }
        }
        assert_eq!(expand_tabs(&line, tabstop, col), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn expand_tabs_without_tabs_is_identity(tc: TestCase) {
        let line = tc.draw(special_bytes().filter(|l| !l.contains(&b'\t')));
        let tabstop = tc.draw(gs::integers::<i32>().min_value(1));
        let col = tc.draw(gs::integers::<i32>().min_value(0).max_value(1000));
        assert_eq!(expand_tabs(&line, tabstop, col), line);
    }

    fn printable_ascii() -> impl hegel::PrintableGenerator<String> {
        gs::text().alphabet("abc XYZ-_/:'")
    }

    #[hegel::test(test_cases = 1000)]
    fn left_pad_matches_std_right_alignment(tc: TestCase) {
        let s = tc.draw(printable_ascii());
        let n = tc.draw(gs::integers::<i32>().min_value(0).max_value(20));
        let truncated = &s[..s.len().min(n as usize)];
        assert_eq!(left_pad(s.as_bytes(), n, ' ' as u32), format!("{:>w$}", truncated, w = n as usize).as_bytes());
    }

    #[hegel::test(test_cases = 1000)]
    fn right_pad_matches_std_left_alignment(tc: TestCase) {
        let s = tc.draw(printable_ascii());
        let n = tc.draw(gs::integers::<i32>().min_value(0).max_value(20));
        let truncated = &s[..s.len().min(n as usize)];
        assert_eq!(right_pad(s.as_bytes(), n, ' ' as u32), format!("{:<w$}", truncated, w = n as usize).as_bytes());
    }

    #[hegel::test(test_cases = 1000)]
    fn pad_has_requested_width_for_ascii(tc: TestCase) {
        let s = tc.draw(printable_ascii());
        let n = tc.draw(gs::integers::<i32>().min_value(0).max_value(20));
        let c = tc.draw(gs::sampled_from(vec!['.' as u32, '0' as u32]));
        assert_eq!(column_length(&left_pad(s.as_bytes(), n, c)), n);
        assert_eq!(column_length(&right_pad(s.as_bytes(), n, c)), n);
    }

    // A negative size means no truncation (substr with a negative length).
    // [DEVIATION] Sizes close to i32::MIN make `size - column_length`
    // overflow, UB in C++ (it would wrap to a huge padding) and a panic in
    // debug builds here, so they are excluded.
    #[hegel::test(test_cases = 1000)]
    fn pad_with_negative_size_is_identity(tc: TestCase) {
        let s = tc.draw(valid_text());
        let n = tc.draw(gs::integers::<i32>().min_value(-1_000_000).max_value(-1));
        assert_eq!(left_pad(s.as_bytes(), n, ' ' as u32), s.as_bytes());
        assert_eq!(right_pad(s.as_bytes(), n, ' ' as u32), s.as_bytes());
    }

    #[hegel::test(test_cases = 1000)]
    fn substr_bytes_clamps_length(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let from = tc.draw(gs::integers::<usize>().max_value(s.len()));
        let length = tc.draw(gs::integers::<isize>().min_value(-3).max_value(s.len() as isize + 3));
        let end = if length < 0 { s.len() } else { (from + length as usize).min(s.len()) };
        assert_eq!(substr(&s, from, length), &s[from..end]);
    }

    fn valid_text() -> impl hegel::PrintableGenerator<String> {
        hegel::one_of!(gs::text(), gs::text().alphabet("aé€😀中\u{300}"))
    }

    #[hegel::test(test_cases = 1000)]
    fn substr_columns_is_column_range_on_ascii(tc: TestCase) {
        let s = tc.draw(printable_ascii());
        let from = tc.draw(gs::integers::<i32>().min_value(0).max_value(s.len() as i32 + 2));
        let length = tc.draw(gs::integers::<i32>().min_value(-1).max_value(s.len() as i32 + 2));
        let b = s.as_bytes();
        let beg = (from as usize).min(b.len());
        let end = if length < 0 { b.len() } else { (beg + length as usize).min(b.len()) };
        assert_eq!(substr_columns(b, from, length), &b[beg..end]);
        assert_eq!(byte_count_to_column(b, from), beg);
    }

    #[hegel::test(test_cases = 1000)]
    fn unescape_chars_is_unescape_of_char_and_escape(tc: TestCase) {
        let s = tc.draw(special_bytes());
        let c = tc.draw(separator());
        assert_eq!(unescape_chars(&s, c, b'\\'), unescape(&s, &[c, b'\\'], b'\\'));
    }

    // static_gather semantics: an empty input gathers as one empty element,
    // errors carry the index where gathering stopped (k - 1 for k < n
    // elements, n when there are too many)
    #[hegel::test(test_cases = 1000)]
    fn split_unescape_n_gathers_exactly_n(tc: TestCase) {
        let parts = tc.draw(gs::vecs(special_bytes().filter(|p| !p.is_empty())).max_size(5));
        let sep = tc.draw(separator());
        let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));
        let joined = join(&parts, sep, true);
        let available = if parts.is_empty() { vec![Vec::new()] } else { parts.clone() };
        match split_unescape_n(&joined, sep, n, |got| Error::runtime(format!("{got}"))) {
            Ok(res) => assert_eq!(res, available),
            Err(e) => {
                let k = available.len();
                assert_ne!(k, n);
                let expected = if k < n { k - 1 } else { n };
                assert_eq!(e.what(), expected.to_string().as_bytes());
            }
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn quoter_dispatches_on_quoting(tc: TestCase) {
        let s = tc.draw(special_bytes());
        assert_eq!(quoter(Quoting::Raw)(&s), s);
        assert_eq!(quoter(Quoting::Kakoune)(&s), quote(&s));
        assert_eq!(quoter(Quoting::Shell)(&s), shell_quote(&s));
    }

    #[hegel::test(test_cases = 1000)]
    fn substr_chars_matches_std(tc: TestCase) {
        let s = tc.draw(valid_text());
        let count = s.chars().count() as i32;
        let from = tc.draw(gs::integers::<i32>().min_value(0).max_value(count + 2));
        let length = tc.draw(gs::integers::<i32>().min_value(-1).max_value(count + 2));
        let expected: String =
            s.chars().skip(from as usize).take(if length < 0 { usize::MAX } else { length as usize }).collect();
        assert_eq!(substr_chars(s.as_bytes(), from, length), expected.as_bytes());
    }

    #[hegel::test(test_cases = 1000)]
    fn char_at_matches_std(tc: TestCase) {
        let s = tc.draw(valid_text());
        tc.assume(!s.is_empty());
        let count = s.chars().count();
        let i = tc.draw(gs::integers::<usize>().max_value(count - 1));
        assert_eq!(char_at(s.as_bytes(), i as i32), s.chars().nth(i).unwrap() as u32);
        assert_eq!(byte_count_to_char(s.as_bytes(), i as i32), s.char_indices().nth(i).unwrap().0);
        assert_eq!(char_length(s.as_bytes()), count as i32);
    }

    #[hegel::test(test_cases = 1000)]
    fn string_of_cp_repeats_encoding(tc: TestCase) {
        init_test_locale();
        let cp = tc.draw(gs::characters()) as u32;
        let count = tc.draw(gs::integers::<i32>().min_value(-3).max_value(10));
        assert_eq!(string_of_cp(cp, count), utf8::encode(cp).repeat(count.max(0) as usize));
    }

    #[hegel::test(test_cases = 1000)]
    fn string_of_cp_cols_repeats_to_fill_columns(tc: TestCase) {
        init_test_locale();
        let cp = tc.draw(gs::characters()) as u32;
        let cols = tc.draw(gs::integers::<i32>().min_value(-3).max_value(10));
        let width = codepoint_width(cp).max(1);
        assert_eq!(string_of_cp_cols(cp, cols), utf8::encode(cp).repeat((cols / width).max(0) as usize));
    }
}
