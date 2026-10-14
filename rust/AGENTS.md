# rust/AGENTS.md

The `kakoune` crate: Rust code linked into `kak` as the static library
`libkakoune.a`. The Makefile drives the build; run `make` from the repo root
rather than `cargo build` here, since plain cargo builds don't put the
generated headers where the C++ build looks for them.

## How the build fits together

- The Makefile runs cargo (release, or debug when `debug=yes`), passing `CXX`
  so the generated C++ glue is compiled by the same compiler and C++ standard
  library as the rest of Kakoune.
- `build.rs` compiles each bridge's generated glue with the same language
  standard, include path and `KAK_DEBUG` setting as the Makefile, and reruns
  whenever a header in `src/` changes, since the glue includes them.
- cxx writes generated headers under cargo's `OUT_DIR`, whose path the
  Makefile can't predict, so `build.rs` copies them to the directory named by
  `KAK_RUST_INCLUDE_DIR`, rewriting files only when their contents change.

## Adding a bridge

1. Write a module with a `#[cxx::bridge(namespace = "Kakoune::rs")]` block,
   declare it in `src/lib.rs`, and add its path to `BRIDGES` in `build.rs`.
2. C++ includes the generated header as `"kakoune/src/<module>.rs.h"`. Each
   C++ object file that includes one needs a
   `src/<file>$(tag).o: $(rust_lib)` line in the Makefile, so the header
   exists before that file compiles.
3. C++ functions that Rust calls go in `src/rust_bridge.hh`, which the
   generated glue includes; their signatures may only use types cxx
   understands. Helpers for converting Kakoune types to cxx types, used by C++
   code calling into Rust, go in `src/rust_interop.hh`. A header that uses
   `rust::` types must include `"rust/cxx.h"` itself.

## Testing

- `cargo test` needs `nix develop`, which points `KAKOUNE_UPSTREAM_SRC` at
  upstream's source for `reference/`, and a UTF-8 locale.
- Conformance tests (`src/<module>/conformance.rs`) use Hegel; the plan in
  `RUST_MIGRATION.md` says how to write one. A failure prints a shrunk
  counterexample, is saved under `.hegel/` and replayed first on the next
  run. `HEGEL_TEST_CASES` overrides the case count in `hegel.toml`, and
  `HEGEL_SEED` fixes the seed, e.g. to reproduce a `nix flake check` failure.
- `cargo test` links only this crate and the generated glue, not Kakoune's
  object files. Rust tests can call C++ functions defined inline in headers,
  but anything defined in a `.cc` file fails to link. Test such code through
  the C++ side (`UnitTest`s in a `make debug=yes` build) or the integration
  tests instead.
- C++ `UnitTest`s that call into Rust only run in debug builds, at startup. To
  check that one actually runs, make it fail on purpose once.

## The reference crate

`reference/` builds upstream's C++ for the conformance tests and is only a
dev-dependency, so none of it reaches `libkakoune.a`.

- Its `build.rs` compiles the upstream `.cc` files in `UPSTREAM_SOURCES`
  with `-DKakoune=KakouneReference`, and without `KAK_DEBUG`, since
  upstream's assertions would pull in most of the editor. Add whatever a
  newly covered module needs; the linker names anything missing.
- Each bridge module `src/<module>.rs` has its glue in
  `src/<module>_glue.{hh,cc}`. The suffix keeps glue headers from shadowing
  upstream ones, which glue includes by their upstream names.
- Glue is written in `namespace Kakoune::reference`, which the rename turns
  into `KakouneReference::reference` consistently on both sides.

## Dependencies

Add crates with `cargo add` and commit `Cargo.lock`. The Nix build vendors
crates from the lockfile and builds with `--frozen`, so a stale lockfile
breaks `nix build`.
