//! Port of selection.hh/cc

use crate::buffer::{Buffer, Change, ChangeType};
use crate::buffer_utils::{column_length_line, get_byte_to_column, get_column};
use crate::changes::{
    backward_sorted_until, forward_sorted_until, update_backward, update_forward, update_ranges, ForwardChangesTracker,
    UpdatableRange,
};
use crate::exception::{Error, Result};
use crate::string_utils::{byte_count_to_char, char_length, join, str_to_int};
use crate::units::{BufferCoord, BufferCoordAndTarget, BufferRange, ColumnCount, DisplayCoord};
use std::rc::Rc;

pub type CaptureList = Vec<Vec<u8>>;

pub const MAX_COLUMN: ColumnCount = i32::MAX;
pub const MAX_NON_EOL_COLUMN: ColumnCount = MAX_COLUMN - 1;

pub const fn max_column() -> ColumnCount {
    MAX_COLUMN
}

/// Port of BasicSelection
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BasicSelection {
    anchor: BufferCoord,
    cursor: BufferCoordAndTarget,
}

impl BasicSelection {
    pub fn new(anchor: BufferCoord, cursor: impl Into<BufferCoordAndTarget>) -> Self {
        BasicSelection { anchor, cursor: cursor.into() }
    }
    pub fn anchor(&self) -> BufferCoord {
        self.anchor
    }
    pub fn cursor(&self) -> BufferCoordAndTarget {
        self.cursor
    }
    pub fn min(&self) -> BufferCoord {
        if self.anchor <= self.cursor.coord { self.anchor } else { self.cursor.coord }
    }
    pub fn max(&self) -> BufferCoord {
        if self.anchor <= self.cursor.coord { self.cursor.coord } else { self.anchor }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    anchor: BufferCoord,
    cursor: BufferCoordAndTarget,
    captures: CaptureList,
}

impl Selection {
    pub fn new(pos: impl Into<BufferCoordAndTarget>) -> Self {
        let cursor = pos.into();
        Selection { anchor: cursor.coord, cursor, captures: Vec::new() }
    }
    pub fn from_range(anchor: BufferCoord, cursor: impl Into<BufferCoordAndTarget>) -> Self {
        Selection { anchor, cursor: cursor.into(), captures: Vec::new() }
    }
    pub fn with_captures(anchor: BufferCoord, cursor: impl Into<BufferCoordAndTarget>, captures: CaptureList) -> Self {
        Selection { anchor, cursor: cursor.into(), captures }
    }

    pub fn anchor(&self) -> BufferCoord {
        self.anchor
    }
    pub fn cursor(&self) -> BufferCoordAndTarget {
        self.cursor
    }
    pub fn anchor_mut(&mut self) -> &mut BufferCoord {
        &mut self.anchor
    }
    pub fn cursor_mut(&mut self) -> &mut BufferCoordAndTarget {
        &mut self.cursor
    }
    pub fn set_anchor(&mut self, anchor: BufferCoord) {
        self.anchor = anchor;
    }
    /// C++ `sel.cursor() = x`: assigning a BufferCoord resets the target
    pub fn set_cursor(&mut self, cursor: impl Into<BufferCoordAndTarget>) {
        self.cursor = cursor.into();
    }
    /// C++ set(anchor, cursor)
    pub fn set(&mut self, anchor: BufferCoord, cursor: BufferCoord) {
        self.anchor = anchor;
        self.cursor = cursor.into();
    }
    pub fn set_pos(&mut self, coord: BufferCoord) {
        self.set(coord, coord);
    }

    // When selections are single char, we want the anchor to be considered min, and cursor max
    pub fn min(&self) -> BufferCoord {
        if self.anchor <= self.cursor.coord { self.anchor } else { self.cursor.coord }
    }
    pub fn max(&self) -> BufferCoord {
        if self.anchor <= self.cursor.coord { self.cursor.coord } else { self.anchor }
    }
    /// C++ `sel.min() = x` (keeps cursor target)
    pub fn min_mut(&mut self) -> &mut BufferCoord {
        if self.anchor <= self.cursor.coord { &mut self.anchor } else { &mut self.cursor.coord }
    }
    /// C++ `sel.max() = x` (keeps cursor target)
    pub fn max_mut(&mut self) -> &mut BufferCoord {
        if self.anchor <= self.cursor.coord { &mut self.cursor.coord } else { &mut self.anchor }
    }
    /// references to (min, max), selected before modification
    pub fn min_max_mut(&mut self) -> (&mut BufferCoord, &mut BufferCoord) {
        if self.anchor <= self.cursor.coord {
            (&mut self.anchor, &mut self.cursor.coord)
        } else {
            (&mut self.cursor.coord, &mut self.anchor)
        }
    }

