//! Port of commands.hh/cc

use crate::buffer::{Buffer, BufferFlags, FinalEol};
use crate::buffer_manager::BufferManager;
use crate::buffer_utils::{
    create_buffer_from_string, create_fifo_buffer, generate_buffer_name, open_file_buffer, open_or_create_file_buffer,
    reload_file_buffer, write_buffer_to_file, AutoScroll, WriteFlags, WriteMethod,
};
use crate::client::BusyIndicator;
use crate::client_manager::ClientManager;
use crate::command_manager::{
    CommandCompleter, CommandFlags, CommandFunc, CommandHelper, CommandManager, Completer, NestedCompleter, ShellContext,
};
use crate::completion::{
    complete, complete_filename, complete_nothing, offset_pos, shell_complete, CandidateList, CompletionFlags, Completions,
    FilenameFlags,
};
use crate::context::{Context, ContextFlags, LocalScope, ScopedEdition, ScopedSelectionEdition};
use crate::debug::{write_to_debug_buffer, DebugFlags, EnvVarMap, ProfileScope};
use crate::display_buffer::{parse_display_line, parse_display_line_list, DisplayLine, DisplayLineBuiltins};
use crate::event_manager::{EventMode, FDWatcher, FdEvents};
use crate::exception::{Error, ErrorKind, Result};
use crate::file::{
    fd_readable, parse_filename, real_path, regular_file_exists, split_path, write_to_file, MappedFile,
};
use crate::highlighter::{Highlighter, HighlighterRegistry};
use crate::highlighter_group::HighlighterGroup;
use crate::hook_manager::{Hook, HookFlags, HOOK_DESC};
use crate::input_handler::{on_next_key_with_autoinfo, InputHandler, PromptCompleter, PromptFlags};
use crate::keymap_manager::KeymapMode;
use crate::keys::{key_to_str, named, parse_keys, Key};
use crate::normal::{build_autoinfo_for_mapping, NormalParams};
use crate::option_manager::{OptionFlags, OptionManager};
use crate::option_types::{enum_from_string, CompletionList, OptionElem, StringMap};
use crate::parameters_parser::{
    generate_switches_doc, parameter_error, ParameterDesc, ParameterFlags, ParametersParser, SwitchDesc,
};
use crate::ranked_match::{used_letters, RankedMatch, UsedLetters};
use crate::regex::{regex_match_ranges, regex_match_str, Regex};
use crate::regex_vm::{compile_regex, dump_regex, RegexCompileFlags};
use crate::register_manager::{Register, RegisterManager};
use crate::remote::Server;
use crate::scope::{GlobalScope, Scope};
use crate::selection::{
    compare_selections, selection_list_from_strings, update_selections, ColumnType, Selection, SelectionList,
};
use crate::shell_manager::{Shell, ShellFlags, ShellManager};
use crate::string_utils::{
    indent, join, join_str, prefix_match, quote, quoter, replace, split_escaped, str_to_int, str_to_int_ifp,
    trim_indent, unescape, Quoting,
};
use crate::unicode::{is_identifier, is_word};
use crate::units::{BufferCoord, Codepoint};
use crate::user_interface::InfoStyle;
use crate::utils::ScopedSetBool;
use crate::word_db::for_n_best;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

pub type PromptStyleCompleter = Rc<dyn Fn(&Context, &[u8], usize) -> Completions>;
pub type CommandFn = fn(&ParametersParser, &Context, &ShellContext) -> Result<()>;

fn wrong_argument_count() -> Error {
    parameter_error(b"wrong argument count".to_vec())
}

fn no_selections_remaining() -> Error {
    Error::new(ErrorKind::NoSelectionsRemaining, "no selections remaining")
}

fn open_fifo(name: &[u8], filename: &[u8], flags: BufferFlags, scroll: bool) -> Result<Rc<Buffer>> {
    let path = crate::file::cstr(&parse_filename(filename, b""));
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    if fd < 0 {
        return Err(rt_err!("unable to open '{}'", filename));
    }
    create_fifo_buffer(name.to_vec(), fd, flags, if scroll { AutoScroll::Yes } else { AutoScroll::No })
}

/// Port of PerArgumentCommandCompleter / make_completer
pub fn make_completer(completers: Vec<PromptStyleCompleter>) -> CommandCompleter {
    Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
        match completers.get(token_to_complete) {
            Some(completer) => {
                let empty = Vec::new();
                let arg = params.get(token_to_complete).unwrap_or(&empty);
                completer(context, arg, pos_in_token)
            }
            None => Completions::default(),
        }
    })
}

fn add_flags(completer: PromptStyleCompleter, completions_flags: CompletionFlags) -> PromptStyleCompleter {
    Rc::new(move |context: &Context, prefix: &[u8], cursor_pos: usize| {
        let mut res = completer(context, prefix, cursor_pos);
        res.flags |= completions_flags;
        res
    })
}

fn menu(completer: PromptStyleCompleter) -> PromptStyleCompleter {
    add_flags(completer, CompletionFlags::Menu)
}

fn ps(f: fn(&Context, &[u8], usize) -> Completions) -> PromptStyleCompleter {
    Rc::new(f)
}

fn ignored_files(context: &Context) -> Regex {
    context.options().val::<Regex>("ignored_files")
}

fn filename_completer(menu: bool) -> CommandCompleter {
    make_completer(vec![Rc::new(move |context: &Context, prefix: &[u8], cursor_pos: usize| {
        Completions::with(
            0,
            cursor_pos,
            complete_filename(prefix, &ignored_files(context), cursor_pos, FilenameFlags::Expand),
            if menu { CompletionFlags::Menu } else { CompletionFlags::None },
        )
    })])
}

fn filename_arg_completer(menu: bool) -> PromptStyleCompleter {
    Rc::new(move |context: &Context, prefix: &[u8], cursor_pos: usize| {
        Completions::with(
            0,
            cursor_pos,
            complete_filename(prefix, &ignored_files(context), cursor_pos, FilenameFlags::OnlyDirectories),
            if menu { CompletionFlags::Menu } else { CompletionFlags::None },
        )
    })
}

fn client_arg_completer() -> PromptStyleCompleter {
    Rc::new(|_: &Context, prefix: &[u8], cursor_pos: usize| {
        Completions::with(
            0,
            cursor_pos,
            ClientManager::instance().complete_client_name(prefix, cursor_pos),
            CompletionFlags::Menu,
        )
    })
}

fn arg_completer(candidates: &'static [&'static str]) -> PromptStyleCompleter {
    Rc::new(move |_: &Context, prefix: &[u8], cursor_pos: usize| {
        Completions::with(
            0,
            cursor_pos,
            complete(prefix, cursor_pos, candidates.iter().map(|c| c.as_bytes())),
            CompletionFlags::Menu,
        )
    })
}

fn complete_buffer_name_impl(context: &Context, prefix: &[u8], cursor_pos: usize, ignore_current: bool) -> Completions {
    let query = &prefix[..cursor_pos.min(prefix.len())];
    let buffers = BufferManager::instance().buffers();
    let current = context.try_buffer().ok();
    let names: Vec<(Vec<u8>, bool)> = buffers
        .iter()
        .filter(|b| !(ignore_current && current.as_ref().is_some_and(|c| Rc::ptr_eq(c, b))))
        .map(|b| (b.display_name(), b.flags().has(BufferFlags::File)))
        .collect();

    let mut filename_matches: Vec<(RankedMatch, usize)> = Vec::new();
    let mut matches: Vec<(RankedMatch, usize)> = Vec::new();
    for (i, (bufname, is_file)) in names.iter().enumerate() {
        if *is_file {
            let m = RankedMatch::new(split_path(bufname).1, query);
            if m.is_match() {
                filename_matches.push((m, i));
                continue;
            }
        }
        let m = RankedMatch::new(bufname, query);
        if m.is_match() {
            matches.push((m, i));
        }
    }
    filename_matches.sort_by(|a, b| a.0.ordering(&b.0));
    matches.sort_by(|a, b| a.0.ordering(&b.0));

    let mut res = CandidateList::new();
    for (_, i) in filename_matches.iter().chain(matches.iter()) {
        res.push(names[*i].0.clone());
    }
    Completions::with(0, cursor_pos, res, CompletionFlags::None)
}

fn complete_buffer_name(ignore_current: bool) -> PromptStyleCompleter {
    Rc::new(move |context: &Context, prefix: &[u8], cursor_pos: usize| {
        complete_buffer_name_impl(context, prefix, cursor_pos, ignore_current)
    })
}

fn make_single_word_completer(func: fn(&Context) -> Vec<u8>) -> CommandCompleter {
    make_completer(vec![Rc::new(move |context: &Context, prefix: &[u8], cursor_pos: usize| {
        let candidate = vec![func(context)];
        Completions::with(0, cursor_pos, complete(prefix, cursor_pos, &candidate), CompletionFlags::None)
    })])
}

fn no_params() -> ParameterDesc {
    ParameterDesc::positionals(0, 0)
}
fn single_param() -> ParameterDesc {
    ParameterDesc::positionals(1, 1)
}
fn single_optional_param() -> ParameterDesc {
    ParameterDesc::positionals(0, 1)
}
fn double_params() -> ParameterDesc {
    ParameterDesc::positionals(2, 2)
}

fn complete_scope(_: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    let scopes: [&[u8]; 4] = [b"global", b"buffer", b"window", b"local"];
    Completions::with(0, cursor_pos, complete(prefix, cursor_pos, scopes), CompletionFlags::None)
}

fn complete_scope_including_current(_: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    let scopes: [&[u8]; 5] = [b"global", b"buffer", b"window", b"local", b"current"];
    Completions::with(0, cursor_pos, complete(prefix, cursor_pos, scopes), CompletionFlags::None)
}

fn complete_scope_no_global(_: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    let scopes: [&[u8]; 4] = [b"buffer", b"window", b"local", b"current"];
    Completions::with(0, cursor_pos, complete(prefix, cursor_pos, scopes), CompletionFlags::None)
}

fn complete_command_name(context: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    CommandManager::instance().complete_command_name(context, &prefix[..cursor_pos.min(prefix.len())])
}

/// Shared state of the shell script based completers
struct AsyncShellScript {
    shell_script: Vec<u8>,
    flags: CompletionFlags,
    running_script: RefCell<Option<Shell>>,
    watcher: RefCell<Option<FDWatcher>>,
    stdout_buffer: RefCell<Vec<u8>>,
}

impl AsyncShellScript {
    fn new(shell_script: Vec<u8>, flags: CompletionFlags) -> Self {
        AsyncShellScript {
            shell_script,
            flags,
            running_script: RefCell::new(None),
            watcher: RefCell::new(None),
            stdout_buffer: RefCell::new(Vec::new()),
        }
    }
}

type LineHandler = Rc<dyn Fn(&[u8])>;
type RankedCandidates = Vec<(Vec<u8>, UsedLetters)>;

fn spawn_script(
    state: &Rc<AsyncShellScript>,
    context: &Context,
    shell_context: &ShellContext,
    handle_line: LineHandler,
) -> Result<()> {
    let shell = ShellManager::instance().spawn(&state.shell_script, context, false, shell_context)?;
    let fd = shell.out.fd();
    *state.running_script.borrow_mut() = Some(shell);
    let weak_state = Rc::downgrade(state);
    let weak_ih = context.try_input_handler().ok().map(|ih| Rc::downgrade(&ih));
    let watcher = FDWatcher::new(fd, FdEvents::Read, EventMode::Urgent, move |_, _, _| {
        let Some(state) = weak_state.upgrade() else { return Ok(()) };
        read_stdout(&state, &weak_ih, &handle_line);
        Ok(())
    });
    *state.watcher.borrow_mut() = Some(watcher);
    Ok(())
}

fn read_stdout(state: &Rc<AsyncShellScript>, input_handler: &Option<Weak<InputHandler>>, handle_line: &LineHandler) {
    let mut buffer = [0u8; 2048];
    let mut closed = false;
    let fd = match state.running_script.borrow().as_ref() {
        Some(s) => s.out.fd(),
        None => return,
    };
    while fd_readable(fd) {
        let size = unsafe { libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len()) };
        if size == 0 {
            closed = true;
            break;
        }
        if size < 0 {
            break;
        }
        state.stdout_buffer.borrow_mut().extend_from_slice(&buffer[..size as usize]);
    }

    let lines = {
        let mut out = state.stdout_buffer.borrow_mut();
        let end = if closed { out.len() } else { out.iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1) };
        let chunk: Vec<u8> = out.drain(..end).collect();
        chunk
    };
    for line in crate::string_utils::split(&lines, b'\n') {
        if !line.is_empty() {
            handle_line(line);
        }
    }

    if let Some(ih) = input_handler.as_ref().and_then(|w| w.upgrade()) {
        ih.refresh_ifn();
    }

    if closed {
        state.running_script.borrow_mut().take();
        let watcher = state.watcher.borrow_mut().take();
        drop(watcher);
    }
}

