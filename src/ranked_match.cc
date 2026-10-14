#include "ranked_match.hh"

#include "kakoune/src/ranked_match.rs.h"
#include "rust_interop.hh"
#include "unit_tests.hh"

namespace Kakoune
{

UsedLetters used_letters(StringView str)
{
    return rs::used_letters(to_rust_bytes(str));
}

RankedMatch::RankedMatch(StringView candidate, StringView query)
{
    rs::Rank rank{};
    if (rs::ranked_match(to_rust_bytes(candidate), to_rust_bytes(query), rank))
        set_rank(candidate, rank);
}

RankedMatch::RankedMatch(StringView candidate, UsedLetters candidate_letters,
                         StringView query, UsedLetters query_letters)
{
    rs::Rank rank{};
    if (rs::ranked_match_with_letters(to_rust_bytes(candidate), candidate_letters,
                                      to_rust_bytes(query), query_letters, rank))
        set_rank(candidate, rank);
}

void RankedMatch::set_rank(StringView candidate, const rs::Rank& rank)
{
    m_candidate = candidate;
    m_matches = true;
    m_flags = rank.flags;
    m_full_word_match_count = rank.full_word_match_count;
    m_word_boundary_match_count = rank.word_boundary_match_count;
    m_max_index = rank.max_index;
    m_input_sequence_number = rank.input_sequence_number;
}

rs::Rank RankedMatch::rank() const
{
    return {m_flags, m_full_word_match_count, m_word_boundary_match_count,
            m_max_index, m_input_sequence_number};
}

bool RankedMatch::operator<(const RankedMatch& other) const
{
    kak_assert((bool)*this and (bool)other);
    return rs::ranked_match_less(to_rust_bytes(m_candidate), rank(),
                                 to_rust_bytes(other.m_candidate), other.rank());
}

// The matching and ranking tests are in Rust; this checks the shim.
UnitTest test_ranked_match{[] {
    kak_assert(RankedMatch{"source", "so"} < RankedMatch{"source_data", "so"});
    kak_assert(not (RankedMatch{"source_data", "so"} < RankedMatch{"source", "so"}));
    kak_assert(not RankedMatch{"source", "x"});
    kak_assert(RankedMatch{"FooBar", used_letters("FooBar"), "fb", used_letters("fb")});
    kak_assert(not RankedMatch{"foobar", used_letters("foobar"), "FB", used_letters("FB")});
}};

}
