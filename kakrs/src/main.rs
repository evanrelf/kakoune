#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

#[macro_use]
pub mod format;
#[macro_use]
pub mod exception;
#[macro_use]
pub mod flags;
#[macro_use]
pub mod option_types;

pub mod alias_registry;
pub mod buffer;
pub mod buffer_manager;
pub mod buffer_utils;
pub mod changes;
pub mod client;
pub mod client_manager;
pub mod command_manager;
pub mod commands;
pub mod completion;
pub mod context;
pub mod debug;
pub mod diff;
pub mod display_buffer;
pub mod event_manager;
pub mod face;
pub mod file;
pub mod highlighter;
pub mod highlighter_group;
pub mod highlighters;
pub mod hook_manager;
pub mod input_handler;
pub mod insert_completer;
pub mod json;
pub mod json_ui;
pub mod kak_main;
pub mod keymap_manager;
pub mod keys;
pub mod line_modification;
pub mod normal;
pub mod option_manager;
pub mod parameters_parser;
pub mod ranked_match;
pub mod regex;
pub mod regex_vm;
pub mod register_manager;
pub mod remote;
pub mod scope;
pub mod selection;
pub mod shell_manager;
pub mod string_utils;
pub mod terminal_ui;
pub mod unicode;
pub mod units;
pub mod user_interface;
pub mod utf8;
pub mod utils;
pub mod value;
pub mod window;
pub mod word_db;

fn main() {
    std::process::exit(kak_main::main());
}
