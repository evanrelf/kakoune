//! Port of option.hh and option_types.hh: conversion of option values to and
//! from strings.

use crate::context::Context;
use crate::exception::{Error, Result};
use crate::string_utils::{escape, join, quoter, split_escaped, str_to_int, unescape, Quoting};
use crate::units::{Codepoint, DisplayCoord};
use indexmap::IndexMap;
use std::any::Any;

/// An option value type (C++: types usable with TypedOption<T>)
pub trait OptionType: Any + Clone + PartialEq {
    fn type_name() -> Vec<u8>;
    fn to_string_q(&self, quoting: Quoting) -> Vec<u8>;
    fn to_strings(&self) -> Vec<Vec<u8>> {
        vec![self.to_string_q(Quoting::Raw)]
    }
    fn from_strings(strs: &[Vec<u8>]) -> Result<Self>;
    fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        check_single(strs)?;
        Err(Error::runtime("no add operation supported for this option type"))
    }
    fn remove_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        check_single(strs)?;
        Err(Error::runtime("no remove operation supported for this option type"))
    }
    fn update(&mut self, _context: &Context) -> Result<()> {
        Err(Error::runtime("no update operation supported for this option type"))
    }
    /// value shown in hook parameters (int, bool and str show their value)
    fn desc_string(&self) -> Vec<u8> {
        b"...".to_vec()
    }
}

/// A type that can be an element of a list/map/tuple option
pub trait OptionElem: Any + Clone + PartialEq {
    fn elem_type_name() -> Vec<u8>;
    fn elem_to_string(&self, quoting: Quoting) -> Vec<u8>;
    fn elem_from_string(s: &[u8]) -> Result<Self>;
    fn list_postprocess(_list: &mut Vec<Self>) {}
    fn list_add_from_strings(list: &mut Vec<Self>, strs: &[Vec<u8>]) -> Result<bool> {
        let vec = list_from_strings::<Self>(strs)?;
        let not_empty = !vec.is_empty();
        list.extend(vec);
        Self::list_postprocess(list);
        Ok(not_empty)
    }
}

pub fn check_single(strs: &[Vec<u8>]) -> Result<()> {
    if strs.len() != 1 {
        return Err(Error::runtime("expected a single value for option"));
    }
    Ok(())
}

pub fn list_from_strings<T: OptionElem>(strs: &[Vec<u8>]) -> Result<Vec<T>> {
    let mut res = Vec::with_capacity(strs.len());
    for s in strs {
        res.push(T::elem_from_string(s)?);
    }
    T::list_postprocess(&mut res);
    Ok(res)
}

/// Implements OptionType for a single value type that implements OptionElem.
/// The add/remove variants delegate to OptionAdd.
#[macro_export]
macro_rules! single_value_option {
    ($t:ty) => {
        impl $crate::option_types::OptionType for $t {
            fn type_name() -> Vec<u8> {
                <$t as $crate::option_types::OptionElem>::elem_type_name()
            }
            fn to_string_q(&self, quoting: $crate::string_utils::Quoting) -> Vec<u8> {
                $crate::option_types::OptionElem::elem_to_string(self, quoting)
            }
            fn from_strings(strs: &[Vec<u8>]) -> $crate::exception::Result<Self> {
                $crate::option_types::check_single(strs)?;
                <$t as $crate::option_types::OptionElem>::elem_from_string(&strs[0])
            }
        }
    };
    ($t:ty, add) => {
        impl $crate::option_types::OptionType for $t {
            fn type_name() -> Vec<u8> {
                <$t as $crate::option_types::OptionElem>::elem_type_name()
            }
            fn to_string_q(&self, quoting: $crate::string_utils::Quoting) -> Vec<u8> {
                $crate::option_types::OptionElem::elem_to_string(self, quoting)
            }
            fn from_strings(strs: &[Vec<u8>]) -> $crate::exception::Result<Self> {
                $crate::option_types::check_single(strs)?;
                <$t as $crate::option_types::OptionElem>::elem_from_string(&strs[0])
            }
            fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> $crate::exception::Result<bool> {
                $crate::option_types::check_single(strs)?;
                $crate::option_types::OptionAdd::option_add(self, &strs[0])
            }
            fn remove_from_strings(&mut self, strs: &[Vec<u8>]) -> $crate::exception::Result<bool> {
                $crate::option_types::check_single(strs)?;
                $crate::option_types::OptionAdd::option_remove(self, &strs[0])
            }
        }
    };
}

