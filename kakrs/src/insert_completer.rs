//! Port of insert_completer.hh/cc

use crate::buffer::{Buffer, BufferFlags, BufferIterator};
use crate::buffer_manager::BufferManager;
use crate::buffer_utils::{get_column, replace_ranges};
use crate::changes::{update_ranges, UpdatableRange};
use crate::command_manager::{CommandManager, ShellContext};
use crate::completion::{complete_filename as complete_filename_impl, FilenameFlags};
use crate::debug::write_to_debug_buffer;
use crate::display_buffer::{parse_display_line, DisplayAtom, DisplayLine, DisplayLineBuiltins};
use crate::exception::Result;
use crate::face::{Face, FaceRegistry};
use crate::file::{parse_filename, real_path, split_path};
use crate::hook_manager::Hook;
use crate::input_handler::InputHandler;
use crate::keys::{named, Key};
use crate::option_manager::{Opt, OptionManager, OptionWatcher};
use crate::option_types::{CodepointOpt, OptionElem};
use crate::ranked_match::{used_letters, RankedMatch, UsedLetters, MAX_WORD_LEN};
use crate::regex::{regex_match_ranges, Regex};
use crate::regex_vm::RegexCompileFlags;
use crate::selection::{selection_to_string, ColumnType, Selection, SelectionList};
use crate::string_utils::{column_length, expand_tabs, join, str_to_int, string_of_cp, substr_columns, Quoting};
use crate::unicode::{is_horizontal_blank, is_word};
use crate::units::{BufferCoord, BufferRange, ByteCount, CharCount, Codepoint, ColumnCount};
use crate::user_interface::MenuStyle;
use crate::utf8::{self, Utf8Iter};
use crate::word_db::{for_n_best, get_word_db};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub use crate::option_types::{CompletionCandidate, CompletionList};

