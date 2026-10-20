//! Port of hook_manager.hh/cc

use crate::command_manager::{CommandManager, ShellContext};
use crate::completion::{complete, CandidateList};
use crate::context::Context;
use crate::debug::{write_to_debug_buffer, DebugFlags, EnvVarMap, ProfileScope};
use crate::display_buffer::DisplayLine;
use crate::exception::Result;
use crate::option_types::EnumDesc;
use crate::regex::{regex_match_ranges, regex_match_str, Regex};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Hook {
    BufCreate,
    BufNewFile,
    BufOpenFile,
    BufClose,
    BufWritePost,
    BufReload,
    BufWritePre,
    BufOpenFifo,
    BufCloseFifo,
    BufReadFifo,
    BufSetOption,
    ClientCreate,
    ClientClose,
    ClientRenamed,
    SessionRenamed,
    InsertChar,
    InsertDelete,
    InsertIdle,
    InsertKey,
    InsertMove,
    InsertCompletionHide,
    InsertCompletionShow,
    KakBegin,
    KakEnd,
    FocusIn,
    FocusOut,
    GlobalSetOption,
    RuntimeError,
    PromptIdle,
    NormalIdle,
    NextKeyIdle,
    NormalKey,
    ModeChange,
    EnterDirectory,
    RawKey,
    RegisterModified,
    WinClose,
    WinCreate,
    WinDisplay,
    WinResize,
    WinSetOption,
    ModuleLoaded,
    User,
}

pub const HOOK_DESC: &[(Hook, &str)] = &[
    (Hook::BufCreate, "BufCreate"),
    (Hook::BufNewFile, "BufNewFile"),
    (Hook::BufOpenFile, "BufOpenFile"),
    (Hook::BufClose, "BufClose"),
    (Hook::BufWritePost, "BufWritePost"),
    (Hook::BufReload, "BufReload"),
    (Hook::BufWritePre, "BufWritePre"),
    (Hook::BufOpenFifo, "BufOpenFifo"),
    (Hook::BufCloseFifo, "BufCloseFifo"),
    (Hook::BufReadFifo, "BufReadFifo"),
    (Hook::BufSetOption, "BufSetOption"),
    (Hook::ClientCreate, "ClientCreate"),
    (Hook::ClientClose, "ClientClose"),
    (Hook::ClientRenamed, "ClientRenamed"),
    (Hook::SessionRenamed, "SessionRenamed"),
    (Hook::InsertChar, "InsertChar"),
    (Hook::InsertDelete, "InsertDelete"),
    (Hook::InsertIdle, "InsertIdle"),
    (Hook::InsertKey, "InsertKey"),
    (Hook::InsertMove, "InsertMove"),
    (Hook::InsertCompletionHide, "InsertCompletionHide"),
    (Hook::InsertCompletionShow, "InsertCompletionShow"),
    (Hook::KakBegin, "KakBegin"),
    (Hook::KakEnd, "KakEnd"),
    (Hook::FocusIn, "FocusIn"),
    (Hook::FocusOut, "FocusOut"),
    (Hook::GlobalSetOption, "GlobalSetOption"),
    (Hook::RuntimeError, "RuntimeError"),
    (Hook::PromptIdle, "PromptIdle"),
    (Hook::NormalIdle, "NormalIdle"),
    (Hook::NextKeyIdle, "NextKeyIdle"),
    (Hook::NormalKey, "NormalKey"),
    (Hook::ModeChange, "ModeChange"),
    (Hook::EnterDirectory, "EnterDirectory"),
    (Hook::RawKey, "RawKey"),
    (Hook::RegisterModified, "RegisterModified"),
    (Hook::WinClose, "WinClose"),
    (Hook::WinCreate, "WinCreate"),
    (Hook::WinDisplay, "WinDisplay"),
    (Hook::WinResize, "WinResize"),
    (Hook::WinSetOption, "WinSetOption"),
    (Hook::ModuleLoaded, "ModuleLoaded"),
    (Hook::User, "User"),
];

