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

impl FormatParam for bool {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(if *self { b"true" } else { b"false" })
    }
}

impl FormatParam for f32 {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Owned(to_string_float(*self as f64).into_bytes())
    }
}
impl FormatParam for f64 {
    fn format_param(&self) -> Cow<'_, [u8]> {
        Cow::Owned(to_string_float(*self).into_bytes())
    }
}

/// Format a float like std::to_chars with chars_format::general (shortest representation)
fn to_string_float(v: f64) -> String {
    let s = format!("{}", v);
    s
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
}
