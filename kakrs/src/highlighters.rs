//! Port of highlighters.hh/cc

use crate::buffer::{Buffer, BufferIterator, StringDataPtr};
use crate::buffer_utils::{get_column, is_bol, is_bow, is_eol, is_eow};
use crate::changes::{update_ranges, UpdatableRange};
use crate::command_manager::{expand, CommandParser, ShellContext, TokenType};
use crate::completion::{complete, offset_pos, CompletionFlags, Completions};
use crate::context::Context;
use crate::debug::write_to_debug_buffer;
use crate::display_buffer::{
    get_iterator_coord, parse_display_line, AtomType, DisplayAtom, DisplayBuffer, DisplayLine, DisplayLineBuiltins,
    DisplayLineList,
};
use crate::exception::{Error, ErrorKind, Result};
use crate::face::{merge_faces, parse_face, Face, FaceSpec};
use crate::highlighter::{
    compute_display_setup, highlight, DisplaySetup, HighlightContext, HighlightPass, Highlighter, HighlighterDesc,
    HighlighterParameters, HighlighterRegistry,
};
use crate::highlighter_group::{child_not_found, HighlighterGroup};
use crate::line_modification::{compute_line_modifications, LineModification, LineRange, LineRangeSet};
use crate::option_types::{
    list_from_strings, tuple_from_string, tuple_to_string, CodepointOpt, OptionElem, PrefixedListType,
    TimestampedList,
};
use crate::parameters_parser::{switches, ParameterDesc, ParameterFlags, ParametersParser, SwitchDesc};
use crate::regex::{match_flags, noop_idle, Regex, RegexIterator};
use crate::regex_vm::{ForwardSearch, RegexCompileFlags, RegexExecFlags, ThreadedRegexVM};
use crate::scope::GlobalScope;
use crate::string_utils::{char_length, column_length, str_to_int, str_to_int_ifp, string_of_cp, Quoting};
use crate::unicode::{codepoint_width, is_blank, is_horizontal_blank, is_word};
use crate::units::{BufferCoord, BufferRange, ByteCount, Codepoint, ColumnCount, LineCount};
use crate::utf8::{self, SliceIt, Utf8Iter};
use crate::value::{get_free_value_id, ValueId};
use indexmap::IndexMap;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::{Rc, Weak};

// Helpers --------------------------------------------------------------------

type HighlightFunc = dyn Fn(&HighlightContext, &mut DisplayBuffer, BufferRange) -> Result<()>;

/// make_highlighter: a highlighter running a simple function
struct SimpleHighlighter {
    passes: HighlightPass,
    func: Box<HighlightFunc>,
}

impl Highlighter for SimpleHighlighter {
    fn passes(&self) -> HighlightPass {
        self.passes
    }
    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) -> Result<()> {
        (self.func)(context, display_buffer, range)
    }
}

fn make_highlighter(
    func: impl Fn(&HighlightContext, &mut DisplayBuffer, BufferRange) -> Result<()> + 'static,
    pass: HighlightPass,
) -> Rc<dyn Highlighter> {
    Rc::new(SimpleHighlighter { passes: pass, func: Box::new(func) })
}

/// String{' ', count}
fn spaces(count: ColumnCount) -> Vec<u8> {
    vec![b' '; count.max(0) as usize]
}

// [FAITHFUL] C++ converts a (signed) char to a Codepoint, non ascii bytes end
// up as huge codepoints that never match any character class.
fn char_to_codepoint(c: u8) -> Codepoint {
    c as i8 as i32 as Codepoint
}

/// DisplayAtom::begin() for any atom type (text atoms have a default range)
fn atom_begin(atom: &DisplayAtom) -> BufferCoord {
    if atom.has_buffer_range() { atom.begin() } else { BufferCoord::default() }
}

fn contains_id(ids: &[Vec<u8>], id: &[u8]) -> bool {
    ids.iter().any(|i| i.as_slice() == id)
}

/// std::inplace_merge: stable merge of the two sorted halves [..mid] and [mid..]
fn inplace_merge<T>(v: &mut Vec<T>, mid: usize, less: impl Fn(&T, &T) -> bool) {
    if mid == 0 || mid == v.len() {
        return;
    }
    let right = v.split_off(mid);
    let left = std::mem::take(v);
    v.reserve(left.len() + right.len());
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    loop {
        match (left.peek(), right.peek()) {
            (Some(l), Some(r)) => {
                if less(r, l) {
                    v.push(right.next().unwrap());
                } else {
                    v.push(left.next().unwrap());
                }
            }
            (Some(_), None) => v.push(left.next().unwrap()),
            (None, Some(_)) => v.push(right.next().unwrap()),
            (None, None) => break,
        }
    }
}

fn expand_expr(expr: &[u8], context: &Context) -> Result<Vec<u8>> {
    expand(expr, context, &ShellContext::default())
}

// [DEVIATION] is_bow/is_eow tolerating one past the end of line coords: the
// C++ reads past the line there (its terminating nul character), we compute
// the same result (a non word character) without the out of range access.
fn is_bow_tolerant(buffer: &Buffer, coord: BufferCoord) -> bool {
    if !buffer.is_end(coord) && coord.column >= buffer.line_length(coord.line) {
        return false;
    }
    is_bow(buffer, coord)
}

fn is_eow_tolerant(buffer: &Buffer, coord: BufferCoord) -> bool {
    if !buffer.is_end(coord) && coord.line >= 0 && coord.column >= buffer.line_length(coord.line) {
        if coord == BufferCoord::new(0, 0) {
            return false;
        }
        let lines = buffer.lines();
        let line: &[u8] = &lines[coord.line as usize];
        let pos = (line.len()).min(coord.column as usize);
        let prev = utf8::prev_codepoint(line, pos, 0);
        return is_word(prev, &['_' as Codepoint]);
    }
    is_eow(buffer, coord)
}

fn highlight_range(
    display_buffer: &mut DisplayBuffer,
    begin: BufferCoord,
    end: BufferCoord,
    skip_replaced: bool,
    mut func: impl FnMut(&mut DisplayAtom),
) {
    // tolerate begin > end as that can be triggered by wrong encodings
    let db_range = display_buffer.range();
    if begin >= end || end <= db_range.begin || begin >= db_range.end {
        return;
    }

    for line in display_buffer.lines_mut() {
        let range = line.range();
        if range.end <= begin || end < range.begin {
            continue;
        }

        let mut i = 0;
        while i < line.len() {
            let atom = &line.atoms()[i];
            let is_replaced = atom.type_() == AtomType::ReplacedRange;

            if !atom.has_buffer_range() || (skip_replaced && is_replaced) || end <= atom.begin() || begin >= atom.end() {
                i += 1;
                continue;
            }

            if !is_replaced && begin > atom.begin() {
                i = line.split_at_coord(i, begin) + 1;
            }

            if !is_replaced && end < line.atoms()[i].end() {
                i = line.split_at_coord(i, end);
                func(&mut line.atoms_mut()[i]);
                i += 1;
            } else {
                func(&mut line.atoms_mut()[i]);
            }
            i += 1;
        }
    }
}

fn replace_range(
    display_buffer: &mut DisplayBuffer,
    begin: BufferCoord,
    end: BufferCoord,
    func: impl FnOnce(&mut DisplayLineList, usize, usize) -> Result<()>,
) -> Result<()> {
    // tolerate begin > end as that can be triggered by wrong encodings
    let db_range = display_buffer.range();
    if begin > end || end < db_range.begin || begin > db_range.end {
        return Ok(());
    }

    let lines = display_buffer.lines_mut();
    let first_it = lines.partition_point(|l| l.range().end < begin);
    if first_it == lines.len() {
        return Ok(());
    }

    let mut first_atom_it = lines[first_it].split(begin);

    let mut last_it = first_it + lines[first_it..].partition_point(|l| l.range().end < end);

    if first_it == last_it {
        let first_atom_idx = first_atom_it;
        let end_atom_it = lines[first_it].split(end);
        first_atom_it = lines[first_it].erase(first_atom_idx, end_atom_it);
    } else {
        let len = lines[first_it].len();
        first_atom_it = lines[first_it].erase(first_atom_it, len);
        if last_it != lines.len() {
            let end_atom_it = lines[last_it].split(end);
            lines[last_it].erase(0, end_atom_it);

            let remaining = lines[last_it].atoms().clone();
            first_atom_it = lines[first_it].insert_all(first_atom_it, remaining);
            last_it += 1;
        }
        lines.drain(first_it + 1..last_it);
    }

    func(lines, first_it, first_atom_it)
}

fn apply_face(face: Face) -> impl FnMut(&mut DisplayAtom) {
    move |atom: &mut DisplayAtom| atom.face = merge_faces(&atom.face, &face)
}

fn no_builtins() -> DisplayLineBuiltins {
    DisplayLineBuiltins::new()
}

// Fill -----------------------------------------------------------------------

fn fill_desc() -> HighlighterDesc {
    HighlighterDesc { docstring: "Fill the whole highlighted range with the given face", params: ParameterDesc::default() }
}

fn create_fill_highlighter(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
    if params.len() != 1 {
        return Err(Error::runtime("wrong parameter count"));
    }

    let spec = parse_face(&params[0])?;
    Ok(make_highlighter(
        move |context, display_buffer, range| {
            let face = context.context.faces().get_spec(&spec);
            highlight_range(display_buffer, range.begin, range.end, false, apply_face(face));
            Ok(())
        },
        HighlightPass::Colorize,
    ))
}

/// A per buffer cache, stored in the buffer values
struct BufferSideCache<T> {
    id: ValueId,
    _marker: PhantomData<T>,
}

impl<T: Default + 'static> BufferSideCache<T> {
    fn new() -> Self {
        BufferSideCache { id: get_free_value_id(), _marker: PhantomData }
    }

    fn get(&self, buffer: &Buffer) -> Rc<RefCell<T>> {
        buffer.values().get_or_insert_with(self.id, || RefCell::new(T::default()))
    }
}

type FacesSpec = Vec<(usize, FaceSpec)>;

// Regex ----------------------------------------------------------------------

fn regex_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: <regex> <capture num>:<face> <capture num>:<face>...\n\
                    Highlights the matches for captures from the regex with the given faces",
        params: ParameterDesc::default(),
    }
}

/// stores the range for each highlighted capture of each match
type MatchList = Vec<BufferRange>;

struct RangeAndMatches {
    range: BufferRange,
    matches: MatchList,
}

struct RegexCache {
    timestamp: usize,
    regex_version: usize,
    matches: HashMap<BufferRange, Vec<RangeAndMatches>>,
    /// Total number of RangeAndMatches in `matches`, kept up to date rather
    /// than summed on every lookup (the C++ sums them each time)
    range_count: usize,
}

impl Default for RegexCache {
    fn default() -> Self {
        RegexCache { timestamp: usize::MAX, regex_version: usize::MAX, matches: HashMap::new(), range_count: 0 }
    }
}

struct RegexState {
    regex: Regex,
    faces: FacesSpec,
    regex_version: usize,
}

impl RegexState {
    fn ensure_first_face_is_capture_0(&mut self) {
        if self.faces.is_empty() {
            return;
        }

        // [DEVIATION] stable sort, std::sort leaves the order of equal captures unspecified
        self.faces.sort_by_key(|f| f.0);
        if self.faces[0].0 != 0 {
            self.faces.insert(0, (0, FaceSpec::default()));
        }
    }
}

struct RegexHighlighter {
    state: RefCell<RegexState>,
    cache: BufferSideCache<RegexCache>,
}

impl RegexHighlighter {
    pub fn new(regex: Regex, faces: FacesSpec) -> Self {
        let mut state = RegexState { regex, faces, regex_version: 0 };
        state.ensure_first_face_is_capture_0();
        RegexHighlighter { state: RefCell::new(state), cache: BufferSideCache::new() }
    }

    pub fn reset(&self, regex: Regex, faces: FacesSpec) {
        let mut state = self.state.borrow_mut();
        state.regex = regex;
        state.faces = faces;
        state.ensure_first_face_is_capture_0();
        state.regex_version += 1;
    }

    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        if params.len() < 2 {
            return Err(Error::runtime("wrong parameter count"));
        }

        let re = Regex::new(&params[0], RegexCompileFlags::OPTIMIZE)?;

        let mut faces = FacesSpec::new();
        for spec in &params[1..] {
            let Some(colon) = spec.iter().position(|&c| c == b':') else {
                return Err(rt_err!("wrong face spec: '{}' expected <capture>:<facespec>", spec));
            };
            let capture_name = &spec[..colon];
            let capture = str_to_int_ifp(capture_name).unwrap_or_else(|| re.named_capture_index(capture_name));
            if capture < 0 {
                return Err(rt_err!(
                    "capture name {} is neither a capture index, nor an existing capture name",
                    capture_name
                ));
            }
            faces.push((capture as usize, parse_face(&spec[colon + 1..])?));
        }

        Ok(Rc::new(RegexHighlighter::new(re, faces)))
    }

    fn add_matches(state: &RegexState, buffer: &Buffer, matches: &mut MatchList, range: BufferRange) {
        debug_assert!(matches.len().is_multiple_of(state.faces.len()));
        let flags = match_flags(
            is_bol(range.begin),
            is_eol(buffer, range.end),
            is_bow_tolerant(buffer, range.begin),
            is_eow_tolerant(buffer, range.end),
        );
        let lines = buffer.lines();
        let begin = BufferIterator::new(&lines, get_iterator_coord(buffer, range.begin));
        let end = BufferIterator::new(&lines, get_iterator_coord(buffer, range.end));
        let mut it = RegexIterator::<BufferIterator, ForwardSearch>::new(
            begin,
            end,
            BufferIterator::begin(&lines),
            BufferIterator::end(&lines),
            &state.regex,
            flags,
        );
        while let Some(m) = it.next_match() {
            for face in &state.faces {
                let sub = &m[face.0];
                // [FAITHFUL] unmatched captures give an empty {0,0} range (C++
                // default constructed iterators), which highlight_range ignores
                matches.push(BufferRange::new(
                    sub.first.map(|i| i.coord()).unwrap_or_default(),
                    sub.second.map(|i| i.coord()).unwrap_or_default(),
                ));
            }
        }
    }

    fn get_matches<'c>(
        state: &RegexState,
        cache: &'c mut RegexCache,
        buffer: &Buffer,
        display_range: BufferRange,
        buffer_range: BufferRange,
    ) -> &'c MatchList {
        if cache.regex_version != state.regex_version
            || cache.timestamp != buffer.timestamp()
            || cache.range_count > 1000
        {
            cache.matches.clear();
            cache.range_count = 0;
            cache.timestamp = buffer.timestamp();
            cache.regex_version = state.regex_version;
        }

        let matches = cache.matches.entry(buffer_range).or_default();

        const LINE_OFFSET: LineCount = 3;
        let range = BufferRange::new(
            buffer_range.begin.max(BufferCoord::line(display_range.begin.line - LINE_OFFSET)),
            buffer_range.end.min(BufferCoord::line(display_range.end.line + LINE_OFFSET)),
        );

        let it = matches.partition_point(|m| m.range.end <= range.begin);

        if it == matches.len() || matches[it].range.begin > range.end {
            matches.insert(it, RangeAndMatches { range, matches: MatchList::new() });
            cache.range_count += 1;
            Self::add_matches(state, buffer, &mut matches[it].matches, range);
        } else if matches[it].matches.is_empty() {
            matches[it].range = range;
            Self::add_matches(state, buffer, &mut matches[it].matches, range);
        } else {
            // Here we extend the matches, that is not strictly valid,
            // but may work nicely with every reasonable regex, and
            // greatly reduces regex parsing. To change if we encounter
            // regex that do not work great with that.
            let entry = &mut matches[it];
            let nb_faces = state.faces.len();

            // Thanks to the ensure_first_face_is_capture_0 method, we know
            // these point to the first/last matches capture 0.
            let first_end = entry.matches[0].end;
            let last_end = entry.matches[entry.matches.len() - nb_faces].end;

            // add regex matches from new begin to old first match end
            if range.begin < entry.range.begin {
                entry.matches.drain(0..nb_faces);
                let pivot = entry.matches.len();
                entry.range.begin = range.begin;
                Self::add_matches(state, buffer, &mut entry.matches, BufferRange::new(range.begin, first_end));

                entry.matches.rotate_left(pivot);
            }
            // add regex matches from old last match begin to new end
            if entry.range.end < range.end {
                entry.range.end = range.end;
                Self::add_matches(state, buffer, &mut entry.matches, BufferRange::new(last_end, range.end));
            }
        }
        &matches[it].matches
    }
}

