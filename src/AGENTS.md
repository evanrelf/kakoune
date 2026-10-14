# src/AGENTS.md

Kakoune's C++ sources. Match the surrounding code:

- Code lives in `namespace Kakoune`. Braces go on their own line; indent with
  four spaces. Prefer `and`, `or` and `not` over `&&`, `||` and `!`.
- Use Kakoune's own types (`String`, `StringView`, `Vector`, `HashMap`,
  `RefPtr` and friends) rather than their `std` equivalents.
- Errors are reported by throwing `Kakoune::runtime_error` and its subclasses,
  which do not derive from `std::exception`.
- Unit tests are `UnitTest` objects at the bottom of the `.cc` file they test,
  using `kak_assert`. They only run in `make debug=yes` builds.

`rust_bridge.hh` and `rust_interop.hh` are the C++ side of the Rust bridges;
see `rust/AGENTS.md`.
