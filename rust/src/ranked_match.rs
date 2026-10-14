//! Fuzzy matching and ranking of completion candidates against a query.

use crate::unicode::{Codepoint, DEFAULT_EXTRA_WORD_CHARS, is_lower, is_upper, is_word, to_lower};
use crate::utf8::{self, char_to_codepoint, codepoints, read_codepoint};
use crate::word_splitter::words;
use std::cmp::Ordering;

#[cxx::bridge(namespace = "Kakoune::rs")]
mod ffi {
    /// How well a candidate matches, minus the candidate itself.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct Rank {
        flags: u8,
        full_word_match_count: i32,
        word_boundary_match_count: i32,
        max_index: i32,
        input_sequence_number: usize,
    }

    extern "Rust" {
        fn used_letters(s: &[u8]) -> u64;
        fn ranked_match(candidate: &[u8], query: &[u8], rank: &mut Rank) -> bool;
        fn ranked_match_with_letters(
            candidate: &[u8],
            candidate_letters: u64,
            query: &[u8],
            query_letters: u64,
            rank: &mut Rank,
        ) -> bool;
        fn ranked_match_less(
            lhs_candidate: &[u8],
            lhs: &Rank,
            rhs_candidate: &[u8],
            rhs: &Rank,
        ) -> bool;
    }
}

pub use ffi::Rank;

/// A set of characters, as a bit per ASCII letter plus `_`, `-` and
/// everything else. Lets matching rule out most candidates cheaply.
pub type UsedLetters = u64;

const UPPER_MASK: UsedLetters = 0xFFFFFFC000000;

pub fn used_letters(s: &[u8]) -> UsedLetters {
    s.iter().fold(0, |res, &c| {
        res | 1
            << match c {
                b'a'..=b'z' => c - b'a',
                b'A'..=b'Z' => c - b'A' + 26,
                b'_' => 53,
                b'-' => 54,
                _ => 63,
            }
    })
}

fn letters_to_lower(letters: UsedLetters) -> UsedLetters {
    (letters & UPPER_MASK) >> 26 | letters & !UPPER_MASK
}

fn letters_match(query: UsedLetters, letters: UsedLetters) -> bool {
    query & letters == query
}

/// Ranking criteria, in increasing order of precedence when comparing.
mod flags {
    pub const SINGLE_WORD: u8 = 1 << 0;
    pub const CONTIGUOUS: u8 = 1 << 1;
    pub const ONLY_WORD_BOUNDARY: u8 = 1 << 2;
    pub const PREFIX: u8 = 1 << 3;
    pub const BASE_NAME: u8 = 1 << 4;
    pub const SMART_FULL_MATCH: u8 = 1 << 5;
    pub const FULL_MATCH: u8 = 1 << 6;
}

#[derive(Clone, Copy, Debug)]
pub struct RankedMatch<'a> {
    pub candidate: &'a [u8],
    pub rank: Rank,
}

impl<'a> RankedMatch<'a> {
    /// Matches `candidate` against `query`, or returns `None` if it doesn't
    /// contain the query's characters in order.
    pub fn new(candidate: &'a [u8], query: &[u8]) -> Option<Self> {
        Self::with_filter(candidate, query, || true)
    }

    /// Like `new`, but first rules out candidates missing some of the query's
    /// letters, using the precomputed `used_letters` of each.
    pub fn with_letters(
        candidate: &'a [u8],
        candidate_letters: UsedLetters,
        query: &[u8],
        query_letters: UsedLetters,
    ) -> Option<Self> {
        Self::with_filter(candidate, query, || {
            letters_match(
                letters_to_lower(query_letters),
                letters_to_lower(candidate_letters),
            ) && letters_match(query_letters & UPPER_MASK, candidate_letters & UPPER_MASK)
        })
    }

