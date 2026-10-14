# AGENTS.md

This is a fork of Kakoune, a modal text editor written in C++, that is being
incrementally ported to Rust. `RUST_MIGRATION.md` has the plan, its status
and the decisions behind it; read it before working on the port, and update it
as the plan changes.

In this project, you are explicitly allowed to use version control tools and
author commits yourself. You are encouraged to make commits often. Break your
work down into smaller, atomic changes and commit those individually. Unless
there's something particularly unusual about the changes or their motivation,
err towards short commit messages (i.e. not including a body).

## Building and testing

- Work inside `nix develop`, which provides every tool the build needs.
- `make` builds `src/kak`; `make debug=yes` builds with `KAK_DEBUG`, which
  enables assertions and runs the unit tests at startup.
- `make test` runs the integration tests in `test/`, which need a UTF-8
  locale. `nix build` builds and runs them in a sandbox. The test format is
  described in `test/README.asciidoc`.
- Before calling a change done, run the tests in both debug and optimized
  builds, and when Rust changed, `cargo test`, `cargo clippy` and
  `cargo fmt --check`.

## Repository

- The working copy is snapshotted by jj automatically, so anything not ignored
  gets tracked as soon as it exists. Update `.gitignore` before generating new
  build outputs, not after.
- Nix flakes only see tracked files.
- The CI configs are inherited from upstream and unused; ignore them.

## Naming

"Kakoune" is the project, namespace and crate name; `kak` refers only to the
binary.

## The Rust port

- Rust code lives in `rust/` and is linked into the program as a single static
  library, so the Rust standard library is only linked once. C++ and Rust talk
  through cxx bridges.
- Port one module at a time behind its existing C++ interface, so callers
  don't change. Work from self-contained modules inward; the core types
  (buffers, contexts, windows) are a web of shared mutable objects that need
  redesigning for Rust rather than translating line by line.
- Behaviour must not change. The integration tests are the oracle; for subtle
  logic such as ranking or regex matching, also compare the old and new
  implementations directly before deleting the C++.
- Port a module's unit tests along with it.

When crossing the boundary:

- Pass strings as bytes, not `&str`: Kakoune strings may contain invalid
  UTF-8.
- Character classification must match the C++, which falls back to the
  locale-dependent libc functions (`iswalnum`, `towlower`, ...) above ASCII.
  Rust's `char` methods use different Unicode tables.
- Exceptions can't unwind through Rust, and panics abort at the boundary.
  Since Kakoune's exceptions aren't `std::exception`s, convert errors
  explicitly in both directions.
- Compile any C++ glue with the same compiler and flags as the rest of the C++
  (including `KAK_DEBUG`), and keep Rust's debug assertions in step with it.
