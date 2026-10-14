#[cxx::bridge(namespace = "Kakoune::reference")]
mod ffi {
    unsafe extern "C++" {
        include!("kakoune-reference/src/locale_glue.hh");

        fn init_locale() -> bool;
    }
}

/// Sets the locale from the environment, as `kak` does at startup, and
/// returns whether it classifies non-ASCII letters, which tests of
/// character classification need.
pub fn init_locale() -> bool {
    ffi::init_locale()
}
