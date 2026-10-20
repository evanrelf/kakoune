//! Port of shell_manager.hh/cc

use crate::client::BusyIndicator;
use crate::command_manager::{CommandManager, ShellContext};
use crate::completion::{complete, CandidateList};
use crate::context::Context;
use crate::debug::{write_to_debug_buffer, DebugFlags};
use crate::display_buffer::DisplayLine;
use crate::event_manager::{EventManager, EventMode, FDWatcher, FdEvents};
use crate::exception::{Error, Result};
use crate::file::{cstr, fd_readable, fd_writable, get_kak_binary_path, last_error_str, split_path, tmpdir};
use crate::regex::{noop_idle, Regex, RegexIterator};
use crate::regex_vm::{RegexCompileFlags, RegexExecFlags};
use crate::string_utils::{join, quoter, shell_quote, Quoting};
use crate::utf8::SliceIt;
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub type EnvVarRetriever = fn(&[u8], &Context) -> Result<Vec<Vec<u8>>>;

pub struct EnvVarDesc {
    pub str: &'static str,
    pub prefix: bool,
    pub func: EnvVarRetriever,
}

/// An owned file descriptor that can be closed early
#[derive(Debug)]
pub struct UniqueFd {
    fd: Cell<i32>,
}

impl UniqueFd {
    pub fn new(fd: i32) -> Self {
        UniqueFd { fd: Cell::new(fd) }
    }
    pub fn none() -> Self {
        UniqueFd { fd: Cell::new(-1) }
    }
    pub fn fd(&self) -> i32 {
        self.fd.get()
    }
    pub fn is_open(&self) -> bool {
        self.fd.get() != -1
    }
    pub fn close(&self) {
        let fd = self.fd.replace(-1);
        if fd != -1 {
            unsafe { libc::close(fd) };
        }
    }
    /// Give up ownership of the descriptor
    pub fn release(&self) -> i32 {
        self.fd.replace(-1)
    }
}

impl Drop for UniqueFd {
    fn drop(&mut self) {
        self.close();
    }
}

/// An owned child process, killed and reaped on drop
#[derive(Debug)]
pub struct UniquePid {
    pid: Cell<i32>,
}

impl UniquePid {
    pub fn new(pid: i32) -> Self {
        UniquePid { pid: Cell::new(pid) }
    }
    pub fn pid(&self) -> i32 {
        self.pid.get()
    }
    pub fn release(&self) -> i32 {
        self.pid.replace(-1)
    }
}

impl Drop for UniquePid {
    fn drop(&mut self) {
        let pid = self.pid.get();
        if pid > 0 {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
                let mut status = 0;
                libc::waitpid(pid, &mut status, 0);
            }
        }
    }
}

pub struct Shell {
    pub pid: UniquePid,
    pub in_: Rc<UniqueFd>,
    pub out: Rc<UniqueFd>,
    pub err: Rc<UniqueFd>,
}

crate::kak_flags! {
    pub struct ShellFlags: u32 {
        const None = 0;
        const WaitForStdout = 1;
    }
}

pub struct ShellManager {
    shell: Vec<u8>,
    env_vars: &'static [EnvVarDesc],
}

thread_local! {
    static SHELL_MANAGER: OnceCell<&'static ShellManager> = const { OnceCell::new() };
}

fn is_executable(path: &[u8]) -> bool {
    let c = cstr(path);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(c.as_ptr(), &mut st) } != 0 {
        return false;
    }
    let executable = (st.st_mode & (libc::S_IXUSR | libc::S_IXGRP | libc::S_IXOTH)) != 0;
    (st.st_mode & libc::S_IFMT) == libc::S_IFREG && executable
}

fn posix_path() -> Vec<u8> {
    unsafe {
        let size = libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0);
        if size == 0 {
            return b"/bin:/usr/bin".to_vec();
        }
        let mut buf = vec![0u8; size];
        libc::confstr(libc::_CS_PATH, buf.as_mut_ptr() as *mut libc::c_char, size);
        buf.truncate(size - 1);
        buf
    }
}

