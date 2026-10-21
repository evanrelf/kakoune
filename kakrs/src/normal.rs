//! Port of normal.hh/cc

use crate::buffer::{Buffer, BufferFlags, BufferIterator, StringDataPtr};
use crate::buffer_manager::BufferManager;
use crate::buffer_utils::{
    char_length_sel, content, get_byte_to_column, get_column, is_bol, is_bow, is_eow, open_file_buffer,
};
use crate::changes::ForwardChangesTracker;
use crate::command_manager::{CommandManager, Completer, ShellContext};
use crate::completion::{complete_nothing, shell_complete, CandidateList, Completions};
use crate::context::{Context, Direction, ScopedEdition, ScopedSelectionEdition};
use crate::debug::EnvVarMap;
use crate::diff::{for_each_diff, DiffOp};
use crate::display_buffer::DisplayLine;
use crate::event_manager::UrgentEventsIdle;
use crate::exception::{Error, ErrorKind, Result};
use crate::file::{find_file, split_path};
use crate::input_handler::{
    hide_auto_info_ifn, on_next_key_with_autoinfo, scroll_window, show_auto_info_ifn, AutoInfo, InputHandler, InsertMode,
    OnHiddenCursor, PromptCompleter, PromptEvent, PromptFlags,
};
use crate::keymap_manager::KeymapMode;
use crate::keys::{alt, ctrl, key, key_to_str, named, parse_keys, Key};
use crate::option_types::{EnumDesc, OptionElem};
use crate::ranked_match::{used_letters, RankedMatch};
use crate::regex::{match_flags, regex_search, Regex};
use crate::regex_vm::{RegexCompileFlags, RegexMode};
use crate::register_manager::RegisterManager;
use crate::selection::{
    compute_modified_ranges, insert_at, replace_selection, selection_list_from_strings, selection_to_string,
    ColumnType, Selection, SelectionList, MAX_COLUMN,
};
use crate::selectors::{self, ObjectFlags};
use crate::shell_manager::{ShellFlags, ShellManager};
use crate::string_utils::{column_length, escape, join, split_after, split_escaped, str_to_int, string_of_cp, string_of_cp_cols, unescape, Quoting};
use crate::unicode::{
    is_basic_alpha, is_blank, is_horizontal_blank, is_punctuation, is_word_default, to_lower, to_upper, WordType,
};
use crate::units::{BufferCoord, BufferCoordAndTarget, BufferRange, ByteCount, CharCount, ColumnCount, Codepoint, DisplayCoord, LineCount};
use crate::user_interface::InfoStyle;
use crate::utf8;
use crate::utils::ScopedSetBool;
use crate::word_db::{for_n_best, get_word_db};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

fn no_selections_remaining() -> Error {
    Error::new(ErrorKind::NoSelectionsRemaining, "no selections remaining")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct NormalParams {
    pub count: i32,
    pub reg: u8,
}

#[derive(Clone, Copy)]
pub struct NormalCmd {
    pub docstring: &'static str,
    pub func: fn(&Context, NormalParams) -> Result<()>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
    Replace,
    Extend,
    Append,
}

impl EnumDesc for SelectMode {
    fn desc() -> &'static [(Self, &'static str)] {
        &[(SelectMode::Replace, "replace"), (SelectMode::Extend, "extend"), (SelectMode::Append, "append")]
    }
}
crate::enum_option!(SelectMode);

/// A selector applied to each selection
pub type SelectorFn = Rc<dyn Fn(&Context, &Selection) -> Result<Option<Selection>>>;

// Selector adapters ---------------------------------------------------------

fn sel_fn(f: impl Fn(&Context, &Selection) -> Result<Option<Selection>> + 'static) -> SelectorFn {
    Rc::new(f)
}

fn to_next_word(word_type: WordType) -> SelectorFn {
    sel_fn(move |c, s| Ok(selectors::select_to_next_word(word_type, c, s)))
}
fn to_next_word_end(word_type: WordType) -> SelectorFn {
    sel_fn(move |c, s| Ok(selectors::select_to_next_word_end(word_type, c, s)))
}
fn to_previous_word(word_type: WordType) -> SelectorFn {
    sel_fn(move |c, s| Ok(selectors::select_to_previous_word(word_type, c, s)))
}
fn to_line_end(only_move: bool) -> SelectorFn {
    sel_fn(move |c, s| Ok(if only_move { selectors::select_to_line_end::<true>(c, s) } else { selectors::select_to_line_end::<false>(c, s) }))
}
fn to_line_begin(only_move: bool) -> SelectorFn {
    sel_fn(move |c, s| Ok(if only_move { selectors::select_to_line_begin::<true>(c, s) } else { selectors::select_to_line_begin::<false>(c, s) }))
}
fn matching(forward: bool) -> SelectorFn {
    sel_fn(move |c, s| Ok(if forward { selectors::select_matching::<true>(c, s) } else { selectors::select_matching::<false>(c, s) }))
}

pub fn merge_selections(sel: &mut Selection, new_sel: &Selection) {
    let forward = sel.cursor().coord >= sel.anchor();
    let new_forward = new_sel.cursor().coord > new_sel.anchor();
    if forward && new_forward {
        let a = sel.anchor().min(new_sel.anchor());
        sel.set_anchor(a);
    }
    let backward = sel.cursor().coord <= sel.anchor();
    let new_backward = new_sel.cursor().coord < new_sel.anchor();
    if backward && new_backward {
        let a = sel.anchor().max(new_sel.anchor());
        sel.set_anchor(a);
    }
    sel.set_cursor(new_sel.cursor());
}

pub fn select(context: &Context, mode: SelectMode, func: &dyn Fn(&Context, &Selection) -> Result<Option<Selection>>) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut selections = context.selections();
    if mode == SelectMode::Append {
        let sel = selections.main().clone();
        if let Some(mut res) = func(context, &sel)? {
            if res.captures().is_empty() {
                *res.captures_mut() = sel.captures().clone();
            }
            selections.push(res);
            let n = selections.size();
            selections.set_main_index(n - 1);
        }
    } else {
        let mut main_index = selections.main_index();
        let mut new_size = 0;
        for i in 0..selections.size() {
            let sel = selections[i].clone();
            let Some(mut res) = func(context, &sel)? else {
                if i <= main_index && main_index != 0 {
                    main_index -= 1;
                }
                continue;
            };

            let mut sel = sel;
            if mode == SelectMode::Extend {
                merge_selections(&mut sel, &res);
            } else {
                sel.set_anchor(res.anchor());
                sel.set_cursor(res.cursor());
            }
            if !res.captures().is_empty() {
                *sel.captures_mut() = std::mem::take(res.captures_mut());
            }

            selections[new_size] = sel;
            new_size += 1;
        }

        if new_size == 0 {
            return Err(no_selections_remaining());
        }

        selections.set_main_index(main_index);
        selections.remove_from(new_size);
    }

    selections.sort_and_merge_overlapping();
    Ok(())
}

fn select_and_set_last(context: &Context, mode: SelectMode, func: SelectorFn) -> Result<()> {
    let f = func.clone();
    context.set_last_select(Rc::new(move |context: &Context| select(context, mode, f.as_ref())));
    select(context, mode, func.as_ref())
}

fn select_coord(context: &Context, mode: SelectMode, coord: BufferCoord) -> Result<()> {
    let buffer = context.try_buffer()?;
    let _selection_edition = ScopedSelectionEdition::new(context);
    let coord = buffer.clamp(coord);
    if mode == SelectMode::Replace {
        *context.selections() = SelectionList::new(buffer.clone(), Selection::new(coord));
    } else if mode == SelectMode::Extend {
        let mut selections = context.selections();
        for sel in selections.iter_mut() {
            sel.set_cursor(coord);
        }
        selections.sort_and_merge_overlapping();
    }
    Ok(())
}

fn enter_insert_mode(context: &Context, mode: InsertMode, params: NormalParams) -> Result<()> {
    context.try_input_handler()?.insert(mode, params.count)
}

fn repeat_last_insert(context: &Context, _: NormalParams) -> Result<()> {
    context.try_input_handler()?.repeat_last_insert()
}

fn repeat_last_select(context: &Context, _: NormalParams) -> Result<()> {
    context.repeat_last_select()
}

pub struct KeyInfo {
    pub keys: Vec<Key>,
    pub docstring: &'static str,
}

fn ki(keys: &[Key], docstring: &'static str) -> KeyInfo {
    KeyInfo { keys: keys.to_vec(), docstring }
}

pub fn build_autoinfo_for_mapping(context: &Context, mode: KeymapMode, built_ins: &[KeyInfo]) -> Vec<u8> {
    let keymaps = context.keymaps();

    let mut descs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for built_in in built_ins {
        let keys = join(
            built_in.keys.iter().filter(|k| keymaps.get_mapping(**k, mode).is_none()).map(|k| key_to_str(*k)),
            b',',
            false,
        );
        if !keys.is_empty() {
            descs.push((keys, built_in.docstring.as_bytes().to_vec()));
        }
    }

    for key in keymaps.get_mapped_keys(mode) {
        let Some(mapping) = keymaps.get_mapping(key, mode) else { continue };
        let docstring = mapping.docstring;
        if mapping.keys.is_empty() && docstring.is_empty() {
            continue;
        }
        if let Some(elem) = descs.iter_mut().find(|e| e.1 == docstring) {
            elem.0.push(b',');
            elem.0.extend(key_to_str(key));
        } else {
            descs.push((key_to_str(key), docstring));
        }
    }

    let max_len = descs.iter().map(|(k, _)| column_length(k)).max().unwrap_or(0);

    let mut res = Vec::new();
    for (keys, docstring) in &descs {
        res.extend(kformat!("{}:{}{}\n", keys, string_of_cp_cols(' ' as u32, max_len - column_length(keys) + 1), docstring));
    }
    res
}

// goto / view ----------------------------------------------------------------

fn goto_commands(context: &Context, params: NormalParams, mode: SelectMode) -> Result<()> {
    if params.count != 0 {
        context.push_jump(false);
        select_coord(context, mode, BufferCoord::line(params.count - 1))?;
        if let Ok(window) = context.try_window() {
            window.center_line(params.count - 1);
        }
        return Ok(());
    }

    on_next_key_with_autoinfo(
        context,
        b"goto",
        KeymapMode::Goto,
        Rc::new(move |key: Key, context: &Context| {
            let Some(cp) = key.codepoint() else { return Ok(()) };
            if key == Key::new(named::ESCAPE) {
                return Ok(());
            }
            let buffer = context.try_buffer()?;
            let lower_cp = to_lower(cp);
            match char::from_u32(lower_cp).unwrap_or('\0') {
                'g' | 'k' => {
                    context.push_jump(false);
                    select_coord(context, mode, BufferCoord::new(0, 0))
                }
                'l' => select(context, mode, to_line_end(true).as_ref()),
                'h' => select(context, mode, to_line_begin(true).as_ref()),
                'i' => select(context, mode, &|c, s| Ok(selectors::select_to_first_non_blank(c, s))),
                'j' => {
                    context.push_jump(false);
                    select_coord(context, mode, BufferCoord::line(buffer.line_count() - 1))
                }
                'e' => {
                    context.push_jump(false);
                    select_coord(context, mode, buffer.back_coord())
                }
                't' => {
                    if let Ok(window) = context.try_window() {
                        let line = window.position().line;
                        select_coord(context, mode, BufferCoord::line(line))?;
                    }
                    Ok(())
                }
                'b' => {
                    if let Ok(window) = context.try_window() {
                        let line = window.position().line + window.dimensions().line - 1;
                        select_coord(context, mode, BufferCoord::line(line))?;
                    }
                    Ok(())
                }
                'c' => {
                    if let Ok(window) = context.try_window() {
                        let line = window.position().line + window.dimensions().line / 2;
                        select_coord(context, mode, BufferCoord::line(line))?;
                    }
                    Ok(())
                }
                'a' => {
                    let Some(target) = context.last_buffer() else {
                        return Err(Error::runtime("no last buffer"));
                    };
                    context.push_jump(false);
                    context.change_buffer(&target, None)
                }
                'd' | 'u' => {
                    let offset = (if lower_cp == 'd' as u32 { 1 } else { -1 }) * params.count.max(1);
                    select(context, mode, &|context, sel| {
                        let mut cursor = sel.cursor();
                        if let Ok(window) = context.try_window()
                            && let Some(mut display_coord) = window.display_coord(cursor.coord) {
                                if cursor.display_target == -1 {
                                    cursor.display_target = display_coord.column;
                                }
                                display_coord.column = cursor.display_target;
                                if let Some(buffer_coord) = window.buffer_coord(display_coord + DisplayCoord::new(offset, 0)) {
                                    return Ok(Some(Selection::new(BufferCoordAndTarget::new(
                                        buffer_coord,
                                        -1,
                                        cursor.display_target,
                                    ))));
                                }
                            }
                        let tabstop = context.options().val::<i32>("tabstop");
                        Ok(Some(Selection::new(context.buffer().offset_coord_lines(cursor, offset, tabstop))))
                    })
                }
                'f' => {
                    const FORBIDDEN: [u8; 3] = [b'\'', b'\\', 0];
                    let paths_opt = context.options().val::<Vec<Vec<u8>>>("path");
                    let (sels, main_index) = {
                        let sels = context.selections();
                        (sels.as_slice().to_vec(), sels.main_index())
                    };
                    let mut paths = Vec::new();
                    for sel in &sels {
                        let filename = content(&buffer, sel);
                        if filename.iter().any(|c| FORBIDDEN.contains(c)) {
                            return Err(rt_err!("filename contains invalid characters: '{}'", filename));
                        }
                        let buffer_dir = split_path(&buffer.filename()).0.to_vec();
                        let path = find_file(&filename, &buffer_dir, &paths_opt);
                        if path.is_empty() {
                            return Err(rt_err!("unable to find file '{}'", filename));
                        }
                        paths.push(path);
                    }

                    let mut buffer_main = None;
                    for (i, path) in paths.iter().enumerate() {
                        let b = match BufferManager::instance().get_buffer_ifp(path) {
                            Some(b) => b,
                            None => {
                                let b = open_file_buffer(
                                    path,
                                    if context.hooks_disabled().is_set() { BufferFlags::NoHooks } else { BufferFlags::None },
                                )?;
                                b.remove_flags(BufferFlags::NoHooks);
                                b
                            }
                        };
                        if i == main_index {
                            buffer_main = Some(b);
                        }
                    }

                    if let Some(buffer_main) = buffer_main
                        && !Rc::ptr_eq(&buffer_main, &context.buffer()) {
                            context.push_jump(false);
                            context.change_buffer(&buffer_main, None)?;
                        }
                    Ok(())
                }
                '.' => {
                    context.push_jump(false);
                    let Some(mut pos) = buffer.last_modification_coord() else {
                        return Err(Error::runtime("no last modification position"));
                    };
                    if pos >= buffer.back_coord() {
                        pos = buffer.back_coord();
                    }
                    select_coord(context, mode, pos)
                }
                _ => Err(Error::runtime("key not mapped")),
            }
        }),
        (if mode == SelectMode::Extend { "goto (extend to)" } else { "goto" }).as_bytes().to_vec(),
        build_autoinfo_for_mapping(
            context,
            KeymapMode::Goto,
            &[
                ki(&[key('g'), key('k')], "buffer top"),
                ki(&[key('l')], "line end"),
                ki(&[key('h')], "line begin"),
                ki(&[key('u')], "up display line"),
                ki(&[key('d')], "down display line"),
                ki(&[key('i')], "line non blank start"),
                ki(&[key('j')], "buffer bottom"),
                ki(&[key('e')], "buffer end"),
                ki(&[key('t')], "window top"),
                ki(&[key('b')], "window bottom"),
                ki(&[key('c')], "window center"),
                ki(&[key('a')], "last buffer"),
                ki(&[key('f')], "file"),
                ki(&[key('.')], "last buffer change"),
            ],
        ),
    )
}