pub trait OptionAdd {
    fn option_add(&mut self, s: &[u8]) -> Result<bool>;
    fn option_remove(&mut self, _s: &[u8]) -> Result<bool> {
        Err(Error::runtime("no remove operation supported for this option type"))
    }
}

// int ----------------------------------------------------------------------

impl OptionElem for i32 {
    fn elem_type_name() -> Vec<u8> {
        b"int".to_vec()
    }
    fn elem_to_string(&self, _: Quoting) -> Vec<u8> {
        self.to_string().into_bytes()
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        str_to_int(s)
    }
}
impl OptionAdd for i32 {
    fn option_add(&mut self, s: &[u8]) -> Result<bool> {
        let val = str_to_int(s)?;
        // [DEVIATION] signed overflow is UB in C++, wrap here
        *self = self.wrapping_add(val);
        Ok(val != 0)
    }
    fn option_remove(&mut self, s: &[u8]) -> Result<bool> {
        let val = str_to_int(s)?;
        *self = self.wrapping_sub(val);
        Ok(val != 0)
    }
}
impl OptionType for i32 {
    fn type_name() -> Vec<u8> {
        b"int".to_vec()
    }
    fn to_string_q(&self, q: Quoting) -> Vec<u8> {
        self.elem_to_string(q)
    }
    fn from_strings(strs: &[Vec<u8>]) -> Result<Self> {
        check_single(strs)?;
        str_to_int(&strs[0])
    }
    fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        check_single(strs)?;
        self.option_add(&strs[0])
    }
    fn remove_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        check_single(strs)?;
        self.option_remove(&strs[0])
    }
    fn desc_string(&self) -> Vec<u8> {
        self.to_string_q(Quoting::Raw)
    }
}

impl OptionElem for usize {
    fn elem_type_name() -> Vec<u8> {
        b"int".to_vec()
    }
    fn elem_to_string(&self, _: Quoting) -> Vec<u8> {
        self.to_string().into_bytes()
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        Ok(str_to_int(s)? as usize)
    }
}

// bool ---------------------------------------------------------------------

impl OptionElem for bool {
    fn elem_type_name() -> Vec<u8> {
        b"bool".to_vec()
    }
    fn elem_to_string(&self, _: Quoting) -> Vec<u8> {
        if *self { b"true".to_vec() } else { b"false".to_vec() }
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        match s {
            b"true" | b"yes" => Ok(true),
            b"false" | b"no" => Ok(false),
            _ => Err(Error::runtime("boolean values are either true, yes, false or no")),
        }
    }
}
impl OptionType for bool {
    fn type_name() -> Vec<u8> {
        b"bool".to_vec()
    }
    fn to_string_q(&self, q: Quoting) -> Vec<u8> {
        self.elem_to_string(q)
    }
    fn from_strings(strs: &[Vec<u8>]) -> Result<Self> {
        check_single(strs)?;
        bool::elem_from_string(&strs[0])
    }
    fn desc_string(&self) -> Vec<u8> {
        self.to_string_q(Quoting::Raw)
    }
}

// String -------------------------------------------------------------------

impl OptionElem for Vec<u8> {
    fn elem_type_name() -> Vec<u8> {
        b"str".to_vec()
    }
    fn elem_to_string(&self, q: Quoting) -> Vec<u8> {
        quoter(q)(self)
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        Ok(s.to_vec())
    }
}
impl OptionType for Vec<u8> {
    fn type_name() -> Vec<u8> {
        b"str".to_vec()
    }
    fn to_string_q(&self, q: Quoting) -> Vec<u8> {
        quoter(q)(self)
    }
    fn from_strings(strs: &[Vec<u8>]) -> Result<Self> {
        check_single(strs)?;
        Ok(strs[0].clone())
    }
    fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        check_single(strs)?;
        self.extend_from_slice(&strs[0]);
        Ok(!strs[0].is_empty())
    }
    fn desc_string(&self) -> Vec<u8> {
        self.clone()
    }
}

// Codepoint ----------------------------------------------------------------

/// Codepoint newtype for options (to distinguish from int)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct CodepointOpt(pub Codepoint);

impl OptionElem for CodepointOpt {
    fn elem_type_name() -> Vec<u8> {
        b"codepoint".to_vec()
    }
    fn elem_to_string(&self, q: Quoting) -> Vec<u8> {
        quoter(q)(&crate::utf8::encode(self.0))
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        if crate::utf8::distance(s) != 1 {
            return Err(rt_err!("'{}' is not a single codepoint", s));
        }
        Ok(CodepointOpt(crate::utf8::codepoint(s)))
    }
}
single_value_option!(CodepointOpt);

// Coord --------------------------------------------------------------------

