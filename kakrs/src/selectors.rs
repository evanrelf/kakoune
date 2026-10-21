//! Port of selectors.hh / selectors.cc

use crate::buffer::{Buffer, BufferIterator, StringDataPtr};
use crate::buffer_utils::{is_bol, is_bow, is_eow};
use crate::context::Context;
use crate::event_manager::UrgentEventsIdle;
use crate::exception::{Error, Result};
use crate::option_types::{CodepointOpt, EnumDesc};
use crate::regex::{MatchResults, Regex, RegexIterator, backward_regex_search, match_flags, regex_search_captures};
use crate::regex_vm::{BackwardSearch, ForwardSearch, RegexExecFlags, RegexMode, ThreadedRegexVM, is_direction};
use crate::selection::{CaptureList, MAX_COLUMN, MAX_NON_EOL_COLUMN, Selection, compare_selections};
use crate::unicode::{WordType, categorize, is_blank, is_eol, is_horizontal_blank, is_punctuation, is_word_type};
use crate::units::{BufferCoord, BufferCoordAndTarget, CharCount, Codepoint, LineCount, div_or_zero};
use crate::utf8::{ByteIt, Utf8Iter, it_next, it_previous};

type Utf8Iterator<'a> = Utf8Iter<BufferIterator<'a>>;

crate::kak_flags! {
    pub struct ObjectFlags: u32 {
        const ToBegin = 1;
        const ToEnd   = 2;
        const Inner   = 4;
        const Nested  = 8;
    }
}

impl EnumDesc for ObjectFlags {
    fn desc() -> &'static [(Self, &'static str)] {
        &[(ObjectFlags::ToBegin, "to_begin"), (ObjectFlags::ToEnd, "to_end"), (ObjectFlags::Inner, "inner")]
    }
}
crate::flags_option!(ObjectFlags);

// Helpers -------------------------------------------------------------------

/// Iterators that can be dereferenced and moved, so that skip_while works on
/// both byte iterators and codepoint iterators.
trait SkipIt: Clone + PartialEq {
    type Value: Copy;
    fn value(&self) -> Self::Value;
    fn step(&mut self);
    fn step_back(&mut self);
}

impl SkipIt for BufferIterator<'_> {
    type Value = u8;
    fn value(&self) -> u8 {
        ByteIt::get(self)
    }
    fn step(&mut self) {
        ByteIt::inc(self)
    }
    fn step_back(&mut self) {
        ByteIt::dec(self)
    }
}

impl<'a> SkipIt for Utf8Iterator<'a> {
    type Value = Codepoint;
    fn value(&self) -> Codepoint {
        self.get()
    }
    fn step(&mut self) {
        self.inc()
    }
    fn step_back(&mut self) {
        self.dec()
    }
}

fn skip_while<I: SkipIt>(it: &mut I, end: &I, condition: impl Fn(I::Value) -> bool) -> bool {
    while it != end && condition(it.value()) {
        it.step();
    }
    it != end
}

fn skip_while_reverse<I: SkipIt>(it: &mut I, begin: &I, condition: impl Fn(I::Value) -> bool) -> bool {
    while it != begin && condition(it.value()) {
        it.step_back();
    }
    condition(it.value())
}

// [FAITHFUL] C++ char to Codepoint conversion: char is signed, so bytes
// >= 0x80 become huge codepoints that match no character class.
#[inline]
fn char_cp(c: u8) -> Codepoint {
    c as i8 as Codepoint
}

fn buffer_begin(lines: &[StringDataPtr]) -> BufferIterator<'_> {
    BufferIterator::begin(lines)
}

fn buffer_end(lines: &[StringDataPtr]) -> BufferIterator<'_> {
    BufferIterator::end(lines)
}

/// C++ Utf8Iterator{buffer.iterator_at(coord), buffer}
fn utf8_iterator_at(lines: &[StringDataPtr], coord: BufferCoord) -> Utf8Iterator<'_> {
    Utf8Iter::new(BufferIterator::new(lines, coord), buffer_begin(lines), buffer_end(lines))
}

fn utf8_range(first: &Utf8Iterator, last: &Utf8Iterator) -> Selection {
    Selection::from_range(first.base().coord(), last.base().coord())
}

fn get_extra_word_chars(context: &Context) -> Vec<Codepoint> {
    context.options().opt("extra_word_chars").get::<Vec<CodepointOpt>>().iter().map(|c| c.0).collect()
}

// ---------------------------------------------------------------------------

pub fn keep_direction(mut res: Selection, reference: &Selection) -> Selection {
    if (res.cursor().coord < res.anchor()) != (reference.cursor().coord < reference.anchor()) {
        // std::swap<BufferCoord>: the cursor target is kept
        let anchor = res.anchor();
        let cursor = res.cursor().coord;
        res.set_anchor(cursor);
        res.cursor_mut().coord = anchor;
    }
    res
}

pub fn select_to_next_word(word_type: WordType, context: &Context, selection: &Selection) -> Option<Selection> {
    let extra_word_chars = get_extra_word_chars(context);
    let buffer = context.buffer();
    let lines = buffer.lines();
    let buf_end = utf8_iterator_at(&lines, buffer.end_coord());
    let mut begin = utf8_iterator_at(&lines, selection.cursor().coord);
    if begin.add(1) == buf_end {
        return None;
    }
    if categorize(word_type, begin.get(), &extra_word_chars)
        != categorize(word_type, begin.add(1).get(), &extra_word_chars)
    {
        begin.inc();
    }

    if !skip_while(&mut begin, &buf_end, is_eol) {
        return None;
    }
    let mut end = begin.add(1);

    let is_word = |c: Codepoint| is_word_type(word_type, c, &extra_word_chars);
    let is_punctuation = |c: Codepoint| is_punctuation(c, &extra_word_chars);

    if is_word(begin.get()) {
        skip_while(&mut end, &buf_end, is_word);
    } else if is_punctuation(begin.get()) {
        skip_while(&mut end, &buf_end, is_punctuation);
    }

    skip_while(&mut end, &buf_end, is_horizontal_blank);

    Some(utf8_range(&begin, &end.sub(1)))
}

pub fn select_to_next_word_end(word_type: WordType, context: &Context, selection: &Selection) -> Option<Selection> {
    let extra_word_chars = get_extra_word_chars(context);
    let buffer = context.buffer();
    let lines = buffer.lines();
    let buf_end = utf8_iterator_at(&lines, buffer.end_coord());
    let mut begin = utf8_iterator_at(&lines, selection.cursor().coord);
    if begin.add(1) == buf_end {
        return None;
    }
    if categorize(word_type, begin.get(), &extra_word_chars)
        != categorize(word_type, begin.add(1).get(), &extra_word_chars)
    {
        begin.inc();
    }

    if !skip_while(&mut begin, &buf_end, is_eol) {
        return None;
    }
    let mut end = begin.clone();
    skip_while(&mut end, &buf_end, is_horizontal_blank);

    let is_word = |c: Codepoint| is_word_type(word_type, c, &extra_word_chars);
    let is_punctuation = |c: Codepoint| is_punctuation(c, &extra_word_chars);

    if is_word(end.get()) {
        skip_while(&mut end, &buf_end, is_word);
    } else if is_punctuation(end.get()) {
        skip_while(&mut end, &buf_end, is_punctuation);
    }

    Some(utf8_range(&begin, &end.sub(1)))
}