impl Highlighter for RegexHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Colorize
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) -> Result<()> {
        let overlaps = |lhs: BufferRange, rhs: BufferRange| {
            if lhs.begin < rhs.begin { lhs.end > rhs.begin } else { rhs.end > lhs.begin }
        };

        if !overlaps(display_buffer.range(), range) {
            return Ok(());
        }

        let state = self.state.borrow();
        let registry = context.context.faces();
        let faces: Vec<Face> = state.faces.iter().map(|spec| registry.get_spec(&spec.1)).collect();
        if faces.is_empty() {
            return Ok(());
        }

        let buffer = context.context.buffer();
        let cache = self.cache.get(&buffer);
        let mut cache = cache.borrow_mut();
        let matches = Self::get_matches(&state, &mut cache, &buffer, display_buffer.range(), range);
        debug_assert!(matches.len().is_multiple_of(faces.len()));
        for (m, r) in matches.iter().enumerate() {
            let face = faces[m % faces.len()];
            if face == Face::default() {
                continue;
            }

            highlight_range(display_buffer, r.begin, r.end, false, apply_face(face));
        }
        Ok(())
    }
}

// Dynamic regex --------------------------------------------------------------

type RegexGetter = Box<dyn Fn(&Context) -> Result<Regex>>;
type FaceGetter = Box<dyn Fn(&Regex) -> FacesSpec>;

struct DynamicRegexHighlighter {
    last_regex: RefCell<Regex>,
    get_regex: RegexGetter,

    last_face: RefCell<FacesSpec>,
    resolve_faces: FaceGetter,

    highlighter: RegexHighlighter,
}

impl DynamicRegexHighlighter {
    fn new(get_regex: RegexGetter, resolve_faces: FaceGetter) -> Self {
        DynamicRegexHighlighter {
            last_regex: RefCell::new(Regex::default()),
            get_regex,
            last_face: RefCell::new(FacesSpec::new()),
            resolve_faces,
            highlighter: RegexHighlighter::new(Regex::default(), FacesSpec::new()),
        }
    }
}

impl Highlighter for DynamicRegexHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Colorize
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) -> Result<()> {
        let regex = (self.get_regex)(context.context)?;
        let face = if regex.empty() { FacesSpec::new() } else { (self.resolve_faces)(&regex) };
        let changed = regex != *self.last_regex.borrow() || face != *self.last_face.borrow();
        if changed {
            *self.last_regex.borrow_mut() = regex;
            *self.last_face.borrow_mut() = face;
            let last_regex = self.last_regex.borrow();
            if !last_regex.empty() {
                self.highlighter.reset(last_regex.clone(), self.last_face.borrow().clone());
            }
        }
        if !self.last_regex.borrow().empty() && !self.last_face.borrow().is_empty() {
            highlight(&self.highlighter, context, display_buffer, range);
        }
        Ok(())
    }
}

fn dynamic_regex_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: <expr> <capture num>:<face> <capture num>:<face>...\n\
                    Evaluate expression at every redraw to gather a regex",
        params: ParameterDesc::default(),
    }
}

fn create_dynamic_regex_highlighter(
    params: HighlighterParameters,
    _: Option<&dyn Highlighter>,
) -> Result<Rc<dyn Highlighter>> {
    if params.len() < 2 {
        return Err(Error::runtime("wrong parameter count"));
    }

    let mut faces: Vec<(Vec<u8>, FaceSpec)> = Vec::new();
    for spec in &params[1..] {
        let Some(colon) = spec.iter().position(|&c| c == b':') else {
            return Err(rt_err!("wrong face spec: '{}' expected <capture>:<facespec>", spec));
        };
        faces.push((spec[..colon].to_vec(), parse_face(&spec[colon + 1..])?));
    }

    let resolve_faces: FaceGetter = Box::new(move |regex: &Regex| {
        let mut spec = FacesSpec::new();
        for face in &faces {
            let capture = str_to_int_ifp(&face.0).unwrap_or_else(|| regex.named_capture_index(&face.0));
            if capture < 0 {
                write_to_debug_buffer(&kformat!(
                    "Error while evaluating dynamic regex expression faces, \
                     {} is neither a capture index nor a capture name",
                    face.0
                ));
                return FacesSpec::new();
            }
            spec.push((capture as usize, face.1.clone()));
        }
        spec
    });

    let mut parser = CommandParser::new(&params[0]);
    let token = parser.read_token(true)?;
    if let Some(token) = token
        && parser.done()
        && token.type_ == TokenType::OptionExpand
        && GlobalScope::instance().options().get(&token.content)?.is_of_type::<Regex>()
    {
        let option_name = token.content;
        let get_regex: RegexGetter = Box::new(move |context: &Context| {
            Ok(context.options().get(&option_name)?.try_get::<Regex>()?.clone())
        });
        return Ok(Rc::new(DynamicRegexHighlighter::new(get_regex, resolve_faces)));
    }

    let expr = params[0].clone();
    let get_regex: RegexGetter = Box::new(move |context: &Context| {
        let res = expand_expr(&expr, context).and_then(|re| {
            if re.is_empty() { Ok(Regex::default()) } else { Regex::new(&re, RegexCompileFlags::NONE) }
        });
        match res {
            Ok(re) => Ok(re),
            Err(err) if err.is_runtime() => {
                write_to_debug_buffer(&kformat!("Error while evaluating dynamic regex expression: {}", err.what()));
                Ok(Regex::default())
            }
            Err(err) => Err(err),
        }
    });
    Ok(Rc::new(DynamicRegexHighlighter::new(get_regex, resolve_faces)))
}

// Line -----------------------------------------------------------------------

fn line_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: <value string> <face>\n\
                    Highlight the line given by evaluating <value string> with <face>",
        params: ParameterDesc::default(),
    }
}

fn create_line_highlighter(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
    if params.len() != 2 {
        return Err(Error::runtime("wrong parameter count"));
    }

    let line_expr = params[0].clone();
    let facespec = parse_face(&params[1])?;
    let func = move |context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange| -> Result<()> {
        let mut line: LineCount = -1;
        match expand_expr(&line_expr, context.context) {
            Ok(s) => line = str_to_int_ifp(&s).unwrap_or(0) - 1,
            Err(err) if err.is_runtime() => {
                write_to_debug_buffer(&kformat!("Error evaluating highlight line expression: {}", err.what()))
            }
            Err(err) => return Err(err),
        }

        if line < 0 {
            return Ok(());
        }

        let Some(it) = display_buffer.lines_mut().iter_mut().find(|l| l.range().begin.line == line) else {
            return Ok(());
        };

        let face = context.context.faces().get_spec(&facespec);
        let mut column: ColumnCount = 0;
        for atom in it.atoms_mut() {
            column += atom.length();
            if atom.has_buffer_range() && atom.begin().line != line {
                break;
            }
            apply_face(face)(atom);
        }
        let remaining = context.context.try_window()?.dimensions().column - column;
        if remaining > 0 {
            it.push_back(DisplayAtom::text(spaces(remaining), face));
        }
        Ok(())
    };

    Ok(make_highlighter(func, HighlightPass::Colorize))
}

// Column ---------------------------------------------------------------------

fn column_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: [-ruler <character>] <column> <face>\n\
                    Highlight the column <column> with <face>",
        params: ParameterDesc::new(
            switches(vec![(
                "ruler",
                SwitchDesc::arg(
                    "replace empty or whitespace cells with the given character. \
                     When provided, <face> is not applied to non-empty cells",
                ),
            )]),
            ParameterFlags::None,
            2,
            2,
        ),
    }
}

fn create_column_highlighter(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
    let parser = ParametersParser::new(params, &column_desc().params)?;

    let positionals = parser.positionals_from(0);

    let ruler = parser.get_switch("ruler");
    let highlight_non_blank = ruler.is_none();
    let col_char = ruler.unwrap_or(b" ").to_vec();
    if char_length(&col_char) > 1 {
        return Err(Error::runtime("-ruler expects a single character"));
    }

    let col_expr = positionals[0].clone();
    let facespec = parse_face(&positionals[1])?;
    let func = move |context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange| -> Result<()> {
        let mut column: ColumnCount = -1;
        match expand_expr(&col_expr, context.context) {
            Ok(s) => column = str_to_int_ifp(&s).unwrap_or(0) - 1,
            Err(err) if err.is_runtime() => {
                write_to_debug_buffer(&kformat!("Error evaluating highlight column expression: {}", err.what()))
            }
            Err(err) => return Err(err),
        }

        if column < 0 {
            return Ok(());
        }

        let face = context.context.faces().get_spec(&facespec);
        let win_column = context.context.try_window()?.dimensions().column;
        if column < context.setup.first_column || column >= context.setup.first_column + win_column {
            return Ok(());
        }

        column += context.setup.widget_columns - context.setup.first_column;
        for line in display_buffer.lines_mut() {
            let mut remaining_col = column;
            let mut found = false;
            let mut i = 0;
            while i < line.len() {
                let atom_len = line.atoms()[i].length();
                if remaining_col < atom_len {
                    if remaining_col > 0 {
                        i = line.split_at_column(i, remaining_col) + 1;
                    }
                    if line.atoms()[i].length() > 1 {
                        i = line.split_at_column(i, 1);
                    }
                    let atom = &mut line.atoms_mut()[i];
                    if highlight_non_blank {
                        atom.face = merge_faces(&atom.face, &face);
                    } else if atom.content().first().is_some_and(|&c| is_blank(char_to_codepoint(c))) {
                        // [DEVIATION] the C++ reads content()[0] even when the content is empty
                        atom.replace_text(col_char.clone());
                        atom.face = merge_faces(&atom.face, &face);
                    }
                    found = true;
                    break;
                }
                remaining_col -= atom_len;
                i += 1;
            }
            if found {
                continue;
            }

            if remaining_col > 0 {
                line.push_back(DisplayAtom::text(spaces(remaining_col), Face::default()));
            }
            line.push_back(DisplayAtom::text(col_char.clone(), face));
        }
        Ok(())
    };

    Ok(make_highlighter(func, HighlightPass::Colorize))
}

// Wrap -----------------------------------------------------------------------

fn wrap_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: [-word] [-indent] [-width <max_width>] [-marker <marker_text>]\n\
                    Wrap lines to window width",
        params: ParameterDesc::new(
            switches(vec![
                ("word", SwitchDesc::flag("wrap at word boundaries instead of codepoint boundaries")),
                ("indent", SwitchDesc::flag("preserve line indentation of the wrapped line")),
                ("width", SwitchDesc::arg("wrap at the given column instead of the window's width")),
                ("marker", SwitchDesc::arg("insert the given text at the beginning of the wrapped line")),
            ]),
            ParameterFlags::None,
            0,
            0,
        ),
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct SplitPos {
    atom_it: usize,
    byte: ByteCount,
    column: ColumnCount,
}

struct WrapHighlighter {
    word_wrap: bool,
    preserve_indent: bool,
    max_width: ColumnCount,
    marker: Vec<u8>,
}

impl WrapHighlighter {
    const ID: &'static [u8] = b"wrap";

    fn next_split_pos(&self, line: &DisplayLine, pos: &mut SplitPos, wrap_column: ColumnCount, prefix_len: ColumnCount) -> bool {
        let atoms = line.atoms();
        let line_end = atoms.len();
        // [DEVIATION] the C++ dereferences the end iterator on an empty line
        if pos.atom_it >= line_end {
            return false;
        }

        let mut last_word_boundary = SplitPos::default();
        #[allow(non_snake_case)]
        let mut last_WORD_boundary = SplitPos::default();

        let word_wrap = self.word_wrap;
        #[allow(non_snake_case)]
        let update_word_boundaries =
            |cp: Codepoint, pos: &SplitPos, last_word_boundary: &mut SplitPos, last_WORD_boundary: &mut SplitPos| {
                if word_wrap && !is_word(cp, &['_' as Codepoint]) {
                    *last_word_boundary = *pos;
                }
                if word_wrap && is_blank(cp) {
                    *last_WORD_boundary = *pos;
                }
            };

        if atoms[pos.atom_it].type_() != AtomType::Range && pos.column + atoms[pos.atom_it].length() >= wrap_column {
            pos.atom_it += 1;
            return pos.atom_it != line_end;
        }

        let mut content_idx = pos.atom_it;
        let mut content = atoms[content_idx].content();
        while pos.atom_it != line_end && pos.column < wrap_column {
            if content_idx != pos.atom_it {
                content_idx = pos.atom_it;
                content = atoms[content_idx].content();
            }
            let mut it = pos.byte as usize;
            let cp = utf8::read_codepoint(&content, &mut it);
            let width = codepoint_width(cp);
            if pos.column + width > wrap_column {
                // the target column was in the char
                update_word_boundaries(cp, pos, &mut last_word_boundary, &mut last_WORD_boundary);
                break;
            }
            pos.column += width;
            pos.byte = it as ByteCount;
            update_word_boundaries(cp, pos, &mut last_word_boundary, &mut last_WORD_boundary);
            if it >= content.len() {
                pos.atom_it += 1;
                pos.byte = 0;

                if pos.atom_it != line_end
                    && atoms[pos.atom_it].type_() != AtomType::Range
                    && pos.column + atoms[pos.atom_it].length() >= wrap_column
                {
                    return true;
                }

                // Thanks to the pass ordering, atom boundary should always be reasonable word split points
                last_word_boundary = *pos;
                last_WORD_boundary = *pos;
            }
        }
        if pos.atom_it == line_end {
            return false;
        }

        if content_idx != pos.atom_it {
            content = atoms[pos.atom_it].content();
        }
        if self.word_wrap && (pos.byte as usize) < content.len() {
            let find_split_pos = |pos: &mut SplitPos, start_pos: SplitPos, is_word: &dyn Fn(Codepoint) -> bool| -> bool {
                if start_pos.column == 0 {
                    return false;
                }
                let mut it = pos.byte as usize;
                // split at current position if is a word boundary
                if !is_word(utf8::codepoint_at(&content, it)) {
                    return true;
                }
                // split at last word boundary if the word is shorter than our wrapping width
                let mut word_length = pos.column - start_pos.column;
                let max_word_length = wrap_column - prefix_len;
                while it != content.len() && word_length <= max_word_length {
                    let cp = utf8::read_codepoint(&content, &mut it);
                    if !is_word(cp) {
                        break;
                    }
                    word_length += codepoint_width(cp);
                }
                if word_length <= max_word_length {
                    *pos = start_pos;
                    return true;
                }
                false
            };
            #[allow(non_snake_case)]
            let is_WORD = |cp: Codepoint| !is_blank(cp);
            let is_word_ = |cp: Codepoint| is_word(cp, &['_' as Codepoint]);
            if find_split_pos(pos, last_WORD_boundary, &is_WORD) || find_split_pos(pos, last_word_boundary, &is_word_) {
                return true;
            }
        }

        true
    }

    fn line_indent(buffer: &Buffer, tabstop: ColumnCount, line: LineCount) -> ColumnCount {
        let l = buffer.line(line);
        let mut col = 0usize;
        while col < l.len() && is_horizontal_blank(char_to_codepoint(l[col])) {
            col += 1;
        }
        get_column(buffer, tabstop, BufferCoord::new(line, col as ByteCount))
    }

    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        let parser = ParametersParser::new(params, &wrap_desc().params)?;
        let max_width = match parser.get_switch("width") {
            Some(width) => str_to_int(width)?,
            None => i32::MAX,
        };

        Ok(Rc::new(WrapHighlighter {
            word_wrap: parser.has_switch("word"),
            preserve_indent: parser.has_switch("indent"),
            max_width,
            marker: parser.get_switch("marker").unwrap_or(b"").to_vec(),
        }))
    }

    fn zero_if_greater(val: ColumnCount, max: ColumnCount) -> ColumnCount {
        if val < max { val } else { 0 }
    }
}