fn view_commands(context: &Context, params: NormalParams, lock: bool) -> Result<()> {
    let count = params.count;
    on_next_key_with_autoinfo(
        context,
        b"view",
        KeymapMode::View,
        Rc::new(move |key: Key, context: &Context| {
            context.ensure_cursor_visible.set(false);
            if key == Key::new(named::ESCAPE) {
                return Ok(());
            }

            if lock {
                view_commands(context, NormalParams { count, reg: 0 }, true)?;
            }

            let Some(cp) = key.codepoint() else { return Ok(()) };
            let Ok(window) = context.try_window() else { return Ok(()) };

            let cursor = context.selections().main().cursor().coord;
            drop(window.update_display_buffer(context)?);
            if let Ok(client) = context.try_client() {
                client.force_redraw(false);
            }

            let scrolloff = context.options().val::<DisplayCoord>("scrolloff");
            let line_offset = ((window.dimensions().line - 1) / 2).min(scrolloff.line);
            let column_offset = ((window.dimensions().column - 1) / 2).min(scrolloff.column);
            let cursor_column = || column_length(&context.buffer().line(cursor.line)[..cursor.column as usize]);
            match char::from_u32(cp).unwrap_or('\0') {
                'v' | 'c' => window.center_line(cursor.line),
                'm' => window.center_column(cursor_column()),
                't' => window.display_line_at(cursor.line, line_offset),
                'b' => window.display_line_at(cursor.line, window.dimensions().line - 1 - line_offset),
                '<' => window.display_column_at(cursor_column(), column_offset),
                '>' => window.display_column_at(cursor_column(), window.dimensions().column - 1 - column_offset),
                'h' => window.scroll_columns(-count.max(1)),
                'j' => scroll_window(context, count.max(1), OnHiddenCursor::PreserveSelections)?,
                'k' => scroll_window(context, -count.max(1), OnHiddenCursor::PreserveSelections)?,
                'l' => window.scroll_columns(count.max(1)),
                _ => return Err(Error::runtime("key not mapped")),
            }
            Ok(())
        }),
        (if lock { "view (lock)" } else { "view" }).as_bytes().to_vec(),
        build_autoinfo_for_mapping(
            context,
            KeymapMode::View,
            &[
                ki(&[key('v'), key('c')], "center cursor (vertically)"),
                ki(&[key('m')], "center cursor (horizontally)"),
                ki(&[key('t')], "cursor on top"),
                ki(&[key('b')], "cursor on bottom"),
                ki(&[key('<')], "cursor on left"),
                ki(&[key('>')], "cursor on right"),
                ki(&[key('h')], "scroll left"),
                ki(&[key('j')], "scroll down"),
                ki(&[key('k')], "scroll up"),
                ki(&[key('l')], "scroll right"),
            ],
        ),
    )
}

fn replace_with_char(context: &Context, _: NormalParams) -> Result<()> {
    on_next_key_with_autoinfo(
        context,
        b"replace-char",
        KeymapMode::None,
        Rc::new(|key: Key, context: &Context| {
            let Some(cp) = key.codepoint() else { return Ok(()) };
            if key == Key::new(named::ESCAPE) {
                return Ok(());
            }
            let _edition = ScopedEdition::new(context);
            let _selection_edition = ScopedSelectionEdition::new(context);
            let buffer = context.try_buffer()?;
            let mut sels = context.selections();
            sels.merge_overlapping();
            sels.for_each(
                |_, sel| {
                    let count = char_length_sel(&buffer, sel);
                    replace_selection(&buffer, sel, &string_of_cp(cp, count))
                },
                true,
            )
        }),
        b"replace with char".to_vec(),
        b"enter char to replace with\n".to_vec(),
    )
}

fn swap_case(cp: Codepoint) -> Codepoint {
    let res = to_lower(cp);
    if res == cp { to_upper(cp) } else { res }
}

fn for_each_codepoint(context: &Context, func: fn(Codepoint) -> Codepoint) -> Result<()> {
    let _edition = ScopedEdition::new(context);
    let _selection_edition = ScopedSelectionEdition::new(context);
    let buffer = context.try_buffer()?;
    context.selections().for_each(
        |_, sel| {
            let s = {
                let lines = buffer.lines();
                let begin = BufferIterator::new(&lines, sel.min());
                let end = BufferIterator::end(&lines);
                let last = utf8::it_next(&BufferIterator::new(&lines, sel.max()), &end);
                let mut it = begin;
                let mut s = Vec::new();
                while it != last {
                    utf8::dump(&mut s, func(utf8::it_codepoint(&it, &end)));
                    utf8::it_to_next(&mut it, &end);
                }
                s
            };
            replace_selection(&buffer, sel, &s)
        },
        false,
    )
}

// command prompt ---------------------------------------------------------------

pub fn command_with_env(context: &Context, env_vars: EnvVarMap, reg: u8) -> Result<()> {
    let default_command = context.main_sel_register_value(&[if reg != 0 { reg } else { b':' }])?;
    let completer = Rc::new(Completer::default());
    let default_for_cb = default_command.clone();
    context.try_input_handler()?.prompt(
        b":",
        Vec::new(),
        default_command,
        context.faces().face("Prompt"),
        PromptFlags::DropHistoryEntriesWithBlankPrefix | PromptFlags::Command,
        b':',
        Some(Rc::new(move |context: &Context, cmd_line: &[u8], pos: usize| completer.complete(context, cmd_line, pos))),
        Rc::new(move |cmdline: &[u8], event: PromptEvent, context: &Context| {
            if let Ok(client) = context.try_client() {
                client.info_hide(false);
                if event == PromptEvent::Change {
                    let info = CommandManager::instance().command_info(context, cmdline);
                    context.try_input_handler()?.set_prompt_face(
                        context.faces().face(if info.is_some() || cmdline.is_empty() { "Prompt" } else { "Error" }),
                    );

                    let autoinfo = context.options().val::<AutoInfo>("autoinfo");
                    if autoinfo.has(AutoInfo::Command) {
                        if cmdline.len() == 1 && is_horizontal_blank(cmdline[0] as u32) {
                            client.info_show_str(
                                b"prompt",
                                b"commands preceded by a blank wont be saved to history",
                                BufferCoord::default(),
                                InfoStyle::Prompt,
                            );
                        } else if let Some(info) = info.filter(|i| !i.info.is_empty()) {
                            client.info_show_str(&info.name, &info.info, BufferCoord::default(), InfoStyle::Prompt);
                        }
                    }
                }
            }
            if event == PromptEvent::Validate {
                let cmdline = if cmdline.is_empty() { &default_for_cb[..] } else { cmdline };
                CommandManager::instance().execute(cmdline, context, &ShellContext { params: Vec::new(), env_vars: env_vars.clone() })?;
            }
            Ok(())
        }),
    )
}

fn command(context: &Context, params: NormalParams) -> Result<()> {
    let mut env_vars = EnvVarMap::new();
    env_vars.insert(b"count".to_vec(), params.count.to_string().into_bytes());
    env_vars.insert(b"register".to_vec(), vec![params.reg]);
    command_with_env(context, env_vars, params.reg)
}

pub fn apply_diff(buffer: &Buffer, pos: BufferCoord, lines_before: &[&[u8]], after: &[u8]) -> Result<BufferRange> {
    let mut first = pos;
    let mut pos = pos;
    let lines_after = split_after(after, b'\n');

    let byte_count = |lines: &[&[u8]], first: usize, count: usize| -> ByteCount {
        lines[first..first + count].iter().map(|l| l.len() as ByteCount).sum()
    };

    let mut diffs = Vec::new();
    for_each_diff(
        lines_before.len() as i32,
        lines_after.len() as i32,
        |i, j| lines_before[i as usize] == lines_after[j as usize],
        |op, len| diffs.push((op, len as usize)),
    );

    let mut tried_to_erase_final_newline = false;
    let (mut pos_a, mut pos_b) = (0usize, 0usize);
    for (op, len) in diffs {
        match op {
            DiffOp::Keep => {
                pos = buffer.advance(pos, byte_count(lines_before, pos_a, len));
                pos_a += len;
                pos_b += len;
            }
            DiffOp::Add => {
                if buffer.is_end(pos) {
                    tried_to_erase_final_newline = false;
                }
                let content: Vec<u8> = lines_after[pos_b..pos_b + len].concat();
                pos = buffer.insert(pos, &content)?.end;
                pos_b += len;
            }
            DiffOp::Remove => {
                let end = buffer.advance(pos, byte_count(lines_before, pos_a, len));
                tried_to_erase_final_newline |= buffer.is_end(end);
                pos = buffer.erase(pos, end)?;
                pos_a += len;
            }
        }
    }

    if tried_to_erase_final_newline {
        first = first.min(buffer.back_coord());
        pos = buffer.erase(buffer.back_coord(), buffer.end_coord())?;
    }

    Ok(BufferRange::new(first, pos))
}

/// A selection edition guard owned by a prompt callback (C++ captures a
/// ScopedSelectionEdition in the lambda)
struct OwnedSelectionEdition {
    input_handler: Weak<InputHandler>,
    valid: bool,
}

impl OwnedSelectionEdition {
    fn new(context: &Context) -> Self {
        let valid = !context.flags().has(crate::context::ContextFlags::Draft) && context.has_buffer();
        if valid {
            context.begin_selection_edition();
        }
        OwnedSelectionEdition {
            input_handler: context.try_input_handler().map(|ih| Rc::downgrade(&ih)).unwrap_or_default(),
            valid,
        }
    }
}

impl Drop for OwnedSelectionEdition {
    fn drop(&mut self) {
        if self.valid
            && let Some(ih) = self.input_handler.upgrade() {
                ih.context().end_selection_edition();
            }
    }
}

fn pipe(context: &Context, params: NormalParams, replace: bool) -> Result<()> {
    let prompt: &[u8] = if replace { b"pipe:" } else { b"pipe-to:" };
    let default_command = context.main_sel_register_value(&[if params.reg != 0 { params.reg } else { b'|' }])?;
    let selection_edition = OwnedSelectionEdition::new(context);
    let default_for_cb = default_command.clone();
    context.try_input_handler()?.prompt(
        prompt,
        Vec::new(),
        default_command,
        context.faces().face("Prompt"),
        PromptFlags::DropHistoryEntriesWithBlankPrefix,
        b'|',
        Some(Rc::new(shell_complete)),
        Rc::new(move |cmdline: &[u8], event: PromptEvent, context: &Context| {
            let _ = &selection_edition;
            if event != PromptEvent::Validate {
                return Ok(());
            }

            let cmdline = if cmdline.is_empty() { &default_for_cb[..] } else { cmdline };
            if cmdline.is_empty() {
                return Ok(());
            }

            let buffer = context.try_buffer()?;
            if replace {
                buffer.throw_if_read_only()?;
                let _edition = ScopedEdition::new(context);
                let mut changes_tracker = ForwardChangesTracker::default();
                let mut timestamp = buffer.timestamp();
                let mut selections = context.selections().clone();
                for idx in 0..selections.size() {
                    let sel = selections[idx].clone();
                    let first = changes_tracker.get_new_coord_tolerant(sel.min());
                    let last = changes_tracker.get_new_coord_tolerant(sel.max());

                    let mut keep_alive: Vec<StringDataPtr> = Vec::new();
                    let mut ranges: Vec<(usize, usize)> = Vec::new();
                    for line in first.line..=last.line {
                        let storage = buffer.line(line);
                        let mut b = 0usize;
                        let mut e = storage.len();
                        if line == last.line {
                            e = (last.column as usize + utf8::codepoint_size_of_byte(storage[last.column as usize])).min(storage.len());
                        }
                        if line == first.line {
                            b = first.column as usize;
                        }
                        ranges.push((b, e.max(b)));
                        keep_alive.push(storage);
                    }
                    let in_lines: Vec<Vec<u8>> =
                        keep_alive.iter().zip(ranges.iter()).map(|(s, (b, e))| s[*b..*e].to_vec()).collect();

                    // Needed in case we read selections inside the cmdline
                    context
                        .selections_write_only()
                        .set(vec![selectors::keep_direction(Selection::from_range(first, last), &sel)], 0);

                    let mut lines_iter = in_lines.clone().into_iter();
                    let (mut out, _) = ShellManager::instance().eval_with_generator(
                        cmdline,
                        context,
                        Box::new(move || lines_iter.next().unwrap_or_default()),
                        ShellFlags::WaitForStdout,
                        &ShellContext::default(),
                    )?;

                    if in_lines.last().and_then(|l| l.last()) != Some(&b'\n') && out.last() == Some(&b'\n') {
                        out.pop();
                    }

                    let in_refs: Vec<&[u8]> = in_lines.iter().map(|l| l.as_slice()).collect();
                    let range = apply_diff(&buffer, first, &in_refs, &out)?;
                    let (new_first, mut new_end) = (range.begin, range.end);
                    let sel = &mut selections[idx];
                    if new_first != new_end {
                        let (min, max) = sel.min_max_mut();
                        *min = new_first;
                        *max = buffer.char_prev(new_end);
                    } else {
                        if new_end != BufferCoord::default() {
                            new_end = buffer.char_prev(new_end);
                        }
                        sel.set_pos(new_end);
                    }
                    changes_tracker.update_from_buffer(&buffer, &mut timestamp);
                }
                selections.force_timestamp(timestamp);
                *context.selections_write_only() = selections;
            } else {
                let (count, old_main) = {
                    let selections = context.selections();
                    (selections.size(), selections.main_index())
                };
                for i in 0..count {
                    let sel = {
                        let mut selections = context.selections();
                        selections.set_main_index(i);
                        selections.main().clone()
                    };
                    ShellManager::instance().eval(
                        cmdline,
                        context,
                        &content(&buffer, &sel),
                        ShellFlags::None,
                        &ShellContext::default(),
                    )?;
                }
                context.selections().set_main_index(old_main);
            }
            Ok(())
        }),
    )
}

fn yank(context: &Context, params: NormalParams) -> Result<()> {
    let reg = if params.reg != 0 { params.reg } else { b'"' };
    RegisterManager::instance().get(&[reg])?.set(context, &context.selections_content(), false)?;
    context.print_status(DisplayLine::from_str_face(
        kformat!("yanked {} selections to register {}", context.selections().size(), [reg]),
        context.faces().face("Information"),
    ));
    Ok(())
}

