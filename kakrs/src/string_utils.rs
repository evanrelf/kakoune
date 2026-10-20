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
        if is_blank(last as Codepoint) {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    let mut it = 0;
    while it < s.len() {
        let mut next = it;
        let cp = utf8::read_codepoint(s, &mut next);
        if !is_horizontal_blank(cp) {
            break;
        }
        it = next;
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
pub fn split_unescape_n(s: &[u8], separator: u8, n: usize, err: impl FnOnce(usize) -> Error) -> Result<Vec<Vec<u8>>> {
    let parts = split_escaped(s, separator, Some(b'\\'));
    if parts.len() != n {
        return Err(err(parts.len()));
    }
    Ok(parts.into_iter().map(|p| unescape(p, &[separator, b'\\'], b'\\')).collect())
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
}