impl Highlighter for WrapHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Wrap
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
        if contains_id(context.disabled_ids, Self::ID) {
            return Ok(());
        }

        let wrap_column =
            self.max_width.min(context.context.try_window()?.dimensions().column - context.setup.widget_columns);
        if wrap_column <= 0 {
            return Ok(());
        }

        let buffer = context.context.buffer();
        let tabstop = context.context.options().val::<i32>("tabstop");
        let marker_len = Self::zero_if_greater(column_length(&self.marker), wrap_column);
        let face_marker = context.context.faces().face("WrapMarker");
        let lines = display_buffer.lines_mut();
        let mut it = 0;
        while it < lines.len() {
            let indent = if self.preserve_indent {
                Self::zero_if_greater(Self::line_indent(&buffer, tabstop, lines[it].range().begin.line), wrap_column)
            } else {
                0
            };
            let prefix_len = marker_len.max(indent);

            let mut pos = SplitPos::default();
            // [FAITHFUL] loops forever when a line starts with a codepoint wider
            // than the wrap column (each new line splits at its first atom again)
            while self.next_split_pos(&lines[it], &mut pos, wrap_column, prefix_len) {
                let line = &mut lines[it];

                if pos.byte > 0 {
                    let split_coord = line.atoms()[pos.atom_it].begin() + BufferCoord::new(0, pos.byte);
                    pos.atom_it = line.split_at_coord(pos.atom_it, split_coord) + 1;
                }

                let coord = atom_begin(&line.atoms()[pos.atom_it]);
                let mut new_line = DisplayLine::new(line.atoms()[pos.atom_it..].to_vec());
                let len = line.len();
                line.erase(pos.atom_it, len);
                let mut next_atom_it = 0;

                if marker_len != 0 {
                    next_atom_it = new_line.insert(0, DisplayAtom::text(self.marker.clone(), face_marker)) + 1;
                }
                if indent > marker_len {
                    next_atom_it = new_line.insert(
                        next_atom_it,
                        DisplayAtom::range(&buffer, BufferRange::new(coord, coord), Face::default()),
                    );
                    new_line.atoms_mut()[next_atom_it].replace_text(spaces(indent - marker_len));
                    next_atom_it += 1;
                }

                lines.insert(it + 1, new_line);
                it += 1;
                pos = SplitPos { atom_it: next_atom_it, byte: 0, column: marker_len.max(indent) };
            }
            it += 1;
        }
        Ok(())
    }

    fn do_compute_display_setup(&self, context: &HighlightContext, setup: &mut DisplaySetup) -> Result<()> {
        if contains_id(context.disabled_ids, Self::ID) {
            return Ok(());
        }

        let wrap_column = self.max_width.min(context.context.try_window()?.dimensions().column - setup.widget_columns);
        if wrap_column <= 0 {
            return Ok(());
        }

        // Disable horizontal scrolling when using a WrapHighlighter
        setup.first_column = 0;
        setup.scroll_offset.column = 0;
        Ok(())
    }

    fn fill_unique_ids(&self, unique_ids: &mut Vec<Vec<u8>>) {
        unique_ids.push(Self::ID.to_vec());
    }
}

// Tabulations ----------------------------------------------------------------

struct TabulationHighlighter;

impl Highlighter for TabulationHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Replace
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
        let tabstop: ColumnCount = context.context.options().val::<i32>("tabstop");
        let buffer = context.context.buffer();
        for line in display_buffer.lines_mut() {
            let mut column: ColumnCount = 0;
            // C++ compares line data pointers, we compare line numbers
            let mut line_data: Option<(LineCount, StringDataPtr)> = None;
            let mut pos: usize = 0;
            let mut atom_it = 0;
            while atom_it < line.len() {
                if line.atoms()[atom_it].type_() != AtomType::Range {
                    atom_it += 1;
                    continue;
                }

                let begin = line.atoms()[atom_it].begin();
                if line_data.as_ref().is_none_or(|(l, _)| *l != begin.line) {
                    line_data = Some((begin.line, buffer.line(begin.line)));
                    pos = 0;
                    column = 0;
                }
                let data = line_data.as_ref().unwrap().1.clone();

                debug_assert!(pos <= begin.column as usize);
                let end = line.atoms()[atom_it].end().column as usize;
                // [DEVIATION] `pos < end` where the C++ loops on `pos != end`, which
                // would run past the line if an atom ended before pos
                while pos < end {
                    let Some(next_tab) = memchr::memchr(b'\t', &data[pos..end]).map(|p| p + pos) else {
                        pos = end;
                        break;
                    };

                    while pos < next_tab {
                        column += codepoint_width(utf8::read_codepoint(&data[..next_tab], &mut pos));
                    }
                    let tabwidth = tabstop - (column % tabstop);
                    column += tabwidth;

                    if atom_it < line.len() && pos >= line.atoms()[atom_it].begin().column as usize {
                        if pos != line.atoms()[atom_it].begin().column as usize {
                            atom_it = line.split_at_coord(atom_it, BufferCoord::new(begin.line, pos as ByteCount)) + 1;
                        }
                        if pos + 1 != end {
                            atom_it = line.split_at_coord(atom_it, BufferCoord::new(begin.line, (pos + 1) as ByteCount));
                        }

                        line.atoms_mut()[atom_it].replace_text(spaces(tabwidth));
                        atom_it += 1;
                    }
                    pos += 1;
                }

                if atom_it >= line.len() {
                    break;
                }
                // [FAITHFUL] when a tab ended the atom, atom_it already points to the
                // next atom, which this increment skips (its tabs are not expanded)
                atom_it += 1;
            }
        }
        Ok(())
    }

    fn do_compute_display_setup(&self, context: &HighlightContext, setup: &mut DisplaySetup) -> Result<()> {
        let buffer = context.context.buffer();
        // Ensure that a cursor on a tab character makes the full tab character visible
        let cursor = context.context.selections().main().cursor().coord;
        if buffer.byte_at(cursor) != b'\t' {
            return Ok(());
        }

        let tabstop: ColumnCount = context.context.options().val::<i32>("tabstop");
        let column = get_column(&buffer, tabstop, cursor);
        let width = tabstop - (column % tabstop);
        let win_end =
            setup.first_column + context.context.try_window()?.dimensions().column - setup.widget_columns;
        let offset = (column + width - win_end).max(0);

        setup.first_column += offset;
        Ok(())
    }
}

// Show whitespaces -----------------------------------------------------------

fn show_whitespace_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: [-tab <separator>] [-tabpad <separator>] [-lf <separator>] [-spc <separator>] \
                    [-nbsp <separator>] [-indent <separator>]\n\
                    Display whitespaces using symbols",
        params: ParameterDesc::new(
            switches(vec![
                ("tab", SwitchDesc::arg("replace tabulations with the given character")),
                (
                    "tabpad",
                    SwitchDesc::arg("append as many of the given character as is necessary to honor `tabstop`"),
                ),
                ("spc", SwitchDesc::arg("replace spaces with the given character")),
                ("lf", SwitchDesc::arg("replace line feeds with the given character")),
                ("nbsp", SwitchDesc::arg("replace non-breakable spaces with the given character")),
                (
                    "indent",
                    SwitchDesc::arg(
                        "replace first space of every indent with the given character according to `indentwidth`",
                    ),
                ),
                ("only-trailing", SwitchDesc::flag("only highlighting trailing whitespaces")),
            ]),
            ParameterFlags::None,
            0,
            0,
        ),
    }
}

struct ShowWhitespacesHighlighter {
    tab: Vec<u8>,
    tabpad: Vec<u8>,
    spc: Vec<u8>,
    lf: Vec<u8>,
    nbsp: Vec<u8>,
    indent: Vec<u8>,
    only_trailing: bool,
}

impl ShowWhitespacesHighlighter {
    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        let parser = ParametersParser::new(params, &show_whitespace_desc().params)?;

        let only_trailing = parser.has_switch("only-trailing");
        let get_param = |param: &str, fallback: &str| -> Result<Vec<u8>> {
            let value = parser.get_switch(param).unwrap_or(fallback.as_bytes());
            if char_length(value) > 1 {
                return Err(rt_err!("-{} expects a single character or empty parameter", param));
            }
            Ok(value.to_vec())
        };

        Ok(Rc::new(ShowWhitespacesHighlighter {
            tab: get_param("tab", "→")?,
            tabpad: get_param("tabpad", " ")?,
            spc: get_param("spc", "·")?,
            lf: get_param("lf", "¬")?,
            nbsp: get_param("nbsp", "⍽")?,
            indent: get_param("indent", "│")?,
            only_trailing,
        }))
    }
}

impl Highlighter for ShowWhitespacesHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Replace
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
        let options = context.context.options();
        let tabstop: ColumnCount = options.val::<i32>("tabstop");
        let indentwidth: ColumnCount = options.val::<i32>("indentwidth");
        let faces = context.context.faces();
        let whitespaceface = faces.face("Whitespace");
        let indentface = faces.face("WhitespaceIndent");
        let buffer = context.context.buffer();
        let lines = buffer.lines();
        let is_whitespace = |cp: Codepoint| {
            cp == '\t' as Codepoint || cp == ' ' as Codepoint || cp == '\n' as Codepoint || cp == 0xA0 || cp == 0x202F
        };
        for line in display_buffer.lines_mut() {
            let mut is_indentation = true;
            let mut atom_it = 0;
            while atom_it < line.len() {
                if line.atoms()[atom_it].type_() != AtomType::Range {
                    atom_it += 1;
                    continue;
                }

                let begin = BufferIterator::new(&lines, get_iterator_coord(&buffer, line.atoms()[atom_it].begin()));
                let end = BufferIterator::new(&lines, get_iterator_coord(&buffer, line.atoms()[atom_it].end()));
                let mut last_non_space = begin.coord();

                if self.only_trailing {
                    let mut it = begin;
                    while it != end {
                        if !is_whitespace(utf8::it_read_codepoint(&mut it, &end)) {
                            last_non_space = it.coord();
                        }
                    }
                }

                let mut it = begin;
                while it != end {
                    let coord = it.coord();
                    let cp = utf8::it_read_codepoint(&mut it, &end);
                    let mut face = whitespaceface;
                    if is_whitespace(cp) {
                        if self.only_trailing && it.coord() <= last_non_space {
                            continue;
                        }

                        if coord != begin.coord() {
                            atom_it = line.split_at_coord(atom_it, coord) + 1;
                        }
                        if it != end {
                            atom_it = line.split_at_coord(atom_it, it.coord());
                        }

                        let atom = &mut line.atoms_mut()[atom_it];
                        if cp == '\t' as Codepoint && !self.tab.is_empty() && !self.tabpad.is_empty() {
                            let column = get_column(&buffer, tabstop, coord);
                            let count = tabstop - (column % tabstop);
                            let mut text = self.tab.clone();
                            text.extend(string_of_cp(utf8::codepoint(&self.tabpad), count - column_length(&self.tab)));
                            atom.replace_text(text);
                        } else if cp == ' ' as Codepoint && is_indentation && indentwidth > 0 && !self.indent.is_empty() {
                            let column = get_column(&buffer, tabstop, coord);
                            if column % indentwidth == 0 && column != 0 {
                                atom.replace_text(self.indent.clone());
                                face = indentface;
                            } else {
                                atom.replace_text(self.spc.clone());
                            }
                        } else if cp == ' ' as Codepoint && !self.spc.is_empty() {
                            atom.replace_text(self.spc.clone());
                        } else if cp == '\n' as Codepoint && !self.lf.is_empty() {
                            atom.replace_text(self.lf.clone());
                        } else if (cp == 0xA0 || cp == 0x202F) && !self.nbsp.is_empty() {
                            atom.replace_text(self.nbsp.clone());
                        }
                        atom.face = merge_faces(&atom.face, &face);
                        break;
                    } else {
                        is_indentation = false;
                    }
                }
                atom_it += 1;
            }
        }
        Ok(())
    }
}

// Line numbers ---------------------------------------------------------------

fn line_numbers_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: [-relative] [-hlcursor] [-separators <separator|separator:cursor|cursor:up:down>] \
                    [-min-digits <cols>]\n\
                    Display line numbers",
        params: ParameterDesc::new(
            switches(vec![
                ("relative", SwitchDesc::flag("show line numbers relative to the main cursor line")),
                (
                    "full-relative",
                    SwitchDesc::flag("show line numbers relative to the main cursor line and the main cursor line as 0"),
                ),
                (
                    "separator",
                    SwitchDesc::arg(
                        "string to separate the line numbers column from the rest of the buffer (default '|')",
                    ),
                ),
                (
                    "cursor-separator",
                    SwitchDesc::arg(
                        "identical to -separator but applies only to the line of the cursor \
                         (default is the same value passed to -separator)",
                    ),
                ),
                (
                    "min-digits",
                    SwitchDesc::arg("use at least the given number of columns to display line numbers (default 2)"),
                ),
                ("hlcursor", SwitchDesc::flag("highlight the cursor line with a separate face")),
            ]),
            ParameterFlags::None,
            0,
            0,
        ),
    }
}

struct LineNumbersHighlighter {
    relative: bool,
    zero_cursor_line: bool,
    hl_cursor_line: bool,
    separator: Vec<u8>,
    cursor_separator: Vec<u8>,
    min_digits: i32,
}

impl LineNumbersHighlighter {
    const ID: &'static [u8] = b"line-numbers";

    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        let parser = ParametersParser::new(params, &line_numbers_desc().params)?;

        let separator = parser.get_switch("separator").unwrap_or("│".as_bytes());
        let cursor_separator = parser.get_switch("cursor-separator").unwrap_or(separator);

        if separator.len() > 10 {
            return Err(Error::runtime("separator length is limited to 10 bytes"));
        }

        if column_length(cursor_separator) != column_length(separator) {
            return Err(Error::runtime("separator for active line should have the same length as 'separator'"));
        }

        let min_digits = match parser.get_switch("min-digits") {
            Some(s) => str_to_int(s)?,
            None => 2,
        };
        if min_digits < 0 {
            return Err(Error::runtime("min digits must be positive"));
        }
        if min_digits > 10 {
            return Err(Error::runtime("min digits is limited to 10"));
        }
        let relative = parser.has_switch("relative");
        let full_relative = parser.has_switch("full-relative");
        Ok(Rc::new(LineNumbersHighlighter {
            relative: relative || full_relative,
            zero_cursor_line: full_relative,
            hl_cursor_line: parser.has_switch("hlcursor"),
            separator: separator.to_vec(),
            cursor_separator: cursor_separator.to_vec(),
            min_digits,
        }))
    }

    fn compute_digit_count(&self, context: &HighlightContext) -> i32 {
        let mut digit_count = 0;
        let cursor_line = context.context.selections().main().cursor().line + 1;
        let line_count: LineCount = if self.relative && self.zero_cursor_line {
            (context.setup.first_line - cursor_line)
                .abs()
                .max((context.setup.first_line + context.setup.line_count - cursor_line).abs())
        } else {
            context.context.buffer().line_count()
        };
        let mut c = line_count;
        while c > 0 {
            digit_count += 1;
            c /= 10;
        }
        digit_count.max(self.min_digits)
    }
}

impl Highlighter for LineNumbersHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Move
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
        if contains_id(context.disabled_ids, Self::ID) {
            return Ok(());
        }

        let faces = context.context.faces();
        let face = faces.face("LineNumbers");
        let face_wrapped = faces.face("LineNumbersWrapped");
        let face_absolute = faces.face("LineNumberCursor");
        let digit_count = self.compute_digit_count(context) as usize;

        let main_line = context.context.selections().main().cursor().line + 1;
        let mut last_line = -1;
        for line in display_buffer.lines_mut() {
            let current_line = line.range().begin.line + 1;
            let is_cursor_line = main_line == current_line;
            let line_to_format = if self.relative && (!is_cursor_line || self.zero_cursor_line) {
                current_line - main_line
            } else {
                current_line
            };
            let buffer = format!("{:>width$}", line_to_format.abs(), width = digit_count).into_bytes();
            let atom_face = if last_line == current_line {
                face_wrapped
            } else if self.hl_cursor_line && is_cursor_line {
                face_absolute
            } else {
                face
            };

            let separator =
                if is_cursor_line && last_line != current_line { &self.cursor_separator } else { &self.separator };

            line.insert(0, DisplayAtom::text(buffer, atom_face));
            line.insert(1, DisplayAtom::text(separator.clone(), face));

            last_line = current_line;
        }
        Ok(())
    }

    fn do_compute_display_setup(&self, context: &HighlightContext, setup: &mut DisplaySetup) -> Result<()> {
        if contains_id(context.disabled_ids, Self::ID) {
            return Ok(());
        }

        let width = self.compute_digit_count(context) + column_length(&self.separator);
        setup.widget_columns += width;
        Ok(())
    }

    fn fill_unique_ids(&self, unique_ids: &mut Vec<Vec<u8>>) {
        unique_ids.push(Self::ID.to_vec());
    }
}

// Show matching --------------------------------------------------------------

fn show_matching_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Apply the MatchingChar face to the char matching the one under the cursor",
        params: ParameterDesc::new(
            switches(vec![("previous", SwitchDesc::flag(""))]),
            ParameterFlags::SwitchesOnlyAtStart,
            0,
            0,
        ),
    }
}

