# Rust migration plan

This fork is being ported from C++ to Rust incrementally, keeping Kakoune
working and fully tested after every step. This document is the plan and its
current status; conventions that apply to any change live in `AGENTS.md`.
[Fish 4.0: The Fish Of Theseus][fish] describes a similar port.

[fish]: https://fishshell.com/blog/rustport/

## Goals

- End with a Rust program that behaves identically to the C++ one, as judged
  by the integration tests in `test/`, the unit tests ported along the way,
  and conformance tests against upstream (see below).
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

### Conformance testing

The integration tests are black-box regression tests: they catch a port that
breaks a feature, but rarely one that ranks a candidate differently or
decodes a malformed byte differently. So each ported module is also tested
against the C++ it replaced, on generated inputs.

The oracle is upstream Kakoune at the commit this fork branched from, pinned
as the `kakoune-upstream` flake input, so nothing has to be kept or copied
here once a module's C++ is deleted. It serves at two levels:

- **Modules.** `rust/reference` (crate `kakoune-reference`, a dev-dependency
  of `kakoune`) compiles the upstream `.cc` files a module needs, with the
  `Kakoune` namespace renamed so they link alongside the port, and exposes
  them through cxx bridges. Property tests written with [Hegel] (`hegeltest`)
  run both implementations in one process, with the real locale and libc, and
  require identical results. They live in `rust/src/<module>/conformance.rs`
  and stay after the C++ is gone.
- **The whole program.** `packages.kakoune-upstream` builds upstream's `kak`.
  Nothing uses it yet; it's the oracle for phase 3, where the redesigned core
  has no C++ module with the same interface to compare against, so scripted
  sessions must run through both binaries and their results be compared.

`cargo test` runs the conformance tests with 10,000 random cases each (set in
`rust/hegel.toml`); `nix flake check` runs 100,000 from a fixed seed.

[Hegel]: https://docs.rs/hegeltest

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
- **Conformance tests.** Before switching callers over, add the module's
  upstream sources to `rust/reference` with a bridge that mirrors the port's
  (glue can read private fields through explicit template instantiation, as
  `ranked_match_glue.cc` does), and write Hegel tests comparing every result
  field and, for orderings, every pair. Generate from tokens mixing ASCII,
  non-ASCII, invalid UTF-8 and characters that matter to the module, plus
  occasional arbitrary characters, which is how differences between the
  locale and Rust's Unicode tables get found. Bias generation so most inputs
  exercise the interesting paths and check with `HEGEL_STATISTICS=1` and
  `tc.event` that they do. Then inject a few bugs, in the module and in the
  helpers it uses, and make sure each is caught.
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
file at a time. Before the first of them lands, build whole-program
conformance tests against `kakoune-upstream`: the integration tests plus
generated key sequences on varied buffers, run through both binaries, with
buffer contents, selections and UI output compared.

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
