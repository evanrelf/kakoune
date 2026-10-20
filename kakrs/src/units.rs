//! Port of units.hh and coord.hh

pub type LineCount = i32;
pub type ByteCount = i32;
pub type CharCount = i32;
pub type ColumnCount = i32;
pub type Codepoint = u32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferCoord {
    pub line: LineCount,
    pub column: ByteCount,
}

impl BufferCoord {
    pub const fn new(line: LineCount, column: ByteCount) -> Self {
        BufferCoord { line, column }
    }
    pub const fn line(line: LineCount) -> Self {
        BufferCoord { line, column: 0 }
    }
}

impl std::ops::Add for BufferCoord {
    type Output = BufferCoord;
    fn add(self, o: BufferCoord) -> BufferCoord {
        BufferCoord::new(self.line + o.line, self.column + o.column)
    }
}

impl std::ops::Sub for BufferCoord {
    type Output = BufferCoord;
    fn sub(self, o: BufferCoord) -> BufferCoord {
        BufferCoord::new(self.line - o.line, self.column - o.column)
    }
}

impl std::ops::AddAssign for BufferCoord {
    fn add_assign(&mut self, o: BufferCoord) {
        self.line += o.line;
        self.column += o.column;
    }
}

impl std::ops::SubAssign for BufferCoord {
    fn sub_assign(&mut self, o: BufferCoord) {
        self.line -= o.line;
        self.column -= o.column;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayCoord {
    pub line: LineCount,
    pub column: ColumnCount,
}

impl DisplayCoord {
    pub const fn new(line: LineCount, column: ColumnCount) -> Self {
        DisplayCoord { line, column }
    }
}

impl std::ops::Add for DisplayCoord {
    type Output = DisplayCoord;
    fn add(self, o: DisplayCoord) -> DisplayCoord {
        DisplayCoord::new(self.line + o.line, self.column + o.column)
    }
}

impl std::ops::Sub for DisplayCoord {
    type Output = DisplayCoord;
    fn sub(self, o: DisplayCoord) -> DisplayCoord {
        DisplayCoord::new(self.line - o.line, self.column - o.column)
    }
}

impl std::ops::AddAssign for DisplayCoord {
    fn add_assign(&mut self, o: DisplayCoord) {
        self.line += o.line;
        self.column += o.column;
    }
}

impl std::ops::SubAssign for DisplayCoord {
    fn sub_assign(&mut self, o: DisplayCoord) {
        self.line -= o.line;
        self.column -= o.column;
    }
}

/// A BufferCoord with a preferred display column, used for vertical movement.
/// Mirrors the C++ subclass: it derefs to its BufferCoord, and comparison and
/// equality only consider the coordinate.
#[derive(Clone, Copy, Debug)]
pub struct BufferCoordAndTarget {
    pub coord: BufferCoord,
    pub target: ColumnCount,
    pub display_target: ColumnCount,
}

impl Default for BufferCoordAndTarget {
    fn default() -> Self {
        BufferCoordAndTarget { coord: BufferCoord::default(), target: -1, display_target: -1 }
    }
}

impl BufferCoordAndTarget {
    pub const fn new(coord: BufferCoord, target: ColumnCount, display_target: ColumnCount) -> Self {
        BufferCoordAndTarget { coord, target, display_target }
    }
    pub const fn coord(&self) -> BufferCoord {
        self.coord
    }
}

impl std::ops::Deref for BufferCoordAndTarget {
    type Target = BufferCoord;
    fn deref(&self) -> &BufferCoord {
        &self.coord
    }
}

impl std::ops::DerefMut for BufferCoordAndTarget {
    fn deref_mut(&mut self) -> &mut BufferCoord {
        &mut self.coord
    }
}

impl From<BufferCoord> for BufferCoordAndTarget {
    fn from(c: BufferCoord) -> Self {
        BufferCoordAndTarget { coord: c, target: -1, display_target: -1 }
    }
}

impl From<BufferCoordAndTarget> for BufferCoord {
    fn from(c: BufferCoordAndTarget) -> Self {
        c.coord
    }
}

impl PartialEq for BufferCoordAndTarget {
    fn eq(&self, o: &Self) -> bool {
        self.coord == o.coord
    }
}

impl Eq for BufferCoordAndTarget {}

impl PartialOrd for BufferCoordAndTarget {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for BufferCoordAndTarget {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.coord.cmp(&o.coord)
    }
}

/// Generic half-open range, port of range.hh
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Range<T> {
    pub begin: T,
    pub end: T,
}

impl<T> Range<T> {
    pub const fn new(begin: T, end: T) -> Self {
        Range { begin, end }
    }
}

impl<T: PartialEq> Range<T> {
    pub fn empty(&self) -> bool {
        self.begin == self.end
    }
}

pub type BufferRange = Range<BufferCoord>;

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::generators as gs;
    use hegel::TestCase;

    // Coordinates within a range where sums cannot overflow (as in C++,
    // where signed overflow is undefined behaviour)
    fn coord_part() -> impl hegel::PrintableGenerator<i32> {
        gs::integers::<i32>().min_value(i32::MIN / 2).max_value(i32::MAX / 2)
    }

    fn buffer_coord(tc: &TestCase) -> BufferCoord {
        BufferCoord::new(tc.draw(coord_part()), tc.draw(coord_part()))
    }

    #[hegel::test(test_cases = 1000)]
    fn coord_ordering_is_lexicographic(tc: TestCase) {
        let a = buffer_coord(&tc);
        let b = buffer_coord(&tc);
        assert_eq!(a.cmp(&b), (a.line, a.column).cmp(&(b.line, b.column)));
        let da = DisplayCoord::new(a.line, a.column);
        let db = DisplayCoord::new(b.line, b.column);
        assert_eq!(da.cmp(&db), a.cmp(&b));
    }

    #[hegel::test(test_cases = 1000)]
    fn coord_sub_inverts_add(tc: TestCase) {
        let a = buffer_coord(&tc);
        let b = buffer_coord(&tc);
        assert_eq!((a + b) - b, a);
        let mut c = a;
        c += b;
        assert_eq!(c, a + b);
        c -= b;
        assert_eq!(c, a);
        let (da, db) = (DisplayCoord::new(a.line, a.column), DisplayCoord::new(b.line, b.column));
        assert_eq!((da + db) - db, da);
    }

    // BufferCoordAndTarget compares like its coordinate, ignoring targets
    #[hegel::test(test_cases = 1000)]
    fn coord_and_target_compares_coords_only(tc: TestCase) {
        let a = BufferCoordAndTarget::new(buffer_coord(&tc), tc.draw(gs::integers()), tc.draw(gs::integers()));
        let b = BufferCoordAndTarget::new(buffer_coord(&tc), tc.draw(gs::integers()), tc.draw(gs::integers()));
        assert_eq!(a == b, a.coord == b.coord);
        assert_eq!(a.cmp(&b), a.coord.cmp(&b.coord));
        let from: BufferCoordAndTarget = a.coord.into();
        assert_eq!((from.target, from.display_target), (-1, -1));
        assert_eq!(BufferCoord::from(a), a.coord);
    }

    #[hegel::test(test_cases = 1000)]
    fn range_is_empty_iff_bounds_equal(tc: TestCase) {
        let a = buffer_coord(&tc);
        let b = if tc.draw(gs::booleans()) { a } else { buffer_coord(&tc) };
        assert_eq!(Range::new(a, b).empty(), a == b);
    }
}