impl EnumDesc for Hook {
    fn desc() -> &'static [(Self, &'static str)] {
        HOOK_DESC
    }
}

impl Hook {
    pub fn name(self) -> &'static str {
        HOOK_DESC[self as usize].1
    }
}

crate::kak_flags! {
    pub struct HookFlags: u32 {
        const None = 0;
        const Always = 1 << 0;
        const Once = 1 << 1;
    }
}

pub struct HookData {
    pub group: Vec<u8>,
    pub flags: HookFlags,
    pub filter: Regex,
    pub commands: Vec<u8>,
}

/// captures as optional byte ranges into the hook parameter
type Captures = Vec<Option<(usize, usize)>>;

impl HookData {
    fn should_run(&self, only_always: bool, disabled_hooks: &Regex, param: &[u8]) -> Option<Captures> {
        if only_always && !self.flags.has(HookFlags::Always) {
            return None;
        }
        if !(self.group.is_empty() || disabled_hooks.empty() || !regex_match_str(&self.group, disabled_hooks)) {
            return None;
        }
        regex_match_ranges(param, &self.filter)
    }

    fn exec(&self, hook: Hook, param: &[u8], context: &Context, captures: &Captures) -> Result<()> {
        if context.options().val::<DebugFlags>("debug").has(DebugFlags::Hooks) {
            write_to_debug_buffer(&kformat!("hook {}({})/{}", hook.name(), param, self.group));
        }
        let mut env_vars = EnvVarMap::new();
        env_vars.insert(b"hook_param".to_vec(), param.to_vec());
        let sub = |c: &Option<(usize, usize)>| match c {
            Some((b, e)) => param[*b..*e].to_vec(),
            None => Vec::new(),
        };
        for (i, c) in captures.iter().enumerate() {
            env_vars.insert(kformat!("hook_param_capture_{}", i), sub(c));
        }
        for (name, index) in self.filter.named_captures() {
            let value = captures.get(index).map(sub).unwrap_or_default();
            env_vars.insert(kformat!("hook_param_capture_{}", name), value);
        }
        CommandManager::instance().execute(&self.commands, context, &ShellContext { params: Vec::new(), env_vars })
    }
}

const HOOK_COUNT: usize = HOOK_DESC.len();

pub struct HookManager {
    parent: RefCell<Option<Rc<HookManager>>>,
    hooks: RefCell<Vec<Vec<Rc<HookData>>>>,
    running_hooks: RefCell<Vec<(Hook, Vec<u8>)>>,
    hooks_trash: RefCell<Vec<Rc<HookData>>>,
}

impl HookManager {
    pub fn new_root() -> HookManager {
        HookManager {
            parent: RefCell::new(None),
            hooks: RefCell::new(vec![Vec::new(); HOOK_COUNT]),
            running_hooks: RefCell::new(Vec::new()),
            hooks_trash: RefCell::new(Vec::new()),
        }
    }

    pub fn new(parent: &Rc<HookManager>) -> HookManager {
        let res = HookManager::new_root();
        *res.parent.borrow_mut() = Some(parent.clone());
        res
    }

    pub fn reparent(&self, parent: &Rc<HookManager>) {
        *self.parent.borrow_mut() = Some(parent.clone());
    }

    pub fn add_hook(
        &self,
        hook: Hook,
        group: Vec<u8>,
        flags: HookFlags,
        filter: Regex,
        commands: Vec<u8>,
        context: &Context,
    ) -> Result<()> {
        let hook_data = Rc::new(HookData { group, flags, filter, commands });
        if hook == Hook::ModuleLoaded {
            let only_always = context.hooks_disabled().is_set();
            let disabled_hooks = context.options().val::<Regex>("disabled_hooks");
            for name in CommandManager::instance().loaded_modules() {
                if let Some(captures) = hook_data.should_run(only_always, &disabled_hooks, &name) {
                    hook_data.exec(hook, &name, context, &captures)?;
                    if hook_data.flags.has(HookFlags::Once) {
                        return Ok(());
                    }
                }
            }
        }
        self.hooks.borrow_mut()[hook as usize].push(hook_data);
        Ok(())
    }