fn show_matching_char(
    match_prev: bool,
    context: &HighlightContext,
    display_buffer: &mut DisplayBuffer,
    _: BufferRange,
) -> Result<()> {
    let face = context.context.faces().face("MatchingChar");
    let matching_pairs: Vec<Codepoint> = context
        .context
        .options()
        .get(b"matching_pairs")?
        .try_get::<Vec<CodepointOpt>>()?
        .iter()
        .map(|c| c.0)
        .collect();
    let range = display_buffer.range();
    let buffer = context.context.buffer();
    let cursors: Vec<BufferCoord> = context.context.selections().iter().map(|s| s.cursor().coord).collect();
    let lines = buffer.lines();
    let buf_begin = BufferIterator::begin(&lines);
    let buf_end = BufferIterator::end(&lines);
    let find = |cp: Codepoint| matching_pairs.iter().position(|&c| c == cp);
    for pos in cursors {
        if pos < range.begin || pos >= range.end {
            continue;
        }

        let mut it = Utf8Iter::new(BufferIterator::new(&lines, pos), buf_begin, buf_end);
        let mut m = find(it.get());
        let matching_prev = m.is_none() && match_prev;
        if matching_prev {
            it.dec();
            m = find(it.get());
        }

        let Some(m) = m else { continue };

        if matching_prev {
            highlight_range(display_buffer, it.base().coord(), it.add(1).base().coord(), false, apply_face(face));
        }

        let mut level = 0;
        if m % 2 == 0 {
            let opening = matching_pairs[m];
            // [DEVIATION] the C++ reads past the end of an odd length matching_pairs
            let Some(&closing) = matching_pairs.get(m + 1) else { continue };
            while it.base().coord() <= range.end {
                let c = it.get();
                if c == opening {
                    level += 1;
                } else if c == closing {
                    level -= 1;
                    if level == 0 {
                        highlight_range(display_buffer, it.base().coord(), it.add(1).base().coord(), false, apply_face(face));
                        break;
                    }
                }
                // [DEVIATION] the C++ dereferences the buffer end iterator (and never
                // moves past it) when the display range reaches the buffer end
                if it.at_end() {
                    break;
                }
                it.inc();
            }
        } else if pos > range.begin {
            let opening = matching_pairs[m - 1];
            let closing = matching_pairs[m];
            loop {
                let c = it.get();
                if c == closing {
                    level += 1;
                } else if c == opening {
                    level -= 1;
                    if level == 0 {
                        highlight_range(display_buffer, it.base().coord(), it.add(1).base().coord(), false, apply_face(face));
                        break;
                    }
                }
                if it.base().coord() <= range.begin {
                    break;
                }
                it.dec();
            }
        }
    }
    Ok(())
}

fn create_matching_char_highlighter(
    params: HighlighterParameters,
    _: Option<&dyn Highlighter>,
) -> Result<Rc<dyn Highlighter>> {
    let parser = ParametersParser::new(params, &show_matching_desc().params)?;
    let match_prev = parser.has_switch("previous");
    Ok(make_highlighter(
        move |context, display_buffer, range| show_matching_char(match_prev, context, display_buffer, range),
        HighlightPass::Colorize,
    ))
}

// Builtin highlighters -------------------------------------------------------

fn highlight_selections(context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
    let buffer = context.context.buffer();
    let faces = context.context.faces();
    let sel_faces: [Face; 6] = [
        faces.face("PrimarySelection"),
        faces.face("SecondarySelection"),
        faces.face("PrimaryCursor"),
        faces.face("SecondaryCursor"),
        faces.face("PrimaryCursorEol"),
        faces.face("SecondaryCursorEol"),
    ];

    let (selections, main_index): (Vec<(BufferCoord, BufferCoord)>, usize) = {
        let sels = context.context.selections();
        (sels.iter().map(|s| (s.anchor(), s.cursor().coord)).collect(), sels.main_index())
    };
    for (i, &(anchor, cursor)) in selections.iter().enumerate() {
        let forward = anchor <= cursor;
        let begin = if forward { anchor } else { buffer.char_next(cursor) };
        let end = if forward { cursor } else { buffer.char_next(anchor) };

        let primary = i == main_index;
        highlight_range(display_buffer, begin, end, false, apply_face(sel_faces[if primary { 0 } else { 1 }]));
    }
    for (i, &(_, coord)) in selections.iter().enumerate() {
        let primary = i == main_index;
        let eol = buffer.line_length(coord.line) - 1 == coord.column;
        highlight_range(
            display_buffer,
            coord,
            buffer.char_next(coord),
            false,
            apply_face(sel_faces[2 + if eol { 2 } else { 0 } + if primary { 0 } else { 1 }]),
        );
    }
    Ok(())
}

fn expand_unprintable(context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
    // follow musl iswprint set of printable characters, but with straightforward impl
    let is_printable = |cp: Codepoint| {
        if cp < 0xff {
            cp == '\n' as Codepoint || cp >= ' ' as Codepoint
        } else {
            !(cp == 0x2028 || cp == 0x2029 || (0xFFF9..=0xFFFB).contains(&cp))
        }
    };
    let buffer = context.context.buffer();
    let error = context.context.faces().get_spec(&FaceSpec { face: Face::default(), base: b"Error".to_vec() });
    for line in display_buffer.lines_mut() {
        let mut atom_it = 0;
        while atom_it < line.len() {
            if line.atoms()[atom_it].type_() != AtomType::Range {
                atom_it += 1;
                continue;
            }

            let begin = line.atoms()[atom_it].begin();
            let line_data = buffer.line(begin.line);
            // [DEVIATION] the C++ does not bound the atom end to the line data
            let end = (line.atoms()[atom_it].end().column as usize).min(line_data.len());
            let mut it = begin.column as usize;
            while it < end {
                let mut next = it;
                let cp = utf8::read_codepoint(&line_data[..end], &mut next);
                if !is_printable(cp) {
                    if it as ByteCount != begin.column {
                        atom_it = line.split_at_coord(atom_it, BufferCoord::new(begin.line, it as ByteCount)) + 1;
                    }
                    if (next as ByteCount) < line.atoms()[atom_it].end().column {
                        atom_it = line.split_at_coord(atom_it, BufferCoord::new(begin.line, next as ByteCount));
                    }

                    let atom = &mut line.atoms_mut()[atom_it];
                    atom.replace_text("�".as_bytes().to_vec());
                    atom.face = error;
                    break;
                }
                it = next;
            }
            atom_it += 1;
        }
    }
    Ok(())
}

// Line specs -----------------------------------------------------------------

pub type LineAndSpec = (LineCount, Vec<u8>);
pub type LineAndSpecList = TimestampedList<LineAndSpec>;

fn update_line_specs_ifn(buffer: &Buffer, line_flags: &mut LineAndSpecList) {
    if line_flags.prefix == buffer.timestamp() {
        return;
    }

    let modifs = compute_line_modifications(buffer, line_flags.prefix);
    line_flags.list.retain_mut(|(line, _)| {
        // that line is 1 based as it comes from user side
        let modif_it = modifs.partition_point(|c| c.old_line < *line);
        if modif_it != 0 {
            let prev = &modifs[modif_it - 1];
            if *line - 1 < prev.old_line + prev.num_removed {
                return false; // line removed
            }

            *line += prev.diff();
        }
        true
    });
    line_flags.prefix = buffer.timestamp();
}

impl OptionElem for LineAndSpec {
    fn elem_type_name() -> Vec<u8> {
        b"line-and-spec".to_vec()
    }
    fn elem_to_string(&self, quoting: Quoting) -> Vec<u8> {
        tuple_to_string(quoting, &[self.0.to_string().into_bytes(), self.1.clone()])
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        let mut elems = tuple_from_string(s, 2)?;
        let spec = elems.pop().unwrap();
        Ok((str_to_int(&elems[0])?, spec))
    }
    /// option_list_postprocess
    fn list_postprocess(list: &mut Vec<Self>) {
        // [DEVIATION] stable sort, std::sort leaves the order of equal lines unspecified
        list.sort_by_key(|l| l.0);
    }
}

impl PrefixedListType for LineAndSpecList {
    fn type_name() -> Vec<u8> {
        b"line-specs".to_vec()
    }
    /// option_update
    fn update(&mut self, context: &Context) -> Result<()> {
        let buffer = context.try_buffer()?;
        update_line_specs_ifn(&buffer, self);
        Ok(())
    }
}

fn flag_lines_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: <face> <option name>\n\
                    Display flags specified in the line-spec option <option name> with <face>",
        params: ParameterDesc::default(),
    }
}

struct FlagLinesHighlighter {
    option_name: Vec<u8>,
    default_face: Vec<u8>,
    after: bool,
}

impl FlagLinesHighlighter {
    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        let parser = ParametersParser::new(
            params,
            &ParameterDesc::new(
                switches(vec![("after", SwitchDesc::flag("display at line end"))]),
                ParameterFlags::SwitchesOnlyAtStart,
                2,
                2,
            ),
        )?;

        let default_face = parser[0].to_vec();
        let option_name = parser[1].to_vec();

        // throw if wrong option type
        GlobalScope::instance().options().get(&option_name)?.try_get::<LineAndSpecList>()?;

        Ok(Rc::new(FlagLinesHighlighter { option_name, default_face, after: parser.has_switch("after") }))
    }

    /// Get the (updated) line specs
    fn get_lines(&self, context: &Context) -> Result<Vec<LineAndSpec>> {
        let option = context.options().get(&self.option_name)?;
        let buffer = context.try_buffer()?;
        let mut line_flags = option.try_get_mutable::<LineAndSpecList>()?;
        update_line_specs_ifn(&buffer, &mut line_flags);
        Ok(line_flags.list.clone())
    }
}

impl Highlighter for FlagLinesHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Move
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
        let lines = self.get_lines(context.context)?;

        let faces = context.context.faces();
        let def_face = faces.get(&self.default_face)?;
        let mut display_lines: Vec<DisplayLine> = Vec::new();
        for line in &lines {
            match parse_display_line(&line.1, &faces, &no_builtins()) {
                Ok(mut display_line) => {
                    for atom in display_line.atoms_mut() {
                        atom.face = merge_faces(&def_face, &atom.face);
                    }
                    display_lines.push(display_line);
                }
                Err(err) if err.is_runtime() => {
                    write_to_debug_buffer(&kformat!("Error while evaluating line flag: {}", err.what()));
                    return Ok(());
                }
                Err(err) => return Err(err),
            }
        }

        let mut width: ColumnCount = 0;
        for l in &display_lines {
            width = width.max(l.length());
        }
        let empty = DisplayAtom::text(spaces(width), def_face);
        // [FAITHFUL] Atoms are moved out of the flag display lines in the C++ code,
        // which leaves empty atoms behind when a buffer line is displayed multiple
        // times (wrapped lines then get empty atoms and a full width padding)
        let take_atoms = |display_line: &mut DisplayLine| -> Vec<DisplayAtom> {
            display_line
                .atoms_mut()
                .iter_mut()
                .map(|atom| {
                    let face = atom.face;
                    std::mem::replace(atom, DisplayAtom::text(Vec::new(), face))
                })
                .collect()
        };
        for line in display_buffer.lines_mut() {
            let line_num = line.range().begin.line + 1;
            let it = lines.iter().position(|l| l.0 == line_num);
            if self.after {
                if let Some(it) = it {
                    for atom in take_atoms(&mut display_lines[it]) {
                        let len = line.len();
                        line.insert(len, atom);
                    }
                }
                continue;
            }
            match it {
                None => {
                    line.insert(0, empty.clone());
                }
                Some(it) => {
                    let display_line = &mut display_lines[it];
                    let padding_atom = DisplayAtom::text(spaces(width - display_line.length()), def_face);
                    let mut pos = 0;
                    for atom in take_atoms(display_line) {
                        pos = line.insert(pos, atom) + 1;
                    }

                    if padding_atom.length() != 0 {
                        line.insert(pos, padding_atom);
                    }
                }
            }
        }
        Ok(())
    }

    fn do_compute_display_setup(&self, context: &HighlightContext, setup: &mut DisplaySetup) -> Result<()> {
        if self.after {
            return Ok(());
        }

        let lines = self.get_lines(context.context)?;

        let faces = context.context.faces();
        let mut width: ColumnCount = 0;
        for line in &lines {
            match parse_display_line(&line.1, &faces, &no_builtins()) {
                Ok(display_line) => width = display_line.length().max(width),
                Err(err) if err.is_runtime() => {
                    write_to_debug_buffer(&kformat!("Error while evaluating line flag: {}", err.what()));
                    return Ok(());
                }
                Err(err) => return Err(err),
            }
        }

        setup.widget_columns += width;
        Ok(())
    }
}

// Range specs ----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct InclusiveBufferRange {
    pub first: BufferCoord,
    pub last: BufferCoord,
}

pub fn is_empty(range: &InclusiveBufferRange) -> bool {
    range.last < BufferCoord::new(0, 0)
}

/// option_to_string(InclusiveBufferRange)
pub fn inclusive_buffer_range_to_string(range: &InclusiveBufferRange) -> Vec<u8> {
    if is_empty(range) {
        return kformat!("{}.{}+0", range.first.line + 1, range.first.column + 1);
    }
    kformat!("{}.{},{}.{}", range.first.line + 1, range.first.column + 1, range.last.line + 1, range.last.column + 1)
}

/// option_from_string(Meta::Type<InclusiveBufferRange>, StringView)
pub fn inclusive_buffer_range_from_string(str: &[u8]) -> Result<InclusiveBufferRange> {
    let end = str.len();
    let sep = str.iter().position(|&c| c == b',' || c == b'+').unwrap_or(end);
    let dot_beg = str[..sep].iter().position(|&c| c == b'.').unwrap_or(sep);
    let dot_end = str[sep..].iter().position(|&c| c == b'.').map(|p| p + sep).unwrap_or(end);

    if sep == end || dot_beg == sep || (str[sep] == b',' && dot_end == end) {
        return Err(rt_err!(
            "'{}' does not follow <line>.<column>,<line>.<column> or <line>.<column>+<len> format",
            str
        ));
    }

    let first = BufferCoord::new(str_to_int(&str[..dot_beg])? - 1, str_to_int(&str[dot_beg + 1..sep])? - 1);

    if first.line < 0 || first.column < 0 {
        return Err(Error::runtime("coordinates elements should be >= 1"));
    }

    if str[sep] == b'+' {
        let len = str_to_int(&str[sep + 1..])?;
        return Ok(InclusiveBufferRange {
            first,
            last: if len == 0 { BufferCoord::new(-1, -1) } else { BufferCoord::new(first.line, first.column + len - 1) },
        });
    }

    let last = BufferCoord::new(str_to_int(&str[sep + 1..dot_end])? - 1, str_to_int(&str[dot_end + 1..])? - 1);
    if last.line < 0 || last.column < 0 {
        return Err(Error::runtime("coordinates elements should be >= 1"));
    }

    Ok(InclusiveBufferRange { first: first.min(last), last: first.max(last) })
}

impl OptionElem for InclusiveBufferRange {
    fn elem_type_name() -> Vec<u8> {
        b"inclusive-buffer-range".to_vec()
    }
    fn elem_to_string(&self, _: Quoting) -> Vec<u8> {
        inclusive_buffer_range_to_string(self)
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        inclusive_buffer_range_from_string(s)
    }
}

pub type RangeAndString = (InclusiveBufferRange, Vec<u8>);
pub type RangeAndStringList = TimestampedList<RangeAndString>;

impl UpdatableRange for RangeAndString {
    fn first_last(&mut self) -> (&mut BufferCoord, &mut BufferCoord) {
        (&mut self.0.first, &mut self.0.last)
    }
    fn last(&self) -> BufferCoord {
        self.0.last
    }
}

fn option_element_compare(lhs: &RangeAndString, rhs: &RangeAndString) -> std::cmp::Ordering {
    if lhs.0.first == rhs.0.first { lhs.0.last.cmp(&rhs.0.last) } else { lhs.0.first.cmp(&rhs.0.first) }
}

impl OptionElem for RangeAndString {
    fn elem_type_name() -> Vec<u8> {
        b"range-and-string".to_vec()
    }
    fn elem_to_string(&self, quoting: Quoting) -> Vec<u8> {
        tuple_to_string(quoting, &[inclusive_buffer_range_to_string(&self.0), self.1.clone()])
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        let mut elems = tuple_from_string(s, 2)?;
        let string = elems.pop().unwrap();
        Ok((inclusive_buffer_range_from_string(&elems[0])?, string))
    }
    /// option_list_postprocess
    fn list_postprocess(list: &mut Vec<Self>) {
        // [DEVIATION] stable sort, std::sort leaves the order of equal ranges unspecified
        list.sort_by(option_element_compare);
    }
    /// option_add_from_strings
    fn list_add_from_strings(list: &mut Vec<Self>, strs: &[Vec<u8>]) -> Result<bool> {
        let vec = list_from_strings::<Self>(strs)?;
        if vec.is_empty() {
            return Ok(false);
        }
        let middle = list.len();
        list.extend(vec);
        list[middle..].sort_by(option_element_compare); // [DEVIATION] stable sort
        inplace_merge(list, middle, |lhs, rhs| option_element_compare(lhs, rhs) == std::cmp::Ordering::Less);
        Ok(true)
    }
}