type StringList = Vec<Vec<u8>>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InsertCompleterMode {
    Word,
    Option,
    Filename,
    Line,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InsertCompleterDesc {
    pub mode: InsertCompleterMode,
    pub param: Option<Vec<u8>>,
}

pub type InsertCompleterDescList = Vec<InsertCompleterDesc>;

pub fn option_to_string(opt: &InsertCompleterDesc) -> Vec<u8> {
    let param = |prefix: &[u8]| {
        let mut res = prefix.to_vec();
        res.extend_from_slice(opt.param.as_deref().unwrap_or(b""));
        res
    };
    match opt.mode {
        InsertCompleterMode::Word => param(b"word="),
        InsertCompleterMode::Filename => b"filename".to_vec(),
        InsertCompleterMode::Option => param(b"option="),
        InsertCompleterMode::Line => param(b"line="),
    }
}

#[allow(clippy::collapsible_if)]
pub fn option_from_string(s: &[u8]) -> Result<InsertCompleterDesc> {
    if s.starts_with(b"option=") {
        return Ok(InsertCompleterDesc { mode: InsertCompleterMode::Option, param: Some(s[7..].to_vec()) });
    } else if let Some(param) = s.strip_prefix(b"word=") {
        if param == b"all" || param == b"buffer" {
            return Ok(InsertCompleterDesc { mode: InsertCompleterMode::Word, param: Some(param.to_vec()) });
        }
    } else if s == b"filename" {
        return Ok(InsertCompleterDesc { mode: InsertCompleterMode::Filename, param: None });
    } else if let Some(param) = s.strip_prefix(b"line=") {
        if param == b"all" || param == b"buffer" {
            return Ok(InsertCompleterDesc { mode: InsertCompleterMode::Line, param: Some(param.to_vec()) });
        }
    }
    Err(rt_err!("invalid completer description: '{}'", s))
}

impl OptionElem for InsertCompleterDesc {
    fn elem_type_name() -> Vec<u8> {
        b"completer".to_vec()
    }
    // quoting is ignored, as in the C++ generic option_to_string(value, Quoting)
    fn elem_to_string(&self, _: Quoting) -> Vec<u8> {
        option_to_string(self)
    }
    fn elem_from_string(s: &[u8]) -> Result<Self> {
        option_from_string(s)
    }
}
crate::single_value_option!(InsertCompleterDesc);

/// InsertCompletion::Candidate
#[derive(Clone, Debug, Default)]
pub struct InsertCompletionCandidate {
    pub completion: Vec<u8>,
    pub on_select: Vec<u8>,
    pub menu_entry: DisplayLine,
}

pub type InsertCompletionCandidateList = Vec<InsertCompletionCandidate>;

#[derive(Clone, Debug, Default)]
pub struct InsertCompletion {
    pub candidates: InsertCompletionCandidateList,
    pub begin: BufferCoord,
    pub end: BufferCoord,
    pub timestamp: usize,
}

impl InsertCompletion {
    pub fn is_valid(&self) -> bool {
        !self.candidates.is_empty()
    }
}

// get_first/get_last for BufferRange, used by update_ranges
impl UpdatableRange for BufferRange {
    fn first_last(&mut self) -> (&mut BufferCoord, &mut BufferCoord) {
        (&mut self.begin, &mut self.end)
    }
    fn last(&self) -> BufferCoord {
        self.end
    }
}

type BufferUtf8It<'a> = Utf8Iter<BufferIterator<'a>>;

/// Port of skip_while (utils.hh)
fn skip_while(it: &mut BufferUtf8It, mut condition: impl FnMut(Codepoint) -> bool) -> bool {
    while !it.at_end() && condition(it.get()) {
        it.inc();
    }
    !it.at_end()
}

/// Port of skip_while_reverse (utils.hh), note that condition gets evaluated
/// once more on the final position.
fn skip_while_reverse(it: &mut BufferUtf8It, mut condition: impl FnMut(Codepoint) -> bool) -> bool {
    while !it.at_begin() && condition(it.get()) {
        it.dec();
    }
    condition(it.get())
}

/// Port of unordered_erase (ranges.hh): erase the first element equal to value
fn unordered_erase<T>(vec: &mut Vec<T>, eq: impl Fn(&T) -> bool) {
    if let Some(pos) = vec.iter().position(eq) {
        vec.swap_remove(pos);
    }
}

type CompleteFunc = fn(&SelectionList, &OptionManager, &FaceRegistry) -> Result<InsertCompletion>;

fn extra_word_chars(options: &OptionManager) -> Result<Vec<Codepoint>> {
    Ok(options.get(b"extra_word_chars")?.try_get::<Vec<CodepointOpt>>()?.iter().map(|c| c.0).collect())
}

fn complete_word<const OTHER_BUFFERS: bool>(
    sels: &SelectionList,
    options: &OptionManager,
    faces: &FaceRegistry,
) -> Result<InsertCompletion> {
    let extra_word_chars = extra_word_chars(options)?;
    let is_word_pred = |c: Codepoint| is_word(c, &extra_word_chars);

    let buffer = sels.buffer();
    let cursor_pos = sels.main().cursor().coord;

    let mut word_begin = BufferCoord::default();
    let mut prefix: Vec<u8> = Vec::new();
    // vector of (word, count) pairs, in insertion order
    let mut sel_word_counts: Vec<(Vec<u8>, i32)> = Vec::new();
    {
        let lines = buffer.lines();
        let utf8_it = |coord: BufferCoord| {
            Utf8Iter::new(BufferIterator::new(&lines, coord), BufferIterator::begin(&lines), BufferIterator::end(&lines))
        };

        let pos = utf8_it(cursor_pos);
        if pos.at_begin() || !is_word_pred(pos.sub(1).get()) {
            return Ok(InsertCompletion::default());
        }

        for i in 0..sels.size() {
            let len = Cell::new(0usize);
            let is_short_enough_word = |c: Codepoint| {
                let l = len.get();
                len.set(l + 1);
                l < MAX_WORD_LEN && is_word_pred(c)
            };

            let mut end = utf8_it(sels[i].cursor().coord);
            let mut begin = end.sub(1);
            if !skip_while_reverse(&mut begin, is_short_enough_word) && begin < end {
                // (begin might == end if end == buffer.begin())
                begin.inc();
            }

            if i == sels.main_index() {
                word_begin = begin.base().coord();
                prefix = buffer.substr(word_begin, end.base().coord());
            }

            skip_while(&mut end, is_short_enough_word);

            if len.get() <= MAX_WORD_LEN {
                let word = buffer.substr(begin.base().coord(), end.base().coord());
                match sel_word_counts.iter_mut().find(|(w, _)| *w == word) {
                    Some((_, count)) => *count += 1,
                    None => sel_word_counts.push((word, 1)),
                }
            }
        }
    }

    // RankedMatchAndBuffer, the ranked match gets computed once all
    // candidates are gathered, as RankedMatch borrows its candidate.
    struct MatchAndBuffer {
        word: Rc<[u8]>,
        letters: Option<UsedLetters>,
        buffer: Option<Rc<Buffer>>,
    }

    let word_db = get_word_db(buffer);
    let mut matches: Vec<MatchAndBuffer> = word_db
        .find_matching(&prefix)
        .into_iter()
        .map(|(word, info)| MatchAndBuffer { word, letters: Some(info.letters), buffer: Some(buffer.clone()) })
        .collect();
    // Remove words that are being edited
    for (word, count) in &sel_word_counts {
        if word_db.get_word_occurences(word) <= *count {
            unordered_erase(&mut matches, |m| *m.word == **word);
        }
    }

    if OTHER_BUFFERS {
        for buf in BufferManager::instance().buffers() {
            if Rc::ptr_eq(&buf, buffer) || buf.flags().has(BufferFlags::Debug) {
                continue;
            }
            for (word, info) in get_word_db(&buf).find_matching(&prefix) {
                // filter out words that are not considered words for the current buffer
                if !utf8::codepoints(&word).all(is_word_pred) {
                    continue;
                }
                matches.push(MatchAndBuffer { word, letters: Some(info.letters), buffer: Some(buf.clone()) });
            }
        }
    }

    for word in options.get(b"static_words")?.try_get::<StringList>()?.iter() {
        if RankedMatch::new(word, &prefix).is_match() {
            matches.push(MatchAndBuffer { word: Rc::from(word.as_slice()), letters: None, buffer: None });
        }
    }

    unordered_erase(&mut matches, |m| *m.word == *prefix);
    let longest: CharCount = matches.iter().map(|m| utf8::distance(&m.word)).max().unwrap_or(0).max(0);

    let limit = |s: &[u8], l: ColumnCount| -> Vec<u8> {
        if column_length(s) <= l {
            s.to_vec()
        } else {
            let mut res = "…".as_bytes().to_vec();
            res.extend_from_slice(substr_columns(s, column_length(s) - (l + 1), -1));
            res
        }
    };

    let prefix_letters = used_letters(&prefix);
    let ranked: Vec<(RankedMatch, &MatchAndBuffer)> = matches
        .iter()
        .map(|m| {
            let rm = match m.letters {
                Some(letters) => RankedMatch::with_letters(&m.word, letters, &prefix, prefix_letters),
                None => RankedMatch::new(&m.word, &prefix),
            };
            (rm, m)
        })
        .collect();

    const MAX_COUNT: usize = 100;
    // Gather best max_count matches
    let mut candidates: InsertCompletionCandidateList = Vec::with_capacity(ranked.len().min(MAX_COUNT) + 1);
    let menu_info_face = if OTHER_BUFFERS { faces.get(b"MenuInfo")? } else { Face::default() };

    for_n_best(
        ranked,
        MAX_COUNT,
        |lhs, rhs| lhs.0.less(&rhs.0),
        |(rm, m)| {
            let candidate = rm.candidate();
            if candidates.last().is_some_and(|c| c.completion == candidate) {
                return false;
            }
            let mut menu_entry = DisplayLine::default();
            match &m.buffer {
                Some(buf) if OTHER_BUFFERS => {
                    let pad_len = longest + 1 - utf8::distance(candidate);
                    menu_entry.push_back(DisplayAtom::text(candidate, Face::default()));
                    menu_entry.push_back(DisplayAtom::text(string_of_cp(' ' as Codepoint, pad_len), Face::default()));
                    menu_entry.push_back(DisplayAtom::text(limit(&buf.display_name(), 20), menu_info_face));
                }
                _ => {
                    menu_entry.push_back(DisplayAtom::text(candidate, Face::default()));
                }
            }

            candidates.push(InsertCompletionCandidate {
                completion: candidate.to_vec(),
                on_select: Vec::new(),
                menu_entry,
            });
            true
        },
    );

    Ok(InsertCompletion { candidates, begin: word_begin, end: cursor_pos, timestamp: buffer.timestamp() })
}

fn complete_filename<const REQUIRE_SLASH: bool>(
    sels: &SelectionList,
    options: &OptionManager,
    _faces: &FaceRegistry,
) -> Result<InsertCompletion> {
    let buffer = sels.buffer();
    let (begin, pos, prefix, front) = {
        let lines = buffer.lines();
        let pos = BufferIterator::new(&lines, sels.main().cursor().coord);
        let buf_begin = BufferIterator::begin(&lines);
        let mut begin = pos;

        let is_filename = |c: u8| c.is_ascii_alphanumeric() || c == b'/' || c == b'.' || c == b'_' || c == b'-';
        while begin != buf_begin && is_filename(crate::utf8::ByteIt::get(&begin.prev_byte())) {
            begin = begin.prev_byte();
        }

        if begin != buf_begin
            && crate::utf8::ByteIt::get(&begin) == b'/'
            && crate::utf8::ByteIt::get(&begin.prev_byte()) == b'~'
        {
            begin = begin.prev_byte();
        }

        let prefix = buffer.substr(begin.coord(), pos.coord());
        // prefix.front() reads the byte at the cursor when prefix is empty
        let front = match prefix.first() {
            Some(&c) => c,
            None if pos != BufferIterator::end(&lines) => crate::utf8::ByteIt::get(&pos),
            None => 0,
        };
        (begin.coord(), pos.coord(), prefix, front)
    };

    if REQUIRE_SLASH && !prefix.contains(&b'/') {
        return Ok(InsertCompletion::default());
    }

    // Do not try to complete in that case as its unlikely to be a filename,
    // and triggers network host search of cygwin.
    if prefix.starts_with(b"//") {
        return Ok(InsertCompletion::default());
    }

    let mut candidates: InsertCompletionCandidateList = Vec::new();
    if front == b'/' || front == b'~' {
        let ignored_files = options.get(b"ignored_files")?.try_get::<Regex>()?.clone();
        for filename in complete_filename_impl(&prefix, &ignored_files, prefix.len(), FilenameFlags::None) {
            candidates.push(InsertCompletionCandidate {
                menu_entry: DisplayLine::from_str_face(filename.clone(), Face::default()),
                completion: filename,
                on_select: Vec::new(),
            });
        }
    } else {
        let mut visited_dirs: Vec<Vec<u8>> = Vec::new();
        let paths = options.get(b"path")?.try_get::<StringList>()?.clone();
        for dir in paths {
            let buf_dir: Vec<u8> = if buffer.flags().has(BufferFlags::File) {
                split_path(&buffer.filename()).0.to_vec()
            } else {
                Vec::new()
            };
            let mut dir = real_path(&parse_filename(&dir, &buf_dir));

            if !dir.is_empty() && dir.last() != Some(&b'/') {
                dir.push(b'/');
            }

            if visited_dirs.contains(&dir) {
                continue;
            }

            let mut query = dir.clone();
            query.extend_from_slice(&prefix);
            let ignored_files = options.get(b"ignored_files")?.try_get::<Regex>()?.clone();
            for filename in complete_filename_impl(&query, &ignored_files, query.len(), FilenameFlags::None) {
                let candidate = crate::string_utils::substr(&filename, dir.len(), -1).to_vec();
                candidates.push(InsertCompletionCandidate {
                    menu_entry: DisplayLine::from_str_face(candidate.clone(), Face::default()),
                    completion: candidate,
                    on_select: Vec::new(),
                });
            }

            visited_dirs.push(dir);
        }
    }
    if candidates.is_empty() {
        return Ok(InsertCompletion::default());
    }
    Ok(InsertCompletion { candidates, begin, end: pos, timestamp: buffer.timestamp() })
}

thread_local! {
    static COMPLETION_DESC_REGEX: Regex =
        Regex::new(br"(\d+)\.(\d+)(?:\+(\d+))?@(\d+)", RegexCompileFlags::NONE).expect("valid regex");
}

fn complete_option(
    sels: &SelectionList,
    options: &OptionManager,
    faces: &FaceRegistry,
    option_name: &[u8],
) -> Result<InsertCompletion> {
    let buffer = sels.buffer();
    let cursor_pos = sels.main().cursor().coord;

    let opt_ref = options.get(option_name)?;
    let opt = opt_ref.try_get::<CompletionList>()?;
    if opt.list.is_empty() {
        return Ok(InsertCompletion::default());
    }

    let desc = &opt.prefix;
    let Some(captures) = COMPLETION_DESC_REGEX.with(|re| regex_match_ranges(desc, re)) else {
        return Ok(InsertCompletion::default());
    };
    let capture = |i: usize| captures.get(i).copied().flatten().map(|(b, e)| &desc[b..e]);
    let capture_int = |i: usize| str_to_int(capture(i).unwrap_or(b""));

    let coord = BufferCoord::new(capture_int(1)? - 1, capture_int(2)? - 1);
    if !buffer.is_valid(coord) {
        return Ok(InsertCompletion::default());
    }
    let timestamp = capture_int(4)? as usize;
    if buffer.with_changes_since(timestamp, |changes| changes.iter().any(|change| change.begin < coord)) {
        return Ok(InsertCompletion::default());
    }

    if cursor_pos.line != coord.line || cursor_pos.column < coord.column {
        return Ok(InsertCompletion::default());
    }

    let tabstop: ColumnCount = *options.get(b"tabstop")?.try_get::<i32>()?;
    let column = get_column(buffer, tabstop, cursor_pos);

    // RankedMatchAndInfo
    struct MatchAndInfo<'a> {
        m: RankedMatch<'a>,
        on_select: &'a [u8],
        menu_entry: DisplayLine,
    }

    let query = buffer.substr(coord, cursor_pos);
    let mut matches: Vec<MatchAndInfo> = Vec::new();

    let builtins = DisplayLineBuiltins::default();
    for (i, candidate) in opt.list.iter().enumerate() {
        let mut m = RankedMatch::new(&candidate.0, &query);
        if m.is_match() {
            m.set_input_sequence_number(i);
            let menu = &candidate.2;
            let menu_entry = if !menu.is_empty() {
                parse_display_line(&expand_tabs(menu, tabstop, column), faces, &builtins)?
            } else {
                DisplayLine::from_str_face(Vec::new(), Face::default())
            };
            matches.push(MatchAndInfo { m, on_select: &candidate.1, menu_entry });
        }
    }

    const MAX_COUNT: usize = 100;
    // Gather best max_count matches
    let mut candidates: InsertCompletionCandidateList = Vec::with_capacity(matches.len().min(MAX_COUNT) + 1);

    for_n_best(
        matches,
        MAX_COUNT,
        |lhs, rhs| lhs.m.less(&rhs.m),
        |m| {
            if candidates.last().is_some_and(|c| c.completion == m.m.candidate() && c.on_select == m.on_select) {
                return false;
            }
            candidates.push(InsertCompletionCandidate {
                completion: m.m.candidate().to_vec(),
                on_select: m.on_select.to_vec(),
                menu_entry: m.menu_entry.clone(),
            });
            true
        },
    );

    let mut end = cursor_pos;
    if capture(3).is_some() {
        let len: ByteCount = capture_int(3)?;
        end = buffer.advance(coord, len);
    }
    Ok(InsertCompletion { candidates, begin: coord, end, timestamp })
}

