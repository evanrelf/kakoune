//! Port of display_buffer.hh/cc
//!
//! C++ iterators into atom lists are represented as indices.

use crate::buffer::{Buffer, BufferIterator};
use crate::exception::{Error, Result};
use crate::face::{Face, FaceRegistry};
use crate::string_utils::{column_length, string_of_cp_cols, substr, substr_columns};
use crate::unicode::codepoint_width;
use crate::units::{BufferCoord, BufferRange, ColumnCount};
use crate::utf8;
use indexmap::IndexMap;
use std::borrow::Cow;
use std::rc::Rc;

/// Return a buffer coord usable as an iterator, tolerating one past end of line coords
pub fn get_iterator_coord(buffer: &Buffer, mut coord: BufferCoord) -> BufferCoord {
    // Correct one past the end of line as next line
    if !buffer.is_end(coord) && coord.column == buffer.line_length(coord.line) {
        coord = BufferCoord::line(coord.line + 1);
    }
    coord
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AtomType {
    Range,
    ReplacedRange,
    Text,
}

#[derive(Clone)]
pub struct DisplayAtom {
    pub face: Face,
    type_: AtomType,
    buffer: Option<Rc<Buffer>>,
    range: BufferRange,
    text: Vec<u8>,
}

impl std::fmt::Debug for DisplayAtom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DisplayAtom({:?}, {:?}, {:?})", self.type_, self.range, String::from_utf8_lossy(&self.content()))
    }
}

impl DisplayAtom {
    pub fn range(buffer: &Rc<Buffer>, range: BufferRange, face: Face) -> Self {
        DisplayAtom { face, type_: AtomType::Range, buffer: Some(buffer.clone()), range, text: Vec::new() }
    }

    pub fn replaced_range(buffer: &Rc<Buffer>, range: BufferRange, text: Vec<u8>, face: Face) -> Self {
        DisplayAtom { face, type_: AtomType::ReplacedRange, buffer: Some(buffer.clone()), range, text }
    }

    pub fn text(text: impl Into<Vec<u8>>, face: Face) -> Self {
        DisplayAtom { face, type_: AtomType::Text, buffer: None, range: BufferRange::default(), text: text.into() }
    }

