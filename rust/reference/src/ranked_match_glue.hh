#ifndef ranked_match_glue_hh_INCLUDED
#define ranked_match_glue_hh_INCLUDED

#include "rust/cxx.h"

#include <cstddef>
#include <cstdint>

namespace Kakoune::reference
{

struct Rank;

uint64_t used_letters(rust::Slice<const uint8_t> s);
bool ranked_match(rust::Slice<const uint8_t> candidate, rust::Slice<const uint8_t> query,
                  Rank& rank);
bool ranked_match_with_letters(rust::Slice<const uint8_t> candidate, uint64_t candidate_letters,
                               rust::Slice<const uint8_t> query, uint64_t query_letters,
                               Rank& rank);
bool ranked_match_less(rust::Slice<const uint8_t> query,
                       rust::Slice<const uint8_t> lhs_candidate, size_t lhs_input_sequence_number,
                       rust::Slice<const uint8_t> rhs_candidate, size_t rhs_input_sequence_number);

}

#endif // ranked_match_glue_hh_INCLUDED
