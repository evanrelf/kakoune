//! Port of line_modification.hh/cc

use crate::buffer::{Buffer, Change, ChangeType};
use crate::units::{LineCount, Range};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LineModification {
    /// line position in the old buffer
    pub old_line: LineCount,
    /// new line position
    pub new_line: LineCount,
    /// number of lines removed (including this one)
    pub num_removed: LineCount,
    /// number of lines added (including this one)
    pub num_added: LineCount,
}

impl LineModification {
    pub const fn new(old_line: LineCount, new_line: LineCount, num_removed: LineCount, num_added: LineCount) -> Self {
        LineModification { old_line, new_line, num_removed, num_added }
    }
    pub fn diff(&self) -> LineCount {
        self.new_line - self.old_line + self.num_added - self.num_removed
    }
}

fn make_line_modif(change: &Change) -> LineModification {
    let mut num_added = 0;
    let mut num_removed = 0;
    if change.type_ == ChangeType::Insert {
        num_added = change.end.line - change.begin.line;
    } else {
        num_removed = change.end.line - change.begin.line;
    }
    // modified a line
    if change.begin.column != 0 || change.end.column != 0 {
        num_removed += 1;
        num_added += 1;
    }
    LineModification::new(change.begin.line, change.begin.line, num_removed, num_added)
}

pub fn compute_line_modifications(buffer: &Buffer, timestamp: usize) -> Vec<LineModification> {
    buffer.with_changes_since(timestamp, compute_line_modifications_from)
}

pub fn compute_line_modifications_from(changes: &[Change]) -> Vec<LineModification> {
    let mut res: Vec<LineModification> = Vec::new();
    for buf_change in changes {
        let mut change = make_line_modif(buf_change);
        let mut pos = res.partition_point(|c| c.new_line <= change.new_line);
        if pos != 0 {
            let prev = res[pos - 1];
            if change.new_line <= prev.new_line + prev.num_added {
                pos -= 1;
                let p = &mut res[pos];
                let removed_from_previously_added_by_pos =
                    (p.new_line + p.num_added - change.new_line).clamp(0, p.num_added.min(change.num_removed));
                p.num_removed += change.num_removed - removed_from_previously_added_by_pos;
                p.num_added += change.num_added - removed_from_previously_added_by_pos;
            } else {
                change.old_line -= prev.diff();
                res.insert(pos, change);
            }
        } else {
            res.insert(pos, change);
        }

        let mut next = pos + 1;
        let diff = buf_change.end.line - buf_change.begin.line;
        if buf_change.type_ == ChangeType::Erase {
            let limit = change.new_line + change.num_removed;
            let delend = next + res[next..].partition_point(|c| c.new_line <= limit);
            for i in next..delend {
                let it = res[i];
                let removed_from_previously_added_by_it = it.num_added.min(limit - it.new_line);
                res[pos].num_removed += it.num_removed - removed_from_previously_added_by_it;
                res[pos].num_added += it.num_added - removed_from_previously_added_by_it;
            }
            res.drain(next..delend);
            if diff != 0 {
                for it in &mut res[next..] {
                    it.new_line -= diff;
                }
            }
        } else if diff != 0 {
            next = pos + 1;
            for it in &mut res[next..] {
                it.new_line += diff;
            }
        }
    }
    res
}

pub type LineRange = Range<LineCount>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LineRangeSet {
    ranges: Vec<LineRange>,
}

impl LineRangeSet {
    pub fn view(&self) -> &[LineRange] {
        &self.ranges
    }

    pub fn reset(&mut self, range: LineRange) {
        self.ranges = vec![range];
    }

    pub fn update(&mut self, modifs: &[LineModification]) {
        if modifs.is_empty() {
            return;
        }

        let mut i = 0;
        while i < self.ranges.len() {
            let it_begin = self.ranges[i].begin;
            let it_end = self.ranges[i].end;
            let mut modif_beg = modifs.partition_point(|c| c.old_line + c.num_removed < it_begin);
            let modif_end = modifs.partition_point(|c| c.old_line <= it_end);

            if modif_beg == modifs.len() {
                let diff = modifs[modif_beg - 1].diff();
                self.ranges[i].begin += diff;
                self.ranges[i].end += diff;
                i += 1;
                continue;
            }

            let diff = modifs[modif_beg].new_line - modifs[modif_beg].old_line;
            self.ranges[i].begin += diff;
            self.ranges[i].end += diff;

            while modif_beg < modif_end {
                let m = modifs[modif_beg];
                modif_beg += 1;
                if m.num_removed > 0 {
                    let it = &mut self.ranges[i];
                    if m.new_line < it.begin {
                        it.begin = m.new_line.max(it.begin - m.num_removed);
                    }
                    it.end = m.new_line.max(it.begin.max(it.end - m.num_removed));
                }
                if m.num_added > 0 {
                    if self.ranges[i].begin >= m.new_line {
                        self.ranges[i].begin += m.num_added;
                    } else {
                        let b = self.ranges[i].begin;
                        self.ranges.insert(i, LineRange::new(b, m.new_line));
                        i += 1;
                        self.ranges[i].begin = m.new_line + m.num_added;
                    }
                    self.ranges[i].end += m.num_added;
                }
            }
            i += 1;
        }
        self.ranges.retain(|r| r.begin < r.end);
    }