    pub fn content(&self) -> Cow<'_, [u8]> {
        match self.type_ {
            AtomType::Range => {
                let buffer = self.buffer.as_ref().unwrap();
                let line = buffer.line(self.range.begin.line);
                if self.range.begin.line == self.range.end.line {
                    Cow::Owned(
                        substr(
                            &line,
                            self.range.begin.column as usize,
                            (self.range.end.column - self.range.begin.column) as isize,
                        )
                        .to_vec(),
                    )
                } else if self.range.begin.line + 1 == self.range.end.line && self.range.end.column == 0 {
                    Cow::Owned(substr(&line, self.range.begin.column as usize, -1).to_vec())
                } else {
                    Cow::Borrowed(&[])
                }
            }
            AtomType::Text | AtomType::ReplacedRange => Cow::Borrowed(&self.text),
        }
    }

    pub fn length(&self) -> ColumnCount {
        match self.type_ {
            AtomType::Range => {
                let buffer = self.buffer.as_ref().unwrap();
                let lines = buffer.lines();
                let b = BufferIterator::new(&lines, get_iterator_coord(buffer, self.range.begin));
                let e = BufferIterator::new(&lines, get_iterator_coord(buffer, self.range.end));
                utf8::it_column_distance(&b, &e)
            }
            AtomType::Text | AtomType::ReplacedRange => column_length(&self.text),
        }
    }

    pub fn empty(&self) -> bool {
        if self.type_ == AtomType::Range { self.range.begin == self.range.end } else { self.text.is_empty() }
    }

    pub fn begin(&self) -> BufferCoord {
        debug_assert!(self.has_buffer_range());
        self.range.begin
    }

    pub fn end(&self) -> BufferCoord {
        debug_assert!(self.has_buffer_range());
        self.range.end
    }

    pub fn replace_text(&mut self, text: Vec<u8>) {
        if self.type_ == AtomType::Range {
            self.type_ = AtomType::ReplacedRange;
        }
        self.text = text;
    }

    pub fn replace_range(&mut self, range: BufferRange) {
        debug_assert!(self.type_ == AtomType::Text);
        self.type_ = AtomType::ReplacedRange;
        self.range = range;
    }

    pub fn has_buffer_range(&self) -> bool {
        self.type_ == AtomType::Range || self.type_ == AtomType::ReplacedRange
    }

    pub fn buffer(&self) -> &Rc<Buffer> {
        self.buffer.as_ref().expect("atom has a buffer")
    }

    pub fn type_(&self) -> AtomType {
        self.type_
    }

    pub fn raw_text(&self) -> &[u8] {
        &self.text
    }

    pub fn trim_begin(&mut self, count: ColumnCount) -> ColumnCount {
        let mut res = 0;
        if self.type_ == AtomType::Range {
            let buffer = self.buffer.clone().unwrap();
            let lines = buffer.lines();
            let mut it = BufferIterator::new(&lines, get_iterator_coord(&buffer, self.range.begin));
            let end = BufferIterator::new(&lines, get_iterator_coord(&buffer, self.range.end));
            while it != end && res < count {
                res += codepoint_width(utf8::it_read_codepoint(&mut it, &end));
            }
            self.range.begin = it.coord().min(self.range.end);
        } else {
            let mut it = 0;
            while it != self.text.len() && res < count {
                res += codepoint_width(utf8::read_codepoint(&self.text, &mut it));
            }
            self.text = self.text[it..].to_vec();
        }
        res
    }

    pub fn trim_end_to_length(&mut self, count: ColumnCount) -> ColumnCount {
        let mut res = 0;
        if self.type_ == AtomType::Range {
            let buffer = self.buffer.clone().unwrap();
            let lines = buffer.lines();
            let mut it = BufferIterator::new(&lines, get_iterator_coord(&buffer, self.range.begin));
            let end = BufferIterator::new(&lines, get_iterator_coord(&buffer, self.range.end));
            while it != end && res < count {
                res += codepoint_width(utf8::it_read_codepoint(&mut it, &end));
            }
            self.range.end = it.coord().min(self.range.end);
        } else {
            let mut it = 0;
            while it != self.text.len() && res < count {
                res += codepoint_width(utf8::read_codepoint(&self.text, &mut it));
            }
            self.text.truncate(it);
        }
        res
    }
}

impl PartialEq for DisplayAtom {
    fn eq(&self, other: &DisplayAtom) -> bool {
        self.face == other.face && self.type_ == other.type_ && self.content() == other.content()
    }
}

pub type AtomList = Vec<DisplayAtom>;

const INIT_RANGE: BufferRange =
    BufferRange::new(BufferCoord::new(i32::MAX, i32::MAX), BufferCoord::new(i32::MIN, i32::MIN));

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayLine {
    range: BufferRange,
    atoms: AtomList,
}

impl Default for DisplayLine {
    fn default() -> Self {
        DisplayLine { range: INIT_RANGE, atoms: Vec::new() }
    }
}

impl DisplayLine {
    pub fn new(atoms: AtomList) -> Self {
        let mut res = DisplayLine { range: INIT_RANGE, atoms };
        res.compute_range(false);
        res
    }

    /// DisplayLine(String str, Face face)
    pub fn from_str_face(s: impl Into<Vec<u8>>, face: Face) -> Self {
        let mut res = DisplayLine::default();
        res.push_back(DisplayAtom::text(s, face));
        res
    }

    pub fn atoms(&self) -> &AtomList {
        &self.atoms
    }

    pub fn atoms_mut(&mut self) -> &mut AtomList {
        &mut self.atoms
    }

