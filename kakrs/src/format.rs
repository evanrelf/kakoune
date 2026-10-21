//! Port of format.hh/format.cc: byte preserving string formatting.

use crate::exception::{Error, Result};
use crate::string_utils::str_to_int;
use crate::units::Codepoint;
use std::borrow::Cow;

/// Something that can be passed as a kformat! parameter
pub trait FormatParam {
    fn format_param(&self) -> Cow<'_, [u8]>;
}

impl FormatParam for [u8] {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self)
    }
}
impl<const N: usize> FormatParam for [u8; N] {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self)
    }
}
impl FormatParam for Vec<u8> {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self)
    }
}
impl FormatParam for str {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.as_bytes())
    }
}
impl FormatParam for String {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.as_bytes())
    }
}
impl FormatParam for std::rc::Rc<[u8]> {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self)
    }
}
impl<T: FormatParam + ?Sized> FormatParam for &T {
    fn format_param(&self) -> Cow<'_, [u8]> {
        (**self).format_param()
    }
}
impl<T: FormatParam + ?Sized> FormatParam for &mut T {
    fn format_param(&self) -> Cow<'_, [u8]> {
        (**self).format_param()
    }
}
impl FormatParam for Cow<'_, [u8]> {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self)
    }
}

macro_rules! impl_format_param_int {
    ($($t:ty),*) => {
        $(impl FormatParam for $t {
            fn format_param(&self) -> Cow<'_, [u8]> {
                Cow::Owned(self.to_string().into_bytes())
            }
        })*
    };
}
impl_format_param_int!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

/// [FAITHFUL] C++ has no to_string(bool): a bool parameter goes through
/// to_string(int) and prints as 1 or 0
impl FormatParam for bool {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(if *self { b"1" } else { b"0" })
    }
}

impl FormatParam for f32 {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Owned(to_string_float(*self).into_bytes())
    }
}
impl FormatParam for f64 {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Owned(to_string_float(*self).into_bytes())
    }
}