pub fn select_to_previous_word(word_type: WordType, context: &Context, selection: &Selection) -> Option<Selection> {
    let extra_word_chars = get_extra_word_chars(context);
    let buffer = context.buffer();
    let lines = buffer.lines();
    let buf_begin = utf8_iterator_at(&lines, BufferCoord::new(0, 0));
    let mut begin = utf8_iterator_at(&lines, selection.cursor().coord);
    if begin == buf_begin {
        return None;
    }
    if categorize(word_type, begin.get(), &extra_word_chars)
        != categorize(word_type, begin.sub(1).get(), &extra_word_chars)
    {
        begin.dec();
    }

    skip_while_reverse(&mut begin, &buf_begin, is_eol);
    let mut end = begin.clone();

    let is_word = |c: Codepoint| is_word_type(word_type, c, &extra_word_chars);
    let is_punctuation = |c: Codepoint| is_punctuation(c, &extra_word_chars);

    let mut with_end = skip_while_reverse(&mut end, &buf_begin, is_horizontal_blank);
    if is_word(end.get()) {
        with_end = skip_while_reverse(&mut end, &buf_begin, is_word);
    } else if is_punctuation(end.get()) {
        with_end = skip_while_reverse(&mut end, &buf_begin, is_punctuation);
    }

    Some(utf8_range(&begin, &if with_end { end } else { end.add(1) }))
}

pub fn select_word(
    word_type: WordType,
    context: &Context,
    selection: &Selection,
    _count: i32,
    flags: ObjectFlags,
) -> Option<Selection> {
    let extra_word_chars = get_extra_word_chars(context);
    let buffer = context.buffer();
    let lines = buffer.lines();
    let buf_begin = utf8_iterator_at(&lines, BufferCoord::new(0, 0));
    let buf_end = utf8_iterator_at(&lines, buffer.end_coord());

    let is_word = |c: Codepoint| is_word_type(word_type, c, &extra_word_chars);

    let mut first = utf8_iterator_at(&lines, selection.cursor().coord);
    if !is_word(first.get()) {
        return None;
    }

    let mut last = first.clone();
    if flags.has(ObjectFlags::ToBegin) {
        skip_while_reverse(&mut first, &buf_begin, is_word);
        if !is_word(first.get()) {
            first.inc();
        }
    }
    if flags.has(ObjectFlags::ToEnd) {
        skip_while(&mut last, &buf_end, is_word);
        if !flags.has(ObjectFlags::Inner) {
            skip_while(&mut last, &buf_end, is_horizontal_blank);
        }
        last.dec();
    }
    Some(if flags.has(ObjectFlags::ToEnd) { utf8_range(&first, &last) } else { utf8_range(&last, &first) })
}

pub fn select_to_line_end<const ONLY_MOVE: bool>(context: &Context, selection: &Selection) -> Option<Selection> {
    let buffer = context.buffer();
    let lines = buffer.lines();
    let begin = selection.cursor().coord;
    let line = begin.line;
    let mut end = it_previous(
        &BufferIterator::new(&lines, BufferCoord::new(line, buffer.line_length(line) - 1)),
        &BufferIterator::new(&lines, BufferCoord::line(line)),
    )
    .coord();
    if end < begin {
        // Do not go backward when cursor is on eol
        end = begin;
    }
    Some(Selection::from_range(
        if ONLY_MOVE { end } else { begin },
        BufferCoordAndTarget::new(end, MAX_NON_EOL_COLUMN, -1),
    ))
}

pub fn select_to_line_begin<const ONLY_MOVE: bool>(_context: &Context, selection: &Selection) -> Option<Selection> {
    let begin = selection.cursor().coord;
    let end = BufferCoord::line(begin.line);
    Some(Selection::from_range(if ONLY_MOVE { end } else { begin }, end))
}

pub fn select_to_first_non_blank(context: &Context, selection: &Selection) -> Option<Selection> {
    let buffer = context.buffer();
    let lines = buffer.lines();
    let line = selection.cursor().line;
    let mut it = BufferIterator::new(&lines, BufferCoord::line(line));
    skip_while(&mut it, &BufferIterator::new(&lines, BufferCoord::line(line + 1)), |c| is_horizontal_blank(char_cp(c)));
    Some(Selection::new(it.coord()))
}

pub fn select_matching<const FORWARD: bool>(context: &Context, selection: &Selection) -> Option<Selection> {
    let buffer = context.buffer();
    let matching_pairs: Vec<Codepoint> =
        context.options().opt("matching_pairs").get::<Vec<CodepointOpt>>().iter().map(|c| c.0).collect();
    let lines = buffer.lines();
    let buf_begin = utf8_iterator_at(&lines, BufferCoord::new(0, 0));
    let buf_end = utf8_iterator_at(&lines, buffer.end_coord());
    let mut it = utf8_iterator_at(&lines, selection.cursor().coord);
    let find = |c: Codepoint| matching_pairs.iter().position(|&p| p == c);
    let mut match_ = None;

    if FORWARD {
        while it != buf_end {
            match_ = find(it.get());
            if match_.is_some() {
                break;
            }
            it.inc();
        }
    } else {
        loop {
            match_ = find(it.get());
            if match_.is_some() || it == buf_begin {
                break;
            }
            it.dec();
        }
    }

    let match_ = match_?;

    let begin = it.clone();

    if match_ % 2 == 0 {
        let mut level = 0;
        let opening = matching_pairs[match_];
        let closing = matching_pairs[match_ + 1];
        while it != buf_end {
            let c = it.get();
            if c == opening {
                level += 1;
            } else if c == closing && {
                level -= 1;
                level == 0
            } {
                return Some(utf8_range(&begin, &it));
            }
            it.inc();
        }
    } else {
        let mut level = 0;
        let opening = matching_pairs[match_ - 1];
        let closing = matching_pairs[match_];
        loop {
            let c = it.get();
            if c == closing {
                level += 1;
            } else if c == opening && {
                level -= 1;
                level == 0
            } {
                return Some(utf8_range(&begin, &it));
            }
            if it == buf_begin {
                break;
            }
            it.dec();
        }
    }
    None
}

/// main capture (begin, end) of a match
fn main_capture<I: Clone>(m: &MatchResults<I>) -> (I, I) {
    (m[0].first.clone().unwrap(), m[0].second.clone().unwrap())
}

fn find_opening<I: ByteIt>(
    mut pos: I,
    begin: &I,
    end: &I,
    opening: &Regex,
    closing: &Regex,
    mut level: i32,
    nestable: bool,
) -> Result<Option<(I, I)>> {
    // [DEVIATION] urgent event errors abort the search and are returned by check()
    let mut idle = UrgentEventsIdle::default();
    let mut res = MatchResults::default();
    // When on the token of a non-nestable block, we want to consider it opening
    if nestable
        && backward_regex_search(
            begin.clone(),
            pos.clone(),
            begin.clone(),
            end.clone(),
            &mut res,
            closing,
            RegexExecFlags::NONE,
            || idle.idle(),
        )
        && res[0].second.as_ref() == Some(&pos)
    {
        pos = res[0].first.clone().unwrap();
    }
    idle.check()?;

    let mut idle_func = || idle.idle();
    let mut matches = RegexIterator::<I, BackwardSearch>::new(
        begin.clone(),
        pos.clone(),
        begin.clone(),
        end.clone(),
        opening,
        RegexExecFlags::NONE,
    )
    .with_idle(&mut idle_func);
    while let Some(m) = matches.next_match() {
        let (first, second) = main_capture(m);
        if nestable {
            let mut closings_idle = UrgentEventsIdle::default();
            let mut idle_func = || closings_idle.idle();
            let mut closings = RegexIterator::<I, BackwardSearch>::new(
                second.clone(),
                pos.clone(),
                begin.clone(),
                end.clone(),
                closing,
                RegexExecFlags::NONE,
            )
            .with_idle(&mut idle_func);
            while closings.next_match().is_some() {
                level += 1;
            }
            closings_idle.check()?;
        }

        if !nestable || level == 0 {
            return Ok(Some((first, second)));
        }
        pos = first;
        level -= 1;
    }
    idle.check()?;
    Ok(None)
}

