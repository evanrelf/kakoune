#ifndef ranked_match_hh_INCLUDED
#define ranked_match_hh_INCLUDED

#include "string.hh"

#include <cstdint>

namespace Kakoune
{

namespace rs { struct Rank; }

using UsedLetters = uint64_t;
UsedLetters used_letters(StringView str);

// Matching and ranking are implemented in rust/src/ranked_match.rs
struct RankedMatch
{
    RankedMatch(StringView candidate, StringView query);
    RankedMatch(StringView candidate, UsedLetters candidate_letters,
                StringView query, UsedLetters query_letters);

    const StringView& candidate() const { return m_candidate; }
    bool operator<(const RankedMatch& other) const;
    bool operator==(const RankedMatch& other) const { return m_candidate == other.m_candidate; }

    explicit operator bool() const { return m_matches; }

    void set_input_sequence_number(size_t i) { m_input_sequence_number = i; }

private:
    void set_rank(StringView candidate, const rs::Rank& rank);
    rs::Rank rank() const;

    StringView m_candidate{};
    bool m_matches = false;
    uint8_t m_flags = 0;
    int m_full_word_match_count = 0;
    int m_word_boundary_match_count = 0;
    int m_max_index = 0;
    size_t m_input_sequence_number = 0;
};

}

#endif // ranked_match_hh_INCLUDED