impl ShellManager {
    pub fn create(builtin_env_vars: &'static [EnvVarDesc]) -> Result<&'static ShellManager> {
        let shell = if let Some(shell) = crate::file::getenv("KAKOUNE_POSIX_SHELL") {
            if !is_executable(&shell) {
                return Err(rt_err!("KAKOUNE_POSIX_SHELL '{}' is not executable", shell));
            }
            shell
        } else {
            // Get a guaranteed to be POSIX shell binary
            let path = posix_path();
            let mut found = None;
            for dir in crate::string_utils::split(&path, b':') {
                let candidate = kformat!("{}/sh", dir);
                if is_executable(&candidate) {
                    found = Some(candidate);
                    break;
                }
            }
            match found {
                Some(s) => s,
                None => return Err(rt_err!("unable to find a posix shell in {}", path)),
            }
        };

        // Add Kakoune binary location to the path to guarantee that %sh{ ... }
        // have access to the kak command regardless of if the user installed it
        {
            let path = crate::file::getenv("PATH").unwrap_or_default();
            let binary = get_kak_binary_path().unwrap_or_default();
            let new_path = kformat!("{}../libexec/kak:{}", split_path(&binary).0, path);
            use std::os::unix::ffi::OsStrExt;
            // SAFETY: single threaded program
            unsafe { std::env::set_var("PATH", std::ffi::OsStr::from_bytes(&new_path)) };
        }

        let sm: &'static ShellManager = Box::leak(Box::new(ShellManager { shell, env_vars: builtin_env_vars }));
        SHELL_MANAGER.with(|s| {
            let _ = s.set(sm);
        });
        Ok(sm)
    }

    pub fn instance() -> &'static ShellManager {
        SHELL_MANAGER.with(|s| *s.get().expect("ShellManager not created"))
    }

    pub fn has_instance() -> bool {
        SHELL_MANAGER.with(|s| s.get().is_some())
    }

    pub fn eval(
        &self,
        cmdline: &[u8],
        context: &Context,
        input: &[u8],
        flags: ShellFlags,
        shell_context: &ShellContext,
    ) -> Result<(Vec<u8>, i32)> {
        let mut input = Some(input.to_vec());
        self.eval_with_generator(cmdline, context, Box::new(move || input.take().unwrap_or_default()), flags, shell_context)
    }

