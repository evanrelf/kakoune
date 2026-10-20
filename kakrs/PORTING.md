# Kakoune Rust port notes

`kakrs` is a port of the C++ sources in `../src` to Rust. The goal is
behavioural fidelity: the integration test suite in `../test` must pass
against the Rust binary.

## Conventions

- Strings are byte strings (`Vec<u8>` / `&[u8]`), as Kakoune strings may
  hold invalid UTF-8. `kformat!` is a byte preserving port of `format()`.
- C++ exceptions are `exception::Result<T>`; `Error::kind` distinguishes the
  runtime_error subclasses that are caught specifically (cancel, failure,
  parameter errors...). `is_runtime()` mirrors `catch (runtime_error&)`.
- Unit types (`LineCount`, `ByteCount`, `CharCount`, `ColumnCount`) are `i32`
  aliases, `Codepoint` is `u32`.
- Shared, re-entrant objects (buffers, windows, clients, input handlers,
  input modes) live in `Rc` and expose `&self` methods, with interior
  mutability kept as small as possible (`Cell` for copyable state, one
  grouped `RefCell` where no re-entrancy happens). Borrows are never held
  across calls that can run hooks or commands.
- Singletons are leaked thread local statics with `instance()` accessors.
- Event watchers and timers are owned handles registered in slot maps of the
  event manager; dropping a handle unregisters it.
- Kakoune's insertion-ordered `HashMap` is `indexmap::IndexMap`
  (`remove` -> `shift_remove`, `erase`/`unordered_remove` -> `swap_remove`).

## Deliberate deviations from the C++

- `EventManager::handle_next_events`: a pending forced fd makes the next
  wait non blocking. The C++ resets the timeout (blocking until another fd is
  ready), which regressed in 9358c62bf "Take a timeout argument in
  EventManager::handle_next_events" where it replaced `block = false`.
- Undefined behaviour in the C++ (out of range accesses, more than 32
  regex captures in the valid capture mask...) is replaced by well defined
  behaviour, see the comments at the relevant places.
- `InsertCompleter::select` records the inserted completion one key per
  codepoint. The C++ records one key per byte (`for (auto& c : completion)
  record_key(c)`, with sign extended chars), so repeating an insertion of a
  non ascii completion with `.` inserts nothing for those characters.
- Regex searches abort when handling urgent events fails (e.g. `<c-g>`
  cancel): the idle callback returns `ControlFlow::Break` and the error is
  returned through `UrgentEventsIdle::check`, where the C++ unwinds through
  the regex engine with an exception.