impl PrefixedListType for RangeAndStringList {
    fn type_name() -> Vec<u8> {
        b"range-specs".to_vec()
    }
    /// option_update
    fn update(&mut self, context: &Context) -> Result<()> {
        let buffer = context.try_buffer()?;
        update_ranges(&buffer, self.prefix, &mut self.list);
        self.prefix = buffer.timestamp();
        Ok(())
    }
}

/// OptionBasedHighlighter: checks the option type at creation
fn check_option_based_params<T: crate::option_types::OptionType>(params: HighlighterParameters) -> Result<Vec<u8>> {
    if params.len() != 1 {
        return Err(Error::runtime("wrong parameter count"));
    }

    let option_name = &params[0];
    // throw if wrong option type
    GlobalScope::instance().options().get(option_name)?.try_get::<T>()?;
    Ok(option_name.clone())
}

/// get_option, updating the ranges, returns a copy of the list
fn get_updated_ranges(context: &Context, option_name: &[u8], buffer: &Buffer) -> Result<Vec<RangeAndString>> {
    let option = context.options().get(option_name)?;
    let mut range_and_faces = option.try_get_mutable::<RangeAndStringList>()?;
    update_ranges(buffer, range_and_faces.prefix, &mut range_and_faces.list);
    range_and_faces.prefix = buffer.timestamp();
    Ok(range_and_faces.list.clone())
}

fn ranges_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: <option name>\n\
                    Use the range-specs option given as parameter to highlight buffer\n\
                    each spec is interpreted as a face to apply to the range",
        params: ParameterDesc::default(),
    }
}

struct RangesHighlighter {
    option_name: Vec<u8>,
}

impl RangesHighlighter {
    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        let option_name = check_option_based_params::<RangeAndStringList>(params)?;
        Ok(Rc::new(RangesHighlighter { option_name }))
    }
}

impl Highlighter for RangesHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Colorize
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
        let buffer = context.context.buffer();
        let range_and_faces = get_updated_ranges(context.context, &self.option_name, &buffer)?;

        let faces = context.context.faces();
        for (range, face) in &range_and_faces {
            if buffer.is_valid(range.first)
                && (buffer.is_valid(range.last) && !buffer.is_end(range.last))
                && let Ok(face) = faces.get(face)
            {
                highlight_range(display_buffer, range.first, buffer.char_next(range.last), false, apply_face(face));
            }
        }
        Ok(())
    }
}

fn replace_ranges_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: <option name>\n\
                    Use the range-specs option given as parameter to highlight buffer\n\
                    each spec is interpreted as a display line to display in place of the range",
        params: ParameterDesc::default(),
    }
}

struct ReplaceRangesHighlighter {
    option_name: Vec<u8>,
}

impl ReplaceRangesHighlighter {
    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        let option_name = check_option_based_params::<RangeAndStringList>(params)?;
        Ok(Rc::new(ReplaceRangesHighlighter { option_name }))
    }

    fn is_valid(buffer: &Buffer, c: BufferCoord) -> bool {
        c.line >= 0 && c.column >= 0 && c.line < buffer.line_count() && c.column <= buffer.line_length(c.line)
    }

    /// sels are (min, max) pairs
    fn is_fully_selected(sels: &[(BufferCoord, BufferCoord)], range: &InclusiveBufferRange) -> bool {
        let it = sels.partition_point(|s| s.1 < range.first);
        if it == sels.len() {
            return true;
        }
        let (min, max) = sels[it];
        min > range.last || (min <= range.first && max >= range.last)
    }

    fn selections(context: &Context) -> Vec<(BufferCoord, BufferCoord)> {
        context.selections().iter().map(|s| (s.min(), s.max())).collect()
    }
}

impl Highlighter for ReplaceRangesHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Replace
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, _: BufferRange) -> Result<()> {
        let buffer = context.context.buffer();
        let sels = Self::selections(context.context);
        let range_and_faces = get_updated_ranges(context.context, &self.option_name, &buffer)?;

        let faces = context.context.faces();
        for (range, spec) in &range_and_faces {
            if !Self::is_valid(&buffer, range.first)
                || (!is_empty(range) && !Self::is_valid(&buffer, range.last))
                || !Self::is_fully_selected(&sels, range)
            {
                continue;
            }
            let end = if is_empty(range) { range.first } else { buffer.char_next(range.last) };
            let replaced = BufferRange::new(range.first, end);
            let res = replace_range(display_buffer, range.first, end, |lines, mut line, mut pos| {
                let mut it = 0;
                let mut eol = spec.iter().position(|&c| c == b'\n').unwrap_or(spec.len());
                loop {
                    for mut atom in parse_display_line(&spec[it..eol], &faces, &no_builtins())?.atoms().iter().cloned() {
                        atom.replace_range(replaced);
                        pos = lines[line].insert(pos, atom) + 1;
                    }
                    if eol == spec.len() {
                        break;
                    }
                    let len = lines[line].len();
                    let remaining = lines[line].extract(pos, len);
                    line += 1;
                    lines.insert(line, remaining);
                    pos = 0;
                    it = eol + 1;
                    eol = spec[it..].iter().position(|&c| c == b'\n').map(|p| p + it).unwrap_or(spec.len());
                }
                Ok(())
            });
            match res {
                Ok(()) => {}
                Err(err) if err.is_runtime() => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    fn do_compute_display_setup(&self, context: &HighlightContext, setup: &mut DisplaySetup) -> Result<()> {
        let buffer = context.context.buffer();
        let sels = Self::selections(context.context);
        let range_and_faces = get_updated_ranges(context.context, &self.option_name, &buffer)?;

        for (range, spec) in &range_and_faces {
            if !Self::is_valid(&buffer, range.first)
                || (!is_empty(range) && !Self::is_valid(&buffer, range.last))
                || !Self::is_fully_selected(&sels, range)
            {
                continue;
            }

            let last = if is_empty(range) { range.first } else { buffer.char_next(range.last) };
            if range.first.line < setup.first_line && last.line >= setup.first_line {
                setup.first_line = range.first.line;
            }

            if last.line >= setup.first_line
                && range.first.line <= setup.first_line + setup.line_count
                && range.first.line != last.line
            {
                let added_count = spec.iter().filter(|&&c| c == b'\n').count() as LineCount;
                let removed_count = last.line - range.first.line;
                setup.line_count += removed_count - added_count;
            }
        }
        Ok(())
    }
}

// Groups and references ------------------------------------------------------

fn parse_passes(str: &[u8]) -> Result<HighlightPass> {
    let mut passes = HighlightPass::empty();
    for pass in crate::string_utils::split(str, b'|') {
        match pass {
            b"colorize" => passes |= HighlightPass::Colorize,
            b"move" => passes |= HighlightPass::Move,
            b"wrap" => passes |= HighlightPass::Wrap,
            b"replace" => passes |= HighlightPass::Replace,
            _ => return Err(rt_err!("invalid highlight pass: {}", pass)),
        }
    }
    if passes == HighlightPass::empty() {
        return Err(Error::runtime("no passes specified"));
    }

    Ok(passes)
}

fn highlighter_group_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: [-passes <passes>]\n\
                    Creates a group that can contain other highlighters",
        params: ParameterDesc::new(
            switches(vec![(
                "passes",
                SwitchDesc::arg(
                    "flags(colorize|move|wrap|replace) \
                     kind of highlighters can be put in the group \
                     (default colorize)",
                ),
            )]),
            ParameterFlags::SwitchesOnlyAtStart,
            0,
            0,
        ),
    }
}

fn create_highlighter_group(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
    let parser = ParametersParser::new(params, &highlighter_group_desc().params)?;
    let passes = parse_passes(parser.get_switch("passes").unwrap_or(b"colorize"))?;

    Ok(Rc::new(HighlighterGroup::new(passes)))
}

fn ref_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: [-passes <passes>] <path>\n\
                    Reference the highlighter at <path> in shared highlighters",
        params: ParameterDesc::new(
            switches(vec![(
                "passes",
                SwitchDesc::arg(
                    "flags(colorize|move|wrap|replace) \
                     kind of highlighters that can be referenced \
                     (default colorize)",
                ),
            )]),
            ParameterFlags::SwitchesOnlyAtStart,
            1,
            1,
        ),
    }
}

thread_local! {
    static SHARED_HIGHLIGHTERS: Rc<HighlighterGroup> = Rc::new(HighlighterGroup::new(HighlightPass::All));
    static RUNNING_REFS: RefCell<Vec<(Vec<u8>, BufferRange)>> = const { RefCell::new(Vec::new()) };
}

/// SharedHighlighters::instance(): the highlighters that can be referenced by
/// the ref highlighter (the 'shared' scope of add-highlighter)
pub fn shared_highlighters() -> Rc<HighlighterGroup> {
    SHARED_HIGHLIGHTERS.with(|s| s.clone())
}

struct ReferenceHighlighter {
    passes: HighlightPass,
    name: Vec<u8>,
}

impl ReferenceHighlighter {
    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        let parser = ParametersParser::new(params, &ref_desc().params)?;
        let passes = parse_passes(parser.get_switch("passes").unwrap_or(b"colorize"))?;
        Ok(Rc::new(ReferenceHighlighter { passes, name: parser[0].to_vec() }))
    }

    fn get_shared_child(&self) -> Result<Option<Rc<dyn Highlighter>>> {
        match shared_highlighters().get_child(&self.name) {
            Ok(hl) => Ok(Some(hl)),
            Err(err) if err.kind == ErrorKind::ChildNotFound => Ok(None),
            Err(err) => Err(err),
        }
    }
}

struct PopRunningRef;

impl Drop for PopRunningRef {
    fn drop(&mut self) {
        RUNNING_REFS.with(|r| r.borrow_mut().pop());
    }
}

impl Highlighter for ReferenceHighlighter {
    fn passes(&self) -> HighlightPass {
        self.passes
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) -> Result<()> {
        let desc = (self.name.clone(), range);
        if RUNNING_REFS.with(|r| r.borrow().contains(&desc)) {
            write_to_debug_buffer(&kformat!("highlighting recursion detected with ref to {}", self.name));
            return Ok(());
        }

        RUNNING_REFS.with(|r| r.borrow_mut().push(desc));
        let _pop_desc = PopRunningRef;

        if let Some(hl) = self.get_shared_child()? {
            highlight(hl.as_ref(), context, display_buffer, range);
        }
        Ok(())
    }

    fn do_compute_display_setup(&self, context: &HighlightContext, setup: &mut DisplaySetup) -> Result<()> {
        if let Some(hl) = self.get_shared_child()? {
            compute_display_setup(hl.as_ref(), context, setup);
        }
        Ok(())
    }
}

// Regions --------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct RegexMatch {
    line: LineCount,
    begin: ByteCount,
    end: ByteCount,
    capture_pos: u16,
    capture_len: u16,
}

impl RegexMatch {
    fn begin_coord(&self) -> BufferCoord {
        BufferCoord::new(self.line, self.begin)
    }
    fn end_coord(&self) -> BufferCoord {
        BufferCoord::new(self.line, self.end)
    }
    fn empty(&self) -> bool {
        self.begin == self.end
    }

    fn capture<'a>(&self, lines: &'a [StringDataPtr]) -> &'a [u8] {
        if self.capture_len == 0 {
            return &[];
        }
        let line: &[u8] = &lines[self.line as usize];
        // [DEVIATION] clamped, the C++ substr can go out of range when the
        // capture started before the match (wrapped uint16_t capture_pos)
        let begin = (self.begin as usize + self.capture_pos as usize).min(line.len());
        let end = (begin + self.capture_len as usize).min(line.len());
        &line[begin..end]
    }
}

type RegexMatchList = Vec<RegexMatch>;

struct ForwardHighlighterApplier<'a, 'c> {
    display_buffer: &'a mut DisplayBuffer,
    context: &'a HighlightContext<'c>,
    cur_line: usize,
    end_line: usize,
    cur_atom: usize,
    region_display: DisplayBuffer,
}

impl<'a, 'c> ForwardHighlighterApplier<'a, 'c> {
    fn new(display_buffer: &'a mut DisplayBuffer, context: &'a HighlightContext<'c>) -> Self {
        let end_line = display_buffer.lines().len();
        ForwardHighlighterApplier {
            display_buffer,
            context,
            cur_line: 0,
            end_line,
            cur_atom: 0,
            region_display: DisplayBuffer::default(),
        }
    }

    fn apply(&mut self, begin: BufferCoord, end: BufferCoord, highlighter: &dyn Highlighter) {
        if begin == end {
            return;
        }

        let lines = self.display_buffer.lines_mut();
        let first_line = (self.cur_line..self.end_line).find(|&l| lines[l].range().end > begin).unwrap_or(self.end_line);
        if first_line != self.cur_line && first_line != self.end_line {
            self.cur_atom = 0;
        }
        self.cur_line = first_line;
        if self.cur_line == self.end_line || lines[self.cur_line].range().begin >= end {
            return;
        }

        let region_lines = self.region_display.lines_mut();
        region_lines.clear();
        let mut insert_pos: Vec<(usize, usize)> = Vec::new();
        while self.cur_line != self.end_line && lines[self.cur_line].range().begin < end {
            let line = &mut lines[self.cur_line];
            let mut first = (self.cur_atom..line.len())
                .find(|&i| {
                    let atom = &line.atoms()[i];
                    atom.has_buffer_range() && atom.end() > begin
                })
                .unwrap_or(line.len());
            if first != line.len()
                && line.atoms()[first].type_() == AtomType::Range
                && line.atoms()[first].begin() < begin
            {
                first = line.split_at_coord(first, begin) + 1;
            }
            let idx = first;

            let mut last = (first..line.len())
                .find(|&i| {
                    let atom = &line.atoms()[i];
                    atom.has_buffer_range() && atom.end() > end
                })
                .unwrap_or(line.len());
            if last != line.len() && line.atoms()[last].type_() == AtomType::Range && line.atoms()[last].begin() < end {
                last = line.split_at_coord(last, end) + 1;
            }

            if idx != last {
                insert_pos.push((self.cur_line, idx));
                region_lines.push(line.extract(idx, last));
            }

            if idx != line.atoms().len() {
                break;
            }
            self.cur_line += 1;
            if self.cur_line != self.end_line {
                self.cur_atom = 0;
            }
        }

        if region_lines.is_empty() {
            return;
        }

        self.region_display.compute_range();
        highlight(highlighter, self.context, &mut self.region_display, BufferRange::new(begin, end));

        let region_lines = self.region_display.lines_mut();
        for (i, &(line_it, idx)) in insert_pos.iter().enumerate() {
            // [DEVIATION] the C++ indexes region_lines out of range if the
            // highlighter changed the region line count
            if i >= region_lines.len() {
                break;
            }
            let atoms = std::mem::take(region_lines[i].atoms_mut());
            let count = atoms.len();
            let it = self.display_buffer.lines_mut()[line_it].insert_all(idx, atoms);

            if line_it == self.cur_line {
                self.cur_atom = it + count;
            }
        }
    }
}

fn default_region_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters: <delegate_type> <delegate_params>...\n\
                    Define the default region of a regions highlighter",
        params: ParameterDesc::default(),
    }
}

fn region_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Parameters:  [-match-capture] [-recurse <recurse>] <opening> <closing> <type> <params>...\n\
                    Define a region for a regions highlighter, and apply the given delegate\n\
                    highlighter as defined by <type> and eventual <params>...\n\
                    The region starts at <begin> match and ends at the first <end>",
        params: ParameterDesc::new(
            switches(vec![
                (
                    "match-capture",
                    SwitchDesc::flag(
                        "only consider region ending/recurse delimiters whose first capture group match \
                         the region beginning delimiter",
                    ),
                ),
                (
                    "recurse",
                    SwitchDesc::arg(
                        "make the region end on the first ending delimiter that does not close the given parameter",
                    ),
                ),
            ]),
            ParameterFlags::SwitchesOnlyAtStart | ParameterFlags::IgnoreUnknownSwitches,
            3,
            usize::MAX,
        ),
    }
}

fn regions_desc() -> HighlighterDesc {
    HighlighterDesc {
        docstring: "Holds child region highlighters and segments the buffer in ranges based on those regions\n\
                    definitions. The regions highlighter finds the next region to start by finding which\n\
                    of its child region has the leftmost starting point from current position. In between\n\
                    regions, the default-region child highlighter is applied (if such a child exists)",
        params: ParameterDesc::default(),
    }
}

// [DEVIATION] As the Highlighter trait does not support downcasting, we keep track of the
// region(s) highlighters instances to implement the C++ dynamic_casts. The weak
// references keep the allocations alive, so addresses are never reused while
// registered.
thread_local! {
    static REGIONS_INSTANCES: RefCell<Vec<Weak<RegionsHighlighter>>> = const { RefCell::new(Vec::new()) };
    static REGION_INSTANCES: RefCell<Vec<Weak<RegionHighlighter>>> = const { RefCell::new(Vec::new()) };
}

