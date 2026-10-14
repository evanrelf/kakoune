//! Exercises the build plumbing in both directions: C++ calls into Rust, which
//! calls back into C++. Remove once real code crosses the bridge.

#[cxx::bridge(namespace = "Kakoune::rs")]
mod ffi {
    extern "Rust" {
        fn count_word_bytes(s: &[u8]) -> usize;
    }

    unsafe extern "C++" {
        include!("rust_bridge.hh");

        fn is_word(c: u32) -> bool;
    }
}

/// Number of bytes in `s` that Kakoune considers word characters.
fn count_word_bytes(s: &[u8]) -> usize {
    s.iter().filter(|&&b| ffi::is_word(b.into())).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_word_bytes() {
        assert_eq!(count_word_bytes(b"foo_bar baz!"), 10);
    }
}