    pub fn iter(&self) -> std::slice::Iter<'_, DisplayAtom> {
        self.atoms.iter()
    }

    pub fn len(&self) -> usize {
        self.atoms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    pub fn length(&self) -> ColumnCount {
        self.atoms.iter().map(|a| a.length()).sum()
    }

    pub fn range(&self) -> BufferRange {
        self.range
    }

    /// Split atom at index at buffer coord pos, returns the index of the first atom
    pub fn split_at_coord(&mut self, idx: usize, pos: BufferCoord) -> usize {
        let it = &mut self.atoms[idx];
        debug_assert!(it.type_ == AtomType::Range);
        let mut atom = it.clone();
        atom.range.end = pos;
        it.range.begin = pos;
        self.atoms.insert(idx, atom);
        idx
    }

    /// Split atom at index at its count column, returns the index of the first atom
    pub fn split_at_column(&mut self, idx: usize, count: ColumnCount) -> usize {
        let it = &mut self.atoms[idx];
        if it.type_ == AtomType::Text || it.type_ == AtomType::ReplacedRange {
            let mut atom = it.clone();
            atom.text = substr_columns(&it.text, 0, count).to_vec();
            it.text = substr_columns(&it.text, count, -1).to_vec();
            self.atoms.insert(idx, atom);
            return idx;
        }
        let buffer = it.buffer().clone();
        let pos = {
            let lines = buffer.lines();
            let mut b = BufferIterator::new(&lines, get_iterator_coord(&buffer, it.begin()));
            let e = BufferIterator::new(&lines, get_iterator_coord(&buffer, it.end()));
            // utf8::advance(it, end, ColumnCount)
            let mut d = count;
            if b != e && d > 0 {
                let begin = b;
                while b != e && d > 0 {
                    d -= codepoint_width(utf8::it_read_codepoint(&mut b, &e));
                    if b != e && d < 0 {
                        utf8::it_to_previous(&mut b, &begin);
                    }
                }
            }
            b.coord()
        };
        let face = it.face;
        if pos == it.begin() {
            // Can happen if we try to split in the middle of a multi-column codepoint
            self.atoms.insert(idx, DisplayAtom::range(&buffer, BufferRange::new(pos, pos), face));
            return idx;
        }
        if pos == it.end() {
            self.atoms.insert(idx + 1, DisplayAtom::range(&buffer, BufferRange::new(pos, pos), face));
            return idx;
        }
        self.split_at_coord(idx, pos)
    }

    /// Split the line at buffer coord pos, returns the index of the atom starting at pos
    pub fn split(&mut self, pos: BufferCoord) -> usize {
        let found = self.atoms.iter().position(|a| {
            (a.has_buffer_range() && a.begin() >= pos) || (a.type_ == AtomType::Range && a.end() > pos)
        });
        match found {
            None => self.atoms.len(),
            Some(idx) => {
                if self.atoms[idx].begin() >= pos {
                    idx
                } else {
                    self.split_at_coord(idx, pos) + 1
                }
            }
        }
    }

    pub fn insert(&mut self, idx: usize, atom: DisplayAtom) -> usize {
        if atom.has_buffer_range() {
            self.range.begin = self.range.begin.min(atom.begin());
            self.range.end = self.range.end.max(atom.end());
        }
        self.atoms.insert(idx, atom);
        idx
    }

    pub fn insert_all(&mut self, idx: usize, atoms: Vec<DisplayAtom>) -> usize {
        let had_range = self.atoms.iter().any(|a| a.has_buffer_range());
        if let Some(first) = atoms.iter().find(|a| a.has_buffer_range()) {
            let last = atoms.iter().rev().find(|a| a.has_buffer_range()).unwrap();
            self.range.begin = if had_range { self.range.begin.min(first.begin()) } else { first.begin() };
            self.range.end = if had_range { self.range.end.max(last.end()) } else { last.end() };
        }
        self.atoms.splice(idx..idx, atoms);
        idx
    }

    pub fn push_back(&mut self, atom: DisplayAtom) -> &mut DisplayAtom {
        if atom.has_buffer_range() {
            self.range.begin = self.range.begin.min(atom.begin());
            self.range.end = self.range.end.max(atom.end());
        }
        self.atoms.push(atom);
        self.atoms.last_mut().unwrap()
    }

    pub fn extract(&mut self, beg: usize, end: usize) -> DisplayLine {
        if beg == 0 && end == self.atoms.len() {
            return std::mem::take(self);
        }
        let extracted = DisplayLine::new(self.atoms.drain(beg..end).collect());
        if extracted.range.begin == self.range.begin || extracted.range.end == self.range.end {
            self.compute_range(true);
        }
        extracted
    }

    pub fn erase(&mut self, beg: usize, end: usize) -> usize {
        self.atoms.drain(beg..end);
        self.compute_range(true);
        beg
    }

    /// Merge together consecutive atoms sharing the same display attributes
    pub fn optimize(&mut self) {
        if self.atoms.is_empty() {
            return;
        }
        let mut atom_it = 0;
        for next_it in 1..self.atoms.len() {
            let (head, tail) = self.atoms.split_at_mut(next_it);
            let atom = &mut head[atom_it];
            let next = &mut tail[0];
            let type_ = atom.type_;
            if type_ == next.type_ && atom.face == next.face {
                if type_ == AtomType::Text {
                    let t = std::mem::take(&mut next.text);
                    atom.text.extend(t);
                    continue;
                } else if (type_ == AtomType::Range || type_ == AtomType::ReplacedRange) && next.begin() == atom.end() {
                    atom.range.end = next.end();
                    if type_ == AtomType::ReplacedRange {
                        let t = std::mem::take(&mut next.text);
                        atom.text.extend(t);
                    }
                    continue;
                }
            }
            atom_it += 1;
            if atom_it != next_it {
                self.atoms.swap(atom_it, next_it);
            }
        }
        self.atoms.truncate(atom_it + 1);
    }

    /// remove front from the begining of the line, and make sure
    /// the line is less that col_count character
    pub fn trim(&mut self, front: ColumnCount, col_count: ColumnCount) -> bool {
        self.trim_from(0, front, col_count)
    }

    /// remove front from the begining of the line + first_col, and make sure
    /// the line is less that col_count character
    pub fn trim_from(&mut self, mut first_col: ColumnCount, mut front: ColumnCount, mut col_count: ColumnCount) -> bool {
        let mut it = 0;
        while first_col > 0 && it != self.atoms.len() {
            let len = self.atoms[it].length();
            if len <= first_col {
                it += 1;
                first_col -= len;
            } else {
                if front > 0 {
                    it = self.split_at_column(it, front) + 1;
                }
                first_col = 0;
            }
        }

        let front_it = it;
        let mut padding: Option<DisplayAtom> = None;
        while front > 0 && it != self.atoms.len() {
            front -= self.atoms[it].trim_begin(front);
            if front < 0 {
                let a = &self.atoms[it];
                let spaces = string_of_cp_cols(' ' as u32, -front);
                padding = Some(if a.has_buffer_range() {
                    DisplayAtom::replaced_range(a.buffer(), BufferRange::new(a.begin(), a.begin()), spaces, a.face)
                } else {
                    DisplayAtom::text(spaces, a.face)
                });
            }
            if self.atoms[it].empty() {
                it += 1;
            }
        }
        self.atoms.drain(front_it..it);
        let mut it = front_it;
        if let Some(padding) = padding {
            self.atoms.insert(it, padding);
        }

        it = 0;
        while it != self.atoms.len() && col_count > 0 {
            col_count -= self.atoms[it].trim_end_to_length(col_count);
            it += 1;
        }
        let did_trim = it != self.atoms.len() && col_count == 0;
        self.atoms.truncate(it);
        self.compute_range(true);
        did_trim
    }

    fn compute_range(&mut self, preserve_if_no_atoms: bool) {
        match self.atoms.iter().position(|a| a.has_buffer_range()) {
            None => {
                self.range = if preserve_if_no_atoms && self.range != INIT_RANGE {
                    self.range
                } else {
                    BufferRange::new(BufferCoord::default(), BufferCoord::default())
                };
            }
            Some(first) => {
                let last = self.atoms.iter().rposition(|a| a.has_buffer_range()).unwrap();
                self.range.begin = self.atoms[first].begin();
                self.range.end = self.atoms[last].end();
            }
        }
    }
}

