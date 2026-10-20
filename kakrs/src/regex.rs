//! Port of regex.hh / regex.cc

use crate::exception::Result;
use crate::regex_vm::{
    BackwardSearch, CompiledRegex, ForwardMode, ForwardSearch, Mode, RegexCompileFlags, RegexExecFlags, RegexMode,
    ThreadedRegexVM, VmMode, compile_regex,
};
use crate::string_utils::{Quoting, quoter};
use crate::utf8::{ByteIt, SliceIt};
use std::ops::ControlFlow;
use std::rc::Rc;

/// Regex that keeps track of its string representation
#[derive(Clone, Default)]
pub struct Regex {
    impl_: Option<Rc<CompiledRegex>>,
    str: Vec<u8>,
}

impl Regex {
    pub const OPTION_TYPE_NAME: &'static str = "regex";

    pub fn new(re: &[u8], flags: RegexCompileFlags) -> Result<Regex> {
        Ok(Regex { impl_: Some(Rc::new(compile_regex(re, flags)?)), str: re.to_vec() })
    }

    pub fn empty(&self) -> bool {
        self.str.is_empty()
    }

    pub fn str(&self) -> &[u8] {
        &self.str
    }

    pub fn mark_count(&self) -> usize {
        self.impl_.as_ref().map_or(0, |i| (i.save_count / 2) as usize - 1)
    }

    pub fn named_capture_index(&self, name: &[u8]) -> i32 {
        if let Some(impl_) = &self.impl_ {
            for capture in &impl_.named_captures {
                if capture.name == name {
                    return capture.index as i32;
                }
            }
        }
        -1
    }

    /// The compiled program, panics on a default constructed Regex (which
    /// is a null pointer in the C++ code).
    pub fn impl_(&self) -> &CompiledRegex {
        self.impl_.as_deref().expect("regex has no compiled program")
    }

    /// The compiled program, None for a default constructed Regex
    pub fn impl_opt(&self) -> Option<&CompiledRegex> {
        self.impl_.as_deref()
    }
}

impl PartialEq for Regex {
    fn eq(&self, other: &Self) -> bool {
        self.str == other.str
    }
}

impl Eq for Regex {}

impl std::fmt::Debug for Regex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Regex({:?})", String::from_utf8_lossy(&self.str))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubMatch<I> {
    pub first: Option<I>,
    pub second: Option<I>,
    pub matched: bool,
}

impl<I> SubMatch<I> {
    pub fn new(first: Option<I>, second: Option<I>) -> Self {
        let matched = first.is_some();
        SubMatch { first, second, matched }
    }
}

impl<I> Default for SubMatch<I> {
    fn default() -> Self {
        SubMatch { first: None, second: None, matched: false }
    }
}

#[derive(Clone, Debug)]
pub struct MatchResults<I> {
    values: Vec<SubMatch<I>>,
    none: SubMatch<I>,
}

impl<I> Default for MatchResults<I> {
    fn default() -> Self {
        MatchResults { values: Vec::new(), none: SubMatch::default() }
    }
}

impl<I: Clone> MatchResults<I> {
    /// Build from a flat list of captures (begin, end, begin, end...)
    pub fn from_captures(captures: &[Option<I>]) -> Self {
        let mut res = MatchResults::default();
        res.set_captures(captures);
        res
    }

    pub fn set_captures(&mut self, captures: &[Option<I>]) {
        self.values.clear();
        self.values.extend(captures.as_chunks::<2>().0.iter().map(|[b, e]| SubMatch::new(b.clone(), e.clone())));
    }
}

impl<I> MatchResults<I> {
    pub fn size(&self) -> usize {
        self.values.len()
    }

    pub fn empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn iter(&self) -> std::slice::Iter<'_, SubMatch<I>> {
        self.values.iter()
    }

    pub fn swap(&mut self, other: &mut MatchResults<I>) {
        std::mem::swap(&mut self.values, &mut other.values);
    }

    pub fn values(&self) -> &[SubMatch<I>] {
        &self.values
    }
}

impl<I> std::ops::Index<usize> for MatchResults<I> {
    type Output = SubMatch<I>;
    fn index(&self, i: usize) -> &SubMatch<I> {
        self.values.get(i).unwrap_or(&self.none)
    }
}

impl<I: PartialEq> PartialEq for MatchResults<I> {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}