fn find_closing<I: ByteIt>(
    mut pos: I,
    begin: &I,
    end: &I,
    opening: &Regex,
    closing: &Regex,
    mut level: i32,
    nestable: bool,
) -> Result<Option<(I, I)>> {
    // [DEVIATION] urgent event errors abort the search and are returned by check()
    let mut idle = UrgentEventsIdle::default();
    let mut idle_func = || idle.idle();
    let mut matches = RegexIterator::<I, ForwardSearch>::new(
        pos.clone(),
        end.clone(),
        begin.clone(),
        end.clone(),
        closing,
        RegexExecFlags::NONE,
    )
    .with_idle(&mut idle_func);
    while let Some(m) = matches.next_match() {
        let (first, second) = main_capture(m);
        if nestable {
            let mut openings_idle = UrgentEventsIdle::default();
            let mut idle_func = || openings_idle.idle();
            let mut openings = RegexIterator::<I, ForwardSearch>::new(
                pos.clone(),
                first.clone(),
                begin.clone(),
                end.clone(),
                opening,
                RegexExecFlags::NONE,
            )
            .with_idle(&mut idle_func);
            while openings.next_match().is_some() {
                level += 1;
            }
            openings_idle.check()?;
        }

        if !nestable || level == 0 {
            return Ok(Some((first, second)));
        }
        pos = second;
        level -= 1;
    }
    idle.check()?;
    Ok(None)
}

// [DEVIATION] std::next, which would go past the end in the C++ code if it
// was already at the end (undefined behaviour), we stay at end instead.
fn next_byte<I: ByteIt>(it: &I, end: &I) -> I {
    let mut it = it.clone();
    if &it != end {
        it.inc();
    }
    it
}

fn find_surrounding<I: ByteIt>(
    begin: &I,
    end: &I,
    pos: I,
    opening: &Regex,
    closing: &Regex,
    flags: ObjectFlags,
    mut level: i32,
) -> Result<Option<(I, I)>> {
    let empty = |m: &(I, I)| m.0 == m.1;
    let nestable = opening != closing;
    let mut first = pos.clone();
    let mut last = pos.clone();
    if flags.has(ObjectFlags::ToBegin) {
        let Some(res) = find_opening(next_byte(&first, end), begin, end, opening, closing, level, nestable)? else {
            return Ok(None);
        };
        first = if flags.has(ObjectFlags::Inner) { res.1.clone() } else { res.0.clone() };
        if flags.has(ObjectFlags::ToEnd) {
            // ensure we find the matching end
            last = if empty(&res) { next_byte(&res.1, end) } else { res.1 };
            level = 0;
        }
    } else {
        // [DEVIATION] urgent event errors abort the search and are returned by check()
        let mut idle = UrgentEventsIdle::default();
        let mut res = MatchResults::default();
        let found = regex_search_captures(
            pos.clone(),
            end.clone(),
            begin.clone(),
            end.clone(),
            &mut res,
            opening,
            RegexExecFlags::NONE,
            || idle.idle(),
        );
        idle.check()?;
        // Skip opening match if pos lies on it
        if found && res[0].first.as_ref() == Some(&pos) {
            let (m_first, m_second) = main_capture(&res);
            last = if m_first == m_second { next_byte(&m_second, end) } else { m_second };
        }
    }

    if flags.has(ObjectFlags::ToEnd) {
        let Some(res) = find_closing(last, begin, end, opening, closing, level, nestable)? else {
            return Ok(None);
        };
        last = it_previous(if flags.has(ObjectFlags::Inner) { &res.0 } else { &res.1 }, begin);
    }
    if first > last {
        return Ok(None);
    }

    Ok(Some((first, last)))
}

pub fn select_surrounding(
    context: &Context,
    selection: &Selection,
    opening: &Regex,
    closing: &Regex,
    level: i32,
    flags: ObjectFlags,
) -> Result<Option<Selection>> {
    let buffer = context.buffer();
    let lines = buffer.lines();
    let (begin, end) = (buffer_begin(&lines), buffer_end(&lines));
    let pos = BufferIterator::new(&lines, selection.cursor().coord);

    let mut res = find_surrounding(&begin, &end, pos, opening, closing, flags, level)?;

    // If the ends we're changing didn't move, find the parent
    if let Some(r) = &res
        && !flags.has(ObjectFlags::Inner)
        && (r.0.coord() == selection.min() || !flags.has(ObjectFlags::ToBegin))
        && (r.1.coord() == selection.max() || !flags.has(ObjectFlags::ToEnd))
    {
        res = find_surrounding(&begin, &end, pos, opening, closing, flags, level + 1)?;
    }

    let Some((first, last)) = res else { return Ok(None) };
    Ok(Some(if flags.has(ObjectFlags::ToEnd) {
        Selection::from_range(first.coord(), last.coord())
    } else {
        Selection::from_range(last.coord(), first.coord())
    }))
}

pub fn select_to(
    context: &Context,
    selection: &Selection,
    c: Codepoint,
    mut count: i32,
    inclusive: bool,
) -> Option<Selection> {
    let buffer = context.buffer();
    let lines = buffer.lines();
    let buf_end = utf8_iterator_at(&lines, buffer.end_coord());
    let begin = utf8_iterator_at(&lines, selection.cursor().coord);
    let mut end = begin.clone();
    loop {
        end.inc();
        skip_while(&mut end, &buf_end, |cur| cur != c);
        if end == buf_end {
            return None;
        }
        count -= 1;
        if count <= 0 {
            break;
        }
    }

    Some(utf8_range(&begin, &if inclusive { end } else { end.sub(1) }))
}

pub fn select_to_reverse(
    context: &Context,
    selection: &Selection,
    c: Codepoint,
    mut count: i32,
    inclusive: bool,
) -> Option<Selection> {
    let buffer = context.buffer();

    // if we are selecting backwards from the beginning of the buffer,
    // there is nothing more that can be selected.
    if selection.cursor().coord == BufferCoord::new(0, 0) {
        return None;
    }

    let lines = buffer.lines();
    let buf_begin = utf8_iterator_at(&lines, BufferCoord::new(0, 0));
    let begin = utf8_iterator_at(&lines, selection.cursor().coord);
    let mut end = begin.clone();
    loop {
        end.dec();
        if skip_while_reverse(&mut end, &buf_begin, |cur| cur != c) {
            return None;
        }
        count -= 1;
        if count <= 0 {
            break;
        }
    }

    Some(utf8_range(&begin, &if inclusive { end } else { end.add(1) }))
}