fn shell_script_completer(shell_script: Vec<u8>, flags: CompletionFlags) -> CommandCompleter {
    struct State {
        script: Rc<AsyncShellScript>,
        candidates: Rc<RefCell<CandidateList>>,
        last_token: Cell<i64>,
        last_pos_in_token: Cell<i64>,
    }
    let state = Rc::new(State {
        script: Rc::new(AsyncShellScript::new(shell_script, flags)),
        candidates: Rc::new(RefCell::new(Vec::new())),
        last_token: Cell::new(-1),
        last_pos_in_token: Cell::new(-1),
    });
    Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
        let candidates;
        if state.last_token.get() != token_to_complete as i64 || pos_in_token as i64 != state.last_pos_in_token.get() {
            let mut env_vars = EnvVarMap::new();
            env_vars.insert(b"token_to_complete".to_vec(), token_to_complete.to_string().into_bytes());
            env_vars.insert(b"pos_in_token".to_vec(), pos_in_token.to_string().into_bytes());
            let shell_context = ShellContext { params: params.to_vec(), env_vars };
            let cands = state.candidates.clone();
            let _ = spawn_script(&state.script, context, &shell_context, Rc::new(move |line: &[u8]| {
                cands.borrow_mut().push(line.to_vec());
            }));
            // avoid completion menu flicker by keeping the previous result visible
            candidates = std::mem::take(&mut *state.candidates.borrow_mut());
            state.last_token.set(token_to_complete as i64);
            state.last_pos_in_token.set(pos_in_token as i64);
        } else {
            candidates = state.candidates.borrow().clone();
        }
        Completions::with(0, pos_in_token, candidates, state.script.flags)
    })
}

fn shell_candidates_completer(shell_script: Vec<u8>, flags: CompletionFlags) -> CommandCompleter {
    struct State {
        script: Rc<AsyncShellScript>,
        candidates: Rc<RefCell<RankedCandidates>>,
        last_token: Cell<i64>,
    }
    let state = Rc::new(State {
        script: Rc::new(AsyncShellScript::new(shell_script, flags)),
        candidates: Rc::new(RefCell::new(Vec::new())),
        last_token: Cell::new(-1),
    });
    Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
        if state.last_token.get() != token_to_complete as i64 {
            let mut env_vars = EnvVarMap::new();
            env_vars.insert(b"token_to_complete".to_vec(), token_to_complete.to_string().into_bytes());
            let shell_context = ShellContext { params: params.to_vec(), env_vars };
            let cands = state.candidates.clone();
            let _ = spawn_script(&state.script, context, &shell_context, Rc::new(move |line: &[u8]| {
                cands.borrow_mut().push((line.to_vec(), used_letters(line)));
            }));
            state.candidates.borrow_mut().clear();
            state.last_token.set(token_to_complete as i64);
        }
        let token = params.get(token_to_complete).cloned().unwrap_or_default();
        let query = &token[..pos_in_token.min(token.len())];
        let query_letters = used_letters(query);
        let candidates = state.candidates.borrow();
        let mut matches: Vec<RankedMatch> = Vec::new();
        for (i, (candidate, letters)) in candidates.iter().enumerate() {
            let mut m = RankedMatch::with_letters(candidate, *letters, query, query_letters);
            if m.is_match() {
                m.set_input_sequence_number(i);
                matches.push(m);
            }
        }
        const MAX_COUNT: usize = 100;
        let mut res = CandidateList::new();
        // Gather best max_count matches
        for_n_best(matches, MAX_COUNT, |a, b| a.less(b), |m| {
            if res.last().is_some_and(|l| l == m.candidate()) {
                return false;
            }
            res.push(m.candidate().to_vec());
            true
        });
        Completions::with(0, query.len(), res, state.script.flags)
    })
}

/// PromptCompleterAdapter: use a command completer in a prompt
fn prompt_completer_adapter(completer: Option<CommandCompleter>) -> Option<PromptCompleter> {
    let completer = completer?;
    Some(Rc::new(move |context: &Context, prefix: &[u8], cursor_pos: usize| {
        completer(context, &[prefix.to_vec()], 0, cursor_pos)
    }))
}

fn get_scope_ifp(scope: &[u8], context: &Context) -> Result<Option<Scope>> {
    if prefix_match(b"global", scope) {
        Ok(Some(GlobalScope::instance().scope().clone()))
    } else if prefix_match(b"buffer", scope) {
        Ok(Some(context.try_buffer()?.scope().clone()))
    } else if prefix_match(b"window", scope) {
        Ok(Some(context.try_window()?.scope().clone()))
    } else if prefix_match(b"local", scope) {
        Ok(context.local_scope())
    } else if prefix_match(scope, b"buffer=") {
        Ok(Some(BufferManager::instance().get_buffer(&scope[7..])?.scope().clone()))
    } else {
        Ok(None)
    }
}

fn get_scope(scope: &[u8], context: &Context) -> Result<Scope> {
    match get_scope_ifp(scope, context)? {
        Some(s) => Ok(s),
        None => Err(rt_err!("no such scope: '{}'", scope)),
    }
}

pub struct CommandDesc {
    pub name: &'static str,
    pub alias: Option<&'static str>,
    pub docstring: &'static str,
    pub params: ParameterDesc,
    pub flags: CommandFlags,
    pub helper: Option<CommandHelper>,
    pub completer: Option<CommandCompleter>,
    pub func: CommandFn,
}

fn cmd(
    name: &'static str,
    alias: Option<&'static str>,
    docstring: &'static str,
    params: ParameterDesc,
    helper: Option<CommandHelper>,
    completer: Option<CommandCompleter>,
    func: CommandFn,
) -> CommandDesc {
    CommandDesc { name, alias, docstring, params, flags: CommandFlags::None, helper, completer, func }
}

fn switches(items: Vec<(&str, SwitchDesc)>) -> crate::parameters_parser::SwitchMap {
    crate::parameters_parser::switches(items)
}

fn flag(desc: &str) -> SwitchDesc {
    SwitchDesc::flag(desc)
}

fn arg(desc: &str) -> SwitchDesc {
    SwitchDesc::arg(desc)
}

fn arg_with(completer: PromptStyleCompleter, desc: &str) -> SwitchDesc {
    SwitchDesc::arg_with(completer, desc)
}

// edit ---------------------------------------------------------------------

fn edit_impl(parser: &ParametersParser, context: &Context, force_reload: bool) -> Result<()> {
    let scratch = parser.has_switch("scratch");
    if parser.positional_count() == 0 && !force_reload && !scratch {
        return Err(wrong_argument_count());
    }

    let no_hooks = context.hooks_disabled().is_set();
    let mut flags = if no_hooks { BufferFlags::NoHooks } else { BufferFlags::None };
    if parser.has_switch("debug") {
        flags |= BufferFlags::Debug;
    }

    let buffer_manager = BufferManager::instance();
    let name = if parser.positional_count() > 0 {
        parser[0].to_vec()
    } else if scratch {
        generate_buffer_name(b"*scratch-{}*")?
    } else {
        context.try_buffer()?.name()
    };
    let mut buffer = buffer_manager.get_buffer_ifp(&name);
    if scratch {
        if parser.has_switch("readonly") || parser.has_switch("fifo") || parser.has_switch("scroll") {
            return Err(Error::runtime("scratch is not compatible with readonly, fifo or scroll"));
        }

        match &buffer {
            Some(b) if !force_reload => {
                if b.flags().has(BufferFlags::File) {
                    return Err(rt_err!("buffer '{}' exists but is not a scratch buffer", name));
                }
            }
            _ => {
                if let (Some(b), true) = (&buffer, force_reload) {
                    buffer_manager.delete_buffer(b)?;
                }
                buffer = Some(create_buffer_from_string(name.clone(), flags, b"")?);
            }
        }
    } else if force_reload && buffer.as_ref().is_some_and(|b| b.flags().has(BufferFlags::File)) {
        reload_file_buffer(buffer.as_ref().unwrap())?;
    } else {
        if let Some(fifo) = parser.get_switch("fifo") {
            buffer = Some(open_fifo(&name, fifo, flags, parser.has_switch("scroll"))?);
        } else if buffer.is_none() {
            let b = if parser.has_switch("existing") {
                open_file_buffer(&name, flags)?
            } else {
                open_or_create_file_buffer(&name, flags)?
            };
            if b.flags().has(BufferFlags::New) {
                context.print_status(DisplayLine::from_str_face(
                    kformat!("new file '{}'", name),
                    context.faces().face("StatusLine"),
                ));
            }
            buffer = Some(b);
        }

        let b = buffer.as_ref().unwrap();
        b.remove_flags(BufferFlags::NoHooks);
        if parser.has_switch("readonly") {
            b.add_flags(BufferFlags::ReadOnly);
            b.options().get_local_option(b"readonly")?.set(true, true)?;
        }
    }

    let buffer = buffer.unwrap();
    let current_buffer = context.try_buffer().ok();
    let param_count = parser.positional_count();
    if let Some(current) = &current_buffer
        && (!Rc::ptr_eq(&buffer, current) || param_count > 1) {
            context.push_jump(false);
        }

    if current_buffer.as_ref().is_none_or(|c| !Rc::ptr_eq(&buffer, c)) {
        context.change_buffer(&buffer, None)?;
    }
    let buffer = context.buffer(); // change_buffer hooks might change the buffer again

    if parser.has_switch("fifo") && !parser.has_switch("scroll") {
        *context.selections_write_only() = SelectionList::new(buffer.clone(), Selection::default());
    } else if param_count > 1 && !parser[1].is_empty() {
        let line = (str_to_int(&parser[1])? - 1).max(0);
        let column = if param_count > 2 && !parser[2].is_empty() { (str_to_int(&parser[2])? - 1).max(0) } else { 0 };

        *context.selections_write_only() =
            SelectionList::new(buffer.clone(), Selection::new(buffer.clamp(BufferCoord::new(line, column))));
        if context.has_window() {
            let cursor_line = context.selections().main().cursor().line;
            context.window().center_line(cursor_line);
        }
    }
    Ok(())
}

fn edit_params() -> ParameterDesc {
    ParameterDesc::new(
        switches(vec![
            ("existing", flag("fail if the file does not exist, do not open a new file")),
            ("scratch", flag("create a scratch buffer, not linked to a file")),
            ("debug", flag("create buffer as debug output")),
            ("fifo", arg_with(filename_arg_completer(true), "create a buffer reading its content from a named fifo")),
            ("readonly", flag("create a buffer in readonly mode")),
            ("scroll", flag("place the initial cursor so that the fifo will scroll to show new data")),
        ]),
        ParameterFlags::None,
        0,
        3,
    )
}

// write --------------------------------------------------------------------

fn method_completer() -> PromptStyleCompleter {
    arg_completer(&["replace", "overwrite"])
}

fn write_params() -> ParameterDesc {
    ParameterDesc::new(
        switches(vec![
            ("sync", flag("force the synchronization of the file onto the filesystem")),
            ("method", arg_with(method_completer(), "explicit writemethod (replace|overwrite)")),
            ("force", flag("Allow overwriting existing file with explicit filename")),
        ]),
        ParameterFlags::None,
        0,
        1,
    )
}

fn write_params_except_force() -> ParameterDesc {
    ParameterDesc::new(
        switches(vec![
            ("sync", flag("force the synchronization of the file onto the filesystem")),
            ("method", arg_with(method_completer(), "explicit writemethod (replace|overwrite)")),
        ]),
        ParameterFlags::None,
        0,
        1,
    )
}

fn parse_write_method(s: &[u8]) -> Result<WriteMethod> {
    enum_from_string::<WriteMethod>(s).map_err(|_| rt_err!("invalid writemethod '{}'", s))
}

fn do_write_buffer(
    context: &Context,
    filename: Option<Vec<u8>>,
    flags: WriteFlags,
    write_method: Option<WriteMethod>,
) -> Result<()> {
    let buffer = context.try_buffer()?;
    let is_file = buffer.flags().has(BufferFlags::File);

    if filename.is_none() && !is_file {
        return Err(Error::runtime("cannot write a non file buffer without a filename"));
    }

    let is_readonly = buffer.flags().has(BufferFlags::ReadOnly);
    // if the buffer is in read-only mode and we try to save it directly
    // or we try to write to it indirectly using e.g. a symlink, throw an error
    if is_file && is_readonly && filename.as_ref().is_none_or(|f| real_path(f) == buffer.filename()) {
        return Err(Error::runtime("cannot overwrite the buffer when in readonly mode"));
    }

    let effective_filename = match &filename {
        Some(f) => parse_filename(f, b""),
        None => buffer.filename(),
    };
    if filename.is_some()
        && !flags.has(WriteFlags::Force)
        && real_path(&effective_filename) != buffer.filename()
        && regular_file_exists(&effective_filename)
    {
        return Err(Error::runtime("cannot overwrite existing file without -force"));
    }

    let method = match write_method {
        Some(m) => m,
        None => context.options().val::<WriteMethod>("writemethod"),
    };

    context.hooks().run_hook(Hook::BufWritePre, &effective_filename, context)?;

    let filename_for_status = buffer.filename();
    let faces = context.faces();
    let busy_indicator = BusyIndicator::new(
        context,
        Box::new(move |elapsed: Duration| {
            DisplayLine::from_str_face(
                kformat!("waiting while writing buffer '{}' ({}s)", filename_for_status, elapsed.as_secs()),
                faces.face("Information"),
            )
        }),
        Instant::now(),
    );
    let finaleol = context.options().val::<FinalEol>("finaleol");
    let res = write_buffer_to_file(&buffer, &effective_filename, method, flags, Some(finaleol));
    if res.is_err() {
        busy_indicator.skip_restore();
    }
    drop(busy_indicator);
    res?;
    context.hooks().run_hook(Hook::BufWritePost, &effective_filename, context)
}

fn write_buffer_impl(parser: &ParametersParser, context: &Context, force: bool) -> Result<()> {
    let mut flags = if parser.has_switch("sync") { WriteFlags::Sync } else { WriteFlags::None };
    if parser.has_switch("force") || force {
        flags |= WriteFlags::Force;
    }
    let method = match parser.get_switch("method") {
        Some(m) => Some(parse_write_method(m)?),
        None => None,
    };
    do_write_buffer(context, if parser.positional_count() > 0 { Some(parser[0].to_vec()) } else { None }, flags, method)
}