fn complete_line<const OTHER_BUFFERS: bool>(
    sels: &SelectionList,
    options: &OptionManager,
    _faces: &FaceRegistry,
) -> Result<InsertCompletion> {
    let buffer = sels.buffer();
    let cursor_pos = sels.main().cursor().coord;

    let tabstop: ColumnCount = *options.get(b"tabstop")?.try_get::<i32>()?;
    let column = get_column(buffer, tabstop, cursor_pos);

    let trim_leading_whitespaces = |s: &[u8]| -> usize {
        let mut it = 0;
        while it != s.len() && is_horizontal_blank(utf8::codepoint_at(s, it)) {
            it = utf8::next(s, it);
        }
        it
    };

    let cursor_line = buffer.line(cursor_pos.line);
    let line_prefix = crate::string_utils::substr(&cursor_line, 0, cursor_pos.column as isize);
    let prefix = &line_prefix[trim_leading_whitespaces(line_prefix)..];
    let replace_begin = buffer.advance(cursor_pos, -(prefix.len() as ByteCount));
    let mut candidates: InsertCompletionCandidateList = Vec::new();

    let mut add_candidates = |buf: &Rc<Buffer>| {
        let lines = buf.lines();
        for (l, line) in lines.iter().enumerate() {
            if Rc::ptr_eq(buf, buffer) && l as i32 == cursor_pos.line {
                continue;
            }

            let line = crate::string_utils::substr(line, 0, line.len() as isize - 1);
            let candidate = &line[trim_leading_whitespaces(line)..];

            if candidate.is_empty() {
                continue;
            }

            if prefix == crate::string_utils::substr(candidate, 0, prefix.len() as isize) {
                candidates.push(InsertCompletionCandidate {
                    completion: candidate.to_vec(),
                    on_select: Vec::new(),
                    menu_entry: DisplayLine::from_str_face(expand_tabs(candidate, tabstop, column), Face::default()),
                });
                // perf: it's unlikely the user intends to search among >10 candidates anyway
                if candidates.len() == 100 {
                    break;
                }
            }
        }
    };

    add_candidates(buffer);

    if OTHER_BUFFERS {
        for buf in BufferManager::instance().buffers() {
            if !Rc::ptr_eq(&buf, buffer) && !buf.flags().has(BufferFlags::Debug) {
                add_candidates(&buf);
            }
        }
    }

    if candidates.is_empty() {
        return Ok(InsertCompletion::default());
    }
    candidates.sort_by(|lhs, rhs| crate::string_utils::cpp_str_cmp(&lhs.completion, &rhs.completion));
    candidates.dedup_by(|a, b| a.completion == b.completion);
    Ok(InsertCompletion { candidates, begin: replace_begin, end: cursor_pos, timestamp: buffer.timestamp() })
}

