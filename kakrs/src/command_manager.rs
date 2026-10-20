//! Port of command_manager.hh/cc

use crate::completion::{complete, complete_filename, offset_pos, shell_complete, CompletionFlags, Completions, FilenameFlags};
use crate::context::{Context, LocalScope};
use crate::debug::{write_to_debug_buffer, DebugFlags, EnvVarMap, ProfileScope};
use crate::exception::{Error, Result};
use crate::file::read_file;
use crate::format::Cp;
use crate::hook_manager::Hook;
use crate::parameters_parser::{generate_switches_doc, ParameterDesc, ParametersParser, ParserState};
use crate::regex::Regex;
use crate::register_manager::RegisterManager;
use crate::scope::GlobalScope;
use crate::shell_manager::{ShellFlags, ShellManager};
use crate::string_utils::{escape, indent, join, quote, str_to_int, Quoting};
use crate::units::{BufferCoord, Codepoint};
use crate::utf8;
use indexmap::IndexMap;
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

pub type CommandParameters<'a> = &'a [Vec<u8>];
pub type CommandFunc = Rc<dyn Fn(&ParametersParser, &Context, &ShellContext) -> Result<()>>;
pub type CommandCompleter = Rc<dyn Fn(&Context, &[Vec<u8>], usize, usize) -> Completions>;
pub type CommandHelper = Rc<dyn Fn(&Context, &[Vec<u8>]) -> Vec<u8>>;

crate::kak_flags! {
    pub struct CommandFlags: u32 {
        const None = 0;
        const Hidden = 1;
    }
}

#[derive(Clone, Default)]
pub struct ShellContext {
    pub params: Vec<Vec<u8>>,
    pub env_vars: EnvVarMap,
}

#[derive(Clone, Debug, Default)]
pub struct CommandInfo {
    pub name: Vec<u8>,
    pub info: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenType {
    Raw,
    RawQuoted,
    Expand,
    ShellExpand,
    RegisterExpand,
    OptionExpand,
    ValExpand,
    ArgExpand,
    FileExpand,
    CommandSeparator,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub type_: TokenType,
    pub pos: usize,
    pub content: Vec<u8>,
    pub terminated: bool,
}

impl Default for Token {
    fn default() -> Self {
        Token { type_: TokenType::Raw, pos: 0, content: Vec::new(), terminated: false }
    }
}

pub struct ParseState<'a> {
    pub str: &'a [u8],
    pub pos: usize,
}

impl ParseState<'_> {
    fn at_end(&self) -> bool {
        self.pos == self.str.len()
    }
}

fn parse_error(error: Vec<u8>) -> Error {
    Error::runtime(kformat!("parse error: {}", error))
}

fn is_command_separator(c: u8) -> bool {
    c == b';' || c == b'\n'
}

struct ParseResult {
    content: Vec<u8>,
    terminated: bool,
}

/// parse_quoted with a byte delimiter (codepoint delimiter if !ascii)
fn parse_quoted(state: &mut ParseState, delimiter: Codepoint, as_codepoint: bool) -> ParseResult {
    let s = state.str;
    let end = s.len();
    let read = |pos: &mut usize| -> Codepoint {
        if as_codepoint {
            utf8::read_codepoint(s, pos)
        } else {
            let c = s[*pos];
            *pos += 1;
            c as Codepoint
        }
    };
    let mut beg = state.pos;
    let mut res: Vec<u8> = Vec::new();
    while state.pos != end {
        let cur = state.pos;
        let c = read(&mut state.pos);
        if c == delimiter {
            let mut next = state.pos;
            if next == end || read(&mut next) != delimiter {
                res.extend_from_slice(&s[beg..cur]);
                return ParseResult { content: res, terminated: true };
            }
            res.extend_from_slice(&s[beg..state.pos]);
            state.pos = next;
            beg = next;
        }
    }
    if beg < end {
        res.extend_from_slice(&s[beg..end]);
    }
    ParseResult { content: res, terminated: false }
}

fn parse_quoted_balanced(state: &mut ParseState, opening: u8, closing: u8) -> ParseResult {
    let mut level = 1;
    let s = state.str;
    let mut pos = state.pos;
    let beg = pos;
    while pos != s.len() {
        let c = s[pos];
        pos += 1;
        if c == opening {
            level += 1;
        } else if c == closing {
            level -= 1;
            if level == 0 {
                break;
            }
        }
    }
    state.pos = pos;
    let terminated = level == 0;
    ParseResult { content: s[beg..pos - terminated as usize].to_vec(), terminated }
}