/// Format a float like std::to_chars with chars_format::general: the
/// shortest representation that round trips, in printf's %g style (fixed
/// notation when the decimal exponent X satisfies -4 <= X < 6, scientific
/// otherwise, exponent with a sign and at least two digits).
///
/// [DEVIATION] C++ only uses to_chars when __cpp_lib_to_chars is defined, it
/// falls back on snprintf("%f") otherwise (e.g. with Apple's libc++, giving
/// "0.100000"): the port always follows the to_chars path.
fn to_string_float<F: std::fmt::LowerExp + Copy + Into<f64>>(v: F) -> String {
    let f: f64 = v.into();
    if f.is_nan() {
        return if f.is_sign_negative() { "-nan" } else { "nan" }.to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    // `{:e}` gives the shortest round tripping digits for the type of v
    let sci = format!("{:e}", v);
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    if (-4..6).contains(&exp) {
        let digits: String = mantissa.chars().filter(|&c| c != '.').collect();
        let point = exp + 1;
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat(-point as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{body}")
    } else {
        format!("{sign}{mantissa}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// A codepoint formatted as its utf8 encoding
#[derive(Clone, Copy)]
pub struct Cp(pub Codepoint);
impl FormatParam for Cp {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Owned(crate::utf8::encode(self.0))
    }
}

/// An integer formatted as hexadecimal
#[derive(Clone, Copy)]
pub struct Hex(pub usize);
impl FormatParam for Hex {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Owned(format!("{:x}", self.0).into_bytes())
    }
}

/// An integer formatted with thousands separators
#[derive(Clone, Copy)]
pub struct Grouped(pub usize);
impl FormatParam for Grouped {
    fn format_param(&self) -> Cow<'_, [u8]> {
        let ungrouped = self.0.to_string().into_bytes();
        let len = ungrouped.len();
        let mut res = Vec::new();
        for (pos, c) in ungrouped.iter().enumerate() {
            if !res.is_empty() && (len - pos) % 3 == 0 {
                res.push(b',');
            }
            res.push(*c);
        }
        Cow::Owned(res)
    }
}

pub fn to_string_int(v: i64) -> Vec<u8> {
    v.to_string().into_bytes()
}

fn format_impl(fmt: &[u8], params: &[&[u8]], append: &mut dyn FnMut(&[u8])) -> Result<()> {
    let mut implicit_index = 0usize;
    let end = fmt.len();
    let mut it = 0usize;
    while it != end {
        let opening = fmt[it..].iter().position(|&c| c == b'{').map(|p| p + it).unwrap_or(end);
        if opening == end {
            append(&fmt[it..opening]);
            break;
        } else if opening != it && fmt[opening - 1] == b'\\' {
            append(&fmt[it..opening - 1]);
            append(b"{");
            it = opening + 1;
        } else {
            append(&fmt[it..opening]);
            let closing = fmt[opening..].iter().position(|&c| c == b'}').map(|p| p + opening).unwrap_or(end);
            if closing == end {
                return Err(Error::runtime("format string error, unclosed '{'"));
            }
            let mut format = fmt[opening + 1..closing]
                .iter()
                .position(|&c| c == b':')
                .map(|p| p + opening + 1)
                .unwrap_or(closing);
            let index = if opening + 1 == format {
                implicit_index
            } else {
                str_to_int(&fmt[opening + 1..format])? as usize
            };
            if index >= params.len() {
                return Err(Error::runtime("format string parameter index too big"));
            }
            if format != closing {
                let mut padding = b' ';
                format += 1;
                if fmt[format] == b'0' {
                    padding = b'0';
                    format += 1;
                }
                let mut width = str_to_int(&fmt[format..closing])?;
                let len = crate::utf8::column_distance(params[index]);
                while width > len {
                    append(&[padding]);
                    width -= 1;
                }
            }
            append(params[index]);
            implicit_index = index + 1;
            it = closing + 1;
        }
    }
    Ok(())
}

pub fn try_format(fmt: &[u8], params: &[&[u8]]) -> Result<Vec<u8>> {
    let size = fmt.len() + params.iter().map(|p| p.len()).sum::<usize>();
    let mut res = Vec::with_capacity(size);
    format_impl(fmt, params, &mut |s| res.extend_from_slice(s))?;
    Ok(res)
}

/// format with a static format string, panics on malformed format strings
/// (which would be a programming error).
pub fn format(fmt: &[u8], params: &[&[u8]]) -> Vec<u8> {
    match try_format(fmt, params) {
        Ok(r) => r,
        Err(e) => panic!("invalid format string {:?}: {}", String::from_utf8_lossy(fmt), e),
    }
}

pub fn format_with(append: &mut dyn FnMut(&[u8]), fmt: &[u8], params: &[&[u8]]) -> Result<()> {
    format_impl(fmt, params, append)
}

/// kformat!("{} is {}", a, b) -> Vec<u8>, byte preserving equivalent of
/// Kakoune's format()
#[macro_export]
macro_rules! kformat {
    ($fmt:expr) => {
        $crate::format::format(AsRef::<[u8]>::as_ref($fmt), &[])
    };
    ($fmt:expr, $($arg:expr),+ $(,)?) => {{
        use $crate::format::FormatParam as _;
        $crate::format::format(AsRef::<[u8]>::as_ref($fmt), &[$(&*($arg).format_param()),+])
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_format() {
        assert_eq!(
            kformat!("Youhou {1} {} '{0:4}' {2:04} \\{}", 10, "hehe", 5),
            b"Youhou hehe 5 '  10' 0005 {}".to_vec()
        );
        assert_eq!(Grouped(1234567).format_param().as_ref(), b"1,234,567");
    }

    use crate::unicode::init_test_locale;
    use crate::utf8::column_distance;
    use hegel::generators as gs;
    use hegel::{Generator as _, TestCase};

    fn param() -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(
            gs::binary(),
            gs::text().map(String::into_bytes),
            gs::vecs(gs::sampled_from(b"ab{}\\:0".to_vec())),
        )
    }

    /// Literal format text: no '{', and not ending with a backslash (which
    /// would escape a following '{')
    fn literal() -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(gs::binary(), gs::vecs(gs::sampled_from(b"ab}\\:0 ".to_vec())))
            .filter(|l| !l.contains(&b'{') && l.last() != Some(&b'\\'))
    }

    fn fmt(f: &[u8], params: &[Vec<u8>]) -> Result<Vec<u8>> {
        let params: Vec<&[u8]> = params.iter().map(|p| &p[..]).collect();
        try_format(f, &params)
    }

    #[hegel::test(test_cases = 1000)]
    fn literal_text_is_unchanged(tc: TestCase) {
        let l = tc.draw(literal());
        let params = tc.draw(gs::vecs(param()).max_size(3));
        assert_eq!(fmt(&l, &params).unwrap(), l);
    }

    #[hegel::test(test_cases = 1000)]
    fn implicit_indices_interleave_params(tc: TestCase) {
        let params = tc.draw(gs::vecs(param()).max_size(5));
        let literals = tc.draw(gs::vecs(literal()).min_size(params.len() + 1).max_size(params.len() + 1));
        let mut f = literals[0].clone();
        let mut expected = literals[0].clone();
        for (p, l) in params.iter().zip(&literals[1..]) {
            f.extend_from_slice(b"{}");
            f.extend_from_slice(l);
            expected.extend_from_slice(p);
            expected.extend_from_slice(l);
        }
        assert_eq!(fmt(&f, &params).unwrap(), expected);
    }

    // An implicit index continues after the last used index, explicit or not
    #[hegel::test(test_cases = 1000)]
    fn explicit_index_then_implicit(tc: TestCase) {
        let params = tc.draw(gs::vecs(param()).min_size(2).max_size(6));
        let i = tc.draw(gs::integers::<usize>().max_value(params.len() - 2));
        let zeros = tc.draw(gs::integers::<usize>().max_value(2));
        let f = format!("<{{{}{}}}|{{}}>", "0".repeat(zeros), i);
        let expected = [&b"<"[..], &params[i], b"|", &params[i + 1], b">"].concat();
        assert_eq!(fmt(f.as_bytes(), &params).unwrap(), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn padding_pads_to_column_width(tc: TestCase) {
        init_test_locale();
        let params = tc.draw(gs::vecs(param()).min_size(1).max_size(3));
        let i = tc.draw(gs::integers::<usize>().max_value(params.len() - 1));
        let zero = tc.draw(gs::booleans());
        // a width starting with '0' is the zero padding flag, so a plain zero
        // width ("{:0}") is a format error, tested elsewhere
        let width = tc.draw(gs::integers::<i32>().min_value(if zero { 0 } else { 1 }).max_value(30));
        let f = format!("{{{}:{}{}}}", i, if zero { "0" } else { "" }, width);
        let pad = (width - column_distance(&params[i])).max(0) as usize;
        let mut expected = vec![if zero { b'0' } else { b' ' }; pad];
        expected.extend_from_slice(&params[i]);
        assert_eq!(fmt(f.as_bytes(), &params).unwrap(), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn escaped_brace_is_literal(tc: TestCase) {
        let before = tc.draw(literal());
        let inside = tc.draw(literal());
        let params = tc.draw(gs::vecs(param()).max_size(2));
        let f = [&before[..], b"\\{", &inside].concat();
        let expected = [&before[..], b"{", &inside].concat();
        assert_eq!(fmt(&f, &params).unwrap(), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn index_out_of_range_is_an_error(tc: TestCase) {
        let params = tc.draw(gs::vecs(param()).max_size(4));
        let i = tc.draw(gs::integers::<i32>().min_value(params.len() as i32));
        assert!(fmt(format!("{{{i}}}").as_bytes(), &params).is_err());
        // negative indices convert to huge size_t values
        let neg = tc.draw(gs::integers::<i32>().max_value(-1));
        assert!(fmt(format!("{{{neg}}}").as_bytes(), &params).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn malformed_placeholders_are_errors(tc: TestCase) {
        let params = tc.draw(gs::vecs(param()).min_size(1).max_size(3));
        let before = tc.draw(literal());
        let tail = tc.draw(literal().filter(|l| !l.contains(&b'}')));
        let bad: Vec<u8> = tc.draw(gs::sampled_from(vec![
            [&b"{"[..], &tail].concat(), // unclosed
            b"{:}".to_vec(),              // empty width
            b"{:0}".to_vec(),             // empty width after zero
            b"{x}".to_vec(),              // non numeric index
            b"{0:x}".to_vec(),            // non numeric width
        ]));
        assert!(fmt(&[&before[..], &bad].concat(), &params).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn integers_format_as_decimal(tc: TestCase) {
        let i = tc.draw(gs::integers::<i64>());
        assert_eq!(i.format_param().as_ref(), i.to_string().as_bytes());
        assert_eq!(to_string_int(i), i.to_string().as_bytes());
    }

    fn check_decimal<T: FormatParam + std::fmt::Display>(v: T) {
        assert_eq!(v.format_param().as_ref(), v.to_string().as_bytes());
    }

    // every integer instantiation of the format parameter
    #[hegel::test(test_cases = 1000)]
    fn all_integer_types_format_as_decimal(tc: TestCase) {
        check_decimal(tc.draw(gs::integers::<i8>()));
        check_decimal(tc.draw(gs::integers::<i16>()));
        check_decimal(tc.draw(gs::integers::<i32>()));
        check_decimal(tc.draw(gs::integers::<isize>()));
        check_decimal(tc.draw(gs::integers::<u8>()));
        check_decimal(tc.draw(gs::integers::<u16>()));
        check_decimal(tc.draw(gs::integers::<u32>()));
        check_decimal(tc.draw(gs::integers::<u64>()));
        check_decimal(tc.draw(gs::integers::<usize>()));
    }

    #[hegel::test(test_cases = 1000)]
    fn bools_format_as_integers(tc: TestCase) {
        let b = tc.draw(gs::booleans());
        assert_eq!(b.format_param().as_ref(), if b { &b"1"[..] } else { b"0" });
    }

    #[hegel::test(test_cases = 1000)]
    fn format_with_agrees_with_try_format(tc: TestCase) {
        let f = tc.draw(hegel::one_of!(literal(), gs::vecs(gs::sampled_from(b"{}0:\\a".to_vec()))));
        let params = tc.draw(gs::vecs(param()).max_size(3));
        let refs: Vec<&[u8]> = params.iter().map(|p| &p[..]).collect();
        let mut out = Vec::new();
        let res = format_with(&mut |s| out.extend_from_slice(s), &f, &refs);
        match try_format(&f, &refs) {
            Ok(expected) => {
                assert!(res.is_ok());
                assert_eq!(out, expected);
            }
            Err(_) => assert!(res.is_err()),
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn hex_is_lowercase_hexadecimal(tc: TestCase) {
        let n = tc.draw(gs::integers::<usize>());
        let s = Hex(n).format_param().into_owned();
        assert_eq!(usize::from_str_radix(std::str::from_utf8(&s).unwrap(), 16).unwrap(), n);
        assert!(s.iter().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c)));
        assert!(s == b"0" || s[0] != b'0');
    }

    #[hegel::test(test_cases = 1000)]
    fn grouped_inserts_commas_every_three_digits(tc: TestCase) {
        let n = tc.draw(gs::integers::<usize>());
        let digits = n.to_string();
        let mut groups: Vec<String> = digits
            .as_bytes()
            .rchunks(3)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect();
        groups.reverse();
        assert_eq!(Grouped(n).format_param().as_ref(), groups.join(",").as_bytes());
    }

    #[hegel::test(test_cases = 1000)]
    fn codepoint_formats_as_utf8(tc: TestCase) {
        let c = tc.draw(gs::characters());
        assert_eq!(Cp(c as u32).format_param().as_ref(), c.to_string().as_bytes());
    }

    /// Decimal exponent of the shortest scientific representation
    fn sci_exponent(v: f32) -> i32 {
        format!("{v:e}").split_once('e').unwrap().1.parse().unwrap()
    }

    #[hegel::test(test_cases = 1000)]
    fn float_formatting_round_trips(tc: TestCase) {
        let v = tc.draw(gs::floats::<f32>().allow_nan(false).allow_infinity(false));
        let s = String::from_utf8(v.format_param().into_owned()).unwrap();
        let parsed: f32 = s.parse().unwrap();
        assert_eq!(parsed.to_bits(), v.to_bits(), "{s}");
    }

    #[hegel::test(test_cases = 1000)]
    fn float_formatting_uses_g_style_notation(tc: TestCase) {
        let v = tc.draw(gs::floats::<f32>().allow_nan(false).allow_infinity(false));
        let s = String::from_utf8(v.format_param().into_owned()).unwrap();
        let x = sci_exponent(v);
        assert_eq!(s.contains('e'), !(-4..6).contains(&x), "{s}");
        // shortest representation: no trailing zeros after a decimal point
        if !s.contains('e') && s.contains('.') {
            assert!(!s.ends_with('0') && !s.ends_with('.'), "{s}");
        }
    }

    #[test]
    fn float_formatting_examples() {
        // expected std::to_chars(float, chars_format::general) output: shortest
        // round tripping digits, printf %g notation choice (P = 6)
        let cases: &[(f32, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (0.1, "0.1"),
            (-2.5, "-2.5"),
            (100000.0, "100000"),
            (999999.0, "999999"),
            (1000000.0, "1e+06"),
            (1234567.0, "1.234567e+06"),
            (123456.7, "123456.7"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e20, "1e+20"),
            (1.5e-7, "1.5e-07"),
            (1e-45, "1e-45"),
            (3.4028235e38, "3.4028235e+38"),
            (f32::INFINITY, "inf"),
            (f32::NEG_INFINITY, "-inf"),
            (f32::NAN, "nan"),
        ];
        for (v, s) in cases {
            assert_eq!(v.format_param().as_ref(), s.as_bytes(), "{v}");
        }
    }
}