/// The completer state, kept in an Rc so that it can be registered as an
/// option watcher.
struct InsertCompleterImpl {
    input_handler: Weak<InputHandler>,
    options: Rc<OptionManager>,
    faces: Rc<FaceRegistry>,
    completions: RefCell<InsertCompletion>,
    inserted_ranges: RefCell<Vec<BufferRange>>,
    current_candidate: Cell<i32>,
    enabled: Cell<bool>,
    explicit_completer: Cell<Option<CompleteFunc>>,
}

pub struct InsertCompleter {
    inner: Rc<InsertCompleterImpl>,
}

impl InsertCompleter {
    pub fn new(input_handler: &Rc<InputHandler>) -> Self {
        // local scopes might go away before completion ends, make sure to register on a long lived one
        let scope = input_handler.context().scope_allow_local(false);
        let inner = Rc::new(InsertCompleterImpl {
            input_handler: Rc::downgrade(input_handler),
            options: scope.options().clone(),
            faces: scope.faces().clone(),
            completions: RefCell::new(InsertCompletion::default()),
            inserted_ranges: RefCell::new(Vec::new()),
            current_candidate: Cell::new(-1),
            enabled: Cell::new(true),
            explicit_completer: Cell::new(None),
        });
        inner.options.register_watcher(Rc::downgrade(&inner) as Weak<dyn OptionWatcher>);
        InsertCompleter { inner }
    }

