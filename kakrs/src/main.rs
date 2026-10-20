#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

#[macro_use]
pub mod format;
#[macro_use]
pub mod exception;
pub mod string_utils;
pub mod unicode;
pub mod units;
pub mod utf8;

fn main() {
    println!("Hello, world!");
}