impl OptionElem for DisplayCoord {
    fn elem_type_name() -> Vec<u8> {
        b"coord".to_vec()
    }
    fn elem_to_string(&self, _: Quoting) -> Vec<u8> {
        kformat!("{},{}", self.line, self.column)
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        let vals = crate::string_utils::split(s, b',');
        if vals.len() != 2 {
            return Err(Error::runtime("expected <line>,<column>"));
        }
        Ok(DisplayCoord::new(str_to_int(vals[0])?, str_to_int(vals[1])?))
    }
}
single_value_option!(DisplayCoord);

// Lists --------------------------------------------------------------------

impl<T: OptionElem> OptionType for Vec<T> {
    fn type_name() -> Vec<u8> {
        let mut n = T::elem_type_name();
        n.extend_from_slice(b"-list");
        n
    }
    fn to_string_q(&self, q: Quoting) -> Vec<u8> {
        join(self.iter().map(|t| t.elem_to_string(q)), b' ', false)
    }
    fn to_strings(&self) -> Vec<Vec<u8>> {
        self.iter().map(|t| t.elem_to_string(Quoting::Raw)).collect()
    }
    fn from_strings(strs: &[Vec<u8>]) -> Result<Self> {
        list_from_strings(strs)
    }
    fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        T::list_add_from_strings(self, strs)
    }
    fn remove_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        let mut did_remove = false;
        for s in strs {
            let val = T::elem_from_string(s)?;
            if let Some(pos) = self.iter().position(|x| *x == val) {
                self.remove(pos);
                did_remove = true;
            }
        }
        Ok(did_remove)
    }
}

// Maps ---------------------------------------------------------------------

pub type StringMap = IndexMap<Vec<u8>, Vec<u8>>;

// [FAITHFUL] keys and values only escape '=' when printed, but parsing
// unescapes both '=' and '\\', so backslashes do not round trip (as in C++
// option_types.hh, escape(str, '=', '\\') vs unescape<'=', '\\'>)
impl OptionType for StringMap {
    fn type_name() -> Vec<u8> {
        b"str-to-str-map".to_vec()
    }
    fn to_string_q(&self, q: Quoting) -> Vec<u8> {
        join(
            self.iter().map(|(k, v)| quoter(q)(&kformat!("{}={}", escape(k, b"=", b'\\'), escape(v, b"=", b'\\')))),
            b' ',
            false,
        )
    }
    fn to_strings(&self) -> Vec<Vec<u8>> {
        self.iter().map(|(k, v)| kformat!("{}={}", escape(k, b"=", b'\\'), escape(v, b"=", b'\\'))).collect()
    }
    fn from_strings(strs: &[Vec<u8>]) -> Result<Self> {
        let mut res = StringMap::new();
        res.add_from_strings(strs)?;
        Ok(res)
    }
    fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        let mut changed = false;
        for s in strs {
            let kv = split_key_value(s)?;
            self.insert(kv.0, kv.1);
            changed = true;
        }
        Ok(changed)
    }
    fn remove_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        let mut changed = false;
        for s in strs {
            let (k, v) = split_key_value(s)?;
            if let Some(cur) = self.get(&k) {
                if v.is_empty() || v == *cur {
                    self.shift_remove(&k);
                    changed = true;
                }
            }
        }
        Ok(changed)
    }
}

fn split_key_value(s: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let parts = split_escaped(s, b'=', Some(b'\\'));
    if parts.len() != 2 {
        return Err(Error::runtime("map option expects key=value"));
    }
    Ok((unescape(parts[0], b"=\\", b'\\'), unescape(parts[1], b"=\\", b'\\')))
}

// Tuples -------------------------------------------------------------------

pub const TUPLE_SEPARATOR: u8 = b'|';

/// [FAITHFUL] join drops leading empty elements, so a tuple whose first
/// element is empty does not parse back
pub fn tuple_to_string(quoting: Quoting, elems: &[Vec<u8>]) -> Vec<u8> {
    quoter(quoting)(&join(elems, TUPLE_SEPARATOR, true))
}

pub fn tuple_from_string(s: &[u8], n: usize) -> Result<Vec<Vec<u8>>> {
    let parts = split_escaped(s, TUPLE_SEPARATOR, Some(b'\\'));
    if parts.len() != n {
        return Err(Error::runtime(if parts.len() < n {
            "not enough elements in tuple"
        } else {
            "too many elements in tuple"
        }));
    }
    Ok(parts.into_iter().map(|p| unescape(p, &[TUPLE_SEPARATOR, b'\\'], b'\\')).collect())
}