fn highlighter_address(hl: &dyn Highlighter) -> *const () {
    hl as *const dyn Highlighter as *const ()
}

/// dynamic_cast<RegionsHighlighter*>
fn is_regions_highlighter(hl: &dyn Highlighter) -> bool {
    let addr = highlighter_address(hl);
    REGIONS_INSTANCES.with(|r| r.borrow().iter().any(|w| w.strong_count() > 0 && w.as_ptr() as *const () == addr))
}

/// dynamic_cast<RegionHighlighter*>
fn as_region_highlighter(hl: &dyn Highlighter) -> Option<Rc<RegionHighlighter>> {
    let addr = highlighter_address(hl);
    REGION_INSTANCES.with(|r| r.borrow().iter().find(|w| w.as_ptr() as *const () == addr).and_then(|w| w.upgrade()))
}

fn register_instance<T>(instances: &'static std::thread::LocalKey<RefCell<Vec<Weak<T>>>>, hl: &Rc<T>) {
    instances.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|w| w.strong_count() > 0);
        r.push(Rc::downgrade(hl));
    });
}

struct RegionHighlighter {
    delegate: Rc<dyn Highlighter>,

    begin: Vec<u8>,
    end: Vec<u8>,
    recurse: Vec<u8>,
    match_capture: bool,
    default: bool,
}

impl RegionHighlighter {
    fn new(delegate: Rc<dyn Highlighter>, begin: Vec<u8>, end: Vec<u8>, recurse: Vec<u8>, match_capture: bool) -> Rc<Self> {
        let res = Rc::new(RegionHighlighter { delegate, begin, end, recurse, match_capture, default: false });
        register_instance(&REGION_INSTANCES, &res);
        res
    }

    fn new_default(delegate: Rc<dyn Highlighter>) -> Rc<Self> {
        let res = Rc::new(RegionHighlighter {
            delegate,
            begin: Vec::new(),
            end: Vec::new(),
            recurse: Vec::new(),
            match_capture: false,
            default: true,
        });
        register_instance(&REGION_INSTANCES, &res);
        res
    }

    fn match_capture(&self) -> bool {
        self.match_capture
    }
    fn is_default(&self) -> bool {
        self.default
    }
    fn delegate(&self) -> &Rc<dyn Highlighter> {
        &self.delegate
    }
}

impl Highlighter for RegionHighlighter {
    fn passes(&self) -> HighlightPass {
        self.delegate.passes()
    }

    fn has_children(&self) -> bool {
        self.delegate.has_children()
    }

    fn get_child(&self, path: &[u8]) -> Result<Rc<dyn Highlighter>> {
        self.delegate.get_child(path)
    }

    fn add_child(&self, name: Vec<u8>, hl: Rc<dyn Highlighter>, override_: bool) -> Result<()> {
        self.delegate.add_child(name, hl, override_)
    }

    fn remove_child(&self, id: &[u8]) -> Result<()> {
        self.delegate.remove_child(id)
    }

    fn complete_child(&self, path: &[u8], cursor_pos: usize, group: bool) -> Result<Completions> {
        self.delegate.complete_child(path, cursor_pos, group)
    }

    fn fill_unique_ids(&self, unique_ids: &mut Vec<Vec<u8>>) {
        self.delegate.fill_unique_ids(unique_ids)
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) -> Result<()> {
        highlight(self.delegate.as_ref(), context, display_buffer, range);
        Ok(())
    }
}

#[derive(Clone)]
struct Region {
    begin: BufferCoord,
    end: BufferCoord,
    highlighter: Rc<RegionHighlighter>,
}
type RegionList = Vec<Region>;

/// (regex, match_captures)
type RegexKey = (Vec<u8>, bool);

#[derive(Default)]
struct RegionsCache {
    buffer_timestamp: usize,
    regions_timestamp: usize,
    ranges: LineRangeSet,
    matches: IndexMap<RegexKey, RegexMatchList>,
    regions: HashMap<BufferRange, Rc<RegionList>>,
}

/// Indices in RegionsCache::matches of the begin/end/recurse matches of a region
#[derive(Clone, Copy, Default)]
struct RegionMatchesIndices {
    begin: Option<usize>,
    end: Option<usize>,
    recurse: Option<usize>,
}

fn compare_to_begin(lhs: &RegexMatch, rhs: BufferCoord) -> bool {
    lhs.begin_coord() < rhs
}

struct RegionsHighlighter {
    regions: RefCell<IndexMap<Vec<u8>, Rc<RegionHighlighter>>>,
    regexes: RefCell<IndexMap<RegexKey, Regex>>,
    default_region: RefCell<Vec<u8>>,

    regions_timestamp: Cell<usize>,
    cache: BufferSideCache<RegionsCache>,
}

impl RegionsHighlighter {
    fn new() -> Rc<Self> {
        let res = Rc::new(RegionsHighlighter {
            regions: RefCell::new(IndexMap::new()),
            regexes: RefCell::new(IndexMap::new()),
            default_region: RefCell::new(Vec::new()),
            regions_timestamp: Cell::new(0),
            cache: BufferSideCache::new(),
        });
        register_instance(&REGIONS_INSTANCES, &res);
        res
    }

    fn create(params: HighlighterParameters, _: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        if !params.is_empty() {
            return Err(Error::runtime("unexpected parameters"));
        }
        Ok(RegionsHighlighter::new())
    }

    fn is_regions(parent: Option<&dyn Highlighter>) -> bool {
        let Some(parent) = parent else { return false };
        if is_regions_highlighter(parent) {
            return true;
        }
        if let Some(region) = as_region_highlighter(parent) {
            return Self::is_regions(Some(region.delegate().as_ref()));
        }
        false
    }

    fn create_region(params: HighlighterParameters, parent: Option<&dyn Highlighter>) -> Result<Rc<dyn Highlighter>> {
        if !Self::is_regions(parent) {
            return Err(Error::runtime("region highlighter can only be added to a regions parent"));
        }

        let parser = ParametersParser::new(params, &region_desc().params)?;

        let match_capture = parser.has_switch("match-capture");
        if parser[0].is_empty() || parser[1].is_empty() {
            return Err(Error::runtime("begin and end must not be empty"));
        }

        let type_ = &parser[2];
        let Some(it) = HighlighterRegistry::instance().get(type_) else {
            return Err(rt_err!("no such highlighter type: '{}'", type_));
        };

        // validate regexes, TODO: less costly
        Regex::new(&parser[0], RegexCompileFlags::NONE)?;
        Regex::new(&parser[1], RegexCompileFlags::NONE)?;
        if let Some(recurse_switch) = parser.get_switch("recurse") {
            Regex::new(recurse_switch, RegexCompileFlags::NONE)?;
        }

        let delegate = (it.factory)(parser.positionals_from(3), None)?;
        Ok(RegionHighlighter::new(
            delegate,
            parser[0].to_vec(),
            parser[1].to_vec(),
            parser.get_switch("recurse").unwrap_or(b"").to_vec(),
            match_capture,
        ))
    }

    fn create_default_region(
        params: HighlighterParameters,
        parent: Option<&dyn Highlighter>,
    ) -> Result<Rc<dyn Highlighter>> {
        if !Self::is_regions(parent) {
            return Err(Error::runtime("default-region highlighter can only be added to a regions parent"));
        }

        let param_desc = ParameterDesc::new(Default::default(), ParameterFlags::SwitchesOnlyAtStart, 1, usize::MAX);
        let parser = ParametersParser::new(params, &param_desc)?;

        let type_ = &parser[0];
        let Some(it) = HighlighterRegistry::instance().get(type_) else {
            return Err(rt_err!("no such highlighter type: '{}'", type_));
        };

        let delegate = (it.factory)(parser.positionals_from(1), None)?;
        Ok(RegionHighlighter::new_default(delegate))
    }

    fn find_matching_end(
        lines: &[StringDataPtr],
        mut beg_pos: BufferCoord,
        end_matches: &RegexMatchList,
        recurse_matches: &RegexMatchList,
        capture: Option<&[u8]>,
    ) -> usize {
        let mut end_it = 0;
        let mut rec_it = 0;
        let mut recurse_level = 0;
        loop {
            end_it += end_matches[end_it..].partition_point(|m| compare_to_begin(m, beg_pos));
            rec_it += recurse_matches[rec_it..].partition_point(|m| compare_to_begin(m, beg_pos));

            if end_it == end_matches.len() {
                return end_it;
            }

            let end_match = &end_matches[end_it];
            while rec_it != recurse_matches.len() && recurse_matches[rec_it].end_coord() <= end_match.end_coord() {
                if capture.is_none_or(|c| recurse_matches[rec_it].capture(lines) == c) {
                    recurse_level += 1;
                }
                rec_it += 1;
            }

            if capture.is_none_or(|c| c == end_match.capture(lines)) {
                if recurse_level == 0 {
                    return end_it;
                }
                recurse_level -= 1;
            }

            if beg_pos != end_match.end_coord() {
                beg_pos = end_match.end_coord();
            }
            end_it += 1;
        }
    }

    /// find the begin closest to pos in all matches, returns (region index, match index)
    fn find_next_begin(
        indices: &[RegionMatchesIndices],
        cache: &RegionsCache,
        pos: BufferCoord,
    ) -> Option<(usize, usize)> {
        let mut res: Option<(usize, usize, BufferCoord)> = None;

        for (i, region_indices) in indices.iter().enumerate() {
            // default regions have no begin matches
            let Some(begin_idx) = region_indices.begin else { continue };

            let matches = &cache.matches[begin_idx];
            let it = matches.partition_point(|m| compare_to_begin(m, pos));
            if it != matches.len() && res.is_none_or(|r| matches[it].begin_coord() < r.2) {
                res = Some((i, it, matches[it].begin_coord()));
            }
        }
        res.map(|r| (r.0, r.1))
    }

    fn add_regex(regexes: &mut IndexMap<RegexKey, Regex>, str: &[u8], captures: bool) -> Result<()> {
        let key: RegexKey = (str.to_vec(), captures);
        if str.is_empty() || regexes.contains_key(&key) {
            return Ok(());
        }

        let mut flags = RegexCompileFlags::OPTIMIZE;
        if !captures {
            flags |= RegexCompileFlags::NO_SUBS;
        }

        regexes.insert(key, Regex::new(str, flags)?);
        Ok(())
    }

    /// MatchAdder: add the matches of the given line ranges
    fn add_matches(regexes: &IndexMap<RegexKey, Regex>, buffer: &Buffer, cache: &mut RegionsCache, ranges: &[LineRange]) {
        for key in regexes.keys() {
            cache.matches.entry(key.clone()).or_default();
        }

        let lines = buffer.lines();
        for (key, regex) in regexes {
            let matches = cache.matches.get_mut(key).unwrap();
            let pivot = matches.len();
            let mut vm = ThreadedRegexVM::<SliceIt, ForwardSearch>::new(regex.impl_());
            let mark_count = regex.mark_count();

            for range in ranges {
                for line in range.begin..range.end {
                    let l: &[u8] = &lines[line as usize];
                    let flags = RegexExecFlags::NOT_END_OF_LINE; // buffer line already ends with \n

                    let l_begin = SliceIt::new(l, 0);
                    let l_end = SliceIt::new(l, l.len());
                    let mut extra_flags = RegexExecFlags::NONE;
                    let mut pos = 0;
                    while vm.exec(&SliceIt::new(l, pos), &l_end, &l_begin, &l_end, flags | extra_flags, noop_idle) {
                        let captures = vm.captures();
                        let cap = |i: usize| captures.get(i).copied().flatten().map(|it| it.pos);
                        let (c0, c1) = (cap(0).unwrap_or(pos), cap(1).unwrap_or(pos));
                        let with_capture = mark_count > 0 && cap(2).is_some() && c1 - c0 < u16::MAX as usize;
                        let (capture_pos, capture_len) = if with_capture {
                            let c2 = cap(2).unwrap();
                            let c3 = cap(3).unwrap_or(c2);
                            ((c2 as isize - c0 as isize) as u16, (c3 as isize - c2 as isize) as u16)
                        } else {
                            (0, 0)
                        };
                        matches.push(RegexMatch {
                            line,
                            begin: c0 as ByteCount,
                            end: c1 as ByteCount,
                            capture_pos,
                            capture_len,
                        });
                        pos = c1;

                        extra_flags =
                            if c0 == c1 { RegexExecFlags::NOT_INITIAL_NULL } else { RegexExecFlags::NONE };
                    }
                }
            }

            // Move new matches into position.
            inplace_merge(matches, pivot, |lhs, rhs| lhs.line < rhs.line);
        }
    }

    fn update_changed_lines(buffer: &Buffer, modifs: &[LineModification], cache: &mut RegionsCache) {
        for matches in cache.matches.values_mut() {
            // remove out of date matches and update line for others
            matches.retain_mut(|m| {
                let modif_it = modifs.partition_point(|c| c.old_line <= m.line);

                if modif_it != 0 {
                    let prev = &modifs[modif_it - 1];
                    if m.line < prev.old_line + prev.num_removed {
                        return false; // match removed
                    }

                    m.line += prev.diff();
                }

                debug_assert!(buffer.is_valid(m.begin_coord()) || buffer.line_length(m.line) == m.begin);
                debug_assert!(buffer.is_valid(m.end_coord()) || buffer.line_length(m.line) == m.end);
                true
            });
        }
    }

    fn update_matches(&self, cache: &mut RegionsCache, buffer: &Buffer, range: LineRange) -> Result<bool> {
        let buffer_timestamp = buffer.timestamp();
        if cache.buffer_timestamp == 0 || cache.regions_timestamp != self.regions_timestamp.get() {
            let mut regexes = self.regexes.borrow_mut();
            regexes.clear();
            cache.matches.clear();
            for region in self.regions.borrow().values() {
                Self::add_regex(&mut regexes, &region.begin, region.match_capture())?;
                Self::add_regex(&mut regexes, &region.end, region.match_capture())?;
                Self::add_regex(&mut regexes, &region.recurse, region.match_capture())?;
            }

            Self::add_matches(&regexes, buffer, cache, &[range]);
            cache.ranges.reset(range);
            cache.buffer_timestamp = buffer_timestamp;
            cache.regions_timestamp = self.regions_timestamp.get();
            Ok(true)
        } else {
            let mut modified = false;
            if cache.buffer_timestamp != buffer_timestamp {
                let modifs = compute_line_modifications(buffer, cache.buffer_timestamp);
                Self::update_changed_lines(buffer, &modifs, cache);
                cache.ranges.update(&modifs);
                cache.buffer_timestamp = buffer_timestamp;
                modified = true;
            }

            let mut new_ranges = Vec::new();
            cache.ranges.add_range(range, &mut |range: LineRange| {
                if range.begin == range.end {
                    return;
                }
                new_ranges.push(range);
            });
            if !new_ranges.is_empty() {
                Self::add_matches(&self.regexes.borrow(), buffer, cache, &new_ranges);
                modified = true;
            }
            Ok(modified)
        }
    }

    fn get_regions_for_range(&self, buffer: &Buffer, range: BufferRange) -> Result<Rc<RegionList>> {
        let cache = self.cache.get(buffer);
        let mut cache = cache.borrow_mut();
        let line_range = LineRange::new(range.begin.line, buffer.line_count().min(range.end.line + 1));
        if self.update_matches(&mut cache, buffer, line_range)? {
            cache.regions.clear();
        }

        if let Some(regions) = cache.regions.get(&range) {
            return Ok(regions.clone());
        }

        let regions = {
            let cache = &*cache;
            let region_highlighters = self.regions.borrow();
            let index_of = |regex: &[u8], match_capture: bool| -> Option<usize> {
                if regex.is_empty() {
                    return None;
                }
                cache.matches.get_index_of(&(regex.to_vec(), match_capture))
            };
            let indices: Vec<RegionMatchesIndices> = region_highlighters
                .values()
                .map(|region| {
                    if region.is_default() {
                        return RegionMatchesIndices::default();
                    }
                    RegionMatchesIndices {
                        begin: index_of(&region.begin, region.match_capture()),
                        end: index_of(&region.end, region.match_capture()),
                        recurse: index_of(&region.recurse, region.match_capture()),
                    }
                })
                .collect();

            let lines = buffer.lines();
            let empty_matches = RegexMatchList::new();
            let mut regions = RegionList::new();
            let mut begin = Self::find_next_begin(&indices, cache, range.begin);
            while let Some((index, beg_idx)) = begin {
                let region = region_highlighters[index].clone();
                let region_indices = indices[index];
                let beg = cache.matches[region_indices.begin.unwrap()][beg_idx];
                let end_matches = region_indices.end.map(|i| &cache.matches[i]).unwrap_or(&empty_matches);
                let recurse_matches = region_indices.recurse.map(|i| &cache.matches[i]).unwrap_or(&empty_matches);

                let end_it = Self::find_matching_end(
                    &lines,
                    beg.end_coord(),
                    end_matches,
                    recurse_matches,
                    if region.match_capture() { Some(beg.capture(&lines)) } else { None },
                );

                if end_it == end_matches.len() || end_matches[end_it].end_coord() >= range.end {
                    // region continue past range end
                    let begin_coord = beg.begin_coord();
                    if begin_coord < range.end {
                        regions.push(Region { begin: begin_coord, end: range.end, highlighter: region });
                    }
                    break;
                }

                let mut end_coord = end_matches[end_it].end_coord();
                regions.push(Region { begin: beg.begin_coord(), end: end_coord, highlighter: region });

                // With empty begin and end matches (for example if the regexes
                // are /"\K/ and /(?=")/), that case can happen, and would
                // result in an infinite loop.
                if end_coord == beg.begin_coord() {
                    debug_assert!(beg.empty() && end_matches[end_it].empty());
                    end_coord.column += 1;
                }
                begin = Self::find_next_begin(&indices, cache, end_coord);
            }
            Rc::new(regions)
        };
        cache.regions.insert(range, regions.clone());
        Ok(regions)
    }
}

