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
}
