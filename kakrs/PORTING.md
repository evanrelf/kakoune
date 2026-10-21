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

## Comment tags

Two greppable tags mark where the port's behaviour was a deliberate choice
(`grep -rn '\[FAITHFUL\]\|\[DEVIATION\]' src`):

- `// [FAITHFUL]`: the code copies questionable C++ behaviour on purpose
  (a hang, a bug, a quirk such as decoding bytes through a signed `char`).
  Each one has an entry in `UPSTREAM-QUIRKS.md`.
- `// [DEVIATION]`: the code deliberately behaves differently from the C++,
  usually because the C++ is undefined behaviour there.

Ordinary porting notes ("C++ name", "as in the C++") are not tagged. Tests
narrowed because of a faithful quirk use `[FAITHFUL]` too.

## Deliberate deviations from the C++

Questionable but well defined C++ behaviour (hangs, bugs) is copied as is and
listed in `UPSTREAM-QUIRKS.md`. The port only deviates where it has to:

- Undefined behaviour in the C++ (out of range accesses, more than 32
  regex captures in the valid capture mask...) is replaced by well defined
  behaviour, see the comments at the relevant places.
- Regex searches abort when handling urgent events fails (e.g. `<c-g>`
  cancel): the idle callback returns `ControlFlow::Break` and the error is
  returned through `UrgentEventsIdle::check`, where the C++ unwinds through
  the regex engine with an exception.

## Testing

- `cargo test`: ported C++ unit tests and hegel property tests.
- `./run-tests.sh [dirs...]`: the upstream integration suite (`../test`)
  against the Rust binary (`PROFILE=release` for the release build).
- `python3 tools/difffuzz.py <seed> <count>`: differential fuzzing of random
  normal mode key sequences against the C++ build (`../src/kak`).

## Building with Nix

`nix build` (from the repository root) builds the Rust `kak` with the
runtime files (kakrc, rc, colors, doc) installed in `share/kak`, the layout
of the C++ `make install`, so it runs without `KAKOUNE_RUNTIME`. `nix develop`
gives a shell with the Rust toolchain. A bare `cargo build` binary still
needs `KAKOUNE_RUNTIME=<repo>/share/kak`.