impl<'a> IntoIterator for &'a DisplayLine {
    type Item = &'a DisplayAtom;
    type IntoIter = std::slice::Iter<'a, DisplayAtom>;
    fn into_iter(self) -> Self::IntoIter {
        self.atoms.iter()
    }
}

pub type DisplayLineList = Vec<DisplayLine>;

#[derive(Clone, Debug)]
pub struct DisplayBuffer {
    lines: DisplayLineList,
    range: BufferRange,
    timestamp: usize,
}

impl Default for DisplayBuffer {
    fn default() -> Self {
        DisplayBuffer { lines: Vec::new(), range: BufferRange::default(), timestamp: usize::MAX }
    }
}

impl DisplayBuffer {
    pub fn lines(&self) -> &DisplayLineList {
        &self.lines
    }
    pub fn lines_mut(&mut self) -> &mut DisplayLineList {
        &mut self.lines
    }
    /// returns the smallest BufferRange which contains every DisplayAtoms
    pub fn range(&self) -> BufferRange {
        self.range
    }
    pub fn compute_range(&mut self) {
        self.range = INIT_RANGE;
        for line in &self.lines {
            self.range.begin = line.range().begin.min(self.range.begin);
            self.range.end = line.range().end.max(self.range.end);
        }
        if self.range == INIT_RANGE {
            self.range = BufferRange::default();
        }
    }
    /// Optimize all lines, set DisplayLine::optimize
    pub fn optimize(&mut self) {
        for line in &mut self.lines {
            line.optimize();
        }
    }
    pub fn set_timestamp(&mut self, timestamp: usize) {
        self.timestamp = timestamp;
    }
    pub fn timestamp(&self) -> usize {
        self.timestamp
    }
}