    fn with_filter(
        candidate: &'a [u8],
        query: &[u8],
        filter: impl FnOnce() -> bool,
    ) -> Option<Self> {
        if query.len() > candidate.len() {
            return None;
        }
        let mut rank = Rank::default();
        if query.is_empty() {
            return Some(Self { candidate, rank });
        }
        if !filter() {
            return None;
        }

        let res = subsequence_match_smart_case(candidate, query)?;
        rank.max_index = res.max_index;
        if res.single_word {
            rank.flags |= flags::SINGLE_WORD;
        }

        // The base name starts after the last '/', ignoring a trailing one
        let base_name = candidate[..candidate.len() - 1]
            .iter()
            .rposition(|&c| c == b'/')
            .map_or(0, |i| i + 1);
        if base_name == 0 || subsequence_match_smart_case(&candidate[base_name..], query).is_some()
        {
            rank.flags |= flags::BASE_NAME;
            if candidate.len() - base_name >= query.len() {
                // Past the end of the candidate, the C++ compares against
                // what reading at the end returns
                let mut candidate_cps =
                    codepoints(&candidate[base_name..]).chain(std::iter::repeat(utf8::END));
                if codepoints(query).all(|q| smartcase_eq(candidate_cps.next().unwrap(), q)) {
                    rank.flags |= flags::PREFIX;
                }
            }
        }

        // Compares bytes, like the C++ comparing `char`s converted to
        // `Codepoint`s
        if let Some(pos) = candidate.windows(query.len()).position(|window| {
            window
                .iter()
                .zip(query)
                .all(|(&c, &q)| smartcase_eq(char_to_codepoint(c), char_to_codepoint(q)))
        }) {
            rank.flags |= flags::CONTIGUOUS;
            if codepoints(query).all(|cp| is_word(cp, DEFAULT_EXTRA_WORD_CHARS)) {
                rank.flags |= flags::SINGLE_WORD;
            }
            if pos == 0 {
                rank.flags |= flags::PREFIX;
                if query.len() == candidate.len() {
                    rank.flags |= flags::SMART_FULL_MATCH;
                    if candidate == query {
                        rank.flags |= flags::FULL_MATCH;
                    }
                }
            }
        }

        rank.full_word_match_count = count_full_word_match(candidate, query);
        rank.word_boundary_match_count = count_word_boundaries_match(candidate, query);
        // Compares a character count with a byte count, like the C++
        if rank.word_boundary_match_count as usize == query.len() {
            rank.flags |= flags::ONLY_WORD_BOUNDARY;
        }

        Some(Self { candidate, rank })
    }

    /// Orders matches from best to worst.
    pub fn rank_cmp(&self, other: &Self) -> Ordering {
        let (lhs, rhs) = (&self.rank, &other.rank);

        // Flags are different, use their ordering to return the first match
        let diff = lhs.flags ^ rhs.flags;
        if diff != 0 {
            return (rhs.flags & diff).cmp(&(lhs.flags & diff));
        }

        // If we are SingleWord, we dont want to take word boundaries from
        // other words into account.
        if lhs.flags & (flags::PREFIX | flags::SINGLE_WORD) == 0 {
            let ord = rhs
                .word_boundary_match_count
                .cmp(&lhs.word_boundary_match_count);
            if ord.is_ne() {
                return ord;
            }
        }

        rhs.full_word_match_count
            .cmp(&lhs.full_word_match_count)
            .then(lhs.max_index.cmp(&rhs.max_index))
            .then(lhs.input_sequence_number.cmp(&rhs.input_sequence_number))
            .then_with(|| cmp_candidates(self.candidate, other.candidate))
    }
}

/// Orders candidates by their first differing character, preferring word
/// boundaries, then lowercase, then smaller codepoints except for `/`, which
/// comes first.
fn cmp_candidates(s1: &[u8], s2: &[u8]) -> Ordering {
    let order = |cp: Codepoint| if cp == '/' as Codepoint { 0 } else { cp };

    let (mut it1, mut it2) = (0, 0);
    let (mut last1, mut last2) = (0, 0);
    loop {
        // Find the next mismatch
        while it1 != s1.len() && it2 != s2.len() && s1[it1] == s2[it2] {
            it1 += 1;
            it2 += 1;
        }
        if it1 == s1.len() || it2 == s2.len() {
            return (it2 == s2.len()).cmp(&(it1 == s1.len()));
        }

        // Compare codepoints
        it1 = utf8::character_start(s1, it1, last1);
        it2 = utf8::character_start(s2, it2, last2);
        let (start1, start2) = (it1, it2);
        let cp1 = read_codepoint(s1, &mut it1);
        let cp2 = read_codepoint(s2, &mut it2);
        if cp1 != cp2 {
            let is_wb1 = is_word_boundary(utf8::prev_codepoint(s1, start1), cp1);
            let is_wb2 = is_word_boundary(utf8::prev_codepoint(s2, start2), cp2);
            if is_wb1 != is_wb2 {
                return is_wb2.cmp(&is_wb1);
            }
            let (low1, low2) = (is_lower(cp1), is_lower(cp2));
            if low1 != low2 {
                return low2.cmp(&low1);
            }
            return order(cp1).cmp(&order(cp2));
        }
        last1 = it1;
        last2 = it2;
    }
}

