//! Rust parts of Kakoune, linked into the C++ executable as a static library.
//! C++ and Rust talk through `#[cxx::bridge]` modules, each listed in
//! `build.rs`.

mod ranked_match;
mod smoke_test;
mod unicode;
mod utf8;
mod word_splitter;
