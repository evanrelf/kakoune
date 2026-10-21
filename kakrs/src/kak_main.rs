//! Port of main.cc

use crate::buffer::{BufferFlags, ByteOrderMark, EolFormat, FinalEol};
use crate::buffer_manager::BufferManager;
use crate::buffer_utils::{
    char_length_sel, create_buffer_from_string, create_fifo_buffer, get_column, history_as_strings,
    open_file_buffer, open_or_create_file_buffer, undo_group_as_strings, write_buffer_to_fd, write_buffer_to_file,
    AutoScroll, WriteFlags, WriteMethod,
};
use crate::client::{Autoreload, Client};
use crate::client_manager::ClientManager;
use crate::command_manager::{CommandManager, ShellContext};
use crate::context::{Context, ContextFlags};
use crate::debug::{get_env_vars, write_to_debug_buffer, DebugFlags};
use crate::display_buffer::{parse_display_line, DisplayBuffer, DisplayLine, DisplayLineBuiltins, DisplayLineList};
use crate::event_manager::{set_signal_handler, EventManager, EventMode, SignalHandler};
use crate::exception::{Error, ErrorKind, Result};
use crate::face::{Attribute, Face};
use crate::file::{getenv, homedir, list_files, read_fd, real_path, split_path};
use crate::hook_manager::Hook;
use crate::input_handler::{AutoComplete, AutoInfo, InputHandler};
use crate::insert_completer::{InsertCompleterDesc, InsertCompleterMode};
use crate::json_ui::JsonUI;
use crate::keymap_manager::KeymapMode;
use crate::keys::{alt, key, named, parse_keys, shift, Key};
use crate::option_manager::{OptionFlags, OptionsRegistry};
use crate::option_types::{CodepointOpt, OptionElem, StringMap};
use crate::parameters_parser::{generate_switches_doc, ParameterDesc, ParametersParser, SwitchDesc, SwitchMap};
use crate::regex::Regex;
use crate::regex_vm::RegexCompileFlags;
use crate::register_manager::{make_dyn_reg, make_dyn_reg_with_setter, null_register, HistoryRegister, RegisterManager, StaticRegister};
use crate::remote::{check_session, send_command, session_directory, session_path, RemoteClient, Server};
use crate::scope::GlobalScope;
use crate::selection::{selection_list_to_string, selection_to_string, ColumnType, Selection};
use crate::shell_manager::{EnvVarDesc, ShellManager};
use crate::string_utils::{escape, str_to_int, str_to_int_ifp};
use crate::terminal_ui::TerminalUI;
use crate::unicode::{is_blank, is_punctuation, DEFAULT_EXTRA_WORD_CHARS};
use crate::units::{BufferCoord, ColumnCount, DisplayCoord};
use crate::user_interface::{
    InfoStyle, MenuStyle, OnKeyCallback, OnPasteCallback, StatusStyle, UiOptions, UserInterface,
};
use crate::utf8;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const VERSION: &str = env!("KAK_VERSION");

struct VersionNotes {
    version: u32,
    notes: &'static str,
}

const VERSION_NOTES: &[VersionNotes] = &[
    VersionNotes { version: 0, notes: "» local scope for command line evaluation\n" },
    VersionNotes {
        version: 20260521,
        notes: "» support the {+b}\\N{} escape sequence in regex (matches {+b}[^\\n]{})\n\
                » count and register forwarding to user modes\n\
                » back switch added for the arrange-buffers command\n",
    },
    VersionNotes {
        version: 20260412,
        notes: "» {+u}finaleol{} option to preserve files with no final end-of-line\n\
                » {+b}%val\\{buffile}{} is now empty for scratch buffers\n\
                » {+b}FocusIn{}/{+b}FocusOut{} events on suspend\n\
                » {+u}number-lines -full-relative{} switch to keep a smaller line number gutter\n\
                » {+b}<a-I>{} and {+b}<a-A>{} to select nested text objects\n\
                » {+b}kak -C <session>{} to connect-or-create a session\n",
    },
    VersionNotes {
        version: 20250603,
        notes: "» kak_* appearing in shell arguments will be added to the environment\n\
                » {+U}double underline{} support\n\
                » {+u}git apply{} can stage/revert selected changes to current buffer\n\
                » {+u}exec/eval -client{} accepts '*' and comma separated list\n",
    },
];

fn show_startup_info(local_client: &Client, last_version: i32) {
    let version_face = Face::attr(Attribute::Bold);
    let mut info: DisplayLineList = Vec::new();
    for VersionNotes { version, notes } in VERSION_NOTES {
        let version = *version;
        if version != 0 && version as i32 <= last_version {
            continue;
        }

        if version == 0 {
            info.push(DisplayLine::from_str_face("• Development version", version_face));
        } else {
            let year = version / 10000;
            let month = (version / 100) % 100;
            let day = version % 100;
            info.push(DisplayLine::from_str_face(kformat!("• Kakoune v{}.{:02}.{:02}", year, month, day), version_face));
        }

        for line in crate::string_utils::split(notes.as_bytes(), b'\n') {
            if let Ok(l) = parse_display_line(line, GlobalScope::instance().faces(), &DisplayLineBuiltins::new()) {
                info.push(l);
            }
        }
    }
    if !info.is_empty() {
        local_client.info_show(
            DisplayLine::new(vec![
                crate::display_buffer::DisplayAtom::text(kformat!("Kakoune {}", VERSION), version_face),
                crate::display_buffer::DisplayAtom::text(", more info at ", Face::default()),
                crate::display_buffer::DisplayAtom::text(":doc changelog", Face::attr(Attribute::Underline)),
            ]),
            info,
            BufferCoord::default(),
            InfoStyle::Prompt,
        );
    }
}

fn write_stdout(s: &[u8]) {
    let _ = crate::file::write_fd(1, s, true);
}

fn write_stderr(s: &[u8]) {
    let _ = crate::file::write_fd(2, s, true);
}

pub fn runtime_directory() -> Vec<u8> {
    if let Some(runtime_directory) = getenv("KAKOUNE_RUNTIME") {
        return runtime_directory;
    }
    let binary = crate::file::get_kak_binary_path().unwrap_or_default();
    let relpath = kformat!("{}../share/kak", split_path(&binary).0);
    let c = crate::file::cstr(&relpath);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(c.as_ptr(), &mut st) } == 0 && crate::file::is_dir_mode(st.st_mode) {
        return real_path(&relpath);
    }
    b"/usr/share/kak".to_vec()
}

pub fn config_directory() -> Vec<u8> {
    if let Some(dir) = getenv("KAKOUNE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return dir;
    }
    if let Some(dir) = getenv("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return kformat!("{}/kak", dir);
    }
    kformat!("{}/.config/kak", homedir())
}

