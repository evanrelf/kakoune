use std::{env, path::PathBuf};

/// Modules containing a `#[cxx::bridge]`, each with C++ glue in
/// `<module>_glue.{hh,cc}`. The suffix keeps the glue from shadowing
/// upstream headers of the same name.
const BRIDGES: &[&str] = &["locale", "ranked_match"];

/// Upstream `.cc` files the bridges need, relative to upstream's `src/`.
const UPSTREAM_SOURCES: &[&str] = &["hash.cc", "memory.cc", "ranked_match.cc", "string.cc"];

fn main() {
    println!("cargo:rerun-if-env-changed=KAKOUNE_UPSTREAM_SRC");
    let upstream = PathBuf::from(env::var_os("KAKOUNE_UPSTREAM_SRC").expect(
        "KAKOUNE_UPSTREAM_SRC must point at upstream Kakoune's source; run inside `nix develop`",
    ))
    .join("src");

    let mut build = cxx_build::bridges(BRIDGES.iter().map(|b| format!("src/{b}.rs")));
    build
        .files(BRIDGES.iter().map(|b| format!("src/{b}_glue.cc")))
        .files(UPSTREAM_SOURCES.iter().map(|s| upstream.join(s)))
        .std("c++2b")
        .include(&upstream)
        // Renaming the namespace lets this link alongside the port's C++
        // shims, which define the same classes.
        .define("Kakoune", "KakouneReference")
        // Upstream's own code, not ours to fix
        .warnings(false);
    // Not built with `KAK_DEBUG`: upstream's assertions report through its
    // debug buffer, which would pull in most of the editor.
    build.compile("kakoune-reference");

    for bridge in BRIDGES {
        println!("cargo:rerun-if-changed=src/{bridge}.rs");
        println!("cargo:rerun-if-changed=src/{bridge}_glue.hh");
        println!("cargo:rerun-if-changed=src/{bridge}_glue.cc");
    }
}
