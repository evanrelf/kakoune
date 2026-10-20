//! Port of regex_vm.hh / regex_vm.cc

use crate::exception::{Error, ErrorKind, Result};
use crate::format::{Cp, Hex};
use crate::unicode::{is_blank, is_horizontal_blank, is_word_default, to_lower, to_upper};
use crate::units::{CharCount, Codepoint};
use crate::utf8::{ByteIt, it_previous, it_to_next, it_to_previous};
use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::ControlFlow;

fn regex_error(msg: impl Into<Vec<u8>>) -> Error {
    Error::new(ErrorKind::RegexError, msg)
}

macro_rules! regex_flags {
    ($(#[$meta:meta])* $name:ident: $t:ty { $($c:ident = $v:expr),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
        pub struct $name(pub $t);

        impl $name {
            $(pub const $c: $name = $name($v);)*
            /// true if any of the bits of `other` is set (C++ `flags & other`)
            #[inline]
            pub const fn contains(self, other: $name) -> bool {
                self.0 & other.0 != 0
            }
            #[inline]
            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }
        }
        impl std::ops::BitOr for $name {
            type Output = $name;
            #[inline]
            fn bitor(self, o: $name) -> $name { $name(self.0 | o.0) }
        }
        impl std::ops::BitOrAssign for $name {
            #[inline]
            fn bitor_assign(&mut self, o: $name) { self.0 |= o.0 }
        }
        impl std::ops::BitAnd for $name {
            type Output = $name;
            #[inline]
            fn bitand(self, o: $name) -> $name { $name(self.0 & o.0) }
        }
        impl std::ops::BitAndAssign for $name {
            #[inline]
            fn bitand_assign(&mut self, o: $name) { self.0 &= o.0 }
        }
        impl std::ops::Not for $name {
            type Output = $name;
            #[inline]
            fn not(self) -> $name { $name(!self.0) }
        }
    };
}

regex_flags!(CharacterType: u8 {
    NONE = 0,
    WHITESPACE = 1 << 0,
    HORIZONTAL_WHITESPACE = 1 << 1,
    WORD = 1 << 2,
    DIGIT = 1 << 3,
    NOT_WHITESPACE = 1 << 4,
    NOT_HORIZONTAL_WHITESPACE = 1 << 5,
    NOT_WORD = 1 << 6,
    NOT_DIGIT = 1 << 7,
});

#[allow(non_camel_case_types)]
type wint_t = i32;

unsafe extern "C" {
    fn iswdigit(c: wint_t) -> libc::c_int;
}