fn main_sel_first(context: &Context) -> Result<Vec<Selection>> {
    let sels = context.try_selections()?;
    let main = sels.main_index();
    let all = sels.as_slice();
    Ok(all[main..].iter().chain(all[..main].iter()).cloned().collect())
}

fn one(s: impl Into<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
    Ok(vec![s.into()])
}

fn num(n: impl std::fmt::Display) -> Result<Vec<Vec<u8>>> {
    one(n.to_string().into_bytes())
}

pub static BUILTIN_ENV_VARS: &[EnvVarDesc] = &[
    EnvVarDesc { str: "bufname", prefix: false, func: |_, c| one(c.try_buffer()?.display_name()) },
    EnvVarDesc { str: "buffile", prefix: false, func: |_, c| one(c.try_buffer()?.filename()) },
    EnvVarDesc {
        str: "buflist",
        prefix: false,
        func: |_, _| Ok(BufferManager::instance().buffers().iter().map(|b| b.display_name()).collect()),
    },
    EnvVarDesc { str: "buf_line_count", prefix: false, func: |_, c| num(c.try_buffer()?.line_count()) },
    EnvVarDesc { str: "timestamp", prefix: false, func: |_, c| num(c.try_buffer()?.timestamp()) },
    EnvVarDesc { str: "history_id", prefix: false, func: |_, c| num(c.try_buffer()?.current_history_id()) },
    EnvVarDesc {
        str: "selection",
        prefix: false,
        func: |_, c| {
            let sel = c.try_selections()?.main().clone();
            one(crate::buffer_utils::content(&c.buffer(), &sel))
        },
    },
    EnvVarDesc {
        str: "selections",
        prefix: false,
        func: |_, c| {
            c.try_selections()?;
            Ok(c.selections_content())
        },
    },
    EnvVarDesc { str: "runtime", prefix: false, func: |_, _| one(runtime_directory()) },
    EnvVarDesc { str: "config", prefix: false, func: |_, _| one(config_directory()) },
    EnvVarDesc { str: "version", prefix: false, func: |_, _| one(VERSION) },
    EnvVarDesc { str: "opt_", prefix: true, func: |name, c| Ok(c.options().get(&name[4..])?.get_as_strings()) },
    EnvVarDesc { str: "main_reg_", prefix: true, func: |name, c| one(c.main_sel_register_value(&name[9..])?) },
    EnvVarDesc {
        str: "reg_",
        prefix: true,
        func: |name, c| RegisterManager::instance().get(&name[4..])?.get(c),
    },
    EnvVarDesc {
        str: "client_env_",
        prefix: true,
        func: |name, c| one(c.try_client()?.get_env_var(&name[11..]).unwrap_or_default().to_vec()),
    },
    EnvVarDesc { str: "session", prefix: false, func: |_, _| one(Server::instance().session()) },
    EnvVarDesc { str: "client", prefix: false, func: |_, c| one(c.name()) },
    EnvVarDesc { str: "client_pid", prefix: false, func: |_, c| num(c.try_client()?.pid()) },
    EnvVarDesc {
        str: "client_list",
        prefix: false,
        func: |_, _| Ok(ClientManager::instance().clients().iter().map(|c| c.context().name()).collect()),
    },
    EnvVarDesc {
        str: "modified",
        prefix: false,
        func: |_, c| one(if c.try_buffer()?.is_modified() { "true" } else { "false" }),
    },
    EnvVarDesc { str: "cursor_line", prefix: false, func: |_, c| num(c.try_selections()?.main().cursor().line + 1) },
    EnvVarDesc { str: "cursor_column", prefix: false, func: |_, c| num(c.try_selections()?.main().cursor().column + 1) },
    EnvVarDesc {
        str: "cursor_char_value",
        prefix: false,
        func: |_, c| {
            let coord = c.try_selections()?.main().cursor().coord;
            let buffer = c.buffer();
            let lines = buffer.lines();
            let it = crate::buffer::BufferIterator::new(&lines, coord);
            let end = crate::buffer::BufferIterator::end(&lines);
            num(utf8::it_codepoint(&it, &end) as usize)
        },
    },
    EnvVarDesc {
        str: "cursor_char_column",
        prefix: false,
        func: |_, c| {
            let coord = c.try_selections()?.main().cursor().coord;
            let line = c.buffer().line(coord.line);
            num(utf8::distance(&line[..coord.column as usize]) + 1)
        },
    },
    EnvVarDesc {
        str: "cursor_display_column",
        prefix: false,
        func: |_, c| {
            let coord = c.try_selections()?.main().cursor().coord;
            num(get_column(&c.buffer(), c.options().val::<i32>("tabstop"), coord) + 1)
        },
    },
    EnvVarDesc {
        str: "cursor_byte_offset",
        prefix: false,
        func: |_, c| {
            let cursor = c.try_selections()?.main().cursor().coord;
            num(c.buffer().distance(BufferCoord::new(0, 0), cursor))
        },
    },
    EnvVarDesc {
        str: "recording_register",
        prefix: false,
        func: |_, c| {
            let reg = c.try_client()?.input_handler().recording_reg();
            one(if reg != 0 { utf8::encode(reg) } else { Vec::new() })
        },
    },
    EnvVarDesc {
        str: "selection_desc",
        prefix: false,
        func: |_, c| {
            let sel = c.try_selections()?.main().clone();
            one(selection_to_string(ColumnType::Byte, &c.buffer(), &sel, -1))
        },
    },
    EnvVarDesc {
        str: "selections_desc",
        prefix: false,
        func: |_, c| {
            let buffer = c.try_buffer()?;
            Ok(main_sel_first(c)?.iter().map(|s| selection_to_string(ColumnType::Byte, &buffer, s, -1)).collect())
        },
    },
    EnvVarDesc {
        str: "selections_char_desc",
        prefix: false,
        func: |_, c| {
            let buffer = c.try_buffer()?;
            Ok(main_sel_first(c)?.iter().map(|s| selection_to_string(ColumnType::Codepoint, &buffer, s, -1)).collect())
        },
    },
    EnvVarDesc {
        str: "selections_display_column_desc",
        prefix: false,
        func: |_, c| {
            let buffer = c.try_buffer()?;
            let tabstop = c.options().val::<i32>("tabstop");
            Ok(main_sel_first(c)?
                .iter()
                .map(|s| selection_to_string(ColumnType::DisplayColumn, &buffer, s, tabstop))
                .collect())
        },
    },
    EnvVarDesc {
        str: "selection_length",
        prefix: false,
        func: |_, c| {
            let sel = c.try_selections()?.main().clone();
            num(char_length_sel(&c.buffer(), &sel))
        },
    },
    EnvVarDesc {
        str: "selections_length",
        prefix: false,
        func: |_, c| {
            let sels = c.try_selections()?.as_slice().to_vec();
            let buffer = c.buffer();
            Ok(sels.iter().map(|s| char_length_sel(&buffer, s).to_string().into_bytes()).collect())
        },
    },
    EnvVarDesc { str: "selection_count", prefix: false, func: |_, c| num(c.try_selections()?.size()) },
    EnvVarDesc { str: "window_width", prefix: false, func: |_, c| num(c.try_window()?.dimensions().column) },
    EnvVarDesc { str: "window_height", prefix: false, func: |_, c| num(c.try_window()?.dimensions().line) },
    EnvVarDesc { str: "user_modes", prefix: false, func: |_, c| Ok(c.keymaps().user_modes()) },
    EnvVarDesc {
        str: "window_range",
        prefix: false,
        func: |_, c| {
            let window = c.try_window()?;
            let setup = window.last_display_setup();
            let width = window.dimensions().column - setup.widget_columns;
            Ok(vec![
                setup.first_line.to_string().into_bytes(),
                setup.first_column.to_string().into_bytes(),
                setup.line_count.to_string().into_bytes(),
                width.to_string().into_bytes(),
            ])
        },
    },
    EnvVarDesc { str: "history", prefix: false, func: |_, c| Ok(history_as_strings(&c.try_buffer()?.history())) },
    EnvVarDesc {
        str: "history_since_",
        prefix: true,
        func: |name, c| {
            let buffer = c.try_buffer()?;
            let history = buffer.history();
            let from = (str_to_int(&name[14..])? + 1).max(0) as usize;
            Ok(history_as_strings(&history[from.min(history.len())..]))
        },
    },
    EnvVarDesc {
        str: "uncommitted_modifications",
        prefix: false,
        func: |_, c| Ok(undo_group_as_strings(&c.try_buffer()?.current_undo_group())),
    },
];