pub fn select_number(context: &Context, selection: &Selection, _count: i32, flags: ObjectFlags) -> Option<Selection> {
    let is_number = |c: u8| c.is_ascii_digit() || (!flags.has(ObjectFlags::Inner) && c == b'.');

    let buffer = context.buffer();
    let lines = buffer.lines();
    let (buf_begin, buf_end) = (buffer_begin(&lines), buffer_end(&lines));
    let mut first = BufferIterator::new(&lines, selection.cursor().coord);
    let mut last = first;

    if !is_number(first.value()) && first.value() != b'-' {
        return None;
    }

    if flags.has(ObjectFlags::ToBegin) {
        skip_while_reverse(&mut first, &buf_begin, is_number);
        if !is_number(first.value()) && first.value() != b'-' && first.next_byte() != buf_end {
            first.step();
        }
    }

    if flags.has(ObjectFlags::ToEnd) {
        if last.value() == b'-' {
            last.step();
        }
        skip_while(&mut last, &buf_end, is_number);
        if last != buf_begin {
            last.step_back();
        }
    }

    Some(if flags.has(ObjectFlags::ToEnd) {
        Selection::from_range(first.coord(), last.coord())
    } else {
        Selection::from_range(last.coord(), first.coord())
    })
}

pub fn select_sentence(context: &Context, selection: &Selection, count: i32, flags: ObjectFlags) -> Option<Selection> {
    let is_end_of_sentence = |c: u8| c == b'.' || c == b';' || c == b'!' || c == b'?';

    let buffer = context.buffer();
    let lines = buffer.lines();
    let (buf_begin, buf_end) = (buffer_begin(&lines), buffer_end(&lines));
    let mut first = BufferIterator::new(&lines, selection.cursor().coord);
    let mut last = first;

    for i in 0..=count {
        if !flags.has(ObjectFlags::ToEnd) && first != buf_begin {
            let mut prev_non_blank = first.prev_byte();
            skip_while_reverse(&mut prev_non_blank, &buf_begin, |c| {
                is_horizontal_blank(char_cp(c)) || is_eol(char_cp(c))
            });
            if is_end_of_sentence(prev_non_blank.value()) {
                first = prev_non_blank;
            }
        }

        if i == 0 {
            last = first;
        }

        if flags.has(ObjectFlags::ToBegin) {
            let mut saw_non_blank = false;
            while first != buf_begin {
                let cur = first.value();
                let prev = first.prev_byte().value();
                if !is_horizontal_blank(char_cp(cur)) {
                    saw_non_blank = true;
                }
                if is_eol(char_cp(prev)) && is_eol(char_cp(cur)) && first.next_byte() != buf_end {
                    first.step();
                    break;
                } else if is_end_of_sentence(prev) {
                    if saw_non_blank {
                        break;
                    } else if flags.has(ObjectFlags::ToEnd) {
                        last = first.prev_byte();
                    }
                }
                first.step_back();
            }
            skip_while(&mut first, &buf_end, |c| is_horizontal_blank(char_cp(c)));
        }
        if flags.has(ObjectFlags::ToEnd) {
            while last != buf_end {
                let cur = last.value();
                if is_end_of_sentence(cur)
                    || (is_eol(char_cp(cur))
                        && (last.next_byte() == buf_end || is_eol(char_cp(last.next_byte().value()))))
                {
                    break;
                }
                last.step();
            }
            if !flags.has(ObjectFlags::Inner) && last != buf_end {
                last.step();
                skip_while(&mut last, &buf_end, |c| is_horizontal_blank(char_cp(c)));
                last.step_back();
            }
        }
    }
    Some(if flags.has(ObjectFlags::ToEnd) {
        Selection::from_range(first.coord(), last.coord())
    } else {
        Selection::from_range(last.coord(), first.coord())
    })
}

pub fn select_paragraph(context: &Context, selection: &Selection, count: i32, flags: ObjectFlags) -> Option<Selection> {
    let buffer = context.buffer();
    let lines = buffer.lines();
    let (buf_begin, buf_end) = (buffer_begin(&lines), buffer_end(&lines));
    let mut first = BufferIterator::new(&lines, selection.cursor().coord);
    let mut last = first;
    let is_eol_char = |c: u8| is_eol(char_cp(c));

    for i in 0..=count {
        if !flags.has(ObjectFlags::ToEnd)
            && first.coord() > BufferCoord::new(0, 1)
            && is_eol_char(first.prev_byte().value())
            && first.prev_byte() != buf_begin
            && is_eol_char(first.sub(2).value())
        {
            first.step_back();
        } else if flags.has(ObjectFlags::ToEnd)
            && first != buf_begin
            && first.next_byte() != buf_end
            && is_eol_char(first.prev_byte().value())
            && is_eol_char(first.value())
        {
            first.step();
        }
        if i == 0 {
            last = first;
        }

        if flags.has(ObjectFlags::ToBegin) && first != buf_begin {
            skip_while_reverse(&mut first, &buf_begin, is_eol_char);
            if flags.has(ObjectFlags::ToEnd) {
                last = first;
            }
            while first != buf_begin {
                let cur = first.value();
                let prev = first.prev_byte().value();
                if is_eol_char(prev) && is_eol_char(cur) {
                    first.step();
                    break;
                }
                first.step_back();
            }
        }
        if flags.has(ObjectFlags::ToEnd) {
            if last != buf_end && is_eol_char(last.value()) {
                last.step();
            }
            while last != buf_end {
                if last != buf_begin && is_eol_char(last.value()) && is_eol_char(last.prev_byte().value()) {
                    if !flags.has(ObjectFlags::Inner) {
                        skip_while(&mut last, &buf_end, is_eol_char);
                    }
                    break;
                }
                last.step();
            }
            last.step_back();
        }
    }
    Some(if flags.has(ObjectFlags::ToEnd) {
        Selection::from_range(first.coord(), last.coord())
    } else {
        Selection::from_range(last.coord(), first.coord())
    })
}

pub fn select_whitespaces(
    context: &Context,
    selection: &Selection,
    _count: i32,
    flags: ObjectFlags,
) -> Option<Selection> {
    let is_whitespace = |c: u8| c == b' ' || c == b'\t' || (!flags.has(ObjectFlags::Inner) && c == b'\n');
    let buffer = context.buffer();
    let lines = buffer.lines();
    let (buf_begin, buf_end) = (buffer_begin(&lines), buffer_end(&lines));
    let mut first = BufferIterator::new(&lines, selection.cursor().coord);
    let mut last = first;

    if !is_whitespace(first.value()) {
        return None;
    }

    if flags.has(ObjectFlags::ToBegin) && is_whitespace(first.value()) {
        skip_while_reverse(&mut first, &buf_begin, is_whitespace);
        if !is_whitespace(first.value()) {
            first.step();
        }
    }
    if flags.has(ObjectFlags::ToEnd) && is_whitespace(last.value()) {
        skip_while(&mut last, &buf_end, is_whitespace);
        last.step_back();
    }
    Some(if flags.has(ObjectFlags::ToEnd) {
        Selection::from_range(first.coord(), last.coord())
    } else {
        Selection::from_range(last.coord(), first.coord())
    })
}

