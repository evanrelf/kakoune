#ifndef rust_bridge_hh_INCLUDED
#define rust_bridge_hh_INCLUDED

// C++ functions exposed to Rust through the `#[cxx::bridge]` modules in
// rust/src. Signatures must only use types cxx understands.

#include "unicode.hh"

#include <cstdint>

namespace Kakoune::rs
{

inline bool is_word(uint32_t c) { return Kakoune::is_word(c); }

}

#endif // rust_bridge_hh_INCLUDED
