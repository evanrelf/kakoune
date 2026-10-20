//! Port of event_manager.hh/cc and clock.hh
//!
//! Watchers and timers are stored in slot maps owned by the event manager.
//! The `FDWatcher` and `Timer` handles own their registration and remove it
//! when dropped. Callbacks are `Fn` closures held in `Rc`, so that they can
//! be called re-entrantly and survive the destruction of their handle while
//! running.

use crate::exception::Result;
use slotmap::{new_key_type, SlotMap};
use std::cell::{Cell, OnceCell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventMode {
    Normal,
    Urgent,
}

crate::kak_flags! {
    pub struct FdEvents: u32 {
        const None = 0;
        const Read = 1 << 0;
        const Write = 1 << 1;
        const Except = 1 << 2;
    }
}

/// A point in time, None meaning never (C++ TimePoint::max())
pub type TimePoint = Option<Instant>;

new_key_type! {
    pub struct WatcherKey;
    pub struct TimerKey;
}

pub type FdCallback = Rc<dyn Fn(&WatcherRef, FdEvents, EventMode) -> Result<()>>;
pub type TimerCallback = Rc<dyn Fn(&TimerRef) -> Result<()>>;

struct WatcherData {
    fd: i32,
    events: FdEvents,
    mode: EventMode,
    callback: FdCallback,
}

struct TimerData {
    date: TimePoint,
    mode: EventMode,
    callback: TimerCallback,
}

pub struct EventManager {
    watchers: RefCell<SlotMap<WatcherKey, WatcherData>>,
    timers: RefCell<SlotMap<TimerKey, TimerData>>,
}

// Forced fds can be set from signal handlers, so they live in lock free globals
const FORCED_WORDS: usize = 16; // fds 0..1024
static FORCED_FDS: [AtomicU64; FORCED_WORDS] = [const { AtomicU64::new(0) }; FORCED_WORDS];
static HAS_FORCED_FD: AtomicBool = AtomicBool::new(false);

fn take_forced_fds() -> [u64; FORCED_WORDS] {
    let mut res = [0u64; FORCED_WORDS];
    for (i, w) in FORCED_FDS.iter().enumerate() {
        res[i] = w.swap(0, Ordering::SeqCst);
    }
    res
}

fn is_forced(forced: &[u64; FORCED_WORDS], fd: i32) -> bool {
    let fd = fd as usize;
    fd < FORCED_WORDS * 64 && (forced[fd / 64] >> (fd % 64)) & 1 != 0
}

thread_local! {
    static EVENT_MANAGER: OnceCell<&'static EventManager> = const { OnceCell::new() };
}

impl EventManager {
    pub fn create() -> &'static EventManager {
        let em: &'static EventManager = Box::leak(Box::new(EventManager {
            watchers: RefCell::new(SlotMap::with_key()),
            timers: RefCell::new(SlotMap::with_key()),
        }));
        EVENT_MANAGER.with(|e| {
            let _ = e.set(em);
        });
        em
    }

    pub fn instance() -> &'static EventManager {
        EVENT_MANAGER.with(|e| *e.get().expect("EventManager not created"))
    }

    pub fn has_instance() -> bool {
        EVENT_MANAGER.with(|e| e.get().is_some())
    }

    /// blocks until next event if no timeout given, errors from callbacks are ignored
    pub fn handle_next_events(&self, mode: EventMode, sigmask: Option<&libc::sigset_t>, timeout: Option<Duration>) -> bool {
        self.try_handle_next_events(mode, sigmask, timeout).unwrap_or(true)
    }

    /// blocks until next event if no timeout given, errors from callbacks
    /// abort processing and are returned
    pub fn try_handle_next_events(
        &self,
        mode: EventMode,
        sigmask: Option<&libc::sigset_t>,
        mut timeout: Option<Duration>,
    ) -> Result<bool> {
        let mut max_fd = 0;
        let mut rfds: libc::fd_set = unsafe { std::mem::zeroed() };
        let mut wfds: libc::fd_set = unsafe { std::mem::zeroed() };
        let mut efds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut rfds);
            libc::FD_ZERO(&mut wfds);
            libc::FD_ZERO(&mut efds);
        }
        for (_, watcher) in self.watchers.borrow().iter() {
            if watcher.mode == EventMode::Normal && mode == EventMode::Urgent {
                continue;
            }
            let fd = watcher.fd;
            if fd == -1 {
                continue;
            }
            max_fd = max_fd.max(fd);
            unsafe {
                if watcher.events.has(FdEvents::Read) {
                    libc::FD_SET(fd, &mut rfds);
                }
                if watcher.events.has(FdEvents::Write) {
                    libc::FD_SET(fd, &mut wfds);
                }
                if watcher.events.has(FdEvents::Except) {
                    libc::FD_SET(fd, &mut efds);
                }
            }
        }

        // Deviation: the C++ resets the timeout here (blocking), a regression from
        // the commit that introduced the timeout parameter, where a forced fd used
        // to mean "do not block". See PORTING.md.
        if HAS_FORCED_FD.load(Ordering::SeqCst) {
            timeout = Some(Duration::ZERO);
        }

        let next_date = self.timers.borrow().iter().filter_map(|(_, t)| t.date).min();
        if let Some(next_date) = next_date {
            let remaining = next_date.saturating_duration_since(Instant::now());
            timeout = Some(match timeout {
                Some(t) => t.min(remaining),
                None => remaining,
            });
        }

        let ts = timeout.map(|t| libc::timespec { tv_sec: t.as_secs() as libc::time_t, tv_nsec: t.subsec_nanos() as _ });
        let res = unsafe {
            libc::pselect(
                max_fd + 1,
                &mut rfds,
                &mut wfds,
                &mut efds,
                ts.as_ref().map_or(std::ptr::null(), |t| t as *const _),
                sigmask.map_or(std::ptr::null(), |m| m as *const _),
            )
        };

        // copy forced fds *after* select, so that signal handlers can write to
        // forced fds, interupt select, and directly be serviced.
        HAS_FORCED_FD.store(false, Ordering::SeqCst);
        let forced = take_forced_fds();

        for fd in 0..max_fd + 1 {
            let mut events = if is_forced(&forced, fd) { FdEvents::Read } else { FdEvents::None };
            if res > 0 {
                unsafe {
                    if libc::FD_ISSET(fd, &rfds) {
                        events |= FdEvents::Read;
                    }
                    if libc::FD_ISSET(fd, &wfds) {
                        events |= FdEvents::Write;
                    }
                    if libc::FD_ISSET(fd, &efds) {
                        events |= FdEvents::Except;
                    }
                }
            }
            if events != FdEvents::None {
                let found = self
                    .watchers
                    .borrow()
                    .iter()
                    .find(|(_, w)| w.fd == fd)
                    .map(|(k, w)| (k, w.callback.clone()));
                if let Some((key, callback)) = found {
                    callback(&WatcherRef { key }, events, mode)?;
                }
            }
        }

        let now = Instant::now();
        // copy timers in case timers gets mutated
        let timers: Vec<TimerKey> = self.timers.borrow().keys().collect();
        for key in timers {
            let due = self.timers.borrow().get(key).and_then(|t| t.date).is_some_and(|d| d <= now);
            if due {
                self.run_timer(key, mode)?;
            }
        }

        Ok(res > 0)
    }

    fn run_timer(&self, key: TimerKey, mode: EventMode) -> Result<()> {
        let callback = {
            let mut timers = self.timers.borrow_mut();
            let Some(timer) = timers.get_mut(key) else { return Ok(()) };
            if mode == timer.mode {
                timer.date = None;
                Some(timer.callback.clone())
            } else {
                // try again a little later
                timer.date = Some(Instant::now() + Duration::from_millis(10));
                None
            }
        };
        if let Some(callback) = callback {
            callback(&TimerRef { key })?;
        }
        Ok(())
    }

    /// force the watchers associated with fd to be executed
    /// on next handle_next_events call.
    /// async signal safe
    pub fn force_signal(fd: i32) {
        let fd = fd as usize;
        if fd < FORCED_WORDS * 64 {
            FORCED_FDS[fd / 64].fetch_or(1 << (fd % 64), Ordering::SeqCst);
        }
        HAS_FORCED_FD.store(true, Ordering::SeqCst);
    }

    pub fn handle_urgent_events() -> Result<()> {
        if Self::has_instance() {
            Self::instance().try_handle_next_events(EventMode::Urgent, None, Some(Duration::ZERO))?;
        }
        Ok(())
    }
}