pub type DisplayLineBuiltins = IndexMap<Vec<u8>, DisplayLine>;

fn parse_display_line_with_face(
    line: &[u8],
    face: &mut Face,
    faces: &FaceRegistry,
    builtins: &DisplayLineBuiltins,
) -> Result<DisplayLine> {
    let mut res = DisplayLine::default();
    let mut was_antislash = false;
    let mut pos = 0;
    let mut content: Vec<u8> = Vec::new();
    let end = line.len();
    let mut it = 0;
    while it != end {
        let c = line[it];
        if c == b'{' {
            if was_antislash {
                content.extend_from_slice(&line[pos..it]);
                *content.last_mut().unwrap() = b'{';
                pos = it + 1;
            } else {
                content.extend_from_slice(&line[pos..it]);
                if !content.is_empty() {
                    res.push_back(DisplayAtom::text(std::mem::take(&mut content), *face));
                }
                content.clear();
                let Some(mut closing) = line[it + 1..].iter().position(|&c| c == b'}').map(|p| p + it + 1) else {
                    return Err(Error::runtime("unclosed face definition"));
                };
                if line.get(it + 1) == Some(&b'{') && closing + 1 != end && line[closing + 1] == b'}' {
                    let name = &line[it + 2..closing];
                    let Some(builtin) = builtins.get(name) else {
                        return Err(rt_err!("undefined atom {}", name));
                    };
                    for atom in builtin.iter() {
                        res.push_back(atom.clone());
                    }
                    // closing is now at the first char of "}}", advance it to the second
                    closing += 1;
                } else if closing == it + 2 && line[it + 1] == b'\\' {
                    pos = closing + 1;
                    break;
                } else {
                    *face = faces.get(&line[it + 1..closing])?;
                }
                it = closing;
                pos = closing + 1;
            }
        }
        if c == b'\n' || c == b'\t' {
            // line breaks and tabs are forbidden, replace with space
            content.extend_from_slice(&line[pos..it + 1]);
            *content.last_mut().unwrap() = b' ';
            pos = it + 1;
        }
        if c == b'\\' {
            if was_antislash {
                content.extend_from_slice(&line[pos..it]);
                pos = it + 1;
                was_antislash = false;
            } else {
                was_antislash = true;
            }
        } else {
            was_antislash = false;
        }
        it += 1;
    }
    content.extend_from_slice(&line[pos.min(end)..]);
    if !content.is_empty() {
        res.push_back(DisplayAtom::text(content, *face));
    }
    Ok(res)
}

pub fn parse_display_line(line: &[u8], faces: &FaceRegistry, builtins: &DisplayLineBuiltins) -> Result<DisplayLine> {
    let mut face = Face::default();
    parse_display_line_with_face(line, &mut face, faces, builtins)
}

pub fn parse_display_line_list(
    content: &[u8],
    faces: &FaceRegistry,
    builtins: &DisplayLineBuiltins,
) -> Result<DisplayLineList> {
    let mut face = Face::default();
    crate::string_utils::split(content, b'\n')
        .into_iter()
        .map(|s| parse_display_line_with_face(s, &mut face, faces, builtins))
        .collect()
}