fn is_ascii_horizontal_blank(c: u8) -> bool {
    c == b'\t' || c == 0x0C || c == b' '
}

fn parse_unquoted(state: &mut ParseState) -> Vec<u8> {
    let s = state.str;
    let end = s.len();
    let mut beg = state.pos;
    let mut res: Vec<u8> = Vec::new();
    while state.pos != end {
        let c = s[state.pos];
        if is_command_separator(c) || is_ascii_horizontal_blank(c) {
            res.extend_from_slice(&s[beg..state.pos]);
            if state.pos != beg && s[state.pos - 1] == b'\\' {
                *res.last_mut().unwrap() = c;
                beg = state.pos + 1;
            } else {
                return res;
            }
        }
        state.pos += 1;
    }
    if beg < end {
        res.extend_from_slice(&s[beg..end]);
    }
    res
}

fn token_type(type_name: &[u8], throw_on_invalid: bool) -> Result<TokenType> {
    Ok(match type_name {
        b"" => TokenType::RawQuoted,
        b"sh" => TokenType::ShellExpand,
        b"reg" => TokenType::RegisterExpand,
        b"opt" => TokenType::OptionExpand,
        b"val" => TokenType::ValExpand,
        b"arg" => TokenType::ArgExpand,
        b"file" => TokenType::FileExpand,
        b"exp" => TokenType::Expand,
        _ if throw_on_invalid => return Err(parse_error(kformat!("unknown expand '{}'", type_name))),
        _ => TokenType::RawQuoted,
    })
}

fn skip_blanks_and_comments(state: &mut ParseState) {
    let s = state.str;
    while !state.at_end() {
        let c = s[state.pos];
        if is_ascii_horizontal_blank(c) {
            state.pos += 1;
        } else if c == b'\\' && state.pos + 1 != s.len() && s[state.pos + 1] == b'\n' {
            state.pos += 2;
        } else if c == b'#' {
            while !state.at_end() && s[state.pos] != b'\n' {
                state.pos += 1;
            }
        } else {
            break;
        }
    }
}

fn compute_coord(s: &[u8]) -> BufferCoord {
    let mut coord = BufferCoord::new(0, 0);
    for &c in s {
        if c == b'\n' {
            coord.line += 1;
            coord.column = 0;
        } else {
            coord.column += 1;
        }
    }
    coord
}

fn iswalpha(cp: Codepoint) -> bool {
    unsafe extern "C" {
        fn iswalpha(c: i32) -> i32;
    }
    unsafe { iswalpha(cp as i32) != 0 }
}

fn parse_percent_token(state: &mut ParseState, throw_on_unterminated: bool) -> Result<Token> {
    let s = state.str;
    let type_start = state.pos;
    while !state.at_end() && s[state.pos].is_ascii_lowercase() {
        state.pos += 1;
    }
    let type_name = &s[type_start..state.pos];

    let at_end = state.at_end();
    let opening_delimiter = utf8::read_codepoint(s, &mut state.pos);
    if at_end || iswalpha(opening_delimiter) {
        if throw_on_unterminated {
            return Err(parse_error(kformat!("expected a string delimiter after '%{}'", type_name)));
        }
        return Ok(Token::default());
    }

    let type_ = token_type(type_name, throw_on_unterminated)?;

    const MATCHING_PAIRS: [(u8, u8); 4] = [(b'(', b')'), (b'[', b']'), (b'{', b'}'), (b'<', b'>')];

    let start = state.pos;
    let byte_pos = start;
    if let Some(&(opening, closing)) = MATCHING_PAIRS.iter().find(|(o, _)| opening_delimiter == *o as u32) {
        let quoted = parse_quoted_balanced(state, opening, closing);
        if throw_on_unterminated && !quoted.terminated {
            let coord = compute_coord(&s[..start]);
            return Err(parse_error(kformat!(
                "{}:{}: unterminated string '%{}{}...{}'",
                coord.line + 1,
                coord.column + 1,
                type_name,
                [opening],
                [closing]
            )));
        }
        Ok(Token { type_, pos: byte_pos, content: quoted.content, terminated: quoted.terminated })
    } else {
        let is_ascii = opening_delimiter < 128;
        let quoted = parse_quoted(state, opening_delimiter, !is_ascii);
        if throw_on_unterminated && !quoted.terminated {
            let coord = compute_coord(&s[..start]);
            return Err(parse_error(kformat!(
                "{}:{}: unterminated string '%{}{}...{}'",
                coord.line + 1,
                coord.column + 1,
                type_name,
                Cp(opening_delimiter),
                Cp(opening_delimiter)
            )));
        }
        Ok(Token { type_, pos: byte_pos, content: quoted.content, terminated: quoted.terminated })
    }
}