    pub fn select(&self, index: i32, relative: bool, record_key: &mut dyn FnMut(Key)) -> Result<()> {
        self.inner.select(index, relative, record_key)
    }

    pub fn update(&self, allow_implicit: bool) -> Result<()> {
        self.inner.update(allow_implicit)
    }

    pub fn try_accept(&self) -> Result<()> {
        if self.inner.has_candidate_selected() {
            self.inner.reset()?;
        }
        Ok(())
    }

    /// reset, reporting errors that are not runtime errors (such as kill
    /// session requests coming from hooks)
    pub fn try_reset(&self) -> Result<()> {
        self.inner.reset()
    }

    pub fn reset(&self) {
        if let Err(err) = self.inner.reset() {
            write_to_debug_buffer(&kformat!("error while resetting insert completion: {}", err.what()));
        }
    }

    pub fn explicit_file_complete(&self) -> Result<()> {
        self.inner.explicit_complete(complete_filename::<false>)
    }

    pub fn explicit_word_buffer_complete(&self) -> Result<()> {
        self.inner.explicit_complete(complete_word::<false>)
    }

    pub fn explicit_word_all_complete(&self) -> Result<()> {
        self.inner.explicit_complete(complete_word::<true>)
    }

    pub fn explicit_line_buffer_complete(&self) -> Result<()> {
        self.inner.explicit_complete(complete_line::<false>)
    }

    pub fn explicit_line_all_complete(&self) -> Result<()> {
        self.inner.explicit_complete(complete_line::<true>)
    }

    pub fn has_candidate_selected(&self) -> bool {
        self.inner.has_candidate_selected()
    }
}

impl Drop for InsertCompleter {
    fn drop(&mut self) {
        let watcher = Rc::downgrade(&self.inner) as Weak<dyn OptionWatcher>;
        self.inner.options.unregister_watcher(&watcher);
    }
}

impl InsertCompleterImpl {
    fn input_handler(&self) -> Option<Rc<InputHandler>> {
        self.input_handler.upgrade()
    }