pub fn select_indent(context: &Context, selection: &Selection, _count: i32, flags: ObjectFlags) -> Option<Selection> {
    let get_indent = |s: &[u8], tabstop: i32| -> CharCount {
        let mut indent: CharCount = 0;
        for &c in s {
            if c == b' ' {
                indent += 1;
            } else if c == b'\t' {
                indent = (div_or_zero(indent, tabstop) + 1) * tabstop;
            } else {
                break;
            }
        }
        indent
    };

    let get_current_indent = |lines: &[StringDataPtr], line: LineCount, tabstop: i32| -> CharCount {
        for l in (0..=line).rev() {
            if &*lines[l as usize] != b"\n" {
                return get_indent(&lines[l as usize], tabstop);
            }
        }
        for l in line + 1..lines.len() as LineCount {
            if &*lines[l as usize] != b"\n" {
                return get_indent(&lines[l as usize], tabstop);
            }
        }
        0
    };

    let is_only_whitespaces = |s: &[u8]| s.iter().all(|&c| c == b' ' || c == b'\t' || c == b'\n');

    let to_begin = flags.has(ObjectFlags::ToBegin);
    let to_end = flags.has(ObjectFlags::ToEnd);

    let buffer = context.buffer();
    let tabstop = context.options().val::<i32>("tabstop");
    let lines = buffer.lines();
    let pos = selection.cursor();
    let line = pos.line;
    let indent = get_current_indent(&lines, line, tabstop);

    let mut begin_line = line - 1;
    if to_begin {
        while begin_line >= 0
            && (&*lines[begin_line as usize] == b"\n" || get_indent(&lines[begin_line as usize], tabstop) >= indent)
        {
            begin_line -= 1;
        }
    }
    begin_line += 1;
    let mut end_line = line + 1;
    if to_end {
        let end = lines.len() as LineCount;
        while end_line < end
            && (&*lines[end_line as usize] == b"\n" || get_indent(&lines[end_line as usize], tabstop) >= indent)
        {
            end_line += 1;
        }
    }
    end_line -= 1;
    // remove only whitespaces lines in inner mode
    if flags.has(ObjectFlags::Inner) {
        while begin_line < end_line && is_only_whitespaces(&lines[begin_line as usize]) {
            begin_line += 1;
        }
        while begin_line < end_line && is_only_whitespaces(&lines[end_line as usize]) {
            end_line -= 1;
        }
    }

    // As in the C++, first is a BufferCoordAndTarget (keeping pos target)
    // and last a BufferCoord
    let first: BufferCoordAndTarget = if to_begin { BufferCoord::line(begin_line).into() } else { pos };
    let last = if to_end { BufferCoord::new(end_line, lines[end_line as usize].len() as i32 - 1) } else { pos.coord };
    Some(if to_end { Selection::from_range(first.coord, last) } else { Selection::from_range(last, first) })
}

pub fn select_argument(context: &Context, selection: &Selection, level: i32, flags: ObjectFlags) -> Option<Selection> {
    #[derive(PartialEq, Eq, Clone, Copy)]
    enum Class {
        None,
        Opening,
        Closing,
        Delimiter,
    }
    let classify = |c: u8| match c {
        b'(' | b'[' | b'{' => Class::Opening,
        b')' | b']' | b'}' => Class::Closing,
        b',' | b';' => Class::Delimiter,
        _ => Class::None,
    };

    let buffer = context.buffer();
    let lines = buffer.lines();
    let (buf_begin, buf_end) = (buffer_begin(&lines), buffer_end(&lines));
    let mut pos = BufferIterator::new(&lines, selection.cursor().coord);
    match classify(pos.value()) {
        //Class::Closing => if pos.next_byte() != buf_end { pos.step() },
        Class::Opening | Class::Delimiter if pos != buf_begin => pos.step_back(),
        _ => {}
    }

    let mut first_arg = false;
    let mut begin = pos;
    let mut lev = level;
    while begin != buf_begin {
        let c = classify(begin.value());
        if c == Class::Closing {
            lev += 1;
        } else if c == Class::Opening && {
            lev -= 1;
            lev + 1 == 0
        } {
            first_arg = true;
            begin.step();
            break;
        } else if c == Class::Delimiter && lev == 0 {
            begin.step();
            break;
        }
        begin.step_back();
    }

    let mut last_arg = false;
    let mut end = pos;
    let mut lev = level;
    while end != buf_end {
        let c = classify(end.value());
        if c == Class::Opening {
            lev += 1;
        } else if end != pos && c == Class::Closing && {
            lev -= 1;
            lev + 1 == 0
        } {
            last_arg = true;
            end.step_back();
            break;
        } else if c == Class::Delimiter && lev == 0 {
            // include whitespaces *after* the delimiter only for first argument
            if first_arg && !flags.has(ObjectFlags::Inner) {
                while end.next_byte() != buf_end && is_blank(char_cp(end.next_byte().value())) {
                    end.step();
                }
            }
            break;
        }
        end.step();
    }

    if flags.has(ObjectFlags::Inner) {
        // [DEVIATION] The C++ decrements unconditionally, which is undefined
        // behaviour when end is at the buffer begin
        if !last_arg && end != buf_begin {
            end.step_back();
        }
        let is_blank_char = |c: u8| is_blank(char_cp(c));
        skip_while(&mut begin, &end, is_blank_char);
        skip_while_reverse(&mut end, &begin, is_blank_char);
    }
    // get starting delimiter for non inner last arg
    else if !first_arg && last_arg && begin != buf_begin {
        begin.step_back();
    }

    if end == buf_end {
        end.step_back();
    }

    if flags.has(ObjectFlags::ToBegin) && !flags.has(ObjectFlags::ToEnd) {
        return Some(Selection::from_range(pos.coord(), begin.coord()));
    }
    Some(Selection::from_range(if flags.has(ObjectFlags::ToBegin) { begin } else { pos }.coord(), end.coord()))
}

fn for_each_sel(
    context: &Context,
    mut callback: impl for<'b> FnMut(&mut Vec<Selection>, BufferIterator<'b>, BufferIterator<'b>) -> Result<()>,
) -> Result<Vec<Selection>> {
    let buffer = context.buffer();
    let ranges: Vec<(BufferCoord, BufferCoord)> =
        context.selections().iter().map(|sel| (sel.min(), buffer.char_next(sel.max()))).collect();
    let lines = buffer.lines();
    let mut res = Vec::new();
    for (begin, end) in ranges {
        callback(&mut res, BufferIterator::new(&lines, begin), BufferIterator::new(&lines, end))?;
    }
    if res.is_empty() {
        return Err(Error::runtime("nothing selected"));
    }
    Ok(res)
}

/// std::find_if on a BufferIterator range
fn find_if<'a>(mut it: BufferIterator<'a>, end: &BufferIterator<'a>, pred: impl Fn(u8) -> bool) -> BufferIterator<'a> {
    while it != *end && !pred(it.value()) {
        it.step();
    }
    it
}

