//! Port of completion.hh/cc

use crate::context::Context;
use crate::file::{is_dir_mode, is_reg_mode, list_files, parse_filename, split_path, st_mtim, Timespec};
use crate::ranked_match::{sort_matches, RankedMatch};
use crate::regex::Regex;
use crate::unicode::is_horizontal_blank;
use std::cell::RefCell;
use std::collections::HashMap;

pub type CandidateList = Vec<Vec<u8>>;

crate::kak_flags! {
    pub struct CompletionFlags: u32 {
        const None = 0;
        const Quoted = 0b1;
        const Menu = 0b10;
        const NoEmpty = 0b100;
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Completions {
    pub candidates: CandidateList,
    pub start: usize,
    pub end: usize,
    pub flags: CompletionFlags,
}

impl Completions {
    pub fn new(start: usize, end: usize) -> Self {
        Completions { candidates: Vec::new(), start, end, flags: CompletionFlags::None }
    }
    pub fn with(start: usize, end: usize, candidates: CandidateList, flags: CompletionFlags) -> Self {
        Completions { candidates, start, end, flags }
    }
}

pub fn complete_nothing(_: &Context, _: &[u8], cursor_pos: usize) -> Completions {
    Completions::new(cursor_pos, cursor_pos)
}

pub fn offset_pos(completion: Completions, offset: usize) -> Completions {
    Completions {
        start: completion.start + offset,
        end: completion.end + offset,
        candidates: completion.candidates,
        flags: completion.flags,
    }
}

/// Port of the complete() template: rank candidates matching query[..cursor_pos]
pub fn complete<I, S>(query: &[u8], cursor_pos: usize, container: I) -> CandidateList
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u8]>,
{
    let query = &query[..cursor_pos.min(query.len())];
    let items: Vec<S> = container.into_iter().collect();
    let mut matches: Vec<RankedMatch> = Vec::new();
    for s in &items {
        let m = RankedMatch::new(s.as_ref(), query);
        if m.is_match() {
            matches.push(m);
        }
    }
    sort_matches(&mut matches);
    matches.iter().map(|m| m.candidate().to_vec()).collect()
}

crate::kak_flags! {
    pub struct FilenameFlags: u32 {
        const None = 0;
        const OnlyDirectories = 1 << 0;
        const Expand = 1 << 1;
    }
}

fn candidates(matches: &[RankedMatch], dirname: &[u8]) -> CandidateList {
    matches
        .iter()
        .map(|m| {
            let mut s = dirname.to_vec();
            s.extend_from_slice(m.candidate());
            s
        })
        .collect()
}

pub fn complete_filename(prefix: &[u8], ignored_regex: &Regex, cursor_pos: usize, flags: FilenameFlags) -> CandidateList {
    let prefix = &prefix[..cursor_pos.min(prefix.len())];
    let (dirname, fileprefix) = split_path(prefix);
    let parsed_dirname = parse_filename(dirname, b"");

    let mut use_ignore = !ignored_regex.empty();
    if use_ignore && crate::regex::regex_match_str(fileprefix, ignored_regex) {
        use_ignore = false;
    }

    let only_dirs = flags.has(FilenameFlags::OnlyDirectories);
    let mut files: Vec<Vec<u8>> = Vec::new();
    list_files(&parsed_dirname, &mut |filename, st| {
        if (!use_ignore || !crate::regex::regex_match_str(filename, ignored_regex))
            && (!only_dirs || is_dir_mode(st.st_mode))
        {
            files.push(filename.to_vec());
        }
    });
    let mut matches: Vec<RankedMatch> = Vec::new();
    for file in &files {
        let m = RankedMatch::new(file, fileprefix);
        if m.is_match() {
            matches.push(m);
        }
    }
    // Hack: when completing directories, also echo back the query if it
    // is a valid directory. This enables menu completion to select the
    // directory instead of a child.
    if only_dirs && !dirname.is_empty() && dirname.last() == Some(&b'/') && fileprefix.is_empty() && !files.is_empty() {
        matches.push(RankedMatch::new(fileprefix, fileprefix));
    }
    sort_matches(&mut matches);
    let expand = flags.has(FilenameFlags::Expand);
    candidates(&matches, if expand { &parsed_dirname } else { dirname })
}

struct CommandCache {
    mtim: Timespec,
    commands: Vec<Vec<u8>>,
}

thread_local! {
    static COMMAND_CACHE: RefCell<HashMap<Vec<u8>, CommandCache>> = RefCell::new(HashMap::new());
}

fn is_executable(st: &libc::stat) -> bool {
    (st.st_mode & (libc::S_IXUSR | libc::S_IXGRP | libc::S_IXOTH)) != 0
}

pub fn complete_command(prefix: &[u8], cursor_pos: usize) -> CandidateList {
    let real_prefix = parse_filename(&prefix[..cursor_pos.min(prefix.len())], b"");
    let (dirname, fileprefix) = split_path(&real_prefix);

    if !dirname.is_empty() {
        let mut files: Vec<Vec<u8>> = Vec::new();
        list_files(dirname, &mut |filename, st| {
            if is_dir_mode(st.st_mode) || (is_reg_mode(st.st_mode) && is_executable(st)) {
                files.push(filename.to_vec());
            }
        });
        let mut matches: Vec<RankedMatch> = Vec::new();
        for file in &files {
            let m = RankedMatch::new(file, fileprefix);
            if m.is_match() {
                matches.push(m);
            }
        }
        sort_matches(&mut matches);
        return candidates(&matches, dirname);
    }

    let path = crate::file::getenv("PATH").unwrap_or_default();
    let mut all_commands: Vec<Vec<u8>> = Vec::new();
    COMMAND_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        for dir in crate::string_utils::split(&path, b':') {
            let dirname = if dir.last() == Some(&b'/') { &dir[..dir.len() - 1] } else { dir }.to_vec();
            let c = crate::file::cstr(&dirname);
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::stat(c.as_ptr(), &mut st) } != 0 {
                continue;
            }
            let entry = cache
                .entry(dirname.clone())
                .or_insert_with(|| CommandCache { mtim: Timespec { tv_sec: 0, tv_nsec: 0 }, commands: Vec::new() });
            let mtim = st_mtim(&st);
            if entry.mtim != mtim {
                entry.commands.clear();
                let commands = &mut entry.commands;
                list_files(&dirname, &mut |filename, st| {
                    if is_reg_mode(st.st_mode) && is_executable(st) {
                        commands.push(filename.to_vec());
                    }
                });
                entry.mtim = mtim;
            }
            all_commands.extend(entry.commands.iter().cloned());
        }
    });
    let mut matches: Vec<RankedMatch> = Vec::new();
    for cmd in &all_commands {
        let m = RankedMatch::new(cmd, fileprefix);
        if m.is_match() {
            matches.push(m);
        }
    }
    sort_matches(&mut matches);
    matches.dedup_by(|a, b| a.candidate() == b.candidate());
    candidates(&matches, b"")
}

pub fn shell_complete(context: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    let mut word_start = 0;
    let mut word_end = 0;
    let mut command = true;
    let len = prefix.len();
    let mut pos = 0;
    while pos < cursor_pos && pos < len {
        command = pos == 0
            || prefix[pos - 1] == b';'
            || prefix[pos - 1] == b'|'
            || (pos > 1 && prefix[pos - 1] == b'&' && prefix[pos - 2] == b'&');
        while pos != len && is_horizontal_blank(prefix[pos] as u32) {
            pos += 1;
        }
        word_start = pos;
        while pos != len && !is_horizontal_blank(prefix[pos] as u32) {
            pos += 1;
        }
        word_end = pos;
    }
    let mut completions = Completions::new(word_start, word_end);
    let word = crate::string_utils::substr(prefix, word_start, word_end as isize);
    if command {
        completions.candidates = complete_command(word, cursor_pos - word_start);
    } else {
        let ignored = context.options().opt("ignored_files");
        let ignored = ignored.get::<Regex>().clone();
        completions.candidates = complete_filename(word, &ignored, cursor_pos - word_start, FilenameFlags::None);
    }
    completions
}
