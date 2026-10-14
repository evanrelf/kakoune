#ifndef rust_bridge_hh_INCLUDED
#define rust_bridge_hh_INCLUDED

// C++ functions exposed to Rust through the `#[cxx::bridge]` modules in
// rust/src. Signatures must only use types cxx understands.

#include "unicode.hh"

#include <cstdint>

namespace Kakoune::rs
{

inline bool is_word(uint32_t c) { return Kakoune::is_word(c); }

// The locale-dependent fallbacks unicode.hh uses above ASCII, with the same
// conversions.
inline bool locale_is_alnum(uint32_t c) { return iswalnum((wchar_t)c); }
inline bool locale_is_lower(uint32_t c) { return iswlower((wchar_t)c); }
inline bool locale_is_upper(uint32_t c) { return iswupper((wchar_t)c); }
inline uint32_t locale_to_lower(uint32_t c) { return towlower((wchar_t)c); }

}

#endif // rust_bridge_hh_INCLUDED