    /// eval with an input generator, called repeatedly until it returns an empty string
    pub fn eval_with_generator(
        &self,
        cmdline: &[u8],
        context: &Context,
        input_generator: Box<dyn FnMut() -> Vec<u8>>,
        flags: ShellFlags,
        shell_context: &ShellContext,
    ) -> Result<(Vec<u8>, i32)> {
        let debug_flags = context.options().val::<DebugFlags>("debug");
        let profile = debug_flags.has(DebugFlags::Profile);
        if debug_flags.has(DebugFlags::Shell) {
            write_to_debug_buffer(&kformat!(
                "shell:\n{}\n----\nargs: {}\n----\n",
                cmdline,
                crate::string_utils::join_str(shell_context.params.iter().map(|p| shell_quote(p)), b" ")
            ));
        }

        let start_time = Instant::now();

        let command_fifos: RefCell<Option<CommandFifos>> = RefCell::new(None);
        let kak_env = generate_env(cmdline, &shell_context.params, |name, quoting| {
            if name == b"command_fifo" || name == b"response_fifo" {
                let mut fifos = command_fifos.borrow_mut();
                if fifos.is_none() {
                    *fifos = Some(CommandFifos::new(context, shell_context)?);
                }
                let fifos = fifos.as_ref().unwrap();
                return Ok(if name == b"command_fifo" { fifos.command_fifo_path() } else { fifos.response_fifo_path() });
            }
            if let Some(v) = shell_context.env_vars.get(name) {
                return Ok(v.clone());
            }
            let val = self.get_val(name, context)?;
            Ok(crate::string_utils::join_str(val.iter().map(|v| quoter(quoting)(v)), b" "))
        });

        let spawn_time = Instant::now();
        let shell = spawn_shell(&self.shell, cmdline, &shell_context.params, &kak_env, true)?;
        let wait_time = Instant::now();

        let stdout_contents = Rc::new(RefCell::new(Vec::new()));
        let stderr_contents = Rc::new(RefCell::new(Vec::new()));
        let out_fd = shell.out.clone();
        let err_fd = shell.err.clone();
        let stdout_reader = make_reader(shell.out.fd(), stdout_contents.clone(), move |_| {
            out_fd.close();
            Ok(())
        });
        let stderr_reader = make_reader(shell.err.fd(), stderr_contents.clone(), move |_| {
            err_fd.close();
            Ok(())
        });
        let stdin_writer = make_pipe_writer(shell.in_.clone(), input_generator);

        // block SIGCHLD to make sure we wont receive it before
        // our call to pselect, that will end up blocking indefinitly.
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut orig_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGCHLD);
            libc::sigprocmask(libc::SIG_BLOCK, &mask, &mut orig_mask);
        }
        let _restore_mask = crate::utils::OnScopeEnd::new(|| unsafe {
            libc::sigprocmask(libc::SIG_SETMASK, &orig_mask, std::ptr::null_mut());
        });

        let mut status = 0;
        // check for termination now that SIGCHLD is blocked
        let terminated = Rc::new(Cell::new(unsafe { libc::waitpid(shell.pid.pid(), &mut status, libc::WNOHANG) } != 0));
        let failed = Rc::new(Cell::new(false));

        let busy_indicator = {
            let terminated = terminated.clone();
            let failed = failed.clone();
            let faces = context.faces();
            BusyIndicator::new(
                context,
                Box::new(move |elapsed: Duration| {
                    DisplayLine::from_str_face(
                        kformat!(
                            "waiting for shell command to finish{} ({}s)",
                            if terminated.get() { " (shell terminated)" } else { "" },
                            elapsed.as_secs()
                        ),
                        faces.face(if failed.get() { "Error" } else { "Information" }),
                    )
                }),
                Instant::now(),
            )
        };

        let mut cancelling = false;
        while !terminated.get()
            || shell.in_.is_open()
            || (flags.has(ShellFlags::WaitForStdout) && (shell.out.is_open() || shell.err.is_open()))
        {
            match EventManager::instance().try_handle_next_events(EventMode::Urgent, Some(&orig_mask), None) {
                Ok(_) => {}
                Err(e) if e.is_cancel() => {
                    unsafe { libc::kill(shell.pid.pid(), libc::SIGINT) };
                    cancelling = true;
                }
                Err(e) if e.is_runtime() => {
                    write_to_debug_buffer(&kformat!("error while waiting for shell: {}", e.what()));
                    failed.set(true);
                }
                Err(e) => return Err(e),
            }
            if !terminated.get() {
                terminated.set(unsafe { libc::waitpid(shell.pid.pid(), &mut status, libc::WNOHANG) } == shell.pid.pid());
            }
        }
        // the process has been reaped
        shell.pid.release();
        if cancelling {
            busy_indicator.skip_restore();
        }
        drop(busy_indicator);
        drop(stdout_reader);
        drop(stderr_reader);
        drop(stdin_writer);
        drop(command_fifos);

        let stderr_contents = stderr_contents.borrow();
        if !stderr_contents.is_empty() {
            write_to_debug_buffer(&kformat!("shell stderr: <<<\n{}>>>", *stderr_contents));
        }

        if profile {
            let end_time = Instant::now();
            let full = end_time - start_time;
            let spawn = wait_time - spawn_time;
            let wait = end_time - wait_time;
            write_to_debug_buffer(&kformat!(
                "shell execution took {} us (spawn: {}, wait: {})",
                full.as_micros() as usize,
                spawn.as_micros() as usize,
                wait.as_micros() as usize
            ));
        }

        if cancelling {
            return Err(Error::cancel());
        }

        let exit = if libc::WIFEXITED(status) { libc::WEXITSTATUS(status) } else { -1 };
        let out = std::mem::take(&mut *stdout_contents.borrow_mut());
        Ok((out, exit))
    }

    pub fn spawn(&self, cmdline: &[u8], context: &Context, open_stdin: bool, shell_context: &ShellContext) -> Result<Shell> {
        let kak_env = generate_env(cmdline, &shell_context.params, |name, quoting| {
            if let Some(v) = shell_context.env_vars.get(name) {
                return Ok(v.clone());
            }
            let val = self.get_val(name, context)?;
            Ok(crate::string_utils::join_str(val.iter().map(|v| quoter(quoting)(v)), b" "))
        });
        spawn_shell(&self.shell, cmdline, &shell_context.params, &kak_env, open_stdin)
    }

    pub fn get_val(&self, name: &[u8], context: &Context) -> Result<Vec<Vec<u8>>> {
        let env_var = self.env_vars.iter().find(|desc| {
            if desc.prefix { name.starts_with(desc.str.as_bytes()) } else { name == desc.str.as_bytes() }
        });
        match env_var {
            Some(desc) => (desc.func)(name, context),
            None => {
                let mut msg = b"no such variable: ".to_vec();
                msg.extend_from_slice(name);
                Err(Error::runtime(msg))
            }
        }
    }

    pub fn complete_env_var(&self, prefix: &[u8], cursor_pos: usize) -> CandidateList {
        complete(prefix, cursor_pos, self.env_vars.iter().map(|d| d.str.as_bytes()))
    }
}