/// Target of a token expansion: either a single string or a list
enum ExpandTarget<'a> {
    Single(&'a mut Vec<u8>),
    List(&'a mut Vec<Vec<u8>>),
}

fn expand_token(token: Token, context: &Context, shell_context: &ShellContext, target: ExpandTarget) -> Result<()> {
    let single = matches!(target, ExpandTarget::Single(_));
    let set_one = |target: ExpandTarget, s: Vec<u8>| match target {
        ExpandTarget::Single(t) => *t = s,
        ExpandTarget::List(t) => t.push(s),
    };
    let set_many = |target: ExpandTarget, v: Vec<Vec<u8>>| match target {
        ExpandTarget::Single(_) => unreachable!(),
        ExpandTarget::List(t) => t.extend(v),
    };
    let content = token.content;
    match token.type_ {
        TokenType::ShellExpand => {
            let (mut s, _) = ShellManager::instance().eval(
                &content,
                context,
                b"",
                ShellFlags::WaitForStdout,
                shell_context,
            )?;
            if s.last() == Some(&b'\n') {
                s.pop();
            }
            set_one(target, s);
        }
        TokenType::RegisterExpand => {
            if single {
                set_one(target, context.main_sel_register_value(&content)?);
            } else {
                set_many(target, RegisterManager::instance().get(&content)?.get(context)?);
            }
        }
        TokenType::OptionExpand => {
            let opt = context.options().get(&content)?;
            if single {
                set_one(target, opt.get_as_string(Quoting::Raw));
            } else {
                set_many(target, opt.get_as_strings());
            }
        }
        TokenType::ValExpand => {
            if let Some(v) = shell_context.env_vars.get(&content) {
                set_one(target, v.clone());
                return Ok(());
            }
            let val = ShellManager::instance().get_val(&content, context)?;
            if single {
                set_one(target, join(&val, b' ', false));
            } else {
                set_many(target, val);
            }
        }
        TokenType::ArgExpand => {
            let params = &shell_context.params;
            if content == b"@" {
                if single {
                    set_one(target, join(params, b' ', false));
                } else {
                    set_many(target, params.clone());
                }
                return Ok(());
            }
            let arg = str_to_int(&content)?;
            if arg < 1 {
                return Err(Error::runtime("invalid argument index"));
            }
            set_one(target, params.get(arg as usize - 1).cloned().unwrap_or_default());
        }
        TokenType::FileExpand => set_one(target, read_file(&content, false)?),
        TokenType::Expand => set_one(target, expand(&content, context, shell_context)?),
        TokenType::Raw | TokenType::RawQuoted => set_one(target, content),
        TokenType::CommandSeparator => unreachable!(),
    }
    Ok(())
}

pub struct CommandParser<'a> {
    state: ParseState<'a>,
}

impl<'a> CommandParser<'a> {
    pub fn new(command_line: &'a [u8]) -> Self {
        CommandParser { state: ParseState { str: command_line, pos: 0 } }
    }

    pub fn pos(&self) -> usize {
        self.state.pos
    }

    pub fn done(&self) -> bool {
        self.state.at_end()
    }

    pub fn read_token(&mut self, throw_on_unterminated: bool) -> Result<Option<Token>> {
        skip_blanks_and_comments(&mut self.state);
        if self.state.at_end() {
            return Ok(None);
        }
        let line = self.state.str;
        let start = self.state.pos;
        let c = line[self.state.pos];
        if c == b'"' || c == b'\'' {
            self.state.pos += 1;
            let start = self.state.pos;
            let quoted = parse_quoted(&mut self.state, c as Codepoint, false);
            if throw_on_unterminated && !quoted.terminated {
                return Err(parse_error(kformat!("unterminated string {0}...{0}", [c])));
            }
            return Ok(Some(Token {
                type_: if c == b'"' { TokenType::Expand } else { TokenType::RawQuoted },
                pos: start,
                content: quoted.content,
                terminated: quoted.terminated,
            }));
        } else if c == b'%' {
            self.state.pos += 1;
            return Ok(Some(parse_percent_token(&mut self.state, throw_on_unterminated)?));
        } else if is_command_separator(c) {
            self.state.pos += 1;
            return Ok(Some(Token {
                type_: TokenType::CommandSeparator,
                pos: self.state.pos,
                content: Vec::new(),
                terminated: false,
            }));
        }
        if c == b'\\' && self.state.pos + 1 != line.len() {
            let next = line[self.state.pos + 1];
            if next == b'%' || next == b'\'' || next == b'"' {
                self.state.pos += 1;
            }
        }
        Ok(Some(Token {
            type_: TokenType::Raw,
            pos: start,
            content: parse_unquoted(&mut self.state),
            terminated: false,
        }))
    }
}