fn erase_selections(context: &Context, params: NormalParams, yank: bool) -> Result<()> {
    if yank {
        let reg = if params.reg != 0 { params.reg } else { b'"' };
        RegisterManager::instance().get(&[reg])?.set(context, &context.selections_content(), false)?;
    }
    let _edition = ScopedEdition::new(context);
    let _selection_edition = ScopedSelectionEdition::new(context);
    context.selections().erase()
}

fn change(context: &Context, params: NormalParams, yank: bool) -> Result<()> {
    if yank {
        let reg = if params.reg != 0 { params.reg } else { b'"' };
        RegisterManager::instance().get(&[reg])?.set(context, &context.selections_content(), false)?;
    }
    enter_insert_mode(context, InsertMode::Replace, params)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PasteMode {
    Append,
    Insert,
    Replace,
}

pub fn paste_pos(buffer: &Buffer, min: BufferCoord, max: BufferCoord, mode: PasteMode, linewise: bool) -> BufferCoord {
    match mode {
        PasteMode::Append => {
            if buffer.is_end(max) {
                return max;
            }
            if linewise { BufferCoord::line(buffer.line_count().min(max.line + 1)) } else { buffer.char_next(max) }
        }
        PasteMode::Insert => {
            if linewise { BufferCoord::line(min.line) } else { min }
        }
        PasteMode::Replace => BufferCoord::default(),
    }
}

fn paste(context: &Context, params: NormalParams, mode: PasteMode) -> Result<()> {
    let reg = if params.reg != 0 { params.reg } else { b'"' };
    let strings = RegisterManager::instance().get(&[reg])?.get(context)?;
    let linewise = strings.iter().all(|s| s.last() == Some(&b'\n'));
    let buffer = context.try_buffer()?;
    let _edition = ScopedEdition::new(context);
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut last = BufferCoord::default();
    context.selections().for_each(
        |index, sel| {
            let s = &strings[index % strings.len()];
            let (min, max) = sel.min_max_mut();
            let range = if mode == PasteMode::Replace {
                buffer.replace(*min, buffer.char_next(*max), s)?
            } else {
                buffer.insert(paste_pos(&buffer, *min, (*max).max(last), mode, linewise), s)?
            };
            *min = range.begin;
            *max = if range.end > range.begin { buffer.char_prev(range.end) } else { range.begin };
            last = *max;
            Ok(())
        },
        mode == PasteMode::Append,
    )
}

fn paste_all(context: &Context, params: NormalParams, mode: PasteMode) -> Result<()> {
    let reg = if params.reg != 0 { params.reg } else { b'"' };
    let strings = RegisterManager::instance().get(&[reg])?.get(context)?;
    let mut all = Vec::new();
    let mut offsets: Vec<(ByteCount, ByteCount)> = Vec::new();
    for s in &strings {
        if s.is_empty() {
            continue;
        }
        all.extend_from_slice(s);
        let cursor_offset = utf8::previous(s, s.len(), 0) as ByteCount;
        offsets.push((cursor_offset, s.len() as ByteCount));
    }
    let linewise = strings.iter().all(|s| s.last() == Some(&b'\n'));

    if offsets.is_empty() {
        return Err(Error::runtime("nothing to paste"));
    }

    let buffer = context.try_buffer()?;
    let mut result: Vec<Selection> = Vec::new();
    let _selection_edition = ScopedSelectionEdition::new(context);
    {
        let _edition = ScopedEdition::new(context);
        context.selections().for_each(
            |_, sel| {
                let range = if mode == PasteMode::Replace {
                    buffer.replace(sel.min(), buffer.char_next(sel.max()), &all)?
                } else {
                    buffer.insert(paste_pos(&buffer, sel.min(), sel.max(), mode, linewise), &all)?
                };

                let mut pos = range.begin;
                for (cursor_offset, length) in &offsets {
                    let cursor = buffer.advance(pos, *cursor_offset);
                    result.push(Selection::from_range(pos, cursor));
                    pos = buffer.advance(cursor, length - cursor_offset);
                }
                Ok(())
            },
            mode == PasteMode::Append,
        )?;
    }
    context.selections().assign(result);
    Ok(())
}

fn insert_output(context: &Context, params: NormalParams, mode: PasteMode) -> Result<()> {
    let prompt: &[u8] = if mode == PasteMode::Insert { b"insert-output:" } else { b"append-output:" };
    let default_command = context.main_sel_register_value(&[if params.reg != 0 { params.reg } else { b'|' }])?;
    let selection_edition = OwnedSelectionEdition::new(context);
    let default_for_cb = default_command.clone();
    context.try_input_handler()?.prompt(
        prompt,
        Vec::new(),
        default_command,
        context.faces().face("Prompt"),
        PromptFlags::DropHistoryEntriesWithBlankPrefix,
        b'|',
        Some(Rc::new(shell_complete)),
        Rc::new(move |cmdline: &[u8], event: PromptEvent, context: &Context| {
            let _ = &selection_edition;
            if event != PromptEvent::Validate {
                return Ok(());
            }

            let cmdline = if cmdline.is_empty() { &default_for_cb[..] } else { cmdline };
            if cmdline.is_empty() {
                return Ok(());
            }

            let _edition = ScopedEdition::new(context);
            let buffer = context.try_buffer()?;
            let mut selections = context.selections().clone();
            let old_main = selections.main_index();

            let res = selections.for_each(
                |index, sel| {
                    context.selections().set_main_index(index);
                    let (out, _) = ShellManager::instance().eval(
                        cmdline,
                        context,
                        &content(&buffer, sel),
                        ShellFlags::WaitForStdout,
                        &ShellContext::default(),
                    )?;

                    let (min, max) = (sel.min(), sel.max());
                    let range = insert_at(&buffer, sel, paste_pos(&buffer, min, max, mode, false), &out)?;
                    let (min, max) = sel.min_max_mut();
                    *min = range.begin;
                    *max = if range.end > range.begin { buffer.char_prev(range.end) } else { range.begin };
                    Ok(())
                },
                mode == PasteMode::Append,
            );
            selections.set_main_index(old_main);
            *context.selections_write_only() = selections;
            res
        }),
    )
}

fn direction_flags(mode: u8) -> RegexCompileFlags {
    if mode & RegexMode::FORWARD != 0 {
        RegexCompileFlags::NONE
    } else {
        RegexCompileFlags::BACKWARD | RegexCompileFlags::NO_FORWARD
    }
}

type RegexPromptFunc = Rc<dyn Fn(Regex, PromptEvent, &Context) -> Result<()>>;

fn regex_prompt(context: &Context, prompt: Vec<u8>, reg: u8, mode: u8, func: RegexPromptFunc) -> Result<()> {
    let position = context.try_window().map(|w| w.position()).unwrap_or_default();
    let selections = RefCell::new(context.selections().clone());
    let main_index = context.selections().main_index();
    let register = RegisterManager::instance().get(&[reg])?;
    let default_regex = register.get_main(context, main_index)?;
    let saved_reg = register.save(context)?;
    let selection_edition = OwnedSelectionEdition::new(context);
    let default_for_cb = default_regex.clone();

    let completer: PromptCompleter = Rc::new(|context: &Context, regex: &[u8], pos: usize| {
        let current_word = |s: &[u8]| -> (usize, usize) {
            let mut it = s.len();
            while it != 0 && is_word_default(s[it - 1] as i8 as i32 as u32) {
                it -= 1;
            }
            if it == 0 || it == s.len() {
                return (it, s.len());
            }
            let mut backslashes = 0;
            let mut bs = it;
            while bs != 0 && s[bs - 1] == b'\\' {
                backslashes += 1;
                bs -= 1;
            }
            if backslashes % 2 == 1 { (it + 1, s.len()) } else { (it, s.len()) }
        };
        let prefix = &regex[..pos.min(regex.len())];
        let (wb, we) = current_word(prefix);
        let word = &prefix[wb..we];
        let Ok(buffer) = context.try_buffer() else { return Completions::default() };
        let matches = get_word_db(&buffer).find_matching(word);
        const MAX_COUNT: usize = 100;
        let mut candidates = CandidateList::new();
        let word_letters = used_letters(word);
        for_n_best(
            matches,
            MAX_COUNT,
            |a, b| {
                RankedMatch::with_letters(&a.0, a.1.letters, word, word_letters)
                    .less(&RankedMatch::with_letters(&b.0, b.1.letters, word, word_letters))
            },
            |m| {
                candidates.push(m.0.to_vec());
                true
            },
        );
        Completions::with(wb, pos, candidates, crate::completion::CompletionFlags::None)
    });

    context.try_input_handler()?.prompt(
        &prompt,
        Vec::new(),
        default_regex,
        context.faces().face("Prompt"),
        PromptFlags::Search,
        reg,
        Some(completer),
        Rc::new(move |s: &[u8], event: PromptEvent, context: &Context| {
            let _ = &selection_edition;
            let res = (|| -> Result<()> {
                if event != PromptEvent::Change
                    && let Ok(client) = context.try_client() {
                        client.info_hide(false);
                    }

                let incsearch = context.options().val::<bool>("incsearch");
                if incsearch {
                    let sels = {
                        let mut sels = selections.borrow_mut();
                        sels.update(true);
                        sels.clone()
                    };
                    *context.selections_write_only() = sels;
                    if let Ok(window) = context.try_window() {
                        window.set_position(position);
                    }
                    context.try_input_handler()?.set_prompt_face(context.faces().face("Prompt"));
                    RegisterManager::instance().get(&[reg])?.restore(context, &saved_reg)?;
                }

                match event {
                    PromptEvent::Abort => return Ok(()),
                    PromptEvent::Change => {
                        if !incsearch {
                            return Ok(());
                        }
                        if !s.is_empty() {
                            RegisterManager::instance().get(&[reg])?.set(context, &[s.to_vec()], false)?;
                        }
                    }
                    PromptEvent::Validate => {
                        if !s.is_empty() {
                            RegisterManager::instance().get(&[reg])?.set(context, &[s.to_vec()], false)?;
                        }
                        context.push_jump(false);
                    }
                }

                let re = Regex::new(if s.is_empty() { &default_for_cb } else { s }, direction_flags(mode))?;
                func(re, event, context)
            })();
            match res {
                Ok(()) => Ok(()),
                Err(err) if err.kind == ErrorKind::RegexError => {
                    if event == PromptEvent::Validate {
                        Err(err)
                    } else {
                        context.try_input_handler()?.set_prompt_face(context.faces().face("Error"));
                        Ok(())
                    }
                }
                Err(err) if err.is_runtime() => {
                    *context.selections_write_only() = selections.borrow().clone();
                    // only validation should propagate errors,
                    // incremental search should not.
                    if event == PromptEvent::Validate { Err(err) } else { Ok(()) }
                }
                Err(err) => Err(err),
            }
        }),
    )
}

fn select_next_matches(context: &Context, regex: &Regex, mode: u8, count: i32) -> Result<()> {
    let mut count = count;
    loop {
        let sels: Vec<Selection> = context.selections().as_slice().to_vec();
        let mut new_sels = Vec::with_capacity(sels.len());
        let mut wrapped = false;
        for sel in &sels {
            new_sels.push(selectors::keep_direction(selectors::find_next_match(context, sel, regex, mode, &mut wrapped)?, sel));
        }
        {
            let mut selections = context.selections();
            for (i, s) in new_sels.into_iter().enumerate() {
                selections[i] = s;
            }
            selections.sort_and_merge_overlapping();
        }
        count -= 1;
        if count <= 0 {
            break;
        }
    }
    Ok(())
}

fn extend_to_next_matches(context: &Context, regex: &Regex, mode: u8, count: i32) -> Result<()> {
    let mut count = count;
    loop {
        let (sels, mut main_index) = {
            let selections = context.selections();
            (selections.as_slice().to_vec(), selections.main_index())
        };
        let mut new_sels: Vec<Selection> = Vec::new();
        let mut wrapped = false;
        for sel in &sels {
            let new_sel = selectors::find_next_match(context, sel, regex, mode, &mut wrapped)?;
            if !wrapped {
                let mut s = sel.clone();
                merge_selections(&mut s, &new_sel);
                new_sels.push(s);
            } else if new_sels.len() <= main_index && main_index != 0 {
                main_index -= 1;
            }
        }
        if new_sels.is_empty() {
            return Err(Error::runtime("All selections wrapped"));
        }
        context.selections().set(new_sels, main_index);
        count -= 1;
        if count <= 0 {
            break;
        }
    }
    Ok(())
}

fn search(context: &Context, params: NormalParams, mode: SelectMode, regex_mode: u8) -> Result<()> {
    let forward = regex_mode & RegexMode::FORWARD != 0;
    let prompt: &str = if mode == SelectMode::Extend {
        if forward { "search (extend):" } else { "reverse search (extend):" }
    } else if forward {
        "search:"
    } else {
        "reverse search:"
    };
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '/' as u32 }) as u8;
    let count = params.count;
    regex_prompt(
        context,
        prompt.as_bytes().to_vec(),
        reg,
        regex_mode,
        Rc::new(move |regex: Regex, _event, context: &Context| {
            if regex.empty() || regex.str().is_empty() {
                return Ok(());
            }
            if mode == SelectMode::Extend {
                extend_to_next_matches(context, &regex, regex_mode, count)
            } else {
                select_next_matches(context, &regex, regex_mode, count)
            }
        }),
    )
}

fn search_next(context: &Context, params: NormalParams, mode: SelectMode, regex_mode: u8) -> Result<()> {
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '/' as u32 }) as u8;
    let s = RegisterManager::instance().get(&[reg])?.get(context)?.swap_remove(0);
    if s.is_empty() {
        return Err(Error::runtime("no search pattern"));
    }

    let regex = Regex::new(&s, direction_flags(regex_mode))?;
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut main_wrapped = false;
    let mut count = params.count;
    loop {
        let mut wrapped = false;
        let main = context.selections().main().clone();
        let found = selectors::keep_direction(selectors::find_next_match(context, &main, &regex, regex_mode, &mut wrapped)?, &main);
        {
            let mut selections = context.selections();
            if mode == SelectMode::Replace {
                *selections.main_mut() = found;
            } else if mode == SelectMode::Append {
                selections.push(found);
                let n = selections.size();
                selections.set_main_index(n - 1);
            }
            selections.sort_and_merge_overlapping();
        }
        main_wrapped = main_wrapped || wrapped;
        count -= 1;
        if count <= 0 {
            break;
        }
    }

    if main_wrapped {
        context.print_status(DisplayLine::from_str_face(
            "main selection search wrapped around buffer",
            context.faces().face("Information"),
        ));
    }
    Ok(())
}