pub fn select_nested_words(
    word_type: WordType,
    context: &Context,
    _count: i32,
    flags: ObjectFlags,
) -> Result<Vec<Selection>> {
    let extra_word_chars = get_extra_word_chars(context);
    let is_word = |c: Codepoint| is_word_type(word_type, c, &extra_word_chars);
    let is_not_word = |c: Codepoint| !is_word(c);
    let buffer = context.buffer();

    for_each_sel(context, |res, beg, end| {
        let lines = beg.lines();
        let make = |it| Utf8Iter::new(it, buffer_begin(lines), buffer_end(lines));
        let end = make(end);
        let mut it = make(beg);
        // [FAITHFUL] hangs when the selection ends inside an invalid utf-8
        // sequence, as codepoint iteration then jumps over end
        while it != end {
            if !skip_while(&mut it, &end, is_not_word) {
                break;
            }
            let start = it.clone();
            skip_while(&mut it, &end, is_word);
            if !flags.has(ObjectFlags::Inner) {
                skip_while(&mut it, &end, is_horizontal_blank);
            }
            res.push(Selection::from_range(start.base().coord(), buffer.char_prev(it.base().coord())));
        }
        Ok(())
    })
}

pub fn select_nested_numbers(context: &Context, _count: i32, flags: ObjectFlags) -> Result<Vec<Selection>> {
    let is_digit = |c: u8| c.is_ascii_digit();
    let is_number = |c: u8| is_digit(c) || (!flags.has(ObjectFlags::Inner) && c == b'.');
    let buffer = context.buffer();

    for_each_sel(context, |res, mut it, end| {
        while it != end {
            if !skip_while(&mut it, &end, |c| !(c == b'-' || is_number(c))) {
                break;
            }

            let start = it;
            if it.value() == b'-' {
                it.step();
            }
            skip_while(&mut it, &end, is_number);
            if it == start {
                it.step();
            } else if find_if(start, &it, is_digit) != it {
                // [FAITHFUL] when only end of lines remain, start is `end` and the
            // selection goes past the buffer end; a later edit then crashes
            // (see UPSTREAM-QUIRKS.md)
            res.push(Selection::from_range(start.coord(), buffer.char_prev(it.coord())));
            }
        }
        Ok(())
    })
}

pub fn select_nested_sentences(context: &Context, _count: i32, flags: ObjectFlags) -> Result<Vec<Selection>> {
    let is_end_of_sentence = |c: u8| c == b'.' || c == b';' || c == b'!' || c == b'?';
    let buffer = context.buffer();

    for_each_sel(context, |res, mut it, end| {
        while it != end {
            if !skip_while(&mut it, &end, |c| c == b' ' || c == b'\n' || c == b'\t') {
                break;
            }
            let start = it;
            it = find_if(it, &end, is_end_of_sentence);
            if it != end {
                it.step();
                if !flags.has(ObjectFlags::Inner) {
                    skip_while(&mut it, &end, |c| c == b' ');
                }
            }
            res.push(Selection::from_range(start.coord(), buffer.char_prev(it.coord())));
        }
        Ok(())
    })
}

pub fn select_nested_paragraphs(context: &Context, _count: i32, flags: ObjectFlags) -> Result<Vec<Selection>> {
    let is_eol = |c: u8| c == b'\n';
    let buffer = context.buffer();

    for_each_sel(context, |res, mut it, end| {
        while it != end {
            let start = find_if(it, &end, |c| !is_eol(c));
            let mut consecutive_eols = 0;
            it = start;
            while it != end {
                if is_eol(it.value()) {
                    consecutive_eols += 1;
                    if consecutive_eols == 2 && flags.has(ObjectFlags::Inner) {
                        break;
                    }
                } else if consecutive_eols >= 2 {
                    break;
                } else {
                    consecutive_eols = 0;
                }
                it.step();
            }
            res.push(Selection::from_range(start.coord(), buffer.char_prev(it.coord())));
        }
        Ok(())
    })
}

pub fn select_nested_whitespaces(context: &Context, _count: i32, flags: ObjectFlags) -> Result<Vec<Selection>> {
    let is_whitespace = |c: u8| c == b' ' || c == b'\t' || (!flags.has(ObjectFlags::Inner) && c == b'\n');
    let buffer = context.buffer();

    for_each_sel(context, |res, mut it, end| {
        while it != end {
            if !skip_while(&mut it, &end, |c| !is_whitespace(c)) {
                break;
            }
            let start = it;
            skip_while(&mut it, &end, is_whitespace);
            res.push(Selection::from_range(start.coord(), buffer.char_prev(it.coord())));
        }
        Ok(())
    })
}

pub fn select_nested_indents(_context: &Context, _count: i32, _flags: ObjectFlags) -> Result<Vec<Selection>> {
    Err(Error::runtime("nested indents are not implemented"))
}

pub fn select_nested_arguments(context: &Context, _count: i32, flags: ObjectFlags) -> Result<Vec<Selection>> {
    let is_whitespace = |c: u8| c == b' ' || c == b'\t' || c == b'\n';
    let inner = flags.has(ObjectFlags::Inner);
    let buffer = context.buffer();

    for_each_sel(context, |res, mut it, end| {
        let mut start = it;
        let mut level = 0;
        while it != end {
            match it.value() {
                b'(' | b'[' | b'{' => {
                    level += 1;
                    it.step();
                }
                b')' | b']' | b'}' => {
                    level -= 1;
                    it.step();
                }
                b',' | b';' if level == 0 => {
                    res.push(Selection::from_range(
                        start.coord(),
                        if inner { buffer.char_prev(it.coord()) } else { it.coord() },
                    ));
                    it.step();
                    while inner && it != end && is_whitespace(it.value()) {
                        it.step();
                    }
                    start = it;
                }
                _ => it.step(),
            }
        }
        if start != it {
            res.push(Selection::from_range(start.coord(), buffer.char_prev(it.coord())));
        }
        Ok(())
    })
}

pub fn regex_select_nested(
    context: &Context,
    opening: &Regex,
    closing: &Regex,
    count: i32,
    flags: ObjectFlags,
) -> Result<Vec<Selection>> {
    debug_assert!(opening != closing);
    let buffer = context.buffer();
    let inner = flags.has(ObjectFlags::Inner);

    for_each_sel(context, |res, beg, end| {
        let lines = beg.lines();
        let (buf_begin, buf_end) = (buffer_begin(lines), buffer_end(lines));
        // [DEVIATION] urgent event errors abort the search and are returned by check()
        let (mut opening_urgent, mut closing_urgent) = (UrgentEventsIdle::default(), UrgentEventsIdle::default());
        let (mut opening_idle, mut closing_idle) = (|| opening_urgent.idle(), || closing_urgent.idle());
        let mut opening_matches = RegexIterator::<BufferIterator, ForwardSearch>::new(
            beg,
            end,
            buf_begin,
            buf_end,
            opening,
            RegexExecFlags::NONE,
        )
        .with_idle(&mut opening_idle);
        let mut closing_matches = RegexIterator::<BufferIterator, ForwardSearch>::new(
            beg,
            end,
            buf_begin,
            buf_end,
            closing,
            RegexExecFlags::NONE,
        )
        .with_idle(&mut closing_idle);
        let mut opening_it = opening_matches.next_match().map(main_capture);
        let mut closing_it = closing_matches.next_match().map(main_capture);

        let mut start: Option<BufferIterator> = None;
        let mut level = -count - 1;
        // [FAITHFUL] hangs when an opening and a closing match start at the
        // same position, as neither inner loop advances then
        while opening_it.is_some() && closing_it.is_some() {
            while let Some(op) = &opening_it
                && closing_it.as_ref().is_none_or(|cl| op.0 < cl.0)
            {
                level += 1;
                if level == 0 {
                    start = Some(if inner { op.1 } else { op.0 });
                }
                opening_it = opening_matches.next_match().map(main_capture);
            }
            while let Some(cl) = &closing_it
                && opening_it.as_ref().is_none_or(|op| cl.0 < op.0)
            {
                let old_level = level;
                level -= 1;
                if old_level == 0
                    && let Some(s) = start
                {
                    let end_coord = buffer.char_prev(if inner { cl.0 } else { cl.1 }.coord());
                    if s.coord() <= end_coord {
                        res.push(Selection::from_range(s.coord(), end_coord));
                    }
                    start = None;
                }
                closing_it = closing_matches.next_match().map(main_capture);
            }
        }
        // [DEVIATION] An urgent event error interrupts the searches, which end
        // the loops, and is returned here (the C++ unwinds with an exception)
        opening_urgent.check()?;
        closing_urgent.check()?;
        if let Some(s) = start {
            res.push(Selection::from_range(s.coord(), buffer.char_prev(end.coord())));
        }
        Ok(())
    })
}