fn expand_impl(
    s: &[u8],
    context: &Context,
    shell_context: &ShellContext,
    postprocess: &dyn Fn(Vec<u8>) -> Vec<u8>,
) -> Result<Vec<u8>> {
    let mut state = ParseState { str: s, pos: 0 };
    let mut res = Vec::new();
    let mut beg = 0;
    while !state.at_end() {
        let c = s[state.pos];
        state.pos += 1;
        if c == b'%' {
            if !state.at_end() && s[state.pos] == b'%' {
                res.extend_from_slice(&s[beg..state.pos]);
                state.pos += 1;
                beg = state.pos;
            } else {
                res.extend_from_slice(&s[beg..state.pos - 1]);
                let mut token = Vec::new();
                let parsed = parse_percent_token(&mut state, true)?;
                expand_token(parsed, context, shell_context, ExpandTarget::Single(&mut token))?;
                res.extend(postprocess(token));
                beg = state.pos;
            }
        }
    }
    res.extend_from_slice(&s[beg..state.pos]);
    Ok(res)
}

pub fn expand(s: &[u8], context: &Context, shell_context: &ShellContext) -> Result<Vec<u8>> {
    expand_impl(s, context, shell_context, &|s| s)
}

pub fn expand_with(
    s: &[u8],
    context: &Context,
    shell_context: &ShellContext,
    postprocess: &dyn Fn(Vec<u8>) -> Vec<u8>,
) -> Result<Vec<u8>> {
    expand_impl(s, context, shell_context, postprocess)
}

fn resolve_alias(context: &Context, name: &[u8]) -> Vec<u8> {
    let alias = context.aliases().get(name);
    if alias.is_empty() { name.to_vec() } else { alias }
}

pub struct Command {
    pub func: CommandFunc,
    pub docstring: Vec<u8>,
    pub param_desc: ParameterDesc,
    pub flags: CommandFlags,
    pub helper: Option<CommandHelper>,
    pub completer: Option<CommandCompleter>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ModuleState {
    Registered,
    Loading,
    Loaded,
}

struct Module {
    state: ModuleState,
    commands: Vec<u8>,
}

pub struct CommandManager {
    commands: RefCell<IndexMap<Vec<u8>, Rc<Command>>>,
    command_depth: Cell<i32>,
    modules: RefCell<IndexMap<Vec<u8>, Module>>,
}

thread_local! {
    static COMMAND_MANAGER: OnceCell<&'static CommandManager> = const { OnceCell::new() };
}

impl CommandManager {
    pub fn instance() -> &'static CommandManager {
        COMMAND_MANAGER.with(|c| {
            *c.get_or_init(|| {
                Box::leak(Box::new(CommandManager {
                    commands: RefCell::new(IndexMap::new()),
                    command_depth: Cell::new(0),
                    modules: RefCell::new(IndexMap::new()),
                }))
            })
        })
    }

    pub fn command_defined(&self, command_name: &[u8]) -> bool {
        self.commands.borrow().contains_key(command_name)
    }

    pub fn register_command(
        &self,
        command_name: Vec<u8>,
        func: CommandFunc,
        docstring: Vec<u8>,
        param_desc: ParameterDesc,
        flags: CommandFlags,
        helper: Option<CommandHelper>,
        completer: Option<CommandCompleter>,
    ) {
        self.commands
            .borrow_mut()
            .insert(command_name, Rc::new(Command { func, docstring, param_desc, flags, helper, completer }));
    }

    pub fn get_command(&self, name: &[u8]) -> Option<Rc<Command>> {
        self.commands.borrow().get(name).cloned()
    }

    pub fn set_command_completer(&self, command_name: &[u8], completer: Option<CommandCompleter>) -> Result<()> {
        let mut commands = self.commands.borrow_mut();
        let Some(cmd) = commands.get_mut(command_name) else {
            return Err(rt_err!("no such command '{}'", command_name));
        };
        let old = cmd.as_ref();
        *cmd = Rc::new(Command {
            func: old.func.clone(),
            docstring: old.docstring.clone(),
            param_desc: old.param_desc.clone(),
            flags: old.flags,
            helper: old.helper.clone(),
            completer,
        });
        Ok(())
    }