fn use_selection_as_search_pattern(context: &Context, params: NormalParams, smart: bool) -> Result<()> {
    let buffer = context.try_buffer()?;
    let sels = context.selections().as_slice().to_vec();
    let mut patterns: Vec<Vec<u8>> = Vec::new();
    for sel in &sels {
        let beg = sel.min();
        let end = buffer.char_next(sel.max());
        let pattern = kformat!(
            "{}{}{}",
            if smart && is_bow(&buffer, beg) { "\\b" } else { "" },
            escape(&buffer.string(beg, end), b"^$\\.*+?()[]{}|", b'\\'),
            if smart && is_eow(&buffer, end) { "\\b" } else { "" }
        );
        if !patterns.contains(&pattern) {
            patterns.push(pattern);
        }
    }
    let pattern = join(&patterns, b'|', false);
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '/' as u32 }) as u8;
    context.print_status(DisplayLine::from_str_face(
        kformat!("register '{}' set to '{}'", [reg], pattern),
        context.faces().face("Information"),
    ));

    RegisterManager::instance().get(&[reg])?.set(context, &[pattern], false)?;

    // Hack, as Window do not take register state into account
    if let Ok(client) = context.try_client() {
        client.force_redraw(false);
    }
    Ok(())
}

fn select_regex(context: &Context, params: NormalParams) -> Result<()> {
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '/' as u32 }) as u8;
    let capture = params.count;
    let prompt = if capture != 0 { kformat!("select (capture {}):", capture) } else { b"select:".to_vec() };
    regex_prompt(
        context,
        prompt,
        reg,
        RegexMode::FORWARD,
        Rc::new(move |ex: Regex, _event, context: &Context| {
            if !ex.empty() && !ex.str().is_empty() {
                let sels = context.selections().clone();
                let buffer = sels.buffer().clone();
                let res = selectors::select_matches(&buffer, sels.as_slice(), &ex, capture)?;
                // [FAITHFUL] the C++ builds a SelectionList from the vector, which
                // (unlike assigning a vector) does not sort it
                *context.selections() = SelectionList::from_vec(buffer, res);
            }
            Ok(())
        }),
    )
}

fn split_regex(context: &Context, params: NormalParams) -> Result<()> {
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '/' as u32 }) as u8;
    let capture = params.count;
    let prompt = if capture != 0 { kformat!("split (on capture {}):", capture) } else { b"split:".to_vec() };
    regex_prompt(
        context,
        prompt,
        reg,
        RegexMode::FORWARD,
        Rc::new(move |ex: Regex, _event, context: &Context| {
            if !ex.empty() && !ex.str().is_empty() {
                let sels = context.selections().clone();
                let buffer = sels.buffer().clone();
                let res = selectors::split_on_matches(&buffer, sels.as_slice(), &ex, capture)?;
                // [FAITHFUL] the C++ builds a SelectionList from the vector, which
                // (unlike assigning a vector) does not sort it
                *context.selections() = SelectionList::from_vec(buffer, res);
            }
            Ok(())
        }),
    )
}

fn split_lines(context: &Context, params: NormalParams) -> Result<()> {
    let count: LineCount = if params.count == 0 { 1 } else { params.count };
    let _selection_edition = ScopedSelectionEdition::new(context);
    let buffer = context.try_buffer()?;
    let mut selections = context.selections();
    let mut res = Vec::new();
    for sel in selections.iter() {
        if sel.anchor().line == sel.cursor().line {
            res.push(sel.clone());
            continue;
        }
        let min = sel.min();
        let max = sel.max();
        let mut line = min.line;
        while line <= max.line {
            let last_line = (line + count - 1).min(buffer.line_count() - 1);
            res.push(selectors::keep_direction(
                Selection::from_range(
                    min.max(BufferCoord::line(line)),
                    max.min(BufferCoord::new(last_line, buffer.line_length(last_line) - 1)),
                ),
                sel,
            ));
            line += count;
        }
    }
    selections.assign(res);
    Ok(())
}

fn select_boundaries(context: &Context, _: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut selections = context.selections();
    let mut res = Vec::new();
    for sel in selections.iter() {
        res.push(Selection::new(sel.min()));
        if sel.min() != sel.max() {
            res.push(Selection::new(sel.max()));
        }
    }
    selections.assign(res);
    Ok(())
}

fn join_lines_select_spaces(context: &Context, _: NormalParams) -> Result<()> {
    let buffer = context.try_buffer()?;
    let mut selections = Vec::new();
    let _selection_edition = ScopedSelectionEdition::new(context);
    {
        let sels = context.selections();
        let lines = buffer.lines();
        for sel in sels.iter() {
            let min_line = sel.min().line;
            let max_line = sel.max().line;
            let end_line = (buffer.line_count() - 1).min(max_line + if min_line == max_line { 1 } else { 0 });
            for line in min_line..end_line {
                let begin = BufferIterator::new(&lines, BufferCoord::new(line, lines[line as usize].len() as i32 - 1));
                let end_it = BufferIterator::end(&lines);
                let mut end = begin.next_byte();
                while end != end_it && is_horizontal_blank(crate::utf8::ByteIt::get(&end) as i8 as i32 as u32) {
                    crate::utf8::ByteIt::inc(&mut end);
                }
                selections.push(Selection::from_range(begin.coord(), end.prev_byte().coord()));
            }
        }
    }
    if selections.is_empty() {
        return Ok(());
    }
    context.selections_write_only().assign(selections);
    context.selections().merge_consecutive();
    let _edition = ScopedEdition::new(context);
    context.selections().replace(&[b" ".to_vec()])
}

fn join_lines(context: &Context, params: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut sels = context.selections().clone();
    let res = join_lines_select_spaces(context, params);
    sels.update(true);
    *context.selections_write_only() = sels;
    res
}

fn keep(context: &Context, params: NormalParams, matching: bool) -> Result<()> {
    let prompt: &[u8] = if matching { b"keep matching:" } else { b"keep not matching:" };
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '/' as u32 }) as u8;
    let selection_edition = Rc::new(OwnedSelectionEdition::new(context));
    regex_prompt(
        context,
        prompt.to_vec(),
        reg,
        RegexMode::FORWARD,
        Rc::new(move |regex: Regex, _event, context: &Context| {
            let _ = &selection_edition;
            if regex.empty() || regex.str().is_empty() {
                return Ok(());
            }

            let buffer = context.try_buffer()?;
            let sels = context.selections().as_slice().to_vec();
            let mut keep = Vec::new();
            for sel in &sels {
                let matched = {
                    let lines = buffer.lines();
                    let begin = BufferIterator::new(&lines, sel.min());
                    let end_it = BufferIterator::end(&lines);
                    let end = utf8::it_next(&BufferIterator::new(&lines, sel.max()), &end_it);
                    // We do not consider if end is on an eol, as it seems to
                    // give more intuitive behaviours in keep use cases.
                    let flags = match_flags(
                        is_bol(begin.coord()),
                        false,
                        is_bow_lines(&lines, begin.coord()),
                        is_eow_lines(&lines, end.coord()),
                    );
                    let mut idle = UrgentEventsIdle::default();
                    let r = regex_search(begin, end, begin, end, &regex, flags, || idle.idle());
                    idle.check()?;
                    r
                };
                if matched == matching {
                    keep.push(sel.clone());
                }
            }
            if keep.is_empty() {
                return Err(no_selections_remaining());
            }
            context.selections_write_only().assign(keep);
            Ok(())
        }),
    )
}

fn is_bow_lines(lines: &[StringDataPtr], coord: BufferCoord) -> bool {
    let it = BufferIterator::new(lines, coord);
    let begin = BufferIterator::begin(lines);
    let end = BufferIterator::end(lines);
    if coord == BufferCoord::new(0, 0) {
        return is_word_default(utf8::it_codepoint(&it, &end));
    }
    let prev = utf8::it_previous(&it, &begin);
    !is_word_default(utf8::it_codepoint(&prev, &it)) && is_word_default(utf8::it_codepoint(&it, &end))
}

fn is_eow_lines(lines: &[StringDataPtr], coord: BufferCoord) -> bool {
    let end = BufferIterator::end(lines);
    if coord >= end.coord() || coord == BufferCoord::new(0, 0) {
        return false;
    }
    let it = BufferIterator::new(lines, coord);
    let begin = BufferIterator::begin(lines);
    let prev = utf8::it_previous(&it, &begin);
    is_word_default(utf8::it_codepoint(&prev, &it)) && !is_word_default(utf8::it_codepoint(&it, &end))
}

fn keep_pipe(context: &Context, params: NormalParams) -> Result<()> {
    let default_command = context.main_sel_register_value(&[if params.reg != 0 { params.reg } else { b'|' }])?;
    let selection_edition = OwnedSelectionEdition::new(context);
    let default_for_cb = default_command.clone();
    context.try_input_handler()?.prompt(
        b"keep pipe:",
        Vec::new(),
        default_command,
        context.faces().face("Prompt"),
        PromptFlags::DropHistoryEntriesWithBlankPrefix,
        b'|',
        Some(Rc::new(shell_complete)),
        Rc::new(move |cmdline: &[u8], event: PromptEvent, context: &Context| {
            let _ = &selection_edition;
            if event != PromptEvent::Validate {
                return Ok(());
            }

            let cmdline = if cmdline.is_empty() { &default_for_cb[..] } else { cmdline };
            if cmdline.is_empty() {
                return Ok(());
            }

            let buffer = context.try_buffer()?;
            let shell_manager = ShellManager::instance();
            let mut keep = Vec::new();
            let (count, old_main) = {
                let selections = context.selections();
                (selections.size(), selections.main_index())
            };
            let mut new_main = usize::MAX;
            for i in 0..count {
                let sel = {
                    let mut selections = context.selections();
                    selections.set_main_index(i);
                    selections[i].clone()
                };
                if shell_manager.eval(cmdline, context, &content(&buffer, &sel), ShellFlags::None, &ShellContext::default())?.1 == 0 {
                    keep.push(sel);
                    if i >= old_main && new_main == usize::MAX {
                        new_main = keep.len() - 1;
                    }
                }
            }
            if keep.is_empty() {
                return Err(no_selections_remaining());
            }
            if new_main == usize::MAX {
                new_main = keep.len() - 1;
            }
            context.selections_write_only().set(keep, new_main);
            Ok(())
        }),
    )
}

fn indent(context: &Context, params: NormalParams, indent_empty: bool) -> Result<()> {
    let count: CharCount = if params.count != 0 { params.count } else { 1 };
    let indent_width: CharCount = context.options().val::<i32>("indentwidth");
    let indent = if indent_width == 0 {
        string_of_cp('\t' as u32, count)
    } else {
        string_of_cp(' ' as u32, indent_width * count)
    };

    let _edition = ScopedEdition::new(context);
    let buffer = context.try_buffer()?;
    let mut last_line = 0;
    let _selection_edition = ScopedSelectionEdition::new(context);
    let sels = context.selections().as_slice().to_vec();
    for sel in &sels {
        for line in last_line.max(sel.min().line)..sel.max().line + 1 {
            if indent_empty || buffer.line_length(line) > 1 {
                buffer.insert(BufferCoord::line(line), &indent)?;
            }
        }
        // avoid reindenting the same line if multiple selections are on it
        last_line = sel.max().line + 1;
    }
    Ok(())
}

fn deindent(context: &Context, params: NormalParams, deindent_incomplete: bool) -> Result<()> {
    let count: ColumnCount = if params.count != 0 { params.count } else { 1 };
    let tabstop = context.options().val::<i32>("tabstop");
    let mut indent_width = context.options().val::<i32>("indentwidth");
    if indent_width == 0 {
        indent_width = tabstop;
    }
    indent_width *= count;

    let buffer = context.try_buffer()?;
    let mut last_line = 0;
    let _selection_edition = ScopedSelectionEdition::new(context);
    let sels = context.selections().as_slice().to_vec();
    for sel in &sels {
        for line in sel.min().line.max(last_line)..sel.max().line + 1 {
            let mut width = 0;
            let content = buffer.line(line);
            for column in 0..content.len() {
                let c = content[column];
                if c == b'\t' {
                    width = (width / tabstop + 1) * tabstop;
                } else if c == b' ' {
                    width += 1;
                } else {
                    if deindent_incomplete && width != 0 {
                        buffer.erase(BufferCoord::line(line), BufferCoord::new(line, column as i32))?;
                    }
                    break;
                }
                if width >= indent_width {
                    buffer.erase(BufferCoord::line(line), BufferCoord::new(line, column as i32 + 1))?;
                    break;
                }
            }
        }
        // avoid reindenting the same line if multiple selections are on it
        last_line = sel.max().line + 1;
    }
    Ok(())
}

// objects ------------------------------------------------------------------

type NestedFn = Rc<dyn Fn(&Context, i32, ObjectFlags) -> Result<Vec<Selection>>>;