fn write_all_buffers(context: &Context, sync: bool, write_method: Option<WriteMethod>) -> Result<()> {
    // Copy buffer list because hooks might be creating/deleting buffers
    let buffers = BufferManager::instance().buffers();
    for buffer in buffers {
        let flags = buffer.flags();
        if flags.has(BufferFlags::File)
            && (flags.has(BufferFlags::New) || buffer.is_modified())
            && !flags.has(BufferFlags::ReadOnly)
        {
            let method = match write_method {
                Some(m) => m,
                None => context.options().val::<WriteMethod>("writemethod"),
            };
            let flags = if sync { WriteFlags::Sync } else { WriteFlags::None };
            buffer.run_hook_in_own_context(Hook::BufWritePre, &buffer.name(), context.name())?;
            let faces = context.faces();
            let busy_indicator = BusyIndicator::new(
                context,
                Box::new(move |elapsed: Duration| {
                    DisplayLine::from_str_face(
                        kformat!("waiting while writing buffer ({}s)", elapsed.as_secs()),
                        faces.face("Information"),
                    )
                }),
                Instant::now(),
            );
            let res = write_buffer_to_file(&buffer, &buffer.name(), method, flags, None);
            if res.is_err() {
                busy_indicator.skip_restore();
            }
            drop(busy_indicator);
            res?;
            buffer.run_hook_in_own_context(Hook::BufWritePost, &buffer.name(), context.name())?;
        }
    }
    Ok(())
}

fn ensure_all_buffers_are_saved(context: &Context) -> Result<()> {
    let is_modified = |buf: &Rc<Buffer>| buf.flags().has(BufferFlags::File) && buf.is_modified();
    let buffers = BufferManager::instance().buffers();
    let modified: Vec<&Rc<Buffer>> = buffers.iter().filter(|b| is_modified(b)).collect();
    if modified.is_empty() {
        return Ok(());
    }

    if !context.try_buffer()?.is_modified() {
        context.push_jump(false);
        context.change_buffer(modified[0], None)?;
    }

    let mut message = kformat!("{} modified buffers remaining: [", modified.len());
    for (i, b) in modified.iter().enumerate() {
        message.extend(b.display_name());
        message.extend_from_slice(if i + 1 != modified.len() { b", " } else { b"]" });
    }
    Err(Error::runtime(message))
}

fn status_param(parser: &ParametersParser) -> Result<i32> {
    if parser.positional_count() > 0 { str_to_int(&parser[0]) } else { Ok(0) }
}

fn kill_impl(parser: &ParametersParser, context: &Context, force: bool) -> Result<()> {
    let client_manager = ClientManager::instance();

    if !force {
        ensure_all_buffers_are_saved(context)?;
    }

    let status = status_param(parser)?;
    loop {
        let first = client_manager.clients().first().cloned();
        let Some(client) = first else { break };
        client_manager.remove_client(&client, true, status)?;
    }

    Err(Error::kill_session(status))
}

fn quit_impl(parser: &ParametersParser, context: &Context, force: bool) -> Result<()> {
    if !force && ClientManager::instance().count() == 1 && !Server::instance().is_daemon() {
        ensure_all_buffers_are_saved(context)?;
    }

    let status = status_param(parser)?;
    ClientManager::instance().remove_client(&context.try_client()?, true, status)
}

fn write_quit_impl(parser: &ParametersParser, context: &Context, force: bool) -> Result<()> {
    let method = match parser.get_switch("method") {
        Some(m) => Some(parse_write_method(m)?),
        None => None,
    };
    do_write_buffer(
        context,
        None,
        if parser.has_switch("sync") { WriteFlags::Sync } else { WriteFlags::None },
        method,
    )?;
    quit_impl(parser, context, force)
}

// buffers ------------------------------------------------------------------

fn cycle_buffer(context: &Context, next: bool) -> Result<()> {
    let oldbuf = context.try_buffer()?;
    let buffers = BufferManager::instance().buffers();
    let Some(mut it) = buffers.iter().position(|b| Rc::ptr_eq(b, &oldbuf)) else { return Ok(()) };

    let mut cycle = || {
        if !next {
            if it == 0 {
                it = buffers.len();
            }
            it -= 1;
        } else {
            it += 1;
            if it == buffers.len() {
                it = 0;
            }
        }
        buffers[it].clone()
    };

    let mut newbuf = cycle();
    while !Rc::ptr_eq(&newbuf, &oldbuf) && newbuf.flags().has(BufferFlags::Debug) {
        newbuf = cycle();
    }

    if !Rc::ptr_eq(&newbuf, &oldbuf) {
        context.push_jump(false);
        context.change_buffer(&newbuf, None)?;
    }
    Ok(())
}

fn delete_buffer_impl(parser: &ParametersParser, context: &Context, force: bool) -> Result<()> {
    let manager = BufferManager::instance();
    let buffer = if parser.positional_count() == 0 { context.try_buffer()? } else { manager.get_buffer(&parser[0])? };
    if !force && buffer.flags().has(BufferFlags::File) && buffer.is_modified() {
        return Err(rt_err!("buffer '{}' is modified", buffer.display_name()));
    }

    manager.delete_buffer(&buffer)?;
    context.forget_buffer(&buffer)
}

// highlighters -------------------------------------------------------------

const HIGHLIGHTER_SCOPES: [&[u8]; 4] = [b"global/", b"buffer/", b"window/", b"shared/"];

fn highlighter_cmd_completer(add: bool) -> CommandCompleter {
    Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
        if token_to_complete == 0 {
            let path = &params[0];
            let Some(sep) = path.iter().position(|&c| c == b'/') else {
                return Completions::with(
                    0,
                    pos_in_token,
                    complete(path, pos_in_token, HIGHLIGHTER_SCOPES),
                    CompletionFlags::Menu,
                );
            };
            let scope = &path[..sep];
            let root: Rc<HighlighterGroup> = if scope == b"shared" {
                crate::highlighters::shared_highlighters()
            } else {
                match get_scope_ifp(scope, context) {
                    Ok(Some(s)) => s.highlighters().group().clone(),
                    _ => return Completions::default(),
                }
            };
            let offset = scope.len() + 1;
            match root.complete_child(&path[sep + 1..], pos_in_token.wrapping_sub(offset), add) {
                Ok(c) => offset_pos(c, offset),
                Err(_) => Completions::default(),
            }
        } else if add && token_to_complete == 1 {
            let name = &params[1];
            Completions::with(
                0,
                name.len(),
                complete(name, pos_in_token, HighlighterRegistry::instance().names()),
                CompletionFlags::Menu,
            )
        } else {
            Completions::default()
        }
    })
}

/// returns (root group, target highlighter)
fn get_highlighter(context: &Context, path: &[u8]) -> Result<(Rc<HighlighterGroup>, Rc<dyn Highlighter>)> {
    let mut path = path;
    if path.last() == Some(&b'/') {
        path = &path[..path.len() - 1];
    }

    let find_from = |from: usize| path[from.min(path.len())..].iter().position(|&c| c == b'/').map(|p| p + from).unwrap_or(path.len());
    let mut sep = find_from(0);
    if path.starts_with(b"buffer=") {
        let buffer_manager = BufferManager::instance();
        while sep != path.len() && buffer_manager.get_buffer_ifp(&path[7..sep]).is_none() {
            sep = find_from(sep + 1);
        }
    }

    let scope = &path[..sep];
    let root: Rc<HighlighterGroup> = if scope == b"shared" {
        crate::highlighters::shared_highlighters()
    } else {
        get_scope(scope, context)?.highlighters().group().clone()
    };
    if sep != path.len() {
        let child = root.get_child(&path[sep + 1..])?;
        return Ok((root, child));
    }
    let as_hl: Rc<dyn Highlighter> = root.clone();
    Ok((root, as_hl))
}

fn redraw_relevant_clients(root: &Rc<HighlighterGroup>) {
    for client in ClientManager::instance().clients() {
        let context = client.context();
        let is = |g: &Rc<HighlighterGroup>| Rc::ptr_eq(g, root);
        if is(&crate::highlighters::shared_highlighters())
            || is(GlobalScope::instance().highlighters().group())
            || context.try_buffer().is_ok_and(|b| is(b.scope().highlighters().group()))
            || context.try_window().is_ok_and(|w| is(w.scope().highlighters().group()))
        {
            client.force_redraw(false);
        }
    }
}

fn add_highlighter_helper(_: &Context, params: &[Vec<u8>]) -> Vec<u8> {
    if params.len() > 1
        && let Some(entry) = HighlighterRegistry::instance().get(&params[1]) {
            let docstring = entry.description.docstring.as_bytes();
            let desc_params = generate_switches_doc(&entry.description.params.switches);
            if desc_params.is_empty() {
                return kformat!("{}:\n{}", params[1], indent(docstring, b"    "));
            }
            let parts: Vec<Vec<u8>> = vec![docstring.to_vec(), b"Switches:".to_vec(), indent(&desc_params, b"    ")]
                .into_iter()
                .map(|s| indent(&s, b"    "))
                .collect();
            return kformat!("{}:\n{}", params[1], join_str(&parts, b"\n"));
        }
    Vec::new()
}

fn add_highlighter(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let registry = HighlighterRegistry::instance();
    let path = &parser[0];
    let type_ = &parser[1];
    let highlighter_params: Vec<Vec<u8>> = parser.iter().skip(2).map(|p| p.to_vec()).collect();

    let Some(entry) = registry.get(type_) else {
        return Err(rt_err!("no such highlighter type: '{}'", type_));
    };

    let Some(slash) = path.iter().rposition(|&c| c == b'/') else {
        return Err(Error::runtime("no parent in path"));
    };

    let auto_name = |params: &[Vec<u8>]| {
        join_str(params.iter().map(|s| replace(s, b"/", b"<slash>")), b"_")
    };

    let name = path[slash + 1..].to_vec();
    let (root, parent) = get_highlighter(context, &path[..slash])?;
    let hl = (entry.factory)(&highlighter_params, Some(parent.as_ref()))?;
    parent.add_child(
        if name.is_empty() { auto_name(parser.positionals_from(1)) } else { name },
        hl,
        parser.has_switch("override"),
    )?;

    redraw_relevant_clients(&root);
    Ok(())
}

fn remove_highlighter(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let mut path = &parser[0];
    if path.last() == Some(&b'/') {
        // ignore trailing /
        path = &path[..path.len() - 1];
    }

    let Some(sep) = path.iter().rposition(|&c| c == b'/') else { return Ok(()) };
    let (root, parent) = get_highlighter(context, &path[..sep + 1])?;
    parent.remove_child(&path[sep + 1..])?;
    redraw_relevant_clients(&root);
    Ok(())
}

// hooks --------------------------------------------------------------------

fn complete_hooks(_: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    Completions::with(
        0,
        cursor_pos,
        complete(prefix, cursor_pos, HOOK_DESC.iter().map(|(_, n)| n.as_bytes())),
        CompletionFlags::None,
    )
}

fn add_hook(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let Some((hook, _)) = HOOK_DESC.iter().find(|(_, n)| n.as_bytes() == &parser[1]) else {
        return Err(rt_err!("no such hook: '{}'", &parser[1]));
    };
    let regex = Regex::new(&parser[2], RegexCompileFlags::OPTIMIZE)?;
    let command = parser[3].to_vec();
    let group = parser.get_switch("group").unwrap_or_default();
    let is_word_dash = |c: u8| is_word(c as i8 as i32 as u32, &['-' as u32]);
    if group.iter().any(|&c| !is_word_dash(c))
        || (!group.is_empty() && !crate::unicode::is_word_default(group[0] as i8 as i32 as u32))
    {
        return Err(rt_err!("invalid group name '{}'", group));
    }

    let mut flags = HookFlags::None;
    if parser.has_switch("always") {
        flags |= HookFlags::Always;
    }
    if parser.has_switch("once") {
        flags |= HookFlags::Once;
    }
    get_scope(&parser[0], context)?.hooks().add_hook(*hook, group.to_vec(), flags, regex, command, context)
}

fn remove_hook_completer() -> CommandCompleter {
    Rc::new(|context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
        if token_to_complete == 0 {
            return menu(ps(complete_scope))(context, &params[0], pos_in_token);
        } else if token_to_complete == 1
            && let Ok(Some(scope)) = get_scope_ifp(&params[0], context) {
                return Completions::with(
                    0,
                    params[0].len(),
                    scope.hooks().complete_hook_group(&params[1], pos_in_token),
                    CompletionFlags::Menu,
                );
            }
        Completions::default()
    })
}

fn params_to_shell(parser: &ParametersParser) -> Vec<Vec<u8>> {
    parser.positionals()
}

// define-command -----------------------------------------------------------

fn complete_completer_type(_: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    let completers: [&[u8]; 7] =
        [b"file", b"client", b"buffer", b"shell-script", b"shell-script-candidates", b"command", b"shell"];
    Completions::with(0, cursor_pos, complete(prefix, cursor_pos, completers), CompletionFlags::None)
}

