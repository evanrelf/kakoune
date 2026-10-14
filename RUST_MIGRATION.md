# Rust migration plan

This fork is being ported from C++ to Rust incrementally, keeping Kakoune
working and fully tested after every step. This document is the plan and its
current status; conventions that apply to any change live in `AGENTS.md`.
[Fish 4.0: The Fish Of Theseus][fish] describes a similar port.

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
  `Buffer` or `Context`. Use the maintained fork, [Choochmeque/autocxx]; the
  original is archived. Expect Kakoune's C++23-heavy headers to need wrapper
  headers.
- **Crubit**: re-evaluate before phase 3. It currently only supports Bazel.

[Choochmeque/autocxx]: https://github.com/Choochmeque/autocxx

## Phases

### Phase 0: build plumbing (done, 2026-10-13)

- `rust/` crate built as `libkakoune.a` with `cxx_build`, linked by the
  Makefile, with cargo's profile following `debug=`.
- Generated headers copied to a stable include directory for the Makefile.
- `src/rust_bridge.hh` (C++ functions exposed to Rust) and
  `src/rust_interop.hh` (Kakoune ↔ cxx type conversions).
- Nix vendors crates from `rust/Cargo.lock`; the dev shell has the Rust tools.

### Phase 1: first real module (done, 2026-10-13)

Ported `ranked_match`, the fuzzy matcher behind completion menus, to
`rust/src/ranked_match.rs`, along with the helpers it needs:
`unicode.rs` and `utf8.rs` (mirroring `unicode.hh` and `utf8.hh`) and
`word_splitter.rs`. The C++ `RankedMatch` class remains as a shim.

### Patterns for porting a module

These were settled by `ranked_match`; follow them unless a module gives a
reason not to.

- **Shims.** Keep the C++ class and header so callers don't change. Its
  methods convert arguments and call Rust; state Rust computes comes back in
  a cxx shared struct (`rs::Rank`) that the shim copies into its existing
  fields. Headers included widely forward-declare the shared struct
  (`namespace rs { struct Rank; }`) rather than including the generated
  header, so only the shim's `.cc` needs a Makefile dependency on the Rust
  library.
- **Rust API.** Write the idiomatic Rust type first (`RankedMatch<'a>` with
  `Option`-returning constructors and an `Ordering`-returning comparison),
  then thin `extern "Rust"` functions over it. cxx can't return `Option`, so
  bridge functions return `bool` and fill a `&mut` out-parameter.
- **Faithful helpers.** `utf8.rs` decodes invalid UTF-8 to the same values
  as the C++, including sign-extending bytes wherever `char` is signed
  (`char_to_codepoint`). `unicode.rs` classifies ASCII in Rust and calls the
  C++ `locale_*` wrappers in `rust_bridge.hh` above it. Where C++ compares
  `char`s as codepoints or mixes byte and character counts, the Rust does the
  same, with a comment.
- **Differential tests.** Before switching callers over, commit a temporary
  C++ `UnitTest` next to the old implementation that runs both on generated
  inputs (a seeded xorshift over tokens mixing ASCII, non-ASCII, invalid
  UTF-8 and characters that matter to the module) and compares every result
  field and every pairwise ordering. Bias generation so most inputs exercise
  the interesting paths, check it catches a deliberately injected bug, run it
  once with many more rounds and another seed, then delete it together with
  the C++ implementation. It runs in Kakoune's process, so the locale and
  libc are the real ones. `git show` the `ranked_match` one for a template.
- **Unit tests.** Port the C++ `UnitTest`s to Rust tests, and keep a small
  C++ `UnitTest` exercising the shim, so debug builds check the wiring.
- **Errors.** Not settled yet: `ranked_match` can't fail. The first module
  that throws (likely `parameters_parser` or `keys`) decides how
  `runtime_error`s cross; cxx's `Result` support converts Rust errors into
  `rust::Error` exceptions, which Kakoune's handlers won't catch, so
  shims will need to catch and rethrow.

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
- Keep a thin Makefile for installation.
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
| `ranked_match` | Done |