fn register_registers() {
    let register_manager = RegisterManager::instance();
    for c in "abcdefghijklmnopqrstuvwxyz\"^@".chars() {
        register_manager.add_register(c as u32, Rc::new(StaticRegister::new(&c.to_string())));
    }
    for c in "/|:\\".chars() {
        register_manager.add_register(c as u32, Rc::new(HistoryRegister::new(&c.to_string())));
    }

    register_manager.add_register('%' as u32, make_dyn_reg(|c| Ok(vec![c.try_buffer()?.display_name()])));
    register_manager.add_register(
        '.' as u32,
        make_dyn_reg(|c| {
            c.try_selections()?;
            Ok(c.selections_content())
        }),
    );
    register_manager.add_register(
        '#' as u32,
        make_dyn_reg(|c| {
            let count = c.try_selections()?.size();
            Ok((1..count + 1).map(|i| i.to_string().into_bytes()).collect())
        }),
    );

    for i in 0..10usize {
        register_manager.add_register(
            '0' as u32 + i as u32,
            make_dyn_reg_with_setter(
                move |c| {
                    let sels = c.try_selections()?;
                    Ok(sels.iter().map(|sel| sel.captures().get(i).cloned().unwrap_or_default()).collect())
                },
                move |c, values| {
                    if values.is_empty() {
                        return Ok(());
                    }
                    let mut sels = c.try_selections()?;
                    let count = sels.size();
                    for sel_index in 0..count {
                        let sel = &mut sels[sel_index];
                        if sel.captures().len() < i + 1 {
                            sel.captures_mut().resize(i + 1, Vec::new());
                        }
                        sel.captures_mut()[i] = values[sel_index.min(values.len() - 1)].clone();
                    }
                    Ok(())
                },
            ),
        );
    }

    register_manager.add_register('_' as u32, null_register());
}

fn register_keymaps() {
    let keymaps = GlobalScope::instance().keymaps();
    let map = |from: Key, to: Key| keymaps.map_key(from, KeymapMode::Normal, vec![to], Vec::new(), false);
    map(Key::new(named::LEFT), key('h'));
    map(Key::new(named::RIGHT), key('l'));
    map(Key::new(named::DOWN), key('j'));
    map(Key::new(named::UP), key('k'));
    map(shift(Key::new(named::LEFT)), key('H'));
    map(shift(Key::new(named::RIGHT)), key('L'));
    map(shift(Key::new(named::DOWN)), key('J'));
    map(shift(Key::new(named::UP)), key('K'));
    map(Key::new(named::END), alt(key('l')));
    map(Key::new(named::HOME), alt(key('h')));
    map(shift(Key::new(named::END)), alt(key('L')));
    map(shift(Key::new(named::HOME)), alt(key('H')));
}

fn check_tabstop(val: &dyn std::any::Any) -> Result<()> {
    if *val.downcast_ref::<i32>().unwrap() < 1 {
        return Err(Error::runtime("tabstop should be strictly positive"));
    }
    Ok(())
}

fn check_indentwidth(val: &dyn std::any::Any) -> Result<()> {
    if *val.downcast_ref::<i32>().unwrap() < 0 {
        return Err(Error::runtime("indentwidth should be positive or zero"));
    }
    Ok(())
}

fn check_scrolloff(val: &dyn std::any::Any) -> Result<()> {
    let so = val.downcast_ref::<DisplayCoord>().unwrap();
    if so.line < 0 || so.column < 0 {
        return Err(Error::runtime("scroll offset must be positive or zero"));
    }
    Ok(())
}

fn check_timeout(val: &dyn std::any::Any) -> Result<()> {
    if *val.downcast_ref::<i32>().unwrap() < 50 {
        return Err(Error::runtime("the minimum acceptable timeout is 50 milliseconds"));
    }
    Ok(())
}

fn check_extra_word_chars(val: &dyn std::any::Any) -> Result<()> {
    let chars = val.downcast_ref::<Vec<CodepointOpt>>().unwrap();
    if chars.iter().any(|c| is_blank(c.0)) {
        return Err(Error::runtime("blanks are not accepted for extra completion characters"));
    }
    Ok(())
}

fn check_matching_pairs(val: &dyn std::any::Any) -> Result<()> {
    let pairs = val.downcast_ref::<Vec<CodepointOpt>>().unwrap();
    if pairs.len() % 2 != 0 {
        return Err(Error::runtime("matching pairs should have a pair number of element"));
    }
    if !pairs.iter().all(|c| is_punctuation(c.0, DEFAULT_EXTRA_WORD_CHARS)) {
        return Err(Error::runtime("matching pairs can only be punctuation"));
    }
    Ok(())
}