    fn select(&self, index: i32, relative: bool, record_key: &mut dyn FnMut(Key)) -> Result<()> {
        self.enabled.set(true);
        if !self.setup_ifn()? {
            return Ok(());
        }
        let Some(ih) = self.input_handler() else { return Ok(()) };
        let context = ih.context();

        let buffer = context.buffer();
        let (candidate, completion_begin, completion_end) = {
            let completions = self.completions.borrow();
            let count = completions.candidates.len() as i32;
            if count == 0 {
                return Ok(());
            }
            let mut current = (if relative { self.current_candidate.get() + index } else { index }) % count;
            if current < 0 {
                current += count;
            }
            self.current_candidate.set(current);
            (completions.candidates[current as usize].clone(), completions.begin, completions.end)
        };

        let (prefix_len, suffix_len, mut ranges) = {
            let selections = context.selections();
            let cursor_pos = selections.main().cursor().coord;
            let prefix_len = buffer.distance(completion_begin, cursor_pos);
            let suffix_len = buffer.distance(cursor_pos, completion_end).max(0);

            let reference = buffer.string(completion_begin, completion_end);
            let lines = buffer.lines();
            let buffer_end = BufferIterator::end(&lines);
            let equal = |mut it: BufferIterator| {
                for &c in &reference {
                    if it == buffer_end || crate::utf8::ByteIt::get(&it) != c {
                        return false;
                    }
                    crate::utf8::ByteIt::inc(&mut it);
                }
                true
            };
            let mut ranges: Vec<BufferRange> = Vec::new();
            for sel in selections.iter() {
                let pos = BufferIterator::new(&lines, sel.cursor().coord);
                if pos.coord().column >= prefix_len && pos.add(suffix_len) != buffer_end && equal(pos.sub(prefix_len)) {
                    ranges.push(BufferRange::new(pos.sub(prefix_len).coord(), pos.add(suffix_len).coord()));
                }
            }
            (prefix_len, suffix_len, ranges)
        };
        replace_ranges(&buffer, &mut ranges, std::slice::from_ref(&candidate.completion))?;

        let cursor_pos = {
            let mut selections = context.selections_write_only();
            selections.update(true);
            selections.main().cursor().coord
        };
        {
            let mut completions = self.completions.borrow_mut();
            completions.end = cursor_pos;
            completions.begin = buffer.advance(cursor_pos, -(candidate.completion.len() as ByteCount));
            completions.timestamp = buffer.timestamp();
        }
        *self.inserted_ranges.borrow_mut() = ranges;

        if context.has_client() {
            context.client().menu_select(self.current_candidate.get());
        }

        {
            for _ in 0..prefix_len {
                record_key(Key::new(named::BACKSPACE));
            }
            for _ in 0..suffix_len {
                record_key(Key::new(named::DELETE));
            }
            // [FAITHFUL] one key per byte, as a sign extended char, see
            // UPSTREAM-QUIRKS.md
            for &c in candidate.completion.iter() {
                record_key(Key::new(c as i8 as Codepoint));
            }
        }

        if !candidate.on_select.is_empty() {
            CommandManager::instance().execute(&candidate.on_select, context, &ShellContext::default())?;
        }
        Ok(())
    }

    fn update(&self, allow_implicit: bool) -> Result<()> {
        self.enabled.set(allow_implicit);
        if let Some(explicit_completer) = self.explicit_completer.get()
            && self.try_complete(explicit_completer)?
        {
            return Ok(());
        }

        self.reset()?;
        self.setup_ifn()?;
        Ok(())
    }

    fn has_candidate_selected(&self) -> bool {
        let completions = self.completions.borrow();
        let current = self.current_candidate.get();
        completions.is_valid() && current >= 0 && (current as usize) < completions.candidates.len() - 1
    }

    fn reset(&self) -> Result<()> {
        if self.explicit_completer.get().is_none() && !self.completions.borrow().is_valid() {
            return Ok(());
        }
        let Some(ih) = self.input_handler() else { return Ok(()) };
        let context = ih.context();

        let mut hook_param = Vec::new();
        if context.has_client() && self.has_candidate_selected() {
            let buffer = context.buffer();
            let mut inserted_ranges = self.inserted_ranges.borrow_mut();
            let timestamp = self.completions.borrow().timestamp;
            update_ranges(&buffer, timestamp, &mut inserted_ranges);
            self.completions.borrow_mut().timestamp = buffer.timestamp();

            hook_param = join(
                inserted_ranges.iter().filter(|r| !r.empty()).map(|r| {
                    selection_to_string(
                        ColumnType::Byte,
                        &buffer,
                        &Selection::from_range(r.begin, buffer.char_prev(r.end)),
                        -1,
                    )
                }),
                b' ',
                true,
            );
        }

        self.explicit_completer.set(None);
        *self.completions.borrow_mut() = InsertCompletion::default();
        self.inserted_ranges.borrow_mut().clear();
        if context.has_client() {
            let client = context.client();
            client.menu_hide();
            client.info_hide(false);
            context.hooks().run_hook(Hook::InsertCompletionHide, &hook_param, context)?;
        }
        Ok(())
    }

    fn completers(&self) -> Result<InsertCompleterDescList> {
        Ok(self.options.get(b"completers")?.try_get::<InsertCompleterDescList>()?.clone())
    }