fn select_object(context: &Context, params: NormalParams, flags: ObjectFlags, mode: SelectMode) -> Result<()> {
    let get_title = || {
        let whole_flags = ObjectFlags::ToBegin | ObjectFlags::ToEnd;
        let whole = (flags & whole_flags) == whole_flags;
        kformat!(
            "{} {}{}{} object{}",
            if mode == SelectMode::Extend { "extend" } else { "select" },
            if whole { "" } else { "to " },
            if flags.has(ObjectFlags::Inner) { "inner " } else { "" },
            if flags.has(ObjectFlags::Nested) { "nested" } else { "surrounding" },
            if whole {
                ""
            } else if flags.has(ObjectFlags::ToBegin) {
                " begin"
            } else {
                " end"
            }
        )
    };

    on_next_key_with_autoinfo(
        context,
        b"text-object",
        KeymapMode::Object,
        Rc::new(move |pressed: Key, context: &Context| {
            if pressed == Key::new(named::ESCAPE) {
                return Ok(());
            }

            let nested = flags.has(ObjectFlags::Nested);
            let count = if params.count <= 0 { 0 } else { params.count - 1 };

            let select_nested_and_set_last = move |context: &Context, func: NestedFn| -> Result<()> {
                let f = func.clone();
                context.set_last_select(Rc::new(move |context: &Context| {
                    let _selection_edition = ScopedSelectionEdition::new(context);
                    let res = f(context, count, flags)?;
                    context.selections().assign(res);
                    Ok(())
                }));
                let _selection_edition = ScopedSelectionEdition::new(context);
                let res = func(context, count, flags)?;
                context.selections().assign(res);
                Ok(())
            };

            type ObjFn = fn(&Context, &Selection, i32, ObjectFlags) -> Option<Selection>;
            type NestFn = fn(&Context, i32, ObjectFlags) -> Result<Vec<Selection>>;
            let object_types: [(Key, ObjFn, NestFn); 8] = [
                (key('w'), |c, s, n, f| selectors::select_word(WordType::Word, c, s, n, f), |c, n, f| selectors::select_nested_words(WordType::Word, c, n, f)),
                (alt(key('w')), |c, s, n, f| selectors::select_word(WordType::WORD, c, s, n, f), |c, n, f| selectors::select_nested_words(WordType::WORD, c, n, f)),
                (key('s'), selectors::select_sentence, selectors::select_nested_sentences),
                (key('p'), selectors::select_paragraph, selectors::select_nested_paragraphs),
                (Key::new(named::SPACE), selectors::select_whitespaces, selectors::select_nested_whitespaces),
                (key('i'), selectors::select_indent, selectors::select_nested_indents),
                (key('n'), selectors::select_number, selectors::select_nested_numbers),
                (key('u'), selectors::select_argument, selectors::select_nested_arguments),
            ];

            if let Some((_, func, nested_func)) = object_types.iter().find(|(k, _, _)| *k == pressed) {
                let (func, nested_func) = (*func, *nested_func);
                if nested {
                    return select_nested_and_set_last(context, Rc::new(nested_func));
                }
                return select_and_set_last(context, mode, sel_fn(move |c, s| Ok(func(c, s, count, flags))));
            }

            let regex_selector = |open: &[u8], close: &[u8], count: i32, flags: ObjectFlags| -> Result<SelectorFn> {
                let open = Regex::new(open, RegexCompileFlags::BACKWARD)?;
                let close = Regex::new(close, RegexCompileFlags::BACKWARD)?;
                Ok(sel_fn(move |c, s| selectors::select_surrounding(c, s, &open, &close, count, flags)))
            };

            let nested_regex = |open: Regex, close: Regex| -> NestedFn {
                Rc::new(move |context: &Context, count: i32, flags: ObjectFlags| {
                    if open == close {
                        selectors::regex_select_nested_delimiter(context, &open, flags)
                    } else {
                        selectors::regex_select_nested(context, &open, &close, count, flags)
                    }
                })
            };

            if pressed == crate::keys::key('c') {
                let info = show_auto_info_ifn(
                    b"Enter object desc",
                    b"format: <open regex>,<close regex>\n        escape commas with '\\'",
                    AutoInfo::Command,
                    context,
                );
                return context.try_input_handler()?.prompt(
                    b"object desc:",
                    Vec::new(),
                    Vec::new(),
                    context.faces().face("Prompt"),
                    PromptFlags::None,
                    b'_',
                    Some(Rc::new(complete_nothing)),
                    Rc::new(move |cmdline: &[u8], event: PromptEvent, context: &Context| {
                        if event != PromptEvent::Change {
                            hide_auto_info_ifn(context, info);
                        }
                        if event != PromptEvent::Validate {
                            return Ok(());
                        }

                        let parts = split_escaped(cmdline, b',', Some(b'\\'));
                        let desc_error = || Error::runtime("desc parsing failed, expected <open>,<close>");
                        if parts.len() != 2 {
                            return Err(desc_error());
                        }
                        let open = unescape(parts[0], b",\\", b'\\');
                        let close = unescape(parts[1], b",\\", b'\\');
                        if open.is_empty() || close.is_empty() {
                            return Err(desc_error());
                        }

                        if nested {
                            let open = Regex::new(&open, RegexCompileFlags::NONE)?;
                            let close = Regex::new(&close, RegexCompileFlags::NONE)?;
                            select_nested_and_set_last(context, nested_regex(open, close))
                        } else {
                            select_and_set_last(context, mode, regex_selector(&open, &close, count, flags)?)
                        }
                    }),
                );
            }

            if pressed == alt(crate::keys::key(';')) {
                let mut env_vars = EnvVarMap::new();
                env_vars.insert(b"count".to_vec(), params.count.to_string().into_bytes());
                env_vars.insert(b"register".to_vec(), vec![params.reg]);
                env_vars.insert(b"select_mode".to_vec(), mode.elem_to_string(Quoting::Raw));
                env_vars.insert(b"object_flags".to_vec(), flags.elem_to_string(Quoting::Raw));
                return command_with_env(context, env_vars, 0);
            }

            const SURROUNDING_PAIRS: [(u8, u8, u8); 7] = [
                (b'(', b')', b'b'),
                (b'{', b'}', b'B'),
                (b'[', b']', b'r'),
                (b'<', b'>', b'a'),
                (b'"', b'"', b'Q'),
                (b'\'', b'\'', b'q'),
                (b'`', b'`', b'g'),
            ];
            if let Some((open, close, _)) = SURROUNDING_PAIRS.iter().find(|(o, c, n)| {
                pressed == crate::keys::key(*o as char) || pressed == crate::keys::key(*c as char) || pressed == crate::keys::key(*n as char)
            }) {
                let open_re = kformat!("\\Q{}", [*open]);
                let close_re = kformat!("\\Q{}", [*close]);
                if nested {
                    return select_nested_and_set_last(
                        context,
                        nested_regex(
                            Regex::new(&open_re, RegexCompileFlags::NONE)?,
                            Regex::new(&close_re, RegexCompileFlags::NONE)?,
                        ),
                    );
                }
                return select_and_set_last(context, mode, regex_selector(&open_re, &close_re, count, flags)?);
            }

            if let Some(cp) = pressed.codepoint()
                && is_punctuation(cp, &[]) {
                    let re = kformat!("\\Q{}", crate::format::Cp(cp));
                    if nested {
                        let regex = Regex::new(&re, RegexCompileFlags::NONE)?;
                        return select_nested_and_set_last(
                            context,
                            Rc::new(move |context: &Context, _count: i32, flags: ObjectFlags| {
                                selectors::regex_select_nested_delimiter(context, &regex, flags)
                            }),
                        );
                    }
                    return select_and_set_last(context, mode, regex_selector(&re, &re, count, flags)?);
                }
            Ok(())
        }),
        get_title(),
        build_autoinfo_for_mapping(
            context,
            KeymapMode::Object,
            &[
                ki(&[key('b'), key('('), key(')')], "parenthesis block"),
                ki(&[key('B'), key('{'), key('}')], "brace block"),
                ki(&[key('r'), key('['), key(']')], "bracket block"),
                ki(&[key('a'), key('<'), key('>')], "angle block"),
                ki(&[key('"'), key('Q')], "double quote string"),
                ki(&[key('\''), key('q')], "single quote string"),
                ki(&[key('`'), key('g')], "grave quote string"),
                ki(&[key('w')], "word"),
                ki(&[alt(key('w'))], "WORD"),
                ki(&[key('s')], "sentence"),
                ki(&[key('p')], "paragraph"),
                ki(&[Key::new(named::SPACE)], "whitespaces"),
                ki(&[key('i')], "indent"),
                ki(&[key('u')], "argument"),
                ki(&[key('n')], "number"),
                ki(&[key('c')], "custom object desc"),
                ki(&[alt(key(';'))], "run command in object context"),
            ],
        ),
    )
}

fn scroll(context: &Context, params: NormalParams, direction: Direction, half: bool) -> Result<()> {
    let window = context.try_window()?;
    let count = if params.count != 0 { params.count } else { 1 };
    let offset = (window.dimensions().line - 2) / (if half { 2 } else { 1 }) * count;
    scroll_window(context, offset * direction as i32, OnHiddenCursor::MoveCursorAndAnchor)
}

fn copy_selections_on_next_lines(context: &Context, params: NormalParams, direction: Direction) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    let buffer = context.try_buffer()?;
    let tabstop = context.options().val::<i32>("tabstop");
    let (sels, main) = {
        let selections = context.selections();
        (selections.as_slice().to_vec(), selections.main_index())
    };
    let mut result: Vec<Selection> = Vec::new();
    let mut main_index = 0;
    for (idx, sel) in sels.into_iter().enumerate() {
        let is_main = idx == main;
        let anchor = sel.anchor();
        let cursor = sel.cursor();
        let cursor_col = get_column(&buffer, tabstop, cursor.coord);
        let anchor_col = get_column(&buffer, tabstop, anchor);
        if is_main {
            main_index = result.len();
        }
        result.push(sel);
        let height = anchor.line.max(cursor.line) - anchor.line.min(cursor.line) + 1;
        let max_lines = params.count.max(1) as usize;
        let mut i = 0;
        let mut nb_sels = 0;
        while nb_sels < max_lines {
            let offset = direction as i32 * (i + 1) * height;
            i += 1;
            let anchor_line = anchor.line + offset;
            let cursor_line = cursor.line + offset;

            if anchor_line < 0 || cursor_line < 0 || anchor_line >= buffer.line_count() || cursor_line >= buffer.line_count() {
                break;
            }

            let anchor_byte = get_byte_to_column(&buffer, tabstop, DisplayCoord::new(anchor_line, anchor_col));
            let cursor_byte = get_byte_to_column(&buffer, tabstop, DisplayCoord::new(cursor_line, cursor_col));

            if anchor_byte != buffer.line_length(anchor_line) && cursor_byte != buffer.line_length(cursor_line) {
                if is_main {
                    main_index = result.len();
                }
                result.push(Selection::from_range(
                    BufferCoord::new(anchor_line, anchor_byte),
                    BufferCoordAndTarget::new(BufferCoord::new(cursor_line, cursor_byte), cursor.target, -1),
                ));
                nb_sels += 1;
            }
        }
    }
    let mut selections = context.selections();
    selections.set(result, main_index);
    selections.sort_and_merge_overlapping();
    Ok(())
}

fn rotate_selections(context: &Context, params: NormalParams, direction: Direction) -> Result<()> {
    let count = if params.count != 0 { params.count } else { 1 };
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut selections = context.selections();
    let index = selections.main_index() as i32;
    let num = selections.size() as i32;
    selections.set_main_index(if direction == Direction::Forward {
        ((index + count) % num) as usize
    } else {
        ((index + (num - count % num)) % num) as usize
    });
    Ok(())
}

fn rotate_selections_content(context: &Context, params: NormalParams, direction: Direction) -> Result<()> {
    let mut group = params.count as usize;
    let mut count = 1usize;
    let mut strings = context.selections_content();
    if group == 0 || group > strings.len() {
        group = strings.len();
    }
    count %= group;
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut main = context.selections().main_index();
    let mut it = 0;
    while it != strings.len() {
        let end = strings.len().min(it + group);
        let new_beg = if direction == Direction::Forward { end - count } else { it + count };
        strings[it..end].rotate_left(new_beg - it);
        if it <= main && main < end {
            main = if main < new_beg { end - (new_beg - main) } else { it + (main - new_beg) };
        }
        it = end;
    }
    let mut selections = context.selections();
    selections.replace(&strings)?;
    selections.set_main_index(main);
    Ok(())
}

crate::kak_flags! {
    pub struct SelectFlags: u32 {
        const None = 0;
        const Reverse = 1;
        const Inclusive = 2;
        const Extend = 4;
    }
}

fn select_to_next_char(context: &Context, params: NormalParams, flags: SelectFlags) -> Result<()> {
    let title = kformat!(
        "{} {} {} char",
        if flags.has(SelectFlags::Extend) { "extend" } else { "select" },
        if flags.has(SelectFlags::Inclusive) { "onto" } else { "to" },
        if flags.has(SelectFlags::Reverse) { "previous" } else { "next" }
    );
    on_next_key_with_autoinfo(
        context,
        b"to-char",
        KeymapMode::None,
        Rc::new(move |key: Key, context: &Context| {
            let Some(cp) = key.codepoint() else { return Ok(()) };
            if key == Key::new(named::ESCAPE) {
                return Ok(());
            }
            let new_flags = if flags.has(SelectFlags::Extend) { SelectMode::Extend } else { SelectMode::Replace };
            let count = params.count;
            select_and_set_last(
                context,
                new_flags,
                sel_fn(move |c, s| {
                    if flags.has(SelectFlags::Reverse) {
                        Ok(selectors::select_to_reverse(c, s, cp, count, flags.has(SelectFlags::Inclusive)))
                    } else {
                        Ok(selectors::select_to(c, s, cp, count, flags.has(SelectFlags::Inclusive)))
                    }
                }),
            )
        }),
        title,
        b"enter char to select to".to_vec(),
    )
}

fn start_or_end_macro_recording(context: &Context, params: NormalParams) -> Result<()> {
    let ih = context.try_input_handler()?;
    if ih.is_recording() {
        ih.stop_recording()
    } else {
        let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '@' as u32 });
        if !is_basic_alpha(reg) && reg != '@' as u32 {
            return Err(Error::runtime("macros can only use the '@' and alphabetic registers"));
        }
        ih.start_recording(reg);
        Ok(())
    }
}

thread_local! {
    static RUNNING_MACROS: RefCell<[bool; 27]> = const { RefCell::new([false; 27]) };
}

fn replay_macro(context: &Context, params: NormalParams) -> Result<()> {
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '@' as u32 });
    if !is_basic_alpha(reg) && reg != '@' as u32 {
        return Err(Error::runtime("macros can only use the '@' and alphabetic registers"));
    }

    let idx = if reg != '@' as u32 { (reg - 'a' as u32) as usize } else { 26 };
    if RUNNING_MACROS.with(|r| r.borrow()[idx]) {
        return Err(Error::runtime("recursive macros call detected"));
    }

    let reg_val = RegisterManager::instance().get_cp(reg)?.get(context)?;
    if reg_val.is_empty() || reg_val[0].is_empty() {
        return Err(rt_err!("register '{}' is empty", crate::format::Cp(reg)));
    }

    RUNNING_MACROS.with(|r| r.borrow_mut()[idx] = true);
    let _stop = crate::utils::OnScopeEnd::new(|| RUNNING_MACROS.with(|r| r.borrow_mut()[idx] = false));

    let keys = parse_keys(&reg_val[0])?;
    let _edition = ScopedEdition::new(context);
    let _selection_edition = ScopedSelectionEdition::new(context);
    let _disable_keymaps = ScopedSetBool::set(context.keymaps_disabled());
    let ih = context.try_input_handler()?;
    let mut count = params.count;
    loop {
        for key in &keys {
            ih.handle_key(*key, true)?;
        }
        count -= 1;
        if count <= 0 {
            break;
        }
    }
    Ok(())
}

fn jump(context: &Context, params: NormalParams, direction: Direction) -> Result<()> {
    let count = params.count.max(1);
    let jump = if direction == Direction::Forward { context.jump_forward(count)? } else { context.jump_backward(count)? };
    let oldbuf = context.try_buffer()?;
    let buffer = jump.buffer().clone();
    let _selection_edition = ScopedSelectionEdition::new(context);
    if !Rc::ptr_eq(&buffer, &oldbuf) {
        context.change_buffer(&buffer, None)?;
    }
    *context.selections_write_only() = jump;
    Ok(())
}

fn push_selections(context: &Context, _: NormalParams) -> Result<()> {
    context.push_jump(true);
    context.print_status(DisplayLine::from_str_face(
        kformat!("saved {} selections", context.selections().size()),
        context.faces().face("Information"),
    ));
    Ok(())
}

