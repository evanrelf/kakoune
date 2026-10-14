# Rust migration plan

This fork is being ported from C++ to Rust incrementally, keeping Kakoune
working and fully tested after every step, in the spirit of fish shell's
port ([Fish 4.0: The Fish Of Theseus][fish]). This document is the plan and its
current status; conventions that apply to any change live in `AGENTS.md`.

[fish]: https://fishshell.com/blog/rustport/

## Goals

- End with a Rust program that behaves identically to the C++ one, as judged
  by the integration tests in `test/` plus the unit tests ported along the way.
- Never have a broken intermediate state: each step builds, passes the tests,
  and could be released.
- Leave the result idiomatic Rust, not C++ transliterated into Rust. The core
  data model will be redesigned rather than translated.

Not goals: changing behaviour, adding features, or keeping the C++ buildable
without a Rust toolchain.

## Approach

C++ keeps `main()` until late in the migration. Rust code is compiled into one
static library (`rust/`, crate `kakoune`) that the Makefile links into `kak`,
and the two languages talk through [cxx] bridges.

Each module is ported behind its existing C++ interface: the C++ header and
class shapes that callers use stay put, and their implementation becomes a
thin shim over Rust. Callers don't change, so each port is a small, reviewable
diff. Once every caller of an interface is itself in Rust, the shim goes away.

Modules are ported from the leaves of the dependency graph inward, because a
module whose dependencies are still C++ needs bridges for every one of them.

[cxx]: https://cxx.rs/

### Interop tooling

- **cxx** (now): bridges are declared by hand on the Rust side, and cxx
  generates both halves. That keeps the boundary small and explicit, which
  suits Kakoune's custom types, overloads and exceptions.
- **autocxx** (maybe, for phase 3): generates Rust bindings from C++ headers,
  which becomes worthwhile once Rust code has to call broad C++ APIs such as
  `Buffer` or `Context`. Google archived it in September 2026; the community
  fork at [Choochmeque/autocxx] is the maintained continuation. Fish relied on
  autocxx for its port, forked it, and found it slow and finicky but workable.
  Expect Kakoune's C++23-heavy headers to need wrapper headers.
- **Crubit**: Google's bidirectional interop tool. Currently Bazel-only and
  built from source, so not usable here; worth re-checking before phase 3.

[Choochmeque/autocxx]: https://github.com/Choochmeque/autocxx

## Decisions

Recorded so they aren't re-litigated without new information.

- **Rust, not another language.**
  - Zig was the main alternative. Zig can't import C++ headers either, so every
    boundary would need a hand-written C ABI. Zig is also pre-1.0, with
    breaking changes every release, and offers no memory-safety gain.
  - Swift can import C++ headers directly, but can't catch C++ exceptions,
    which Kakoune uses for ordinary error handling. Ladybird picked Swift for
    the same kind of migration in 2024, then dropped it in February 2026 over
    C++ interop problems and [moved to Rust][ladybird].
  - Carbon and cppfront are still experimental; D and Nim have too small a
    community.
  - Rust is stable, has workable C++ interop tooling, and fish has shown that
    an incremental port of a project like this succeeds.
- **The Makefile stays in charge until Rust owns `main()`.** Cargo builds only
  the Rust library.
- **Not Zig's build system.** It was tried, and builds Kakoune unchanged and
  cross-compiles easily. But it built about twice as slowly as `make` with the
  system clang, ties the build to Zig's bundled clang and libc++ (no GCC), and
  its build API changes every release.

[ladybird]: https://ladybird.org/newsletter/2026-02-28/

## Phases

### Phase 0: build plumbing (done, 2026-10-13)

- `rust/` crate built as `libkakoune.a` with `cxx_build`, linked by the
  Makefile, with cargo's profile following `debug=`.
- Generated headers copied to a stable include directory for the Makefile.
- `src/rust_bridge.hh` (C++ functions exposed to Rust) and
  `src/rust_interop.hh` (Kakoune ↔ cxx type conversions).
- Nix vendors crates from `rust/Cargo.lock`; the dev shell has the Rust tools.
- A smoke-test bridge (`rust/src/smoke_test.rs`, `src/rust_bridge.cc`)
  exercising calls in both directions.

### Phase 1: first real module

Port `ranked_match` (about 390 lines), the fuzzy matcher behind completion
menus. It depends only on string and Unicode helpers, has its own unit tests,
and has few callers (`completion.cc`, `commands.cc`, `word_db.cc`).

- Keep the C++ `RankedMatch` class: `commands.cc` inherits from it. Its
  constructors and `operator<` call into Rust; its private fields are filled
  from a plain bridge struct.
- Candidates and queries cross as bytes. `is_word`, `to_lower` and `is_upper`
  must give the same answers as `src/unicode.hh`, so call back into the C++ or
  the same libc functions.
- Port the `UnitTest` block to Rust tests, and add a differential test that
  runs the old C++ and new Rust implementations on generated inputs and
  requires identical results and ordering, before deleting the C++.
- Delete the smoke test.

Phase 1 is where the patterns for later ports get settled: how shims look,
how errors cross, and how differential tests are structured. Write them down
in this document once they're settled.

### Phase 2: more self-contained modules

Candidates, roughly in order of how few project dependencies they have:

- `color`, `keys`, `parameters_parser`: parsing and formatting with few
  dependencies.
- `regex_vm` (about 2,500 lines): Kakoune's own regex engine, self-contained
  and on the hot path of highlighting and selection. Port it behind the
  existing `regex.hh` interface. Its matching semantics must be preserved
  exactly; it is not a candidate for the `regex` crate.
- `word_db`: the first module that needs to call into `Buffer`, so it's the
  first real test of Rust → C++ calls on core types.
- `json` with `json_ui`: `parse_json` produces Kakoune's type-erased `Value`,
  so it's cheaper to port the JSON UI and its parser together than to bridge
  `Value`.

Small helpers called from all over the C++ (hashing, UTF-8 iteration) are
better ported alongside their callers than on their own, since each call
would otherwise cross the bridge.

### Phase 3: the core

`String`/`StringView`, `Vector` and its memory domains, `HashMap`, `Buffer`,
`Selection`, `Context`, `Window`, `Client`, the option and hook managers, and
the `SafePtr`/`RefPtr` graph that connects them. This is most of the work.

Decide at the start of the phase:

- Whether Rust code uses Kakoune's C++ `String` as an opaque type during the
  transition, or owns Rust strings and converts at the boundary.
- The Rust ownership model for the object graph (arena indices, `Rc`/`RefCell`,
  or a different structure entirely), designed rather than transliterated.
- Whether to adopt autocxx for the C++ APIs that Rust code calls.

Expect to port clusters of tightly coupled modules together rather than one
file at a time.

### Phase 4: Rust owns the program

- Move `main()` to Rust; cargo's build script compiles whatever C++ remains.
- Keep a thin Makefile for installation, as fish kept CMake.
- Remove bridges as the C++ on either side disappears, then the C++
  toolchain itself.

## Cross-cutting work

- **Generated header dependencies.** Each C++ object including a
  cxx-generated header currently needs its own Makefile rule and recompiles
  whenever the Rust library changes. Find a better mechanism before many
  files include generated headers.
- **Platforms.** Only macOS has been built with the Rust library linked in.
  Linux link flags are in the Makefile but untested; other platforms may need
  more.
- **Source tarballs.** `make dist` archives fetch crates from crates.io at
  build time unless dependencies are vendored.

## Progress

| Module | Status |
| --- | --- |
| Build plumbing | Done |
| `ranked_match` | Not started |
