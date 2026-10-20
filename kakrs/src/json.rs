//! Port of json.hh/json.cc

use crate::exception::{Error, Result};
use crate::string_utils::str_to_int;
use crate::unicode::is_blank;
use indexmap::IndexMap;
use std::borrow::Cow;

pub type JsonArray = Vec<JsonValue>;
pub type JsonObject = IndexMap<Vec<u8>, JsonValue>;

/// A parsed json value, replaces the type erased Value used by the C++ code
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum JsonValue {
    Int(i32),
    Bool(bool),
    String(Vec<u8>),
    Array(JsonArray),
    Object(JsonObject),
}

impl JsonValue {
    pub fn as_int(&self) -> Option<i32> {
        match self {
            JsonValue::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_string(&self) -> Option<&[u8]> {
        match self {
            JsonValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&JsonArray> {
        match self {
            JsonValue::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&JsonObject> {
        match self {
            JsonValue::Object(o) => Some(o),
            _ => None,
        }
    }
}

/// Serialization to json, replaces the to_json overload set
pub trait ToJson {
    fn to_json(&self) -> Vec<u8>;
}

impl ToJson for i32 {
    fn to_json(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }
}

impl ToJson for bool {
    fn to_json(&self) -> Vec<u8> {
        if *self { b"true".to_vec() } else { b"false".to_vec() }
    }
}

impl ToJson for [u8] {
    fn to_json(&self) -> Vec<u8> {
        let mut res = Vec::with_capacity(self.len() + 4);
        res.push(b'"');
        let mut it = 0;
        let end = self.len();
        while it != end {
            let next = self[it..]
                .iter()
                .position(|&c| c == b'\\' || c == b'"' || c <= 0x1F)
                .map_or(end, |p| p + it);

            res.extend_from_slice(&self[it..next]);
            if next == end {
                break;
            }

            let c = self[next];
            if c <= 0x1F {
                res.extend_from_slice(format!("\\u{:04x}", c).as_bytes());
            } else {
                res.extend_from_slice(&[b'\\', c]);
            }
            it = next + 1;
        }
        res.push(b'"');
        res
    }
}

impl ToJson for Vec<u8> {
    fn to_json(&self) -> Vec<u8> {
        self.as_slice().to_json()
    }
}

impl ToJson for str {
    fn to_json(&self) -> Vec<u8> {
        self.as_bytes().to_json()
    }
}

impl ToJson for Cow<'_, [u8]> {
    fn to_json(&self) -> Vec<u8> {
        self.as_ref().to_json()
    }
}

impl<T: ToJson + ?Sized> ToJson for &T {
    fn to_json(&self) -> Vec<u8> {
        (**self).to_json()
    }
}

/// json array of elements, the equivalent of to_json(ArrayView<const T>)
pub fn array_to_json<T: ToJson>(array: &[T]) -> Vec<u8> {
    let mut res = b"[".to_vec();
    for (i, elem) in array.iter().enumerate() {
        if i != 0 {
            res.extend_from_slice(b", ");
        }
        res.extend(elem.to_json());
    }
    res.push(b']');
    res
}

impl<T: ToJson> ToJson for [T] {
    fn to_json(&self) -> Vec<u8> {
        array_to_json(self)
    }
}

impl<T: ToJson> ToJson for Vec<T> {
    fn to_json(&self) -> Vec<u8> {
        array_to_json(self)
    }
}

impl<K: ToJson, V: ToJson> ToJson for IndexMap<K, V> {
    fn to_json(&self) -> Vec<u8> {
        let mut res = b"{".to_vec();
        for (i, (key, value)) in self.iter().enumerate() {
            if i != 0 {
                res.push(b',');
            }
            res.extend(key.to_json());
            res.extend_from_slice(b": ");
            res.extend(value.to_json());
        }
        res.push(b'}');
        res
    }
}

pub struct JsonResult {
    pub value: JsonValue,
    /// position in the parsed slice right after the value
    pub new_pos: usize,
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

/// The C++ code passes (signed) chars to is_blank(Codepoint)
fn is_blank_char(c: u8) -> bool {
    is_blank(c as i8 as i32 as u32)
}

fn skip_while(s: &[u8], pos: &mut usize, cond: impl Fn(u8) -> bool) -> bool {
    while *pos != s.len() && cond(s[*pos]) {
        *pos += 1;
    }
    *pos != s.len()
}

const MAX_PARSING_DEPTH: usize = 100;

/// Parse a json value starting at `pos` in `s`. Returns Ok(None) if the
/// input is incomplete.
#[allow(clippy::mut_range_bound)] // pos is moved past escapes inside the string loop
fn parse_json_impl(s: &[u8], mut pos: usize, depth: usize) -> Result<Option<JsonResult>> {
    let end = s.len();
    if !skip_while(s, &mut pos, is_blank_char) {
        return Ok(None);
    }

    if depth >= MAX_PARSING_DEPTH {
        return Err(Error::runtime("maximum parsing depth reached"));
    }

    if is_digit(s[pos]) || s[pos] == b'-' {
        let mut digit_end = pos + 1;
        skip_while(s, &mut digit_end, is_digit);
        return Ok(Some(JsonResult { value: JsonValue::Int(str_to_int(&s[pos..digit_end])?), new_pos: digit_end }));
    }
    if end - pos > 4 && &s[pos..pos + 4] == b"true" {
        return Ok(Some(JsonResult { value: JsonValue::Bool(true), new_pos: pos + 4 }));
    }
    if end - pos > 5 && &s[pos..pos + 5] == b"false" {
        return Ok(Some(JsonResult { value: JsonValue::Bool(false), new_pos: pos + 5 }));
    }
    if s[pos] == b'"' {
        let mut value = Vec::new();
        let mut escaped = false;
        pos += 1;
        for string_end in pos..end {
            if escaped {
                // the escaped character is taken literally, replacing the backslash
                escaped = false;
                value.extend_from_slice(&s[pos..string_end]);
                *value.last_mut().unwrap() = s[string_end];
                pos = string_end + 1;
                continue;
            }
            if s[string_end] == b'\\' {
                escaped = true;
            }
            if s[string_end] == b'"' {
                value.extend_from_slice(&s[pos..string_end]);
                return Ok(Some(JsonResult { value: JsonValue::String(value), new_pos: string_end + 1 }));
            }
        }
        return Ok(None);
    }
    if s[pos] == b'[' {
        let mut array = JsonArray::new();
        pos += 1;
        if pos == end {
            return Err(Error::runtime("unable to parse array"));
        }
        if s[pos] == b']' {
            return Ok(Some(JsonResult { value: JsonValue::Array(array), new_pos: pos + 1 }));
        }

        loop {
            let Some(JsonResult { value: element, new_pos }) = parse_json_impl(s, pos, depth + 1)? else {
                return Ok(None);
            };
            pos = new_pos;
            array.push(element);
            if !skip_while(s, &mut pos, is_blank_char) {
                return Ok(None);
            }

            if s[pos] == b',' {
                pos += 1;
            } else if s[pos] == b']' {
                return Ok(Some(JsonResult { value: JsonValue::Array(array), new_pos: pos + 1 }));
            } else {
                return Err(Error::runtime("unable to parse array, expected ',' or ']'"));
            }
        }
    }
    if s[pos] == b'{' {
        pos += 1;
        if pos == end {
            return Err(Error::runtime("unable to parse object"));
        }
        let mut object = JsonObject::new();
        if s[pos] == b'}' {
            return Ok(Some(JsonResult { value: JsonValue::Object(object), new_pos: pos + 1 }));
        }

        loop {
            let Some(JsonResult { value: name_value, new_pos: name_end }) = parse_json_impl(s, pos, depth + 1)? else {
                return Ok(None);
            };
            pos = name_end;
            // C++ asserts the name is a string
            let JsonValue::String(name) = name_value else {
                return Err(Error::runtime("unable to parse object, expected string key"));
            };
            if !skip_while(s, &mut pos, is_blank_char) {
                return Ok(None);
            }
            let c = s[pos];
            pos += 1;
            if c != b':' {
                return Err(Error::runtime("expected :"));
            }

            let Some(JsonResult { value: element, new_pos: element_end }) = parse_json_impl(s, pos, depth + 1)? else {
                return Ok(None);
            };
            pos = element_end;
            object.insert(name, element);
            if !skip_while(s, &mut pos, is_blank_char) {
                return Ok(None);
            }

            if s[pos] == b',' {
                pos += 1;
            } else if s[pos] == b'}' {
                return Ok(Some(JsonResult { value: JsonValue::Object(object), new_pos: pos + 1 }));
            } else {
                return Err(Error::runtime("unable to parse object, expected ',' or '}'"));
            }
        }
    }
    Err(Error::runtime("unable to parse json"))
}

/// Parse the json value at the start of `json`. Returns Ok(None) if `json`
/// does not contain a complete value (C++ returns an empty Value and a
/// null position).
pub fn parse_json(json: &[u8]) -> Result<Option<JsonResult>> {
    parse_json_impl(json, 0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_parser() {
        {
            let value =
                parse_json(br#"{ "jsonrpc": "2.0", "method": "keys", "params": [ "b", "l", "a", "h" ] }"#).unwrap();
            assert!(value.is_some());
        }

        {
            let value = parse_json(b"[10,20]").unwrap().unwrap().value;
            assert!(value.as_array().is_some());
            assert_eq!(value.as_array().unwrap()[1].as_int(), Some(20));
        }

        {
            let value = parse_json(b"-1").unwrap().unwrap().value;
            assert_eq!(value.as_int(), Some(-1));
        }

        {
            let value = parse_json(b"{}").unwrap().unwrap().value;
            assert!(value.as_object().is_some());
            assert!(value.as_object().unwrap().is_empty());
        }

        {
            let mut big_nested_array = vec![b'['; MAX_PARSING_DEPTH + 1];
            big_nested_array.extend(vec![b']'; MAX_PARSING_DEPTH + 1]);
            assert!(parse_json(&big_nested_array).is_err());
        }
    }

    #[test]
    fn test_json_parser_details() {
        // escapes take the next character literally
        let res = parse_json(br#""a\"b\\c\nd" rest"#).unwrap().unwrap();
        assert_eq!(res.value, JsonValue::String(b"a\"b\\cnd".to_vec()));
        assert_eq!(res.new_pos, 12);
        // incomplete input
        assert!(parse_json(br#"{ "jsonrpc": "2.0", "#).unwrap().is_none());
        assert!(parse_json(b"  ").unwrap().is_none());
        assert!(parse_json(br#""abc"#).unwrap().is_none());
        // true/false need a character after them
        assert!(parse_json(b"true").is_err());
        assert_eq!(parse_json(b"true ").unwrap().unwrap().value, JsonValue::Bool(true));
        assert_eq!(parse_json(b"false]").unwrap().unwrap().value, JsonValue::Bool(false));
        assert!(parse_json(b"x").is_err());
        assert!(parse_json(b"[1 2]").is_err());
        // non breaking space bytes are not blanks
        assert!(parse_json(b"\xa01").is_err());
    }

    #[test]
    fn test_to_json() {
        assert_eq!(true.to_json(), b"true");
        assert_eq!(false.to_json(), b"false");
        let mut map: IndexMap<Vec<u8>, Vec<i32>> = IndexMap::new();
        map.insert(b"foo".to_vec(), vec![1, 2, 3]);
        map.insert(b"\x1b".to_vec(), vec![3, 4, 5]);
        assert_eq!(map.to_json(), br#"{"foo": [1, 2, 3],"\u001b": [3, 4, 5]}"#);
        assert_eq!(b"a\"b\\c\n\x7f\xff"[..].to_json(), b"\"a\\\"b\\\\c\\u000a\x7f\xff\"");
    }
}
