//! Port of ranked_match.hh/cc and word_splitter.hh

use crate::unicode::{is_lower, is_upper, is_word, is_word_default, to_lower};
use crate::units::Codepoint;
use crate::utf8;
use std::cmp::Ordering;

pub type UsedLetters = u64;

pub fn used_letters(s: &[u8]) -> UsedLetters {
    let mut res: UsedLetters = 0;
    for &c in s {
        if c.is_ascii_lowercase() {
            res |= 1u64 << (c - b'a');
        } else if c.is_ascii_uppercase() {
            res |= 1u64 << (c - b'A' + 26);
        } else if c == b'_' {
            res |= 1u64 << 53;
        } else if c == b'-' {
            res |= 1u64 << 54;
        } else {
            res |= 1u64 << 63;
        }
    }
    res
}

pub const UPPER_MASK: UsedLetters = 0xFFFFFFC000000;

pub fn letters_to_lower(letters: UsedLetters) -> UsedLetters {
    ((letters & UPPER_MASK) >> 26) | (letters & !UPPER_MASK)
}

fn letters_match(query: UsedLetters, letters: UsedLetters) -> bool {
    (query & letters) == query
}

/// Port of WordSplitter: iterate over words (sequences of word characters
/// no longer than max_word_len bytes)
pub struct WordSplitter<'a> {
    content: &'a [u8],
    extra_word_chars: &'a [Codepoint],
    word_end: usize,
}

pub const MAX_WORD_LEN: usize = 100;

impl<'a> WordSplitter<'a> {
    pub fn new(content: &'a [u8], extra_word_chars: &'a [Codepoint]) -> Self {
        WordSplitter { content, extra_word_chars, word_end: 0 }
    }
}

impl<'a> Iterator for WordSplitter<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        let s = self.content;
        let end = s.len();
        loop {
            let mut it = self.word_end;
            let mut word_begin = it;
            if it == end {
                return None;
            }
            while it != end && !is_word(utf8::read_codepoint(s, &mut it), self.extra_word_chars) {
                word_begin = it;
            }
            let mut word_end = it;
            while it != end && is_word(utf8::read_codepoint(s, &mut it), self.extra_word_chars) {
                word_end = it;
            }
            self.word_end = word_end;
            if word_begin == end {
                return None;
            }
            if word_end - word_begin <= MAX_WORD_LEN {
                return Some(&s[word_begin..word_end]);
            }
        }
    }
}

