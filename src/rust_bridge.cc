#include "rust_bridge.hh"

#include "kakoune/src/smoke_test.rs.h"
#include "rust_interop.hh"
#include "unit_tests.hh"

namespace Kakoune
{

UnitTest test_rust_bridge{[]{
    kak_assert(rs::count_word_bytes(to_rust_bytes("foo_bar baz!")) == 10);
    kak_assert(rs::count_word_bytes(to_rust_bytes("")) == 0);
}};

}