fn align(context: &Context, _: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    let buffer = context.try_buffer()?;
    let tabstop = context.options().val::<i32>("tabstop");

    let sels = context.selections().as_slice().to_vec();
    let mut columns: Vec<Vec<usize>> = Vec::new();
    let mut last_line = -1;
    let mut column = 0;
    for (i, sel) in sels.iter().enumerate() {
        let line = sel.cursor().line;
        if sel.anchor().line != line {
            return Err(Error::runtime("align cannot work with multi line selections"));
        }

        column = if line == last_line { column + 1 } else { 0 };
        if column >= columns.len() {
            columns.resize(column + 1, Vec::new());
        }
        columns[column].push(i);
        last_line = line;
    }

    let use_tabs = context.options().val::<bool>("aligntab");
    for col in &columns {
        let current: Vec<Selection> = context.selections().as_slice().to_vec();
        let mut maxcol = 0;
        for &i in col {
            maxcol = maxcol.max(get_column(&buffer, tabstop, current[i].cursor().coord));
        }
        for &i in col {
            let current: Vec<Selection> = context.selections_upd(false).as_slice().to_vec();
            let sel = &current[i];
            let insert_coord = sel.min();
            let lastcol = get_column(&buffer, tabstop, sel.cursor().coord);
            let inscount = maxcol - lastcol;
            let padstr = if !use_tabs {
                string_of_cp(' ' as u32, inscount)
            } else {
                let inscol = get_column(&buffer, tabstop, insert_coord);
                let targetcol = inscol + inscount;
                let tabcol = inscol - (inscol % tabstop);
                let tabs = (targetcol - tabcol) / tabstop;
                let spaces = targetcol - if tabs != 0 { tabcol + tabs * tabstop } else { inscol };
                let mut s = string_of_cp('\t' as u32, tabs);
                s.extend(string_of_cp(' ' as u32, spaces));
                s
            };
            buffer.insert(insert_coord, &padstr)?;
        }
        context.selections().update(true);
    }
    Ok(())
}

fn copy_indent(context: &Context, params: NormalParams) -> Result<()> {
    let mut selection = params.count;
    let buffer = context.try_buffer()?;
    let _selection_edition = ScopedSelectionEdition::new(context);
    let (sels, main_index) = {
        let selections = context.selections();
        (selections.as_slice().to_vec(), selections.main_index())
    };

    let mut lines = Vec::new();
    for sel in &sels {
        for l in sel.min().line..sel.max().line + 1 {
            lines.push(l);
        }
    }

    if selection > sels.len() as i32 {
        return Err(Error::runtime("invalid selection index"));
    }
    if selection == 0 {
        selection = main_index as i32 + 1;
    }

    let ref_line = sels[(selection - 1) as usize].min().line;
    let line = buffer.line(ref_line);
    let mut it = 0;
    while it != line.len() && is_horizontal_blank(line[it] as u32) {
        it += 1;
    }
    let indent = line[..it].to_vec();

    let _edition = ScopedEdition::new(context);
    for l in lines {
        if l == ref_line {
            continue;
        }

        let line = buffer.line(l);
        let mut i = 0;
        while i < line.len() && is_horizontal_blank(line[i] as u32) {
            i += 1;
        }
        buffer.replace(BufferCoord::line(l), BufferCoord::new(l, i as i32), &indent)?;
    }
    Ok(())
}

fn tabs_to_spaces(context: &Context, params: NormalParams) -> Result<()> {
    let buffer = context.try_buffer()?;
    let opt_tabstop = context.options().val::<i32>("tabstop");
    let tabstop = if params.count == 0 { opt_tabstop } else { params.count };
    let mut tabs = Vec::new();
    let mut spaces = Vec::new();
    let _selection_edition = ScopedSelectionEdition::new(context);
    let _edition = ScopedEdition::new(context);
    let sels = context.selections().as_slice().to_vec();
    {
        let lines = buffer.lines();
        for sel in &sels {
            let mut it = BufferIterator::new(&lines, sel.min());
            let end = BufferIterator::new(&lines, sel.max()).next_byte();
            while it != end {
                if crate::utf8::ByteIt::get(&it) == b'\t' {
                    let col = get_column(&buffer, opt_tabstop, it.coord());
                    let end_col = (col / tabstop + 1) * tabstop;
                    tabs.push(Selection::new(it.coord()));
                    spaces.push(string_of_cp(' ' as u32, end_col - col));
                }
                crate::utf8::ByteIt::inc(&mut it);
            }
        }
    }
    if !tabs.is_empty() {
        SelectionList::from_vec(buffer.clone(), tabs).replace(&spaces)?;
    }
    Ok(())
}

fn spaces_to_tabs(context: &Context, params: NormalParams) -> Result<()> {
    let buffer = context.try_buffer()?;
    let opt_tabstop = context.options().val::<i32>("tabstop");
    let tabstop = if params.count == 0 { opt_tabstop } else { params.count };
    let mut spaces = Vec::new();
    let _selection_edition = ScopedSelectionEdition::new(context);
    let _edition = ScopedEdition::new(context);
    let sels = context.selections().as_slice().to_vec();
    {
        let lines = buffer.lines();
        let get = |it: &BufferIterator| crate::utf8::ByteIt::get(it);
        for sel in &sels {
            let mut it = BufferIterator::new(&lines, sel.min());
            let end = BufferIterator::new(&lines, sel.max()).next_byte();
            while it != end {
                if get(&it) == b' ' {
                    let spaces_beg = it;
                    let mut spaces_end = spaces_beg.next_byte();
                    let mut col = get_column(&buffer, opt_tabstop, spaces_end.coord());
                    while spaces_end != end && get(&spaces_end) == b' ' && (col % tabstop) != 0 {
                        crate::utf8::ByteIt::inc(&mut spaces_end);
                        col += 1;
                    }
                    if (col % tabstop) == 0 {
                        spaces.push(Selection::from_range(spaces_beg.coord(), spaces_end.prev_byte().coord()));
                    } else if spaces_end != end && get(&spaces_end) == b'\t' {
                        spaces.push(Selection::from_range(spaces_beg.coord(), spaces_end.coord()));
                    }
                    it = spaces_end;
                } else {
                    crate::utf8::ByteIt::inc(&mut it);
                }
            }
        }
    }
    if !spaces.is_empty() {
        SelectionList::from_vec(buffer.clone(), spaces).replace(&[b"\t".to_vec()])?;
    }
    Ok(())
}

fn trim_selections(context: &Context, _: NormalParams) -> Result<()> {
    let buffer = context.try_buffer()?;
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut to_remove = Vec::new();
    let mut new_bounds: Vec<Option<(BufferCoord, BufferCoord)>> = Vec::new();
    let sels = context.selections().as_slice().to_vec();
    {
        let lines = buffer.lines();
        let end_it = BufferIterator::end(&lines);
        let begin_it = BufferIterator::begin(&lines);
        for (i, sel) in sels.iter().enumerate() {
            let mut beg = BufferIterator::new(&lines, sel.min());
            let mut end = BufferIterator::new(&lines, sel.max());
            while beg != end && is_blank(utf8::it_codepoint(&beg, &end_it)) {
                utf8::it_to_next(&mut beg, &end_it);
            }
            while beg != end && is_blank(utf8::it_codepoint(&end, &end_it)) {
                utf8::it_to_previous(&mut end, &begin_it);
            }

            if beg == end && is_blank(utf8::it_codepoint(&beg, &end_it)) {
                to_remove.push(i);
                new_bounds.push(None);
            } else {
                new_bounds.push(Some((beg.coord(), end.coord())));
            }
        }
    }

    if to_remove.len() == sels.len() {
        return Err(no_selections_remaining());
    }

    let mut selections = context.selections();
    for (i, bounds) in new_bounds.iter().enumerate() {
        if let Some((b, e)) = bounds {
            let sel = &mut selections[i];
            *sel.min_mut() = *b;
            *sel.max_mut() = *e;
        }
    }
    for &i in to_remove.iter().rev() {
        selections.remove(i);
    }
    Ok(())
}

fn read_selections_from_register(reg: u8, context: &Context) -> Result<SelectionList> {
    if !is_basic_alpha(reg as u32) && reg != b'^' {
        return Err(Error::runtime("selections can only be saved to the '^' and alphabetic registers"));
    }

    let content = RegisterManager::instance().get(&[reg])?.get(context)?;
    if content.len() < 2 {
        return Err(rt_err!("register '{}' does not contain a selections desc", [reg]));
    }

    // Use the last two values for timestamp and main_index to allow the buffer
    // name to have @ symbols
    let desc = &content[0];
    let error = || Error::runtime("expected <buffer>@<timestamp>@main_index");
    let last_at = desc.iter().rposition(|&c| c == b'@').ok_or_else(error)?;
    let prev_at = desc[..last_at].iter().rposition(|&c| c == b'@').ok_or_else(error)?;
    let main = str_to_int(&desc[last_at + 1..])? as usize;
    let timestamp = str_to_int(&desc[prev_at + 1..last_at])? as usize;
    let buffer_name = &desc[..prev_at];

    let buffer = BufferManager::instance().get_buffer(buffer_name)?;
    selection_list_from_strings(&buffer, ColumnType::Byte, &content[1..], timestamp, main, -1)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CombineOp {
    Append,
    Union,
    Intersect,
    SelectLeftmostCursor,
    SelectRightmostCursor,
    SelectLongest,
    SelectShortest,
}

fn key_to_combine_op(key: Key) -> Result<CombineOp> {
    Ok(match char::from_u32(key.key).unwrap_or('\0') {
        'a' => CombineOp::Append,
        'u' => CombineOp::Union,
        'i' => CombineOp::Intersect,
        '<' => CombineOp::SelectLeftmostCursor,
        '>' => CombineOp::SelectRightmostCursor,
        '+' => CombineOp::SelectLongest,
        '-' => CombineOp::SelectShortest,
        _ => return Err(rt_err!("no such combine operator: '{}'", crate::format::Cp(key.key))),
    })
}

fn combine_selection(buffer: &Buffer, sel: &mut Selection, other: &Selection, op: CombineOp) {
    match op {
        CombineOp::Union => sel.set(sel.min().min(other.min()), sel.max().max(other.max())),
        CombineOp::Intersect => sel.set(sel.min().max(other.min()), sel.max().min(other.max())),
        CombineOp::SelectLeftmostCursor => {
            if sel.cursor() > other.cursor() {
                *sel = other.clone();
            }
        }
        CombineOp::SelectRightmostCursor => {
            if sel.cursor() < other.cursor() {
                *sel = other.clone();
            }
        }
        CombineOp::SelectLongest => {
            if char_length_sel(buffer, sel) < char_length_sel(buffer, other) {
                *sel = other.clone();
            }
        }
        CombineOp::SelectShortest => {
            if char_length_sel(buffer, sel) > char_length_sel(buffer, other) {
                *sel = other.clone();
            }
        }
        CombineOp::Append => {}
    }
}

type CombineFunc = Rc<dyn Fn(&Context, SelectionList) -> Result<()>>;

fn combine_selections(context: &Context, list: SelectionList, func: CombineFunc, title: &[u8]) -> Result<()> {
    if !Rc::ptr_eq(&context.try_buffer()?, list.buffer()) {
        return Err(Error::runtime("cannot combine selections from different buffers"));
    }

    let list = RefCell::new(list);
    on_next_key_with_autoinfo(
        context,
        b"combine-selections",
        KeymapMode::Combine,
        Rc::new(move |key: Key, context: &Context| {
            if key == Key::new(named::ESCAPE) {
                return Ok(());
            }
            let op = key_to_combine_op(key)?;
            let _selection_edition = ScopedSelectionEdition::new(context);
            let sels = context.selections().clone();
            let mut list = list.borrow().clone();
            list.update(true);
            if op == CombineOp::Append {
                let main_index = list.size() + sels.main_index();
                for sel in sels.iter() {
                    list.push(sel.clone());
                }
                list.set_main_index(main_index);
                list.sort_and_merge_overlapping();
            } else {
                if list.size() != sels.size() {
                    return Err(rt_err!(
                        "the two selection lists don't have the same number of elements ({} vs {})",
                        list.size(),
                        sels.size()
                    ));
                }
                for i in 0..list.size() {
                    combine_selection(sels.buffer(), &mut list[i], &sels[i], op);
                }
                list.set_main_index(sels.main_index());
            }
            func(context, list)
        }),
        title.to_vec(),
        build_autoinfo_for_mapping(
            context,
            KeymapMode::Combine,
            &[
                ki(&[key('a')], "append lists"),
                ki(&[key('u')], "union"),
                ki(&[key('i')], "intersection"),
                ki(&[key('<')], "select leftmost cursor"),
                ki(&[key('>')], "select rightmost cursor"),
                ki(&[key('+')], "select longest"),
                ki(&[key('-')], "select shortest"),
            ],
        ),
    )
}

fn save_selections(context: &Context, params: NormalParams, combine: bool) -> Result<()> {
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '^' as u32 }) as u8;
    if !is_basic_alpha(reg as u32) && reg != b'^' {
        return Err(Error::runtime("selections can only be saved to the '^' and alphabetic registers"));
    }

    let content = RegisterManager::instance().get(&[reg])?.get(context)?;
    let empty = content.len() == 1 && content[0].is_empty();

    let save_to_reg: CombineFunc = Rc::new(move |context: &Context, sels: SelectionList| {
        let buffer = context.try_buffer()?;
        let mut descs = vec![kformat!("{}@{}@{}", buffer.name(), buffer.timestamp(), sels.main_index())];
        descs.extend(sels.iter().map(|s| selection_to_string(ColumnType::Byte, &buffer, s, -1)));
        RegisterManager::instance().get(&[reg])?.set(context, &descs, false)?;

        context.print_status(DisplayLine::from_str_face(
            kformat!("{} {} selections to register '{}'", if combine { "Combined" } else { "Saved" }, sels.size(), [reg]),
            context.faces().face("Information"),
        ));
        Ok(())
    });

    if combine && !empty {
        let _selection_edition = ScopedSelectionEdition::new(context);
        combine_selections(context, read_selections_from_register(reg, context)?, save_to_reg, b"combine selections to register")
    } else {
        let sels = context.selections().clone();
        save_to_reg(context, sels)
    }
}

fn restore_selections(context: &Context, params: NormalParams, combine: bool) -> Result<()> {
    let reg = to_lower(if params.reg != 0 { params.reg as u32 } else { '^' as u32 }) as u8;
    let selections = read_selections_from_register(reg, context)?;

    let _selection_edition = ScopedSelectionEdition::new(context);
    let set_selections: CombineFunc = Rc::new(move |context: &Context, sels: SelectionList| {
        let size = sels.size();
        *context.selections_write_only() = sels;
        context.print_status(DisplayLine::from_str_face(
            kformat!("{} {} selections from register '{}'", if combine { "Combined" } else { "Restored" }, size, [reg]),
            context.faces().face("Information"),
        ));
        Ok(())
    });

    if !combine {
        if !Rc::ptr_eq(selections.buffer(), &context.try_buffer()?) {
            context.change_buffer(selections.buffer(), None)?;
        }
        set_selections(context, selections)
    } else {
        combine_selections(context, selections, set_selections, b"combine selections from register")
    }
}

fn undo(context: &Context, params: NormalParams) -> Result<()> {
    let buffer = context.try_buffer()?;
    let timestamp = buffer.timestamp();
    if buffer.undo(params.count.max(1) as usize)? {
        let ranges = compute_modified_ranges(&buffer, timestamp);
        if !ranges.is_empty() {
            context.selections_write_only().assign(ranges);
        }
        Ok(())
    } else {
        Err(Error::runtime("nothing left to undo"))
    }
}