    pub fn captures(&self) -> &CaptureList {
        &self.captures
    }
    pub fn captures_mut(&mut self) -> &mut CaptureList {
        &mut self.captures
    }

    pub fn basic(&self) -> BasicSelection {
        BasicSelection { anchor: self.anchor, cursor: self.cursor }
    }
}

impl From<BasicSelection> for Selection {
    fn from(b: BasicSelection) -> Selection {
        Selection { anchor: b.anchor, cursor: b.cursor, captures: Vec::new() }
    }
}

impl UpdatableRange for Selection {
    fn first_last(&mut self) -> (&mut BufferCoord, &mut BufferCoord) {
        self.min_max_mut()
    }
    fn last(&self) -> BufferCoord {
        self.max()
    }
}

pub fn overlaps(lhs: &Selection, rhs: &Selection) -> bool {
    if lhs.min() <= rhs.min() { lhs.max() >= rhs.min() } else { lhs.min() <= rhs.max() }
}

pub fn compare_selections(lhs: &Selection, rhs: &Selection) -> bool {
    let (lmin, rmin) = (lhs.min(), rhs.min());
    if lmin == rmin { lhs.max() < rhs.max() } else { lmin < rmin }
}

fn cmp_selections(lhs: &Selection, rhs: &Selection) -> std::cmp::Ordering {
    if compare_selections(lhs, rhs) {
        std::cmp::Ordering::Less
    } else if compare_selections(rhs, lhs) {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
}

fn update_insert(coord: BufferCoord, begin: BufferCoord, end: BufferCoord) -> BufferCoord {
    let mut coord = coord;
    if coord < begin {
        return coord;
    }
    if begin.line == coord.line {
        coord.column += end.column - begin.column;
    }
    coord.line += end.line - begin.line;
    coord
}

/// Merge overlapping (per the predicate) sorted selections, returns the new length
fn merge_overlapping(
    sels: &mut Vec<Selection>,
    main: &mut usize,
    overlaps: impl Fn(&Selection, &Selection) -> bool,
) {
    if sels.is_empty() {
        return;
    }
    let size = sels.len();
    let mut i = 0;
    for j in 1..size {
        if overlaps(&sels[i], &sels[j]) {
            let min = sels[i].min().min(sels[j].min());
            let max = sels[i].max().max(sels[j].max());
            // C++ assigns min() then max(), each re-evaluating which field is which
            *sels[i].min_mut() = min;
            *sels[i].max_mut() = max;
            if i < *main {
                *main -= 1;
            }
        } else {
            i += 1;
            if i != j {
                sels[i] = std::mem::take(&mut sels[j]);
            }
        }
    }
    sels.truncate(i + 1);
}

pub fn compute_modified_ranges(buffer: &Buffer, timestamp: usize) -> Vec<Selection> {
    let mut ranges: Vec<Selection> = Vec::new();
    let changes = buffer.changes_since(timestamp);
    let mut change_it = 0;
    while change_it != changes.len() {
        let forward_end = forward_sorted_until(&changes, change_it);
        let backward_end = backward_sorted_until(&changes, change_it);

        let prev_size;
        let mut dummy = 0;
        if forward_end >= backward_end {
            update_forward(&changes[change_it..forward_end], &mut ranges);
            merge_overlapping(&mut ranges, &mut dummy, overlaps);
            prev_size = ranges.len();

            let mut changes_tracker = ForwardChangesTracker::default();
            while change_it != forward_end {
                let c = &changes[change_it];
                if c.type_ == ChangeType::Insert {
                    ranges.push(Selection::from_range(c.begin, c.end));
                } else {
                    ranges.push(Selection::new(c.begin));
                }
                changes_tracker.update(c);
                change_it += 1;
            }
        } else {
            update_backward(&changes[change_it..backward_end], &mut ranges);
            merge_overlapping(&mut ranges, &mut dummy, overlaps);
            prev_size = ranges.len();

            let mut changes_tracker = ForwardChangesTracker::default();
            for it in (change_it..backward_end).rev() {
                let c = &changes[it];
                let change =
                    Change { type_: c.type_, begin: changes_tracker.get_new_coord(c.begin), end: changes_tracker.get_new_coord(c.end) };
                if change.type_ == ChangeType::Insert {
                    ranges.push(Selection::from_range(change.begin, change.end));
                } else {
                    ranges.push(Selection::new(change.begin));
                }
                changes_tracker.update(&change);
            }
            change_it = backward_end;
        }

        // inplace_merge (stable merge of two sorted runs)
        let tail = ranges.split_off(prev_size);
        ranges = merge_sorted(ranges, tail);
        // The newly added ranges might be overlapping pre-existing ones
        merge_overlapping(&mut ranges, &mut dummy, overlaps);
    }

    let end_coord = buffer.end_coord();
    for range in &mut ranges {
        let a = range.anchor().min(end_coord);
        range.set_anchor(a);
        let c = range.cursor().coord.min(end_coord);
        range.set_cursor(c);
    }

    let touches = |lhs: &Selection, rhs: &Selection| lhs.max() == end_coord || buffer.char_next(lhs.max()) >= rhs.min();
    let mut dummy = 0;
    merge_overlapping(&mut ranges, &mut dummy, touches);

    for sel in &mut ranges {
        if buffer.is_end(sel.anchor()) {
            sel.set_anchor(buffer.back_coord());
        }
        if buffer.is_end(sel.cursor().coord) {
            sel.set_cursor(buffer.back_coord());
        }
        if sel.anchor() != sel.cursor().coord {
            let c = buffer.char_prev(sel.cursor().coord);
            sel.set_cursor(c);
        }
    }
    ranges
}

fn merge_sorted(a: Vec<Selection>, b: Vec<Selection>) -> Vec<Selection> {
    let mut res = Vec::with_capacity(a.len() + b.len());
    let mut ia = a.into_iter().peekable();
    let mut ib = b.into_iter().peekable();
    loop {
        match (ia.peek(), ib.peek()) {
            (Some(x), Some(y)) => {
                if compare_selections(y, x) {
                    res.push(ib.next().unwrap());
                } else {
                    res.push(ia.next().unwrap());
                }
            }
            (Some(_), None) => res.push(ia.next().unwrap()),
            (None, Some(_)) => res.push(ib.next().unwrap()),
            (None, None) => break,
        }
    }
    res
}

fn clamp_sel(sel: &mut Selection, buffer: &Buffer) {
    sel.anchor = buffer.clamp(sel.anchor);
    // C++: sel.cursor() = buffer.clamp(sel.cursor()), which resets the target
    sel.cursor = buffer.clamp(sel.cursor.coord).into();
}

pub fn clamp_selections(selections: &mut [Selection], buffer: &Buffer) {
    for sel in selections {
        clamp_sel(sel, buffer);
    }
}

pub fn update_selections(selections: &mut Vec<Selection>, main: &mut usize, buffer: &Buffer, timestamp: usize, merge: bool) {
    if timestamp == buffer.timestamp() {
        return;
    }
    update_ranges(buffer, timestamp, selections.as_mut_slice());
    for sel in selections.iter_mut() {
        clamp_sel(sel, buffer);
    }
    if merge {
        merge_overlapping(selections, main, overlaps);
    }
}

pub fn sort_selections(selections: &mut [Selection], main_index: &mut usize) {
    if selections.len() == 1 {
        return;
    }
    let main_begin = selections[*main_index].min();
    let main = *main_index;
    *main_index = selections
        .iter()
        .enumerate()
        .filter(|(i, sel)| {
            let begin = sel.min();
            if begin == main_begin { *i < main } else { begin < main_begin }
        })
        .count();
    selections.sort_by(cmp_selections);
}

pub fn merge_overlapping_selections(selections: &mut Vec<Selection>, main_index: &mut usize) {
    if selections.len() == 1 {
        return;
    }
    merge_overlapping(selections, main_index, overlaps);
}

fn fix_overflowing_selections(selections: &mut [Selection], buffer: &Buffer) {
    let back_coord = buffer.back_coord();
    for sel in selections {
        sel.cursor = buffer.clamp(sel.cursor.coord).min(back_coord).into();
        sel.anchor = buffer.clamp(sel.anchor).min(back_coord);
    }
}

fn any_overlaps(sels: &[Selection]) -> bool {
    sels.windows(2).any(|w| overlaps(&w[0], &w[1]))
}

pub fn replace_selection(buffer: &Buffer, sel: &mut Selection, content: &[u8]) -> Result<()> {
    // we want min and max from *before* we do any change
    let (min, max) = sel.min_max_mut();
    let range = buffer.replace(*min, buffer.char_next(*max), content)?;
    *min = range.begin;
    *max = if range.end > range.begin { buffer.char_prev(range.end) } else { range.begin };
    Ok(())
}

pub fn insert_at(buffer: &Buffer, sel: &mut Selection, pos: BufferCoord, content: &[u8]) -> Result<BufferRange> {
    let range = buffer.insert(pos, content)?;
    sel.anchor = buffer.clamp(update_insert(sel.anchor, range.begin, range.end));
    sel.cursor = buffer.clamp(update_insert(sel.cursor.coord, range.begin, range.end)).into();
    Ok(range)
}

#[derive(Clone, Debug)]
pub struct SelectionList {
    main: usize,
    selections: Vec<Selection>,
    buffer: Rc<Buffer>,
    timestamp: usize,
}

impl PartialEq for SelectionList {
    fn eq(&self, other: &SelectionList) -> bool {
        Rc::ptr_eq(&self.buffer, &other.buffer) && self.selections == other.selections
    }
}

impl SelectionList {
    pub fn new(buffer: Rc<Buffer>, s: Selection) -> Self {
        let timestamp = buffer.timestamp();
        Self::with_timestamp(buffer, s, timestamp)
    }

    pub fn with_timestamp(buffer: Rc<Buffer>, s: Selection, timestamp: usize) -> Self {
        SelectionList { main: 0, selections: vec![s], buffer, timestamp }
    }

    pub fn from_vec(buffer: Rc<Buffer>, list: Vec<Selection>) -> Self {
        let timestamp = buffer.timestamp();
        Self::from_vec_timestamp(buffer, list, timestamp)
    }

    pub fn from_vec_timestamp(buffer: Rc<Buffer>, list: Vec<Selection>, timestamp: usize) -> Self {
        assert!(!list.is_empty());
        let main = list.len() - 1;
        SelectionList { main, selections: list, buffer, timestamp }
    }

    pub fn update(&mut self, merge: bool) {
        let buffer = self.buffer.clone();
        update_selections(&mut self.selections, &mut self.main, &buffer, self.timestamp, merge);
        self.timestamp = buffer.timestamp();
    }

    pub fn main(&self) -> &Selection {
        &self.selections[self.main]
    }
    pub fn main_mut(&mut self) -> &mut Selection {
        &mut self.selections[self.main]
    }
    pub fn main_index(&self) -> usize {
        self.main
    }
    pub fn set_main_index(&mut self, main: usize) {
        debug_assert!(main < self.size());
        self.main = main;
    }

    pub fn push(&mut self, sel: Selection) {
        self.selections.push(sel);
    }

    pub fn set(&mut self, list: Vec<Selection>, main: usize) {
        debug_assert!(main < list.len());
        self.selections = list;
        self.main = main;
        self.timestamp = self.buffer.timestamp();
        self.sort();
    }

    /// C++ operator=(Vector<Selection>): main becomes the last selection
    pub fn assign(&mut self, list: Vec<Selection>) {
        let main = list.len() - 1;
        self.set(list, main);
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Selection> {
        self.selections.iter()
    }
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Selection> {
        self.selections.iter_mut()
    }
    pub fn as_slice(&self) -> &[Selection] {
        &self.selections
    }
    pub fn as_mut_slice(&mut self) -> &mut [Selection] {
        &mut self.selections
    }

    pub fn remove(&mut self, index: usize) {
        self.selections.remove(index);
        if index < self.main || self.main == self.selections.len() {
            self.main -= 1;
        }
    }

    pub fn remove_from(&mut self, index: usize) {
        debug_assert!(index > 0);
        self.selections.truncate(index);
        if index <= self.main {
            self.main = self.selections.len() - 1;
        }
    }

    pub fn size(&self) -> usize {
        self.selections.len()
    }
    pub fn len(&self) -> usize {
        self.selections.len()
    }
    pub fn is_empty(&self) -> bool {
        self.selections.is_empty()
    }

    pub fn sort(&mut self) {
        sort_selections(&mut self.selections, &mut self.main);
    }

    pub fn merge_overlapping(&mut self) {
        merge_overlapping_selections(&mut self.selections, &mut self.main);
    }

    pub fn merge_consecutive(&mut self) {
        if self.size() == 1 {
            return;
        }
        let buffer = self.buffer.clone();
        merge_overlapping(&mut self.selections, &mut self.main, |lhs, rhs| buffer.char_next(lhs.max()) >= rhs.min());
    }

    pub fn sort_and_merge_overlapping(&mut self) {
        self.sort();
        self.merge_overlapping();
    }

    pub fn buffer(&self) -> &Rc<Buffer> {
        &self.buffer
    }
    pub fn timestamp(&self) -> usize {
        self.timestamp
    }
    pub fn force_timestamp(&mut self, timestamp: usize) {
        self.timestamp = timestamp;
    }

    pub fn for_each(
        &mut self,
        mut func: impl FnMut(usize, &mut Selection) -> Result<()>,
        may_append: bool,
    ) -> Result<()> {
        self.update(true);
        let buffer = self.buffer.clone();
        let res = if may_append && any_overlaps(&self.selections) {
            let timestamp = buffer.timestamp();
            let mut res = Ok(());
            for index in 0..self.selections.len() {
                let sel = &mut self.selections[index];
                update_ranges(&buffer, timestamp, std::slice::from_mut(sel));
                res = func(index, sel);
                if res.is_err() {
                    break;
                }
            }
            res
        } else {
            let mut changes_tracker = ForwardChangesTracker::default();
            let mut res = Ok(());
            for index in 0..self.selections.len() {
                let sel = &mut self.selections[index];
                sel.anchor = changes_tracker.get_new_coord_tolerant(sel.anchor);
                sel.cursor = changes_tracker.get_new_coord_tolerant(sel.cursor.coord).into();
                res = func(index, sel);
                changes_tracker.update_from_buffer(&buffer, &mut self.timestamp);
                if res.is_err() {
                    break;
                }
            }
            res
        };
        // We might just have been deleting text if strings were empty,
        // in which case we could have some selections pushed out of the buffer
        fix_overflowing_selections(&mut self.selections, &buffer);
        res
    }

    pub fn replace(&mut self, strings: &[Vec<u8>]) -> Result<()> {
        if strings.is_empty() {
            return Ok(());
        }
        let buffer = self.buffer.clone();
        self.for_each(
            |index, sel| replace_selection(&buffer, sel, &strings[index.min(strings.len() - 1)]),
            false,
        )
    }

    pub fn erase(&mut self) -> Result<()> {
        self.update(true);
        self.merge_overlapping();
        let buffer = self.buffer.clone();
        let mut changes_tracker = ForwardChangesTracker::default();
        for sel in self.selections.iter_mut() {
            sel.anchor = changes_tracker.get_new_coord(sel.anchor);
            sel.cursor = changes_tracker.get_new_coord(sel.cursor.coord).into();
            let pos = buffer.erase(sel.min(), buffer.char_next(sel.max()))?;
            sel.anchor = pos;
            sel.cursor = pos.into();
            changes_tracker.update_from_buffer(&buffer, &mut self.timestamp);
        }
        fix_overflowing_selections(&mut self.selections, &buffer);
        Ok(())
    }
}

impl std::ops::Index<usize> for SelectionList {
    type Output = Selection;
    fn index(&self, i: usize) -> &Selection {
        &self.selections[i]
    }
}

impl std::ops::IndexMut<usize> for SelectionList {
    fn index_mut(&mut self, i: usize) -> &mut Selection {
        &mut self.selections[i]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColumnType {
    Byte,
    Codepoint,
    DisplayColumn,
}

pub fn selection_to_string(column_type: ColumnType, buffer: &Buffer, selection: &Selection, tabstop: ColumnCount) -> Vec<u8> {
    let cursor = selection.cursor().coord;
    let anchor = selection.anchor();
    match column_type {
        ColumnType::Byte => kformat!("{}.{},{}.{}", anchor.line + 1, anchor.column + 1, cursor.line + 1, cursor.column + 1),
        ColumnType::Codepoint => {
            let aline = buffer.line(anchor.line);
            let cline = buffer.line(cursor.line);
            kformat!(
                "{}.{},{}.{}",
                anchor.line + 1,
                char_length(&aline[..anchor.column as usize]) + 1,
                cursor.line + 1,
                char_length(&cline[..cursor.column as usize]) + 1
            )
        }
        ColumnType::DisplayColumn => kformat!(
            "{}.{},{}.{}",
            anchor.line + 1,
            get_column(buffer, tabstop, anchor) + 1,
            cursor.line + 1,
            get_column(buffer, tabstop, cursor) + 1
        ),
    }
}

pub fn selection_list_to_string(column_type: ColumnType, selections: &SelectionList, tabstop: ColumnCount) -> Vec<u8> {
    let buffer = selections.buffer();
    let main = selections.main_index();
    let sels = selections.as_slice();
    join(
        sels[main..].iter().chain(sels[..main].iter()).map(|s| selection_to_string(column_type, buffer, s, tabstop)),
        b' ',
        false,
    )
}

pub fn selection_from_string(column_type: ColumnType, buffer: &Buffer, desc: &[u8], tabstop: ColumnCount) -> Result<Selection> {
    let end = desc.len();
    let comma = desc.iter().position(|&c| c == b',').unwrap_or(end);
    let dot_anchor = desc[..comma].iter().position(|&c| c == b'.').unwrap_or(comma);
    let dot_cursor = desc[comma..].iter().position(|&c| c == b'.').map(|p| p + comma).unwrap_or(end);

    if comma == end || dot_anchor == comma || dot_cursor == end {
        return Err(rt_err!("'{}' does not follow <line>.<column>,<line>.<column> format", desc));
    }

    let compute_coord = |line: i32, column: i32| -> Result<BufferCoord> {
        let not_exist = || rt_err!("coordinate {}.{} does not exist in buffer", line + 1, column + 1);
        if line < 0 || column < 0 {
            return Err(not_exist());
        }
        match column_type {
            ColumnType::Byte => Ok(BufferCoord::new(line, column)),
            ColumnType::Codepoint => {
                if buffer.line_count() <= line || char_length(&buffer.line(line)) <= column {
                    return Err(not_exist());
                }
                Ok(BufferCoord::new(line, byte_count_to_char(&buffer.line(line), column) as i32))
            }
            ColumnType::DisplayColumn => {
                if buffer.line_count() <= line || column_length_line(buffer, tabstop, line) <= column {
                    return Err(not_exist());
                }
                Ok(BufferCoord::new(line, get_byte_to_column(buffer, tabstop, DisplayCoord::new(line, column))))
            }
        }
    };

    let anchor = compute_coord(
        str_to_int(&desc[..dot_anchor])? - 1,
        str_to_int(&desc[dot_anchor + 1..comma])? - 1,
    )?;
    let cursor = compute_coord(str_to_int(&desc[comma + 1..dot_cursor])? - 1, str_to_int(&desc[dot_cursor + 1..])? - 1)?;
    Ok(Selection::from_range(anchor, cursor))
}

pub fn selection_list_from_strings(
    buffer: &Rc<Buffer>,
    column_type: ColumnType,
    descs: &[Vec<u8>],
    timestamp: usize,
    mut main: usize,
    tabstop: ColumnCount,
) -> Result<SelectionList> {
    if (column_type != ColumnType::Byte && timestamp != buffer.timestamp()) || timestamp > buffer.timestamp() {
        return Err(rt_err!("invalid timestamp '{}'", timestamp));
    }
    let mut sels = Vec::with_capacity(descs.len());
    for desc in descs {
        sels.push(selection_from_string(column_type, buffer, desc, tabstop)?);
    }
    if sels.is_empty() {
        return Err(Error::runtime("empty selection description"));
    }
    if main >= sels.len() {
        return Err(Error::runtime("invalid main selection index"));
    }
    sort_selections(&mut sels, &mut main);
    merge_overlapping_selections(&mut sels, &mut main);
    if timestamp < buffer.timestamp() {
        update_selections(&mut sels, &mut main, buffer, timestamp, true);
    } else {
        clamp_selections(&mut sels, buffer);
    }
    let mut res = SelectionList::from_vec(buffer.clone(), sels);
    res.set_main_index(main);
    Ok(res)
}