fn make_command_completer(type_: &[u8], param: &[u8], completions_flags: CompletionFlags) -> Result<CommandCompleter> {
    match type_ {
        b"file" => Ok(Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
            let prefix = &params[token_to_complete];
            Completions::with(
                0,
                pos_in_token,
                complete_filename(prefix, &ignored_files(context), pos_in_token, FilenameFlags::Expand),
                completions_flags,
            )
        })),
        b"client" => Ok(Rc::new(move |_: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
            let prefix = &params[token_to_complete];
            Completions::with(
                0,
                pos_in_token,
                ClientManager::instance().complete_client_name(prefix, pos_in_token),
                completions_flags,
            )
        })),
        b"buffer" => Ok(Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
            add_flags(complete_buffer_name(false), completions_flags)(context, &params[token_to_complete], pos_in_token)
        })),
        b"shell-script" => {
            if param.is_empty() {
                return Err(Error::runtime("shell-script requires a shell script parameter"));
            }
            Ok(shell_script_completer(param.to_vec(), completions_flags))
        }
        b"shell-script-candidates" => {
            if param.is_empty() {
                return Err(Error::runtime("shell-script-candidates requires a shell script parameter"));
            }
            Ok(shell_candidates_completer(param.to_vec(), completions_flags))
        }
        b"command" => {
            let nested = Rc::new(NestedCompleter::default());
            Ok(Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
                nested.complete(context, params, token_to_complete, pos_in_token)
            }))
        }
        b"shell" => Ok(Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
            add_flags(ps(shell_complete), completions_flags)(context, &params[token_to_complete], pos_in_token)
        })),
        _ => Err(rt_err!("invalid command completion type '{}'", type_)),
    }
}

fn parse_completion_switch(parser: &ParametersParser, completions_flags: CompletionFlags) -> Result<Option<CommandCompleter>> {
    for completion_switch in [
        "file-completion",
        "client-completion",
        "buffer-completion",
        "shell-script-completion",
        "shell-script-candidates",
        "command-completion",
        "shell-completion",
    ] {
        if let Some(param) = parser.get_switch(completion_switch) {
            let type_ = completion_switch.strip_suffix("-completion").unwrap_or(completion_switch);
            return Ok(Some(make_command_completer(type_.as_bytes(), param, completions_flags)?));
        }
    }
    Ok(None)
}

fn define_command(parser: &ParametersParser, _context: &Context, _: &ShellContext) -> Result<()> {
    let cmd_name = &parser[0];
    let cm = CommandManager::instance();

    if !cmd_name.iter().all(|&c| is_identifier(c as u32)) {
        return Err(rt_err!("invalid command name: '{}'", cmd_name));
    }

    if cm.command_defined(cmd_name) && !parser.has_switch("override") {
        return Err(rt_err!("command '{}' already defined", cmd_name));
    }

    let flags = if parser.has_switch("hidden") { CommandFlags::Hidden } else { CommandFlags::None };

    let menu = parser.has_switch("menu");
    let completions_flags = if menu { CompletionFlags::Menu } else { CompletionFlags::None };

    let commands = Rc::new(parser[1].to_vec());
    let cmd: CommandFunc;
    let desc;
    if let Some(params) = parser.get_switch("params") {
        let mut min = 0usize;
        let mut max = usize::MAX;
        let re = Regex::new(br"(\d+)?..(\d+)?", RegexCompileFlags::NONE)?;
        if let Some(res) = regex_match_ranges(params, &re) {
            if let Some((b, e)) = res[1] {
                min = str_to_int(&params[b..e])? as usize;
            }
            if let Some((b, e)) = res[2] {
                max = str_to_int(&params[b..e])? as usize;
            }
        } else {
            min = str_to_int(params)? as usize;
            max = min;
        }
        desc = ParameterDesc::new(Default::default(), ParameterFlags::SwitchesAsPositional, min, max);
        cmd = Rc::new(move |parser: &ParametersParser, context: &Context, sc: &ShellContext| {
            let _local_scope = LocalScope::new(context);
            CommandManager::instance().execute(
                &commands,
                context,
                &ShellContext { params: params_to_shell(parser), env_vars: sc.env_vars.clone() },
            )
        });
    } else {
        desc = ParameterDesc::new(Default::default(), ParameterFlags::SwitchesAsPositional, 0, 0);
        cmd = Rc::new(move |_: &ParametersParser, context: &Context, sc: &ShellContext| {
            let _local_scope = LocalScope::new(context);
            CommandManager::instance().execute(
                &commands,
                context,
                &ShellContext { params: Vec::new(), env_vars: sc.env_vars.clone() },
            )
        });
    }

    let completer = parse_completion_switch(parser, completions_flags)?;
    if menu && completer.is_none() {
        return Err(Error::runtime("menu switch requires a completion switch"));
    }
    let docstring = trim_indent(parser.get_switch("docstring").unwrap_or_default())?;

    cm.register_command(cmd_name.to_vec(), cmd, docstring, desc, flags, None, completer);
    Ok(())
}

fn define_command_params() -> ParameterDesc {
    ParameterDesc::new(
        switches(vec![
            (
                "params",
                arg("take parameters, accessible to each shell escape as $0..$N\nparameter should take the form <count> or <min>..<max> (both omittable)"),
            ),
            ("override", flag("allow overriding an existing command")),
            ("hidden", flag("do not display the command in completion candidates")),
            ("docstring", arg("define the documentation string for command")),
            ("menu", flag("treat completions as the only valid inputs")),
            ("file-completion", flag("complete parameters using filename completion")),
            ("client-completion", flag("complete parameters using client name completion")),
            ("buffer-completion", flag("complete parameters using buffer name completion")),
            ("command-completion", flag("complete parameters using kakoune command completion")),
            ("shell-completion", flag("complete parameters using shell command completion")),
            ("shell-script-completion", arg("complete parameters using the given shell-script")),
            ("shell-script-candidates", arg("get the parameter candidates using the given shell-script")),
        ]),
        ParameterFlags::None,
        2,
        2,
    )
}

fn complete_alias_name(context: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    let names: Vec<Vec<u8>> = context.aliases().flatten_aliases().into_iter().map(|(k, _)| k).collect();
    Completions::with(0, cursor_pos, complete(prefix, cursor_pos, &names), CompletionFlags::None)
}

// echo ---------------------------------------------------------------------

fn echo(parser: &ParametersParser, context: &Context, shell_context: &ShellContext) -> Result<()> {
    let mut message = if let Some(quoting) = parser.get_switch("quoting") {
        let q = quoter(Quoting::elem_from_string(quoting)?);
        join(parser.iter().map(q), b' ', false)
    } else {
        join(parser.iter(), b' ', false)
    };

    if parser.has_switch("end-of-line") {
        message.push(b'\n');
    }

    if let Some(filename) = parser.get_switch("to-file") {
        let faces = context.faces();
        let name = filename.to_vec();
        let busy_indicator = BusyIndicator::new(
            context,
            Box::new(move |elapsed: Duration| {
                DisplayLine::from_str_face(
                    kformat!("waiting while writing to '{}' ({}s)", name, elapsed.as_secs()),
                    faces.face("Information"),
                )
            }),
            Instant::now(),
        );
        let res = write_to_file(filename, &message);
        if res.is_err() {
            busy_indicator.skip_restore();
        }
        res
    } else if let Some(command) = parser.get_switch("to-shell-script") {
        ShellManager::instance().eval(command, context, &message, ShellFlags::None, shell_context)?;
        Ok(())
    } else if parser.has_switch("debug") {
        write_to_debug_buffer(&message);
        Ok(())
    } else if parser.has_switch("markup") {
        context.print_status(parse_display_line(&message, &context.faces(), &DisplayLineBuiltins::new())?);
        Ok(())
    } else {
        context.print_status(DisplayLine::from_str_face(message, context.faces().face("StatusLine")));
        Ok(())
    }
}

pub fn parse_keymap_mode(s: &[u8], user_modes: &[Vec<u8>]) -> Result<KeymapMode> {
    let modes: [(&[u8], KeymapMode); 9] = [
        (b"normal", KeymapMode::Normal),
        (b"insert", KeymapMode::Insert),
        (b"menu", KeymapMode::Menu),
        (b"prompt", KeymapMode::Prompt),
        (b"goto", KeymapMode::Goto),
        (b"view", KeymapMode::View),
        (b"user", KeymapMode::User),
        (b"object", KeymapMode::Object),
        (b"combine", KeymapMode::Combine),
    ];
    for (name, mode) in modes {
        if prefix_match(name, s) {
            return Ok(mode);
        }
    }
    match user_modes.iter().position(|m| m == s) {
        Some(pos) => Ok(KeymapMode(KeymapMode::FirstUserMode.0 + pos as u8)),
        None => Err(rt_err!("no such keymap mode: '{}'", s)),
    }
}

const MODES: [&str; 9] = ["normal", "insert", "menu", "prompt", "goto", "view", "user", "object", "combine"];

fn debug_cmd(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    match &parser[0] {
        b"info" => {
            write_to_debug_buffer(&kformat!("version: {}", crate::kak_main::VERSION));
            write_to_debug_buffer(&kformat!("pid: {}", unsafe { libc::getpid() }));
            write_to_debug_buffer(&kformat!("session: {}", Server::instance().session()));
            if cfg!(debug_assertions) {
                write_to_debug_buffer(b"build: debug");
            } else {
                write_to_debug_buffer(b"build: release");
            }
        }
        b"buffers" => {
            write_to_debug_buffer(b"Buffers:");
            for buffer in BufferManager::instance().buffers() {
                write_to_debug_buffer(&buffer.debug_description());
            }
        }
        b"options" => {
            write_to_debug_buffer(b"Options:");
            for option in context.options().flatten_options() {
                write_to_debug_buffer(&kformat!(
                    " * {} \"{}\"{}: {}",
                    option.name(),
                    option.docstring(),
                    if option.flags().has(OptionFlags::Hidden) { " (hidden)" } else { "" },
                    option.get_as_string(Quoting::Kakoune)
                ));
            }
        }
        b"memory" => {
            write_to_debug_buffer(b"Memory usage:");
            write_to_debug_buffer(b"  memory domains are not tracked in this build");
        }
        b"shared-strings" | b"profile-hash-maps" => {}
        b"faces" => {
            write_to_debug_buffer(b"Faces:");
            for (name, spec) in context.faces().flatten_faces() {
                write_to_debug_buffer(&kformat!(" * {}: {}", name, spec.face));
            }
        }
        b"mappings" => {
            let keymaps = context.keymaps();
            let user_modes = keymaps.user_modes();
            write_to_debug_buffer(b"Mappings:");
            let all_modes: Vec<Vec<u8>> =
                MODES.iter().map(|m| m.as_bytes().to_vec()).chain(user_modes.iter().cloned()).collect();
            for mode in &all_modes {
                let m = parse_keymap_mode(mode, &user_modes)?;
                for key in keymaps.get_mapped_keys(m) {
                    let Some(mapping) = keymaps.get_mapping(key, m) else { continue };
                    let mut keys = Vec::new();
                    for k in &mapping.keys {
                        keys.extend(key_to_str(*k));
                    }
                    write_to_debug_buffer(&kformat!(
                        " * {} {}: '{}' {} {}",
                        mode,
                        key,
                        keys,
                        if mapping.atomic { "(atomic)" } else { "" },
                        mapping.docstring
                    ));
                }
            }
        }
        b"regex" => {
            if parser.positional_count() != 2 {
                return Err(Error::runtime("expected a regex"));
            }
            write_to_debug_buffer(&kformat!(
                " * {}:\n{}",
                &parser[1],
                dump_regex(&compile_regex(&parser[1], RegexCompileFlags::OPTIMIZE)?)
            ));
        }
        b"registers" => {
            write_to_debug_buffer(b"Register info:");
            for (name, reg) in RegisterManager::instance().registers() {
                let content = reg.get(context)?;
                if content.len() == 1 && content[0].is_empty() {
                    continue;
                }
                write_to_debug_buffer(&kformat!(
                    " * {} = {}\n",
                    crate::format::Cp(name),
                    join_str(content.iter().map(|c| quote(c)), b"\n     = ")
                ));
            }
        }
        other => return Err(rt_err!("no such debug command: '{}'", other)),
    }
    Ok(())
}

fn source(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let name = parser[0].to_vec();
    let _profile = ProfileScope::new(
        context.options().val::<DebugFlags>("debug"),
        |duration| {
            write_to_debug_buffer(&kformat!("sourcing '{}' took {} us", name, duration.as_micros() as usize))
        },
        true,
    );

    let path = real_path(&parse_filename(&parser[0], b""));
    let file_content = MappedFile::new(&path)?;
    let params: Vec<Vec<u8>> = parser.iter().skip(1).map(|p| p.to_vec()).collect();
    let mut env_vars = EnvVarMap::new();
    env_vars.insert(b"source".to_vec(), path);
    match CommandManager::instance().execute(&file_content.data, context, &ShellContext { params, env_vars }) {
        Ok(()) => Ok(()),
        Err(err) if err.is_runtime() => {
            write_to_debug_buffer(&kformat!("{}:{}", &parser[0], err.what()));
            Err(err)
        }
        Err(err) => Err(err),
    }
}

fn option_doc_helper(_: &Context, params: &[Vec<u8>]) -> Vec<u8> {
    let is_switch = params.len() > 1 && (params[0] == b"-add" || params[0] == b"-remove");
    if params.len() < 2 + is_switch as usize {
        return Vec::new();
    }
    let Some(desc) = GlobalScope::instance().option_registry().option_desc(&params[1 + is_switch as usize]) else {
        return Vec::new();
    };
    if desc.docstring.is_empty() {
        return Vec::new();
    }
    kformat!("{}:\n{}", desc.name, indent(&desc.docstring, b"    "))
}

fn get_options(scope: &[u8], context: &Context, option_name: &[u8]) -> Result<Rc<OptionManager>> {
    if scope == b"current" {
        return context.options().get(option_name)?.manager().ok_or_else(|| Error::runtime("option manager is gone"));
    }
    Ok(get_scope(scope, context)?.options().clone())
}