fn redo(context: &Context, params: NormalParams) -> Result<()> {
    let buffer = context.try_buffer()?;
    let timestamp = buffer.timestamp();
    if buffer.redo(params.count.max(1) as usize)? {
        let ranges = compute_modified_ranges(&buffer, timestamp);
        if !ranges.is_empty() {
            context.selections_write_only().assign(ranges);
        }
        Ok(())
    } else {
        Err(Error::runtime("nothing left to redo"))
    }
}

fn move_in_history(context: &Context, params: NormalParams, direction: Direction) -> Result<()> {
    let buffer = context.try_buffer()?;
    let timestamp = buffer.timestamp();
    let count = params.count.max(1);
    let history_id = buffer.current_history_id() as i32 + direction as i32 * count;
    let max_history_id = buffer.next_history_id() as i32 - 1;
    if history_id >= 0 && buffer.move_to(history_id as usize)? {
        let ranges = compute_modified_ranges(&buffer, timestamp);
        if !ranges.is_empty() {
            context.selections_write_only().assign(ranges);
        }

        context.print_status(DisplayLine::from_str_face(
            kformat!("moved to change #{} ({})", history_id, max_history_id),
            context.faces().face("Information"),
        ));
        Ok(())
    } else {
        Err(rt_err!("no such change: #{} ({})", history_id, max_history_id))
    }
}

fn undo_selection_change(context: &Context, params: NormalParams, direction: Direction) -> Result<()> {
    for _ in 0..params.count.max(1) {
        context.undo_selection_change(direction)?;
    }
    Ok(())
}

fn exec_user_mappings(context: &Context, params: NormalParams) -> Result<()> {
    on_next_key_with_autoinfo(
        context,
        b"user-mapping",
        KeymapMode::None,
        Rc::new(move |key: Key, context: &Context| {
            let Some(mapping) = context.keymaps().get_mapping(key, KeymapMode::User) else { return Ok(()) };

            let _disable_keymaps = ScopedSetBool::set(context.keymaps_disabled());
            let ih = context.try_input_handler()?;
            let _force_normal = ih.force_normal(params)?;
            let _edition = ScopedEdition::new(context);
            let _selection_edition = ScopedSelectionEdition::new(context);
            // copy to allow reentrant unmap
            for key in mapping.keys.clone() {
                ih.handle_key(key, true)?;
            }
            Ok(())
        }),
        b"user mapping".to_vec(),
        build_autoinfo_for_mapping(context, KeymapMode::User, &[]),
    )
}

fn add_empty_line(context: &Context, params: NormalParams, above: bool) -> Result<()> {
    let count = params.count.max(1);
    let new_lines = string_of_cp('\n' as u32, count);
    let buffer = context.try_buffer()?;
    let _selection_edition = ScopedSelectionEdition::new(context);
    let sels = context.selections().as_slice().to_vec();
    let _edition = ScopedEdition::new(context);
    for (i, sel) in sels.iter().enumerate() {
        let line = (if above { sel.min().line } else { sel.max().line + 1 }) + i as i32 * count;
        buffer.insert(BufferCoord::line(line), &new_lines)?;
    }
    Ok(())
}

fn repeated(context: &Context, params: NormalParams, func: &dyn Fn(&Context, NormalParams) -> Result<()>) -> Result<()> {
    let _edition = ScopedEdition::new(context);
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut count = params.count;
    loop {
        func(context, NormalParams { count: 0, reg: params.reg })?;
        count -= 1;
        if count <= 0 {
            break;
        }
    }
    Ok(())
}

fn move_cursor_chars(context: &Context, params: NormalParams, direction: Direction, mode: SelectMode) -> Result<()> {
    let offset = direction as i32 * params.count.max(1);
    let _selection_edition = ScopedSelectionEdition::new(context);
    let buffer = context.try_buffer()?;
    let mut selections = context.selections();
    for sel in selections.iter_mut() {
        let cursor = buffer.offset_coord_chars(sel.cursor().coord, offset);
        let anchor = if mode == SelectMode::Extend { sel.anchor() } else { cursor };
        sel.set_anchor(anchor);
        sel.set_cursor(cursor);
    }
    selections.sort_and_merge_overlapping();
    Ok(())
}

fn move_cursor_lines(context: &Context, params: NormalParams, direction: Direction, mode: SelectMode) -> Result<()> {
    let offset = direction as i32 * params.count.max(1);
    let tabstop = context.options().val::<i32>("tabstop");
    let _selection_edition = ScopedSelectionEdition::new(context);
    let buffer = context.try_buffer()?;
    let mut selections = context.selections();
    for sel in selections.iter_mut() {
        let cursor = buffer.offset_coord_lines(sel.cursor(), offset, tabstop);
        let anchor = if mode == SelectMode::Extend { sel.anchor() } else { cursor.coord };
        sel.set_anchor(anchor);
        sel.set_cursor(cursor);
    }
    selections.sort_and_merge_overlapping();
    Ok(())
}

fn select_whole_buffer(context: &Context, _: NormalParams) -> Result<()> {
    let buffer = context.try_buffer()?;
    let _selection_edition = ScopedSelectionEdition::new(context);
    *context.selections_write_only() = SelectionList::new(
        buffer.clone(),
        Selection::from_range(BufferCoord::new(0, 0), BufferCoordAndTarget::new(buffer.back_coord(), MAX_COLUMN, -1)),
    );
    Ok(())
}

fn keep_selection(context: &Context, p: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut selections = context.selections();
    let index = if p.count != 0 { p.count - 1 } else { selections.main_index() as i32 };
    if index < 0 || index as usize >= selections.size() {
        return Err(rt_err!("invalid selection index: {}", index));
    }
    let sel = selections[index as usize].clone();
    *selections = SelectionList::new(selections.buffer().clone(), sel);
    Ok(())
}

fn remove_selection(context: &Context, p: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    let mut selections = context.selections();
    let index = if p.count != 0 { p.count - 1 } else { selections.main_index() as i32 };
    if index < 0 || index as usize >= selections.size() {
        return Err(rt_err!("invalid selection index: {}", index));
    }
    if selections.size() == 1 {
        return Err(no_selections_remaining());
    }
    selections.remove(index as usize);
    Ok(())
}

fn clear_selections(context: &Context, _: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    for sel in context.selections().iter_mut() {
        let c = sel.cursor().coord;
        sel.set_anchor(c);
    }
    Ok(())
}

fn flip_selections(context: &Context, _: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    for sel in context.selections().iter_mut() {
        let tmp = sel.anchor();
        let c = sel.cursor().coord;
        sel.set_anchor(c);
        sel.set_cursor(tmp);
    }
    Ok(())
}

fn ensure_forward(context: &Context, _: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    for sel in context.selections().iter_mut() {
        let (min, max) = (sel.min(), sel.max());
        sel.set_anchor(min);
        sel.set_cursor(max);
    }
    Ok(())
}

fn merge_consecutive(context: &Context, params: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    ensure_forward(context, params)?;
    context.selections().merge_consecutive();
    Ok(())
}

fn merge_overlapping(context: &Context, params: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    ensure_forward(context, params)?;
    context.selections().merge_overlapping();
    Ok(())
}

fn duplicate_selections(context: &Context, params: NormalParams) -> Result<()> {
    let _selection_edition = ScopedSelectionEdition::new(context);
    let (sels, sels_main) = {
        let s = context.selections();
        (s.as_slice().to_vec(), s.main_index())
    };
    let mut new_sels = Vec::new();
    let count = if params.count != 0 { params.count as usize } else { 2 };
    let mut last: Option<crate::selection::BasicSelection> = None;
    let mut main_index = 0;
    for (index, sel) in sels.iter().enumerate() {
        let n = if Some(sel.basic()) != last { count } else { 1 };
        for _ in 0..n {
            new_sels.push(sel.clone());
        }
        last = Some(sel.basic());
        if index == sels_main {
            main_index = new_sels.len() - 1;
        }
    }
    context.selections().set(new_sels, main_index);
    Ok(())
}

fn force_redraw(context: &Context, _: NormalParams) -> Result<()> {
    if let Ok(client) = context.try_client() {
        client.force_redraw(true);
        client.redraw_ifn()?;
    }
    Ok(())
}

macro_rules! ncmd {
    ($doc:expr, $f:expr) => {
        NormalCmd { docstring: $doc, func: $f }
    };
}