fn make_pipe() -> Result<(UniqueFd, UniqueFd)> {
    let mut pipefd = [-1i32; 2];
    if unsafe { libc::pipe(pipefd.as_mut_ptr()) } == 0 {
        return Ok((UniqueFd::new(pipefd[0]), UniqueFd::new(pipefd[1])));
    }
    Err(rt_err!("unable to create pipe, errno: {}", last_error_str()))
}

fn spawn_shell(shell: &[u8], cmdline: &[u8], params: &[Vec<u8>], kak_env: &[Vec<u8>], open_stdin: bool) -> Result<Shell> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let mut env_strings: Vec<CString> = Vec::new();
    for (k, v) in std::env::vars_os() {
        let mut s = k.as_bytes().to_vec();
        s.push(b'=');
        s.extend_from_slice(v.as_bytes());
        env_strings.push(cstr(&s));
    }
    for env in kak_env {
        env_strings.push(cstr(env));
    }
    let mut envptrs: Vec<*const libc::c_char> = env_strings.iter().map(|s| s.as_ptr()).collect();
    envptrs.push(std::ptr::null());

    let shell_c = cstr(shell);
    let mut exec_strings: Vec<CString> = vec![cstr(b"sh"), cstr(b"-c"), cstr(cmdline)];
    if !params.is_empty() {
        exec_strings.push(shell_c.clone());
    }
    for param in params {
        exec_strings.push(cstr(param));
    }
    let mut execparams: Vec<*const libc::c_char> = exec_strings.iter().map(|s| s.as_ptr()).collect();
    execparams.push(std::ptr::null());

    let stdin_pipe = if open_stdin {
        make_pipe()?
    } else {
        let devnull = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
        (UniqueFd::new(devnull), UniqueFd::none())
    };
    let stdout_pipe = make_pipe()?;
    let stderr_pipe = make_pipe()?;

    let error_prefix = b"execve failed: ";

    let pid = unsafe { libc::fork() };
    if pid != 0 {
        return Ok(Shell {
            pid: UniquePid::new(pid),
            in_: Rc::new(UniqueFd::new(stdin_pipe.1.release())),
            out: Rc::new(UniqueFd::new(stdout_pipe.0.release())),
            err: Rc::new(UniqueFd::new(stderr_pipe.0.release())),
        });
    }

    // Child process: only async-signal-safe calls from here
    unsafe {
        let renamefd = |oldfd: i32, newfd: i32| {
            if oldfd == newfd {
                return;
            }
            libc::dup2(oldfd, newfd);
            libc::close(oldfd);
        };
        renamefd(stdin_pipe.0.fd(), 0);
        renamefd(stdout_pipe.1.fd(), 1);
        renamefd(stderr_pipe.1.fd(), 2);
        if stdin_pipe.1.fd() != -1 {
            libc::close(stdin_pipe.1.fd());
        }
        libc::close(stdout_pipe.0.fd());
        libc::close(stderr_pipe.0.fd());

        libc::execve(shell_c.as_ptr(), execparams.as_ptr(), envptrs.as_ptr());
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let msg = libc::strerror(err);
        libc::write(2, error_prefix.as_ptr() as *const libc::c_void, error_prefix.len());
        libc::write(2, msg as *const libc::c_void, libc::strlen(msg));
        libc::write(2, b"\n".as_ptr() as *const libc::c_void, 1);
        libc::_exit(-1);
    }
}

