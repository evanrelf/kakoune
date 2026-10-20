//! Port of parameters_parser.hh/cc

use crate::completion::Completions;
use crate::context::Context;
use crate::exception::{Error, ErrorKind, Result};
use crate::string_utils::{column_length, string_of_cp_cols};
use indexmap::IndexMap;
use std::rc::Rc;

pub fn parameter_error(msg: Vec<u8>) -> Error {
    Error::new(ErrorKind::ParameterError, msg)
}

pub type ArgCompleter = Rc<dyn Fn(&Context, &[u8], usize) -> Completions>;

#[derive(Clone)]
pub struct SwitchDesc {
    /// None: boolean switch; Some(completer): switch taking an argument
    pub arg_completer: Option<Option<ArgCompleter>>,
    pub description: Vec<u8>,
}

impl SwitchDesc {
    /// A switch without argument
    pub fn flag(description: &str) -> Self {
        SwitchDesc { arg_completer: None, description: description.as_bytes().to_vec() }
    }
    /// A switch taking an argument, without completion
    pub fn arg(description: &str) -> Self {
        SwitchDesc { arg_completer: Some(None), description: description.as_bytes().to_vec() }
    }
    /// A switch taking an argument with completion
    pub fn arg_with(completer: ArgCompleter, description: &str) -> Self {
        SwitchDesc { arg_completer: Some(Some(completer)), description: description.as_bytes().to_vec() }
    }
    pub fn takes_arg(&self) -> bool {
        self.arg_completer.is_some()
    }
}

pub type SwitchMap = IndexMap<Vec<u8>, SwitchDesc>;

/// Build a SwitchMap from (name, desc) pairs
pub fn switches(items: Vec<(&str, SwitchDesc)>) -> SwitchMap {
    items.into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)).collect()
}

pub fn generate_switches_doc(switches: &SwitchMap) -> Vec<u8> {
    let mut res = Vec::new();
    if switches.is_empty() {
        return res;
    }
    let switch_len = |key: &[u8], sw: &SwitchDesc| column_length(key) + if sw.takes_arg() { 5 } else { 0 };
    let maxlen = switches.iter().map(|(k, v)| switch_len(k, v)).max().unwrap();
    for (key, sw) in switches {
        res.extend(kformat!(
            "-{} {}{}{}\n",
            key,
            if sw.takes_arg() { "<arg>" } else { "" },
            string_of_cp_cols(' ' as u32, maxlen - switch_len(key, sw) + 1),
            sw.description
        ));
    }
    res
}

crate::kak_flags! {
    pub struct ParameterFlags: u32 {
        const None = 0;
        const SwitchesOnlyAtStart   = 0b0001;
        const SwitchesAsPositional  = 0b0010;
        const IgnoreUnknownSwitches = 0b0100;
    }
}

#[derive(Clone)]
pub struct ParameterDesc {
    pub switches: SwitchMap,
    pub flags: ParameterFlags,
    pub min_positionals: usize,
    pub max_positionals: usize,
}

impl Default for ParameterDesc {
    fn default() -> Self {
        ParameterDesc { switches: SwitchMap::new(), flags: ParameterFlags::None, min_positionals: 0, max_positionals: usize::MAX }
    }
}

impl ParameterDesc {
    pub fn new(switches: SwitchMap, flags: ParameterFlags, min_positionals: usize, max_positionals: usize) -> Self {
        ParameterDesc { switches, flags, min_positionals, max_positionals }
    }
    pub fn positionals(min: usize, max: usize) -> Self {
        ParameterDesc { min_positionals: min, max_positionals: max, ..Default::default() }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParserState {
    Switch,
    SwitchArgument,
    Positional,
}

/// ParametersParser provides tools to parse command parameters.
/// There are 3 types of parameters:
///  * unnamed options, which are accessed by position (ignoring named ones)
///  * named boolean options, which are enabled using '-name' syntax
///  * named string options,  which are defined using '-name value' syntax
pub struct ParametersParser<'a> {
    params: &'a [Vec<u8>],
    positional_indices: Vec<usize>,
    switches: IndexMap<Vec<u8>, Vec<u8>>,
    state: Option<ParserState>,
}

impl<'a> ParametersParser<'a> {
    pub fn new(params: &'a [Vec<u8>], desc: &ParameterDesc) -> Result<Self> {
        Self::new_ext(params, desc, false)
    }