/// C++ regex_select_nested(context, delimiter, flags) overload
pub fn regex_select_nested_delimiter(
    context: &Context,
    delimiter: &Regex,
    flags: ObjectFlags,
) -> Result<Vec<Selection>> {
    let buffer = context.buffer();
    let inner = flags.has(ObjectFlags::Inner);

    for_each_sel(context, |res, beg, end| {
        let lines = beg.lines();
        let mut start: Option<BufferIterator> = None;
        // [DEVIATION] urgent event errors abort the search and are returned by check()
        let mut urgent = UrgentEventsIdle::default();
        let mut idle_func = || urgent.idle();
        let mut matches = RegexIterator::<BufferIterator, ForwardSearch>::new(
            beg,
            end,
            buffer_begin(lines),
            buffer_end(lines),
            delimiter,
            RegexExecFlags::NONE,
        )
        .with_idle(&mut idle_func);
        while let Some(m) = matches.next_match() {
            let (first, second) = main_capture(m);
            match start {
                None => start = Some(if inner { second } else { first }),
                Some(s) => {
                    let end_coord = buffer.char_prev(if inner { first } else { second }.coord());
                    if s.coord() <= end_coord {
                        res.push(Selection::from_range(s.coord(), end_coord));
                    }
                    start = None;
                }
            }
        }
        urgent.check()?;
        if let Some(s) = start {
            res.push(Selection::from_range(s.coord(), buffer.char_prev(end.coord())));
        }
        Ok(())
    })
}

pub fn select_lines(context: &Context, selection: &Selection) -> Option<Selection> {
    let buffer = context.buffer();
    let mut anchor = selection.anchor();
    let mut cursor = selection.cursor().coord;
    let (to_line_start, to_line_end) =
        if anchor <= cursor { (&mut anchor, &mut cursor) } else { (&mut cursor, &mut anchor) };

    to_line_start.column = 0;
    to_line_end.column = buffer.line_length(to_line_end.line) - 1;

    Some(Selection::from_range(anchor, BufferCoordAndTarget::new(cursor, MAX_COLUMN, -1)))
}

pub fn trim_partial_lines(context: &Context, selection: &Selection) -> Option<Selection> {
    let buffer = context.buffer();
    let mut anchor = selection.anchor();
    let mut cursor = selection.cursor().coord;
    let (to_line_start, to_line_end) =
        if anchor <= cursor { (&mut anchor, &mut cursor) } else { (&mut cursor, &mut anchor) };

    if to_line_start.column != 0 {
        *to_line_start = BufferCoord::line(to_line_start.line + 1);
    }
    if to_line_end.column != buffer.line_length(to_line_end.line) - 1 {
        if to_line_end.line == 0 {
            return None;
        }

        let prev_line = to_line_end.line - 1;
        *to_line_end = BufferCoord::new(prev_line, buffer.line_length(prev_line) - 1);
    }

    if *to_line_start > *to_line_end {
        return None;
    }

    Some(Selection::from_range(anchor, BufferCoordAndTarget::new(cursor, MAX_COLUMN, -1)))
}

fn buffer_match_flags(buf: &Buffer, begin: &BufferIterator, end: &BufferIterator) -> RegexExecFlags {
    match_flags(
        is_bol(begin.coord()),
        crate::buffer_utils::is_eol(buf, end.coord()),
        is_bow(buf, begin.coord()),
        is_eow(buf, end.coord()),
    )
}

fn find_next<'a>(
    buffer: &Buffer,
    lines: &'a [StringDataPtr],
    pos: BufferIterator<'a>,
    matches: &mut MatchResults<BufferIterator<'a>>,
    ex: &Regex,
    wrapped: &mut bool,
) -> Result<bool> {
    let (begin, end) = (buffer_begin(lines), buffer_end(lines));
    // [DEVIATION] urgent event errors abort the search and are returned by check()
    let mut idle = UrgentEventsIdle::default();
    if pos != end {
        let found =
            regex_search_captures(pos, end, begin, end, matches, ex, buffer_match_flags(buffer, &pos, &end), || {
                idle.idle()
            });
        idle.check()?;
        if found {
            return Ok(true);
        }
    }
    *wrapped = true;
    let found =
        regex_search_captures(begin, end, begin, end, matches, ex, buffer_match_flags(buffer, &begin, &end), || {
            idle.idle()
        });
    idle.check()?;
    Ok(found)
}

fn find_prev<'a>(
    buffer: &Buffer,
    lines: &'a [StringDataPtr],
    pos: BufferIterator<'a>,
    matches: &mut MatchResults<BufferIterator<'a>>,
    ex: &Regex,
    wrapped: &mut bool,
) -> Result<bool> {
    let (begin, end) = (buffer_begin(lines), buffer_end(lines));
    // [DEVIATION] urgent event errors abort the search and are returned by check()
    let mut idle = UrgentEventsIdle::default();
    if pos != begin {
        let found = backward_regex_search(
            begin,
            pos,
            begin,
            end,
            matches,
            ex,
            buffer_match_flags(buffer, &begin, &pos) | RegexExecFlags::NOT_INITIAL_NULL,
            || idle.idle(),
        );
        idle.check()?;
        if found {
            return Ok(true);
        }
    }
    *wrapped = true;
    let found = backward_regex_search(
        begin,
        end,
        begin,
        end,
        matches,
        ex,
        buffer_match_flags(buffer, &begin, &end) | RegexExecFlags::NOT_INITIAL_NULL,
        || idle.idle(),
    );
    idle.check()?;
    Ok(found)
}

/// mode is a RegexMode direction (RegexMode::FORWARD or RegexMode::BACKWARD)
pub fn find_next_match(
    context: &Context,
    sel: &Selection,
    regex: &Regex,
    mode: u8,
    wrapped: &mut bool,
) -> Result<Selection> {
    debug_assert!(is_direction(mode));
    let forward = mode & RegexMode::FORWARD != 0;
    let buffer = context.buffer();
    let lines = buffer.lines();
    let buf_end = buffer_end(&lines);
    let mut matches = MatchResults::default();
    let pos = BufferIterator::new(&lines, if forward { sel.max() } else { sel.min() });
    *wrapped = false;
    let found = if forward {
        find_next(&buffer, &lines, it_next(&pos, &buf_end), &mut matches, regex, wrapped)?
    } else {
        find_prev(&buffer, &lines, pos, &mut matches, regex, wrapped)?
    };

    if !found || matches[0].first == Some(buf_end) {
        return Err(rt_err!("no matches found: '{}'", regex.str()));
    }

    let captures: CaptureList = matches.iter().map(|m| submatch_string(&buffer, &m.first, &m.second)).collect();

    let (mut begin, mut end) = main_capture(&matches);
    end = if begin == end { end } else { it_previous(&end, &begin) };
    if !forward {
        std::mem::swap(&mut begin, &mut end);
    }

    Ok(Selection::with_captures(begin.coord(), end.coord(), captures))
}