fn generate_env(
    cmdline: &[u8],
    params: &[Vec<u8>],
    mut get_value: impl FnMut(&[u8], Quoting) -> Result<Vec<u8>>,
) -> Vec<Vec<u8>> {
    thread_local! {
        static RE: Regex = Regex::new(br"\bkak_(quoted_)?(\w+)\b", RegexCompileFlags::NONE).unwrap();
    }
    let re = RE.with(|r| r.clone());
    let mut env: Vec<Vec<u8>> = Vec::new();
    let mut add_matches = |s: &[u8]| {
        let mut it = RegexIterator::<_, crate::regex_vm::ForwardSearch>::new_range(SliceIt::new(s, 0), SliceIt::new(s, s.len()), &re, RegexExecFlags::NONE);
        while let Some(m) = it.next_match() {
            let range = |i: usize| -> (usize, usize) {
                (m[i].first.map_or(0, |it| it.pos), m[i].second.map_or(0, |it| it.pos))
            };
            let (nb, ne) = range(2);
            let name = &s[nb..ne];
            let (sb, se) = range(0);
            let shell_name = &s[sb..se];
            let match_name =
                |e: &Vec<u8>| e.starts_with(shell_name) && e.get(shell_name.len()) == Some(&b'=');
            if env.iter().any(match_name) {
                continue;
            }
            let (qb, qe) = range(1);
            let quoted = if m[1].matched { &s[qb..qe] } else { &[][..] };
            let quoting = if m[1].matched { Quoting::Shell } else { Quoting::Raw };
            if let Ok(value) = get_value(name, quoting) {
                env.push(kformat!("kak_{}{}={}", quoted, name, value));
            }
        }
    };
    add_matches(cmdline);
    for param in params {
        add_matches(param);
    }
    let _ = noop_idle;
    env
}

fn make_reader(fd: i32, contents: Rc<RefCell<Vec<u8>>>, on_close: impl Fn(bool) -> Result<()> + 'static) -> FDWatcher {
    FDWatcher::new(fd, FdEvents::Read, EventMode::Urgent, move |watcher, _, _| {
        let fd = watcher.fd();
        let mut buffer = [0u8; 1024];
        while fd_readable(fd) {
            let size = unsafe { libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len()) };
            if size <= 0 {
                if size < 0 && crate::file::errno() == libc::EAGAIN {
                    continue; // try again
                }
                watcher.disable();
                return on_close(size == 0);
            }
            contents.borrow_mut().extend_from_slice(&buffer[..size as usize]);
        }
        Ok(())
    })
}