    pub fn new_ext(params: &'a [Vec<u8>], desc: &ParameterDesc, ignore_errors: bool) -> Result<Self> {
        let mut res = ParametersParser { params, positional_indices: Vec::new(), switches: IndexMap::new(), state: None };
        let switches_only_at_start = desc.flags.has(ParameterFlags::SwitchesOnlyAtStart);
        let ignore_unknown_switches = desc.flags.has(ParameterFlags::IgnoreUnknownSwitches);
        let mut only_pos = desc.flags.has(ParameterFlags::SwitchesAsPositional);

        let mut switch_seen = vec![false; desc.switches.len()];
        let mut i = 0;
        while i < params.len() {
            if !only_pos && !ignore_unknown_switches && params[i] == b"--" {
                res.state = Some(ParserState::Switch);
                only_pos = true;
            } else if !only_pos && !params[i].is_empty() && params[i][0] == b'-' {
                let switch_name = &params[i][1..];
                let found = desc.switches.get_full(switch_name);
                res.state = Some(if found.is_none() && ignore_unknown_switches {
                    ParserState::Positional
                } else {
                    ParserState::Switch
                });
                let Some((switch_index, key, sw)) = found else {
                    if ignore_unknown_switches {
                        res.positional_indices.push(i);
                        if switches_only_at_start {
                            only_pos = true;
                        }
                        i += 1;
                        continue;
                    }
                    if ignore_errors {
                        i += 1;
                        continue;
                    }
                    return Err(parameter_error(kformat!("unknown option '{}'", params[i])));
                };

                if switch_seen[switch_index] {
                    if ignore_errors {
                        i += 1;
                        continue;
                    }
                    return Err(rt_err!("switch '-{}' specified more than once", key));
                }
                switch_seen[switch_index] = true;

                if sw.takes_arg() {
                    i += 1;
                    if i == params.len() {
                        if ignore_errors {
                            continue;
                        }
                        return Err(parameter_error(kformat!("missing value for option '{}'", key)));
                    }
                    res.state = Some(ParserState::SwitchArgument);
                }
                let value = if sw.takes_arg() { params[i].clone() } else { Vec::new() };
                res.switches.insert(switch_name.to_vec(), value);
            } else {
                // positional
                res.state = Some(ParserState::Positional);
                if switches_only_at_start {
                    only_pos = true;
                }
                res.positional_indices.push(i);
            }
            i += 1;
        }
        let count = res.positional_indices.len();
        if !ignore_errors && (count > desc.max_positionals || count < desc.min_positionals) {
            return Err(parameter_error(b"wrong argument count".to_vec()));
        }
        Ok(res)
    }

    /// Return Some if the switch was given, with a non empty value if the
    /// switch took an argument.
    pub fn get_switch(&self, name: &str) -> Option<&[u8]> {
        self.switches.get(name.as_bytes()).map(|v| v.as_slice())
    }

    pub fn switches_contains(&self, name: &[u8]) -> bool {
        self.switches.contains_key(name)
    }

    pub fn has_switch(&self, name: &str) -> bool {
        self.switches.contains_key(name.as_bytes())
    }

    /// positional parameters count
    pub fn positional_count(&self) -> usize {
        self.positional_indices.len()
    }

    /// access positional parameter by index
    pub fn get(&self, index: usize) -> &'a [u8] {
        &self.params[self.positional_indices[index]]
    }

    pub fn positionals_from(&self, first: usize) -> &'a [Vec<u8>] {
        if first < self.positional_indices.len() {
            &self.params[self.positional_indices[first]..]
        } else {
            &[]
        }
    }

    pub fn positionals(&self) -> Vec<Vec<u8>> {
        self.positional_indices.iter().map(|&i| self.params[i].clone()).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.positional_indices.iter().map(move |&i| self.params[i].as_slice())
    }

    pub fn state(&self) -> Option<ParserState> {
        self.state
    }
}

impl std::ops::Index<usize> for ParametersParser<'_> {
    type Output = [u8];
    fn index(&self, index: usize) -> &[u8] {
        &self.params[self.positional_indices[index]]
    }
}