const UI_OPTIONS_DOC: &str = "space separated list of <key>=<value> options that are \
passed to and interpreted by the user interface\n\
\n\
The terminal ui supports the following options:\n\
\x20   <key>:                        <value>:\n\
\x20   terminal_assistant             clippy|cat|dilbert|none|off\n\
\x20   terminal_status_on_top         bool\n\
\x20   terminal_set_title             bool\n\
\x20   terminal_title                 str\n\
\x20   terminal_enable_mouse          bool\n\
\x20   terminal_synchronized          bool\n\
\x20   terminal_wheel_scroll_amount   int\n\
\x20   terminal_shift_function_key    int\n\
\x20   terminal_padding_char          codepoint\n\
\x20   terminal_padding_fill          bool\n\
\x20   terminal_cursor_native         bool\n\
\x20   terminal_info_max_width        int\n";

pub fn register_options() -> Result<()> {
    let reg: &OptionsRegistry = GlobalScope::instance().option_registry();
    let none = OptionFlags::None;
    reg.declare_option_checked(b"tabstop", b"size of a tab character", 8i32, none, Some(check_tabstop))?;
    reg.declare_option_checked(b"indentwidth", b"indentation width", 4i32, none, Some(check_indentwidth))?;
    reg.declare_option_checked(
        b"scrolloff",
        b"number of lines and columns to keep visible main cursor when scrolling",
        DisplayCoord::new(0, 0),
        none,
        Some(check_scrolloff),
    )?;
    reg.declare_option(b"eolformat", b"end of line format", EolFormat::Lf, none)?;
    reg.declare_option(b"finaleol", b"write and end of line at end of file", FinalEol::Present, none)?;
    reg.declare_option(b"BOM", b"byte order mark to use when writing buffer", ByteOrderMark::None, none)?;
    reg.declare_option(b"incsearch", b"incrementally apply search/select/split regex", true, none)?;
    reg.declare_option(b"autoinfo", b"automatically display contextual help", AutoInfo::Command | AutoInfo::OnKey, none)?;
    reg.declare_option(
        b"autocomplete",
        b"automatically display possible completions",
        AutoComplete::Insert | AutoComplete::Prompt,
        none,
    )?;
    reg.declare_option(b"aligntab", b"use tab characters when possible for alignment", false, none)?;
    reg.declare_option(
        b"ignored_files",
        b"patterns to ignore when completing filenames",
        Regex::new(br"^(\..*|.*\.(o|so|a))$", RegexCompileFlags::NONE)?,
        none,
    )?;
    reg.declare_option(b"disabled_hooks", b"patterns to disable hooks whose group is matched", Regex::default(), none)?;
    reg.declare_option(b"filetype", b"buffer filetype", Vec::<u8>::new(), none)?;
    reg.declare_option(
        b"path",
        b"path to consider when trying to find a file",
        vec![b"./".to_vec(), b"%/".to_vec(), b"/usr/include".to_vec()],
        none,
    )?;
    reg.declare_option(
        b"completers",
        b"insert mode completers to execute.",
        vec![
            InsertCompleterDesc { mode: InsertCompleterMode::Filename, param: None },
            InsertCompleterDesc { mode: InsertCompleterMode::Word, param: Some(b"all".to_vec()) },
        ],
        none,
    )?;
    reg.declare_option(
        b"static_words",
        b"list of words to always consider for insert word completion",
        Vec::<Vec<u8>>::new(),
        none,
    )?;
    reg.declare_option(
        b"autoreload",
        b"autoreload buffer when a filesystem modification is detected",
        Autoreload::Ask,
        none,
    )?;
    reg.declare_option(b"writemethod", b"how to write buffer to files", WriteMethod::Overwrite, none)?;
    reg.declare_option_checked(
        b"idle_timeout",
        b"timeout, in milliseconds, before idle hooks are triggered",
        50i32,
        none,
        Some(check_timeout),
    )?;
    reg.declare_option_checked(
        b"fs_check_timeout",
        b"timeout, in milliseconds, between file system buffer modification checks",
        500i32,
        none,
        Some(check_timeout),
    )?;
    reg.declare_option(b"ui_options", UI_OPTIONS_DOC.as_bytes(), UiOptions::new(), none)?;
    reg.declare_option(
        b"modelinefmt",
        b"format string used to generate the modeline",
        b"%val{bufname} %val{cursor_line}:%val{cursor_char_column} {{context_info}} {{mode_info}} - %val{client}@[%val{session}]".to_vec(),
        none,
    )?;
    reg.declare_option(b"debug", b"various debug flags", DebugFlags::None, none)?;
    reg.declare_option(b"readonly", b"prevent buffers from being modified", false, none)?;
    reg.declare_option_checked(
        b"extra_word_chars",
        b"Additional characters to be considered as words for insert completion",
        vec![CodepointOpt('_' as u32)],
        none,
        Some(check_extra_word_chars),
    )?;
    reg.declare_option_checked(
        b"matching_pairs",
        b"set of pair of characters to be considered as matching pairs",
        "(){}[]<>".chars().map(|c| CodepointOpt(c as u32)).collect::<Vec<_>>(),
        none,
        Some(check_matching_pairs),
    )?;
    reg.declare_option(
        b"startup_info_version",
        b"version up to which startup info changes should be hidden",
        0i32,
        none,
    )?;
    let _ = StringMap::new();
    Ok(())
}

thread_local! {
    static LOCAL_CLIENT: std::cell::RefCell<Option<std::rc::Weak<Client>>> = const { std::cell::RefCell::new(None) };
}
static CONVERT_TO_CLIENT_PENDING: AtomicBool = AtomicBool::new(false);
static TERMINATE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UIType {
    Terminal,
    Json,
    Dummy,
}

fn parse_ui_type(ui_name: &[u8]) -> Result<UIType> {
    match ui_name {
        b"terminal" => Ok(UIType::Terminal),
        b"json" => Ok(UIType::Json),
        b"dummy" => Ok(UIType::Dummy),
        _ => Err(Error::new(ErrorKind::ParameterError, kformat!("error: unknown ui type: '{}'", ui_name))),
    }
}

struct DummyUI;

impl DummyUI {
    fn new() -> Self {
        set_signal_handler(libc::SIGINT, libc::SIG_DFL);
        DummyUI
    }
}

impl UserInterface for DummyUI {
    fn is_ok(&self) -> bool {
        true
    }
    fn menu_show(&self, _: &[DisplayLine], _: DisplayCoord, _: Face, _: Face, _: MenuStyle) {}
    fn menu_select(&self, _: i32) {}
    fn menu_hide(&self) {}
    fn info_show(&self, _: &DisplayLine, _: &DisplayLineList, _: DisplayCoord, _: Face, _: InfoStyle) {}
    fn info_hide(&self) {}
    fn draw(&self, _: &DisplayBuffer, _: DisplayCoord, _: &Face, _: &Face, _: ColumnCount) {}
    fn draw_status(&self, _: &DisplayLine, _: &DisplayLine, _: ColumnCount, _: &DisplayLine, _: &Face, _: StatusStyle) {}
    fn dimensions(&self) -> DisplayCoord {
        DisplayCoord::new(24, 80)
    }
    fn refresh(&self, _: bool) {}
    fn set_on_key(&self, _: OnKeyCallback) {}
    fn set_on_paste(&self, _: OnPasteCallback) {}
    fn set_ui_options(&self, _: &UiOptions) {}
}