    fn setup_ifn(&self) -> Result<bool> {
        if !self.enabled.get() {
            return Ok(false);
        }
        if !self.completions.borrow().is_valid() {
            let completers = self.completers()?;
            for completer in &completers {
                let param = completer.param.as_deref().unwrap_or(b"");
                let mode = completer.mode;
                if mode == InsertCompleterMode::Filename && self.try_complete(complete_filename::<true>)? {
                    return Ok(true);
                }
                if mode == InsertCompleterMode::Option
                    && self.try_complete(|sels: &SelectionList, options: &OptionManager, faces: &FaceRegistry| {
                        complete_option(sels, options, faces, param)
                    })?
                {
                    return Ok(true);
                }
                if mode == InsertCompleterMode::Word && param == b"buffer" && self.try_complete(complete_word::<false>)? {
                    return Ok(true);
                }
                if mode == InsertCompleterMode::Word && param == b"all" && self.try_complete(complete_word::<true>)? {
                    return Ok(true);
                }
                if mode == InsertCompleterMode::Line && param == b"buffer" && self.try_complete(complete_line::<false>)? {
                    return Ok(true);
                }
                if mode == InsertCompleterMode::Line && param == b"all" && self.try_complete(complete_line::<true>)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        Ok(true)
    }

    fn menu_show(&self) -> Result<()> {
        let Some(ih) = self.input_handler() else { return Ok(()) };
        let context = ih.context();
        if !context.has_client() {
            return Ok(());
        }

        let (menu_entries, begin) = {
            let completions = self.completions.borrow();
            let entries: Vec<DisplayLine> = completions.candidates.iter().map(|c| c.menu_entry.clone()).collect();
            (entries, completions.begin)
        };

        let client = context.client();
        client.menu_show(menu_entries, begin, MenuStyle::Inline);
        client.menu_select(self.current_candidate.get());
        context.hooks().run_hook(Hook::InsertCompletionShow, b"", context)
    }

    fn try_complete(
        &self,
        complete_func: impl Fn(&SelectionList, &OptionManager, &FaceRegistry) -> Result<InsertCompletion>,
    ) -> Result<bool> {
        let Some(ih) = self.input_handler() else { return Ok(false) };
        let context = ih.context();

        let res = (|| -> Result<(InsertCompletion, Rc<Buffer>)> {
            self.reset()?;
            // copy the selections so that no borrow of the context is held
            let sels = context.selections().clone();
            Ok((complete_func(&sels, &self.options, &self.faces)?, sels.buffer().clone()))
        })();
        let buffer = match res {
            Ok((completions, buffer)) => {
                *self.completions.borrow_mut() = completions;
                buffer
            }
            Err(err) if err.is_runtime() => {
                write_to_debug_buffer(&kformat!("error while trying to run completer: {}", err.what()));
                return Ok(false);
            }
            Err(err) => return Err(err),
        };
        if !self.completions.borrow().is_valid() {
            return Ok(false);
        }

        debug_assert!(self.completions.borrow().begin <= context.selections().main().cursor().coord);
        self.current_candidate.set(self.completions.borrow().candidates.len() as i32);
        self.menu_show()?;
        let (begin, end) = {
            let completions = self.completions.borrow();
            (completions.begin, completions.end)
        };
        let completion = buffer.string(begin, end);
        self.completions.borrow_mut().candidates.push(InsertCompletionCandidate {
            completion,
            on_select: Vec::new(),
            menu_entry: DisplayLine::default(),
        });
        Ok(true)
    }

    fn explicit_complete(&self, complete_func: CompleteFunc) -> Result<()> {
        self.try_complete(complete_func)?;
        self.explicit_completer.set(Some(complete_func));
        Ok(())
    }
}

impl OptionWatcher for InsertCompleterImpl {
    fn on_option_changed(&self, opt: &Opt) -> Result<()> {
        // Do not reset the menu if the user has selected an entry
        {
            let completions = self.completions.borrow();
            if !completions.candidates.is_empty()
                && self.current_candidate.get() != completions.candidates.len() as i32 - 1
            {
                return Ok(());
            }
        }

        let completers = self.completers()?;
        for completer in &completers {
            if completer.mode == InsertCompleterMode::Option && completer.param.as_deref() == Some(opt.name()) {
                self.reset()?;
                self.setup_ifn()?;
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::option_types::OptionType;

    fn desc(mode: InsertCompleterMode, param: Option<&str>) -> InsertCompleterDesc {
        InsertCompleterDesc { mode, param: param.map(|p| p.as_bytes().to_vec()) }
    }

    #[test]
    fn test_completer_desc() {
        use InsertCompleterMode::*;
        assert_eq!(option_from_string(b"option=foo").unwrap(), desc(Option, Some("foo")));
        assert_eq!(option_from_string(b"option=").unwrap(), desc(Option, Some("")));
        assert_eq!(option_from_string(b"word=all").unwrap(), desc(Word, Some("all")));
        assert_eq!(option_from_string(b"word=buffer").unwrap(), desc(Word, Some("buffer")));
        assert_eq!(option_from_string(b"line=all").unwrap(), desc(Line, Some("all")));
        assert_eq!(option_from_string(b"filename").unwrap(), desc(Filename, None));
        assert!(option_from_string(b"word=foo").is_err());
        assert!(option_from_string(b"line=").is_err());
        assert!(option_from_string(b"filenames").is_err());
        assert_eq!(
            option_from_string(b"foo").unwrap_err().what(),
            b"invalid completer description: 'foo'"
        );

        for s in ["option=foo", "word=all", "word=buffer", "line=all", "line=buffer", "filename"] {
            assert_eq!(option_to_string(&option_from_string(s.as_bytes()).unwrap()), s.as_bytes());
        }

        assert_eq!(<InsertCompleterDesc as OptionType>::type_name(), b"completer");
        assert_eq!(<InsertCompleterDescList as OptionType>::type_name(), b"completer-list");
        assert_eq!(<CompletionList as OptionType>::type_name(), b"completions");

        let strs: Vec<Vec<u8>> = vec![b"filename".to_vec(), b"word=all".to_vec(), b"option=a b".to_vec()];
        let list = InsertCompleterDescList::from_strings(&strs).unwrap();
        assert_eq!(list.to_strings(), strs);
        assert_eq!(list.to_string_q(Quoting::Kakoune), b"filename word=all option=a b");
    }

    fn setup() -> Rc<OptionManager> {
        use crate::buffer::{ByteOrderMark, EolFormat, FinalEol};
        use crate::option_manager::OptionFlags;
        use crate::scope::GlobalScope;
        let gs = GlobalScope::create();
        let reg = gs.option_registry();
        reg.declare_option(b"eolformat", b"", EolFormat::Lf, OptionFlags::None).unwrap();
        reg.declare_option(b"finaleol", b"", FinalEol::Present, OptionFlags::None).unwrap();
        reg.declare_option(b"BOM", b"", ByteOrderMark::None, OptionFlags::None).unwrap();
        reg.declare_option(b"readonly", b"", false, OptionFlags::None).unwrap();
        reg.declare_option(b"tabstop", b"", 8i32, OptionFlags::None).unwrap();
        reg.declare_option(b"extra_word_chars", b"", vec![CodepointOpt('_' as u32)], OptionFlags::None).unwrap();
        reg.declare_option(b"static_words", b"", StringList::new(), OptionFlags::None).unwrap();
        reg.declare_option(b"completions", b"", CompletionList::default(), OptionFlags::None).unwrap();
        gs.options().clone()
    }

    fn make_buffer(content: &str) -> Rc<Buffer> {
        let lines = crate::buffer_utils::parse_lines(content.as_bytes(), crate::buffer::EolFormat::Lf, 0).unwrap();
        Buffer::new(
            b"test".to_vec(),
            BufferFlags::None,
            lines,
            crate::buffer::ByteOrderMark::None,
            crate::buffer::EolFormat::Lf,
            crate::buffer::FinalEol::Present,
            Default::default(),
        )
        .unwrap()
    }

    fn completions(c: &InsertCompletion) -> Vec<String> {
        c.candidates.iter().map(|c| String::from_utf8(c.completion.clone()).unwrap()).collect()
    }

    #[test]
    fn test_complete_word_and_line() {
        let options = setup();
        let faces = FaceRegistry::new_root();
        let buffer = make_buffer("foobar foobaz foo_qux\n  fo\n\tfoo line\n");
        let sels = SelectionList::new(buffer.clone(), Selection::new(BufferCoord::new(1, 4)));
        let res = complete_word::<false>(&sels, &options, &faces).unwrap();
        assert_eq!(res.begin, BufferCoord::new(1, 2));
        assert_eq!(res.end, BufferCoord::new(1, 4));
        assert_eq!(completions(&res), ["foo", "foo_qux", "foobar", "foobaz"]);

        // at word start, no completion
        let sels = SelectionList::new(buffer.clone(), Selection::new(BufferCoord::new(1, 2)));
        assert!(!complete_word::<false>(&sels, &options, &faces).unwrap().is_valid());

        let sels = SelectionList::new(buffer.clone(), Selection::new(BufferCoord::new(1, 4)));
        let res = complete_line::<false>(&sels, &options, &faces).unwrap();
        assert_eq!(res.begin, BufferCoord::new(1, 2));
        assert_eq!(completions(&res), ["foo line", "foobar foobaz foo_qux"]);
        assert_eq!(res.candidates[0].menu_entry.atoms()[0].content().as_ref(), b"foo line");
    }

    #[test]
    fn test_complete_option() {
        let options = setup();
        let faces = FaceRegistry::new_root();
        let buffer = make_buffer("abc fo\n");
        let timestamp = buffer.timestamp();
        let strs: Vec<Vec<u8>> = vec![
            format!("1.5+2@{timestamp}").into_bytes(),
            b"foo|cmd1|foo menu".to_vec(),
            b"bar|cmd2|".to_vec(),
            b"fooo|cmd3|".to_vec(),
        ];
        options.get(b"completions").unwrap().set(CompletionList::from_strings(&strs).unwrap(), false).unwrap();
        let sels = SelectionList::new(buffer.clone(), Selection::new(BufferCoord::new(0, 6)));
        let res = complete_option(&sels, &options, &faces, b"completions").unwrap();
        assert_eq!(res.begin, BufferCoord::new(0, 4));
        assert_eq!(res.end, BufferCoord::new(0, 6));
        assert_eq!(res.timestamp, timestamp);
        assert_eq!(completions(&res), ["foo", "fooo"]);
        assert_eq!(res.candidates[0].on_select, b"cmd1");

        // cursor before completion start
        let sels = SelectionList::new(buffer.clone(), Selection::new(BufferCoord::new(0, 2)));
        assert!(!complete_option(&sels, &options, &faces, b"completions").unwrap().is_valid());
    }
}