fn is_word_boundary(prev: Codepoint, c: Codepoint) -> bool {
    is_word(prev, &[]) != is_word(c, &[]) || is_lower(prev) != is_lower(c)
}

fn smartcase_eq(candidate: Codepoint, query: Codepoint) -> bool {
    query
        == if is_lower(query) {
            to_lower(candidate)
        } else {
            candidate
        }
}

struct SubseqRes {
    max_index: i32,
    single_word: bool,
}

/// Finds `subseq` in `s` as a subsequence of characters, lowercase characters
/// in `subseq` matching either case.
fn subsequence_match_smart_case(s: &[u8], subseq: &[u8]) -> Option<SubseqRes> {
    let mut single_word = true;
    let mut max_index = -1;
    let mut pos = 0;
    let mut index = 0;
    let mut subseq_pos = 0;
    while subseq_pos != subseq.len() {
        if pos == s.len() {
            return None;
        }
        let c = read_codepoint(subseq, &mut subseq_pos);
        if single_word && !is_word(c, DEFAULT_EXTRA_WORD_CHARS) {
            single_word = false;
        }
        loop {
            let str_c = read_codepoint(s, &mut pos);
            if smartcase_eq(str_c, c) {
                break;
            }
            if max_index != -1 && single_word && !is_word(str_c, DEFAULT_EXTRA_WORD_CHARS) {
                single_word = false;
            }
            index += 1;
            if pos == s.len() {
                return None;
            }
        }
        max_index = index;
        index += 1;
    }
    Some(SubseqRes {
        max_index,
        single_word,
    })
}

/// Counts the words of `query` that are also words of `candidate`.
fn count_full_word_match(candidate: &[u8], query: &[u8]) -> i32 {
    words(query, &[])
        .filter(|query_word| words(candidate, &[]).any(|word| word == *query_word))
        .count() as i32
}

/// Counts the characters of `query` that match, in order, characters at word
/// boundaries in `candidate`.
fn count_word_boundaries_match(candidate: &[u8], query: &[u8]) -> i32 {
    let mut count = 0;
    let mut query_cps = codepoints(query);
    let mut prev = 0;
    for c in codepoints(candidate) {
        let is_word_boundary = prev == 0
            || (!is_word(prev, &[]) && is_word(c, &[]))
            || (is_lower(prev) && is_upper(c));
        prev = c;
        if !is_word_boundary {
            continue;
        }

        let lc = to_lower(c);
        let mut rest = query_cps.clone();
        while let Some(qc) = rest.next() {
            if qc == if is_lower(qc) { lc } else { c } {
                count += 1;
                query_cps = rest;
                break;
            }
        }
        if query_cps.clone().next().is_none() {
            break;
        }
    }
    count
}

fn ranked_match(candidate: &[u8], query: &[u8], rank: &mut Rank) -> bool {
    RankedMatch::new(candidate, query)
        .map(|m| *rank = m.rank)
        .is_some()
}

fn ranked_match_with_letters(
    candidate: &[u8],
    candidate_letters: u64,
    query: &[u8],
    query_letters: u64,
    rank: &mut Rank,
) -> bool {
    RankedMatch::with_letters(candidate, candidate_letters, query, query_letters)
        .map(|m| *rank = m.rank)
        .is_some()
}

