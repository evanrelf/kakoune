//! Splitting text into words, mirroring `src/word_splitter.hh`.

use crate::unicode::{Codepoint, is_word};
use crate::utf8::read_codepoint;

/// Words longer than this many bytes are skipped.
pub const MAX_WORD_LEN: usize = 100;

/// The words of `content`: maximal runs of word characters, as judged by
/// `is_word` with `extra_word_chars`.
pub fn words<'a>(
    content: &'a [u8],
    extra_word_chars: &'a [Codepoint],
) -> impl Iterator<Item = &'a [u8]> {
    let mut word_end = 0;
    std::iter::from_fn(move || {
        loop {
            let mut pos = word_end;
            let mut word_begin = pos;
            while pos != content.len()
                && !is_word(read_codepoint(content, &mut pos), extra_word_chars)
            {
                word_begin = pos;
            }
            word_end = pos;
            while pos != content.len()
                && is_word(read_codepoint(content, &mut pos), extra_word_chars)
            {
                word_end = pos;
            }

            if word_begin == content.len() {
                return None;
            }
            if word_end - word_begin <= MAX_WORD_LEN {
                return Some(&content[word_begin..word_end]);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split<'a>(s: &'a str, extra: &'a [Codepoint]) -> Vec<&'a str> {
        words(s.as_bytes(), extra)
            .map(|w| std::str::from_utf8(w).unwrap())
            .collect()
    }

    #[test]
    fn splits_words() {
        assert_eq!(split("  foo_bar, baz!", &[]), ["foo", "bar", "baz"]);
        assert_eq!(
            split("foo_bar baz", &['_' as Codepoint]),
            ["foo_bar", "baz"]
        );
        assert_eq!(split("", &[]), Vec::<&str>::new());
        assert_eq!(split("...", &[]), Vec::<&str>::new());
    }

    #[test]
    fn skips_long_words() {
        let long = "x".repeat(MAX_WORD_LEN + 1);
        let max = "y".repeat(MAX_WORD_LEN);
        assert_eq!(split(&format!("a {long} {max} b"), &[]), ["a", &max, "b"]);
    }
}