    pub fn add_range(&mut self, mut range: LineRange, on_new_range: &mut dyn FnMut(LineRange)) {
        let mut insert_at = self.ranges.partition_point(|r| r.end < range.begin);
        if insert_at == self.ranges.len() || self.ranges[insert_at].begin > range.end {
            on_new_range(range);
        } else {
            let mut pos = range.begin;
            let mut it = insert_at;
            while it != self.ranges.len() && self.ranges[it].begin <= range.end {
                let r = self.ranges[it];
                if pos < r.begin {
                    on_new_range(LineRange::new(pos, r.begin));
                }
                range = LineRange::new(range.begin.min(r.begin), range.end.max(r.end));
                pos = r.end;
                it += 1;
            }
            self.ranges.drain(insert_at..it);
            if pos < range.end {
                on_new_range(LineRange::new(pos, range.end));
            }
        }
        insert_at = insert_at.min(self.ranges.len());
        self.ranges.insert(insert_at, range);
    }

    pub fn remove_range(&mut self, range: LineRange) {
        let inside = |line: LineCount, range: LineRange| range.begin <= line && line < range.end;
        let mut it = self.ranges.partition_point(|r| r.end < range.begin);
        if it == self.ranges.len() || self.ranges[it].begin > range.end {
            return;
        }
        while it != self.ranges.len() && self.ranges[it].begin <= range.end {
            let r = self.ranges[it];
            if r.begin < range.begin && range.end <= r.end {
                self.ranges.insert(it, LineRange::new(r.begin, range.begin));
                it += 1;
                self.ranges[it].begin = range.end;
            }
            let r = &mut self.ranges[it];
            if inside(r.begin, range) {
                r.begin = range.end;
            }
            if inside(r.end, range) {
                r.end = range.begin;
            }
            if r.end <= r.begin {
                self.ranges.remove(it);
            } else {
                it += 1;
            }
        }
    }
}

impl std::ops::Index<usize> for LineRangeSet {
    type Output = LineRange;
    fn index(&self, i: usize) -> &LineRange {
        &self.ranges[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(b: i32, e: i32) -> LineRange {
        LineRange::new(b, e)
    }

    fn expect(ranges: Vec<LineRange>) -> impl FnMut(LineRange) {
        let mut it = ranges.into_iter();
        move |range| assert_eq!(Some(range), it.next())
    }

    #[test]
    fn test_line_range_set() {
        {
            let mut ranges = LineRangeSet::default();
            ranges.add_range(r(0, 5), &mut expect(vec![r(0, 5)]));
            ranges.add_range(r(10, 15), &mut expect(vec![r(10, 15)]));
            ranges.add_range(r(5, 10), &mut expect(vec![r(5, 10)]));
            assert_eq!(ranges.view(), &[r(0, 15)]);
            ranges.add_range(r(5, 10), &mut expect(vec![]));
            ranges.remove_range(r(3, 8));
            assert_eq!(ranges.view(), &[r(0, 3), r(8, 15)]);
        }
        {
            let mut ranges = LineRangeSet::default();
            ranges.add_range(r(0, 7), &mut expect(vec![r(0, 7)]));
            ranges.add_range(r(9, 15), &mut expect(vec![r(9, 15)]));
            ranges.add_range(r(5, 10), &mut expect(vec![r(7, 9)]));
            assert_eq!(ranges.view(), &[r(0, 15)]);
        }
        {
            let mut ranges = LineRangeSet::default();
            ranges.add_range(r(0, 7), &mut expect(vec![r(0, 7)]));
            ranges.add_range(r(11, 15), &mut expect(vec![r(11, 15)]));
            ranges.add_range(r(5, 10), &mut expect(vec![r(7, 10)]));
            assert_eq!(ranges.view(), &[r(0, 10), r(11, 15)]);
            ranges.remove_range(r(8, 13));
            assert_eq!(ranges.view(), &[r(0, 8), r(13, 15)]);
        }
        {
            let mut ranges = LineRangeSet::default();
            ranges.add_range(r(0, 5), &mut expect(vec![r(0, 5)]));
            ranges.add_range(r(10, 15), &mut expect(vec![r(10, 15)]));
            ranges.update(&[LineModification::new(3, 3, 3, 1), LineModification::new(11, 9, 2, 4)]);
            assert_eq!(ranges.view(), &[r(0, 3), r(8, 9), r(13, 15)]);
        }
        {
            let mut ranges = LineRangeSet::default();
            ranges.add_range(r(0, 5), &mut expect(vec![r(0, 5)]));
            ranges.update(&[LineModification::new(2, 2, 2, 0)]);
            assert_eq!(ranges.view(), &[r(0, 3)]);
        }
        {
            let mut ranges = LineRangeSet::default();
            ranges.add_range(r(0, 5), &mut expect(vec![r(0, 5)]));
            ranges.update(&[LineModification::new(2, 2, 0, 2)]);
            assert_eq!(ranges.view(), &[r(0, 2), r(4, 7)]);
        }
        {
            let mut ranges = LineRangeSet::default();
            ranges.add_range(r(0, 1), &mut expect(vec![r(0, 1)]));
            ranges.add_range(r(5, 10), &mut expect(vec![r(5, 10)]));
            ranges.add_range(r(15, 20), &mut expect(vec![r(15, 20)]));
            ranges.add_range(r(25, 30), &mut expect(vec![r(25, 30)]));
            ranges.update(&[LineModification::new(2, 2, 3, 0)]);
            assert_eq!(ranges.view(), &[r(0, 1), r(2, 7), r(12, 17), r(22, 27)]);
        }
    }
}