fn make_ui(ui_type: UIType) -> Result<Box<dyn UserInterface>> {
    Ok(match ui_type {
        UIType::Terminal => Box::new(TerminalUI::new()?),
        UIType::Json => Box::new(JsonUI::new()),
        UIType::Dummy => Box::new(DummyUI::new()),
    })
}

fn fork_server_to_background() -> i32 {
    let pid = unsafe { libc::fork() };
    if pid != 0 {
        return pid;
    }
    unsafe { libc::setsid() };
    if unsafe { libc::fork() } != 0 {
        // double fork to orphan the server
        unsafe { libc::_exit(0) };
    }
    write_stderr(&kformat!(
        "Kakoune forked server to background ({}), for session '{}'\n",
        unsafe { libc::getpid() },
        Server::instance().session()
    ));
    0
}

static OLD_TSTP_HANDLER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn local_ui_tstp_handler(sig: i32) {
    let old_handler = OLD_TSTP_HANDLER.load(Ordering::SeqCst);
    let only_local = ClientManager::has_instance()
        && ClientManager::instance().count() == 1
        && LOCAL_CLIENT.with(|l| {
            l.borrow().as_ref().and_then(|w| w.upgrade()).is_some_and(|lc| {
                ClientManager::instance().clients().first().is_some_and(|c| Rc::ptr_eq(c, &lc))
            })
        })
        && !Server::instance().is_daemon();
    if only_local {
        if old_handler != libc::SIG_DFL && old_handler != libc::SIG_IGN {
            let f: extern "C" fn(i32) = unsafe { std::mem::transmute(old_handler) };
            f(sig);
        }
    } else {
        CONVERT_TO_CLIENT_PENDING.store(true, Ordering::SeqCst);
        set_signal_handler(libc::SIGTSTP, old_handler);
    }
}

fn create_local_ui(ui_type: UIType) -> Result<Box<dyn UserInterface>> {
    let ui = make_ui(ui_type)?;
    let old = set_signal_handler(libc::SIGTSTP, local_ui_tstp_handler as *const () as SignalHandler);
    OLD_TSTP_HANDLER.store(old, Ordering::SeqCst);
    Ok(ui)
}

fn is_disconnected(e: &Error) -> bool {
    e.kind == ErrorKind::Disconnected
}

fn run_client(
    session: &[u8],
    name: &[u8],
    client_init: &[u8],
    init_coord: Option<BufferCoord>,
    ui_type: UIType,
    suspend: bool,
) -> i32 {
    let res = (|| -> Result<i32> {
        let mut stdin_fd = None;
        // json-ui (or dummy) is not intended to be user interactive.
        // So only worry about making the tty your stdin if:
        // (a) ui_type is Terminal, *and*
        // (b) fd 0 is not interactive.
        if ui_type == UIType::Terminal && unsafe { libc::isatty(0) } == 0 {
            // move stdin to another fd, and restore tty as stdin
            unsafe {
                stdin_fd = Some(libc::dup(0));
                let tty = libc::open(c"/dev/tty".as_ptr(), libc::O_RDONLY);
                libc::dup2(tty, 0);
                libc::close(tty);
            }
        }

        let event_manager = EventManager::create();
        let client = RemoteClient::new(
            session,
            name,
            make_ui(ui_type)?,
            unsafe { libc::getpid() },
            &get_env_vars(),
            client_init,
            init_coord,
            stdin_fd,
        )?;
        if let Some(fd) = stdin_fd {
            unsafe { libc::close(fd) };
        }

        if suspend {
            unsafe { libc::raise(libc::SIGTSTP) };
        }
        while client.exit_status().is_none() && client.is_ui_ok() {
            event_manager.try_handle_next_events(EventMode::Normal, None, None)?;
        }
        Ok(client.exit_status().unwrap_or(-1))
    })();
    match res {
        Ok(status) => status,
        Err(e) if is_disconnected(&e) => {
            write_stderr(&kformat!("{}\ndisconnecting\n", e.what()));
            -1
        }
        Err(e) => {
            write_stderr(&kformat!("Fatal error: {}\n", e.what()));
            -1
        }
    }
}

crate::kak_flags! {
    pub struct ServerFlags: u32 {
        const None        = 0;
        const IgnoreKakrc = 1 << 0;
        const Daemon      = 1 << 1;
        const ReadOnly    = 1 << 2;
        const StartupInfo = 1 << 3;
    }
}

extern "C" fn terminate_handler(_: i32) {
    TERMINATE.store(true, Ordering::SeqCst);
}

fn local_client() -> Option<Rc<Client>> {
    LOCAL_CLIENT.with(|l| l.borrow().as_ref().and_then(|w| w.upgrade()))
}

fn set_local_client(client: Option<&Rc<Client>>) {
    LOCAL_CLIENT.with(|l| *l.borrow_mut() = client.map(Rc::downgrade));
}

