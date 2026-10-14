#ifndef rust_interop_hh_INCLUDED
#define rust_interop_hh_INCLUDED

// Conversions between Kakoune types and the cxx types used by Rust bridges.

#include "string.hh"

#include "rust/cxx.h"

#include <cstdint>

namespace Kakoune
{

// Kakoune strings may hold invalid UTF-8, so pass them to Rust as bytes
// rather than rust::Str, which requires valid UTF-8.
inline rust::Slice<const uint8_t> to_rust_bytes(StringView str)
{
    return {reinterpret_cast<const uint8_t*>(str.data()), (size_t)(int)str.length()};
}

}

#endif // rust_interop_hh_INCLUDED