    pub fn module_defined(&self, module_name: &[u8]) -> bool {
        self.modules.borrow().contains_key(module_name)
    }

    pub fn register_module(&self, module_name: Vec<u8>, commands: Vec<u8>) -> Result<()> {
        let mut modules = self.modules.borrow_mut();
        if let Some(m) = modules.get(&module_name) {
            if m.state != ModuleState::Registered {
                return Err(rt_err!("module already loaded: '{}'", module_name));
            }
        }
        modules.insert(module_name, Module { state: ModuleState::Registered, commands });
        Ok(())
    }

    pub fn load_module(&self, module_name: &[u8], context: &Context) -> Result<()> {
        let commands = {
            let mut modules = self.modules.borrow_mut();
            let Some(module) = modules.get_mut(module_name) else {
                return Err(rt_err!("no such module: '{}'", module_name));
            };
            match module.state {
                ModuleState::Loading => return Err(rt_err!("module '{}' loaded recursively", module_name)),
                ModuleState::Loaded => return Ok(()),
                ModuleState::Registered => {}
            }
            module.state = ModuleState::Loading;
            module.commands.clone()
        };

        let empty_context = Context::new_empty();
        let res = self.execute(&commands, &empty_context, &ShellContext::default());
        {
            let mut modules = self.modules.borrow_mut();
            let module = modules.get_mut(module_name).unwrap();
            match &res {
                Ok(()) => {
                    module.commands.clear();
                    module.state = ModuleState::Loaded;
                }
                Err(_) => module.state = ModuleState::Registered,
            }
        }
        res?;

        context.hooks().run_hook(Hook::ModuleLoaded, module_name, context)
    }

