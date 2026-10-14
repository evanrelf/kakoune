//! Checks the port against upstream's C++ on generated inputs.

use super::{Rank, RankedMatch, used_letters};
use hegel::TestCase;
use hegel::generators as gs;
use kakoune_reference::{locale, ranked_match as reference};
use std::sync::Once;

/// Pieces of candidates and queries: ASCII that matters for ranking, word
/// separators, non-ASCII letters classified by the locale, and invalid UTF-8.
#[rustfmt::skip]
const TOKENS: &[&[u8]] = &[
    b"a", b"b", b"c", b"f", b"o", b"r", b"x", b"A", b"B", b"F", b"O", b"R", b"_", b"-", b"/", b".",
    b"1", b" ", b"(", b"foo", b"bar", b"Foo", "é".as_bytes(), "É".as_bytes(), "ß".as_bytes(),
    "ǅ".as_bytes(), "ﬀ".as_bytes(), "日".as_bytes(), "😀".as_bytes(), b"\xC3", b"\x80", b"\xFF",
    b"\xE2\x82", b"\xF0\x9F", b"\xC3a", b"\0",
];

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| assert!(locale::init_locale(), "needs a UTF-8 locale"));
}

/// Usually one of `TOKENS`, but sometimes any character, to find those the
/// locale and the port classify differently.
#[hegel::composite]
fn token(tc: &TestCase) -> Vec<u8> {
    if tc.draw(gs::weighted_booleans(0.1)) {
        tc.draw(gs::characters()).to_string().into_bytes()
    } else {
        tc.draw(gs::sampled_from(TOKENS)).to_vec()
    }
}

#[hegel::composite]
fn tokens(tc: &TestCase, min_size: usize, max_size: usize) -> Vec<u8> {
    let tokens = gs::vecs(token()).min_size(min_size).max_size(max_size);
    tc.draw(tokens).concat()
}

/// Half the queries are drawn from a candidate, so that they match.
#[hegel::composite]
fn query(tc: &TestCase, candidates: Vec<Vec<u8>>) -> Vec<u8> {
    if tc.draw(gs::booleans()) {
        return tc.draw(tokens(1, 4));
    }
    let source = tc.draw(gs::sampled_from(candidates));
    let mut query = Vec::new();
    for c in source {
        if tc.draw(gs::booleans()) {
            let upper = tc.draw(gs::weighted_booleans(0.25));
            query.push(if upper { c.to_ascii_uppercase() } else { c });
        }
    }
    query
}

/// Usually `query`'s tokens in order, with others around them and some
/// uppercased, so that most candidates match it in varied ways.
#[hegel::composite]
fn candidate_for(tc: &TestCase, query: Vec<Vec<u8>>) -> Vec<u8> {
    if tc.draw(gs::weighted_booleans(0.2)) {
        return tc.draw(tokens(0, 12));
    }
    let mut candidate = Vec::new();
    for token in query {
        candidate.extend(tc.draw(tokens(0, 2)));
        if tc.draw(gs::weighted_booleans(0.25)) {
            candidate.extend(token.to_ascii_uppercase());
        } else {
            candidate.extend(&token);
        }
    }
    candidate.extend(tc.draw(tokens(0, 3)));
    candidate
}

/// Usually the candidate's real letters, as callers pass, but sometimes
/// arbitrary ones to exercise the filter.
#[hegel::composite]
fn letters(tc: &TestCase, s: Vec<u8>) -> u64 {
    if tc.draw(gs::weighted_booleans(0.25)) {
        tc.draw(gs::integers())
    } else {
        used_letters(&s)
    }
}

fn from_reference(rank: reference::Rank) -> Rank {
    Rank {
        flags: rank.flags,
        full_word_match_count: rank.full_word_match_count,
        word_boundary_match_count: rank.word_boundary_match_count,
        max_index: rank.max_index,
        input_sequence_number: rank.input_sequence_number,
    }
}

fn reference_match(candidate: &[u8], query: &[u8]) -> Option<Rank> {
    let mut rank = reference::Rank::default();
    reference::ranked_match(candidate, query, &mut rank).then(|| from_reference(rank))
}

#[hegel::test]
fn matches_like_upstream(tc: TestCase) {
    init();
    let candidate = tc.draw(tokens(0, 12));
    let query = tc.draw(query(vec![candidate.clone()]));

    assert_eq!(
        used_letters(&candidate),
        reference::used_letters(&candidate)
    );
    let expected = reference_match(&candidate, &query);
    tc.event(match expected {
        Some(_) => "matches",
        None => "doesn't match",
    });
    assert_eq!(
        RankedMatch::new(&candidate, &query).map(|m| m.rank),
        expected
    );

    let candidate_letters = tc.draw(letters(candidate.clone()));
    let query_letters = tc.draw(letters(query.clone()));
    let mut rank = reference::Rank::default();
    let expected = reference::ranked_match_with_letters(
        &candidate,
        candidate_letters,
        &query,
        query_letters,
        &mut rank,
    )
    .then(|| from_reference(rank));
    assert_eq!(
        RankedMatch::with_letters(&candidate, candidate_letters, &query, query_letters)
            .map(|m| m.rank),
        expected,
    );
}

#[hegel::test]
fn orders_like_upstream(tc: TestCase) {
    init();
    let query_tokens = tc.draw(gs::vecs(token()).min_size(1).max_size(3));
    let candidates = tc.draw(gs::vecs(candidate_for(query_tokens.clone())).max_size(30));
    let query = query_tokens.concat();

    // Few distinct sequence numbers, so ties still reach the candidate
    // comparison
    let matches: Vec<RankedMatch> = candidates
        .iter()
        .filter_map(|candidate| RankedMatch::new(candidate, &query))
        .map(|mut m| {
            m.rank.input_sequence_number = tc.draw(gs::integers().max_value(2));
            m
        })
        .collect();
    tc.event_value("matches", matches.len() as f64);

    for lhs in &matches {
        for rhs in &matches {
            let expected = reference::ranked_match_less(
                &query,
                lhs.candidate,
                lhs.rank.input_sequence_number,
                rhs.candidate,
                rhs.rank.input_sequence_number,
            )
            .unwrap();
            assert_eq!(
                lhs.rank_cmp(rhs).is_lt(),
                expected,
                "{:?} < {:?}",
                lhs.candidate.escape_ascii().to_string(),
                rhs.candidate.escape_ascii().to_string(),
            );
        }
    }
}