#[allow(clippy::too_many_arguments)]
fn run_server(
    session: &[u8],
    server_init: &[u8],
    client_init: &[u8],
    init_buffer: &[u8],
    init_coord: Option<BufferCoord>,
    flags: ServerFlags,
    ui_type: UIType,
    debug_flags: DebugFlags,
    files: &[Vec<u8>],
) -> Result<i32> {
    set_signal_handler(libc::SIGTERM, terminate_handler as *const () as SignalHandler);
    set_signal_handler(libc::SIGINT, terminate_handler as *const () as SignalHandler);

    if flags.has(ServerFlags::Daemon) && session.is_empty() {
        write_stderr(b"-d needs a session name to be specified with -s\n");
        return Ok(-1);
    }

    let event_manager = EventManager::create();
    let server = Server::create(
        if session.is_empty() { unsafe { libc::getpid() }.to_string().into_bytes() } else { session.to_vec() },
        flags.has(ServerFlags::Daemon),
    )?;
    let _server_guard = crate::utils::OnScopeEnd::new(Server::destroy);
    let global_scope = GlobalScope::create();
    let command_manager = CommandManager::instance();
    ShellManager::create(BUILTIN_ENV_VARS)?;
    RegisterManager::create();
    let client_manager = ClientManager::create();
    let buffer_manager = BufferManager::create();

    register_options()?;
    register_registers();
    register_keymaps();
    crate::commands::register_commands();
    crate::highlighters::register_highlighters();

    global_scope.options().opt("debug").set(debug_flags, true)?;

    write_to_debug_buffer(b"*** This is the debug buffer, where debug info will be written ***");

    let mut startup_error = false;
    if !flags.has(ServerFlags::IgnoreKakrc) {
        let init_context = Context::new_empty();
        if let Err(error) =
            command_manager.execute(&kformat!("source {}/kakrc", runtime_directory()), &init_context, &ShellContext::default())
        {
            if !error.is_runtime() {
                return Err(error);
            }
            startup_error = true;
            write_to_debug_buffer(&kformat!("error while parsing kakrc:\n    {}", error.what()));
        }
    }

    {
        let empty_context = Context::new_empty();
        global_scope.hooks().run_hook(Hook::EnterDirectory, &real_path(b"."), &empty_context)?;
        global_scope.hooks().run_hook(Hook::KakBegin, session, &empty_context)?;
    }

    if !server_init.is_empty() {
        let init_context = Context::new_empty();
        match command_manager.execute(server_init, &init_context, &ShellContext::default()) {
            Ok(()) => {}
            Err(e) => match e.kind {
                ErrorKind::KillSession(status) => {
                    let empty_context = Context::new_empty();
                    global_scope.hooks().run_hook(Hook::KakEnd, b"", &empty_context)?;
                    return Ok(status);
                }
                _ if e.is_runtime() => {
                    startup_error = true;
                    write_to_debug_buffer(&kformat!("error while running server init commands:\n    {}", e.what()));
                }
                _ => return Err(e),
            },
        }
    }

    for file in files {
        match open_or_create_file_buffer(file, BufferFlags::None) {
            Ok(buffer) => {
                if flags.has(ServerFlags::ReadOnly) {
                    buffer.add_flags(BufferFlags::ReadOnly);
                    buffer.options().get_local_option(b"readonly")?.set(true, true)?;
                }
            }
            Err(error) if error.is_runtime() => {
                startup_error = true;
                write_to_debug_buffer(&kformat!("error while opening file '{}':\n    {}", file, error.what()));
            }
            Err(error) => return Err(error),
        }
    }

    let exit_status = Rc::new(Cell::new(0));
    let res = (|| -> Result<()> {
        if ui_type == UIType::Terminal && unsafe { libc::isatty(0) } == 0 {
            // move stdin to another fd, and restore tty as stdin
            let fd = unsafe { libc::dup(0) };
            unsafe {
                let tty = libc::open(c"/dev/tty".as_ptr(), libc::O_RDONLY);
                libc::dup2(tty, 0);
                libc::close(tty);
            }
            create_fifo_buffer(b"*stdin*".to_vec(), fd, BufferFlags::None, AutoScroll::NotInitially)?;
        }

        if !server.is_daemon() {
            let status = exit_status.clone();
            let client = client_manager.create_client(
                create_local_ui(ui_type)?,
                unsafe { libc::getpid() },
                Vec::new(),
                get_env_vars(),
                client_init,
                init_buffer,
                init_coord,
                Box::new(move |s| status.set(s)),
            )?;
            set_local_client(client.as_ref());

            if let Some(local_client) = local_client() {
                if startup_error {
                    local_client.print_status(
                        DisplayLine::default(),
                        DisplayLine::from_str_face(
                            "error during startup, see `:buffer *debug*` for details",
                            local_client.context().faces().face("Error"),
                        ),
                        -1,
                        StatusStyle::Status,
                    );
                }
                if flags.has(ServerFlags::StartupInfo) {
                    show_startup_info(&local_client, global_scope.options().val::<i32>("startup_info_version"));
                }
            }
        }

        while !TERMINATE.load(Ordering::SeqCst)
            && (!client_manager.empty() || server.negotiating() || server.is_daemon())
        {
            client_manager.redraw_clients()?;

            // Loop so that eventual inputs happening during the processing are handled as
            // well, avoiding unneeded redraws.
            let mut timeout = if client_manager.has_pending_inputs() { Some(Duration::ZERO) } else { None };
            let res = (|| -> Result<()> {
                while event_manager.try_handle_next_events(EventMode::Normal, None, timeout)? {
                    if client_manager.process_pending_inputs()? {
                        break;
                    }
                    timeout = Some(Duration::ZERO);
                }
                Ok(())
            })();
            match res {
                Ok(()) => {}
                Err(e) if e.is_cancel() => {}
                Err(e) => return Err(e),
            }

            client_manager.process_pending_inputs()?;

            client_manager.clear_client_trash();
            client_manager.clear_window_trash();
            buffer_manager.clear_buffer_trash();

            let local_client_alive = local_client().is_some_and(|lc| client_manager.clients().iter().any(|c| Rc::ptr_eq(c, &lc)));
            let had_local_client = LOCAL_CLIENT.with(|l| l.borrow().is_some());
            if had_local_client && !local_client_alive {
                set_local_client(None);
                if (!client_manager.empty() || server.is_daemon()) && fork_server_to_background() != 0 {
                    // We do not want to run destructors and hooks here
                    unsafe { libc::_exit(exit_status.get()) };
                }
            } else if CONVERT_TO_CLIENT_PENDING.load(Ordering::SeqCst) {
                let local = local_client().expect("local client");
                let local_context = local.context();
                let client_name = local_context.name();
                let buffer_name = local_context.buffer().name();
                let selections = selection_list_to_string(ColumnType::Byte, &local_context.selections(), -1);

                client_manager.remove_client(&local, true, 0)?;
                client_manager.clear_client_trash();
                set_local_client(None);
                CONVERT_TO_CLIENT_PENDING.store(false, Ordering::SeqCst);

                if fork_server_to_background() != 0 {
                    client_manager.clear(false);
                    let session = server.session();
                    server.close_session(false);
                    return Err(Error::new(
                        ErrorKind::ConvertToClientMode { session, client_name, buffer_name, selections },
                        "convert to client mode",
                    ));
                }
            }
        }
        Ok(())
    })();

    match res {
        Ok(()) => {}
        Err(e) => match e.kind {
            ErrorKind::KillSession(status) => exit_status.set(status),
            _ => return Err(e),
        },
    }

    {
        let empty_context = Context::new_empty();
        global_scope.hooks().run_hook(Hook::KakEnd, b"", &empty_context)?;
    }

    shutdown();
    Ok(exit_status.get())
}

/// Port of the singletons destruction order at the end of run_server
fn shutdown() {
    if BufferManager::has_instance() {
        BufferManager::instance().shutdown();
    }
    if ClientManager::has_instance() {
        ClientManager::instance().clear(true);
    }
}