fn make_pipe_writer(fd: Rc<UniqueFd>, mut generator: Box<dyn FnMut() -> Vec<u8>>) -> FDWatcher {
    let raw = fd.fd();
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL, 0);
        libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let first = generator();
    let state = RefCell::new((first, 0usize, generator));
    FDWatcher::new(raw, FdEvents::Write, EventMode::Urgent, move |watcher, _, _| {
        let mut state = state.borrow_mut();
        let (contents, pos, generator) = &mut *state;
        while fd_writable(fd.fd()) {
            let remaining = &contents[*pos..];
            let size = unsafe { libc::write(fd.fd(), remaining.as_ptr() as *const libc::c_void, remaining.len()) };
            if size > 0 {
                *pos += size as usize;
                if *pos == contents.len() {
                    *contents = generator();
                    *pos = 0;
                }
            }
            let err = crate::file::errno();
            if size == -1 && (err == libc::EAGAIN || err == libc::EWOULDBLOCK) {
                return Ok(());
            }
            if size < 0 || *pos == contents.len() {
                fd.close();
                watcher.disable();
                return Ok(());
            }
        }
        Ok(())
    })
}

struct CommandFifos {
    base_dir: Vec<u8>,
    command_watcher: FDWatcher,
}

impl CommandFifos {
    fn new(context: &Context, shell_context: &ShellContext) -> Result<Self> {
        let mut base_dir = kformat!("{}/kak-fifo.XXXXXX", tmpdir());
        base_dir.push(0);
        let ok = unsafe { !libc::mkdtemp(base_dir.as_mut_ptr() as *mut libc::c_char).is_null() };
        base_dir.pop();
        let command_path = kformat!("{}/command-fifo", base_dir);
        let response_path = kformat!("{}/response-fifo", base_dir);
        if !ok
            || unsafe { libc::mkfifo(cstr(&command_path).as_ptr(), 0o600) } != 0
            || unsafe { libc::mkfifo(cstr(&response_path).as_ptr(), 0o600) } != 0
        {
            return Err(rt_err!("unable to create command/response fifos, errno: {}", last_error_str()));
        }
        let fd = unsafe { libc::open(cstr(&command_path).as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };

        // SAFETY: CommandFifos only lives for the duration of ShellManager::eval,
        // which borrows the context, and the watcher is dropped with it, so the
        // pointers are valid whenever the callback runs.
        let context_ptr = context as *const Context;
        let shell_context = shell_context.clone();
        let command = Rc::new(RefCell::new(Vec::new()));
        let command_for_close = command.clone();
        let reader_state: Rc<Cell<Option<crate::event_manager::WatcherKey>>> = Rc::new(Cell::new(None));
        let reader_key = reader_state.clone();
        let command_watcher = make_reader(fd, command, move |graceful| {
            if !graceful {
                write_to_debug_buffer(&kformat!("error reading from command fifo '{}'", last_error_str()));
                return Ok(());
            }
            let context = unsafe { &*context_ptr };
            let cmd = std::mem::take(&mut *command_for_close.borrow_mut());
            CommandManager::instance().execute(&cmd, context, &shell_context)?;
            if let Some(key) = reader_key.get() {
                crate::event_manager::watcher_reset_fd(key, fd);
            }
            Ok(())
        });
        reader_state.set(Some(command_watcher.key()));
        Ok(CommandFifos { base_dir, command_watcher })
    }

    fn command_fifo_path(&self) -> Vec<u8> {
        kformat!("{}/command-fifo", self.base_dir)
    }

    fn response_fifo_path(&self) -> Vec<u8> {
        kformat!("{}/response-fifo", self.base_dir)
    }
}

impl Drop for CommandFifos {
    fn drop(&mut self) {
        self.command_watcher.close_fd();
        unsafe {
            libc::unlink(cstr(&self.command_fifo_path()).as_ptr());
            libc::unlink(cstr(&self.response_fifo_path()).as_ptr());
            libc::rmdir(cstr(&self.base_dir).as_ptr());
        }
    }
}

/// Helper used by other modules
pub fn shell_join_quoted(values: &[Vec<u8>], quoting: Quoting) -> Vec<u8> {
    join(values.iter().map(|v| quoter(quoting)(v)), b' ', false)
}