fn set_option_completer() -> CommandCompleter {
    Rc::new(|context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
        if token_to_complete == 0 {
            return menu(ps(complete_scope_including_current))(context, &params[0], pos_in_token);
        } else if token_to_complete == 1 {
            return Completions::with(
                0,
                params[1].len(),
                GlobalScope::instance().option_registry().complete_option_name(&params[1], pos_in_token),
                CompletionFlags::Menu,
            );
        } else if token_to_complete == 2
            && params[2].is_empty()
            && GlobalScope::instance().option_registry().option_exists(&params[1])
            && let Ok(scope) = get_scope(&params[0], context)
                && let Ok(opt) = scope.options().get(&params[1]) {
                    return Completions::with(
                        0,
                        params[2].len(),
                        vec![opt.get_as_string(Quoting::Kakoune)],
                        CompletionFlags::Quoted,
                    );
                }
        Completions::default()
    })
}

fn set_option(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let add = parser.has_switch("add");
    let remove = parser.has_switch("remove");
    if add && remove {
        return Err(Error::runtime("cannot add and remove at the same time"));
    }

    let opt = get_options(&parser[0], context, &parser[1])?.get_local_option(&parser[1])?;
    let values = parser.positionals_from(2);
    if add {
        opt.add_from_strings(values)
    } else if remove {
        opt.remove_from_strings(values)
    } else {
        opt.set_from_strings(values)
    }
}

fn complete_option() -> CommandCompleter {
    Rc::new(|context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
        if token_to_complete == 0 {
            return menu(ps(complete_scope_no_global))(context, &params[0], pos_in_token);
        } else if token_to_complete == 1 {
            return Completions::with(
                0,
                params[1].len(),
                GlobalScope::instance().option_registry().complete_option_name(&params[1], pos_in_token),
                CompletionFlags::Menu,
            );
        }
        Completions::default()
    })
}

fn unset_option(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let options = get_options(&parser[0], context, &parser[1])?;
    if Rc::ptr_eq(&options, GlobalScope::instance().options()) {
        return Err(Error::runtime("cannot unset options in global scope"));
    }
    options.unset_option(&parser[1])
}

fn update_option(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let opt = get_options(&parser[0], context, &parser[1])?.get_local_option(&parser[1])?;
    opt.update(context)
}

fn declare_option(parser: &ParametersParser, _context: &Context, _: &ShellContext) -> Result<()> {
    let flags = if parser.has_switch("hidden") { OptionFlags::Hidden } else { OptionFlags::None };
    let docstring = trim_indent(parser.get_switch("docstring").unwrap_or_default())?;
    let reg = GlobalScope::instance().option_registry();
    let name = &parser[1];

    use crate::highlighters::{LineAndSpecList, RangeAndStringList};
    let opt = match &parser[0] {
        b"int" => reg.declare_option(name, &docstring, 0i32, flags)?,
        b"bool" => reg.declare_option(name, &docstring, false, flags)?,
        b"str" => reg.declare_option(name, &docstring, Vec::<u8>::new(), flags)?,
        b"regex" => reg.declare_option(name, &docstring, Regex::default(), flags)?,
        b"int-list" => reg.declare_option(name, &docstring, Vec::<i32>::new(), flags)?,
        b"str-list" => reg.declare_option(name, &docstring, Vec::<Vec<u8>>::new(), flags)?,
        b"completions" => reg.declare_option(name, &docstring, CompletionList::default(), flags)?,
        b"line-specs" => reg.declare_option(name, &docstring, LineAndSpecList::default(), flags)?,
        b"range-specs" => reg.declare_option(name, &docstring, RangeAndStringList::default(), flags)?,
        b"str-to-str-map" => reg.declare_option(name, &docstring, StringMap::new(), flags)?,
        other => return Err(rt_err!("no such option type: '{}'", other)),
    };

    if parser.positional_count() > 2 {
        opt.set_from_strings(parser.positionals_from(2))?;
    }
    Ok(())
}

fn map_key_completer(unmap: bool) -> CommandCompleter {
    Rc::new(move |context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
        if token_to_complete == 0 {
            return menu(ps(complete_scope))(context, &params[0], pos_in_token);
        }
        if token_to_complete == 1 {
            let Ok(scope) = get_scope(&params[0], context) else { return Completions::default() };
            let user_modes = scope.keymaps().user_modes();
            let all: Vec<Vec<u8>> =
                MODES.iter().map(|m| m.as_bytes().to_vec()).chain(user_modes.iter().cloned()).collect();
            return Completions::with(0, params[1].len(), complete(&params[1], pos_in_token, &all), CompletionFlags::Menu);
        }
        if unmap && token_to_complete == 2 {
            let Ok(scope) = get_scope(&params[0], context) else { return Completions::default() };
            let keymaps = scope.keymaps();
            let Ok(keymap_mode) = parse_keymap_mode(&params[1], &keymaps.user_modes()) else {
                return Completions::default();
            };
            let keys: Vec<Vec<u8>> = keymaps.get_mapped_keys(keymap_mode).iter().map(|k| key_to_str(*k)).collect();
            return Completions::with(0, params[2].len(), complete(&params[2], pos_in_token, &keys), CompletionFlags::Menu);
        }
        Completions::default()
    })
}

fn map_key(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let scope = get_scope(&parser[0], context)?;
    let keymaps = scope.keymaps();
    let keymap_mode = parse_keymap_mode(&parser[1], &keymaps.user_modes())?;

    let key = parse_keys(&parser[2])?;
    if key.len() != 1 {
        return Err(Error::runtime("only a single key can be mapped"));
    }

    let mapping = parse_keys(&parser[3])?;
    keymaps.map_key(
        key[0],
        keymap_mode,
        mapping,
        trim_indent(parser.get_switch("docstring").unwrap_or_default())?,
        parser.has_switch("atomic"),
    );
    Ok(())
}

fn unmap_key(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let scope = get_scope(&parser[0], context)?;
    let keymaps = scope.keymaps();
    let keymap_mode = parse_keymap_mode(&parser[1], &keymaps.user_modes())?;

    if parser.positional_count() == 2 {
        keymaps.unmap_keys(keymap_mode);
        return Ok(());
    }

    let key = parse_keys(&parser[2])?;
    if key.len() != 1 {
        return Err(Error::runtime("only a single key can be unmapped"));
    }

    if let Some(mapping) = keymaps.get_mapping(key[0], keymap_mode)
        && (parser.positional_count() < 4 || mapping.keys == parse_keys(&parser[3])?) {
            keymaps.unmap_key(key[0], keymap_mode);
        }
    Ok(())
}

// context_wrap -------------------------------------------------------------

fn make_context_wrap_params(additional: Vec<(&str, SwitchDesc)>) -> ParameterDesc {
    let mut items = vec![
        ("client", arg_with(client_arg_completer(), "run in the client context for each client in the given comma separated list")),
        ("try-client", arg_with(client_arg_completer(), "run in given client context if it exists, or else in the current one")),
        (
            "buffer",
            arg_with(complete_buffer_name(false), "run in a disposable context for each given buffer in the comma separated list argument"),
        ),
        ("draft", flag("run in a disposable context")),
        ("itersel", flag("run once for each selection with that selection as the only one")),
    ];
    items.extend(additional);
    ParameterDesc::new(switches(items), ParameterFlags::SwitchesOnlyAtStart, 1, usize::MAX)
}

/// Restores a register when dropped
struct RegisterRestorer<'a> {
    context: &'a Context,
    name: Codepoint,
    reg: Rc<dyn Register>,
    save: Vec<Vec<u8>>,
}

impl Drop for RegisterRestorer<'_> {
    fn drop(&mut self) {
        if let Err(err) = self.reg.restore(self.context, &self.save) {
            write_to_debug_buffer(&kformat!(
                "failed to restore register '{}': {}",
                crate::format::Cp(self.name),
                err.what()
            ));
        }
        self.reg.modified_hook_disabled().unset();
    }
}

type WrappedFunc<'a> = &'a dyn Fn(&ParametersParser, &Context) -> Result<()>;

fn context_wrap(parser: &ParametersParser, context: &Context, default_saved_regs: &[u8], func: WrappedFunc) -> Result<()> {
    if parser.has_switch("buffer") as i32 + parser.has_switch("client") as i32 + parser.has_switch("try-client") as i32 > 1 {
        return Err(Error::runtime("only one of -buffer, -client or -try-client can be specified"));
    }

    let register_manager = RegisterManager::instance();
    let mut saved_registers: Vec<RegisterRestorer> = Vec::new();
    for &c in parser.get_switch("save-regs").unwrap_or(default_saved_regs) {
        let cp = c as i8 as i32 as u32;
        let reg = register_manager.get_cp(cp)?;
        let save = reg.save(context)?;
        reg.modified_hook_disabled().set();
        saved_registers.push(RegisterRestorer { context, name: cp, reg, save });
    }

    if let Some(bufnames) = parser.get_switch("buffer") {
        let context_wrap_for_buffer = |buffer: &Rc<Buffer>| -> Result<()> {
            let input_handler = InputHandler::new(
                SelectionList::new(buffer.clone(), Selection::default()),
                ContextFlags::Draft,
                Vec::new(),
            );
            func(parser, input_handler.context())
        };

        if bufnames == b"*" {
            // gather as we might be mutating the buffer list in the loop.
            let buffers: Vec<Rc<Buffer>> = BufferManager::instance()
                .buffers()
                .into_iter()
                .filter(|b| !b.flags().has(BufferFlags::Debug))
                .collect();
            for buffer in &buffers {
                context_wrap_for_buffer(buffer)?;
            }
        } else {
            for name in split_escaped(bufnames, b',', Some(b'\\')) {
                let name = unescape(name, b",\\", b'\\');
                context_wrap_for_buffer(&BufferManager::instance().get_buffer(&name)?)?;
            }
        }
        return Ok(());
    }

    let context_wrap_for_context = |base_context: &Context| -> Result<()> {
        let draft = parser.has_switch("draft");
        let input_handler = if draft {
            let ih = InputHandler::new(base_context.try_selections()?.clone(), ContextFlags::Draft, base_context.name());
            // Preserve window so that window scope is available
            if let Ok(window) = base_context.try_window() {
                ih.context().set_window(&window);
            }
            // We do not want this draft context to commit undo groups if the real one is
            // going to commit the whole thing later
            if base_context.is_editing() {
                ih.context().disable_undo_handling();
            }
            Some(ih)
        } else {
            None
        };

        let c: &Context = match &input_handler {
            Some(ih) => ih.context(),
            None => base_context,
        };

        let _edition = ScopedEdition::new(c);
        let _selection_edition = ScopedSelectionEdition::new(c);

        if parser.has_switch("itersel") {
            let sels = base_context.try_selections()?.clone();
            let mut new_sels: Vec<Selection> = Vec::new();
            let mut main = 0usize;
            let mut timestamp = c.buffer().timestamp();
            let mut one_selection_succeeded = false;
            for (index, sel) in sels.iter().enumerate() {
                *c.selections_write_only() =
                    SelectionList::with_timestamp(sels.buffer().clone(), sel.clone(), sels.timestamp());
                c.selections().update(true);

                match func(parser, c) {
                    Ok(()) => {
                        one_selection_succeeded = true;

                        if !Rc::ptr_eq(sels.buffer(), &c.buffer()) {
                            return Err(Error::runtime("buffer has changed while iterating on selections"));
                        }

                        if !draft {
                            update_selections(&mut new_sels, &mut main, &c.buffer(), timestamp, true);
                            timestamp = c.buffer().timestamp();
                            let current = c.selections();
                            if index == sels.main_index() {
                                main = new_sels.len() + current.main_index();
                            }
                            let middle = new_sels.len();
                            new_sels.extend(current.iter().cloned());
                            drop(current);
                            inplace_merge(&mut new_sels, middle);
                        }
                    }
                    Err(e) if e.kind == ErrorKind::NoSelectionsRemaining => {}
                    Err(e) => return Err(e),
                }
            }

            if !one_selection_succeeded {
                *c.selections_write_only() = sels;
                return Err(no_selections_remaining());
            }

            if !draft {
                c.selections_write_only().set(new_sels, main);
            }
        } else {
            let collapse_jumps = !c.flags().has(ContextFlags::Draft) && c.has_buffer();
            let prev_index = c.jump_list().current_index();
            let jump = if collapse_jumps { Some(c.selections().clone()) } else { None };

            func(parser, c)?;

            // If the jump list got mutated, collapse all jumps into a single one from original selections
            let index = c.jump_list().current_index();
            if let Some(jump) = jump {
                let buffer_alive = BufferManager::instance().buffers().iter().any(|b| Rc::ptr_eq(b, jump.buffer()));
                if collapse_jumps && index > prev_index && buffer_alive {
                    c.jump_list().push(jump, Some(prev_index));
                }
            }
        }
        Ok(())
    };

    let cm = ClientManager::instance();
    if let Some(client_names) = parser.get_switch("client") {
        if client_names == b"*" {
            // gather as we might be mutating the client list in the loop.
            for client in cm.clients() {
                context_wrap_for_context(client.context())?;
            }
        } else {
            for name in split_escaped(client_names, b',', Some(b'\\')) {
                let name = unescape(name, b",\\", b'\\');
                let client = cm.get_client(&name)?;
                context_wrap_for_context(client.context())?;
            }
        }
    } else if let Some(client_name) = parser.get_switch("try-client") {
        match cm.get_client_ifp(client_name) {
            Some(client) => context_wrap_for_context(client.context())?,
            None => context_wrap_for_context(context)?,
        }
    } else {
        context_wrap_for_context(context)?;
    }
    drop(saved_registers);
    Ok(())
}