fn build_keymap() -> Vec<(Key, NormalCmd)> {
    use Direction::{Backward, Forward};
    use SelectMode::{Append, Extend, Replace};
    let k = |c: char| key(c);
    let a = |c: char| alt(key(c));
    let c = |ch: char| ctrl(key(ch));
    vec![
        (k('h'), ncmd!("move left", |cx, p| move_cursor_chars(cx, p, Backward, Replace))),
        (k('j'), ncmd!("move down", |cx, p| move_cursor_lines(cx, p, Forward, Replace))),
        (k('k'), ncmd!("move up", |cx, p| move_cursor_lines(cx, p, Backward, Replace))),
        (k('l'), ncmd!("move right", |cx, p| move_cursor_chars(cx, p, Forward, Replace))),
        (k('H'), ncmd!("extend left", |cx, p| move_cursor_chars(cx, p, Backward, Extend))),
        (k('J'), ncmd!("extend down", |cx, p| move_cursor_lines(cx, p, Forward, Extend))),
        (k('K'), ncmd!("extend up", |cx, p| move_cursor_lines(cx, p, Backward, Extend))),
        (k('L'), ncmd!("extend right", |cx, p| move_cursor_chars(cx, p, Forward, Extend))),
        (k('t'), ncmd!("select to next character", |cx, p| select_to_next_char(cx, p, SelectFlags::None))),
        (k('f'), ncmd!("select to next character included", |cx, p| select_to_next_char(cx, p, SelectFlags::Inclusive))),
        (k('T'), ncmd!("extend to next character", |cx, p| select_to_next_char(cx, p, SelectFlags::Extend))),
        (k('F'), ncmd!("extend to next character included", |cx, p| select_to_next_char(cx, p, SelectFlags::Inclusive | SelectFlags::Extend))),
        (a('t'), ncmd!("select to previous character", |cx, p| select_to_next_char(cx, p, SelectFlags::Reverse))),
        (a('f'), ncmd!("select to previous character included", |cx, p| select_to_next_char(cx, p, SelectFlags::Inclusive | SelectFlags::Reverse))),
        (a('T'), ncmd!("extend to previous character", |cx, p| select_to_next_char(cx, p, SelectFlags::Extend | SelectFlags::Reverse))),
        (a('F'), ncmd!("extend to previous character included", |cx, p| select_to_next_char(cx, p, SelectFlags::Inclusive | SelectFlags::Extend | SelectFlags::Reverse))),
        (k('d'), ncmd!("erase selected text", |cx, p| erase_selections(cx, p, true))),
        (a('d'), ncmd!("erase selected text, without yanking", |cx, p| erase_selections(cx, p, false))),
        (k('c'), ncmd!("change selected text", |cx, p| change(cx, p, true))),
        (a('c'), ncmd!("change selected text, without yanking", |cx, p| change(cx, p, false))),
        (k('i'), ncmd!("insert before selected text", |cx, p| enter_insert_mode(cx, InsertMode::Insert, p))),
        (k('I'), ncmd!("insert at line begin", |cx, p| enter_insert_mode(cx, InsertMode::InsertAtLineBegin, p))),
        (k('a'), ncmd!("insert after selected text", |cx, p| enter_insert_mode(cx, InsertMode::Append, p))),
        (k('A'), ncmd!("insert at line end", |cx, p| enter_insert_mode(cx, InsertMode::AppendAtLineEnd, p))),
        (k('o'), ncmd!("insert on new line below", |cx, p| enter_insert_mode(cx, InsertMode::OpenLineBelow, p))),
        (k('O'), ncmd!("insert on new line above", |cx, p| enter_insert_mode(cx, InsertMode::OpenLineAbove, p))),
        (k('r'), ncmd!("replace with character", replace_with_char)),
        (a('o'), ncmd!("add a new empty line below", |cx, p| add_empty_line(cx, p, false))),
        (a('O'), ncmd!("add a new empty line above", |cx, p| add_empty_line(cx, p, true))),
        (k('g'), ncmd!("go to location", |cx, p| goto_commands(cx, p, Replace))),
        (k('G'), ncmd!("extend to location", |cx, p| goto_commands(cx, p, Extend))),
        (k('v'), ncmd!("move view", |cx, p| view_commands(cx, p, false))),
        (k('V'), ncmd!("move view (locked)", |cx, p| view_commands(cx, p, true))),
        (k('y'), ncmd!("yank selected text", yank)),
        (k('p'), ncmd!("paste after selected text", |cx, p| repeated(cx, p, &|cx, p| paste(cx, p, PasteMode::Append)))),
        (k('P'), ncmd!("paste before selected text", |cx, p| repeated(cx, p, &|cx, p| paste(cx, p, PasteMode::Insert)))),
        (a('p'), ncmd!("paste every yanked selection after selected text", |cx, p| paste_all(cx, p, PasteMode::Append))),
        (a('P'), ncmd!("paste every yanked selection before selected text", |cx, p| paste_all(cx, p, PasteMode::Insert))),
        (k('R'), ncmd!("replace selected text with yanked text", |cx, p| paste(cx, p, PasteMode::Replace))),
        (a('R'), ncmd!("replace selected text with every yanked text", |cx, p| paste_all(cx, p, PasteMode::Replace))),
        (k('s'), ncmd!("select regex matches in selected text", select_regex)),
        (k('S'), ncmd!("split selected text on regex matches", split_regex)),
        (a('s'), ncmd!("split selected text on line ends", split_lines)),
        (a('S'), ncmd!("select selection boundaries", select_boundaries)),
        (k('.'), ncmd!("repeat last insert command", repeat_last_insert)),
        (a('.'), ncmd!("repeat last object select/character find", repeat_last_select)),
        (k('%'), ncmd!("select whole buffer", select_whole_buffer)),
        (k(':'), ncmd!("enter command prompt", command)),
        (k('|'), ncmd!("pipe each selection through filter and replace with output", |cx, p| pipe(cx, p, true))),
        (a('|'), ncmd!("pipe each selection through command and ignore output", |cx, p| pipe(cx, p, false))),
        (k('!'), ncmd!("insert command output", |cx, p| insert_output(cx, p, PasteMode::Insert))),
        (a('!'), ncmd!("append command output", |cx, p| insert_output(cx, p, PasteMode::Append))),
        (k(','), ncmd!("remove all selections except main", keep_selection)),
        (a(','), ncmd!("remove main selection", remove_selection)),
        (k(';'), ncmd!("reduce selections to their cursor", clear_selections)),
        (a(';'), ncmd!("swap selections cursor and anchor", flip_selections)),
        (a(':'), ncmd!("ensure selection cursor is after anchor", ensure_forward)),
        (a('_'), ncmd!("merge consecutive selections", merge_consecutive)),
        (k('+'), ncmd!("duplicate each selection", duplicate_selections)),
        (a('+'), ncmd!("merge overlapping selections", merge_overlapping)),
        (k('w'), ncmd!("select to next word start", |cx, p| repeated(cx, p, &|cx, _| select(cx, Replace, to_next_word(WordType::Word).as_ref())))),
        (k('e'), ncmd!("select to next word end", |cx, p| repeated(cx, p, &|cx, _| select(cx, Replace, to_next_word_end(WordType::Word).as_ref())))),
        (k('b'), ncmd!("select to previous word start", |cx, p| repeated(cx, p, &|cx, _| select(cx, Replace, to_previous_word(WordType::Word).as_ref())))),
        (k('W'), ncmd!("extend to next word start", |cx, p| repeated(cx, p, &|cx, _| select(cx, Extend, to_next_word(WordType::Word).as_ref())))),
        (k('E'), ncmd!("extend to next word end", |cx, p| repeated(cx, p, &|cx, _| select(cx, Extend, to_next_word_end(WordType::Word).as_ref())))),
        (k('B'), ncmd!("extend to previous word start", |cx, p| repeated(cx, p, &|cx, _| select(cx, Extend, to_previous_word(WordType::Word).as_ref())))),
        (a('w'), ncmd!("select to next WORD start", |cx, p| repeated(cx, p, &|cx, _| select(cx, Replace, to_next_word(WordType::WORD).as_ref())))),
        (a('e'), ncmd!("select to next WORD end", |cx, p| repeated(cx, p, &|cx, _| select(cx, Replace, to_next_word_end(WordType::WORD).as_ref())))),
        (a('b'), ncmd!("select to previous WORD start", |cx, p| repeated(cx, p, &|cx, _| select(cx, Replace, to_previous_word(WordType::WORD).as_ref())))),
        (a('W'), ncmd!("extend to next WORD start", |cx, p| repeated(cx, p, &|cx, _| select(cx, Extend, to_next_word(WordType::WORD).as_ref())))),
        (a('E'), ncmd!("extend to next WORD end", |cx, p| repeated(cx, p, &|cx, _| select(cx, Extend, to_next_word_end(WordType::WORD).as_ref())))),
        (a('B'), ncmd!("extend to previous WORD start", |cx, p| repeated(cx, p, &|cx, _| select(cx, Extend, to_previous_word(WordType::WORD).as_ref())))),
        (a('l'), ncmd!("select to line end", |cx, p| repeated(cx, p, &|cx, _| select(cx, Replace, to_line_end(false).as_ref())))),
        (a('L'), ncmd!("extend to line end", |cx, p| repeated(cx, p, &|cx, _| select(cx, Extend, to_line_end(false).as_ref())))),
        (a('h'), ncmd!("select to line begin", |cx, p| repeated(cx, p, &|cx, _| select(cx, Replace, to_line_begin(false).as_ref())))),
        (a('H'), ncmd!("extend to line begin", |cx, p| repeated(cx, p, &|cx, _| select(cx, Extend, to_line_begin(false).as_ref())))),
        (k('x'), ncmd!("extend selections to whole lines", |cx, _| select(cx, Replace, &|c, s| Ok(selectors::select_lines(c, s))))),
        (a('x'), ncmd!("crop selections to whole lines", |cx, _| select(cx, Replace, &|c, s| Ok(selectors::trim_partial_lines(c, s))))),
        (k('m'), ncmd!("select to matching character", |cx, _| select(cx, Replace, matching(true).as_ref()))),
        (a('m'), ncmd!("backward select to matching character", |cx, _| select(cx, Replace, matching(false).as_ref()))),
        (k('M'), ncmd!("extend to matching character", |cx, _| select(cx, Extend, matching(true).as_ref()))),
        (a('M'), ncmd!("backward extend to matching character", |cx, _| select(cx, Extend, matching(false).as_ref()))),
        (k('/'), ncmd!("select next given regex match", |cx, p| search(cx, p, Replace, RegexMode::FORWARD))),
        (k('?'), ncmd!("extend with next given regex match", |cx, p| search(cx, p, Extend, RegexMode::FORWARD))),
        (a('/'), ncmd!("select previous given regex match", |cx, p| search(cx, p, Replace, RegexMode::BACKWARD))),
        (a('?'), ncmd!("extend with previous given regex match", |cx, p| search(cx, p, Extend, RegexMode::BACKWARD))),
        (k('n'), ncmd!("select next current search pattern match", |cx, p| search_next(cx, p, Replace, RegexMode::FORWARD))),
        (k('N'), ncmd!("extend with next current search pattern match", |cx, p| search_next(cx, p, Append, RegexMode::FORWARD))),
        (a('n'), ncmd!("select previous current search pattern match", |cx, p| search_next(cx, p, Replace, RegexMode::BACKWARD))),
        (a('N'), ncmd!("extend with previous current search pattern match", |cx, p| search_next(cx, p, Append, RegexMode::BACKWARD))),
        (k('*'), ncmd!("set search pattern to main selection content", |cx, p| use_selection_as_search_pattern(cx, p, true))),
        (a('*'), ncmd!("set search pattern to main selection content, do not detect words", |cx, p| use_selection_as_search_pattern(cx, p, false))),
        (k('u'), ncmd!("undo", undo)),
        (k('U'), ncmd!("redo", redo)),
        (c('k'), ncmd!("move backward in history", |cx, p| move_in_history(cx, p, Backward))),
        (c('j'), ncmd!("move forward in history", |cx, p| move_in_history(cx, p, Forward))),
        (a('u'), ncmd!("undo selection change", |cx, p| undo_selection_change(cx, p, Backward))),
        (a('U'), ncmd!("redo selection change", |cx, p| undo_selection_change(cx, p, Forward))),
        (a('i'), ncmd!("select inner object", |cx, p| select_object(cx, p, ObjectFlags::ToBegin | ObjectFlags::ToEnd | ObjectFlags::Inner, Replace))),
        (a('a'), ncmd!("select whole object", |cx, p| select_object(cx, p, ObjectFlags::ToBegin | ObjectFlags::ToEnd, Replace))),
        (k('['), ncmd!("select to object start", |cx, p| select_object(cx, p, ObjectFlags::ToBegin, Replace))),
        (k(']'), ncmd!("select to object end", |cx, p| select_object(cx, p, ObjectFlags::ToEnd, Replace))),
        (k('{'), ncmd!("extend to object start", |cx, p| select_object(cx, p, ObjectFlags::ToBegin, Extend))),
        (k('}'), ncmd!("extend to object end", |cx, p| select_object(cx, p, ObjectFlags::ToEnd, Extend))),
        (a('['), ncmd!("select to inner object start", |cx, p| select_object(cx, p, ObjectFlags::ToBegin | ObjectFlags::Inner, Replace))),
        (a(']'), ncmd!("select to inner object end", |cx, p| select_object(cx, p, ObjectFlags::ToEnd | ObjectFlags::Inner, Replace))),
        (a('{'), ncmd!("extend to inner object start", |cx, p| select_object(cx, p, ObjectFlags::ToBegin | ObjectFlags::Inner, Extend))),
        (a('}'), ncmd!("extend to inner object end", |cx, p| select_object(cx, p, ObjectFlags::ToEnd | ObjectFlags::Inner, Extend))),
        (a('I'), ncmd!("select nested objects", |cx, p| select_object(cx, p, ObjectFlags::Nested | ObjectFlags::Inner | ObjectFlags::ToBegin | ObjectFlags::ToEnd, Replace))),
        (a('A'), ncmd!("select nested objects", |cx, p| select_object(cx, p, ObjectFlags::Nested | ObjectFlags::ToBegin | ObjectFlags::ToEnd, Replace))),
        (a('j'), ncmd!("join lines", join_lines)),
        (a('J'), ncmd!("join lines and select spaces", join_lines_select_spaces)),
        (a('k'), ncmd!("keep selections matching given regex", |cx, p| keep(cx, p, true))),
        (a('K'), ncmd!("keep selections not matching given regex", |cx, p| keep(cx, p, false))),
        (k('$'), ncmd!("pipe each selection through shell command and keep the ones whose command succeed", keep_pipe)),
        (k('<'), ncmd!("deindent", |cx, p| deindent(cx, p, true))),
        (k('>'), ncmd!("indent", |cx, p| indent(cx, p, false))),
        (a('>'), ncmd!("indent, including empty lines", |cx, p| indent(cx, p, true))),
        (a('<'), ncmd!("deindent, not including incomplete indent", |cx, p| deindent(cx, p, false))),
        (c('i'), ncmd!("jump forward in jump list", |cx, p| jump(cx, p, Forward))),
        (c('o'), ncmd!("jump backward in jump list", |cx, p| jump(cx, p, Backward))),
        (c('s'), ncmd!("push current selections in jump list", push_selections)),
        (k(')'), ncmd!("rotate main selection forward", |cx, p| rotate_selections(cx, p, Forward))),
        (k('('), ncmd!("rotate main selection backward", |cx, p| rotate_selections(cx, p, Backward))),
        (a(')'), ncmd!("rotate selections content forward", |cx, p| rotate_selections_content(cx, p, Forward))),
        (a('('), ncmd!("rotate selections content backward", |cx, p| rotate_selections_content(cx, p, Backward))),
        (k('q'), ncmd!("replay recorded macro", replay_macro)),
        (k('Q'), ncmd!("start or end macro recording", start_or_end_macro_recording)),
        (k('`'), ncmd!("convert to lower case in selections", |cx, _| for_each_codepoint(cx, to_lower))),
        (k('~'), ncmd!("convert to upper case in selections", |cx, _| for_each_codepoint(cx, to_upper))),
        (a('`'), ncmd!("swap case in selections", |cx, _| for_each_codepoint(cx, swap_case))),
        (k('&'), ncmd!("align selection cursors", align)),
        (a('&'), ncmd!("copy indentation", copy_indent)),
        (k('@'), ncmd!("convert tabs to spaces in selections", tabs_to_spaces)),
        (a('@'), ncmd!("convert spaces to tabs in selections", spaces_to_tabs)),
        (k('_'), ncmd!("trim selections", trim_selections)),
        (k('C'), ncmd!("duplicate selections on the lines that follow them.", |cx, p| copy_selections_on_next_lines(cx, p, Forward))),
        (a('C'), ncmd!("duplicate selections on the lines that precede them.", |cx, p| copy_selections_on_next_lines(cx, p, Backward))),
        (Key::new(named::SPACE), ncmd!("user mappings", exec_user_mappings)),
        (Key::new(named::PAGE_UP), ncmd!("scroll one page up", |cx, p| scroll(cx, p, Backward, false))),
        (Key::new(named::PAGE_DOWN), ncmd!("scroll one page down", |cx, p| scroll(cx, p, Forward, false))),
        (c('b'), ncmd!("scroll one page up", |cx, p| scroll(cx, p, Backward, false))),
        (c('f'), ncmd!("scroll one page down", |cx, p| scroll(cx, p, Forward, false))),
        (c('u'), ncmd!("scroll half a page up", |cx, p| scroll(cx, p, Backward, true))),
        (c('d'), ncmd!("scroll half a page down", |cx, p| scroll(cx, p, Forward, true))),
        (k('z'), ncmd!("restore selections from register", |cx, p| restore_selections(cx, p, false))),
        (a('z'), ncmd!("combine selections from register", |cx, p| restore_selections(cx, p, true))),
        (k('Z'), ncmd!("save selections to register", |cx, p| save_selections(cx, p, false))),
        (a('Z'), ncmd!("combine selections to register", |cx, p| save_selections(cx, p, true))),
        (c('l'), ncmd!("force redraw", force_redraw)),
    ]
}

thread_local! {
    static KEYMAP: indexmap::IndexMap<Key, NormalCmd> = build_keymap().into_iter().collect();
}

pub fn get_normal_command(key: Key) -> Option<NormalCmd> {
    KEYMAP.with(|k| k.get(&key).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_diff() {
        use crate::buffer::{Change, ChangeType};
        use crate::test_utils::make_buffer;
        let validate = |line: LineCount, new_text: &[u8], expected_range: BufferRange, expected_changes: &[Change]| {
            let buffer = make_buffer(b"line1\nline2\nline3\n");
            let timestamp = buffer.timestamp();
            let old_text: Vec<StringDataPtr> = (line..buffer.line_count()).map(|l| buffer.line(l)).collect();
            let old_refs: Vec<&[u8]> = old_text.iter().map(|l| &l[..]).collect();
            let range = apply_diff(&buffer, BufferCoord::line(line), &old_refs, new_text).unwrap();
            assert_eq!(range, expected_range);
            assert_eq!(buffer.changes_since(timestamp), expected_changes);
        };
        let c = |type_, begin: (i32, i32), end: (i32, i32)| Change {
            type_,
            begin: BufferCoord::new(begin.0, begin.1),
            end: BufferCoord::new(end.0, end.1),
        };
        let r = |b: (i32, i32), e: (i32, i32)| BufferRange::new(BufferCoord::new(b.0, b.1), BufferCoord::new(e.0, e.1));
        // When appending at end, we add any missing newline
        validate(3, b"added-line3-missing-eol", r((3, 0), (4, 0)), &[c(ChangeType::Insert, (3, 0), (4, 0))]);
        // Special case: erasing until buffer end also erases the final newline.
        validate(2, b"", r((1, 5), (1, 5)), &[c(ChangeType::Erase, (2, 0), (3, 0))]);
        // Erasing and appending at end still produces forward-only changes.
        validate(
            2,
            b"changed-line3\nadded-line4\nadded-line5\n",
            r((2, 0), (5, 0)),
            &[c(ChangeType::Erase, (2, 0), (3, 0)), c(ChangeType::Insert, (2, 0), (5, 0))],
        );
        // Same result when append is missing newline.
        validate(
            2,
            b"changed-line3\nadded-line4\nadded-line5-missing-eol",
            r((2, 0), (5, 0)),
            &[c(ChangeType::Erase, (2, 0), (3, 0)), c(ChangeType::Insert, (2, 0), (5, 0))],
        );
    }

    #[test]
    fn test_merge_selection() {
        let s = |a: (i32, i32), c: (i32, i32)| Selection::from_range(BufferCoord::new(a.0, a.1), BufferCoord::new(c.0, c.1));
        let merge = |mut sel: Selection, new_sel: Selection| {
            merge_selections(&mut sel, &new_sel);
            sel
        };
        assert_eq!(merge(s((0, 1), (0, 2)), s((0, 3), (0, 4))), s((0, 1), (0, 4)));
        assert_eq!(merge(s((0, 1), (0, 2)), s((0, 1), (0, 2))), s((0, 1), (0, 2)));
        assert_eq!(merge(s((0, 1), (0, 2)), s((0, 0), (0, 0))), s((0, 1), (0, 0)));
        assert_eq!(merge(s((0, 1), (0, 2)), s((0, 0), (0, 3))), s((0, 0), (0, 3)));
        assert_eq!(merge(s((0, 1), (0, 3)), s((0, 4), (0, 2))), s((0, 1), (0, 2)));
        assert_eq!(merge(s((0, 1), (0, 2)), s((0, 1), (0, 1))), s((0, 1), (0, 1)));
    }
}