    pub fn loaded_modules(&self) -> Vec<Vec<u8>> {
        self.modules
            .borrow()
            .iter()
            .filter(|(_, m)| m.state == ModuleState::Loaded)
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn execute_single_command(&self, params: &[Vec<u8>], context: &Context, shell_context: &ShellContext) -> Result<()> {
        if params.is_empty() {
            return Ok(());
        }

        const MAX_COMMAND_DEPTH: i32 = 100;
        if self.command_depth.get() > MAX_COMMAND_DEPTH {
            return Err(Error::runtime("maximum nested command depth hit"));
        }

        self.command_depth.set(self.command_depth.get() + 1);
        let _pop_depth = crate::utils::OnScopeEnd::new(|| self.command_depth.set(self.command_depth.get() - 1));

        let Some(command) = self.get_command(&resolve_alias(context, &params[0])) else {
            return Err(Error::runtime("no such command"));
        };

        let debug_flags = context.options().val::<DebugFlags>("debug");
        if debug_flags.has(DebugFlags::Commands) {
            write_to_debug_buffer(&kformat!("command {}", join(params, b' ', true)));
        }

        let _profile = ProfileScope::new(
            debug_flags,
            |duration| {
                write_to_debug_buffer(&kformat!("command {} took {} us", params[0], duration.as_micros() as usize))
            },
            true,
        );

        let parser = ParametersParser::new(&params[1..], &command.param_desc)?;
        (command.func)(&parser, context, shell_context)
    }

    pub fn execute(&self, command_line: &[u8], context: &Context, shell_context: &ShellContext) -> Result<()> {
        let mut parser = CommandParser::new(command_line);
        let _local_scope = LocalScope::new(context);

        let mut command_pos = 0;
        let mut params: Vec<Vec<u8>> = Vec::new();
        loop {
            let token = parser.read_token(true)?;
            match token {
                None => {
                    self.execute_and_wrap_error(&params, command_line, command_pos, context, shell_context)?;
                    return Ok(());
                }
                Some(token) if token.type_ == TokenType::CommandSeparator => {
                    self.execute_and_wrap_error(&params, command_line, command_pos, context, shell_context)?;
                    params.clear();
                    continue;
                }
                Some(token) => {
                    if params.is_empty() {
                        command_pos = token.pos;
                    }
                    if token.type_ == TokenType::ArgExpand && token.content == b"@" {
                        params.extend(shell_context.params.iter().cloned());
                    } else {
                        expand_token(token, context, shell_context, ExpandTarget::List(&mut params))?;
                    }
                }
            }
        }
    }

    fn execute_and_wrap_error(
        &self,
        params: &[Vec<u8>],
        command_line: &[u8],
        command_pos: usize,
        context: &Context,
        shell_context: &ShellContext,
    ) -> Result<()> {
        match self.execute_single_command(params, context, shell_context) {
            Ok(()) => Ok(()),
            Err(error) if error.is_failure() => Err(error),
            Err(mut error) if error.is_runtime() => {
                let coord = compute_coord(&command_line[..command_pos.min(command_line.len())]);
                let what = kformat!("{}:{}: '{}': {}", coord.line + 1, coord.column + 1, params[0], error.what());
                error.set_what(what);
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    pub fn command_info(&self, context: &Context, command_line: &[u8]) -> Option<CommandInfo> {
        let mut parser = CommandParser::new(command_line);
        let mut tokens: Vec<Token> = Vec::new();
        while let Ok(Some(token)) = parser.read_token(false) {
            if token.type_ == TokenType::CommandSeparator {
                tokens.clear();
            } else {
                tokens.push(token);
            }
        }

        if tokens.is_empty() || (tokens[0].type_ != TokenType::Raw && tokens[0].type_ != TokenType::RawQuoted) {
            return None;
        }

        let name = resolve_alias(context, &tokens[0].content);
        let cmd = self.get_command(&name)?;

        let mut res = CommandInfo { name: name.clone(), info: Vec::new() };
        if !cmd.docstring.is_empty() {
            res.info.extend_from_slice(&cmd.docstring);
            res.info.push(b'\n');
        }

        if let Some(helper) = &cmd.helper {
            let params: Vec<Vec<u8>> = tokens[1..]
                .iter()
                .filter(|t| matches!(t.type_, TokenType::Raw | TokenType::RawQuoted | TokenType::Expand))
                .map(|t| t.content.clone())
                .collect();
            let helpstr = helper(context, &params);
            if !helpstr.is_empty() {
                res.info.extend(kformat!("{}\n", helpstr));
            }
        }

        let mut aliases = Vec::new();
        for alias in context.aliases().aliases_for(&name) {
            aliases.push(b' ');
            aliases.extend(alias);
        }
        if !aliases.is_empty() {
            res.info.extend(kformat!("Aliases:{}\n", aliases));
        }

        let switches = &cmd.param_desc.switches;
        if !switches.is_empty() {
            res.info.extend(kformat!("Switches:\n{}", indent(&generate_switches_doc(switches), b"    ")));
        }

        Some(res)
    }

    pub fn complete_command_name(&self, context: &Context, query: &[u8]) -> Completions {
        let mut names: Vec<Vec<u8>> = self
            .commands
            .borrow()
            .iter()
            .filter(|(_, c)| !c.flags.has(CommandFlags::Hidden))
            .map(|(k, _)| k.clone())
            .collect();
        names.extend(context.aliases().flatten_aliases().into_iter().map(|(k, _)| k));
        Completions::with(
            0,
            query.len(),
            complete(query, query.len(), &names),
            CompletionFlags::Menu | CompletionFlags::NoEmpty,
        )
    }

    pub fn complete_module_name(&self, query: &[u8]) -> Completions {
        let names: Vec<Vec<u8>> = self
            .modules
            .borrow()
            .iter()
            .filter(|(_, m)| m.state == ModuleState::Registered)
            .map(|(k, _)| k.clone())
            .collect();
        Completions::with(0, query.len(), complete(query, query.len(), &names), CompletionFlags::None)
    }
}

fn complete_expansion(context: &Context, token: &Token, start: usize, cursor_pos: usize, pos_in_token: usize) -> Completions {
    match token.type_ {
        TokenType::RegisterExpand => Completions::with(
            start,
            cursor_pos,
            RegisterManager::instance().complete_register_name(&token.content, pos_in_token),
            CompletionFlags::None,
        ),
        TokenType::OptionExpand => Completions::with(
            start,
            cursor_pos,
            GlobalScope::instance().option_registry().complete_option_name(&token.content, pos_in_token),
            CompletionFlags::None,
        ),
        TokenType::ShellExpand => offset_pos(shell_complete(context, &token.content, pos_in_token), start),
        TokenType::ValExpand => Completions::with(
            start,
            cursor_pos,
            ShellManager::instance().complete_env_var(&token.content, pos_in_token),
            CompletionFlags::None,
        ),
        TokenType::FileExpand => {
            let ignored_files = context.options().val::<Regex>("ignored_files");
            Completions::with(
                start,
                cursor_pos,
                complete_filename(&token.content, &ignored_files, pos_in_token, FilenameFlags::Expand),
                CompletionFlags::None,
            )
        }
        _ => Completions::default(),
    }
}

fn complete_expand(context: &Context, prefix: &[u8], start: usize, cursor_pos: usize, pos_in_token: usize) -> Completions {
    let mut state = ParseState { str: prefix, pos: 0 };
    while !state.at_end() {
        let c = prefix[state.pos];
        state.pos += 1;
        if c == b'%' {
            if !state.at_end() && prefix[state.pos] == b'%' {
                state.pos += 1;
            } else {
                let Ok(token) = parse_percent_token(&mut state, false) else { return Completions::default() };
                if token.terminated {
                    continue;
                }
                if token.type_ == TokenType::Raw || token.type_ == TokenType::RawQuoted {
                    return Completions::default();
                }
                return complete_expansion(
                    context,
                    &token,
                    start + token.pos,
                    cursor_pos,
                    pos_in_token.wrapping_sub(token.pos),
                );
            }
        }
    }
    Completions::default()
}

fn requote(mut completions: Completions, token_type: TokenType) -> Completions {
    if completions.flags.has(CompletionFlags::Quoted) {
        return completions;
    }
    if token_type == TokenType::Raw {
        let at_token_start = completions.start == 0;
        for candidate in &mut completions.candidates {
            let to_escape: &[u8] = b";\n \t";
            if (at_token_start && candidate.first() == Some(&b'%')) || candidate.iter().any(|c| to_escape.contains(c)) {
                *candidate = if at_token_start { quote(candidate) } else { escape(candidate, to_escape, b'\\') };
            }
        }
    } else if token_type == TokenType::RawQuoted {
        completions.flags |= CompletionFlags::Quoted;
    }
    completions
}

/// Command line completer, keeps track of the last completed command
#[derive(Default)]
pub struct Completer {
    last_complete_command: RefCell<Vec<u8>>,
    command_completer: RefCell<Option<CommandCompleter>>,
}

impl Completer {
    pub fn complete(&self, context: &Context, command_line: &[u8], cursor_pos: usize) -> Completions {
        let prefix = &command_line[..cursor_pos.min(command_line.len())];
        let mut parser = CommandParser::new(prefix);
        let cursor = cursor_pos;
        let mut tokens: Vec<Token> = Vec::new();

        let mut is_last_token = true;
        while let Ok(Some(token)) = parser.read_token(false) {
            if token.type_ == TokenType::CommandSeparator {
                tokens.clear();
                continue;
            }
            tokens.push(token);
            if parser.pos() >= cursor {
                is_last_token = false;
                break;
            }
        }

        if is_last_token {
            tokens.push(Token { type_: TokenType::Raw, pos: prefix.len(), content: Vec::new(), terminated: false });
        }

        let token = tokens.last().unwrap().clone();
        if token.terminated {
            // do not complete past explicit token close
            return Completions::default();
        }

        let start = token.pos;
        let pos_in_token = cursor_pos - start;

        // command name completion
        if tokens.len() == 1 && (token.type_ == TokenType::Raw || token.type_ == TokenType::RawQuoted) {
            return offset_pos(
                requote(CommandManager::instance().complete_command_name(context, prefix), token.type_),
                start,
            );
        }

        match token.type_ {
            TokenType::RegisterExpand
            | TokenType::OptionExpand
            | TokenType::ShellExpand
            | TokenType::ValExpand
            | TokenType::FileExpand => complete_expansion(context, &token, start, cursor_pos, pos_in_token),
            TokenType::Raw | TokenType::RawQuoted => {
                let command_name = tokens[0].content.clone();
                let Some(command) = CommandManager::instance().get_command(&resolve_alias(context, &command_name)) else {
                    return Completions::default();
                };

                if command_name != *self.last_complete_command.borrow() {
                    *self.last_complete_command.borrow_mut() = command_name.clone();
                    *self.command_completer.borrow_mut() = command.completer.clone();
                }

                let raw_params: Vec<Vec<u8>> = tokens[1..].iter().map(|t| t.content.clone()).collect();
                let Ok(parser) = ParametersParser::new_ext(&raw_params, &command.param_desc, true) else {
                    return Completions::default();
                };
                match parser.state() {
                    Some(ParserState::Switch) => {
                        let mut keys: Vec<Vec<u8>> = command
                            .param_desc
                            .switches
                            .keys()
                            .filter(|key| !parser.switches_contains(key))
                            .cloned()
                            .collect();
                        keys.push(b"-".to_vec());
                        let content = &token.content[1.min(token.content.len())..];
                        let switches = complete(content, pos_in_token, &keys);
                        if switches.is_empty() {
                            Completions::default()
                        } else {
                            Completions::with(start + 1, cursor_pos, switches, CompletionFlags::Menu)
                        }
                    }
                    Some(ParserState::SwitchArgument) => {
                        let name = &raw_params[raw_params.len() - 2][1..];
                        let switch_desc = command.param_desc.switches.get(name).unwrap();
                        match &switch_desc.arg_completer {
                            Some(Some(completer)) => offset_pos(
                                requote(completer(context, raw_params.last().unwrap(), pos_in_token), token.type_),
                                start,
                            ),
                            _ => Completions::default(),
                        }
                    }
                    _ => {
                        let completer = self.command_completer.borrow().clone();
                        let Some(completer) = completer else { return Completions::default() };
                        let params = parser.positionals();
                        let index = params.len().wrapping_sub(1);
                        offset_pos(requote(completer(context, &params, index, pos_in_token), token.type_), start)
                    }
                }
            }
            TokenType::Expand => complete_expand(context, &token.content, start, cursor_pos, pos_in_token),
            _ => Completions::default(),
        }
    }
}

/// Completer for commands taking a command as parameters
#[derive(Default)]
pub struct NestedCompleter {
    last_complete_command: RefCell<Vec<u8>>,
    command_completer: RefCell<Option<CommandCompleter>>,
}

impl NestedCompleter {
    pub fn complete(&self, context: &Context, params: &[Vec<u8>], token_to_complete: usize, pos_in_token: usize) -> Completions {
        let token = &params[token_to_complete];
        let prefix = &token[..pos_in_token.min(token.len())];
        if token_to_complete == 0 {
            return CommandManager::instance().complete_command_name(context, prefix);
        }

        let command_name = &params[0];
        if *command_name != *self.last_complete_command.borrow() {
            *self.last_complete_command.borrow_mut() = command_name.clone();
            if let Some(cmd) = CommandManager::instance().get_command(&resolve_alias(context, command_name)) {
                *self.command_completer.borrow_mut() = cmd.completer.clone();
            }
        }
        let completer = self.command_completer.borrow().clone();
        match completer {
            Some(c) => c(context, &params[1..], token_to_complete - 1, pos_in_token),
            None => Completions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_parsing() {
        let check_quoted = |s: &[u8], terminated: bool, content: &[u8]| {
            for as_cp in [true, false] {
                let mut state = ParseState { str: s, pos: 1 };
                let quoted = parse_quoted(&mut state, s[0] as Codepoint, as_cp);
                assert_eq!(quoted.terminated, terminated);
                assert_eq!(quoted.content, content);
            }
        };
        check_quoted(b"'abc'", true, b"abc");
        check_quoted(b"'abc''def", false, b"abc'def");
        check_quoted(b"'abc''def'''", true, b"abc'def'");
        check_quoted(&b"'abc''def'"[..5], true, b"abc");

        let check_balanced = |s: &[u8], terminated: bool, content: &[u8]| {
            let mut state = ParseState { str: s, pos: 1 };
            let quoted = parse_quoted_balanced(&mut state, b'{', b'}');
            assert_eq!(quoted.terminated, terminated);
            assert_eq!(quoted.content, content);
        };
        check_balanced(b"{abc}", true, b"abc");
        check_balanced(b"{abc{def}}", true, b"abc{def}");
        check_balanced(b"{{abc}{def}", false, b"{abc}{def}");

        let check_unquoted = |s: &[u8], content: &[u8]| {
            let mut state = ParseState { str: s, pos: 0 };
            assert_eq!(parse_unquoted(&mut state), content);
        };
        check_unquoted(b"abc def", b"abc");
        check_unquoted(b"abc; def", b"abc");
        check_unquoted(b"abc\\; def", b"abc;");
        check_unquoted(b"abc\\;\\ def", b"abc; def");

        let mut parser = CommandParser::new(br#"foo 'bar' "baz" qux"#);
        assert_eq!(parser.read_token(false).unwrap().unwrap().content, b"foo");
        assert_eq!(parser.read_token(false).unwrap().unwrap().content, b"bar");
        assert_eq!(parser.read_token(false).unwrap().unwrap().content, b"baz");
        assert_eq!(parser.read_token(false).unwrap().unwrap().content, b"qux");
        assert!(parser.read_token(false).unwrap().is_none());
    }
}