/// Access to a registered watcher from inside its callback
pub struct WatcherRef {
    key: WatcherKey,
}

impl WatcherRef {
    pub fn key(&self) -> WatcherKey {
        self.key
    }
    pub fn fd(&self) -> i32 {
        watcher_fd(self.key)
    }
    pub fn close_fd(&self) {
        watcher_close_fd(self.key)
    }
    pub fn disable(&self) {
        watcher_set_fd(self.key, -1)
    }
    pub fn set_events(&self, events: FdEvents) {
        watcher_set_events(self.key, events)
    }
}

fn watcher_fd(key: WatcherKey) -> i32 {
    EventManager::instance().watchers.borrow().get(key).map_or(-1, |w| w.fd)
}

pub fn watcher_reset_fd(key: WatcherKey, fd: i32) {
    watcher_set_fd(key, fd)
}

fn watcher_set_fd(key: WatcherKey, fd: i32) {
    if let Some(w) = EventManager::instance().watchers.borrow_mut().get_mut(key) {
        w.fd = fd;
    }
}

fn watcher_set_events(key: WatcherKey, events: FdEvents) {
    if let Some(w) = EventManager::instance().watchers.borrow_mut().get_mut(key) {
        w.events = events;
    }
}

fn watcher_close_fd(key: WatcherKey) {
    let fd = watcher_fd(key);
    if fd != -1 {
        unsafe { libc::close(fd) };
        watcher_set_fd(key, -1);
    }
}

