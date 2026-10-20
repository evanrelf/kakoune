//! Port of changes.hh/cc

use crate::buffer::{Buffer, Change, ChangeType};
use crate::units::BufferCoord;

/// This tracks position changes for changes that are done
/// in a forward way (each change takes place at a position)
/// *after* the previous one.
#[derive(Clone, Copy, Debug, Default)]
pub struct ForwardChangesTracker {
    /// last change position at current modification
    pub cur_pos: BufferCoord,
    /// last change position at start
    pub old_pos: BufferCoord,
}

impl ForwardChangesTracker {
    pub fn update(&mut self, change: &Change) {
        debug_assert!(change.begin >= self.cur_pos);
        match change.type_ {
            ChangeType::Insert => {
                self.old_pos = self.get_old_coord(change.begin);
                self.cur_pos = change.end;
            }
            ChangeType::Erase => {
                self.old_pos = self.get_old_coord(change.end);
                self.cur_pos = change.begin;
            }
        }
    }

    pub fn update_from_buffer(&mut self, buffer: &Buffer, timestamp: &mut usize) {
        buffer.with_changes_since(*timestamp, |changes| {
            for change in changes {
                self.update(change);
            }
        });
        *timestamp = buffer.timestamp();
    }

    pub fn get_old_coord(&self, mut coord: BufferCoord) -> BufferCoord {
        let pos_change = self.cur_pos - self.old_pos;
        if self.cur_pos.line == coord.line {
            coord.column -= pos_change.column;
        }
        coord.line -= pos_change.line;
        coord
    }

    pub fn get_new_coord(&self, mut coord: BufferCoord) -> BufferCoord {
        let pos_change = self.cur_pos - self.old_pos;
        if self.old_pos.line == coord.line {
            coord.column += pos_change.column;
        }
        coord.line += pos_change.line;
        coord
    }

    pub fn get_new_coord_tolerant(&self, coord: BufferCoord) -> BufferCoord {
        if coord < self.old_pos {
            return self.cur_pos;
        }
        self.get_new_coord(coord)
    }

    pub fn relevant(&self, change: &Change, old_coord: BufferCoord) -> bool {
        let new_coord = self.get_new_coord_tolerant(old_coord);
        match change.type_ {
            ChangeType::Insert => change.begin <= new_coord,
            ChangeType::Erase => change.begin < new_coord,
        }
    }
}

/// returns the index of the first change that breaks forward ordering
pub fn forward_sorted_until(changes: &[Change], first: usize) -> usize {
    let last = changes.len();
    if first != last {
        let mut first = first;
        let mut next = first + 1;
        while next != last {
            let c = &changes[first];
            let reference = if c.type_ == ChangeType::Insert { c.end } else { c.begin };
            if changes[next].begin < reference {
                return next;
            }
            first = next;
            next += 1;
        }
    }
    last
}

/// returns the index of the first change that breaks backward ordering
pub fn backward_sorted_until(changes: &[Change], first: usize) -> usize {
    let last = changes.len();
    if first != last {
        let mut first = first;
        let mut next = first + 1;
        while next != last {
            if changes[first].begin < changes[next].end {
                return next;
            }
            first = next;
            next += 1;
        }
    }
    last
}

/// A range whose bounds can be updated through buffer changes
/// (C++ get_first/get_last customization points)
pub trait UpdatableRange {
    /// mutable references to the first and last coords, chosen before any
    /// modification happens
    fn first_last(&mut self) -> (&mut BufferCoord, &mut BufferCoord);
    fn last(&self) -> BufferCoord;
}

fn update_range<R: UpdatableRange>(
    changes_tracker: &mut ForwardChangesTracker,
    range: &mut R,
    advance_while_relevant: &mut dyn FnMut(&mut ForwardChangesTracker, BufferCoord),
) {
    let (first, last) = range.first_last();
    advance_while_relevant(changes_tracker, *first);
    *first = changes_tracker.get_new_coord_tolerant(*first);
    if *last < BufferCoord::new(0, 0) {
        return;
    }
    advance_while_relevant(changes_tracker, *last);
    *last = changes_tracker.get_new_coord_tolerant(*last);
}

pub fn update_forward<R: UpdatableRange>(changes: &[Change], ranges: &mut [R]) {
    let mut changes_tracker = ForwardChangesTracker::default();
    let mut it = 0usize;
    let mut advance_while_relevant = |tracker: &mut ForwardChangesTracker, pos: BufferCoord| {
        while it != changes.len() && tracker.relevant(&changes[it], pos) {
            tracker.update(&changes[it]);
            it += 1;
        }
    };
    let front_begin = changes[0].begin;
    let start = ranges.partition_point(|r| r.last() < front_begin);
    for range in &mut ranges[start..] {
        update_range(&mut changes_tracker, range, &mut advance_while_relevant);
    }
}

pub fn update_backward<R: UpdatableRange>(changes: &[Change], ranges: &mut [R]) {
    let mut changes_tracker = ForwardChangesTracker::default();
    let mut it = changes.len();
    let mut advance_while_relevant = |tracker: &mut ForwardChangesTracker, pos: BufferCoord| {
        while it != 0 {
            let c = &changes[it - 1];
            let change = Change { type_: c.type_, begin: tracker.get_new_coord(c.begin), end: tracker.get_new_coord(c.end) };
            if !tracker.relevant(&change, pos) {
                break;
            }
            tracker.update(&change);
            it -= 1;
        }
    };
    for range in ranges.iter_mut() {
        update_range(&mut changes_tracker, range, &mut advance_while_relevant);
    }
}

/// Apply a list of changes (as returned by changes_since) to sorted ranges
pub fn update_ranges_with_changes<R: UpdatableRange>(changes: &[Change], ranges: &mut [R]) {
    let mut change_it = 0;
    while change_it != changes.len() {
        let forward_end = forward_sorted_until(changes, change_it);
        let backward_end = backward_sorted_until(changes, change_it);
        if forward_end >= backward_end {
            update_forward(&changes[change_it..forward_end], ranges);
            change_it = forward_end;
        } else {
            update_backward(&changes[change_it..backward_end], ranges);
            change_it = backward_end;
        }
    }
}

pub fn update_ranges<R: UpdatableRange>(buffer: &Buffer, timestamp: usize, ranges: &mut [R]) {
    if timestamp == buffer.timestamp() {
        return;
    }
    buffer.with_changes_since(timestamp, |changes| update_ranges_with_changes(changes, ranges));
}