/// (String, String, String) completion candidate
pub type CompletionCandidate = (Vec<u8>, Vec<u8>, Vec<u8>);

impl OptionElem for CompletionCandidate {
    fn elem_type_name() -> Vec<u8> {
        b"completion".to_vec()
    }
    fn elem_to_string(&self, q: Quoting) -> Vec<u8> {
        tuple_to_string(q, &[self.0.clone(), self.1.clone(), self.2.clone()])
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        let mut e = tuple_from_string(s, 3)?;
        let c = e.pop().unwrap();
        let b = e.pop().unwrap();
        let a = e.pop().unwrap();
        Ok((a, b, c))
    }
}

// Prefixed lists -----------------------------------------------------------

#[derive(Clone, PartialEq, Debug, Default)]
pub struct PrefixedList<P, T> {
    pub prefix: P,
    pub list: Vec<T>,
}

pub type TimestampedList<T> = PrefixedList<usize, T>;
pub type CompletionList = PrefixedList<Vec<u8>, CompletionCandidate>;

/// Customization of PrefixedList option types (names and update)
pub trait PrefixedListType: Sized {
    fn type_name() -> Vec<u8>;
    fn update(&mut self, _context: &Context) -> Result<()> {
        Err(Error::runtime("no update operation supported for this option type"))
    }
}

impl PrefixedListType for CompletionList {
    fn type_name() -> Vec<u8> {
        b"completions".to_vec()
    }
}

impl<P: OptionElem + Default, T: OptionElem> OptionType for PrefixedList<P, T>
where
    PrefixedList<P, T>: PrefixedListType,
{
    fn type_name() -> Vec<u8> {
        <Self as PrefixedListType>::type_name()
    }
    fn to_string_q(&self, q: Quoting) -> Vec<u8> {
        let mut res = self.prefix.elem_to_string(q);
        res.push(b' ');
        res.extend(join(self.list.iter().map(|t| t.elem_to_string(q)), b' ', false));
        res
    }
    fn to_strings(&self) -> Vec<Vec<u8>> {
        let mut res = vec![self.prefix.elem_to_string(Quoting::Raw)];
        res.extend(self.list.iter().map(|t| t.elem_to_string(Quoting::Raw)));
        res
    }
    fn from_strings(strs: &[Vec<u8>]) -> Result<Self> {
        if strs.is_empty() {
            return Ok(PrefixedList { prefix: P::default(), list: Vec::new() });
        }
        Ok(PrefixedList { prefix: P::elem_from_string(&strs[0])?, list: list_from_strings(&strs[1..])? })
    }
    fn add_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        T::list_add_from_strings(&mut self.list, strs)
    }
    fn remove_from_strings(&mut self, strs: &[Vec<u8>]) -> Result<bool> {
        let mut did_remove = false;
        for s in strs {
            let val = T::elem_from_string(s)?;
            if let Some(pos) = self.list.iter().position(|x| *x == val) {
                self.list.remove(pos);
                did_remove = true;
            }
        }
        Ok(did_remove)
    }
    fn update(&mut self, context: &Context) -> Result<()> {
        <Self as PrefixedListType>::update(self, context)
    }
}

// Enums and flags ----------------------------------------------------------

/// Port of enum_desc(): describes the string names of an enum or flags type
pub trait EnumDesc: Copy + PartialEq + 'static {
    fn desc() -> &'static [(Self, &'static str)];
}

pub fn enum_type_name<T: EnumDesc>(flags: bool) -> Vec<u8> {
    kformat!(
        "{}({})",
        if flags { "flags" } else { "enum" },
        join(T::desc().iter().map(|(_, n)| n.as_bytes()), b'|', true)
    )
}

pub fn enum_to_string<T: EnumDesc>(e: T) -> Vec<u8> {
    T::desc().iter().find(|(v, _)| *v == e).map(|(_, n)| n.as_bytes().to_vec()).unwrap_or_default()
}

pub fn enum_from_string<T: EnumDesc>(s: &[u8]) -> Result<T> {
    T::desc()
        .iter()
        .find(|(_, n)| n.as_bytes() == s)
        .map(|(v, _)| *v)
        .ok_or_else(|| rt_err!("invalid enum value '{}'", s))
}

/// Implements OptionType for an enum with an EnumDesc
#[macro_export]
macro_rules! enum_option {
    ($t:ty) => {
        impl $crate::option_types::OptionElem for $t {
            fn elem_type_name() -> Vec<u8> {
                $crate::option_types::enum_type_name::<$t>(false)
            }
            fn elem_to_string(&self, _: $crate::string_utils::Quoting) -> Vec<u8> {
                $crate::option_types::enum_to_string(*self)
            }
            fn elem_from_string(s: &[u8]) -> $crate::exception::Result<Self> {
                $crate::option_types::enum_from_string(s)
            }
        }
        $crate::single_value_option!($t);
    };
}

