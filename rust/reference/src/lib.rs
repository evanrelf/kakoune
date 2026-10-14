//! Upstream Kakoune's C++, from the commit this fork branched from, for
//! testing the Rust port against. `nix develop` points
//! `KAKOUNE_UPSTREAM_SRC` at it.
//!
//! The C++ is compiled with `Kakoune` renamed to `KakouneReference`, so the
//! glue files are written as if in `namespace Kakoune`.

pub mod locale;
pub mod ranked_match;