/// buffer.string(submatch.first.coord(), submatch.second.coord()), non
/// participating submatches have default iterators, giving an empty string
fn submatch_string(buffer: &Buffer, first: &Option<BufferIterator>, second: &Option<BufferIterator>) -> Vec<u8> {
    match (first, second) {
        (Some(first), Some(second)) => buffer.string(first.coord(), second.coord()),
        _ => Vec::new(),
    }
}

pub fn select_matches(
    buffer: &Buffer,
    selections: &[Selection],
    regex: &Regex,
    capture_idx: i32,
) -> Result<Vec<Selection>> {
    let mark_count = regex.mark_count() as i32;
    if capture_idx < 0 || capture_idx > mark_count {
        return Err(Error::runtime("invalid capture number"));
    }

    let mut result = Vec::new();
    let lines = buffer.lines();
    let buf_end = buffer_end(&lines);
    let mut vm = ThreadedRegexVM::<BufferIterator, ForwardSearch>::new(regex.impl_());
    for sel in selections {
        let sel_beg = BufferIterator::new(&lines, sel.min());
        let sel_end = it_next(&BufferIterator::new(&lines, sel.max()), &buf_end);

        // [DEVIATION] urgent event errors abort the search and are returned by check()
        let mut urgent = UrgentEventsIdle::default();
        let mut idle_func = || urgent.idle();
        let mut matches =
            RegexIterator::with_vm_range(sel_beg, sel_end, &mut vm, buffer_match_flags(buffer, &sel_beg, &sel_end))
                .with_idle(&mut idle_func);
        while let Some(m) = matches.next_match() {
            let capture = &m[capture_idx as usize];
            if !capture.matched || capture.first == Some(sel_end) {
                continue;
            }

            let mut captures = CaptureList::with_capacity(mark_count as usize);
            for submatch in m {
                captures.push(submatch_string(buffer, &submatch.first, &submatch.second));
            }

            let (begin, end) = (capture.first.unwrap(), capture.second.unwrap());
            result.push(keep_direction(
                Selection::with_captures(
                    begin.coord(),
                    if begin == end { end } else { it_previous(&end, &begin) }.coord(),
                    captures,
                ),
                sel,
            ));
        }
        urgent.check()?;
    }
    if result.is_empty() {
        return Err(Error::runtime("nothing selected"));
    }

    // [DEVIATION] stable sort, std::sort leaves the order of equal selections unspecified
    result.sort_by(|lhs, rhs| {
        if compare_selections(lhs, rhs) {
            std::cmp::Ordering::Less
        } else if compare_selections(rhs, lhs) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    Ok(result)
}

pub fn split_on_matches(
    buffer: &Buffer,
    selections: &[Selection],
    regex: &Regex,
    capture_idx: i32,
) -> Result<Vec<Selection>> {
    if capture_idx < 0 || capture_idx > regex.mark_count() as i32 {
        return Err(Error::runtime("invalid capture number"));
    }

    let mut result = Vec::new();
    let lines = buffer.lines();
    let buf_end = buffer_end(&lines);
    let mut vm = ThreadedRegexVM::<BufferIterator, ForwardSearch>::new(regex.impl_());
    for sel in selections {
        let sel_begin = BufferIterator::new(&lines, sel.min());
        let mut begin = sel_begin;
        let sel_end = it_next(&BufferIterator::new(&lines, sel.max()), &buf_end);

        // No idle function, as in the C++
        let mut matches =
            RegexIterator::with_vm_range(begin, sel_end, &mut vm, buffer_match_flags(buffer, &begin, &sel_end));
        while let Some(m) = matches.next_match() {
            let capture = &m[capture_idx as usize];
            // [DEVIATION] A non participating capture has default constructed
            // iterators in the C++ code, which leads to undefined behaviour, skip it.
            let (Some(end), Some(capture_end)) = (capture.first, capture.second) else { continue };
            if end == buf_end {
                continue;
            }

            if end != sel_begin {
                let sel_end = if begin == end { end } else { it_previous(&end, &begin) };
                result.push(keep_direction(Selection::from_range(begin.coord(), sel_end.coord()), sel));
            }
            begin = capture_end;
        }
        if begin.coord() <= sel.max() {
            result.push(keep_direction(Selection::from_range(begin.coord(), sel.max()), sel));
        }
    }
    if result.is_empty() {
        return Err(Error::runtime("nothing selected"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regex_vm::RegexCompileFlags;
    use crate::utf8::SliceIt;

    fn find_surrounding_str<'a>(
        s: &'a [u8],
        pos: usize,
        opening: &Regex,
        closing: &Regex,
        flags: ObjectFlags,
        level: i32,
    ) -> Option<(SliceIt<'a>, SliceIt<'a>)> {
        let (begin, end) = (SliceIt::new(s, 0), SliceIt::new(s, s.len()));
        find_surrounding(&begin, &end, SliceIt::new(s, pos), opening, closing, flags, level).unwrap()
    }

    #[test]
    fn test_find_surrounding() {
        let re = |r: &[u8]| Regex::new(&[b"\\Q", r].concat(), RegexCompileFlags::BACKWARD).unwrap();
        let check_equal = |s: &[u8], pos: usize, opening: &[u8], closing: &[u8], flags, level, expected: &[u8]| {
            let res = find_surrounding_str(s, pos, &re(opening), &re(closing), flags, level);
            assert_eq!(res.is_none(), expected.is_empty());
            let Some((first, second)) = res else { return };
            let (min, max) = (first.pos.min(second.pos), first.pos.max(second.pos));
            assert_eq!(&s[min..max + 1], expected);
        };

        let both = ObjectFlags::ToBegin | ObjectFlags::ToEnd;
        let inner = both | ObjectFlags::Inner;
        let s = b"{foo [bar { baz[] }]}";
        check_equal(s, 13, b"{", b"}", both, 0, b"{ baz[] }");
        check_equal(s, 13, b"[", b"]", inner, 0, b"bar { baz[] }");
        check_equal(s, 5, b"[", b"]", both, 0, b"[bar { baz[] }]");
        check_equal(s, 10, b"{", b"}", both, 0, b"{ baz[] }");
        check_equal(s, 16, b"[", b"]", inner, 0, b"");
        check_equal(s, 18, b"[", b"]", both, 0, b"[bar { baz[] }]");
        check_equal(s, 6, b"[", b"]", ObjectFlags::ToBegin, 0, b"[b");

        let s = b"[*][] foo";
        assert!(find_surrounding_str(s, 6, &re(b"["), &re(b"]"), ObjectFlags::ToBegin, 0).is_none());

        let s = b"begin foo begin bar end end";
        check_equal(s, 6, b"begin", b"end", both, 0, s);
        check_equal(s, 22, b"begin", b"end", both, 0, b"begin bar end");
    }
}