    pub fn remove_hooks(&self, regex: &Regex) {
        let mut hooks = self.hooks.borrow_mut();
        let mut trash = self.hooks_trash.borrow_mut();
        for list in hooks.iter_mut() {
            list.retain(|h| {
                if !regex_match_str(&h.group, regex) {
                    return true;
                }
                trash.push(h.clone());
                false
            });
        }
    }

    pub fn complete_hook_group(&self, prefix: &[u8], pos_in_token: usize) -> CandidateList {
        let mut res = CandidateList::new();
        for list in self.hooks.borrow().iter() {
            let groups: Vec<&[u8]> = list.iter().map(|h| h.group.as_slice()).collect();
            for c in complete(prefix, pos_in_token, groups) {
                if !res.contains(&c) {
                    res.push(c);
                }
            }
        }
        res
    }

    pub fn run_hook(&self, hook: Hook, param: &[u8], context: &Context) -> Result<()> {
        let only_always = context.hooks_disabled().is_set();
        let disabled_hooks = context.options().val::<Regex>("disabled_hooks");

        let mut hooks_to_run: Vec<(Rc<HookData>, Captures)> = Vec::new();
        let candidates: Vec<Rc<HookData>> = self.hooks.borrow()[hook as usize].clone();
        for h in candidates {
            if let Some(captures) = h.should_run(only_always, &disabled_hooks, param) {
                hooks_to_run.push((h, captures));
            }
        }

        let hook_name = hook.name();
        if self.running_hooks.borrow().iter().any(|(h, p)| *h == hook && p == param) {
            let error = kformat!("recursive call of hook {}/{}, not executing", hook_name, param);
            write_to_debug_buffer(&error);
            return Ok(());
        }

        self.running_hooks.borrow_mut().push((hook, param.to_vec()));
        let res = self.run_hooks_inner(hook, param, context, hooks_to_run);
        self.running_hooks.borrow_mut().pop();
        if self.running_hooks.borrow().is_empty() {
            self.hooks_trash.borrow_mut().clear();
        }
        res
    }

    fn run_hooks_inner(
        &self,
        hook: Hook,
        param: &[u8],
        context: &Context,
        hooks_to_run: Vec<(Rc<HookData>, Captures)>,
    ) -> Result<()> {
        let hook_name = hook.name();
        let parent = self.parent.borrow().clone();
        if let Some(parent) = parent {
            parent.run_hook(hook, param, context)?;
        }

        let _profile = ProfileScope::new(
            context.options().val::<DebugFlags>("debug"),
            |duration| {
                write_to_debug_buffer(&kformat!(
                    "hook '{}({})' took {} us",
                    hook_name,
                    param,
                    duration.as_micros() as usize
                ))
            },
            true,
        );

        let mut hook_error = false;
        for (to_run, captures) in &hooks_to_run {
            if self.hooks_trash.borrow().iter().any(|h| Rc::ptr_eq(h, to_run)) {
                continue;
            }
            match to_run.exec(hook, param, context, captures) {
                Ok(()) => {
                    if to_run.flags.has(HookFlags::Once) {
                        let mut hooks = self.hooks.borrow_mut();
                        let list = &mut hooks[hook as usize];
                        if let Some(pos) = list.iter().position(|h| Rc::ptr_eq(h, to_run)) {
                            let removed = list.remove(pos);
                            self.hooks_trash.borrow_mut().push(removed);
                        }
                    }
                }
                Err(err) if err.is_runtime() => {
                    hook_error = true;
                    write_to_debug_buffer(&kformat!(
                        "error running hook {}({})/{}: {}",
                        hook_name,
                        param,
                        to_run.group,
                        err.what()
                    ));
                }
                Err(err) => return Err(err),
            }
        }

        if hook_error {
            context.print_status(DisplayLine::from_str_face(
                kformat!("Error running hooks for '{}' '{}', see *debug* buffer", hook_name, param),
                context.faces().face("Error"),
            ));
        }
        Ok(())
    }
}