fn inplace_merge(sels: &mut Vec<Selection>, middle: usize) {
    let tail = sels.split_off(middle);
    let head = std::mem::take(sels);
    let mut a = head.into_iter().peekable();
    let mut b = tail.into_iter().peekable();
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) => {
                if compare_selections(y, x) {
                    sels.push(b.next().unwrap());
                } else {
                    sels.push(a.next().unwrap());
                }
            }
            (Some(_), None) => sels.push(a.next().unwrap()),
            (None, Some(_)) => sels.push(b.next().unwrap()),
            (None, None) => break,
        }
    }
}

fn execute_keys(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    context_wrap(parser, context, b"/\"|^@:", &|parser, context| {
        let _disable_keymaps = ScopedSetBool::new(context.keymaps_disabled(), !parser.has_switch("with-maps"));
        let _disable_hooks = ScopedSetBool::new(context.hooks_disabled(), !parser.has_switch("with-hooks"));

        for param in parser.iter() {
            for key in parse_keys(param)? {
                context.try_input_handler()?.handle_key(key, true)?;
            }
        }
        Ok(())
    })
}

fn evaluate_commands(parser: &ParametersParser, context: &Context, shell_context: &ShellContext) -> Result<()> {
    context_wrap(parser, context, b"", &|parser, context| {
        let no_hooks = context.hooks_disabled().is_set() || parser.has_switch("no-hooks");
        let _disable_hooks = ScopedSetBool::new(context.hooks_disabled(), no_hooks);
        let _local_scope = LocalScope::new(context);

        if parser.has_switch("verbatim") {
            CommandManager::instance().execute_single_command(&parser.positionals(), context, shell_context)
        } else {
            CommandManager::instance().execute(&join(parser.iter(), b' ', false), context, shell_context)
        }
    })
}

fn prompt_cmd(parser: &ParametersParser, context: &Context, shell_context: &ShellContext) -> Result<()> {
    let command = parser[1].to_vec();
    let initstr = parser.get_switch("init").unwrap_or_default().to_vec();

    let completions_flags = if parser.has_switch("menu") { CompletionFlags::Menu } else { CompletionFlags::None };
    let completer = prompt_completer_adapter(parse_completion_switch(parser, completions_flags)?);
    let flags = if parser.has_switch("password") { PromptFlags::Password } else { PromptFlags::None };

    let on_change = parser.get_switch("on-change").unwrap_or_default().to_vec();
    let on_abort = parser.get_switch("on-abort").unwrap_or_default().to_vec();
    let sc = shell_context.clone();

    context.try_input_handler()?.prompt(
        &parser[0],
        initstr,
        Vec::new(),
        context.faces().face("Prompt"),
        flags,
        b'_',
        completer,
        Rc::new(move |s: &[u8], event, context: &Context| {
            use crate::input_handler::PromptEvent;
            if (event == PromptEvent::Abort && on_abort.is_empty()) || (event == PromptEvent::Change && on_change.is_empty()) {
                return Ok(());
            }

            let mut sc = sc.clone();
            sc.env_vars.insert(b"text".to_vec(), s.to_vec());

            let cmd: &[u8] = match event {
                PromptEvent::Validate => &command,
                PromptEvent::Change => &on_change,
                PromptEvent::Abort => &on_abort,
            };
            match CommandManager::instance().execute(cmd, context, &sc) {
                Ok(()) => Ok(()),
                Err(error) if error.is_runtime() => {
                    context.print_status(DisplayLine::from_str_face(error.what().to_vec(), context.faces().face("Error")));
                    context.hooks().run_hook(Hook::RuntimeError, error.what(), context)
                }
                Err(error) => Err(error),
            }
        }),
    )
}

fn on_key(parser: &ParametersParser, context: &Context, shell_context: &ShellContext) -> Result<()> {
    let command = parser[0].to_vec();
    let sc = shell_context.clone();
    context.try_input_handler()?.on_next_key(
        parser.get_switch("mode-name").unwrap_or(b"on-key"),
        KeymapMode::None,
        Rc::new(move |key: Key, context: &Context| {
            let mut sc = sc.clone();
            sc.env_vars.insert(b"key".to_vec(), key_to_str(key));
            CommandManager::instance().execute(&command, context, &sc)
        }),
        None,
    )
}

fn info(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let Ok(client) = context.try_client() else { return Ok(()) };

    let style = match parser.get_switch("style") {
        Some(b"above") => InfoStyle::InlineAbove,
        Some(b"below") => InfoStyle::InlineBelow,
        Some(b"menu") => InfoStyle::MenuDoc,
        Some(b"modal") => InfoStyle::Modal,
        Some(style) => return Err(rt_err!("invalid style: '{}'", style)),
        None => {
            if parser.has_switch("anchor") {
                InfoStyle::Inline
            } else {
                InfoStyle::Prompt
            }
        }
    };

    client.info_hide(style == InfoStyle::Modal);
    if parser.positional_count() == 0 {
        return Ok(());
    }

    let pos = match parser.get_switch("anchor") {
        Some(anchor) => {
            let Some(dot) = anchor.iter().position(|&c| c == b'.') else {
                return Err(Error::runtime("expected <line>.<column> for anchor"));
            };
            BufferCoord::new(str_to_int(&anchor[..dot])? - 1, str_to_int(&anchor[dot + 1..])? - 1)
        }
        None => BufferCoord::default(),
    };

    let title = parser.get_switch("title").unwrap_or_default();
    if parser.has_switch("markup") {
        let faces = context.faces();
        client.info_show(
            parse_display_line(title, &faces, &DisplayLineBuiltins::new())?,
            parse_display_line_list(&parser[0], &faces, &DisplayLineBuiltins::new())?,
            pos,
            style,
        );
    } else {
        client.info_show_str(title, &parser[0], pos, style);
    }
    Ok(())
}

fn try_catch(parser: &ParametersParser, context: &Context, shell_context: &ShellContext) -> Result<()> {
    let count = parser.positional_count();
    if count % 2 != 1 {
        return Err(wrong_argument_count());
    }

    for i in (1..count).step_by(2) {
        if &parser[i] != b"catch" {
            return Err(Error::runtime("usage: try <commands> [catch <on error commands>]..."));
        }
    }

    let command_manager = CommandManager::instance();
    let mut shell_context_with_error: Option<ShellContext> = None;
    let mut i = 0;
    while i < count {
        let sc = shell_context_with_error.as_ref().unwrap_or(shell_context);
        if i == 0 || i < count - 1 {
            match command_manager.execute(&parser[i], context, sc) {
                Ok(()) => return Ok(()),
                Err(error) if error.is_runtime() => {
                    let mut with_error = shell_context.clone();
                    with_error.env_vars.insert(b"error".to_vec(), error.what().to_vec());
                    shell_context_with_error = Some(with_error);
                }
                Err(error) => return Err(error),
            }
        } else {
            command_manager.execute(&parser[i], context, sc)?;
        }
        i += 2;
    }
    Ok(())
}

fn complete_face(context: &Context, prefix: &[u8], cursor_pos: usize) -> Completions {
    let names: Vec<Vec<u8>> = context.faces().flatten_faces().into_iter().map(|(k, _)| k).collect();
    Completions::with(0, cursor_pos, complete(prefix, cursor_pos, &names), CompletionFlags::None)
}

fn face_doc_helper(context: &Context, params: &[Vec<u8>]) -> Vec<u8> {
    if params.len() < 2 {
        return Vec::new();
    }
    match context.faces().get(&params[1]) {
        Ok(face) => kformat!("{}:\n{}", params[1], indent(&crate::face::face_to_string(&face), b"    ")),
        Err(_) => Vec::new(),
    }
}

fn set_face(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    get_scope(&parser[0], context)?.faces().add_face(&parser[1], &parser[2], true)?;
    for client in ClientManager::instance().clients() {
        client.force_redraw(false);
    }
    Ok(())
}

fn rename_client(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let name = &parser[0];
    if !name.iter().all(|&c| is_identifier(c as u32)) {
        return Err(rt_err!("invalid client name: '{}'", name));
    }
    if ClientManager::instance().client_name_exists(name) && context.name() != name {
        return Err(rt_err!("client name '{}' is not unique", name));
    }
    context.set_name(name.to_vec())
}

fn select(parser: &ParametersParser, context: &Context, _: &ShellContext) -> Result<()> {
    let buffer = context.try_buffer()?;
    let timestamp = parser
        .get_switch("timestamp")
        .and_then(str_to_int_ifp)
        .map(|t| t as usize)
        .unwrap_or_else(|| buffer.timestamp());
    let column_type = if parser.has_switch("codepoint") {
        ColumnType::Codepoint
    } else if parser.has_switch("display-column") {
        ColumnType::DisplayColumn
    } else {
        ColumnType::Byte
    };
    let tabstop = context.options().val::<i32>("tabstop");
    let _selection_edition = ScopedSelectionEdition::new(context);
    let sels = selection_list_from_strings(&buffer, column_type, parser.positionals_from(0), timestamp, 0, tabstop)?;
    *context.selections_write_only() = sels;
    Ok(())
}

fn change_directory(parser: &ParametersParser, ctx: &Context, _: &ShellContext) -> Result<()> {
    let target: &[u8] = if parser.positional_count() == 1 { &parser[0] } else { b"~" };
    let path = real_path(&parse_filename(target, b""));
    if unsafe { libc::chdir(crate::file::cstr(&path).as_ptr()) } != 0 {
        return Err(rt_err!("unable to change to directory: '{}'", target));
    }
    for buffer in BufferManager::instance().buffers() {
        buffer.update_display_name()?;
    }
    ctx.hooks().run_hook(Hook::EnterDirectory, &path, ctx)
}

fn rename_session(parser: &ParametersParser, ctx: &Context, _: &ShellContext) -> Result<()> {
    let old_name = Server::instance().session();
    if !Server::instance().rename_session(&parser[0])? {
        return Err(rt_err!("unable to rename current session: '{}' may be already in use", &parser[0]));
    }
    ctx.hooks().run_hook(Hook::SessionRenamed, &kformat!("{}:{}", old_name, Server::instance().session()), ctx)
}

/// We need ownership of the mode_name in the lock case
fn enter_user_mode(context: &Context, params: NormalParams, mode_name: Vec<u8>, mode: KeymapMode, lock: bool) -> Result<()> {
    let title = if lock { kformat!("{} (lock)", mode_name) } else { mode_name.clone() };
    let info = build_autoinfo_for_mapping(context, mode, &[]);
    on_next_key_with_autoinfo(
        context,
        &kformat!("user.{}", mode_name),
        KeymapMode::None,
        Rc::new(move |key: Key, context: &Context| {
            if key == Key::new(named::ESCAPE) {
                return Ok(());
            }
            if let Some(mapping) = context.keymaps().get_mapping(key, mode) {
                let _disable_keymaps = ScopedSetBool::set(context.keymaps_disabled());
                let ih = context.try_input_handler()?;
                let _force_normal = ih.force_normal(params)?;
                let _edition = ScopedEdition::new(context);
                // copy to allow reentrant unmap
                for key in mapping.keys.clone() {
                    ih.handle_key(key, true)?;
                }
            }
            if lock {
                enter_user_mode(context, params, mode_name.clone(), mode, true)?;
            }
            Ok(())
        }),
        title,
        info,
    )
}

pub fn register_commands() {
    let cm = CommandManager::instance();
    cm.register_command(
        b"nop".to_vec(),
        Rc::new(|_, _, _| Ok(())),
        b"do nothing".to_vec(),
        ParameterDesc::new(Default::default(), ParameterFlags::IgnoreUnknownSwitches, 0, usize::MAX),
        CommandFlags::None,
        None,
        None,
    );

    let commands = command_descs();
    for c in commands {
        let func = c.func;
        cm.register_command(
            c.name.as_bytes().to_vec(),
            Rc::new(func),
            c.docstring.as_bytes().to_vec(),
            c.params,
            c.flags,
            c.helper,
            c.completer,
        );
        if let Some(alias) = c.alias {
            GlobalScope::instance().aliases().add_alias(alias.as_bytes().to_vec(), c.name.as_bytes().to_vec());
        }
    }
}

fn helper(f: fn(&Context, &[Vec<u8>]) -> Vec<u8>) -> Option<CommandHelper> {
    Some(Rc::new(f))
}