impl<'a, I> IntoIterator for &'a MatchResults<I> {
    type Item = &'a SubMatch<I>;
    type IntoIter = std::slice::Iter<'a, SubMatch<I>>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

pub fn match_flags(bol: bool, eol: bool, bow: bool, eow: bool) -> RegexExecFlags {
    (if bol { RegexExecFlags::NONE } else { RegexExecFlags::NOT_BEGIN_OF_LINE })
        | (if eol { RegexExecFlags::NONE } else { RegexExecFlags::NOT_END_OF_LINE })
        | (if bow { RegexExecFlags::NONE } else { RegexExecFlags::NOT_BEGIN_OF_WORD })
        | (if eow { RegexExecFlags::NONE } else { RegexExecFlags::NOT_END_OF_WORD })
}

/// NoopIdle
pub fn noop_idle() -> ControlFlow<()> {
    ControlFlow::Continue(())
}

pub type MatchNoSaves = Mode<{ RegexMode::FORWARD | RegexMode::ANY_MATCH | RegexMode::NO_SAVES }>;
pub type SearchNoSaves = Mode<{ RegexMode::FORWARD | RegexMode::SEARCH | RegexMode::ANY_MATCH | RegexMode::NO_SAVES }>;

pub fn regex_match<I: ByteIt>(begin: I, end: I, re: &Regex, idle_func: impl FnMut() -> ControlFlow<()>) -> bool {
    let mut vm = ThreadedRegexVM::<I, MatchNoSaves>::new(re.impl_());
    vm.exec(&begin, &end, &begin, &end, RegexExecFlags::NONE, idle_func)
}

pub fn regex_match_captures<I: ByteIt>(
    begin: I,
    end: I,
    res: &mut MatchResults<I>,
    re: &Regex,
    idle_func: impl FnMut() -> ControlFlow<()>,
) -> bool {
    res.clear();
    let mut vm = ThreadedRegexVM::<I, ForwardMode>::new(re.impl_());
    if vm.exec(&begin, &end, &begin, &end, RegexExecFlags::NONE, idle_func) {
        res.set_captures(vm.captures());
        return true;
    }
    false
}

pub fn regex_search<I: ByteIt>(
    begin: I,
    end: I,
    subject_begin: I,
    subject_end: I,
    re: &Regex,
    flags: RegexExecFlags,
    idle_func: impl FnMut() -> ControlFlow<()>,
) -> bool {
    let mut vm = ThreadedRegexVM::<I, SearchNoSaves>::new(re.impl_());
    vm.exec(&begin, &end, &subject_begin, &subject_end, flags, idle_func)
}

/// regex_search with captures, M must be a search mode
/// (ForwardSearch or BackwardSearch)
pub fn regex_search_mode<I: ByteIt, M: VmMode>(
    begin: I,
    end: I,
    subject_begin: I,
    subject_end: I,
    res: &mut MatchResults<I>,
    re: &Regex,
    flags: RegexExecFlags,
    idle_func: impl FnMut() -> ControlFlow<()>,
) -> bool {
    const { assert!(M::SEARCH) };
    res.clear();
    let mut vm = ThreadedRegexVM::<I, M>::new(re.impl_());
    if vm.exec(&begin, &end, &subject_begin, &subject_end, flags, idle_func) {
        res.set_captures(vm.captures());
        return true;
    }
    false
}

pub fn regex_search_captures<I: ByteIt>(
    begin: I,
    end: I,
    subject_begin: I,
    subject_end: I,
    res: &mut MatchResults<I>,
    re: &Regex,
    flags: RegexExecFlags,
    idle_func: impl FnMut() -> ControlFlow<()>,
) -> bool {
    regex_search_mode::<I, ForwardSearch>(begin, end, subject_begin, subject_end, res, re, flags, idle_func)
}

pub fn backward_regex_search<I: ByteIt>(
    begin: I,
    end: I,
    subject_begin: I,
    subject_end: I,
    res: &mut MatchResults<I>,
    re: &Regex,
    flags: RegexExecFlags,
    idle_func: impl FnMut() -> ControlFlow<()>,
) -> bool {
    regex_search_mode::<I, BackwardSearch>(begin, end, subject_begin, subject_end, res, re, flags, idle_func)
}

pub fn option_to_string(re: &Regex, quoting: Quoting) -> Vec<u8> {
    quoter(quoting)(re.str())
}

pub fn option_from_string(str: &[u8]) -> Result<Regex> {
    Regex::new(str, RegexCompileFlags::NONE)
}

enum VmHolder<'a, 'p, I: ByteIt, M: VmMode> {
    Owned(ThreadedRegexVM<'p, I, M>),
    Borrowed(&'a mut ThreadedRegexVM<'p, I, M>),
}

impl<'p, I: ByteIt, M: VmMode> VmHolder<'_, 'p, I, M> {
    fn vm(&mut self) -> &mut ThreadedRegexVM<'p, I, M> {
        match self {
            VmHolder::Owned(vm) => vm,
            VmHolder::Borrowed(vm) => vm,
        }
    }
}