fn run_filter(keystr: &[u8], files: &[Vec<u8>], quiet: bool, suffix_backup: &[u8]) -> Result<i32> {
    GlobalScope::create();
    EventManager::create();
    ShellManager::create(BUILTIN_ENV_VARS)?;
    RegisterManager::create();
    let buffer_manager = BufferManager::create();

    register_options()?;
    register_registers();

    let res = (|| -> Result<()> {
        let keys = parse_keys(keystr)?;

        let apply_to_buffer = |buffer: &Rc<crate::buffer::Buffer>| -> Result<()> {
            let res = (|| -> Result<()> {
                let input_handler = InputHandler::new(
                    crate::selection::SelectionList::new(
                        buffer.clone(),
                        Selection::from_range(BufferCoord::new(0, 0), buffer.back_coord()),
                    ),
                    ContextFlags::Draft,
                    Vec::new(),
                );
                for key in &keys {
                    input_handler.handle_key(*key, true)?;
                }
                Ok(())
            })();
            match res {
                Ok(()) => Ok(()),
                Err(err) if err.is_runtime() => {
                    if !quiet {
                        write_stderr(&kformat!(
                            "error while applying keys to buffer '{}': {}\n",
                            buffer.display_name(),
                            err.what()
                        ));
                    }
                    Ok(())
                }
                Err(err) => Err(err),
            }
        };

        for file in files {
            let buffer = open_file_buffer(file, BufferFlags::NoHooks)?;
            if !suffix_backup.is_empty() {
                write_buffer_to_file(
                    &buffer,
                    &kformat!("{}{}", buffer.filename(), suffix_backup),
                    WriteMethod::Overwrite,
                    WriteFlags::None,
                    None,
                )?;
            }
            apply_to_buffer(&buffer)?;
            write_buffer_to_file(&buffer, &buffer.filename(), WriteMethod::Overwrite, WriteFlags::None, None)?;
            buffer_manager.delete_buffer(&buffer)?;
        }
        if unsafe { libc::isatty(0) } == 0 {
            let buffer = create_buffer_from_string(b"*stdin*".to_vec(), BufferFlags::NoHooks, &read_fd(0, false)?)?;
            apply_to_buffer(&buffer)?;
            write_buffer_to_fd(&buffer, 1, None)?;
            buffer_manager.delete_buffer(&buffer)?;
        }
        Ok(())
    })();
    if let Err(err) = res {
        if !err.is_runtime() {
            return Err(err);
        }
        write_stderr(&kformat!("error: {}\n", err.what()));
    }

    buffer_manager.clear_buffer_trash();
    Ok(0)
}

fn run_pipe(session: &[u8]) -> i32 {
    let res = (|| -> Result<()> {
        let command = read_fd(0, false)?;
        send_command(session, &command)
    })();
    match res {
        Ok(()) => 0,
        Err(e) if is_disconnected(&e) => {
            write_stderr(&kformat!("{}\ndisconnecting\n", e.what()));
            -1
        }
        Err(e) => {
            write_stderr(&kformat!("Fatal error: {}\n", e.what()));
            -1
        }
    }
}

extern "C" fn signal_handler(signal: i32) {
    TerminalUI::restore_terminal();
    let text = match signal {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGFPE => "SIGFPE",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGPIPE => "SIGPIPE",
        _ => "",
    };
    let msg = kformat!(
        "Received {}, exiting.\nPid: {}\nCallstack:\n{}",
        text,
        unsafe { libc::getpid() },
        std::backtrace::Backtrace::force_capture().to_string()
    );
    write_stderr(&msg);

    if Server::has_instance() {
        Server::instance().close_session(true);
    }
    if BufferManager::has_instance() {
        BufferManager::instance().backup_modified_buffers();
    }

    if signal == libc::SIGSEGV {
        // generate core dump
        unsafe {
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
            libc::kill(libc::getpid(), libc::SIGSEGV);
        }
    } else {
        std::process::abort();
    }
}

extern "C" fn noop_handler(_: i32) {}

fn param_desc() -> ParameterDesc {
    let arg = SwitchDesc::arg;
    let flag = SwitchDesc::flag;
    let switches: SwitchMap = crate::parameters_parser::switches(vec![
        ("c", arg("connect to given session")),
        ("C", arg("connect to given session, create if it does not exist")),
        ("e", arg("execute argument on client initialisation")),
        ("E", arg("execute argument on server initialisation")),
        ("n", flag("do not source kakrc files on startup")),
        ("s", arg("set session name")),
        ("d", flag("run as a headless session (requires -s)")),
        ("p", arg("just send stdin as commands to the given session")),
        ("f", arg("filter: for each file, select the entire buffer and execute the given keys")),
        ("i", arg("backup the files on which a filter is applied using the given suffix")),
        ("q", flag("in filter mode, be quiet about errors applying keys")),
        ("ui", arg("set the type of user interface to use (terminal, dummy, or json)")),
        ("l", flag("list existing sessions")),
        ("clear", flag("clear dead sessions")),
        ("debug", arg("initial debug option value")),
        ("version", flag("display kakoune version and exit")),
        ("ro", flag("readonly mode")),
        ("help", flag("display a help message and quit")),
    ]);
    ParameterDesc { switches, ..Default::default() }
}

/// Panics are caught at the top level: the panic hook restores the terminal
/// and reports the panic like the fatal signal handler does, then once the
/// stack is unwound (and all RefCell borrows released) we close the session
/// and backup modified buffers before exiting.
pub fn main() -> i32 {
    std::panic::set_hook(Box::new(|info| {
        TerminalUI::restore_terminal();
        let msg = kformat!(
            "Kakoune panicked, exiting.\nPid: {}\n{}\nCallstack:\n{}\n",
            unsafe { libc::getpid() },
            info.to_string(),
            std::backtrace::Backtrace::force_capture().to_string()
        );
        write_stderr(&msg);
    }));

    match std::panic::catch_unwind(main_impl) {
        Ok(status) => status,
        Err(_) => {
            if Server::has_instance() {
                Server::instance().close_session(true);
            }
            if BufferManager::has_instance() {
                BufferManager::instance().backup_modified_buffers();
            }
            -1
        }
    }
}