impl Highlighter for RegionsHighlighter {
    fn passes(&self) -> HighlightPass {
        HighlightPass::Colorize
    }

    fn do_highlight(&self, context: &HighlightContext, display_buffer: &mut DisplayBuffer, range: BufferRange) -> Result<()> {
        if self.regions.borrow().is_empty() {
            return Ok(());
        }

        let display_range = display_buffer.range();
        let buffer = context.context.buffer();
        // [DEVIATION] a shared copy of the cached regions: the C++ keeps a reference
        // into the cache, which a reentrant use (through ref) can invalidate
        let regions = self.get_regions_for_range(&buffer, range)?;

        let begin = regions.partition_point(|r| r.end < display_range.begin);
        let end = begin + regions[begin..].partition_point(|r| r.begin < display_range.end);
        let correct = |c: BufferCoord| -> BufferCoord {
            if !buffer.is_end(c) && buffer.line_length(c.line) == c.column {
                return BufferCoord::line(c.line + 1);
            }
            c
        };

        let default_region = self.regions.borrow().get(&*self.default_region.borrow()).cloned();

        let mut last_begin = if begin == 0 { range.begin } else { regions[begin - 1].end };
        debug_assert!(begin <= end);

        let mut apply_highlighter = ForwardHighlighterApplier::new(display_buffer, context);
        for region in &regions[begin..end] {
            if let Some(default_region) = &default_region
                && last_begin < region.begin
            {
                apply_highlighter.apply(correct(last_begin), correct(region.begin), default_region.as_ref());
            }

            apply_highlighter.apply(correct(region.begin), correct(region.end), region.highlighter.as_ref());
            last_begin = region.end;
        }
        if let Some(default_region) = &default_region
            && last_begin < display_range.end
        {
            apply_highlighter.apply(correct(last_begin), range.end, default_region.as_ref());
        }

        display_buffer.compute_range();
        Ok(())
    }

    fn has_children(&self) -> bool {
        true
    }

    fn get_child(&self, path: &[u8]) -> Result<Rc<dyn Highlighter>> {
        let sep_it = path.iter().position(|&c| c == b'/');
        let id = &path[..sep_it.unwrap_or(path.len())];
        let Some(region) = self.regions.borrow().get(id).cloned() else {
            return Err(child_not_found(kformat!("no such id: {}", id)));
        };
        match sep_it {
            None => Ok(region),
            Some(sep_it) => region.get_child(&path[sep_it + 1..]),
        }
    }

    fn add_child(&self, name: Vec<u8>, hl: Rc<dyn Highlighter>, override_: bool) -> Result<()> {
        let Some(region_hl) = as_region_highlighter(hl.as_ref()) else {
            return Err(Error::runtime("only region highlighter can be added as child of a regions highlighter"));
        };
        let exists = self.regions.borrow().contains_key(&name);
        if !override_ && exists {
            return Err(rt_err!("duplicate id: '{}'", name));
        }

        if region_hl.is_default() {
            if !self.default_region.borrow().is_empty() {
                return Err(Error::runtime("default region already defined"));
            }
            *self.default_region.borrow_mut() = name.clone();
        }

        // replaces the value in place if it exists
        let old = self.regions.borrow_mut().insert(name, region_hl);
        drop(old);
        self.regions_timestamp.set(self.regions_timestamp.get() + 1);
        Ok(())
    }

    fn remove_child(&self, id: &[u8]) -> Result<()> {
        if *self.default_region.borrow() == id {
            self.default_region.borrow_mut().clear();
        }

        let removed = self.regions.borrow_mut().shift_remove(id);
        if removed.is_none() {
            return Err(child_not_found(kformat!("no such id: {}", id)));
        }
        drop(removed);
        self.regions_timestamp.set(self.regions_timestamp.get() + 1);
        Ok(())
    }

    fn complete_child(&self, path: &[u8], cursor_pos: usize, group: bool) -> Result<Completions> {
        if let Some(sep_it) = path.iter().position(|&c| c == b'/') {
            let offset = sep_it + 1;
            let hl = self.get_child(&path[..sep_it])?;
            return Ok(offset_pos(hl.complete_child(&path[offset..], cursor_pos - offset, group)?, offset));
        }

        let container: Vec<Vec<u8>> = self.regions.borrow().keys().cloned().collect();
        let completions_flags = if group { CompletionFlags::None } else { CompletionFlags::Menu };
        Ok(Completions::with(0, 0, complete(path, cursor_pos, &container), completions_flags))
    }
}

// Registration ---------------------------------------------------------------

pub fn setup_builtin_highlighters(group: &HighlighterGroup) -> Result<()> {
    group.add_child(b"tabulations".to_vec(), Rc::new(TabulationHighlighter), false)?;
    group.add_child(b"unprintable".to_vec(), make_highlighter(expand_unprintable, HighlightPass::Colorize), false)?;
    group.add_child(b"selections".to_vec(), make_highlighter(highlight_selections, HighlightPass::Colorize), false)?;
    Ok(())
}