fn ranked_match_less(lhs_candidate: &[u8], lhs: &Rank, rhs_candidate: &[u8], rhs: &Rank) -> bool {
    let lhs = RankedMatch {
        candidate: lhs_candidate,
        rank: *lhs,
    };
    let rhs = RankedMatch {
        candidate: rhs_candidate,
        rank: *rhs,
    };
    lhs.rank_cmp(&rhs).is_lt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preferred(query: &str, better: &str, worse: &str) -> bool {
        let better = RankedMatch::new(better.as_bytes(), query.as_bytes()).unwrap();
        let worse = RankedMatch::new(worse.as_bytes(), query.as_bytes()).unwrap();
        better.rank_cmp(&worse).is_lt()
    }

    #[test]
    fn counts_word_boundaries_match() {
        let count = |c: &str, q: &str| count_word_boundaries_match(c.as_bytes(), q.as_bytes());
        assert_eq!(count("run_all_tests", "rat"), 3);
        assert_eq!(count("run_all_tests", "at"), 2);
        assert_eq!(count("countWordBoundariesMatch", "wm"), 2);
        assert_eq!(count("countWordBoundariesMatch", "cobm"), 3);
        assert_eq!(count("countWordBoundariesMatch", "cWBM"), 4);
    }

    #[test]
    fn ranks_matches() {
        assert!(preferred("so", "source", "source_data"));
        assert!(!preferred("so", "source_data", "source"));
        assert!(!preferred("so", "source", "source"));
        assert!(preferred("wo", "single/word", "multiw/ord"));
        assert!(preferred("foobar", "foo/bar/foobar", "foo/bar/baz"));
        assert!(preferred("db", "delete-buffer", "debug"));
        assert!(preferred("ct", "create_task", "constructor"));
        assert!(preferred("cla", "class", "class::attr"));
        assert!(preferred("meta", "meta/", "meta-a/"));
        assert!(preferred("find", "find(1p)", "findfs(8)"));
        assert!(preferred("fin", "find(1p)", "findfs(8)"));
        assert!(preferred("sys_find", "sys_find(1p)", "sys_findfs(8)"));
        assert!(preferred("", "init", "__init__"));
        assert!(preferred("ini", "init", "__init__"));
        assert!(preferred("", "a", "b"));
        assert!(preferred("expresins", "expresions", "expressionism's"));
        assert!(preferred(
            "foo_b",
            "foo/bar/foo_bar.baz",
            "test/test_foo_bar.baz"
        ));
        assert!(preferred(
            "foo_b",
            "bar/bar_qux/foo_bar.baz",
            "foo/test_foo_bar.baz"
        ));
        assert!(preferred("foo_bar", "bar/foo_bar.baz", "foo_bar/qux.baz"));
        assert!(preferred("fb", "foo_bar/", "foo.bar"));
        assert!(preferred("foo_bar", "test_foo_bar", "foo_test_bar"));
        assert!(preferred(
            "rm.cc",
            "src/ranked_match.cc",
            "test/README.asciidoc"
        ));
        assert!(preferred(
            "luaremote",
            "src/script/LuaRemote.cpp",
            "tests/TestLuaRemote.cpp"
        ));
        assert!(preferred(
            "lang/haystack/needle.c",
            "git.evilcorp.com/language/haystack/aaa/needle.c",
            "git.evilcorp.com/aaa/ng/wrong-haystack/needle.cpp"
        ));
        assert!(preferred(
            "evilcorp-lint/bar.go",
            "scripts/evilcorp-lint/foo/bar.go",
            "src/evilcorp-client/foo/bar.go"
        ));
    }

    #[test]
    fn computes_used_letters() {
        assert_eq!(
            used_letters(b"abcd"),
            letters_to_lower(used_letters(b"abcdABCD"))
        );
    }

    #[test]
    fn filters_by_letters() {
        let matches = |c: &str, q: &str| {
            let (c, q) = (c.as_bytes(), q.as_bytes());
            RankedMatch::with_letters(c, used_letters(c), q, used_letters(q)).is_some()
        };
        assert!(matches("FooBar", "fb"));
        assert!(matches("FooBar", "FB"));
        assert!(!matches("foobar", "FB"));
        assert!(!matches("foobar", "fz"));
    }
}