fn main_impl() -> i32 {
    unsafe { libc::setlocale(libc::LC_ALL, c"".as_ptr()) };

    let sh = |f: extern "C" fn(i32)| f as *const () as SignalHandler;
    set_signal_handler(libc::SIGSEGV, sh(signal_handler));
    set_signal_handler(libc::SIGFPE, sh(signal_handler));
    set_signal_handler(libc::SIGQUIT, sh(signal_handler));
    set_signal_handler(libc::SIGTERM, sh(signal_handler));
    set_signal_handler(libc::SIGPIPE, sh(noop_handler));
    set_signal_handler(libc::SIGINT, sh(noop_handler));
    set_signal_handler(libc::SIGCHLD, sh(noop_handler));
    set_signal_handler(libc::SIGTTOU, libc::SIG_IGN);

    let param_desc = param_desc();
    use std::os::unix::ffi::OsStrExt;
    let args: Vec<Vec<u8>> = std::env::args_os().map(|a| a.as_bytes().to_vec()).collect();
    let argv0 = args.first().cloned().unwrap_or_default();
    let params: Vec<Vec<u8>> = args[1..].to_vec();
    let argc = args.len();

    let res = (|| -> Result<i32> {
        let show_usage = || {
            write_stdout(&kformat!(
                "Usage: {} [options] [file]... [+<line>[:<col>]|+:]\n\n\
                 Options:\n\
                 {}\n\
                 Prefixing a positional argument with a plus (`+`) sign will place the\n\
                 cursor at a given set of coordinates, or the end of the buffer if the plus\n\
                 sign is followed only by a colon (`:`)\n",
                argv0,
                generate_switches_doc(&param_desc.switches)
            ));
            0
        };

        if params.iter().any(|p| p == b"--help") {
            return Ok(show_usage());
        }

        let parser = ParametersParser::new(&params, &param_desc)?;

        if parser.has_switch("help") {
            return Ok(show_usage());
        }

        if parser.has_switch("version") {
            write_stdout(&kformat!("Kakoune {}\n", VERSION));
            return Ok(0);
        }

        let list_sessions = parser.has_switch("l");
        let clear_sessions = parser.has_switch("clear");
        if list_sessions || clear_sessions {
            list_files(session_directory(), &mut |session, _| {
                if session.first() == Some(&b'.') {
                    return;
                }
                let valid = check_session(session).unwrap_or(false);
                if list_sessions {
                    write_stdout(&kformat!("{}{}\n", session, if valid { "" } else { " (dead)" }));
                }
                if !valid && clear_sessions
                    && let Ok(path) = session_path(session, false) {
                        unsafe { libc::unlink(crate::file::cstr(&path).as_ptr()) };
                    }
            });
            return Ok(0);
        }

        if let Some(session) = parser.get_switch("p") {
            for opt in ["c", "n", "s", "d", "e", "E", "ro"] {
                if parser.has_switch(opt) {
                    write_stderr(&kformat!("error: -{} is incompatible with -p\n", opt));
                    return Ok(-1);
                }
            }
            return Ok(run_pipe(session));
        }

        let mut client_init = parser.get_switch("e").unwrap_or_default().to_vec();
        let server_init = parser.get_switch("E").unwrap_or_default().to_vec();
        let ui_type = parse_ui_type(parser.get_switch("ui").unwrap_or(b"terminal"))?;

        if let Some(keys) = parser.get_switch("f") {
            if parser.has_switch("ro") {
                write_stderr(b"error: -ro is incompatible with -f\n");
                return Ok(-1);
            }
            let files = parser.positionals();
            return run_filter(keys, &files, parser.has_switch("q"), parser.get_switch("i").unwrap_or_default());
        }

        let mut files: Vec<Vec<u8>> = Vec::new();
        let mut init_coord: Option<BufferCoord> = None;
        for name in parser.iter() {
            if name.first() == Some(&b'+') {
                if name == b"+" || name == b"+:" {
                    // [DEVIATION] the C++ assigns this concatenation to a StringView,
                    // leaving it dangling (garbage commands run); see UPSTREAM-QUIRKS.md
                    client_init.extend_from_slice(b"; exec gj");
                    continue;
                }
                let colon = name.iter().position(|&c| c == b':').unwrap_or(name.len());
                if let Some(line) = str_to_int_ifp(&name[1..colon]) {
                    let column = if colon != name.len() { str_to_int_ifp(&name[colon + 1..]).unwrap_or(1) - 1 } else { 0 };
                    init_coord = Some(BufferCoord::new(0, 0).max(BufferCoord::new(line - 1, column)));
                    continue;
                }
            }
            files.push(name.to_vec());
        }

        let count = parser.has_switch("c") as i32 + parser.has_switch("C") as i32 + parser.has_switch("s") as i32;
        if count > 1 {
            write_stderr(b"error: -s, -c and -C are mutually exclusive\n");
            return Ok(-1);
        }

        if parser.has_switch("c") || parser.get_switch("C").is_some_and(|s| check_session(s).unwrap_or(false)) {
            for opt in ["n", "d", "E", "ro"] {
                if parser.has_switch(opt) {
                    write_stderr(&kformat!("error: -{} incompatible with connecting to an existing session\n", opt));
                    return Ok(-1);
                }
            }

            let session = parser.get_switch("c").or_else(|| parser.get_switch("C")).unwrap();
            let mut new_files = Vec::new();
            for name in &files {
                new_files.extend(kformat!("edit '{}'", escape(&real_path(name), b"'", b'\'')));
                if let Some(coord) = init_coord.take() {
                    new_files.extend(kformat!(" {} {}", coord.line + 1, coord.column + 1));
                }
                new_files.push(b';');
            }

            new_files.extend(client_init);
            return Ok(run_client(session, b"", &new_files, init_coord, ui_type, false));
        }

        let session = parser.get_switch("C").or_else(|| parser.get_switch("s")).unwrap_or_default();
        let ignore_kakrc = parser.has_switch("n");
        let mut flags = ServerFlags::None;
        if ignore_kakrc {
            flags |= ServerFlags::IgnoreKakrc;
        }
        if parser.has_switch("d") {
            flags |= ServerFlags::Daemon;
        }
        if parser.has_switch("ro") {
            flags |= ServerFlags::ReadOnly;
        }
        if (argc == 1 || (ignore_kakrc && argc == 2)) && unsafe { libc::isatty(0) } != 0 {
            flags |= ServerFlags::StartupInfo;
        }
        let debug_flags = DebugFlags::elem_from_string(parser.get_switch("debug").unwrap_or_default())?;
        let init_buffer = files.first().cloned().unwrap_or_default();
        match run_server(session, &server_init, &client_init, &init_buffer, init_coord, flags, ui_type, debug_flags, &files)
        {
            Ok(status) => Ok(status),
            Err(e) => match e.kind {
                ErrorKind::ConvertToClientMode { session, client_name, buffer_name, selections } => Ok(run_client(
                    &session,
                    &client_name,
                    &kformat!(
                        "try %^buffer '{}'; select '{}'^; echo converted to client only mode",
                        escape(&buffer_name, b"'^", b'\\'),
                        selections
                    ),
                    None,
                    ui_type,
                    true,
                )),
                _ => Err(e),
            },
        }
    })();

    match res {
        Ok(status) => status,
        Err(error) if error.kind == ErrorKind::ParameterError => {
            write_stderr(&kformat!(
                "Error while parsing parameters: {}\nValid switches:\n{}",
                error.what(),
                generate_switches_doc(&param_desc.switches)
            ));
            -1
        }
        Err(error) => {
            write_stderr(&kformat!("Fatal error: {}\n", error.what()));
            -1
        }
    }
}