pub fn register_highlighters() {
    let registry = HighlighterRegistry::instance();

    registry.insert("column", create_column_highlighter, column_desc());
    registry.insert("default-region", RegionsHighlighter::create_default_region, default_region_desc());
    registry.insert("dynregex", create_dynamic_regex_highlighter, dynamic_regex_desc());
    registry.insert("fill", create_fill_highlighter, fill_desc());
    registry.insert("flag-lines", FlagLinesHighlighter::create, flag_lines_desc());
    registry.insert("group", create_highlighter_group, highlighter_group_desc());
    registry.insert("line", create_line_highlighter, line_desc());
    registry.insert("number-lines", LineNumbersHighlighter::create, line_numbers_desc());
    registry.insert("ranges", RangesHighlighter::create, ranges_desc());
    registry.insert("ref", ReferenceHighlighter::create, ref_desc());
    registry.insert("regex", RegexHighlighter::create, regex_desc());
    registry.insert("region", RegionsHighlighter::create_region, region_desc());
    registry.insert("regions", RegionsHighlighter::create, regions_desc());
    registry.insert("replace-ranges", ReplaceRangesHighlighter::create, replace_ranges_desc());
    registry.insert("show-matching", create_matching_char_highlighter, show_matching_desc());
    registry.insert("show-whitespaces", ShowWhitespacesHighlighter::create, show_whitespace_desc());
    registry.insert("wrap", WrapHighlighter::create, wrap_desc());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{BufferFlags, ByteOrderMark, EolFormat, FinalEol};
    use crate::context::ContextFlags;
    use crate::debug::DebugFlags;
    use crate::file::FsStatus;
    use crate::json::ToJson;
    use crate::option_manager::OptionFlags;
    use crate::option_types::OptionType;
    use crate::selection::{Selection, SelectionList};
    use crate::units::DisplayCoord;
    use crate::window::Window;
    use std::rc::Weak;

    fn setup_globals() {
        if GlobalScope::has_instance() {
            return;
        }
        GlobalScope::create();
        let reg = GlobalScope::instance().option_registry();
        reg.declare_option(b"tabstop", b"", 8i32, OptionFlags::None).unwrap();
        reg.declare_option(b"indentwidth", b"", 4i32, OptionFlags::None).unwrap();
        reg.declare_option(b"scrolloff", b"", DisplayCoord::new(0, 0), OptionFlags::None).unwrap();
        reg.declare_option(b"eolformat", b"", EolFormat::Lf, OptionFlags::None).unwrap();
        reg.declare_option(b"finaleol", b"", FinalEol::Present, OptionFlags::None).unwrap();
        reg.declare_option(b"BOM", b"", ByteOrderMark::None, OptionFlags::None).unwrap();
        reg.declare_option(b"readonly", b"", false, OptionFlags::None).unwrap();
        reg.declare_option(b"debug", b"", DebugFlags::None, OptionFlags::None).unwrap();
        reg.declare_option(b"disabled_hooks", b"", Regex::default(), OptionFlags::None).unwrap();
        let pairs: Vec<CodepointOpt> = "(){}[]<>".chars().map(|c| CodepointOpt(c as Codepoint)).collect();
        reg.declare_option(b"matching_pairs", b"", pairs, OptionFlags::None).unwrap();
        register_highlighters();
    }

    fn strs(s: &[&str]) -> Vec<Vec<u8>> {
        s.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    struct Env {
        buffer: Rc<Buffer>,
        window: Rc<Window>,
        context: Context,
        counter: Cell<usize>,
    }

    /// Remove %(...) markers, returning the text and the (begin, last) coords of the marked ranges
    fn parse_markers(input: &[u8]) -> (Vec<u8>, Vec<(BufferCoord, BufferCoord)>) {
        let mut text = Vec::new();
        let mut sels = Vec::new();
        let mut coord = BufferCoord::new(0, 0);
        let mut i = 0;
        let push = |text: &mut Vec<u8>, coord: &mut BufferCoord, c: u8| {
            text.push(c);
            if c == b'\n' {
                *coord = BufferCoord::line(coord.line + 1);
            } else {
                coord.column += 1;
            }
        };
        while i < input.len() {
            if input[i..].starts_with(b"%(")
                && let Some(close) = input[i + 2..].iter().position(|&c| c == b')')
                && close > 0
            {
                let begin = coord;
                let mut last = coord;
                for &c in &input[i + 2..i + 2 + close] {
                    last = coord;
                    push(&mut text, &mut coord, c);
                }
                sels.push((begin, last));
                i += close + 3;
                continue;
            }
            push(&mut text, &mut coord, input[i]);
            i += 1;
        }
        (text, sels)
    }

    impl Env {
        fn new(input: &[u8]) -> Env {
            setup_globals();
            let (text, sels) = parse_markers(input);
            let mut lines: Vec<StringDataPtr> =
                crate::string_utils::split_after(&text, b'\n').into_iter().map(Rc::from).collect();
            if lines.is_empty() {
                lines.push(Rc::from(&b"\n"[..]));
            }
            if lines.last().unwrap().last() != Some(&b'\n') {
                let mut last = lines.pop().unwrap().to_vec();
                last.push(b'\n');
                lines.push(Rc::from(last));
            }
            let buffer = Buffer::new(
                b"out".to_vec(),
                BufferFlags::NoHooks,
                lines,
                ByteOrderMark::None,
                EolFormat::Lf,
                FinalEol::Present,
                FsStatus::default(),
            )
            .unwrap();
            let window = Window::new(&buffer).unwrap();
            window.set_dimensions(DisplayCoord::new(24, 80));
            let selections: Vec<Selection> = if sels.is_empty() {
                vec![Selection::new(BufferCoord::new(0, 0))]
            } else {
                sels.iter().map(|&(begin, last)| Selection::from_range(begin, last)).collect()
            };
            let mut list = SelectionList::new(buffer.clone(), selections[0].clone());
            let main = selections.len() - 1;
            list.set(selections, main);
            let context = Context::new(Weak::new(), list, ContextFlags::None, b"test".to_vec());
            context.set_window(&window);
            Env { buffer, window, context, counter: Cell::new(0) }
        }

        fn group(&self, scope: &str) -> Rc<dyn Highlighter> {
            match scope {
                "window" => self.window.scope().highlighters().group().clone(),
                "shared" => shared_highlighters(),
                _ => panic!("unknown scope"),
            }
        }

        /// add-highlighter <path> <type> <params>...
        fn add_highlighter(&self, path: &str, type_: &str, params: &[&str]) {
            let (scope, rest) = path.split_once('/').unwrap();
            let mut parent = self.group(scope);
            let (parent_path, name) = match rest.rfind('/') {
                Some(p) => (&rest[..p], &rest[p + 1..]),
                None => ("", rest),
            };
            if !parent_path.is_empty() {
                parent = parent.get_child(parent_path.as_bytes()).unwrap();
            }
            let name = if name.is_empty() {
                self.counter.set(self.counter.get() + 1);
                format!("{}_{}", type_, self.counter.get())
            } else {
                name.to_string()
            };
            let factory = HighlighterRegistry::instance().get(type_.as_bytes()).unwrap();
            let hl = (factory.factory)(&strs(params), Some(parent.as_ref())).unwrap();
            parent.add_child(name.into_bytes(), hl, false).unwrap();
        }

        fn declare_option<T: OptionType>(&self, name: &str, value: T) {
            GlobalScope::instance()
                .option_registry()
                .declare_option(name.as_bytes(), b"", value, OptionFlags::None)
                .unwrap();
        }

        fn timestamp(&self) -> String {
            self.buffer.timestamp().to_string()
        }

        fn set_option(&self, scope: &str, name: &str, values: &[&str]) {
            let manager = match scope {
                "global" => GlobalScope::instance().options().clone(),
                "buffer" => self.buffer.options().clone(),
                "window" => self.window.options().clone(),
                _ => panic!("unknown scope"),
            };
            manager.get_local_option(name.as_bytes()).unwrap().set_from_strings(&strs(values)).unwrap();
        }

        fn set_cursor(&self, coord: BufferCoord) {
            self.context.selections().assign(vec![Selection::new(coord)]);
        }

        fn draw(&self) -> String {
            let display_buffer = self.window.update_display_buffer(&self.context).unwrap();
            String::from_utf8(display_buffer.lines().to_json()).unwrap()
        }

        /// compare the draw of the display buffer with the first draw of the test script
        fn check(&self, test: &str) {
            self.check_nth(test, 0);
        }

        fn check_nth(&self, test: &str, nth: usize) {
            let drawn = self.draw();
            let expected = expected_draw(test, nth);
            let prefix = format!("{{ \"jsonrpc\": \"2.0\", \"method\": \"draw\", \"params\": [{}, ", drawn);
            assert!(
                expected.starts_with(&prefix),
                "test {}:\nexpected: {}\ngot:      {}",
                test,
                expected,
                prefix
            );
        }
    }

    fn test_dir(test: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test").join(test)
    }

    fn read_input(test: &str) -> Vec<u8> {
        std::fs::read(test_dir(test).join("in")).expect("test input")
    }

    fn expected_draw(test: &str, nth: usize) -> String {
        let script = test_dir(test).join("script");
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("ui_out() { printf '%s\\n' \"$1\"; }; . \"$0\"")
            .arg(&script)
            .output()
            .unwrap();
        let output = String::from_utf8(output.stdout).unwrap();
        output.lines().filter(|l| l.contains("\"method\": \"draw\"")).nth(nth).unwrap().to_string()
    }

    fn make_env(test: &str) -> Env {
        Env::new(&read_input(test))
    }

    #[test]
    fn inclusive_buffer_range_strings() {
        let r = inclusive_buffer_range_from_string(b"2.3,1.5").unwrap();
        assert_eq!(r, InclusiveBufferRange { first: BufferCoord::new(0, 4), last: BufferCoord::new(1, 2) });
        assert_eq!(inclusive_buffer_range_to_string(&r), b"1.5,2.3");
        let r = inclusive_buffer_range_from_string(b"3.1+2").unwrap();
        assert_eq!(r, InclusiveBufferRange { first: BufferCoord::new(2, 0), last: BufferCoord::new(2, 1) });
        let r = inclusive_buffer_range_from_string(b"3.1+0").unwrap();
        assert!(is_empty(&r));
        assert_eq!(inclusive_buffer_range_to_string(&r), b"3.1+0");
        assert!(inclusive_buffer_range_from_string(b"3.1").is_err());
        assert!(inclusive_buffer_range_from_string(b"3,1").is_err());
        assert!(inclusive_buffer_range_from_string(b"0.1+1").is_err());

        let mut list = RangeAndStringList::from_strings(&strs(&["1", "2.1,2.2|b", "1.1+1|a"])).unwrap();
        assert_eq!(list.list[0].1, b"a");
        assert!(list.add_from_strings(&strs(&["1.5+1|c", "3.1+1|d"])).unwrap());
        let names: Vec<&[u8]> = list.list.iter().map(|e| e.1.as_slice()).collect();
        assert_eq!(names, vec![&b"a"[..], b"c", b"b", b"d"]);
        assert_eq!(list.to_strings(), strs(&["1", "1.1,1.1|a", "1.5,1.5|c", "2.1,2.2|b", "3.1,3.1|d"]));
        assert_eq!(<RangeAndStringList as OptionType>::type_name(), b"range-specs");

        let list = LineAndSpecList::from_strings(&strs(&["3", "5|e", "2|a\\|b"])).unwrap();
        assert_eq!(list.list, vec![(2, b"a|b".to_vec()), (5, b"e".to_vec())]);
        assert_eq!(list.to_strings(), strs(&["3", "2|a\\|b", "5|e"]));
        assert_eq!(<LineAndSpecList as OptionType>::type_name(), b"line-specs");
    }

    #[test]
    fn face_override() {
        let env = make_env("highlight/face-override");
        GlobalScope::instance().faces().add_face(b"Foo", b"default,blue+i", true).unwrap();
        env.buffer.scope().faces().add_face(b"Foo", b"green,default+u@Foo", true).unwrap();
        env.buffer.scope().faces().add_face(b"Foo", b"red@Foo", true).unwrap();
        env.add_highlighter("window/", "regex", &["foo", "0:Foo"]);
        env.check("highlight/face-override");
    }

    #[test]
    fn named_captures() {
        let env = make_env("highlight/named-captures");
        env.add_highlighter(
            "window/",
            "regex",
            &["(?<year>\\d+)-(?<month>\\d+)-(?<day>\\d+)", "year:red", "month:green", "day:yellow"],
        );
        env.check("highlight/named-captures");
    }

    #[test]
    fn ranges() {
        let env = make_env("highlight/ranges");
        env.declare_option("r", RangeAndStringList::default());
        env.add_highlighter("window/", "ranges", &["r"]);
        let ts = env.timestamp();
        env.set_option("window", "r", &[&ts, "1.1+0|default,red", "2.1,2.1|default,red", "3.1+2|default,red"]);
        env.check("highlight/ranges");
    }

    #[test]
    fn regions() {
        let env = make_env("highlight/regions");
        env.add_highlighter("window/regions_test", "regions", &[]);
        env.add_highlighter("window/regions_test/code", "default-region", &["fill", "yellow"]);
        env.add_highlighter("window/regions_test/string", "region", &["\"", "(?<!\\\\)(\\\\\\\\)*\"", "fill", "green"]);
        env.add_highlighter("window/regions_test/shell", "region", &["-recurse", "\\{", "\\$\\{", "\\}", "fill", "red"]);
        env.add_highlighter("window/regions_test/tag", "region", &["-match-capture", "<(\\w+)>", "</(\\w+)>", "fill", "blue"]);
        env.check("highlight/regions");
    }

    #[test]
    fn regions_recurse() {
        let env = make_env("highlight/regions-recurse");
        env.add_highlighter("window/regions_test", "regions", &[]);
        env.add_highlighter("window/regions_test/code", "default-region", &["fill", "yellow"]);
        env.add_highlighter(
            "window/regions_test/argument",
            "region",
            &["-recurse", "\\(", "\\w+\\h*\\(\\K", "(?=\\))", "fill", "red"],
        );
        env.check("highlight/regions-recurse");
    }

    #[test]
    fn regions_errors() {
        let env = make_env("highlight/regions-recurse");
        let registry = HighlighterRegistry::instance();
        let region = registry.get(b"region").unwrap();
        assert!((region.factory)(&strs(&["a", "b", "fill", "red"]), None).is_err());
        env.add_highlighter("window/r", "regions", &[]);
        let regions = env.group("window").get_child(b"r").unwrap();
        let fill = registry.get(b"fill").unwrap();
        let hl = (fill.factory)(&strs(&["red"]), None).unwrap();
        assert!(regions.add_child(b"x".to_vec(), hl, false).is_err());
        env.add_highlighter("window/r/a", "region", &["a", "b", "group"]);
        env.add_highlighter("window/r/a/f", "fill", &["red"]);
        assert!(regions.get_child(b"a/f").is_ok());
        assert!(regions.get_child(b"b").err().unwrap().kind == ErrorKind::ChildNotFound);
        let completions = regions.complete_child(b"", 0, false).unwrap();
        assert_eq!(completions.candidates, strs(&["a"]));
        regions.remove_child(b"a").unwrap();
        assert!(regions.remove_child(b"a").is_err());
    }

    fn replace_ranges_test(test: &str, ranges: &[&str]) {
        let env = make_env(test);
        let ts = env.timestamp();
        let mut values = vec![ts.as_str()];
        values.extend_from_slice(ranges);
        env.declare_option("test_ranges", RangeAndStringList::default());
        env.set_option("global", "test_ranges", &values);
        env.add_highlighter("window/", "replace-ranges", &["test_ranges"]);
        env.check(test);
    }

    #[test]
    fn replace_ranges() {
        replace_ranges_test("highlight/replace-ranges/replace-empty-range", &["1.1+0|A", "1.7+0|B", "2.3+0|C"]);
        replace_ranges_test("highlight/replace-ranges/replace-large-with-multiple-lines", &["2.1,3.5|.\n.", "4.2,6.3|.\n."]);
        replace_ranges_test("highlight/replace-ranges/replace-multiline-range", &["2.1,3.5|..", "4.2,5.4|.."]);
        replace_ranges_test(
            "highlight/replace-ranges/replace-only-fully-selected-ranges",
            &[
                "1.2,1.4|{blue}replaced{green} text",
                "2.2,2.4|{blue}replaced{green} text",
                "3.2,3.4|{red}not {blue}replaced{green} text",
            ],
        );
        replace_ranges_test("highlight/replace-ranges/replace-with-multiple-lines", &["2.1,3.5|.\n.", "4.2,5.4|.\n."]);
    }

    #[test]
    fn replace_in_group() {
        let test = "highlight/replace-ranges/replace-in-group";
        let env = make_env(test);
        let ts = env.timestamp();
        env.declare_option("test_ranges", RangeAndStringList::default());
        env.set_option("global", "test_ranges", &[&ts, "1.3+0|A"]);
        env.add_highlighter("window/test", "group", &["-passes", "replace"]);
        env.add_highlighter("window/test/", "replace-ranges", &["test_ranges"]);
        env.check(test);
    }

    #[test]
    fn replace_through_ref() {
        let test = "highlight/replace-ranges/replace-through-ref";
        let env = make_env(test);
        let ts = env.timestamp();
        env.declare_option("test_ranges", RangeAndStringList::default());
        env.set_option("global", "test_ranges", &[&ts, "1.3+0|A"]);
        env.add_highlighter("shared/test", "replace-ranges", &["test_ranges"]);
        env.add_highlighter("window/", "ref", &["-passes", "replace", "test"]);
        env.check(test);
    }

    #[test]
    fn replace_multiline_pulls_new_lines() {
        let test = "highlight/replace-ranges/replace-multiline-range-pulls-new-lines";
        let env = make_env(test);
        env.declare_option("ranges", RangeAndStringList::default());
        env.add_highlighter("window/", "number-lines", &[]);
        env.add_highlighter("window/", "replace-ranges", &["ranges"]);
        let ts = env.timestamp();
        env.set_option("buffer", "ranges", &[&ts, "2.1,6.3|.."]);
        env.check(test);
    }

    #[test]
    fn replace_multiline_scrolls_fine() {
        let test = "highlight/replace-ranges/replace-multiline-range-scrolls-fine";
        let env = make_env(test);
        env.declare_option("ranges", RangeAndStringList::default());
        env.add_highlighter("window/", "number-lines", &[]);
        env.add_highlighter("window/", "replace-ranges", &["ranges"]);
        let ts = env.timestamp();
        env.set_option("buffer", "ranges", &[&ts, "2.1,6.2|..", "35.1,39.2|.."]);
        env.set_cursor(BufferCoord::new(27, 0)); // 27j
        env.check_nth(test, 0);
        env.set_cursor(BufferCoord::new(39, 0)); // gj
        env.check_nth(test, 1);
        env.set_cursor(BufferCoord::new(0, 0)); // gk
        env.check_nth(test, 2);
    }

    #[test]
    fn number_lines_full_relative() {
        let test = "highlight/number-lines/full-relative";
        let env = make_env(test);
        env.add_highlighter("window/", "number-lines", &["-full-relative"]);
        // vv
        env.context.ensure_cursor_visible.set(false);
        let cursor = env.context.selections().main().cursor().coord;
        drop(env.window.update_display_buffer(&env.context).unwrap());
        env.window.center_line(cursor.line);
        env.check(test);
    }

    #[test]
    fn column() {
        let test = "highlight/column/multi-columns";
        let env = make_env(test);
        env.add_highlighter("window/", "number-lines", &[]);
        env.set_option("window", "tabstop", &["4"]);
        env.add_highlighter("window/", "column", &["3", "default,red"]);
        env.add_highlighter("window/", "column", &["7", "default,green"]);
        env.add_highlighter("window/", "column", &["5", "default,blue"]);
        env.check(test);
    }

    #[test]
    fn number_lines_basic() {
        let test = "highlight/number-lines/basic";
        let env = make_env(test);
        env.add_highlighter("window/", "number-lines", &[]);
        env.set_cursor(BufferCoord::new(0, env.buffer.line_length(0) - 2)); // gl
        env.check(test);
    }

    #[test]
    fn wrap() {
        let simple = [
            ("highlight/wrap/avoid-odd-places", vec!["-word"]),
            ("highlight/wrap/basic", vec![]),
            ("highlight/wrap/marker-and-indent", vec!["-marker", ">>>", "-indent"]),
            ("highlight/wrap/word", vec!["-word", "-width", "10"]),
        ];
        for (test, params) in simple {
            let env = make_env(test);
            env.add_highlighter("window/", "wrap", &params);
            env.check(test);
        }

        let test = "highlight/wrap/interact-with-number-lines";
        let env = make_env(test);
        env.add_highlighter("window/", "wrap", &["-word", "-width", "4"]);
        env.add_highlighter("window/", "number-lines", &[]);
        env.check(test);

        let test = "highlight/wrap/interact-with-show-whitespaces";
        let env = make_env(test);
        env.add_highlighter("window/", "wrap", &["-word", "-width", "5"]);
        env.add_highlighter("window/", "show-whitespaces", &[]);
        env.check(test);

        let test = "highlight/wrap/interact-with-tabulation";
        let env = make_env(test);
        env.add_highlighter("window/", "number-lines", &[]);
        env.add_highlighter("window/", "wrap", &[]);
        env.check(test);

        let test = "highlight/wrap/interact-with-replace-ranges";
        let env = make_env(test);
        let ts = env.timestamp();
        env.declare_option("ranges", RangeAndStringList::default());
        env.set_option(
            "global",
            "ranges",
            &[
                &ts,
                "1.1+0|HINT:",
                "2.74+0|WRAPPED HINT",
                "3.8+8|OVERFLOWING HINT XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            ],
        );
        env.add_highlighter("window/", "wrap", &["-word"]);
        env.add_highlighter("window/", "replace-ranges", &["ranges"]);
        env.check(test);

        let test = "highlight/wrap/marker-interact-with-long-replace-ranges";
        let env = make_env(test);
        env.declare_option("ranges", RangeAndStringList::default());
        let ts = env.timestamp();
        let spec = format!("1.1+0|{}", "A".repeat(200));
        env.set_option("window", "ranges", &[&ts, &spec]);
        env.add_highlighter("window/", "wrap", &["-marker", "x"]);
        env.add_highlighter("window/", "replace-ranges", &["ranges"]);
        env.set_cursor(BufferCoord::new(env.buffer.line_count() - 1, 0)); // ge
        env.check(test);
    }

    #[test]
    fn tabulation() {
        let test = "highlight/tabulation/ensure-cursor-on-tab-fully-visible";
        let env = make_env(test);
        env.check(test);
    }

    #[test]
    fn display() {
        let test = "display/horizontal-scroll";
        let env = make_env(test);
        env.set_cursor(BufferCoord::new(0, env.buffer.line_length(0) - 2)); // gl
        env.check(test);

        let test = "display/horizontal-scroll-with-tab";
        let env = make_env(test);
        env.set_cursor(BufferCoord::new(0, env.buffer.line_length(0) - 2)); // gl
        env.check(test);

        let test = "display/horizontal-scroll-onto-tab";
        let env = make_env(test);
        env.set_cursor(BufferCoord::new(1, 77)); // j2f<tab>;
        env.check(test);

        let test = "display/vertical-scroll";
        let env = make_env(test);
        env.set_cursor(BufferCoord::new(env.buffer.line_count() - 1, 0)); // gj
        env.check(test);
    }

    fn render(env: &Env) -> Vec<(String, Face)> {
        let display_buffer = env.window.update_display_buffer(&env.context).unwrap();
        display_buffer
            .lines()
            .iter()
            .flat_map(|l| l.atoms().iter().map(|a| (String::from_utf8_lossy(&a.content()).into_owned(), a.face)))
            .collect()
    }

    #[test]
    fn show_matching() {
        let env = Env::new(b"(a(b)c)\n");
        env.add_highlighter("window/", "show-matching", &[]);
        let atoms = render(&env);
        let matching = GlobalScope::instance().faces().face("MatchingChar");
        let last = atoms.iter().find(|a| a.0 == ")" && a.1 == matching);
        assert!(last.is_some(), "{:?}", atoms);

        let env = Env::new(b"(a(b)c)\n");
        env.add_highlighter("window/", "show-matching", &["-previous"]);
        env.set_cursor(BufferCoord::new(0, 7));
        let atoms = render(&env);
        assert!(atoms.iter().any(|a| a.0 == "(" && a.1 == matching), "{:?}", atoms);
    }

    #[test]
    fn show_whitespaces_and_unprintable() {
        let env = Env::new(b"a \tb\x01\n");
        env.add_highlighter("window/", "show-whitespaces", &[]);
        let atoms = render(&env);
        let contents: Vec<&str> = atoms.iter().map(|a| a.0.as_str()).collect();
        // consecutive replaced atoms with the same face are merged by optimize
        assert_eq!(contents, vec!["a", "·→     ", "b", "�", "¬"]);
    }

    #[test]
    fn flag_lines_and_line() {
        let env = Env::new(b"a\nb\nc\n");
        env.declare_option("flags", LineAndSpecList::default());
        let ts = env.timestamp();
        env.set_option("global", "flags", &[&ts, "2|{red}xx", "1|y"]);
        env.add_highlighter("window/", "flag-lines", &["blue", "flags"]);
        env.add_highlighter("window/", "line", &["3", "green"]);
        let atoms = render(&env);
        let contents: Vec<&str> = atoms.iter().map(|a| a.0.as_str()).collect();
        assert_eq!(contents[..7], ["y ", "a", "\n", "xx", "b\n", "  ", "c\n"]);
        let (blue, red, green) = {
            let faces = GlobalScope::instance().faces();
            (faces.face("blue"), faces.face("red"), faces.face("green"))
        };
        assert_eq!(atoms[0].1, blue);
        assert_eq!(atoms[3].1, red);
        // the line highlighter applies to the whole display line, padded to the window width
        assert_eq!(atoms[5].1, green);
        assert_eq!(atoms[7], (" ".repeat(76), green));

        // line specs follow buffer modifications
        env.buffer.insert(BufferCoord::new(0, 0), b"new\n").unwrap();
        render(&env);
        let list = GlobalScope::instance().options().get(b"flags").unwrap().get_as_strings();
        assert_eq!(list[1..], strs(&["2|y", "3|{red}xx"])[..]);
    }

    #[test]
    fn dynregex() {
        let env = Env::new(b"foo bar foo\n");
        env.declare_option("re", Regex::new(b"bar", RegexCompileFlags::NONE).unwrap());
        env.add_highlighter("window/", "dynregex", &["%opt{re}", "0:red"]);
        let atoms = render(&env);
        let red = GlobalScope::instance().faces().face("red");
        assert!(atoms.iter().any(|a| a.0 == "bar" && a.1 == red), "{:?}", atoms);
        env.set_option("global", "re", &["foo"]);
        let atoms = render(&env);
        assert_eq!(atoms.iter().filter(|a| a.0 == "foo" || a.0 == "oo").count(), 2, "{:?}", atoms);
    }
}