crate::kak_flags! {
    struct MatchFlags: u32 {
        const None = 0;
        // Order is important, the highest bit has precedence for comparison
        const SingleWord       = 1 << 0;
        const Contiguous       = 1 << 1;
        const OnlyWordBoundary = 1 << 2;
        const Prefix           = 1 << 3;
        const BaseName         = 1 << 4;
        const SmartFullMatch   = 1 << 5;
        const FullMatch        = 1 << 6;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RankedMatch<'a> {
    candidate: &'a [u8],
    matches: bool,
    flags: MatchFlags,
    full_word_match_count: i32,
    word_boundary_match_count: i32,
    max_index: i32,
    input_sequence_number: usize,
}

fn count_word_boundaries_match(candidate: &[u8], query: &[u8]) -> i32 {
    let mut count = 0;
    let mut query_it = 0;
    let mut prev: Codepoint = 0;
    let mut it = 0;
    while it < candidate.len() {
        let c = utf8::codepoint_at(candidate, it);
        it = utf8::next(candidate, it);
        let is_word_boundary =
            prev == 0 || (!is_word(prev, &[]) && is_word(c, &[])) || (is_lower(prev) && is_upper(c));
        prev = c;
        if !is_word_boundary {
            continue;
        }
        let lc = to_lower(c);
        let mut qit = query_it;
        while qit < query.len() {
            let qc = utf8::codepoint_at(query, qit);
            let qnext = utf8::next(query, qit);
            if qc == (if is_lower(qc) { lc } else { c }) {
                count += 1;
                query_it = qnext;
                break;
            }
            qit = qnext;
        }
        if query_it == query.len() {
            break;
        }
    }
    count
}

fn count_full_word_match(candidate: &[u8], query: &[u8]) -> i32 {
    let mut count = 0;
    for query_word in WordSplitter::new(query, &[]) {
        for word in WordSplitter::new(candidate, &[]) {
            if word == query_word {
                count += 1;
                break;
            }
        }
    }
    count
}

fn smartcase_eq(candidate: Codepoint, query: Codepoint) -> bool {
    query == (if is_lower(query) { to_lower(candidate) } else { candidate })
}

struct SubseqRes {
    max_index: i32,
    single_word: bool,
}

fn subsequence_match_smart_case(s: &[u8], subseq: &[u8]) -> Option<SubseqRes> {
    let mut single_word = true;
    let mut max_index = -1;
    let mut it = 0;
    let mut index = 0;
    let mut subseq_it = 0;
    while subseq_it < subseq.len() {
        if it == s.len() {
            return None;
        }
        let c = utf8::read_codepoint(subseq, &mut subseq_it);
        if single_word && !is_word_default(c) {
            single_word = false;
        }
        loop {
            let str_c = utf8::read_codepoint(s, &mut it);
            if smartcase_eq(str_c, c) {
                break;
            }
            if max_index != -1 && single_word && !is_word_default(str_c) {
                single_word = false;
            }
            index += 1;
            if it == s.len() {
                return None;
            }
        }
        max_index = index;
        index += 1;
    }
    Some(SubseqRes { max_index, single_word })
}

fn is_word_boundary(prev: Codepoint, c: Codepoint) -> bool {
    is_word(prev, &[]) != is_word(c, &[]) || is_lower(prev) != is_lower(c)
}

impl<'a> RankedMatch<'a> {
    fn empty() -> Self {
        RankedMatch {
            candidate: &[],
            matches: false,
            flags: MatchFlags::None,
            full_word_match_count: 0,
            word_boundary_match_count: 0,
            max_index: 0,
            input_sequence_number: 0,
        }
    }

    fn with_test(candidate: &'a [u8], query: &[u8], test: impl FnOnce() -> bool) -> Self {
        let mut m = RankedMatch::empty();
        if query.len() > candidate.len() {
            return m;
        }
        if query.is_empty() {
            m.candidate = candidate;
            m.matches = true;
            return m;
        }
        if !test() {
            return m;
        }
        let Some(res) = subsequence_match_smart_case(candidate, query) else {
            return m;
        };
        m.candidate = candidate;
        m.matches = true;
        m.max_index = res.max_index;
        if res.single_word {
            m.flags |= MatchFlags::SingleWord;
        }

        // find last '/' ignoring the last character
        let base_start = if candidate.len() <= 1 {
            0
        } else {
            candidate[..candidate.len() - 1].iter().rposition(|&c| c == b'/').map(|p| p + 1).unwrap_or(0)
        };
        if base_start == 0 || subsequence_match_smart_case(&candidate[base_start..], query).is_some() {
            m.flags |= MatchFlags::BaseName;
            let base = &candidate[base_start..];
            if base.len() >= query.len() {
                // std::equal over utf8::iterators: decode at each position,
                // step to the next character start
                let mut eq = true;
                let mut qi = 0;
                let mut ci = 0;
                while qi < query.len() {
                    let q = utf8::codepoint_at(query, qi);
                    let c = utf8::codepoint_at(base, ci);
                    if !smartcase_eq(c, q) {
                        eq = false;
                        break;
                    }
                    qi = utf8::next(query, qi);
                    ci = utf8::next(base, ci);
                }
                if eq {
                    m.flags |= MatchFlags::Prefix;
                }
            }
        }

        // std::search with smartcase_eq applied bytewise (as in the C++ code)
        let found = (0..=candidate.len() - query.len())
            .find(|&start| (0..query.len()).all(|i| smartcase_eq(candidate[start + i] as i8 as i32 as u32, query[i] as i8 as i32 as u32)));
        if let Some(pos) = found {
            m.flags |= MatchFlags::Contiguous;
            if utf8::codepoints(query).all(is_word_default) {
                m.flags |= MatchFlags::SingleWord;
            }
            if pos == 0 {
                m.flags |= MatchFlags::Prefix;
                if query.len() == candidate.len() {
                    m.flags |= MatchFlags::SmartFullMatch;
                    if candidate == query {
                        m.flags |= MatchFlags::FullMatch;
                    }
                }
            }
        }

        m.full_word_match_count = count_full_word_match(candidate, query);
        m.word_boundary_match_count = count_word_boundaries_match(candidate, query);
        if m.word_boundary_match_count as usize == query.len() {
            m.flags |= MatchFlags::OnlyWordBoundary;
        }
        m
    }

    pub fn new(candidate: &'a [u8], query: &[u8]) -> Self {
        RankedMatch::with_test(candidate, query, || true)
    }

    pub fn with_letters(
        candidate: &'a [u8],
        candidate_letters: UsedLetters,
        query: &[u8],
        query_letters: UsedLetters,
    ) -> Self {
        RankedMatch::with_test(candidate, query, || {
            letters_match(letters_to_lower(query_letters), letters_to_lower(candidate_letters))
                && letters_match(query_letters & UPPER_MASK, candidate_letters & UPPER_MASK)
        })
    }

    pub fn candidate(&self) -> &'a [u8] {
        self.candidate
    }

    pub fn is_match(&self) -> bool {
        self.matches
    }

    pub fn set_input_sequence_number(&mut self, i: usize) {
        self.input_sequence_number = i;
    }

    /// operator<
    pub fn less(&self, other: &RankedMatch) -> bool {
        let diff = self.flags ^ other.flags;
        // flags are different, use their ordering to return the first match
        if diff != MatchFlags::None {
            return (self.flags & diff).0 > (other.flags & diff).0;
        }
        // If we are SingleWord, we dont want to take word boundaries from other
        // words into account.
        if !self.flags.has(MatchFlags::Prefix | MatchFlags::SingleWord)
            && self.word_boundary_match_count != other.word_boundary_match_count
        {
            return self.word_boundary_match_count > other.word_boundary_match_count;
        }
        if self.full_word_match_count != other.full_word_match_count {
            return self.full_word_match_count > other.full_word_match_count;
        }
        if self.max_index != other.max_index {
            return self.max_index < other.max_index;
        }
        if self.input_sequence_number != other.input_sequence_number {
            return self.input_sequence_number < other.input_sequence_number;
        }
        // Reorder codepoints to improve matching behaviour
        let order = |cp: Codepoint| if cp == '/' as u32 { 0 } else { cp };
        let s1 = self.candidate;
        let s2 = other.candidate;
        let (mut it1, mut it2) = (0usize, 0usize);
        let (mut last1, mut last2) = (0usize, 0usize);
        loop {
            // find next mismatch
            while it1 != s1.len() && it2 != s2.len() && s1[it1] == s2[it2] {
                it1 += 1;
                it2 += 1;
            }
            if it1 == s1.len() || it2 == s2.len() {
                return it1 == s1.len() && it2 != s2.len();
            }
            // compare codepoints
            it1 = char_start_bounded(s1, it1, last1);
            it2 = char_start_bounded(s2, it2, last2);
            let itsave1 = it1;
            let itsave2 = it2;
            let cp1 = utf8::read_codepoint(s1, &mut it1);
            let cp2 = utf8::read_codepoint(s2, &mut it2);
            if cp1 != cp2 {
                let cplast1 = utf8::prev_codepoint(s1, itsave1, 0);
                let cplast2 = utf8::prev_codepoint(s2, itsave2, 0);
                let is_wb1 = is_word_boundary(cplast1, cp1);
                let is_wb2 = is_word_boundary(cplast2, cp2);
                if is_wb1 != is_wb2 {
                    return is_wb1;
                }
                let low1 = is_lower(cp1);
                let low2 = is_lower(cp2);
                if low1 != low2 {
                    return low1;
                }
                return order(cp1) < order(cp2);
            }
            last1 = it1;
            last2 = it2;
        }
    }

    pub fn cmp(&self, other: &RankedMatch) -> Ordering {
        if self.less(other) {
            Ordering::Less
        } else if other.less(self) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

fn char_start_bounded(s: &[u8], mut pos: usize, begin: usize) -> usize {
    while pos != begin && !utf8::is_character_start(s[pos]) {
        pos -= 1;
    }
    pos
}

/// Sort ranked matches with the C++ operator< ordering
pub fn sort_matches(matches: &mut [RankedMatch]) {
    matches.sort_by(|a, b| a.cmp(b));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preferred(query: &str, better: &str, worse: &str) -> bool {
        RankedMatch::new(better.as_bytes(), query.as_bytes()).less(&RankedMatch::new(worse.as_bytes(), query.as_bytes()))
    }

    #[test]
    fn test_ranked_match() {
        assert_eq!(count_word_boundaries_match(b"run_all_tests", b"rat"), 3);
        assert_eq!(count_word_boundaries_match(b"run_all_tests", b"at"), 2);
        assert_eq!(count_word_boundaries_match(b"countWordBoundariesMatch", b"wm"), 2);
        assert_eq!(count_word_boundaries_match(b"countWordBoundariesMatch", b"cobm"), 3);
        assert_eq!(count_word_boundaries_match(b"countWordBoundariesMatch", b"cWBM"), 4);
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
        assert!(preferred("foo_b", "foo/bar/foo_bar.baz", "test/test_foo_bar.baz"));
        assert!(preferred("foo_b", "bar/bar_qux/foo_bar.baz", "foo/test_foo_bar.baz"));
        assert!(preferred("foo_bar", "bar/foo_bar.baz", "foo_bar/qux.baz"));
        assert!(preferred("fb", "foo_bar/", "foo.bar"));
        assert!(preferred("foo_bar", "test_foo_bar", "foo_test_bar"));
        assert!(preferred("rm.cc", "src/ranked_match.cc", "test/README.asciidoc"));
        assert!(preferred("luaremote", "src/script/LuaRemote.cpp", "tests/TestLuaRemote.cpp"));
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
    fn test_used_letters() {
        assert_eq!(used_letters(b"abcd"), letters_to_lower(used_letters(b"abcdABCD")));
    }

    use crate::unicode::init_test_locale;
    use hegel::generators as gs;
    use hegel::{Generator as _, TestCase};

    fn ascii_word(max: usize) -> impl hegel::PrintableGenerator<Vec<u8>> {
        gs::vecs(gs::sampled_from(b"abAB_-/. x".to_vec())).max_size(max)
    }

    /// Candidates: mostly path/identifier like ascii, sometimes arbitrary
    fn candidate() -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(
            ascii_word(12),
            gs::vecs(gs::sampled_from(vec![&b"a"[..], b"B", b"/", b"_", b"\xc3\xa9", b"\xc3\x89", b"\x00", b"\x80"]))
                .max_size(8)
                .map(|v| v.concat()),
            gs::binary().max_size(12),
        )
    }

    fn query() -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(ascii_word(4), candidate())
    }

    /// Smart case subsequence on ascii: lowercase query letters match either
    /// case, everything else matches exactly.
    fn ascii_smartcase_subsequence(candidate: &[u8], query: &[u8]) -> bool {
        let mut it = candidate.iter();
        query.iter().all(|&q| it.any(|&c| if q.is_ascii_lowercase() { c.to_ascii_lowercase() == q } else { c == q }))
    }

    // Flags computed by the C++ code on invalid utf8: query characters are
    // iterated with utf8::iterator (stepping over stray continuation bytes)
    // for the SingleWord and base name Prefix checks.
    #[test]
    fn flags_on_invalid_utf8_match_cpp() {
        init_test_locale();
        let flags = |q: &[u8], c: &[u8]| RankedMatch::new(c, q).flags;
        assert_eq!(flags(b"\xc3\xe9", b"\xc3\xa9\xc9"), MatchFlags::SingleWord | MatchFlags::BaseName);
        assert_eq!(
            flags(b"\xc3\xa9\x80", b"\x80\x41.\xc3\xa9\x80-\xc3"),
            MatchFlags::SingleWord | MatchFlags::Contiguous | MatchFlags::BaseName
        );
    }

    #[hegel::test(test_cases = 1000)]
    fn matches_iff_smartcase_subsequence(tc: TestCase) {
        let c = tc.draw(ascii_word(12));
        let q = tc.draw(ascii_word(5));
        let expected = q.is_empty() || (q.len() <= c.len() && ascii_smartcase_subsequence(&c, &q));
        assert_eq!(RankedMatch::new(&c, &q).is_match(), expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn candidate_matches_itself_fully(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(candidate().filter(|c| !c.is_empty()));
        let m = RankedMatch::new(&c, &c);
        assert!(m.is_match());
        assert!(m.flags.has(MatchFlags::FullMatch));
    }

    #[hegel::test(test_cases = 1000)]
    fn full_match_flag_iff_equal(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(candidate());
        let q = tc.draw(query().filter(|q| !q.is_empty()));
        let m = RankedMatch::new(&c, &q);
        if m.is_match() {
            assert_eq!(m.flags.has(MatchFlags::FullMatch), c == q);
        }
    }

    fn valid_text() -> impl hegel::PrintableGenerator<Vec<u8>> {
        hegel::one_of!(gs::text(), gs::text().alphabet("aAbB/_.-éÉ")).map(String::into_bytes)
    }

    // Restricted to valid utf8: decoding invalid utf8 depends on the
    // following bytes (a lone "\xC0" decodes differently from "\xC0\x00"),
    // so with invalid text a prefix may fail to match (as in C++).
    #[hegel::test(test_cases = 1000)]
    fn exact_prefix_sets_prefix_and_contiguous(tc: TestCase) {
        init_test_locale();
        let q = tc.draw(valid_text().filter(|q| !q.is_empty()));
        let rest = tc.draw(valid_text());
        let c = [&q[..], &rest].concat();
        let m = RankedMatch::new(&c, &q);
        assert!(m.is_match());
        assert!(m.flags.has(MatchFlags::Prefix));
        assert!(m.flags.has(MatchFlags::Contiguous));
    }

    // The used letters precheck is a pure optimisation for ascii text: it
    // never changes the result.
    #[hegel::test(test_cases = 1000)]
    fn letters_precheck_agrees_on_ascii(tc: TestCase) {
        let c = tc.draw(ascii_word(12));
        let q = tc.draw(ascii_word(5));
        let fast = RankedMatch::with_letters(&c, used_letters(&c), &q, used_letters(&q));
        let slow = RankedMatch::new(&c, &q);
        assert_eq!(fast.is_match(), slow.is_match());
    }

    // Beyond ascii, towlower may map a non ascii candidate char to an ascii
    // query letter (e.g. KELVIN SIGN to 'k') which the precheck does not
    // know about (faithful C++ behaviour), so it can only reject more.
    #[hegel::test(test_cases = 1000)]
    fn letters_precheck_only_rejects(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(candidate());
        let q = tc.draw(query());
        let fast = RankedMatch::with_letters(&c, used_letters(&c), &q, used_letters(&q));
        if fast.is_match() {
            let slow = RankedMatch::new(&c, &q);
            assert!(slow.is_match());
            assert!(!fast.less(&slow) && !slow.less(&fast));
        }
    }

    /// Query and candidates matching it, built by interleaving the query's
    /// characters with filler. NUL bytes are excluded: C++ operator< orders
    /// '/' as codepoint 0, making it equivalent to NUL, which breaks the
    /// strict weak ordering ("\0" ~ "/" ~ "\0a" but "\0" < "\0a"). This is
    /// faithful to the C++ code (checked against it), and only matters for
    /// candidates containing NUL.
    /// Candidates are also valid utf8: invalid sequences decode differently
    /// depending on the following bytes ("\xCB" vs "\xCB\x06" = U+02C6),
    /// which makes the C++ ordering intransitive too ("\x01" < "\xCB" <
    /// "\xCB\x06" < "\x01", checked against the C++ code).
    fn query_and_matches(tc: &TestCase, n: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let no_nul = |v: &Vec<u8>| !v.contains(&0);
        let q = tc.draw(hegel::one_of!(gs::just(Vec::new()), valid_text().filter(|q| q.len() <= 4)));
        let qchars: Vec<&[u8]> = {
            let mut res = Vec::new();
            let mut pos = 0;
            while pos < q.len() {
                let next = crate::utf8::next(&q, pos);
                res.push(&q[pos..next]);
                pos = next;
            }
            res
        };
        let cands = (0..n)
            .map(|_| {
                if q.is_empty() {
                    return tc.draw(valid_text().filter(no_nul));
                }
                let mut c = Vec::new();
                for qc in &qchars {
                    let filler = hegel::one_of!(gs::text().max_size(3), gs::text().alphabet("aAbB/_.-éÉ").max_size(3));
                    c.extend(tc.draw(filler.map(String::into_bytes).filter(no_nul)));
                    c.extend_from_slice(qc);
                }
                c.extend(tc.draw(valid_text().filter(no_nul)));
                c
            })
            .collect();
        (q, cands)
    }

    #[hegel::test(test_cases = 1000)]
    fn interleaved_candidates_match(tc: TestCase) {
        init_test_locale();
        let (q, cands) = query_and_matches(&tc, 1);
        assert!(RankedMatch::new(&cands[0], &q).is_match());
    }

    #[hegel::test(test_cases = 1000)]
    fn ordering_is_irreflexive(tc: TestCase) {
        init_test_locale();
        let (q, c) = query_and_matches(&tc, 1);
        let m = RankedMatch::new(&c[0], &q);
        assert!(!m.less(&m));
    }

    #[hegel::test(test_cases = 1000)]
    fn ordering_is_asymmetric(tc: TestCase) {
        init_test_locale();
        let (q, c) = query_and_matches(&tc, 2);
        let (ma, mb) = (RankedMatch::new(&c[0], &q), RankedMatch::new(&c[1], &q));
        assert!(!(ma.less(&mb) && mb.less(&ma)));
    }

    #[hegel::test(test_cases = 1000)]
    fn ordering_is_transitive(tc: TestCase) {
        init_test_locale();
        let (q, c) = query_and_matches(&tc, 3);
        let (ma, mb, mc) = (RankedMatch::new(&c[0], &q), RankedMatch::new(&c[1], &q), RankedMatch::new(&c[2], &q));
        if ma.less(&mb) && mb.less(&mc) {
            assert!(ma.less(&mc));
        }
    }

    // Strict weak ordering: equivalence (neither is less) is transitive
    #[hegel::test(test_cases = 1000)]
    fn ordering_equivalence_is_transitive(tc: TestCase) {
        init_test_locale();
        let (q, c) = query_and_matches(&tc, 3);
        let (ma, mb, mc) = (RankedMatch::new(&c[0], &q), RankedMatch::new(&c[1], &q), RankedMatch::new(&c[2], &q));
        let equiv = |x: &RankedMatch, y: &RankedMatch| !x.less(y) && !y.less(x);
        if equiv(&ma, &mb) && equiv(&mb, &mc) {
            assert!(equiv(&ma, &mc));
        }
    }

    // Only identical candidates are equivalent
    #[hegel::test(test_cases = 1000)]
    fn equivalent_matches_have_equal_candidates(tc: TestCase) {
        init_test_locale();
        let (q, c) = query_and_matches(&tc, 2);
        let (ma, mb) = (RankedMatch::new(&c[0], &q), RankedMatch::new(&c[1], &q));
        if !ma.less(&mb) && !mb.less(&ma) {
            assert_eq!(c[0], c[1]);
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn sort_orders_matches(tc: TestCase) {
        init_test_locale();
        let n = tc.draw(gs::integers::<usize>().max_value(20));
        let (q, cands) = query_and_matches(&tc, n);
        let mut matches: Vec<RankedMatch> = cands.iter().map(|c| RankedMatch::new(c, &q)).collect();
        sort_matches(&mut matches);
        for w in matches.windows(2) {
            assert!(!w[1].less(&w[0]));
        }
    }

    #[hegel::test(test_cases = 1000)]
    fn input_sequence_number_breaks_ties(tc: TestCase) {
        init_test_locale();
        let (q, c) = query_and_matches(&tc, 1);
        let i = tc.draw(gs::integers::<usize>().max_value(100));
        let j = tc.draw(gs::integers::<usize>().max_value(100));
        let (mut a, mut b) = (RankedMatch::new(&c[0], &q), RankedMatch::new(&c[0], &q));
        a.set_input_sequence_number(i);
        b.set_input_sequence_number(j);
        assert_eq!(a.less(&b), i < j);
    }

    #[hegel::test(test_cases = 1000)]
    fn word_boundary_matches_bounded_by_query(tc: TestCase) {
        init_test_locale();
        let c = tc.draw(candidate());
        let q = tc.draw(query());
        let count = count_word_boundaries_match(&c, &q);
        // each query character (as visited by a utf8 iterator from the start,
        // which may be a continuation byte) is matched at most once
        let mut steps = 0;
        let mut pos = 0;
        while pos < q.len() {
            pos = crate::utf8::next(&q, pos);
            steps += 1;
        }
        assert!(count >= 0 && count <= steps);
    }

    #[hegel::test(test_cases = 1000)]
    fn word_splitter_yields_maximal_ascii_words(tc: TestCase) {
        let text = tc.draw(hegel::one_of!(
            ascii_word(30),
            gs::vecs(gs::sampled_from(vec![&b"a"[..], b"_", b" ", b"-", &[b'b'; 101]])).max_size(6).map(|v| v.concat())
        ));
        let extra: &[Codepoint] = &['_' as u32];
        let words: Vec<&[u8]> = WordSplitter::new(&text, extra).collect();
        let expected: Vec<&[u8]> = text
            .split(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
            .filter(|w| !w.is_empty() && w.len() <= MAX_WORD_LEN)
            .collect();
        assert_eq!(words, expected);
    }

    #[hegel::test(test_cases = 1000)]
    fn used_letters_records_each_byte_class(tc: TestCase) {
        let s = tc.draw(gs::binary());
        let letters = used_letters(&s);
        for c in b'a'..=b'z' {
            assert_eq!(letters & (1 << (c - b'a')) != 0, s.contains(&c));
            assert_eq!(letters & (1 << (c - b'a' + 26)) != 0, s.contains(&c.to_ascii_uppercase()));
        }
        assert_eq!(letters & (1 << 53) != 0, s.contains(&b'_'));
        assert_eq!(letters & (1 << 54) != 0, s.contains(&b'-'));
        assert_eq!(letters & (1 << 63) != 0, s.iter().any(|c| !c.is_ascii_alphabetic() && !b"_-".contains(c)));
        assert_eq!(letters_to_lower(letters) & UPPER_MASK, 0);
        assert_eq!(letters_to_lower(letters), used_letters(&s.to_ascii_lowercase()));
    }
}
