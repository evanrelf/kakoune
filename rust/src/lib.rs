//! Rust parts of Kakoune, linked into the C++ executable as a static library.
//! C++ and Rust talk through `#[cxx::bridge]` modules, each listed in
//! `build.rs`.

mod smoke_test;
