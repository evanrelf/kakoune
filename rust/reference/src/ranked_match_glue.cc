#include "ranked_match_glue.hh"

#include "kakoune-reference/src/ranked_match.rs.h"

// Upstream's, from the include path
#include "ranked_match.hh"

#include <stdexcept>

namespace Kakoune::reference
{

static StringView to_string_view(rust::Slice<const uint8_t> s)
{
    return {reinterpret_cast<const char*>(s.data()), (int)s.size()};
}

// Explicit instantiations may name private members, so this reads
// RankedMatch's fields without changing upstream's header.
template<typename Tag, auto member>
struct Expose
{
    friend constexpr auto get(Tag) { return member; }
};

#define EXPOSE(field) \
    struct field##_tag { friend constexpr auto get(field##_tag); }; \
    template struct Expose<field##_tag, &RankedMatch::field>;

EXPOSE(m_flags)
EXPOSE(m_full_word_match_count)
EXPOSE(m_word_boundary_match_count)
EXPOSE(m_max_index)
EXPOSE(m_input_sequence_number)

#undef EXPOSE

static bool fill_rank(const RankedMatch& m, Rank& rank)
{
    if (not m)
        return false;
    rank = {(uint8_t)(int)(m.*get(m_flags_tag{})),
            m.*get(m_full_word_match_count_tag{}),
            m.*get(m_word_boundary_match_count_tag{}),
            m.*get(m_max_index_tag{}),
            m.*get(m_input_sequence_number_tag{})};
    return true;
}

uint64_t used_letters(rust::Slice<const uint8_t> s)
{
    return Kakoune::used_letters(to_string_view(s));
}

bool ranked_match(rust::Slice<const uint8_t> candidate, rust::Slice<const uint8_t> query,
                  Rank& rank)
{
    return fill_rank(RankedMatch{to_string_view(candidate), to_string_view(query)}, rank);
}

bool ranked_match_with_letters(rust::Slice<const uint8_t> candidate, uint64_t candidate_letters,
                               rust::Slice<const uint8_t> query, uint64_t query_letters,
                               Rank& rank)
{
    return fill_rank(RankedMatch{to_string_view(candidate), candidate_letters,
                                 to_string_view(query), query_letters}, rank);
}

bool ranked_match_less(rust::Slice<const uint8_t> query,
                       rust::Slice<const uint8_t> lhs_candidate, size_t lhs_input_sequence_number,
                       rust::Slice<const uint8_t> rhs_candidate, size_t rhs_input_sequence_number)
{
    RankedMatch lhs{to_string_view(lhs_candidate), to_string_view(query)};
    RankedMatch rhs{to_string_view(rhs_candidate), to_string_view(query)};
    if (not lhs or not rhs)
        throw std::invalid_argument("ranked_match_less: query must match both candidates");
    lhs.set_input_sequence_number(lhs_input_sequence_number);
    rhs.set_input_sequence_number(rhs_input_sequence_number);
    return lhs < rhs;
}

}