/// Iterates over the successive matches of a regex. M is the search mode of
/// the underlying VM (ForwardSearch or BackwardSearch), which is the C++
/// `mode | RegexMode::Search`.
///
/// Use `next_match` to access each match without copying it, or the
/// Iterator implementation that yields copies.
pub struct RegexIterator<'a, 'p, I: ByteIt, M: VmMode = ForwardSearch> {
    vm: VmHolder<'a, 'p, I, M>,
    results: MatchResults<I>,
    next_pos: I,
    begin: I,
    end: I,
    subject_begin: I,
    subject_end: I,
    flags: RegexExecFlags,
    idle_func: Option<&'a mut dyn FnMut() -> ControlFlow<()>>,
}

impl<'a, 'p, I: ByteIt, M: VmMode> RegexIterator<'a, 'p, I, M> {
    fn make(
        vm: VmHolder<'a, 'p, I, M>,
        begin: I,
        end: I,
        subject_begin: I,
        subject_end: I,
        flags: RegexExecFlags,
    ) -> Self {
        const { assert!(M::SEARCH && crate::regex_vm::has_direction(M::BITS)) };
        RegexIterator {
            vm,
            results: MatchResults::default(),
            next_pos: if M::FORWARD { begin.clone() } else { end.clone() },
            begin,
            end,
            subject_begin,
            subject_end,
            flags,
            idle_func: None,
        }
    }

    pub fn new(
        begin: I,
        end: I,
        subject_begin: I,
        subject_end: I,
        regex: &'p Regex,
        flags: RegexExecFlags,
    ) -> Self {
        Self::make(VmHolder::Owned(ThreadedRegexVM::new(regex.impl_())), begin, end, subject_begin, subject_end, flags)
    }

    /// Iterate over [begin, end) using it as the subject as well
    pub fn new_range(begin: I, end: I, regex: &'p Regex, flags: RegexExecFlags) -> Self {
        Self::new(begin.clone(), end.clone(), begin, end, regex, flags)
    }

    pub fn with_vm(
        begin: I,
        end: I,
        subject_begin: I,
        subject_end: I,
        vm: &'a mut ThreadedRegexVM<'p, I, M>,
        flags: RegexExecFlags,
    ) -> Self {
        Self::make(VmHolder::Borrowed(vm), begin, end, subject_begin, subject_end, flags)
    }

    pub fn with_vm_range(begin: I, end: I, vm: &'a mut ThreadedRegexVM<'p, I, M>, flags: RegexExecFlags) -> Self {
        Self::with_vm(begin.clone(), end.clone(), begin, end, vm, flags)
    }

    /// Set the function called periodically during long matches
    pub fn with_idle(mut self, idle_func: &'a mut dyn FnMut() -> ControlFlow<()>) -> Self {
        self.idle_func = Some(idle_func);
        self
    }

    /// Advance to the next match, returning it
    pub fn next_match(&mut self) -> Option<&MatchResults<I>> {
        let mut additional_flags = RegexExecFlags::NONE;
        if self.results.size() != 0 && self.results[0].first == self.results[0].second {
            additional_flags |= RegexExecFlags::NOT_INITIAL_NULL;
        }

        let idle_func = &mut self.idle_func;
        let idle = || match idle_func {
            Some(f) => f(),
            None => ControlFlow::Continue(()),
        };
        let (begin, end) =
            if M::FORWARD { (&self.next_pos, &self.end) } else { (&self.begin, &self.next_pos) };
        let vm = self.vm.vm();
        if !vm.exec(begin, end, &self.subject_begin, &self.subject_end, self.flags | additional_flags, idle) {
            return None;
        }

        self.results.set_captures(vm.captures());
        let sub = &self.results[0];
        self.next_pos = if M::FORWARD { sub.second.clone() } else { sub.first.clone() }
            .expect("regex match without a main capture");
        debug_assert!(if M::FORWARD { self.next_pos <= self.end } else { self.next_pos >= self.begin });
        Some(&self.results)
    }
}

impl<I: ByteIt, M: VmMode> Iterator for RegexIterator<'_, '_, I, M> {
    type Item = MatchResults<I>;
    fn next(&mut self) -> Option<MatchResults<I>> {
        self.next_match().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utf8::SliceIt;

    fn it(s: &[u8], pos: usize) -> SliceIt<'_> {
        SliceIt::new(s, pos)
    }