#[inline]
pub fn is_ctype(ctype: CharacterType, cp: Codepoint) -> bool {
    #[inline(always)]
    fn check(ctype: CharacterType, bit: CharacterType, not_bit: CharacterType, func: impl FnOnce() -> bool) -> bool {
        ctype.contains(bit | not_bit) && func() == ctype.contains(bit)
    }
    check(ctype, CharacterType::WORD, CharacterType::NOT_WORD, || is_word_default(cp))
        || check(ctype, CharacterType::WHITESPACE, CharacterType::NOT_WHITESPACE, || is_blank(cp))
        || check(ctype, CharacterType::HORIZONTAL_WHITESPACE, CharacterType::NOT_HORIZONTAL_WHITESPACE, || {
            is_horizontal_blank(cp)
        })
        || check(ctype, CharacterType::DIGIT, CharacterType::NOT_DIGIT, || unsafe { iswdigit(cp as wint_t) != 0 })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Range {
    pub min: Codepoint,
    pub max: Codepoint,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CharacterClass {
    pub ranges: Vec<Range>,
    pub ctypes: CharacterType,
    pub negative: bool,
    pub ignore_case: bool,
}

impl CharacterClass {
    #[inline]
    pub fn matches(&self, mut cp: Codepoint) -> bool {
        if self.ignore_case {
            cp = to_lower(cp);
        }

        for &Range { min, max } in &self.ranges {
            if cp < min {
                break;
            } else if cp <= max {
                return !self.negative;
            }
        }

        (self.ctypes != CharacterType::NONE && is_ctype(self.ctypes, cp)) != self.negative
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Match,
    Literal { codepoint: Codepoint, ignore_case: bool },
    AnyChar,
    AnyCharExceptNewLine,
    CharRange { min: u8, max: u8, ignore_case: bool, negative: bool },
    CharType(CharacterType),
    CharClass { index: i16 },
    Jump { offset: i16 },
    Split { offset: i16, prioritize_parent: bool },
    Save { index: i16 },
    LineAssertion { line_start: bool },
    SubjectAssertion { subject_begin: bool },
    WordBoundary { positive: bool },
    LookAround { index: i16, ahead: bool, positive: bool, ignore_case: bool },
}

impl Op {
    fn is_jump(&self) -> bool {
        matches!(self, Op::Jump { .. } | Op::Split { .. })
    }

    fn jump_offset(&self) -> i16 {
        match *self {
            Op::Jump { offset } | Op::Split { offset, .. } => offset,
            _ => 0,
        }
    }

    fn is_assertion(&self) -> bool {
        matches!(
            self,
            Op::LineAssertion { .. } | Op::SubjectAssertion { .. } | Op::WordBoundary { .. } | Op::LookAround { .. }
        )
    }
}

/// Values stored in `CompiledRegex::lookarounds`: either a literal codepoint
/// or one of these special values.
pub mod lookaround {
    use crate::units::Codepoint;
    pub const OP_BEGIN: Codepoint = 0xF0000;
    pub const ANY_CHAR: Codepoint = 0xF0000;
    pub const ANY_CHAR_EXCEPT_NEW_LINE: Codepoint = 0xF0001;
    pub const CHARACTER_CLASS: Codepoint = 0xF0002;
    pub const CHARACTER_TYPE: Codepoint = 0xF8000;
    pub const OP_END: Codepoint = 0xFFFFF;
    pub const END_OF_LOOKAROUND: Codepoint = u32::MAX;
}

#[derive(Clone, Debug)]
pub struct Instruction {
    pub op: Op,
    /// used during execution
    pub last_step: Cell<u16>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NamedCapture {
    pub name: Vec<u8>,
    pub index: u32,
}

#[derive(Clone, Debug)]
pub struct StartDesc {
    pub start_byte: u8,
    pub offset: u8,
    pub map: [bool; StartDesc::COUNT],
}

impl StartDesc {
    pub const COUNT: usize = 256;
    const OFFSET_MAX: i32 = u8::MAX as i32;
}

impl Default for StartDesc {
    fn default() -> Self {
        StartDesc { start_byte: 0, offset: 0, map: [false; StartDesc::COUNT] }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CompiledRegex {
    pub instructions: Vec<Instruction>,
    pub character_classes: Vec<CharacterClass>,
    pub lookarounds: Vec<Codepoint>,
    pub named_captures: Vec<NamedCapture>,
    /// u32::MAX if no backward support, 0 if only backward, >0 if both forward and backward
    pub first_backward_inst: u32,
    pub save_count: u32,

    pub forward_start_desc: Option<Box<StartDesc>>,
    pub backward_start_desc: Option<Box<StartDesc>>,
}

impl CompiledRegex {
    /// C++ `explicit operator bool`
    pub fn is_valid(&self) -> bool {
        !self.instructions.is_empty()
    }
}

regex_flags!(RegexCompileFlags: u8 {
    NONE = 0,
    NO_SUBS = 1 << 0,
    OPTIMIZE = 1 << 1,
    BACKWARD = 1 << 2,
    NO_FORWARD = 1 << 3,
});

regex_flags!(RegexExecFlags: u8 {
    NONE = 0,
    NOT_BEGIN_OF_LINE = 1 << 1,
    NOT_END_OF_LINE = 1 << 2,
    NOT_BEGIN_OF_WORD = 1 << 3,
    NOT_END_OF_WORD = 1 << 4,
    NOT_INITIAL_NULL = 1 << 5,
});

/// RegexMode bits, used as the const parameter of `Mode`.
pub struct RegexMode;

impl RegexMode {
    pub const FORWARD: u8 = 1 << 0;
    pub const BACKWARD: u8 = 1 << 1;
    pub const SEARCH: u8 = 1 << 2;
    pub const ANY_MATCH: u8 = 1 << 3;
    pub const NO_SAVES: u8 = 1 << 4;
}

pub const fn has_direction(mode: u8) -> bool {
    ((mode & RegexMode::FORWARD) != 0) ^ ((mode & RegexMode::BACKWARD) != 0)
}

pub const fn is_direction(mode: u8) -> bool {
    has_direction(mode) && (mode & !(RegexMode::FORWARD | RegexMode::BACKWARD)) == 0
}

/// Compile time execution mode of a ThreadedRegexVM
pub trait VmMode {
    const BITS: u8;
    const FORWARD: bool = Self::BITS & RegexMode::FORWARD != 0;
    const SEARCH: bool = Self::BITS & RegexMode::SEARCH != 0;
    const ANY_MATCH: bool = Self::BITS & RegexMode::ANY_MATCH != 0;
    const NO_SAVES: bool = Self::BITS & RegexMode::NO_SAVES != 0;
}

/// `Mode<{ RegexMode::FORWARD | RegexMode::SEARCH }>` is the equivalent of the
/// C++ `RegexMode::Forward | RegexMode::Search` template argument.
pub struct Mode<const M: u8>;

impl<const M: u8> VmMode for Mode<M> {
    const BITS: u8 = M;
}

pub type ForwardMode = Mode<{ RegexMode::FORWARD }>;
pub type BackwardMode = Mode<{ RegexMode::BACKWARD }>;
pub type ForwardSearch = Mode<{ RegexMode::FORWARD | RegexMode::SEARCH }>;
pub type BackwardSearch = Mode<{ RegexMode::BACKWARD | RegexMode::SEARCH }>;

// ---------------------------------------------------------------------------
// Parser

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ParsedOp {
    Literal,
    AnyChar,
    AnyCharExceptNewLine,
    CharClass,
    CharType,
    Sequence,
    Alternation,
    LineStart,
    LineEnd,
    WordBoundary,
    NotWordBoundary,
    SubjectBegin,
    SubjectEnd,
    ResetStart,
    LookAhead,
    NegativeLookAhead,
    LookBehind,
    NegativeLookBehind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Quantifier {
    min: i16,
    max: i16,
    greedy: bool,
}

impl Quantifier {
    const INFINITE: i16 = i16::MAX;

    const fn new(min: i16, max: i16) -> Self {
        Quantifier { min, max, greedy: true }
    }
    fn allows_none(&self) -> bool {
        self.min == 0
    }
    fn allows_infinite_repeat(&self) -> bool {
        self.max == Quantifier::INFINITE
    }
}

type NodeIndex = i32;

#[derive(Clone, Copy, Debug)]
struct Node {
    op: ParsedOp,
    ignore_case: bool,
    children_end: NodeIndex,
    value: Codepoint,
    quantifier: Quantifier,
}

#[derive(Default)]
struct ParsedRegex {
    nodes: Vec<Node>,
    character_classes: Vec<CharacterClass>,
    named_captures: Vec<NamedCapture>,
    capture_count: u32,
}

/// Iterates over the children of a node, forward or backward
struct Children<'a> {
    nodes: &'a [Node],
    pos: NodeIndex,
    end: NodeIndex,
    forward: bool,
}

impl<'a> Children<'a> {
    fn new(nodes: &'a [Node], index: NodeIndex, forward: bool) -> Self {
        let children_end = nodes[index as usize].children_end;
        Children {
            nodes,
            pos: if forward { index + 1 } else { Self::find_prev(nodes, index, children_end) },
            end: if forward { children_end } else { index },
            forward,
        }
    }

    fn find_prev(nodes: &[Node], parent: NodeIndex, pos: NodeIndex) -> NodeIndex {
        let mut child = parent + 1;
        if child == pos {
            return parent;
        }
        while nodes[child as usize].children_end != pos {
            child = nodes[child as usize].children_end;
        }
        child
    }
}

impl Iterator for Children<'_> {
    type Item = NodeIndex;
    fn next(&mut self) -> Option<NodeIndex> {
        if self.pos == self.end {
            return None;
        }
        let res = self.pos;
        self.pos =
            if self.forward { self.nodes[self.pos as usize].children_end } else { Self::find_prev(self.nodes, self.end, self.pos) };
        Some(res)
    }
}

const CHARACTER_CLASS_ESCAPES: [(u8, CharacterType); 8] = [
    (b'd', CharacterType::DIGIT),
    (b'D', CharacterType::NOT_DIGIT),
    (b'w', CharacterType::WORD),
    (b'W', CharacterType::NOT_WORD),
    (b's', CharacterType::WHITESPACE),
    (b'S', CharacterType::NOT_WHITESPACE),
    (b'h', CharacterType::HORIZONTAL_WHITESPACE),
    (b'H', CharacterType::NOT_HORIZONTAL_WHITESPACE),
];

const CONTROL_ESCAPES: [(u8, Codepoint); 5] = [(b'f', 0x0C), (b'n', 0x0A), (b'r', 0x0D), (b't', 0x09), (b'v', 0x0B)];

fn find_class_escape(cp: Codepoint) -> Option<CharacterType> {
    CHARACTER_CLASS_ESCAPES.iter().find(|(c, _)| *c as Codepoint == cp).map(|(_, t)| *t)
}

fn find_control_escape(cp: Codepoint) -> Option<Codepoint> {
    CONTROL_ESCAPES.iter().find(|(c, _)| *c as Codepoint == cp).map(|(_, v)| *v)
}

/// Equivalent of C++ `contains("...", cp)` on a char array literal, which
/// includes the terminating nul character.
fn contains_with_nul(chars: &[u8], cp: Codepoint) -> bool {
    cp == 0 || chars.iter().any(|&c| c as Codepoint == cp)
}

fn is_cp(cp: Codepoint, c: u8) -> bool {
    cp == c as Codepoint
}

const FLAG_IGNORE_CASE: u8 = 1 << 0;
const FLAG_DOT_MATCHES_NEW_LINE: u8 = 1 << 1;

// Recursive descent parser based on naming used in the ECMAScript
// standard, although the syntax is not fully compatible.
struct RegexParser<'r> {
    parsed_regex: ParsedRegex,
    regex: &'r [u8],
    pos: usize,
    flags: u8,
}

impl<'r> RegexParser<'r> {
    fn parse(re: &'r [u8]) -> Result<ParsedRegex> {
        let mut parser = RegexParser {
            parsed_regex: ParsedRegex { capture_count: 1, ..Default::default() },
            regex: re,
            pos: 0,
            flags: FLAG_DOT_MATCHES_NEW_LINE,
        };
        parser.parsed_regex.nodes.reserve(re.len());
        let root = parser.disjunction(0)?;
        debug_assert_eq!(root, 0);
        Ok(parser.parsed_regex)
    }

    fn invalid_utf8() -> Error {
        regex_error("Invalid utf8 in regex")
    }

    /// Codepoint at byte position pos. Like the C++ code, only reading at the
    /// end fails, invalid sequences are decoded with the Pass policy as
    /// read_codepoint does not forward the policy to read_codepoint_multibyte.
    fn cp_at(&self, pos: usize) -> Result<Codepoint> {
        if pos >= self.regex.len() {
            return Err(Self::invalid_utf8());
        }
        let mut it = crate::utf8::SliceIt::new(self.regex, pos);
        Ok(read_codepoint(&mut it, &crate::utf8::SliceIt::new(self.regex, self.regex.len())))
    }

    fn next_pos(&self, mut pos: usize) -> usize {
        let r = self.regex;
        if pos != r.len() {
            pos += 1;
        }
        while pos != r.len() && !crate::utf8::is_character_start(r[pos]) {
            pos += 1;
        }
        pos
    }

    /// *m_pos
    fn cur(&self) -> Result<Codepoint> {
        self.cp_at(self.pos)
    }

    /// ++m_pos
    fn advance(&mut self) {
        self.pos = self.next_pos(self.pos);
    }

    /// *m_pos++
    fn read(&mut self) -> Result<Codepoint> {
        let cp = self.cp_at(self.pos);
        self.advance();
        cp
    }

    fn at_end(&self) -> bool {
        self.pos == self.regex.len()
    }

    fn disjunction(&mut self, capture: u32) -> Result<NodeIndex> {
        let index = self.add_node(ParsedOp::Alternation, capture)?;
        loop {
            self.alternative(ParsedOp::Sequence)?;
            if self.at_end() || !is_cp(self.cur()?, b'|') {
                break;
            }
            self.advance();
        }
        self.parsed_regex.nodes[index as usize].children_end = self.parsed_regex.nodes.len() as NodeIndex;

        Ok(index)
    }

    fn alternative(&mut self, op: ParsedOp) -> Result<NodeIndex> {
        let index = self.add_node(op, u32::MAX)?;
        while self.term()?.is_some() {}
        self.parsed_regex.nodes[index as usize].children_end = self.parsed_regex.nodes.len() as NodeIndex;

        Ok(index)
    }

    fn term(&mut self) -> Result<Option<NodeIndex>> {
        while self.modifiers() {} // read all modifiers
        if let Some(node) = self.assertion()? {
            return Ok(Some(node));
        }
        if let Some(node) = self.atom()? {
            let quantifier = self.quantifier()?;
            self.parsed_regex.nodes[node as usize].quantifier = quantifier;
            return Ok(Some(node));
        }
        Ok(None)
    }

    fn modifiers(&mut self) -> bool {
        let r = self.regex;
        let mut it = self.pos;
        if r.len() - it >= 4 && r[it] == b'(' && r[it + 1] == b'?' {
            it += 2;
            loop {
                let Some(&m) = r.get(it) else { return false };
                it += 1;
                match m {
                    b'i' => self.flags |= FLAG_IGNORE_CASE,
                    b'I' => self.flags &= !FLAG_IGNORE_CASE,
                    b's' => self.flags |= FLAG_DOT_MATCHES_NEW_LINE,
                    b'S' => self.flags &= !FLAG_DOT_MATCHES_NEW_LINE,
                    b')' => {
                        self.pos = it;
                        return true;
                    }
                    _ => return false,
                }
            }
        }
        false
    }

    fn assertion(&mut self) -> Result<Option<NodeIndex>> {
        if self.at_end() {
            return Ok(None);
        }

        let cp = self.cur()?;
        if is_cp(cp, b'^') {
            self.advance();
            return Ok(Some(self.add_node(ParsedOp::LineStart, u32::MAX)?));
        }
        if is_cp(cp, b'$') {
            self.advance();
            return Ok(Some(self.add_node(ParsedOp::LineEnd, u32::MAX)?));
        }
        if is_cp(cp, b'\\') {
            let next = self.next_pos(self.pos);
            if next == self.regex.len() {
                return Ok(None);
            }
            let next_cp = self.cp_at(next)?;
            let op = match u8::try_from(next_cp) {
                Ok(b'b') => ParsedOp::WordBoundary,
                Ok(b'B') => ParsedOp::NotWordBoundary,
                Ok(b'A') => ParsedOp::SubjectBegin,
                Ok(b'z') => ParsedOp::SubjectEnd,
                Ok(b'K') => ParsedOp::ResetStart,
                _ => return Ok(None),
            };
            self.advance();
            self.advance();
            return Ok(Some(self.add_node(op, u32::MAX)?));
        }
        if is_cp(cp, b'(') {
            let r = self.regex;
            let mut it = self.pos + 1;
            if r.len() - it <= 2 || r[it] != b'?' {
                return Ok(None);
            }
            it += 1;

            let c = r[it];
            it += 1;
            let op = match c {
                b'=' => ParsedOp::LookAhead,
                b'!' => ParsedOp::NegativeLookAhead,
                b'<' => {
                    let c = r[it];
                    it += 1;
                    match c {
                        b'=' => ParsedOp::LookBehind,
                        b'!' => ParsedOp::NegativeLookBehind,
                        _ => return Ok(None),
                    }
                }
                _ => return Ok(None),
            };
            self.pos = it;
            let lookaround = self.alternative(op)?;
            let end_pos = self.pos;
            if self.at_end() || !is_cp(self.read()?, b')') {
                if is_cp(self.cp_at(end_pos)?, b'|') {
                    return Err(self.parse_error(b"Alternations cannot be used in lookarounds"));
                }
                return Err(self.parse_error(b"unclosed parenthesis"));
            }
            self.validate_lookaround(lookaround)?;
            return Ok(Some(lookaround));
        }
        Ok(None)
    }

    fn atom(&mut self) -> Result<Option<NodeIndex>> {
        if self.at_end() {
            return Ok(None);
        }

        let cp = self.cur()?;
        match u8::try_from(cp) {
            Ok(b'.') => {
                self.advance();
                let op = if self.flags & FLAG_DOT_MATCHES_NEW_LINE != 0 {
                    ParsedOp::AnyChar
                } else {
                    ParsedOp::AnyCharExceptNewLine
                };
                Ok(Some(self.add_node(op, u32::MAX)?))
            }
            Ok(b'(') => {
                let mut capture_group = u32::MAX;
                self.advance();
                let r = self.regex;
                let mut it = self.pos;
                if r.len() - it < 2 || r[it] != b'?' {
                    capture_group = self.parsed_regex.capture_count;
                    self.parsed_regex.capture_count += 1;
                } else {
                    it += 1;
                    if r[it] == b':' {
                        self.pos = it + 1;
                    } else if r[it] == b'<' {
                        it += 1;
                        let name_start = it;
                        // C++ converts the (signed) char to a Codepoint
                        while it != r.len() && is_word_default(r[it] as i8 as i32 as Codepoint) {
                            it += 1;
                        }
                        if it == r.len() || r[it] != b'>' {
                            return Err(self.parse_error(b"named captures should be only ascii word characters"));
                        }
                        capture_group = self.parsed_regex.capture_count;
                        self.parsed_regex.capture_count += 1;
                        self.parsed_regex
                            .named_captures
                            .push(NamedCapture { name: r[name_start..it].to_vec(), index: capture_group });
                        self.pos = it + 1;
                    }
                }

                let content = self.disjunction(capture_group)?;
                if self.at_end() || !is_cp(self.read()?, b')') {
                    return Err(self.parse_error(b"unclosed parenthesis"));
                }
                Ok(Some(content))
            }
            Ok(b'\\') => {
                self.advance();
                Ok(Some(self.atom_escape()?))
            }
            Ok(b'[') => {
                self.advance();
                Ok(Some(self.character_class()?))
            }
            Ok(b'|') | Ok(b')') => Ok(None),
            _ => {
                if b"^$.*+?[]{}".iter().any(|&c| is_cp(cp, c)) || (0xF0000..=0xFFFFF).contains(&cp) {
                    return Err(self.parse_error(&kformat!("unexpected '{}'", Cp(cp))));
                }
                self.advance();
                Ok(Some(self.add_node(ParsedOp::Literal, cp)?))
            }
        }
    }

    fn read_hex(&mut self, count: usize) -> Result<Codepoint> {
        let mut res: Codepoint = 0;
        for _ in 0..count {
            if self.at_end() {
                return Err(self.parse_error(b"unterminated hex sequence"));
            }
            let digit = self.read()?;
            let digit_value = if (b'0' as Codepoint..=b'9' as Codepoint).contains(&digit) {
                digit - b'0' as Codepoint
            } else if (b'a' as Codepoint..=b'f' as Codepoint).contains(&digit) {
                0xa + digit - b'a' as Codepoint
            } else if (b'A' as Codepoint..=b'F' as Codepoint).contains(&digit) {
                0xa + digit - b'A' as Codepoint
            } else {
                return Err(self.parse_error(&kformat!("invalid hex digit '{}'", Cp(digit))));
            };

            res = res.wrapping_mul(16).wrapping_add(digit_value);
        }
        Ok(res)
    }

    fn atom_escape(&mut self) -> Result<NodeIndex> {
        let cp = self.read()?;

        if is_cp(cp, b'N') {
            return self.add_node(ParsedOp::AnyCharExceptNewLine, u32::MAX);
        }

        if is_cp(cp, b'Q') {
            let escaped_sequence = self.add_node(ParsedOp::Sequence, u32::MAX)?;
            const END_MARK: &[u8] = b"\\E";

            let quote_end = crate::string_utils::find_subslice(&self.regex[self.pos..], END_MARK)
                .map(|p| p + self.pos)
                .unwrap_or(self.regex.len());
            while self.pos != quote_end {
                let cp = self.read()?;
                self.add_node(ParsedOp::Literal, cp)?;
            }
            self.parsed_regex.nodes[escaped_sequence as usize].children_end =
                self.parsed_regex.nodes.len() as NodeIndex;

            if quote_end != self.regex.len() {
                self.advance();
                self.advance();
            }

            return Ok(escaped_sequence);
        }

        // CharacterClassEscape
        if let Some(ctype) = find_class_escape(cp) {
            return self.add_node(ParsedOp::CharType, ctype.0 as Codepoint);
        }

        // CharacterEscape
        if let Some(value) = find_control_escape(cp) {
            return self.add_node(ParsedOp::Literal, value);
        }

        if is_cp(cp, b'0') {
            return self.add_node(ParsedOp::Literal, 0);
        } else if is_cp(cp, b'c') {
            if self.at_end() {
                return Err(self.parse_error(b"unterminated control escape"));
            }
            let ctrl = self.read()?;
            if (b'a' as Codepoint..=b'z' as Codepoint).contains(&ctrl)
                || (b'A' as Codepoint..=b'Z' as Codepoint).contains(&ctrl)
            {
                return self.add_node(ParsedOp::Literal, ctrl % 32);
            }
            return Err(self.parse_error(&kformat!("Invalid control escape character '{}'", Cp(ctrl))));
        } else if is_cp(cp, b'x') {
            let value = self.read_hex(2)?;
            return self.add_node(ParsedOp::Literal, value);
        } else if is_cp(cp, b'u') {
            let value = self.read_hex(6)?;
            return self.add_node(ParsedOp::Literal, value);
        }

        if contains_with_nul(b"^$\\.*+?()[]{}|", cp) {
            // SyntaxCharacter
            return self.add_node(ParsedOp::Literal, cp);
        }
        Err(self.parse_error(&kformat!("unknown atom escape '{}'", Cp(cp))))
    }

    fn normalize_ranges(ranges: &mut Vec<Range>) {
        if ranges.is_empty() {
            return;
        }

        // Sort ranges so that we can use binary search
        ranges.sort_by_key(|r| r.min);

        // merge overlapping ranges
        let mut pos = 0;
        for next in 1..ranges.len() {
            if ranges[pos].max.wrapping_add(1) >= ranges[next].min {
                if ranges[next].max > ranges[pos].max {
                    ranges[pos].max = ranges[next].max;
                }
            } else {
                pos += 1;
                ranges[pos] = ranges[next];
            }
        }
        ranges.truncate(pos + 1);
    }

    fn read_escaped_char(&mut self) -> Result<Codepoint> {
        let cp = self.read()?;
        if let Some(value) = find_control_escape(cp) {
            return Ok(value);
        }
        if is_cp(cp, b'x') {
            return self.read_hex(2);
        }
        if is_cp(cp, b'u') {
            return self.read_hex(6);
        }
        if !contains_with_nul(b"^$\\.*+?()[]{}|-", cp) {
            // SyntaxCharacter and -
            return Err(self.parse_error(&kformat!("unknown character class escape '{}'", Cp(cp))));
        }
        Ok(cp)
    }

    fn character_class(&mut self) -> Result<NodeIndex> {
        let mut character_class = CharacterClass {
            ignore_case: self.flags & FLAG_IGNORE_CASE != 0,
            negative: !self.at_end() && is_cp(self.cur()?, b'^'),
            ..Default::default()
        };
        if character_class.negative {
            self.advance();
        }

        const DASH: Range = Range { min: b'-' as Codepoint, max: b'-' as Codepoint };

        while !self.at_end() && !is_cp(self.cur()?, b']') {
            let mut cp = self.read()?;
            if is_cp(cp, b'-') {
                character_class.ranges.push(DASH);
                continue;
            }

            if self.at_end() {
                break;
            }

            if is_cp(cp, b'\\') {
                if let Some(ctype) = find_class_escape(self.cur()?) {
                    character_class.ctypes |= ctype;
                    self.advance();
                    continue;
                } else {
                    // its an escaped character
                    cp = self.read_escaped_char()?;
                }
            }

            let mut range = Range { min: cp, max: cp };
            if is_cp(self.cur()?, b'-') {
                self.advance();
                if self.at_end() {
                    break;
                }
                if !is_cp(self.cur()?, b']') {
                    cp = self.read()?;
                    if is_cp(cp, b'\\') {
                        cp = self.read_escaped_char()?;
                    }
                    range.max = cp;
                    if range.min > range.max {
                        return Err(self.parse_error(b"invalid range specified"));
                    }
                } else {
                    character_class.ranges.push(range);
                    range = DASH;
                }
            }
            character_class.ranges.push(range);
        }
        if self.at_end() {
            return Err(self.parse_error(b"unclosed character class"));
        }
        self.advance();

        if !character_class.ignore_case {
            let mut could_ignore_case = true;
            for &Range { min, max } in &character_class.ranges {
                if !character_class.ranges.contains(&Range { min: to_lower(min), max: to_lower(max) })
                    || !character_class.ranges.contains(&Range { min: to_upper(min), max: to_upper(max) })
                {
                    could_ignore_case = false;
                }
            }
            character_class.ignore_case = could_ignore_case;
        }

        if character_class.ignore_case {
            for range in &mut character_class.ranges {
                range.min = to_lower(range.min);
                range.max = to_lower(range.max);
            }
        }

        Self::normalize_ranges(&mut character_class.ranges);

        // Optimize the relatively common case of using a character class to
        // escape a character, such as [*]
        if character_class.ctypes == CharacterType::NONE
            && !character_class.negative
            && character_class.ranges.len() == 1
            && character_class.ranges[0].min == character_class.ranges[0].max
        {
            return self.add_node_full(
                ParsedOp::Literal,
                character_class.ranges[0].min,
                Quantifier::new(1, 1),
                character_class.ignore_case,
            );
        }

        if character_class.ctypes != CharacterType::NONE && !character_class.negative && character_class.ranges.is_empty() {
            return self.add_node(ParsedOp::CharType, character_class.ctypes.0 as Codepoint);
        }

        let classes = &mut self.parsed_regex.character_classes;
        let class_id = match classes.iter().position(|c| *c == character_class) {
            Some(id) => id,
            None => {
                classes.push(character_class);
                classes.len() - 1
            }
        };

        self.add_node(ParsedOp::CharClass, class_id as Codepoint)
    }

    fn read_bound(&mut self) -> Result<Option<i16>> {
        const MAX_REPEAT: i16 = 1000;
        let mut res: i16 = 0;
        let begin = self.pos;
        while !self.at_end() {
            let cp = self.cur()?;
            if cp < b'0' as Codepoint || cp > b'9' as Codepoint {
                return Ok(if self.pos == begin { None } else { Some(res) });
            }
            res = res * 10 + (cp - b'0' as Codepoint) as i16;
            if res > MAX_REPEAT {
                return Err(self.parse_error(&kformat!("Explicit quantifier is too big, maximum is {}", MAX_REPEAT as i32)));
            }
            self.advance();
        }
        Ok(Some(res))
    }

    fn check_greedy(&mut self) -> Result<bool> {
        if self.at_end() || !is_cp(self.cur()?, b'?') {
            return Ok(true);
        }
        self.advance();
        Ok(false)
    }

    fn quantifier(&mut self) -> Result<Quantifier> {
        if self.at_end() {
            return Ok(Quantifier::new(1, 1));
        }

        let quantifier = |min, max, greedy| Quantifier { min, max, greedy };
        match u8::try_from(self.cur()?) {
            Ok(b'*') => {
                self.advance();
                Ok(quantifier(0, Quantifier::INFINITE, self.check_greedy()?))
            }
            Ok(b'+') => {
                self.advance();
                Ok(quantifier(1, Quantifier::INFINITE, self.check_greedy()?))
            }
            Ok(b'?') => {
                self.advance();
                Ok(quantifier(0, 1, self.check_greedy()?))
            }
            Ok(b'{') => {
                self.advance();
                let min = self.read_bound()?.unwrap_or(0);
                let mut max = min;
                if is_cp(self.cur()?, b',') {
                    self.advance();
                    max = self.read_bound()?.unwrap_or(Quantifier::INFINITE);
                }
                if !is_cp(self.read()?, b'}') {
                    return Err(self.parse_error(b"expected closing bracket"));
                }
                Ok(quantifier(min, max, self.check_greedy()?))
            }
            _ => Ok(Quantifier::new(1, 1)),
        }
    }

    fn add_node(&mut self, op: ParsedOp, value: Codepoint) -> Result<NodeIndex> {
        self.add_node_full(op, value, Quantifier::new(1, 1), false)
    }

    fn add_node_full(
        &mut self,
        op: ParsedOp,
        value: Codepoint,
        quantifier: Quantifier,
        ignore_case: bool,
    ) -> Result<NodeIndex> {
        const MAX_NODES: usize = i16::MAX as usize;
        let res = self.parsed_regex.nodes.len();
        if res == MAX_NODES {
            return Err(self.parse_error(&kformat!("regex parsed to more than {} ast nodes", MAX_NODES)));
        }
        let next = (res + 1) as NodeIndex;
        self.parsed_regex.nodes.push(Node {
            op,
            ignore_case: ignore_case || self.flags & FLAG_IGNORE_CASE != 0,
            children_end: next,
            value,
            quantifier,
        });
        Ok(res as NodeIndex)
    }

    fn parse_error(&self, error: &[u8]) -> Error {
        let pos = self.pos.min(self.regex.len());
        regex_error(kformat!(
            "regex parse error: {} at '{}<<<HERE>>>{}'",
            error,
            &self.regex[..pos],
            &self.regex[pos..]
        ))
    }

    fn validate_lookaround(&self, index: NodeIndex) -> Result<()> {
        for child_index in Children::new(&self.parsed_regex.nodes, index, true) {
            let child = &self.parsed_regex.nodes[child_index as usize];
            if !matches!(
                child.op,
                ParsedOp::Literal
                    | ParsedOp::CharClass
                    | ParsedOp::CharType
                    | ParsedOp::AnyChar
                    | ParsedOp::AnyCharExceptNewLine
            ) {
                return Err(self.parse_error(b"Lookaround can only contain literals, any chars or character classes"));
            }
            if child.op == ParsedOp::Literal
                && lookaround::OP_BEGIN <= child.value
                && child.value < lookaround::OP_END
            {
                return Err(self.parse_error(b"Lookaround does not support literals codepoint between 0xF0000 and 0xFFFFD"));
            }
            if child.quantifier != Quantifier::new(1, 1) {
                return Err(self.parse_error(b"Quantifiers cannot be used in lookarounds"));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Compiler

type OpIndex = i32;

struct RegexCompiler {
    program: CompiledRegex,
    flags: RegexCompileFlags,
    parsed_regex: ParsedRegex,
}

impl RegexCompiler {
    fn compile(parsed_regex: ParsedRegex, flags: RegexCompileFlags) -> Result<CompiledRegex> {
        debug_assert!(!flags.contains(RegexCompileFlags::NO_FORWARD) || flags.contains(RegexCompileFlags::BACKWARD));
        let mut compiler = RegexCompiler { program: CompiledRegex::default(), flags, parsed_regex };
        // Approximation of the number of instructions generated
        compiler.program.instructions.reserve(
            (compiler.parsed_regex.nodes.len() + 1)
                * if flags.contains(RegexCompileFlags::BACKWARD) && !flags.contains(RegexCompileFlags::NO_FORWARD) {
                    2
                } else {
                    1
                },
        );

        if !flags.contains(RegexCompileFlags::NO_FORWARD) {
            compiler.program.forward_start_desc = compiler.compute_start_desc_for(true);
            compiler.compile_node(0, true)?;
            compiler.optimize(0, compiler.program.instructions.len());
            compiler.push_inst(Op::Match)?;
        }

        if flags.contains(RegexCompileFlags::BACKWARD) {
            compiler.program.first_backward_inst = compiler.program.instructions.len() as u32;
            compiler.program.backward_start_desc = compiler.compute_start_desc_for(false);
            compiler.compile_node(0, false)?;
            compiler.optimize(compiler.program.first_backward_inst as usize, compiler.program.instructions.len());
            compiler.push_inst(Op::Match)?;
        } else {
            compiler.program.first_backward_inst = u32::MAX;
        }

        let mut program = compiler.program;
        program.character_classes = std::mem::take(&mut compiler.parsed_regex.character_classes);
        program.named_captures = std::mem::take(&mut compiler.parsed_regex.named_captures);
        program.save_count = compiler.parsed_regex.capture_count * 2;
        Ok(program)
    }

    fn node(&self, index: NodeIndex) -> Node {
        self.parsed_regex.nodes[index as usize]
    }

    fn children(&self, index: NodeIndex, forward: bool) -> Children<'_> {
        Children::new(&self.parsed_regex.nodes, index, forward)
    }

    fn compile_node_inner(&mut self, index: NodeIndex, forward: bool) -> Result<OpIndex> {
        let node = self.node(index);

        let start_pos = self.op_count();
        let ignore_case = node.ignore_case;

        let save = (node.op == ParsedOp::Alternation || node.op == ParsedOp::Sequence)
            && (node.value == 0 || (node.value != u32::MAX && !self.flags.contains(RegexCompileFlags::NO_SUBS)));
        if save {
            self.push_inst(Op::Save { index: (node.value * 2 + if forward { 0 } else { 1 }) as i16 })?;
        }

        let mut goto_inner_end_offsets: Vec<OpIndex> = Vec::new();
        match node.op {
            ParsedOp::Literal => {
                let codepoint = if ignore_case { to_lower(node.value) } else { node.value };
                self.push_inst(Op::Literal { codepoint: codepoint & 0xFFFFFF, ignore_case })?;
            }
            ParsedOp::AnyChar => {
                self.push_inst(Op::AnyChar)?;
            }
            ParsedOp::AnyCharExceptNewLine => {
                self.push_inst(Op::AnyCharExceptNewLine)?;
            }
            ParsedOp::CharClass => {
                let char_class = &self.parsed_regex.character_classes[node.value as usize];
                let op = if char_class.ranges.len() == 1
                    && char_class.ctypes == CharacterType::NONE
                    && char_class.ranges[0].max <= u8::MAX as Codepoint
                {
                    Op::CharRange {
                        min: char_class.ranges[0].min as u8,
                        max: char_class.ranges[0].max as u8,
                        ignore_case: char_class.ignore_case,
                        negative: char_class.negative,
                    }
                } else {
                    Op::CharClass { index: node.value as i16 }
                };
                self.push_inst(op)?;
            }
            ParsedOp::CharType => {
                self.push_inst(Op::CharType(CharacterType(node.value as u8)))?;
            }
            ParsedOp::Sequence => {
                let children: Vec<NodeIndex> = self.children(index, forward).collect();
                for child in children {
                    self.compile_node(child, forward)?;
                }
            }
            ParsedOp::Alternation => {
                let children: Vec<NodeIndex> = self.children(index, true).collect();
                for &child in &children {
                    if child != index + 1 {
                        self.push_inst(Op::Split { offset: 0, prioritize_parent: false })?;
                    }
                }
                let mut split_pos = self.op_count();

                let end = node.children_end;
                for &child in &children {
                    let node_pos = self.compile_node(child, forward)?;
                    if child != index + 1 {
                        split_pos -= 1;
                        self.program.instructions[split_pos as usize].op =
                            Op::Split { offset: Self::offset(node_pos, split_pos), prioritize_parent: true };
                    }
                    if self.node(child).children_end != end {
                        let jump = self.push_inst(Op::Jump { offset: 0 })?;
                        goto_inner_end_offsets.push(jump);
                    }
                }
            }
            ParsedOp::LookAhead | ParsedOp::NegativeLookAhead => {
                let la_index = self.push_lookaround(index, true, ignore_case);
                self.push_inst(Op::LookAround {
                    index: la_index,
                    ahead: true,
                    positive: node.op == ParsedOp::LookAhead,
                    ignore_case,
                })?;
            }
            ParsedOp::LookBehind | ParsedOp::NegativeLookBehind => {
                let la_index = self.push_lookaround(index, false, ignore_case);
                self.push_inst(Op::LookAround {
                    index: la_index,
                    ahead: false,
                    positive: node.op == ParsedOp::LookBehind,
                    ignore_case,
                })?;
            }
            ParsedOp::LineStart => {
                self.push_inst(Op::LineAssertion { line_start: true })?;
            }
            ParsedOp::LineEnd => {
                self.push_inst(Op::LineAssertion { line_start: false })?;
            }
            ParsedOp::WordBoundary => {
                self.push_inst(Op::WordBoundary { positive: true })?;
            }
            ParsedOp::NotWordBoundary => {
                self.push_inst(Op::WordBoundary { positive: false })?;
            }
            ParsedOp::SubjectBegin => {
                self.push_inst(Op::SubjectAssertion { subject_begin: true })?;
            }
            ParsedOp::SubjectEnd => {
                self.push_inst(Op::SubjectAssertion { subject_begin: false })?;
            }
            ParsedOp::ResetStart => {
                self.push_inst(Op::Save { index: 0 })?;
            }
        }

        let op_count = self.op_count();
        for index in goto_inner_end_offsets {
            self.program.instructions[index as usize].op = Op::Jump { offset: Self::offset(op_count, index) };
        }

        if save {
            self.push_inst(Op::Save { index: (node.value * 2 + if forward { 1 } else { 0 }) as i16 })?;
        }

        Ok(start_pos)
    }

    fn op_count(&self) -> OpIndex {
        self.program.instructions.len() as OpIndex
    }

    fn offset(to: OpIndex, from: OpIndex) -> i16 {
        (to - from) as i16
    }

    fn set_split_offset(&mut self, index: OpIndex, new_offset: i16) {
        if let Op::Split { offset, .. } = &mut self.program.instructions[index as usize].op {
            *offset = new_offset;
        }
    }

    fn compile_node(&mut self, index: NodeIndex, forward: bool) -> Result<OpIndex> {
        let node = self.node(index);

        let start_pos = self.op_count();
        let mut goto_ends: Vec<OpIndex> = Vec::new();

        let quantifier = node.quantifier;

        if quantifier.allows_none() {
            let split_pos = self.push_inst(Op::Split { offset: 0, prioritize_parent: quantifier.greedy })?;
            goto_ends.push(split_pos);
        }

        let mut inner_pos = self.compile_node_inner(index, forward)?;
        // Write the node multiple times when we have a min count quantifier
        for _ in 1..quantifier.min {
            inner_pos = self.compile_node_inner(index, forward)?;
        }

        if quantifier.allows_infinite_repeat() {
            let offset = Self::offset(inner_pos, self.op_count());
            self.push_inst(Op::Split { offset, prioritize_parent: !quantifier.greedy })?;
        } else {
            // Write the node as an optional match for the min -> max counts
            for _ in quantifier.min.max(1)..quantifier.max {
                let split_pos = self.push_inst(Op::Split { offset: 0, prioritize_parent: quantifier.greedy })?;
                goto_ends.push(split_pos);
                self.compile_node_inner(index, forward)?;
            }
        }

        let op_count = self.op_count();
        for index in goto_ends {
            self.set_split_offset(index, Self::offset(op_count, index));
        }

        Ok(start_pos)
    }

    fn push_inst(&mut self, op: Op) -> Result<OpIndex> {
        const MAX_INSTRUCTIONS: OpIndex = i16::MAX as OpIndex;
        let res = self.op_count();
        if res >= MAX_INSTRUCTIONS {
            return Err(regex_error(kformat!("regex compiled to more than {} instructions", MAX_INSTRUCTIONS)));
        }
        self.program.instructions.push(Instruction { op, last_step: Cell::new(0) });
        Ok(res)
    }

    fn push_lookaround(&mut self, index: NodeIndex, forward: bool, ignore_case: bool) -> i16 {
        let res = self.program.lookarounds.len() as i16;
        let children: Vec<NodeIndex> = self.children(index, forward).collect();
        for child in children {
            let character = self.node(child);
            let value = match character.op {
                ParsedOp::Literal => {
                    if ignore_case {
                        to_lower(character.value)
                    } else {
                        character.value
                    }
                }
                ParsedOp::AnyChar => lookaround::ANY_CHAR,
                ParsedOp::AnyCharExceptNewLine => lookaround::ANY_CHAR_EXCEPT_NEW_LINE,
                ParsedOp::CharClass => lookaround::CHARACTER_CLASS.wrapping_add(character.value),
                ParsedOp::CharType => lookaround::CHARACTER_TYPE | character.value,
                _ => unreachable!("invalid lookaround content"),
            };
            self.program.lookarounds.push(value);
        }
        self.program.lookarounds.push(lookaround::END_OF_LOOKAROUND);
        res
    }

    // Mutate start_desc with informations on which Codepoint could start a match.
    // Returns true if the subsequent nodes are still relevant for computing the
    // start desc
    fn compute_start_desc(&self, index: NodeIndex, start_desc: &mut StartDesc, forward: bool) -> bool {
        // fill all bytes that mark the start of an utf8 multi byte sequence
        let add_multi_byte_utf8 = |start_desc: &mut StartDesc| {
            for b in &mut start_desc.map[0b11000000..0b11111000] {
                *b = true;
            }
        };
        const SINGLE_BYTE_LIMIT: Codepoint = 128;

        let node = self.node(index);
        match node.op {
            ParsedOp::Literal => {
                if node.value < SINGLE_BYTE_LIMIT {
                    if node.ignore_case {
                        start_desc.map[to_lower(node.value) as usize] = true;
                        start_desc.map[to_upper(node.value) as usize] = true;
                    } else {
                        start_desc.map[node.value as usize] = true;
                    }
                } else {
                    add_multi_byte_utf8(start_desc);
                }
                node.quantifier.allows_none()
            }
            ParsedOp::AnyChar => {
                if start_desc.offset as i32 + node.quantifier.max as i32 <= StartDesc::OFFSET_MAX {
                    start_desc.offset += node.quantifier.max as u8;
                    return true;
                }
                start_desc.map.fill(true);
                node.quantifier.allows_none()
            }
            ParsedOp::AnyCharExceptNewLine => {
                if start_desc.offset as i32 + node.quantifier.max as i32 <= StartDesc::OFFSET_MAX {
                    start_desc.offset += node.quantifier.max as u8;
                    return true;
                }
                for cp in 0..SINGLE_BYTE_LIMIT {
                    if cp != b'\n' as Codepoint {
                        start_desc.map[cp as usize] = true;
                    }
                }
                node.quantifier.allows_none()
            }
            ParsedOp::CharClass => {
                let character_class = &self.parsed_regex.character_classes[node.value as usize];
                if character_class.ctypes == CharacterType::NONE
                    && !character_class.negative
                    && !character_class.ignore_case
                {
                    for range in &character_class.ranges {
                        let clamp = |cp: Codepoint| SINGLE_BYTE_LIMIT.min(cp);
                        for cp in clamp(range.min)..clamp(range.max.wrapping_add(1)) {
                            start_desc.map[cp as usize] = true;
                        }
                        if range.max >= SINGLE_BYTE_LIMIT {
                            add_multi_byte_utf8(start_desc);
                        }
                    }
                } else {
                    for cp in 0..SINGLE_BYTE_LIMIT {
                        if start_desc.map[cp as usize] || character_class.matches(cp) {
                            start_desc.map[cp as usize] = true;
                        }
                    }
                }
                add_multi_byte_utf8(start_desc);
                node.quantifier.allows_none()
            }
            ParsedOp::CharType => {
                let ctype = CharacterType(node.value as u8);
                for cp in 0..SINGLE_BYTE_LIMIT {
                    if is_ctype(ctype, cp) {
                        start_desc.map[cp as usize] = true;
                    }
                }
                add_multi_byte_utf8(start_desc);
                node.quantifier.allows_none()
            }
            ParsedOp::Sequence => {
                for child in self.children(index, forward) {
                    if !self.compute_start_desc(child, start_desc, forward) {
                        return node.quantifier.allows_none();
                    }
                }
                true
            }
            ParsedOp::Alternation => {
                let mut all_consumed = !node.quantifier.allows_none();
                for child in self.children(index, true) {
                    if self.compute_start_desc(child, start_desc, forward) {
                        all_consumed = false;
                    }
                }
                !all_consumed
            }
            ParsedOp::LineStart
            | ParsedOp::LineEnd
            | ParsedOp::WordBoundary
            | ParsedOp::NotWordBoundary
            | ParsedOp::SubjectBegin
            | ParsedOp::SubjectEnd
            | ParsedOp::ResetStart
            | ParsedOp::LookAhead
            | ParsedOp::LookBehind
            | ParsedOp::NegativeLookAhead
            | ParsedOp::NegativeLookBehind => true,
        }
    }

    fn compute_start_desc_for(&self, forward: bool) -> Option<Box<StartDesc>> {
        let mut start_desc = StartDesc::default();
        if self.compute_start_desc(0, &mut start_desc, forward) || !start_desc.map.contains(&false) {
            return None;
        }

        if start_desc.map.iter().filter(|&&b| b).count() == 1 {
            start_desc.start_byte = start_desc.map.iter().position(|&b| b).unwrap() as u8;
        }

        Some(Box::new(start_desc))
    }

    fn optimize(&mut self, begin: usize, end: usize) {
        if !self.flags.contains(RegexCompileFlags::OPTIMIZE) {
            return;
        }

        let instructions = &mut self.program.instructions;
        for i in begin..end {
            let op = instructions[i].op;
            if op.is_jump() {
                let target = (i as isize + op.jump_offset() as isize) as usize;
                // the target can be one past the end, before the Match instruction is pushed
                if let Some(inst) = instructions.get(target) {
                    inst.last_step.set(0xffff); // tag as jump target
                }
            }
        }

        let mut block_begin = begin;
        while block_begin < end {
            let mut block_end = block_begin + 1;
            while block_end < end
                && !instructions[block_end].op.is_jump()
                && instructions[block_end].last_step.get() != 0xffff
            {
                block_end += 1;
            }
            Self::peephole_optimize(instructions, block_begin, block_end);
            block_begin = block_end;
        }
    }

    fn peephole_optimize(instructions: &mut [Instruction], begin: usize, end: usize) {
        // Move saves after all assertions on the same character
        let mut i = begin;
        while i + 1 < end {
            if matches!(instructions[i].op, Op::Save { .. }) && instructions[i + 1].op.is_assertion() {
                instructions.swap(i, i + 1);
            }
            i += 1;
        }
    }
}

pub fn dump_regex(program: &CompiledRegex) -> Vec<u8> {
    let mut res = Vec::new();
    for (index, inst) in program.instructions.iter().enumerate() {
        let index = index as i32;
        res.extend(kformat!(" {:03}     ", index));
        match inst.op {
            Op::Literal { codepoint, ignore_case } => {
                res.extend(kformat!("literal {}{}\n", if ignore_case { "(ignore case) " } else { "" }, codepoint));
            }
            Op::AnyChar => res.extend_from_slice(b"any char\n"),
            Op::AnyCharExceptNewLine => res.extend_from_slice(b"anything but newline\n"),
            Op::CharRange { min, max, ignore_case, negative } => {
                res.extend(kformat!(
                    "character range {}[{}{}-{}]\n",
                    if ignore_case { "(ignore case) " } else { "" },
                    if negative { "^" } else { "" },
                    min as i32,
                    max as i32
                ));
            }
            Op::CharType(ctype) => res.extend(kformat!("character type {}\n", ctype.0 as i32)),
            Op::CharClass { index } => res.extend(kformat!("character class {}\n", index as i32)),
            Op::Jump { offset } => {
                res.extend(kformat!("jump {} ({:03})\n", offset as i32, index + offset as i32));
            }
            Op::Split { offset, prioritize_parent } => {
                res.extend(kformat!(
                    "split (prioritize {}) {} ({:03})\n",
                    if prioritize_parent { "parent" } else { "child" },
                    offset as i32,
                    index + offset as i32
                ));
            }
            Op::Save { index } => res.extend(kformat!("save {}\n", index as i32)),
            Op::LineAssertion { line_start } => {
                res.extend(kformat!("line {}\n", if line_start { "start" } else { "end" }));
            }
            Op::SubjectAssertion { subject_begin } => {
                res.extend(kformat!("subject {}\n", if subject_begin { "begin" } else { "end" }));
            }
            Op::WordBoundary { positive } => {
                res.extend(kformat!("{}word boundary\n", if positive { "" } else { "not " }));
            }
            Op::LookAround { index: la_index, ahead, positive, ignore_case } => {
                let mut name = Vec::new();
                name.extend_from_slice(if positive { b"" as &[u8] } else { b"negative " });
                name.extend_from_slice(b"look ");
                name.extend_from_slice(if ahead { b"ahead " as &[u8] } else { b"behind " });
                if ignore_case {
                    name.extend_from_slice(b" (ignore case)");
                }

                let mut str = Vec::new();
                for &cp in program.lookarounds[la_index as usize..]
                    .iter()
                    .take_while(|&&cp| cp != lookaround::END_OF_LOOKAROUND)
                {
                    crate::utf8::dump(&mut str, cp);
                }
                res.extend(kformat!("{} ({})\n", name, str));
            }
            Op::Match => res.extend_from_slice(b"match\n"),
        }
    }
    let dump_start_desc = |res: &mut Vec<u8>, desc: &StartDesc, name: &str| {
        res.extend_from_slice(name.as_bytes());
        res.extend_from_slice(b" start desc: [");
        for c in 0..StartDesc::COUNT {
            if desc.map[c] {
                if c < 32 {
                    res.extend(kformat!("<0x{}>", Hex(c)));
                } else {
                    res.push(c as u8);
                }
            }
        }
        res.extend(kformat!("]+{}\n", desc.offset as i32));
    };
    if let Some(desc) = &program.forward_start_desc {
        dump_start_desc(&mut res, desc, "forward");
    }
    if let Some(desc) = &program.backward_start_desc {
        dump_start_desc(&mut res, desc, "backward");
    }
    res
}

pub fn compile_regex(re: &[u8], flags: RegexCompileFlags) -> Result<CompiledRegex> {
    RegexCompiler::compile(RegexParser::parse(re)?, flags)
}

// ---------------------------------------------------------------------------
// Execution

/// utf8 decoding matching the C++ one with the Pass invalid policy: invalid
/// lead bytes are returned as a sign extended char.
#[inline(always)]
fn read_codepoint<I: ByteIt>(it: &mut I, end: &I) -> Codepoint {
    if it == end {
        return u32::MAX;
    }
    let byte = it.get();
    it.inc();
    if byte & 0x80 == 0 {
        return byte as Codepoint;
    }
    read_codepoint_multibyte(it, end, byte)
}

#[inline(never)]
fn read_codepoint_multibyte<I: ByteIt>(it: &mut I, end: &I, byte: u8) -> Codepoint {
    let invalid = byte as i8 as i32 as Codepoint;
    if it == end {
        return invalid;
    }
    let read = |it: &mut I| {
        let c = it.get();
        it.inc();
        (c & 0x3F) as Codepoint
    };
    let byte = byte as Codepoint;
    if byte & 0xE0 == 0xC0 {
        return ((byte & 0x1F) << 6) | read(it);
    }
    if byte & 0xF0 == 0xE0 {
        let cp = ((byte & 0x0F) << 12) | (read(it) << 6);
        if it == end {
            return cp;
        }
        return cp | read(it);
    }
    if byte & 0xF8 == 0xF0 {
        let mut cp = ((byte & 0x0F) << 18) | (read(it) << 12);
        if it == end {
            return cp;
        }
        cp |= read(it) << 6;
        if it == end {
            return cp;
        }
        return cp | read(it);
    }
    invalid
}

#[inline(always)]
fn codepoint<I: ByteIt>(it: &I, end: &I) -> Codepoint {
    let mut it = it.clone();
    read_codepoint(&mut it, end)
}

/// utf8::advance
fn advance<I: ByteIt>(mut it: I, end: &I, mut d: CharCount) -> I {
    if &it == end {
        return it;
    }
    if d < 0 {
        while &it != end && d != 0 {
            d += 1;
            it_to_previous(&mut it, end);
        }
    } else if d > 0 {
        while &it != end && d != 0 {
            d -= 1;
            it_to_next(&mut it, end);
        }
    }
    it
}

#[derive(Clone, Copy, Default)]
struct Thread {
    inst: u32,
    saves: i32,
}

// This stores both the current thread stack and next thread stack in a ring buffer
// current threads go from current to next_begin (pushing decrements current)
// next thrads go from next_begin to next_end (pushing increments next_end)
#[derive(Default)]
struct DualThreadStack {
    data: Box<[Thread]>,
    capacity_mask: u32, // Maximum capacity should be 2*instruction count, so 65536
    current: u32,
    next_begin: u32,
    next_end: u32,
}

impl DualThreadStack {
    #[inline(always)]
    fn current_is_empty(&self) -> bool {
        self.current == self.next_begin
    }
    #[inline(always)]
    fn next_is_empty(&self) -> bool {
        self.next_end == self.next_begin
    }

    #[inline(always)]
    fn push_current(&mut self, thread: Thread) {
        self.current = self.current.wrapping_sub(1) & self.capacity_mask;
        // SAFETY: indices are masked by capacity_mask == data.len() - 1
        unsafe { *self.data.get_unchecked_mut(self.current as usize) = thread };
        self.grow_ifn(true);
    }
    #[inline(always)]
    fn pop_current(&mut self) -> Thread {
        // SAFETY: indices are masked by capacity_mask == data.len() - 1
        let res = unsafe { *self.data.get_unchecked(self.current as usize) };
        self.current = (self.current + 1) & self.capacity_mask;
        res
    }

    #[inline(always)]
    fn push_next(&mut self, thread: Thread) {
        // SAFETY: indices are masked by capacity_mask == data.len() - 1
        unsafe { *self.data.get_unchecked_mut(self.next_end as usize) = thread };
        self.next_end = (self.next_end + 1) & self.capacity_mask;
        self.grow_ifn(false);
    }
    #[inline(always)]
    fn pop_next(&mut self) -> Thread {
        self.next_end = self.next_end.wrapping_sub(1) & self.capacity_mask;
        self.data[self.next_end as usize]
    }

    fn swap_next(&mut self) {
        self.current = self.next_begin;
        self.next_begin = self.next_end;
    }

    fn ensure_initial_capacity(&mut self) {
        if self.capacity_mask != 0 {
            return;
        }

        const INITIAL_CAPACITY: u32 = 64 / std::mem::size_of::<Thread>() as u32;
        const _: () = assert!(INITIAL_CAPACITY >= 4 && INITIAL_CAPACITY.is_power_of_two());
        self.data = vec![Thread::default(); INITIAL_CAPACITY as usize].into_boxed_slice();
        self.capacity_mask = INITIAL_CAPACITY - 1;
    }

    #[inline(always)]
    fn grow_ifn(&mut self, pushed_current: bool) {
        if self.current == self.next_end {
            self.grow(pushed_current);
        }
    }

    #[inline(never)]
    fn grow(&mut self, pushed_current: bool) {
        let capacity = self.capacity_mask + 1;
        let new_capacity = capacity * 2;
        let mut new_data = Vec::with_capacity(new_capacity as usize);
        // copy the whole data to avoid having to consider either stack wrapping around
        new_data.extend_from_slice(&self.data[self.current as usize..]);
        new_data.extend_from_slice(&self.data[..self.current as usize]);
        new_data.resize(new_capacity as usize, Thread::default());
        self.next_begin = self.next_begin.wrapping_sub(self.current) & self.capacity_mask;
        if pushed_current && self.next_begin == 0 {
            self.next_begin = capacity;
        }
        self.next_end = capacity;
        self.current = 0;

        self.data = new_data.into_boxed_slice();
        debug_assert!(new_capacity.is_power_of_two());
        self.capacity_mask = new_capacity - 1;
    }
}

struct Saves {
    refcount: i32,
    next_free: i32,
    valid_mask: u32,
}

struct ExecConfig<I> {
    begin: I,
    end: I,
    subject_begin: I,
    subject_end: I,
    flags: RegexExecFlags,
}

/// Number of saves whose validity is tracked with the valid mask
const MASKED_SAVES: usize = 32;

pub struct ThreadedRegexVM<'p, I: ByteIt, M: VmMode> {
    program: &'p CompiledRegex,
    threads: DualThreadStack,
    saves: Vec<Saves>,
    /// save positions, save_count entries per Saves
    save_pos: Vec<Option<I>>,
    first_free: i32,
    captures: i32,
    found_match: bool,
    _mode: PhantomData<M>,
}

impl<'p, I: ByteIt, M: VmMode> ThreadedRegexVM<'p, I, M> {
    pub fn new(program: &'p CompiledRegex) -> Self {
        const { assert!(has_direction(M::BITS)) };
        debug_assert!(
            (M::FORWARD && program.first_backward_inst != 0) || (!M::FORWARD && program.first_backward_inst != u32::MAX)
        );
        ThreadedRegexVM {
            program,
            threads: DualThreadStack::default(),
            saves: Vec::new(),
            save_pos: Vec::new(),
            first_free: -1,
            captures: -1,
            found_match: false,
            _mode: PhantomData,
        }
    }

    pub fn program(&self) -> &'p CompiledRegex {
        self.program
    }

    pub fn exec(
        &mut self,
        begin: &I,
        end: &I,
        subject_begin: &I,
        subject_end: &I,
        flags: RegexExecFlags,
        mut idle_func: impl FnMut() -> ControlFlow<()>,
    ) -> bool {
        if flags.contains(RegexExecFlags::NOT_INITIAL_NULL) && begin == end {
            return false;
        }

        let config = ExecConfig {
            begin: if M::FORWARD { begin } else { end }.clone(),
            end: if M::FORWARD { end } else { begin }.clone(),
            subject_begin: subject_begin.clone(),
            subject_end: subject_end.clone(),
            flags,
        };

        self.exec_program(if M::FORWARD { begin } else { end }, &config, &mut idle_func);

        while !self.threads.next_is_empty() {
            let thread = self.threads.pop_next();
            self.release_saves(thread.saves);
        }
        self.found_match
    }

    /// Captures of the last match, None for non participating captures
    pub fn captures(&mut self) -> &[Option<I>] {
        if self.captures >= 0 {
            let count = self.program.save_count as usize;
            let base = self.captures as usize * count;
            let valid_mask = self.saves[self.captures as usize].valid_mask;
            for i in 0..count.min(MASKED_SAVES) {
                if valid_mask & (1 << i) == 0 {
                    self.save_pos[base + i] = None;
                }
            }
            return &self.save_pos[base..base + count];
        }
        &[]
    }

    fn new_saves(&mut self, copy_from: Option<i32>, valid_mask: u32) -> i32 {
        let count = self.program.save_count as usize;
        if self.first_free >= 0 {
            let res = self.first_free;
            let saves = &mut self.saves[res as usize];
            self.first_free = saves.next_free;
            debug_assert_eq!(saves.refcount, 1);
            saves.valid_mask = valid_mask;
            let dst = res as usize * count;
            if let Some(src) = copy_from {
                let src = src as usize * count;
                let n = if count > MASKED_SAVES { count } else { (32 - valid_mask.leading_zeros()) as usize };
                for k in 0..n {
                    let value = self.save_pos[src + k].clone();
                    self.save_pos[dst + k] = value;
                }
            } else {
                for k in MASKED_SAVES.min(count)..count {
                    self.save_pos[dst + k] = None;
                }
            }
            return res;
        }

        self.save_pos.reserve(count);
        match copy_from {
            Some(src) => {
                let src = src as usize * count;
                for k in 0..count {
                    let value = self.save_pos[src + k].clone();
                    self.save_pos.push(value);
                }
            }
            None => self.save_pos.extend(std::iter::repeat_n(None, count)),
        }
        self.saves.push(Saves { refcount: 1, next_free: -1, valid_mask });
        (self.saves.len() - 1) as i32
    }

    #[inline(always)]
    fn release_saves(&mut self, index: i32) {
        if index < 0 {
            return;
        }
        let saves = &mut self.saves[index as usize];
        if saves.refcount == 1 {
            saves.next_free = self.first_free;
            self.first_free = index;
        } else {
            saves.refcount -= 1;
        }
    }

    fn inst_range(&self) -> (usize, usize) {
        let len = self.program.instructions.len();
        let first_backward = (self.program.first_backward_inst as usize).min(len);
        if M::FORWARD { (0, first_backward) } else { (first_backward, len) }
    }

    // Steps a thread until it consumes the current character, matches or fail
    #[inline(always)]
    fn step_current_thread(&mut self, pos: &I, cp: Codepoint, current_step: u16, config: &ExecConfig<I>) {
        let program = self.program;
        let instructions = &program.instructions[..];
        let mut thread = self.threads.pop_current();

        loop {
            let inst_index = thread.inst as usize;
            thread.inst += 1;
            let inst = &instructions[inst_index];
            // if this instruction was already executed for this step in another thread,
            // then this thread is redundant and can be dropped
            if inst.last_step.get() == current_step {
                return self.release_saves(thread.saves);
            }
            inst.last_step.set(current_step);

            let consumed = match inst.op {
                Op::Match => {
                    if (*pos != config.end && !M::SEARCH)
                        || (config.flags.contains(RegexExecFlags::NOT_INITIAL_NULL) && *pos == config.begin)
                    {
                        return self.release_saves(thread.saves);
                    }

                    self.release_saves(self.captures);
                    self.captures = thread.saves;
                    self.found_match = true;

                    // remove lower priority threads
                    while !self.threads.current_is_empty() {
                        let thread = self.threads.pop_current();
                        self.release_saves(thread.saves);
                    }
                    return;
                }
                Op::Literal { codepoint, ignore_case } => {
                    *pos != config.end && codepoint == if ignore_case { to_lower(cp) } else { cp }
                }
                Op::AnyChar => true,
                Op::AnyCharExceptNewLine => *pos != config.end && cp != b'\n' as Codepoint,
                Op::CharRange { min, max, ignore_case, negative } => {
                    let actual_cp = if ignore_case { to_lower(cp) } else { cp };
                    *pos != config.end && (actual_cp >= min as Codepoint && actual_cp <= max as Codepoint) != negative
                }
                Op::CharType(ctype) => *pos != config.end && is_ctype(ctype, cp),
                Op::CharClass { index } => *pos != config.end && program.character_classes[index as usize].matches(cp),
                Op::Jump { offset } => {
                    thread.inst = (inst_index as i32 + offset as i32) as u32;
                    continue;
                }
                Op::Split { offset, prioritize_parent } => {
                    let mut target = (inst_index as i32 + offset as i32) as u32;
                    if instructions[target as usize].last_step.get() != current_step {
                        if thread.saves >= 0 {
                            self.saves[thread.saves as usize].refcount += 1;
                        }
                        if !prioritize_parent {
                            std::mem::swap(&mut thread.inst, &mut target);
                        }
                        self.threads.push_current(Thread { inst: target, saves: thread.saves });
                    }
                    continue;
                }
                Op::Save { index } => {
                    if M::NO_SAVES {
                        continue;
                    }
                    if thread.saves < 0 {
                        thread.saves = self.new_saves(None, 0);
                    } else if self.saves[thread.saves as usize].refcount > 1 {
                        let saves = &mut self.saves[thread.saves as usize];
                        saves.refcount -= 1;
                        let valid_mask = saves.valid_mask;
                        thread.saves = self.new_saves(Some(thread.saves), valid_mask);
                    }
                    let index = index as usize;
                    let base = thread.saves as usize * program.save_count as usize;
                    self.save_pos[base + index] = Some(pos.clone());
                    if index < MASKED_SAVES {
                        self.saves[thread.saves as usize].valid_mask |= 1 << index;
                    }
                    continue;
                }
                Op::LineAssertion { line_start } => {
                    if !(if line_start { Self::is_line_start(pos, config) } else { Self::is_line_end(pos, config) }) {
                        return self.release_saves(thread.saves);
                    }
                    continue;
                }
                Op::SubjectAssertion { subject_begin } => {
                    if pos != if subject_begin { &config.subject_begin } else { &config.subject_end } {
                        return self.release_saves(thread.saves);
                    }
                    continue;
                }
                Op::WordBoundary { positive } => {
                    if Self::is_word_boundary(pos, config) != positive {
                        return self.release_saves(thread.saves);
                    }
                    continue;
                }
                Op::LookAround { index, ahead, positive, ignore_case } => {
                    if self.lookaround(index, ahead, ignore_case, pos.clone(), config) != positive {
                        return self.release_saves(thread.saves);
                    }
                    continue;
                }
            };

            if consumed {
                return self.threads.push_next(thread);
            }
            return self.release_saves(thread.saves);
        }
    }

    fn exec_program(&mut self, start: &I, config: &ExecConfig<I>, idle_func: &mut impl FnMut() -> ControlFlow<()>) {
        debug_assert!(self.threads.current_is_empty() && self.threads.next_is_empty());
        self.threads.ensure_initial_capacity();
        self.release_saves(self.captures);
        self.captures = -1;
        self.found_match = false;

        let program = self.program;
        let start_desc =
            if M::FORWARD { program.forward_start_desc.as_deref() } else { program.backward_start_desc.as_deref() };
        let mut next_start = start.clone();
        if let Some(start_desc) = start_desc {
            if M::SEARCH {
                next_start = Self::find_next_start(start, &config.end, start_desc);
            }
            // Non null start_desc means we consume at least one char
            if next_start == config.end
                || (!M::SEARCH && !start_desc.map[if M::FORWARD { start.get() } else { prev_byte(start) } as usize])
            {
                return;
            }
        }

        let (first_inst, last_inst) = self.inst_range();
        let insts = &program.instructions[first_inst..last_inst];
        let first_inst = first_inst as u32;
        self.threads.push_current(Thread { inst: first_inst, saves: -1 });

        let mut current_step: u16 = u16::MAX;
        let mut idle_count: u8 = 0; // Run idle loop every 256 * 65536 == 16M codepoints
        let mut pos = next_start.clone();
        while pos != config.end {
            current_step = current_step.wrapping_add(1);
            if current_step == 0 {
                idle_count = idle_count.wrapping_add(1);
                if idle_count == 0 && idle_func().is_break() {
                    // Abort the search (C++ unwinds through here with an exception)
                    while !self.threads.current_is_empty() {
                        let thread = self.threads.pop_current();
                        self.release_saves(thread.saves);
                    }
                    self.found_match = false;
                    return;
                }

                // We wrapped, avoid potential collision on inst.last_step by resetting them
                for inst in insts {
                    inst.last_step.set(0);
                }
                current_step = 1; // step 0 is never valid
            }

            let mut next = pos.clone();
            let cp = Self::codepoint(&mut next, config);

            while !self.threads.current_is_empty() {
                self.step_current_thread(&pos, cp, current_step, config);
            }

            if M::SEARCH && !self.found_match {
                if let Some(start_desc) = start_desc {
                    if pos == next_start {
                        next_start = Self::find_next_start(&next, &config.end, start_desc);
                    }
                    if self.threads.next_is_empty() {
                        next = next_start.clone();
                    }
                }
                if start_desc.is_none() || next == next_start {
                    self.threads.push_next(Thread { inst: first_inst, saves: -1 });
                }
            } else if self.threads.next_is_empty() || (self.found_match && M::ANY_MATCH) {
                return;
            }

            pos = next;
            self.threads.swap_next();
        }

        current_step = current_step.wrapping_add(1);
        if current_step == 0 {
            for inst in insts {
                inst.last_step.set(0);
            }
            current_step = 1; // step 0 is never valid
        }
        while !self.threads.current_is_empty() {
            self.step_current_thread(&pos, u32::MAX, current_step, config);
        }
    }

    fn find_next_start(start: &I, end: &I, start_desc: &StartDesc) -> I {
        let mut pos = start.clone();
        let start_byte = start_desc.start_byte;
        if start_byte != 0 {
            while &pos != end {
                if M::FORWARD {
                    if pos.get() == start_byte {
                        return advance(pos, start, -(start_desc.offset as CharCount));
                    }
                    pos.inc();
                } else {
                    let prev = it_previous(&pos, end);
                    if prev.get() == start_byte {
                        return advance(pos, start, start_desc.offset as CharCount);
                    }
                    pos = prev;
                }
            }
        }

        while &pos != end {
            if M::FORWARD {
                if start_desc.map[pos.get() as usize] {
                    return advance(pos, start, -(start_desc.offset as CharCount));
                }
                pos.inc();
            } else {
                let prev = it_previous(&pos, end);
                if start_desc.map[prev.get() as usize] {
                    return advance(pos, start, start_desc.offset as CharCount);
                }
                pos = prev;
            }
        }
        pos
    }

    fn lookaround(&self, index: i16, ahead: bool, ignore_case: bool, mut pos: I, config: &ExecConfig<I>) -> bool {
        let program = self.program;
        let lookarounds = &program.lookarounds;
        let mut it = index as usize;

        if !ahead {
            if pos == config.subject_begin {
                return lookarounds[it] == lookaround::END_OF_LOOKAROUND;
            }
            it_to_previous(&mut pos, &config.subject_begin);
        }

        while lookarounds[it] != lookaround::END_OF_LOOKAROUND {
            if ahead && pos == config.subject_end {
                return false;
            }

            let mut cp = codepoint(&pos, &config.subject_end);
            if ignore_case {
                cp = to_lower(cp);
            }

            let op = lookarounds[it];
            if op == lookaround::ANY_CHAR {
                // any character matches
            } else if op == lookaround::ANY_CHAR_EXCEPT_NEW_LINE {
                if cp == b'\n' as Codepoint {
                    return false;
                }
            } else if (lookaround::CHARACTER_CLASS..lookaround::CHARACTER_TYPE).contains(&op) {
                let index = (op - lookaround::CHARACTER_CLASS) as usize;
                if !program.character_classes[index].matches(cp) {
                    return false;
                }
            } else if (lookaround::CHARACTER_TYPE..lookaround::OP_END).contains(&op) {
                let ctype = CharacterType((op & 0xFF) as u8);
                if !is_ctype(ctype, cp) {
                    return false;
                }
            } else if op != cp {
                return false;
            }

            if !ahead && pos == config.subject_begin {
                return lookarounds[it + 1] == lookaround::END_OF_LOOKAROUND;
            }

            if ahead {
                it_to_next(&mut pos, &config.subject_end);
            } else {
                it_to_previous(&mut pos, &config.subject_begin);
            }
            it += 1;
        }
        true
    }

    fn is_line_start(pos: &I, config: &ExecConfig<I>) -> bool {
        if *pos == config.subject_begin {
            return !config.flags.contains(RegexExecFlags::NOT_BEGIN_OF_LINE);
        }
        prev_byte(pos) == b'\n'
    }

    fn is_line_end(pos: &I, config: &ExecConfig<I>) -> bool {
        if *pos == config.subject_end {
            return !config.flags.contains(RegexExecFlags::NOT_END_OF_LINE);
        }
        pos.get() == b'\n'
    }

    fn is_word_boundary(pos: &I, config: &ExecConfig<I>) -> bool {
        if *pos == config.subject_begin {
            return !config.flags.contains(RegexExecFlags::NOT_BEGIN_OF_WORD);
        }
        if *pos == config.subject_end {
            return !config.flags.contains(RegexExecFlags::NOT_END_OF_WORD);
        }
        is_word_default(codepoint(&it_previous(pos, &config.subject_begin), &config.subject_end))
            != is_word_default(codepoint(pos, &config.subject_end))
    }

    #[inline(always)]
    fn codepoint(it: &mut I, config: &ExecConfig<I>) -> Codepoint {
        if M::FORWARD {
            read_codepoint(it, &config.end)
        } else {
            it_to_previous(it, &config.end);
            codepoint(it, &config.begin)
        }
    }
}

#[inline(always)]
fn prev_byte<I: ByteIt>(it: &I) -> u8 {
    let mut prev = it.clone();
    prev.dec();
    prev.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utf8::SliceIt;

    type Vm<'p, M> = ThreadedRegexVM<'p, SliceIt<'static>, M>;

    fn compile<M: VmMode>(re: &str) -> CompiledRegex {
        let flags = if M::FORWARD { RegexCompileFlags::NONE } else { RegexCompileFlags::BACKWARD };
        compile_regex(re.as_bytes(), flags).unwrap()
    }

    fn exec_flags<M: VmMode>(vm: &mut Vm<'_, M>, s: &'static [u8], flags: RegexExecFlags) -> bool {
        let b = SliceIt::new(s, 0);
        let e = SliceIt::new(s, s.len());
        vm.exec(&b, &e, &b, &e, flags, || ControlFlow::Continue(()))
    }

    fn exec<M: VmMode>(vm: &mut Vm<'_, M>, s: &'static str) -> bool {
        exec_flags(vm, s.as_bytes(), RegexExecFlags::NONE)
    }

    fn cap<M: VmMode>(vm: &mut Vm<'_, M>, i: usize, j: usize) -> &'static [u8] {
        let caps = vm.captures();
        let (b, e) = (caps[i].unwrap(), caps[j].unwrap());
        &b.s[b.pos..e.pos]
    }

    /// C++ StringView{const char*}: the rest of the nul terminated subject
    fn rest<M: VmMode>(vm: &mut Vm<'_, M>, i: usize) -> &'static [u8] {
        let c = vm.captures()[i].unwrap();
        &c.s[c.pos..]
    }

    /// C++ *const char*, the nul terminator at the end
    fn byte_at<M: VmMode>(vm: &mut Vm<'_, M>, i: usize) -> u8 {
        let c = vm.captures()[i].unwrap();
        c.s.get(c.pos).copied().unwrap_or(0)
    }

    macro_rules! test_vm {
        (($vm:ident, $program:ident), $mode:ty, $re:expr) => {
            let $program = compile::<$mode>($re);
            #[allow(unused_mut)]
            let mut $vm = Vm::<$mode>::new(&$program);
        };
        ($vm:ident, $mode:ty, $re:expr) => {
            test_vm!(($vm, program), $mode, $re)
        };
        ($vm:ident, $re:expr) => {
            test_vm!($vm, ForwardMode, $re)
        };
    }

    type FS = ForwardSearch;
    type BS = BackwardSearch;

    #[test]
    fn test_regex_basic() {
        {
            test_vm!(vm, r"a*b");
            assert!(exec(&mut vm, "b"));
            assert!(exec(&mut vm, "ab"));
            assert!(exec(&mut vm, "aaab"));
            assert!(!exec(&mut vm, "acb"));
            assert!(!exec(&mut vm, "abc"));
            assert!(!exec(&mut vm, ""));
        }
        {
            test_vm!(vm, r"^a.*b$");
            assert!(exec(&mut vm, "afoob"));
            assert!(exec(&mut vm, "ab"));
            assert!(!exec(&mut vm, "bab"));
            assert!(!exec(&mut vm, ""));
        }
        {
            test_vm!(vm, r"^(foo|qux|baz)+(bar)?baz$");
            assert!(exec(&mut vm, "fooquxbarbaz"));
            assert_eq!(cap(&mut vm, 2, 3), b"qux");
            assert!(!exec(&mut vm, "fooquxbarbaze"));
            assert!(!exec(&mut vm, "quxbar"));
            assert!(!exec(&mut vm, "blahblah"));
            assert!(exec(&mut vm, "bazbaz"));
            assert!(exec(&mut vm, "quxbaz"));
        }
        {
            test_vm!(vm, r".*\b(foo|bar)\b.*");
            assert!(exec(&mut vm, "qux foo baz"));
            assert_eq!(cap(&mut vm, 2, 3), b"foo");
            assert!(!exec(&mut vm, "quxfoobaz"));
            assert!(exec(&mut vm, "bar"));
            assert!(!exec(&mut vm, "foobar"));
        }
        {
            test_vm!(vm, r"(foo|bar)");
            assert!(exec(&mut vm, "foo"));
            assert!(exec(&mut vm, "bar"));
            assert!(!exec(&mut vm, "foobar"));
        }
        {
            test_vm!(vm, r"[aA]");
            assert!(exec(&mut vm, "a"));
            assert!(exec(&mut vm, "A"));
        }
        {
            test_vm!(vm, r"a{3,5}b");
            assert!(!exec(&mut vm, "aab"));
            assert!(exec(&mut vm, "aaab"));
            assert!(!exec(&mut vm, "aaaaaab"));
            assert!(exec(&mut vm, "aaaaab"));
        }
        {
            test_vm!(vm, r"a{3}b");
            assert!(!exec(&mut vm, "aab"));
            assert!(exec(&mut vm, "aaab"));
            assert!(!exec(&mut vm, "aaaab"));
        }
        {
            test_vm!(vm, r"a{3,}b");
            assert!(!exec(&mut vm, "aab"));
            assert!(exec(&mut vm, "aaab"));
            assert!(exec(&mut vm, "aaaaab"));
        }
        {
            test_vm!(vm, r"a{,3}b");
            assert!(exec(&mut vm, "b"));
            assert!(exec(&mut vm, "ab"));
            assert!(exec(&mut vm, "aaab"));
            assert!(!exec(&mut vm, "aaaab"));
        }
        {
            test_vm!(vm, FS, r"f.*a(.*o)");
            assert!(exec(&mut vm, "blahfoobarfoobaz"));
            assert_eq!(cap(&mut vm, 0, 1), b"foobarfoo");
            assert_eq!(cap(&mut vm, 2, 3), b"rfoo");
            assert!(exec(&mut vm, "mais que fais la police"));
            assert_eq!(cap(&mut vm, 0, 1), b"fais la po");
            assert_eq!(cap(&mut vm, 2, 3), b" po");
        }
        {
            test_vm!(vm, r"[àb-dX-Z-]{3,5}");
            assert!(exec(&mut vm, "cà-Y"));
            assert!(!exec(&mut vm, "àeY"));
            assert!(exec(&mut vm, "dcbàX"));
            assert!(!exec(&mut vm, "efg"));
        }
        {
            test_vm!(vm, r"(a{3,5})a+");
            assert!(exec_flags(&mut vm, b"aaaaaa", RegexExecFlags::NONE));
            assert_eq!(cap(&mut vm, 2, 3), b"aaaaa");
        }
        {
            test_vm!(vm, r"(a{3,5}?)a+");
            assert!(exec_flags(&mut vm, b"aaaaaa", RegexExecFlags::NONE));
            assert_eq!(cap(&mut vm, 2, 3), b"aaa");
        }
        {
            test_vm!(vm, r"(a{3,5}?)a");
            assert!(exec(&mut vm, "aaaa"));
        }
        {
            test_vm!(vm, r"\d{3}");
            assert!(exec(&mut vm, "123"));
            assert!(!exec(&mut vm, "1x3"));
        }
        {
            test_vm!(vm, r"[-\d]+");
            assert!(exec(&mut vm, "123-456"));
            assert!(!exec(&mut vm, "123_456"));
        }
        {
            test_vm!(vm, r"[ \H]+");
            assert!(exec(&mut vm, "abc "));
            assert!(!exec(&mut vm, "a \t"));
        }
        {
            test_vm!(vm, r"\Q{}[]*+?\Ea+");
            assert!(exec(&mut vm, "{}[]*+?aa"));
        }
        {
            test_vm!(vm, r"\Q...");
            assert!(exec(&mut vm, "..."));
            assert!(!exec(&mut vm, "bla"));
        }
        {
            test_vm!(vm, ForwardMode, r"foo\Kbar");
            assert!(exec(&mut vm, "foobar"));
            assert_eq!(cap(&mut vm, 0, 1), b"bar");
            assert!(!exec(&mut vm, "bar"));
        }
        {
            test_vm!(vm, FS, r"foobaz|foo|foobar");
            assert!(exec(&mut vm, "foobar"));
            assert_eq!(cap(&mut vm, 0, 1), b"foo");
        }
        {
            test_vm!(vm, ForwardMode, r"(fo+?).*");
            assert!(exec(&mut vm, "foooo"));
            assert_eq!(cap(&mut vm, 2, 3), b"fo");
        }
    }

    #[test]
    fn test_regex_lookarounds() {
        {
            test_vm!(vm, FS, r"(?=fo[\w]).");
            assert!(exec(&mut vm, "barfoo"));
            assert_eq!(cap(&mut vm, 0, 1), b"f");
        }
        {
            test_vm!(vm, r"(?<!f).");
            assert!(exec(&mut vm, "f"));
        }
        {
            test_vm!(vm, r"(?!f[oa]o)...");
            assert!(!exec(&mut vm, "foo"));
            assert!(exec(&mut vm, "qux"));
        }
        {
            test_vm!(vm, r"...(?<=f\w.)");
            assert!(exec(&mut vm, "foo"));
            assert!(!exec(&mut vm, "qux"));
        }
        {
            test_vm!(vm, r"...(?<!foo)");
            assert!(!exec(&mut vm, "foo"));
            assert!(exec(&mut vm, "qux"));
        }
        {
            test_vm!(vm, r"Foo(?i)f[oB]+");
            assert!(exec(&mut vm, "FooFOoBb"));
        }
        {
            test_vm!(vm, r"(?i)[a-z]+");
            assert!(exec(&mut vm, "ABC"));
        }
        {
            test_vm!(vm, r"[^\]]+");
            assert!(!exec(&mut vm, "a]c"));
            assert!(exec(&mut vm, "abc"));
        }
        {
            test_vm!(vm, r"[^:\n]+");
            assert!(!exec(&mut vm, "\nbc"));
            assert!(exec(&mut vm, "abc"));
        }
        {
            test_vm!(vm, r"(?:foo)+");
            assert!(exec(&mut vm, "foofoofoo"));
            assert!(!exec(&mut vm, "barbarbar"));
        }
        {
            test_vm!(vm, FS, r#"(?<!\\)(?:\\\\)*""#);
            assert!(exec(&mut vm, "foo\""));
        }
        {
            test_vm!(vm, FS, r"$");
            assert!(exec(&mut vm, "foo\n"));
            assert_eq!(byte_at(&mut vm, 0), b'\n');
        }
    }

    #[test]
    fn test_regex_backward() {
        {
            test_vm!(vm, BS, r"fo{1,}");
            assert!(exec(&mut vm, "foo1fooo2"));
            assert_eq!(byte_at(&mut vm, 1), b'2');
        }
        {
            test_vm!(vm, BS, r"(?<=f)oo(b[ae]r)?(?=baz)");
            assert!(exec(&mut vm, "foobarbazfoobazfooberbaz"));
            assert_eq!(cap(&mut vm, 0, 1), b"oober");
            assert_eq!(cap(&mut vm, 2, 3), b"ber");
        }
        {
            test_vm!(vm, BS, r"(baz|boz|foo|qux)(?<!baz)(?<!o)");
            assert!(exec(&mut vm, "quxbozfoobaz"));
            assert_eq!(cap(&mut vm, 0, 1), b"boz");
        }
        {
            test_vm!(vm, BS, r"foo");
            assert!(exec(&mut vm, "foofoo"));
            assert_eq!(byte_at(&mut vm, 1), 0);
        }
        {
            test_vm!(vm, BS, r"$");
            assert!(exec_flags(&mut vm, b"foo\nbar\nbaz\nqux", RegexExecFlags::NOT_END_OF_LINE));
            assert_eq!(rest(&mut vm, 0), b"\nqux");
            assert!(exec_flags(&mut vm, b"foo\nbar\nbaz\nqux", RegexExecFlags::NONE));
            assert_eq!(rest(&mut vm, 0), b"");
        }
        {
            test_vm!(vm, BS, r"^");
            assert!(!exec_flags(&mut vm, b"foo", RegexExecFlags::NOT_BEGIN_OF_LINE));
            assert!(exec_flags(&mut vm, b"foo", RegexExecFlags::NONE));
            assert!(exec_flags(&mut vm, b"foo\nbar", RegexExecFlags::NONE));
            assert_eq!(rest(&mut vm, 0), b"bar");
        }
        {
            test_vm!(vm, BS, r"\A\w+");
            assert!(exec_flags(&mut vm, b"foo\nbar\nbaz", RegexExecFlags::NONE));
            assert_eq!(cap(&mut vm, 0, 1), b"foo");
        }
        {
            test_vm!(vm, BS, r"\b\w+\z");
            assert!(exec_flags(&mut vm, b"foo\nbar\nbaz", RegexExecFlags::NONE));
            assert_eq!(cap(&mut vm, 0, 1), b"baz");
        }
        {
            test_vm!(vm, BS, r"a[^\n]*\n|\n");
            assert!(exec_flags(&mut vm, b"foo\nbar\nb", RegexExecFlags::NONE));
            assert_eq!(cap(&mut vm, 0, 1), b"ar\n");
        }
    }

    #[test]
    fn test_regex_misc() {
        {
            test_vm!(vm, r"()*");
            assert!(!exec(&mut vm, " "));
        }
        {
            test_vm!(vm, FS, r"\b(?<!-)(a|b|)(?!-)\b");
            assert!(exec_flags(&mut vm, b"# foo bar", RegexExecFlags::NONE));
            assert_eq!(byte_at(&mut vm, 0), b'#');
        }
        {
            test_vm!(vm, r"(?=)");
            assert!(exec(&mut vm, ""));
        }
        {
            test_vm!(vm, FS, r"(?i)FOO");
            assert!(exec_flags(&mut vm, b"foo", RegexExecFlags::NONE));
        }
        {
            test_vm!(vm, FS, r".?(?=foo)");
            assert!(exec_flags(&mut vm, b"afoo", RegexExecFlags::NONE));
            assert_eq!(byte_at(&mut vm, 0), b'a');
        }
        {
            test_vm!(vm, FS, r"(?i)(?=Foo)");
            assert!(exec_flags(&mut vm, b"fOO", RegexExecFlags::NONE));
            assert_eq!(byte_at(&mut vm, 0), b'f');
        }
        {
            test_vm!(vm, r"a(?<=\N)\N+(?=.\N)\s(?S)d.+(?!.)\s(?<!\N)g");
            assert!(exec(&mut vm, "abc\ndef\ng"));
            assert!(!exec(&mut vm, "abc\ndef g"));
        }
        {
            test_vm!(vm, r"[d-ea-dcf-k]+");
            assert!(exec(&mut vm, "abcde"));
        }
        {
            test_vm!(vm, r"(?i)[a-c]+");
            assert!(exec(&mut vm, "bCa"));
        }
        {
            test_vm!(vm, r"[\t-\r]+");
            assert!(exec(&mut vm, "\t\n\x0B\x0C\r"));
        }
        {
            test_vm!((vm, program), ForwardMode, r"[\t-\r]\h+[\t-\r]");
            assert_eq!(program.character_classes.len(), 1);
            assert!(exec(&mut vm, "\n  \x0C"));
        }
        {
            test_vm!(vm, r"[^\x00-\x7F]+");
            assert!(!exec(&mut vm, "ascii"));
            assert!(exec(&mut vm, "←↑→↓"));
            assert!(exec(&mut vm, "😄😊😉"));
        }
        {
            test_vm!(vm, r"[^\u000000-\u00ffff]+");
            assert!(!exec(&mut vm, "ascii"));
            assert!(!exec(&mut vm, "←↑→↓"));
            assert!(exec(&mut vm, "😄😊😉"));
        }
        {
            test_vm!(vm, FS, r"д");
            assert!(exec_flags(&mut vm, "д".as_bytes(), RegexExecFlags::NONE));
        }
        {
            test_vm!(vm, FS, "ab");
            let s: &'static [u8] = "fa😄ab".as_bytes();
            let it = |pos| SliceIt::new(s, pos);
            assert!(!vm.exec(&it(0), &it(4), &it(0), &it(s.len()), RegexExecFlags::NONE, || ControlFlow::Continue(())));
        }
        {
            test_vm!(vm, r"\0\x0A\u00260e\u00260F");
            assert!(exec_flags(&mut vm, "\0\n☎☏".as_bytes(), RegexExecFlags::NONE));
        }
        {
            test_vm!(vm, FS, ".{40}");
            assert!(exec_flags(&mut vm, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", RegexExecFlags::NONE));
        }
    }

    #[test]
    fn test_regex_start_desc() {
        {
            test_vm!((vm, program), FS, "(.{3,4}|f)oo");
            let desc = program.forward_start_desc.as_deref().unwrap();
            assert_eq!(desc.offset, 4);
            for c in 0..StartDesc::COUNT {
                assert_eq!(desc.map[c], c == b'f' as usize || c == b'o' as usize);
            }

            assert!(exec_flags(&mut vm, b"xxxoo", RegexExecFlags::NONE));
            assert!(exec_flags(&mut vm, b"xfoo", RegexExecFlags::NONE));
            assert!(!exec_flags(&mut vm, "😄xoo".as_bytes(), RegexExecFlags::NONE));
        }
        {
            test_vm!((vm, program), BS, "oo(.{3,4}|f)");
            let desc = program.backward_start_desc.as_deref().unwrap();
            assert_eq!(desc.offset, 4);
            for c in 0..StartDesc::COUNT {
                assert_eq!(desc.map[c], c == b'f' as usize || c == b'o' as usize);
            }

            assert!(exec_flags(&mut vm, b"ooxxx", RegexExecFlags::NONE));
            assert!(exec_flags(&mut vm, b"oofx", RegexExecFlags::NONE));
            assert!(!exec_flags(&mut vm, "oox😄".as_bytes(), RegexExecFlags::NONE));
        }
    }

    #[test]
    fn test_regex_named_captures() {
        test_vm!((vm, program), ForwardMode, r"(?<year>\d+)-(?<month>\d+)-(?<day>\d+)");
        assert!(exec_flags(&mut vm, b"2019-01-03", RegexExecFlags::NONE));
        assert_eq!(cap(&mut vm, 2, 3), b"2019");
        assert_eq!(cap(&mut vm, 4, 5), b"01");
        assert_eq!(cap(&mut vm, 6, 7), b"03");
        assert_eq!(program.named_captures.len(), 3);
        assert_eq!(program.named_captures[0], NamedCapture { name: b"year".to_vec(), index: 1 });
        assert_eq!(program.named_captures[1], NamedCapture { name: b"month".to_vec(), index: 2 });
        assert_eq!(program.named_captures[2], NamedCapture { name: b"day".to_vec(), index: 3 });
    }

    #[test]
    fn test_regex_parse_errors() {
        let check_parse_error = |re: &str, expected_error: &str| match compile_regex(re.as_bytes(), RegexCompileFlags::NONE) {
            Ok(_) => panic!("expected parse error for {re}"),
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::RegexError);
                assert_eq!(
                    String::from_utf8_lossy(err.what()),
                    String::from_utf8_lossy(&[b"regex parse error: ", expected_error.as_bytes()].concat())
                );
            }
        };

        check_parse_error("(?=a*)", "Quantifiers cannot be used in lookarounds at '(?=a*)<<<HERE>>>'");
        check_parse_error(
            "(?=(a))",
            "Lookaround can only contain literals, any chars or character classes at '(?=(a))<<<HERE>>>'",
        );
        check_parse_error("(?=a|b)", "Alternations cannot be used in lookarounds at '(?=a|<<<HERE>>>b)'");
    }
}
