//! Upstream `ranked_match.cc`, with the same signatures as the port's
//! bridge so tests can call either.

#[cxx::bridge(namespace = "Kakoune::reference")]
mod ffi {
    /// `RankedMatch`'s private fields, minus the candidate.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct Rank {
        flags: u8,
        full_word_match_count: i32,
        word_boundary_match_count: i32,
        max_index: i32,
        input_sequence_number: usize,
    }

    unsafe extern "C++" {
        include!("kakoune-reference/src/ranked_match_glue.hh");

        fn used_letters(s: &[u8]) -> u64;
        fn ranked_match(candidate: &[u8], query: &[u8], rank: &mut Rank) -> bool;
        fn ranked_match_with_letters(
            candidate: &[u8],
            candidate_letters: u64,
            query: &[u8],
            query_letters: u64,
            rank: &mut Rank,
        ) -> bool;
        /// Matches both candidates against `query`, which must match them,
        /// and compares the results.
        fn ranked_match_less(
            query: &[u8],
            lhs_candidate: &[u8],
            lhs_input_sequence_number: usize,
            rhs_candidate: &[u8],
            rhs_input_sequence_number: usize,
        ) -> Result<bool>;
    }
}

pub use ffi::*;