    fn sub<'s>(m: &SubMatch<SliceIt<'s>>) -> &'s [u8] {
        let (b, e) = (m.first.unwrap(), m.second.unwrap());
        &b.s[b.pos..e.pos]
    }

    #[test]
    fn regex_api() {
        let re = Regex::new(br"(?<word>\w+)(-)?", RegexCompileFlags::NONE).unwrap();
        assert_eq!(re.mark_count(), 2);
        assert_eq!(re.named_capture_index(b"word"), 1);
        assert_eq!(re.named_capture_index(b"nope"), -1);
        assert_eq!(re, Regex::new(br"(?<word>\w+)(-)?", RegexCompileFlags::OPTIMIZE).unwrap());
        assert!(Regex::default().empty());
        assert_eq!(option_to_string(&re, Quoting::Kakoune), br"'(?<word>\w+)(-)?'");
        assert!(option_from_string(b"(").is_err());

        let s = b"foo-bar baz";
        assert!(regex_match(it(s, 0), it(s, 3), &re, noop_idle));
        assert!(!regex_match(it(s, 0), it(s, 5), &re, noop_idle));
        let mut res = MatchResults::default();
        assert!(regex_match_captures(it(s, 0), it(s, 4), &mut res, &re, noop_idle));
        assert_eq!(res.size(), 3);
        assert_eq!(sub(&res[0]), b"foo-");
        assert_eq!(sub(&res[1]), b"foo");
        assert!(res[2].matched);
        assert!(!res[3].matched);

        let end = it(s, s.len());
        assert!(regex_search(it(s, 0), end, it(s, 0), end, &re, RegexExecFlags::NONE, noop_idle));
        assert!(regex_search_captures(it(s, 4), end, it(s, 0), end, &mut res, &re, RegexExecFlags::NONE, noop_idle));
        assert_eq!(sub(&res[0]), b"bar");
        assert!(!res[2].matched);
        assert!(backward_regex_search(
            it(s, 0),
            it(s, 7),
            it(s, 0),
            end,
            &mut res,
            &Regex::new(br"\w+", RegexCompileFlags::BACKWARD).unwrap(),
            RegexExecFlags::NONE,
            noop_idle
        ));
        assert_eq!(sub(&res[0]), b"bar");
    }

    #[test]
    fn regex_iterator() {
        let s = b"foo bar  baz";
        let (b, e) = (it(s, 0), it(s, s.len()));
        let re = Regex::new(br"\w+|(?=b)", RegexCompileFlags::BACKWARD).unwrap();
        let fwd: Vec<&[u8]> = RegexIterator::<_, ForwardSearch>::new_range(b, e, &re, RegexExecFlags::NONE)
            .map(|m| sub(&m[0]))
            .collect();
        assert_eq!(fwd, vec![&b"foo"[..], b"bar", b"baz"]);

        let bwd: Vec<&[u8]> = RegexIterator::<_, BackwardSearch>::new_range(b, e, &re, RegexExecFlags::NONE)
            .map(|m| sub(&m[0]))
            .collect();
        assert_eq!(bwd, vec![&b"baz"[..], b"", b"bar", b"", b"foo"]);

        let mut vm = ThreadedRegexVM::<_, ForwardSearch>::new(re.impl_());
        let mut idle = noop_idle;
        let mut count = 0;
        let mut iter = RegexIterator::with_vm_range(b, e, &mut vm, RegexExecFlags::NONE).with_idle(&mut idle);
        while let Some(m) = iter.next_match() {
            assert!(m[0].matched);
            count += 1;
        }
        assert_eq!(count, 3);
        // the vm can be reused
        assert_eq!(RegexIterator::with_vm_range(b, it(s, 3), &mut vm, RegexExecFlags::NONE).count(), 1);
    }
}

// Convenience helpers for matching whole byte strings ------------------------

/// regex_match over a whole byte string
pub fn regex_match_str(s: &[u8], re: &Regex) -> bool {
    regex_match(SliceIt::new(s, 0), SliceIt::new(s, s.len()), re, noop_idle)
}

/// regex_match over a whole byte string, returning capture byte ranges
pub fn regex_match_ranges(s: &[u8], re: &Regex) -> Option<Vec<Option<(usize, usize)>>> {
    let mut res = MatchResults::default();
    if !regex_match_captures(SliceIt::new(s, 0), SliceIt::new(s, s.len()), &mut res, re, noop_idle) {
        return None;
    }
    Some(submatch_ranges(&res))
}

/// Convert slice match results to optional byte ranges
pub fn submatch_ranges(res: &MatchResults<SliceIt<'_>>) -> Vec<Option<(usize, usize)>> {
    res.iter()
        .map(|m| match (&m.first, &m.second) {
            (Some(b), Some(e)) if m.matched => Some((b.pos, e.pos)),
            _ => None,
        })
        .collect()
}

impl Regex {
    /// (name, capture index) of named captures
    pub fn named_captures(&self) -> Vec<(Vec<u8>, usize)> {
        match &self.impl_ {
            Some(i) => i.named_captures.iter().map(|c| (c.name.clone(), c.index as usize)).collect(),
            None => Vec::new(),
        }
    }
}

impl crate::option_types::OptionElem for Regex {
    fn elem_type_name() -> Vec<u8> {
        b"regex".to_vec()
    }
    fn elem_to_string(&self, quoting: Quoting) -> Vec<u8> {
        option_to_string(self, quoting)
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        option_from_string(s)
    }
}
crate::single_value_option!(Regex);