fn command_descs() -> Vec<CommandDesc> {
    let nested_completer = || -> CommandCompleter {
        let nested = Rc::new(NestedCompleter::default());
        Rc::new(move |context: &Context, params: &[Vec<u8>], token: usize, pos: usize| {
            nested.complete(context, params, token, pos)
        })
    };
    let command_line_completer = || -> PromptStyleCompleter {
        let completer = Rc::new(Completer::default());
        Rc::new(move |context: &Context, prefix: &[u8], cursor_pos: usize| completer.complete(context, prefix, cursor_pos))
    };
    let mut cmds = vec![
        cmd(
            "edit",
            Some("e"),
            "edit [<switches>] <filename> [<line> [<column>]]: open the given filename in a buffer",
            edit_params(),
            None,
            Some(filename_completer(false)),
            |p, c, _| edit_impl(p, c, false),
        ),
        cmd(
            "edit!",
            Some("e!"),
            "edit! [<switches>] <filename> [<line> [<column>]]: open the given filename in a buffer, force reload if needed",
            edit_params(),
            None,
            Some(filename_completer(false)),
            |p, c, _| edit_impl(p, c, true),
        ),
        cmd(
            "write",
            Some("w"),
            "write [<switches>] [<filename>]: write the current buffer to its file or to <filename> if specified",
            write_params(),
            None,
            Some(filename_completer(false)),
            |p, c, _| write_buffer_impl(p, c, false),
        ),
        cmd(
            "write!",
            Some("w!"),
            "write! [<switches>] [<filename>]: write the current buffer to its file or to <filename> if specified, even when the file is write protected",
            write_params_except_force(),
            None,
            Some(filename_completer(false)),
            |p, c, _| write_buffer_impl(p, c, true),
        ),
        cmd(
            "write-all",
            Some("wa"),
            "write-all [<switches>]: write all changed buffers that are associated to a file",
            ParameterDesc::new(write_params_except_force().switches, ParameterFlags::None, 0, 0),
            None,
            None,
            |p, c, _| {
                let method = match p.get_switch("method") {
                    Some(m) => Some(parse_write_method(m)?),
                    None => None,
                };
                write_all_buffers(c, p.has_switch("sync"), method)
            },
        ),
        cmd(
            "write-all-quit",
            Some("waq"),
            "write-all-quit [<switches>] [<exit status>]: write all buffers associated to a file and quit current client. An optional integer parameter can set the client exit status.",
            write_params_except_force(),
            None,
            None,
            |p, c, _| {
                let method = match p.get_switch("method") {
                    Some(m) => Some(parse_write_method(m)?),
                    None => None,
                };
                write_all_buffers(c, p.has_switch("sync"), method)?;
                quit_impl(p, c, false)
            },
        ),
        cmd(
            "kill",
            None,
            "kill [<exit status>]: terminate the current session, the server and all clients connected. An optional integer parameter can set the server and client processes exit status",
            ParameterDesc::new(Default::default(), ParameterFlags::SwitchesAsPositional, 0, 1),
            None,
            None,
            |p, c, _| kill_impl(p, c, false),
        ),
        cmd(
            "kill!",
            None,
            "kill! [<exit status>]: force the termination of the current session, the server and all clients connected. An optional integer parameter can set the server and client processes exit status",
            ParameterDesc::new(Default::default(), ParameterFlags::SwitchesAsPositional, 0, 1),
            None,
            None,
            |p, c, _| kill_impl(p, c, true),
        ),
        cmd(
            "daemonize-session",
            None,
            "daemonize-session: set the session server not to quit on last client exit",
            ParameterDesc::default(),
            None,
            None,
            |_, _, _| {
                Server::instance().daemonize();
                Ok(())
            },
        ),
        cmd(
            "quit",
            Some("q"),
            "quit [<exit status>]: quit current client, and the kakoune session if the client is the last (if not running in daemon mode). An optional integer parameter can set the client exit status",
            ParameterDesc::new(Default::default(), ParameterFlags::SwitchesAsPositional, 0, 1),
            None,
            None,
            |p, c, _| quit_impl(p, c, false),
        ),
        cmd(
            "quit!",
            Some("q!"),
            "quit! [<exit status>]: quit current client, and the kakoune session if the client is the last (if not running in daemon mode). Force quit even if the client is the last and some buffers are not saved. An optional integer parameter can set the client exit status",
            ParameterDesc::new(Default::default(), ParameterFlags::SwitchesAsPositional, 0, 1),
            None,
            None,
            |p, c, _| quit_impl(p, c, true),
        ),
        cmd(
            "write-quit",
            Some("wq"),
            "write-quit [<switches>] [<exit status>]: write current buffer and quit current client. An optional integer parameter can set the client exit status",
            write_params_except_force(),
            None,
            None,
            |p, c, _| write_quit_impl(p, c, false),
        ),
        cmd(
            "write-quit!",
            Some("wq!"),
            "write-quit! [<switches>] [<exit status>] write: current buffer and quit current client, even if other buffers are not saved. An optional integer parameter can set the client exit status",
            write_params_except_force(),
            None,
            None,
            |p, c, _| write_quit_impl(p, c, true),
        ),
        cmd(
            "buffer",
            Some("b"),
            "buffer <name>: set buffer to edit in current client",
            ParameterDesc::new(switches(vec![("matching", flag("treat the argument as a regex"))]), ParameterFlags::None, 1, 1),
            None,
            Some(make_completer(vec![menu(complete_buffer_name(true))])),
            |parser, context, _| {
                let buffer = if parser.has_switch("matching") {
                    let re = Regex::new(&parser[0], RegexCompileFlags::NONE)?;
                    BufferManager::instance().get_buffer_matching(|buffer| regex_match_str(&buffer.name(), &re))?
                } else {
                    BufferManager::instance().get_buffer(&parser[0])?
                };

                if !Rc::ptr_eq(&buffer, &context.try_buffer()?) {
                    context.push_jump(false);
                    context.change_buffer(&buffer, None)?;
                }
                Ok(())
            },
        ),
        cmd(
            "buffer-next",
            Some("bn"),
            "buffer-next: move to the next buffer in the list",
            no_params(),
            None,
            None,
            |_, c, _| cycle_buffer(c, true),
        ),
        cmd(
            "buffer-previous",
            Some("bp"),
            "buffer-previous: move to the previous buffer in the list",
            no_params(),
            None,
            None,
            |_, c, _| cycle_buffer(c, false),
        ),
        cmd(
            "delete-buffer",
            Some("db"),
            "delete-buffer [name]: delete current buffer or the buffer named <name> if given",
            single_optional_param(),
            None,
            Some(make_completer(vec![menu(complete_buffer_name(false))])),
            |p, c, _| delete_buffer_impl(p, c, false),
        ),
        cmd(
            "delete-buffer!",
            Some("db!"),
            "delete-buffer! [name]: delete current buffer or the buffer named <name> if given, even if the buffer is unsaved",
            single_optional_param(),
            None,
            Some(make_completer(vec![menu(complete_buffer_name(false))])),
            |p, c, _| delete_buffer_impl(p, c, true),
        ),
        cmd(
            "rename-buffer",
            None,
            "rename-buffer <name>: change current buffer name",
            ParameterDesc::new(
                switches(vec![
                    ("scratch", flag("convert a file buffer to a scratch buffer")),
                    ("file", flag("convert a scratch buffer to a file buffer")),
                ]),
                ParameterFlags::None,
                1,
                1,
            ),
            None,
            Some(filename_completer(false)),
            |parser, context, _| {
                if parser.has_switch("scratch") && parser.has_switch("file") {
                    return Err(Error::runtime("scratch and file are incompatible switches"));
                }

                let buffer = context.try_buffer()?;
                if parser.has_switch("scratch") {
                    buffer.remove_flags(BufferFlags::File | BufferFlags::New);
                }
                if parser.has_switch("file") {
                    buffer.add_flags(BufferFlags::File);
                }

                let is_file = buffer.flags().has(BufferFlags::File);
                let name = if is_file { parse_filename(&parser[0], b"") } else { parser[0].to_vec() };
                if !buffer.set_name(name)? {
                    return Err(rt_err!(
                        "unable to change buffer name to '{}': a buffer with this name already exists",
                        &parser[0]
                    ));
                }
                Ok(())
            },
        ),
        cmd(
            "arrange-buffers",
            None,
            "arrange-buffers <buffer>...: reorder the buffers in the buffers list\n    the named buffers will be moved to the front of the buffer list, in the order given\n    buffers that do not appear in the parameters will remain at the end of the list, keeping their current order",
            ParameterDesc::new(
                switches(vec![(
                    "back",
                    flag("place the named buffers at the back, rather than the front, of the buffer list"),
                )]),
                ParameterFlags::None,
                1,
                usize::MAX,
            ),
            None,
            Some(Rc::new(|context: &Context, params: &[Vec<u8>], _: usize, cursor_pos: usize| {
                menu(complete_buffer_name(false))(context, params.last().map_or(&[][..], |p| p), cursor_pos)
            })),
            |parser, _, _| BufferManager::instance().arrange_buffers(parser.positionals_from(0), parser.has_switch("back")),
        ),
        cmd(
            "add-highlighter",
            Some("addhl"),
            "add-highlighter [-override] <path>/<name> <type> <type params>...: add a highlighter to the group identified by <path>\n    <path> is a '/' delimited path or the parent highlighter, starting with either\n   'global', 'buffer', 'window' or 'shared', if <name> is empty, it will be autogenerated",
            ParameterDesc::new(
                switches(vec![("override", flag("replace existing highlighter with same path if it exists"))]),
                ParameterFlags::SwitchesOnlyAtStart,
                2,
                usize::MAX,
            ),
            helper(add_highlighter_helper),
            Some(highlighter_cmd_completer(true)),
            add_highlighter,
        ),
        cmd(
            "remove-highlighter",
            Some("rmhl"),
            "remove-highlighter <path>: remove highlighter identified by <path>",
            single_param(),
            None,
            Some(highlighter_cmd_completer(false)),
            remove_highlighter,
        ),
        cmd(
            "hook",
            None,
            "hook [<switches>] <scope> <hook_name> <filter> <command>: add <command> in <scope> to be executed on hook <hook_name> when its parameter matches the <filter> regex\n<scope> can be:\n  * global: hook is executed for any buffer or window\n  * buffer: hook is executed only for the current buffer\n            (and any window for that buffer)\n  * window: hook is executed only for the current window\n",
            ParameterDesc::new(
                switches(vec![
                    ("group", arg("set hook group, see remove-hooks")),
                    ("always", flag("run hook even if hooks are disabled")),
                    ("once", flag("run the hook only once")),
                ]),
                ParameterFlags::None,
                4,
                4,
            ),
            None,
            Some(make_completer(vec![
                menu(ps(complete_scope)),
                menu(ps(complete_hooks)),
                ps(complete_nothing),
                command_line_completer(),
            ])),
            add_hook,
        ),
        cmd(
            "remove-hooks",
            Some("rmhooks"),
            "remove-hooks <scope> <group>: remove all hooks whose group matches the regex <group>",
            double_params(),
            None,
            Some(remove_hook_completer()),
            |parser, context, _| {
                get_scope(&parser[0], context)?.hooks().remove_hooks(&Regex::new(&parser[1], RegexCompileFlags::NONE)?);
                Ok(())
            },
        ),
        cmd(
            "trigger-user-hook",
            None,
            "trigger-user-hook <param>: run 'User' hook with <param> as filter string",
            single_param(),
            None,
            None,
            |parser, context, _| context.hooks().run_hook(Hook::User, &parser[0], context),
        ),
        cmd(
            "define-command",
            Some("def"),
            "define-command [<switches>] <name> <cmds>: define a command <name> executing <cmds>",
            define_command_params(),
            None,
            None,
            define_command,
        ),
        cmd(
            "complete-command",
            Some("compl"),
            "complete-command [<switches>] <name> <type> [<param>]\ndefine command completion",
            ParameterDesc::new(
                switches(vec![("menu", flag("treat completions as the only valid inputs"))]),
                ParameterFlags::None,
                2,
                3,
            ),
            None,
            Some(make_completer(vec![ps(complete_command_name), ps(complete_completer_type)])),
            |parser, _, _| {
                let flags = if parser.has_switch("menu") { CompletionFlags::Menu } else { CompletionFlags::None };
                let param: &[u8] = if parser.positional_count() >= 3 { &parser[2] } else { b"" };
                let completer = make_command_completer(&parser[1], param, flags)?;
                CommandManager::instance().set_command_completer(&parser[0], Some(completer))
            },
        ),
        cmd(
            "alias",
            None,
            "alias <scope> <alias> <command>: alias <alias> to <command> in <scope>",
            ParameterDesc::positionals(3, 3),
            None,
            Some(make_completer(vec![menu(ps(complete_scope)), ps(complete_alias_name), ps(complete_command_name)])),
            |parser, context, _| {
                if !CommandManager::instance().command_defined(&parser[2]) {
                    return Err(rt_err!("no such command: '{}'", &parser[2]));
                }
                get_scope(&parser[0], context)?.aliases().add_alias(parser[1].to_vec(), parser[2].to_vec());
                Ok(())
            },
        ),
        cmd(
            "unalias",
            None,
            "unalias <scope> <alias> [<expected>]: remove <alias> from <scope>\nIf <expected> is specified, remove <alias> only if its value is <expected>",
            ParameterDesc::positionals(2, 3),
            None,
            Some(make_completer(vec![menu(ps(complete_scope)), ps(complete_alias_name), ps(complete_command_name)])),
            |parser, context, _| {
                let scope = get_scope(&parser[0], context)?;
                let aliases = scope.aliases();
                if parser.positional_count() == 3 && aliases.get(&parser[1]) != parser[2] {
                    return Ok(());
                }
                aliases.remove_alias(&parser[1]);
                Ok(())
            },
        ),
        cmd(
            "echo",
            None,
            "echo <params>...: display given parameters in the status line",
            ParameterDesc::new(
                switches(vec![
                    ("markup", flag("parse markup")),
                    (
                        "quoting",
                        arg_with(
                            arg_completer(&["raw", "kakoune", "shell"]),
                            "quote each argument separately using the given style (raw|kakoune|shell)",
                        ),
                    ),
                    ("end-of-line", flag("add trailing end-of-line")),
                    ("to-file", arg_with(filename_arg_completer(false), "echo contents to given filename")),
                    ("to-shell-script", arg("pipe contents to given shell script")),
                    ("debug", flag("write to debug buffer instead of status line")),
                ]),
                ParameterFlags::SwitchesOnlyAtStart,
                0,
                usize::MAX,
            ),
            None,
            None,
            echo,
        ),
        cmd(
            "debug",
            None,
            "debug <command>: write some debug information to the *debug* buffer",
            ParameterDesc::new(Default::default(), ParameterFlags::SwitchesOnlyAtStart, 1, usize::MAX),
            None,
            Some(make_completer(vec![Rc::new(|_: &Context, prefix: &[u8], cursor_pos: usize| {
                let c: [&[u8]; 10] = [
                    b"info",
                    b"buffers",
                    b"options",
                    b"memory",
                    b"shared-strings",
                    b"profile-hash-maps",
                    b"faces",
                    b"mappings",
                    b"regex",
                    b"registers",
                ];
                Completions::with(0, cursor_pos, complete(prefix, cursor_pos, c), CompletionFlags::Menu)
            })])),
            debug_cmd,
        ),
        cmd(
            "source",
            None,
            "source <filename> <params>...: execute commands contained in <filename>\nparameters are available in the sourced script as %arg{0}, %arg{1}, …",
            ParameterDesc::positionals(1, usize::MAX),
            None,
            Some(filename_completer(true)),
            source,
        ),
        cmd(
            "set-option",
            Some("set"),
            "set-option [<switches>] <scope> <name> <value>: set option <name> in <scope> to <value>\n<scope> can be global, buffer, window, or current which refers to the narrowest scope the option is set in",
            ParameterDesc::new(
                switches(vec![
                    ("add", flag("add to option rather than replacing it")),
                    ("remove", flag("remove from option rather than replacing it")),
                ]),
                ParameterFlags::SwitchesOnlyAtStart,
                2,
                usize::MAX,
            ),
            helper(option_doc_helper),
            Some(set_option_completer()),
            set_option,
        ),
        cmd(
            "unset-option",
            Some("unset"),
            "unset-option <scope> <name>: remove <name> option from scope, falling back on parent scope value\n<scope> can be buffer, window, or current which refers to the narrowest scope the option is set in",
            double_params(),
            helper(option_doc_helper),
            Some(complete_option()),
            unset_option,
        ),
        cmd(
            "update-option",
            None,
            "update-option <scope> <name>: update <name> option from scope\nsome option types, such as line-specs or range-specs can be updated to latest buffer timestamp\n<scope> can be buffer, window, or current which refers to the narrowest scope the option is set in",
            double_params(),
            helper(option_doc_helper),
            Some(complete_option()),
            update_option,
        ),
        cmd(
            "declare-option",
            Some("decl"),
            "declare-option [<switches>] <type> <name> [value]: declare option <name> of type <type>.\nset its initial value to <value> if given and the option did not exist\nAvailable types:\n    int: integer\n    bool: boolean (true/false or yes/no)\n    str: character string\n    regex: regular expression\n    int-list: list of integers\n    str-list: list of character strings\n    completions: list of completion candidates\n    line-specs: list of line specs\n    range-specs: list of range specs\n    str-to-str-map: map from strings to strings\n",
            ParameterDesc::new(
                switches(vec![
                    ("hidden", flag("do not display option name when completing")),
                    ("docstring", arg("specify option description")),
                ]),
                ParameterFlags::SwitchesOnlyAtStart,
                2,
                usize::MAX,
            ),
            None,
            Some(make_completer(vec![Rc::new(|_: &Context, prefix: &[u8], cursor_pos: usize| {
                let c: [&[u8]; 10] = [
                    b"int",
                    b"bool",
                    b"str",
                    b"regex",
                    b"int-list",
                    b"str-list",
                    b"completions",
                    b"line-specs",
                    b"range-specs",
                    b"str-to-str-map",
                ];
                Completions::with(0, cursor_pos, complete(prefix, cursor_pos, c), CompletionFlags::Menu)
            })])),
            declare_option,
        ),
        cmd(
            "map",
            None,
            "map [<switches>] <scope> <mode> <key> <keys>: map <key> to <keys> in given <mode> in <scope>",
            ParameterDesc::new(
                switches(vec![
                    ("atomic", flag("repeat whole mapping if count given")),
                    ("docstring", arg("specify mapping description")),
                ]),
                ParameterFlags::None,
                4,
                4,
            ),
            None,
            Some(map_key_completer(false)),
            map_key,
        ),
        cmd(
            "unmap",
            None,
            "unmap <scope> <mode> [<key> [<expected-keys>]]: unmap <key> from given <mode> in <scope>.\nIf <expected-keys> is specified, remove the mapping only if its value is <expected-keys>.\nIf only <scope> and <mode> are specified remove all mappings",
            ParameterDesc::positionals(2, 4),
            None,
            Some(map_key_completer(true)),
            unmap_key,
        ),
        cmd(
            "execute-keys",
            Some("exec"),
            "execute-keys [<switches>] <keys>: execute given keys as if entered by user",
            make_context_wrap_params(vec![
                ("save-regs", arg("restore all given registers after execution (default: '/\"|^@:')")),
                ("with-maps", flag("use user defined key mapping when executing keys")),
                ("with-hooks", flag("trigger hooks while executing keys")),
            ]),
            None,
            None,
            execute_keys,
        ),
        cmd(
            "evaluate-commands",
            Some("eval"),
            "evaluate-commands [<switches>] <commands>...: execute commands as if entered by user",
            make_context_wrap_params(vec![
                ("save-regs", arg("restore all given registers after execution (default: '')")),
                ("no-hooks", flag("disable hooks while executing commands")),
                ("verbatim", flag("do not reparse argument")),
            ]),
            None,
            Some(nested_completer()),
            evaluate_commands,
        ),
        cmd(
            "prompt",
            None,
            "prompt [<switches>] <prompt> <command>: prompt the user to enter a text string and then executes <command>, entered text is available in the 'text' value",
            ParameterDesc::new(
                switches(vec![
                    ("init", arg("set initial prompt content")),
                    ("password", flag("Do not display entered text and clear reg after command")),
                    ("menu", flag("treat completions as the only valid inputs")),
                    ("file-completion", flag("use file completion for prompt")),
                    ("client-completion", flag("use client completion for prompt")),
                    ("buffer-completion", flag("use buffer completion for prompt")),
                    ("command-completion", flag("use command completion for prompt")),
                    ("shell-completion", flag("use shell command completion for prompt")),
                    ("shell-script-completion", arg("use shell command completion for prompt")),
                    ("shell-script-candidates", arg("use shell command completion for prompt")),
                    ("on-change", arg("command to execute whenever the prompt changes")),
                    ("on-abort", arg("command to execute whenever the prompt is canceled")),
                ]),
                ParameterFlags::None,
                2,
                2,
            ),
            None,
            None,
            prompt_cmd,
        ),
        cmd(
            "on-key",
            None,
            "on-key [<switches>] <command>: wait for next user key and then execute <command>, with key available in the `key` value",
            ParameterDesc::new(switches(vec![("mode-name", arg("set mode name to use"))]), ParameterFlags::None, 1, 1),
            None,
            None,
            on_key,
        ),
        cmd(
            "info",
            None,
            "info [<switches>] <text>: display an info box containing <text>",
            ParameterDesc::new(
                switches(vec![
                    ("anchor", arg("set info anchoring <line>.<column>")),
                    (
                        "style",
                        arg_with(arg_completer(&["above", "below", "menu", "modal"]), "set info style (above, below, menu, modal)"),
                    ),
                    ("markup", flag("parse markup")),
                    ("title", arg("set info title")),
                ]),
                ParameterFlags::None,
                0,
                1,
            ),
            None,
            None,
            info,
        ),
        cmd(
            "try",
            None,
            "try <cmds> [catch <error_cmds>]...: execute <cmds> in current context.\nif an error is raised and <error_cmds> is specified, execute it and do\nnot propagate that error. If <error_cmds> raises an error and another\n<error_cmds> is provided, execute this one and so-on\n",
            ParameterDesc::positionals(1, usize::MAX),
            None,
            None,
            try_catch,
        ),
        cmd(
            "set-face",
            Some("face"),
            "set-face <scope> <name> <facespec>: set face <name> to <facespec> in <scope>\n\nfacespec format is:\n    <fg color>[,<bg color>[,<underline color>]][+<attributes>][@<base>]\ncolors are either a color name, rgb:######, or rgba:######## values.\nattributes is a combination of:\n    u: underline, c: curly underline, U: double underline,\n    i: italic,            b: bold,            r: reverse,\n    s: strikethrough,     B: blink,           d: dim,\n    f: final foreground,              g: final background,\n    a: final attributes,              F: same as +fga\nfacespec can as well just be the name of another face.\nif a base face is specified, colors and attributes are applied on top of it",
            ParameterDesc::positionals(3, 3),
            helper(face_doc_helper),
            Some(make_completer(vec![menu(ps(complete_scope)), ps(complete_face), ps(complete_face)])),
            set_face,
        ),
        cmd(
            "unset-face",
            None,
            "unset-face <scope> <name>: remove <face> from <scope>",
            double_params(),
            helper(face_doc_helper),
            Some(make_completer(vec![menu(ps(complete_scope)), menu(ps(complete_face))])),
            |parser, context, _| {
                get_scope(&parser[0], context)?.faces().remove_face(&parser[1]);
                Ok(())
            },
        ),
        cmd(
            "rename-client",
            None,
            "rename-client <name>: set current client name to <name>",
            single_param(),
            None,
            Some(make_single_word_completer(|context| context.name())),
            rename_client,
        ),
        cmd(
            "set-register",
            Some("reg"),
            "set-register <name> <values>...: set register <name> to <values>",
            ParameterDesc::new(Default::default(), ParameterFlags::SwitchesAsPositional, 1, usize::MAX),
            None,
            Some(make_completer(vec![Rc::new(|_: &Context, prefix: &[u8], cursor_pos: usize| {
                Completions::with(
                    0,
                    cursor_pos,
                    RegisterManager::instance().complete_register_name(prefix, cursor_pos),
                    CompletionFlags::None,
                )
            })])),
            |parser, context, _| RegisterManager::instance().get(&parser[0])?.set(context, parser.positionals_from(1), false),
        ),
        cmd(
            "select",
            None,
            "select <selection_desc>...: select given selections\n\nselection_desc format is <anchor_line>.<anchor_column>,<cursor_line>.<cursor_column>",
            ParameterDesc::new(
                switches(vec![
                    ("timestamp", arg("specify buffer timestamp at which those selections are valid")),
                    ("codepoint", flag("columns are specified in codepoints, not bytes")),
                    ("display-column", flag("columns are specified in display columns, not bytes")),
                ]),
                ParameterFlags::SwitchesOnlyAtStart,
                1,
                usize::MAX,
            ),
            None,
            None,
            select,
        ),
        cmd(
            "change-directory",
            Some("cd"),
            "change-directory [<directory>]: change the server's working directory to <directory>, or the home directory if unspecified",
            single_optional_param(),
            None,
            Some(make_completer(vec![filename_arg_completer(true)])),
            change_directory,
        ),
        cmd(
            "rename-session",
            None,
            "rename-session <name>: change remote session name",
            single_param(),
            None,
            Some(make_single_word_completer(|_| Server::instance().session())),
            rename_session,
        ),
        cmd(
            "fail",
            None,
            "fail [<message>]: raise an error with the given message",
            ParameterDesc::default(),
            None,
            None,
            |parser, _, _| Err(Error::failure(join_str(parser.iter(), b" "))),
        ),
        cmd(
            "declare-user-mode",
            None,
            "declare-user-mode <name>: add a new user keymap mode",
            single_param(),
            None,
            None,
            |parser, context, _| context.keymaps().add_user_mode(parser[0].to_vec()),
        ),
        cmd(
            "enter-user-mode",
            None,
            "enter-user-mode [<switches>] <name> [<count>] [<register>]: enable <name> keymap mode for next key",
            ParameterDesc::new(
                switches(vec![
                    ("lock", flag("stay in mode until <esc> is pressed")),
                    ("count", arg("count to forward to normal mode parameters")),
                    ("register", arg("register to forward to normal mode parameters")),
                ]),
                ParameterFlags::None,
                1,
                1,
            ),
            None,
            Some(Rc::new(|context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize| {
                if token_to_complete == 0 {
                    return Completions::with(
                        0,
                        params[0].len(),
                        complete(&params[0], pos_in_token, context.keymaps().user_modes()),
                        CompletionFlags::Menu,
                    );
                }
                Completions::default()
            })),
            |parser, context, _| {
                let lock = parser.has_switch("lock");
                let mode = parse_keymap_mode(&parser[0], &context.keymaps().user_modes())?;
                let params = NormalParams {
                    count: parser.get_switch("count").and_then(str_to_int_ifp).unwrap_or(0),
                    reg: parser.get_switch("register").map(|s| s.first().copied().unwrap_or(0)).unwrap_or(0),
                };
                enter_user_mode(context, params, parser[0].to_vec(), mode, lock)
            },
        ),
        cmd(
            "provide-module",
            None,
            "provide-module [<switches>] <name> <cmds>: declares a module <name> provided by <cmds>",
            ParameterDesc::new(switches(vec![("override", flag("allow overriding an existing module"))]), ParameterFlags::None, 2, 2),
            None,
            None,
            |parser, _, _| {
                let module_name = &parser[0];
                let cm = CommandManager::instance();
                if !module_name.iter().all(|&c| is_identifier(c as u32)) {
                    return Err(rt_err!("invalid module name: '{}'", module_name));
                }
                if cm.module_defined(module_name) && !parser.has_switch("override") {
                    return Err(rt_err!("module '{}' already defined", module_name));
                }
                cm.register_module(module_name.to_vec(), parser[1].to_vec())
            },
        ),
        cmd(
            "require-module",
            None,
            "require-module <name>: ensures that <name> module has been loaded",
            single_param(),
            None,
            Some(make_completer(vec![menu(Rc::new(|_: &Context, prefix: &[u8], cursor_pos: usize| {
                CommandManager::instance().complete_module_name(&prefix[..cursor_pos.min(prefix.len())])
            }))])),
            |parser, context, _| CommandManager::instance().load_module(&parser[0], context),
        ),
    ];
    cmds.shrink_to_fit();
    cmds
}