/// Owning handle on a file descriptor watcher
pub struct FDWatcher {
    key: WatcherKey,
}

impl FDWatcher {
    pub fn new(
        fd: i32,
        events: FdEvents,
        mode: EventMode,
        callback: impl Fn(&WatcherRef, FdEvents, EventMode) -> Result<()> + 'static,
    ) -> Self {
        let key = EventManager::instance().watchers.borrow_mut().insert(WatcherData {
            fd,
            events,
            mode,
            callback: Rc::new(callback),
        });
        FDWatcher { key }
    }
    pub fn key(&self) -> WatcherKey {
        self.key
    }
    pub fn fd(&self) -> i32 {
        watcher_fd(self.key)
    }
    pub fn events(&self) -> FdEvents {
        EventManager::instance().watchers.borrow().get(self.key).map_or(FdEvents::None, |w| w.events)
    }
    pub fn set_events(&self, events: FdEvents) {
        watcher_set_events(self.key, events)
    }
    pub fn mode(&self) -> EventMode {
        EventManager::instance().watchers.borrow().get(self.key).map_or(EventMode::Normal, |w| w.mode)
    }
    pub fn reset_fd(&self, fd: i32) {
        watcher_set_fd(self.key, fd)
    }
    pub fn close_fd(&self) {
        watcher_close_fd(self.key)
    }
    pub fn disable(&self) {
        watcher_set_fd(self.key, -1)
    }
    /// Run the callback directly
    pub fn run(&self, events: FdEvents, mode: EventMode) -> Result<()> {
        let callback = EventManager::instance().watchers.borrow().get(self.key).map(|w| w.callback.clone());
        if let Some(callback) = callback {
            callback(&WatcherRef { key: self.key }, events, mode)?;
        }
        Ok(())
    }
}

impl Drop for FDWatcher {
    fn drop(&mut self) {
        if EventManager::has_instance() {
            let removed = EventManager::instance().watchers.borrow_mut().remove(self.key);
            drop(removed);
        }
    }
}

/// Access to a registered timer from inside its callback
pub struct TimerRef {
    key: TimerKey,
}

impl TimerRef {
    pub fn set_next_date(&self, date: TimePoint) {
        timer_set_date(self.key, date)
    }
    pub fn next_date(&self) -> TimePoint {
        timer_date(self.key)
    }
    pub fn disable(&self) {
        timer_set_date(self.key, None)
    }
}

fn timer_set_date(key: TimerKey, date: TimePoint) {
    if let Some(t) = EventManager::instance().timers.borrow_mut().get_mut(key) {
        t.date = date;
    }
}

fn timer_date(key: TimerKey) -> TimePoint {
    EventManager::instance().timers.borrow().get(key).and_then(|t| t.date)
}

/// Owning handle on a timer. A timer without callback is not registered.
pub struct Timer {
    key: Option<TimerKey>,
    date: Cell<TimePoint>,
}

impl Timer {
    pub fn new(date: TimePoint, callback: impl Fn(&TimerRef) -> Result<()> + 'static, mode: EventMode) -> Self {
        let key = if EventManager::has_instance() {
            Some(EventManager::instance().timers.borrow_mut().insert(TimerData {
                date,
                mode,
                callback: Rc::new(callback),
            }))
        } else {
            None
        };
        Timer { key, date: Cell::new(date) }
    }

    /// A timer that does nothing and is never registered
    pub fn none() -> Self {
        Timer { key: None, date: Cell::new(None) }
    }

    pub fn next_date(&self) -> TimePoint {
        match self.key {
            Some(key) => timer_date(key),
            None => self.date.get(),
        }
    }
    pub fn set_next_date(&self, date: TimePoint) {
        match self.key {
            Some(key) => timer_set_date(key, date),
            None => self.date.set(date),
        }
    }
    pub fn disable(&self) {
        self.set_next_date(None);
    }
    /// Run the callback directly
    pub fn run(&self, mode: EventMode) -> Result<()> {
        if let Some(key) = self.key {
            EventManager::instance().run_timer(key, mode)?;
        }
        Ok(())
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(key) = self.key {
            if EventManager::has_instance() {
                let removed = EventManager::instance().timers.borrow_mut().remove(key);
                drop(removed);
            }
        }
    }
}

pub type SignalHandler = libc::sighandler_t;

pub fn set_signal_handler(signum: i32, handler: SignalHandler) -> SignalHandler {
    unsafe {
        let mut new_action: libc::sigaction = std::mem::zeroed();
        let mut old_action: libc::sigaction = std::mem::zeroed();
        libc::sigemptyset(&mut new_action.sa_mask);
        new_action.sa_sigaction = handler;
        new_action.sa_flags = libc::SA_RESTART;
        libc::sigaction(signum, &new_action, &mut old_action);
        old_action.sa_sigaction
    }
}