/// Implements OptionType for a kak_flags! type with an EnumDesc
#[macro_export]
macro_rules! flags_option {
    ($t:ty) => {
        impl $crate::option_types::OptionElem for $t {
            fn elem_type_name() -> Vec<u8> {
                $crate::option_types::enum_type_name::<$t>(true)
            }
            fn elem_to_string(&self, _: $crate::string_utils::Quoting) -> Vec<u8> {
                let mut res = Vec::new();
                for (v, n) in <$t as $crate::option_types::EnumDesc>::desc() {
                    if !self.has(*v) {
                        continue;
                    }
                    if !res.is_empty() {
                        res.push(b'|');
                    }
                    res.extend_from_slice(n.as_bytes());
                }
                res
            }
            fn elem_from_string(s: &[u8]) -> $crate::exception::Result<Self> {
                let mut flags = <$t>::empty();
                for f in $crate::string_utils::split(s, b'|') {
                    match <$t as $crate::option_types::EnumDesc>::desc().iter().find(|(_, n)| n.as_bytes() == f) {
                        Some((v, _)) => flags |= *v,
                        None => return Err($crate::rt_err!("invalid flag value '{}'", f)),
                    }
                }
                Ok(flags)
            }
        }
        impl $crate::option_types::OptionAdd for $t {
            fn option_add(&mut self, s: &[u8]) -> $crate::exception::Result<bool> {
                let old = *self;
                *self |= <$t as $crate::option_types::OptionElem>::elem_from_string(s)?;
                Ok(*self != old)
            }
            fn option_remove(&mut self, s: &[u8]) -> $crate::exception::Result<bool> {
                let old = *self;
                *self &= !<$t as $crate::option_types::OptionElem>::elem_from_string(s)?;
                Ok(*self != old)
            }
        }
        $crate::single_value_option!($t, add);
    };
}

impl EnumDesc for Quoting {
    fn desc() -> &'static [(Self, &'static str)] {
        crate::string_utils::QUOTING_DESC
    }
}
enum_option!(Quoting);

#[cfg(test)]
mod tests {
    use super::*;

    fn check<T: OptionType + std::fmt::Debug>(value: T, strs: &[&str]) {
        let strs: Vec<Vec<u8>> = strs.iter().map(|s| s.as_bytes().to_vec()).collect();
        assert_eq!(value.to_strings(), strs);
        assert_eq!(T::from_strings(&strs).unwrap(), value);
    }

    #[test]
    fn test_option_parsing() {
        check(123i32, &["123"]);
        check(true, &["true"]);
        check(vec![b"foo".to_vec(), b"bar:".to_vec(), b"baz".to_vec()], &["foo", "bar:", "baz"]);
        check(vec![10i32, 20, 30], &["10", "20", "30"]);
        let mut map = StringMap::new();
        map.insert(b"foo".to_vec(), b"10".to_vec());
        map.insert(b"b=r".to_vec(), b"20".to_vec());
        map.insert(b"b:z".to_vec(), b"30".to_vec());
        check(map, &["foo=10", "b\\=r=20", "b:z=30"]);
    }

    use crate::string_utils::{quote, shell_quote};
    use hegel::generators as gs;
    use hegel::{Generator as _, TestCase};

    fn bytes() -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(gs::binary(), gs::vecs(gs::sampled_from(b"ab =|\\' \n:".to_vec())))
    }

    fn no_backslash() -> impl hegel::PrintableGenerator<Vec<u8>> {
        bytes().filter(|s| !s.contains(&b'\\'))
    }

    fn roundtrip<T: OptionType + std::fmt::Debug>(v: &T) -> T {
        T::from_strings(&v.to_strings()).unwrap()
    }

    #[hegel::test(test_cases = 1000)]
    fn int_roundtrip(tc: TestCase) {
        let i = tc.draw(gs::integers::<i32>());
        assert_eq!(i.to_strings(), vec![i.to_string().into_bytes()]);
        assert_eq!(roundtrip(&i), i);
    }

    #[hegel::test(test_cases = 1000)]
    fn int_add_then_remove_restores(tc: TestCase) {
        let i = tc.draw(gs::integers::<i32>());
        let d = tc.draw(gs::integers::<i32>());
        let mut v = i;
        assert_eq!(v.add_from_strings(&[d.to_string().into_bytes()]).unwrap(), d != 0);
        assert_eq!(v, i.wrapping_add(d));
        assert_eq!(v.remove_from_strings(&[d.to_string().into_bytes()]).unwrap(), d != 0);
        assert_eq!(v, i);
    }

    #[hegel::test(test_cases = 1000)]
    fn int_rejects_non_numbers(tc: TestCase) {
        let s = tc.draw(bytes().filter(|s| crate::string_utils::str_to_int_ifp(s).is_none()));
        assert!(i32::from_strings(&[s]).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn usize_is_parsed_as_int(tc: TestCase) {
        let i = tc.draw(gs::integers::<i32>());
        assert_eq!(usize::elem_from_string(i.to_string().as_bytes()).unwrap(), i as usize);
    }

    #[hegel::test(test_cases = 1000)]
    fn bool_roundtrip(tc: TestCase) {
        let b = tc.draw(gs::booleans());
        assert_eq!(roundtrip(&b), b);
    }

    #[hegel::test(test_cases = 1000)]
    fn bool_accepts_only_true_yes_false_no(tc: TestCase) {
        let s = tc.draw(bytes());
        let expected = match &s[..] {
            b"true" | b"yes" => Some(true),
            b"false" | b"no" => Some(false),
            _ => None,
        };
        assert_eq!(bool::from_strings(&[s]).ok(), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn single_value_options_need_exactly_one_string(tc: TestCase) {
        let strs = tc.draw(gs::vecs(bytes()).filter(|v| v.len() != 1).map(|v| v.into_iter().take(3).collect::<Vec<_>>()));
        assert!(<Vec<u8> as OptionType>::from_strings(&strs).is_err());
        assert!(bool::from_strings(&strs).is_err());
        assert!(i32::from_strings(&strs).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn str_roundtrip(tc: TestCase) {
        let s = tc.draw(bytes());
        assert_eq!(roundtrip(&s), s);
    }

    #[hegel::test(test_cases = 1000)]
    fn str_quoting(tc: TestCase) {
        let s = tc.draw(bytes());
        assert_eq!(s.to_string_q(Quoting::Raw), s);
        assert_eq!(s.to_string_q(Quoting::Kakoune), quote(&s));
        assert_eq!(s.to_string_q(Quoting::Shell), shell_quote(&s));
    }

    #[hegel::test(test_cases = 1000)]
    fn str_add_appends(tc: TestCase) {
        let s = tc.draw(bytes());
        let t = tc.draw(bytes());
        let mut v = s.clone();
        assert_eq!(v.add_from_strings(std::slice::from_ref(&t)).unwrap(), !t.is_empty());
        assert_eq!(v, [s, t].concat());
    }

    #[hegel::test(test_cases = 1000)]
    fn str_list_roundtrip(tc: TestCase) {
        let l = tc.draw(gs::vecs(bytes()));
        assert_eq!(roundtrip(&l), l);
    }

    // lists print their quoted elements separated by spaces, unescaped
    #[hegel::test(test_cases = 1000)]
    fn str_list_to_string_joins_quoted_elements(tc: TestCase) {
        let l = tc.draw(gs::vecs(bytes()));
        let q = tc.draw(gs::sampled_from(vec![Quoting::Raw, Quoting::Kakoune, Quoting::Shell]));
        let quoted: Vec<Vec<u8>> = l.iter().map(|e| crate::string_utils::quoter(q)(e)).collect();
        assert_eq!(l.to_string_q(q), crate::string_utils::join(&quoted, b' ', false));
    }

    #[hegel::test(test_cases = 1000)]
    fn int_list_roundtrip(tc: TestCase) {
        let l = tc.draw(gs::vecs(gs::integers::<i32>()));
        assert_eq!(roundtrip(&l), l);
    }

    #[hegel::test(test_cases = 1000)]
    fn list_add_appends_and_remove_removes_first_occurrences(tc: TestCase) {
        let l = tc.draw(gs::vecs(gs::integers::<i32>().max_value(5).min_value(0)));
        let more = tc.draw(gs::vecs(gs::integers::<i32>().max_value(5).min_value(0)));
        let strs: Vec<Vec<u8>> = more.iter().map(|i| i.to_string().into_bytes()).collect();
        let mut v = l.clone();
        assert_eq!(v.add_from_strings(&strs).unwrap(), !more.is_empty());
        assert_eq!(v, [l.clone(), more.clone()].concat());
        let mut expected = l.clone();
        let mut removed = false;
        for m in &more {
            if let Some(p) = expected.iter().position(|x| x == m) {
                expected.remove(p);
                removed = true;
            }
        }
        let mut w = l.clone();
        assert_eq!(w.remove_from_strings(&strs).unwrap(), removed);
        assert_eq!(w, expected);
    }

    fn string_map(key: impl hegel::PrintableGenerator<Vec<u8>>, value: impl hegel::PrintableGenerator<Vec<u8>>)
        -> impl hegel::PrintableGenerator<StringMap> {
        gs::vecs(gs::tuples2(key, value)).map(|kvs| kvs.into_iter().collect::<StringMap>())
    }

    // C++ escapes only '=' in keys and values (escape(str, '=', '\\')) but
    // unescapes both '=' and '\\' when parsing, so backslashes do not round
    // trip ([FAITHFUL] C++ quirk): they are excluded here.
    #[hegel::test(test_cases = 1000)]
    fn str_map_roundtrip_without_backslashes(tc: TestCase) {
        let m = tc.draw(string_map(no_backslash(), no_backslash()));
        let back = roundtrip(&m);
        assert_eq!(back.iter().collect::<Vec<_>>(), m.iter().collect::<Vec<_>>());
    }

    #[hegel::test(test_cases = 1000)]
    fn str_map_last_value_wins(tc: TestCase) {
        let k = tc.draw(no_backslash());
        let v1 = tc.draw(no_backslash());
        let v2 = tc.draw(no_backslash());
        let entry = |v: &[u8]| [escape(&k, b"=", b'\\'), b"=".to_vec(), escape(v, b"=", b'\\')].concat();
        let m = StringMap::from_strings(&[entry(&v1), entry(&v2)]).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&k), Some(&v2));
    }

    #[hegel::test(test_cases = 1000)]
    fn str_map_requires_key_value_pairs(tc: TestCase) {
        let s = tc.draw(bytes());
        // number of unescaped '=' in s
        let mut seps = 0;
        let mut escaped = false;
        for &c in &s {
            if !escaped && c == b'=' {
                seps += 1;
            }
            escaped = !escaped && c == b'\\';
        }
        assert_eq!(StringMap::from_strings(std::slice::from_ref(&s)).is_ok(), seps == 1);
    }

    #[hegel::test(test_cases = 1000)]
    fn str_map_remove(tc: TestCase) {
        let m = tc.draw(string_map(gs::vecs(gs::sampled_from(b"ab".to_vec())).max_size(2), gs::vecs(gs::sampled_from(b"xy".to_vec())).max_size(2)));
        let k = tc.draw(gs::vecs(gs::sampled_from(b"ab".to_vec())).max_size(2));
        let v = tc.draw(gs::vecs(gs::sampled_from(b"xy".to_vec())).max_size(2));
        let mut r = m.clone();
        let changed = r.remove_from_strings(&[[&k[..], b"=", &v].concat()]).unwrap();
        let should_remove = m.get(&k).is_some_and(|cur| v.is_empty() || *cur == v);
        assert_eq!(changed, should_remove);
        let mut expected = m.clone();
        if should_remove {
            expected.shift_remove(&k);
        }
        assert_eq!(r.iter().collect::<Vec<_>>(), expected.iter().collect::<Vec<_>>());
    }

    // join() drops leading empty elements (the joiner is only added once the
    // result is non empty), so a candidate with an empty first element does
    // not round trip ([FAITHFUL] C++ quirk): the first element is non empty here.
    #[hegel::test(test_cases = 1000)]
    fn completion_candidate_roundtrip(tc: TestCase) {
        let c: CompletionCandidate =
            (tc.draw(bytes().filter(|s| !s.is_empty())), tc.draw(bytes()), tc.draw(bytes()));
        assert_eq!(CompletionCandidate::elem_from_string(&c.elem_to_string(Quoting::Raw)).unwrap(), c);
    }

    #[hegel::test(test_cases = 1000)]
    fn tuple_element_count_is_checked(tc: TestCase) {
        let elems = tc.draw(gs::vecs(bytes().filter(|s| !s.is_empty())).min_size(1).max_size(6));
        let s = crate::string_utils::join(&elems, TUPLE_SEPARATOR, true);
        let res = tuple_from_string(&s, 3);
        match elems.len() {
            3 => assert_eq!(res.unwrap(), elems),
            n => {
                let msg = res.unwrap_err().what().to_vec();
                let expected: &[u8] = if n < 3 { b"not enough elements in tuple" } else { b"too many elements in tuple" };
                assert_eq!(msg, expected);
            }
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn completion_list_roundtrip(tc: TestCase) {
        let prefix = tc.draw(bytes());
        let list = tc.draw(gs::vecs(gs::tuples3(bytes().filter(|s| !s.is_empty()), bytes(), bytes())));
        let l = CompletionList { prefix, list };
        assert_eq!(roundtrip(&l), l);
    }

    #[hegel::test(test_cases = 1000)]
    fn codepoint_roundtrip(tc: TestCase) {
        let c = tc.draw(gs::characters());
        let cp = CodepointOpt(c as u32);
        assert_eq!(roundtrip(&cp), cp);
    }

    // A lone invalid lead byte is one character, decoded sign extended like
    // C++ (char is signed), which then encodes to nothing.
    #[hegel::test(test_cases = 1000)]
    fn codepoint_from_invalid_byte_is_sign_extended(tc: TestCase) {
        let b = tc.draw(gs::integers::<u8>().min_value(0xC0));
        let cp = CodepointOpt::from_strings(&[vec![b]]).unwrap();
        assert_eq!(cp, CodepointOpt(b as i8 as i32 as u32));
        assert_eq!(cp.to_strings(), vec![Vec::<u8>::new()]);
    }

    #[hegel::test(test_cases = 1000)]
    fn codepoint_requires_single_char(tc: TestCase) {
        let s = tc.draw(gs::text().filter(|s| s.chars().count() != 1));
        assert!(CodepointOpt::from_strings(&[s.into_bytes()]).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn coord_roundtrip(tc: TestCase) {
        let c = DisplayCoord::new(tc.draw(gs::integers::<i32>()), tc.draw(gs::integers::<i32>()));
        assert_eq!(roundtrip(&c), c);
    }

    #[hegel::test(test_cases = 1000)]
    fn coord_requires_two_ints(tc: TestCase) {
        let parts = tc.draw(gs::vecs(gs::integers::<i32>()).filter(|v| v.len() != 2).map(|v| v.into_iter().take(4).collect::<Vec<_>>()));
        let s = parts.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        assert!(DisplayCoord::from_strings(&[s.into_bytes()]).is_err());
    }

    #[hegel::test(test_cases = 1000)]
    fn enum_roundtrip(tc: TestCase) {
        let q = tc.draw(gs::sampled_from(vec![Quoting::Raw, Quoting::Kakoune, Quoting::Shell]));
        assert_eq!(roundtrip(&q), q);
    }

    #[hegel::test(test_cases = 1000)]
    fn enum_rejects_unknown_names(tc: TestCase) {
        let s = tc.draw(bytes().filter(|s| !matches!(&s[..], b"raw" | b"kakoune" | b"shell")));
        assert!(Quoting::from_strings(&[s]).is_err());
    }

    crate::kak_flags! {
        struct TestFlags: u32 {
            const None = 0;
            const A = 1 << 0;
            const B = 1 << 1;
            const C = 1 << 2;
        }
    }
    impl EnumDesc for TestFlags {
        fn desc() -> &'static [(Self, &'static str)] {
            &[(TestFlags::A, "a"), (TestFlags::B, "bee"), (TestFlags::C, "c")]
        }
    }
    crate::flags_option!(TestFlags);

    fn test_flags() -> impl hegel::PrintableGenerator<TestFlags> {
        gs::integers::<u32>().max_value(7).map(TestFlags)
    }

    #[hegel::test(test_cases = 1000)]
    fn flags_roundtrip(tc: TestCase) {
        let f = tc.draw(test_flags());
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn flags_type_name() {
        assert_eq!(TestFlags::type_name(), b"flags(a|bee|c)");
        assert_eq!(Quoting::type_name(), b"enum(raw|kakoune|shell)");
    }

    #[hegel::test(test_cases = 1000)]
    fn flags_add_and_remove(tc: TestCase) {
        let f = tc.draw(test_flags());
        let g = tc.draw(test_flags());
        let gs_ = g.to_strings();
        let mut v = f;
        assert_eq!(v.add_from_strings(&gs_).unwrap(), (f | g) != f);
        assert_eq!(v, f | g);
        let mut w = f;
        assert_eq!(w.remove_from_strings(&gs_).unwrap(), (f & !g) != f);
        assert_eq!(w, f & !g);
    }

    #[hegel::test(test_cases = 1000)]
    fn flags_reject_unknown_names(tc: TestCase) {
        let names = tc.draw(gs::vecs(gs::sampled_from(vec!["a", "bee", "c", "b", "x", ""])).min_size(1));
        // an empty string is no flags (splitting it yields no element)
        let valid = names == [""] || names.iter().all(|n| matches!(*n, "a" | "bee" | "c"));
        assert_eq!(TestFlags::from_strings(&[names.join("|").into_bytes()]).is_ok(), valid);
    }
}
