use std::{env, fs, io, path::Path};

/// Modules containing a `#[cxx::bridge]`.
const BRIDGES: &[&str] = &["src/ranked_match.rs", "src/smoke_test.rs", "src/unicode.rs"];

fn main() {
    let debug = env::var("PROFILE").unwrap() == "debug";

    let mut build = cxx_build::bridges(BRIDGES);
    // Bridges may include Kakoune headers, so compile the generated glue the
    // same way the Makefile compiles Kakoune.
    build.std("c++2b").include("../src");
    if debug {
        build.define("KAK_DEBUG", None);
    }
    build.compile("kakoune-bridge");

    for bridge in BRIDGES {
        println!("cargo:rerun-if-changed={bridge}");
    }
    for entry in fs::read_dir("../src").unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "hh") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    // cxx-build writes the generated headers under OUT_DIR, whose path Make
    // can't know, so copy them to a directory Make asks for.
    println!("cargo:rerun-if-env-changed=KAK_RUST_INCLUDE_DIR");
    if let Some(dest) = env::var_os("KAK_RUST_INCLUDE_DIR") {
        let src = Path::new(&env::var_os("OUT_DIR").unwrap()).join("cxxbridge/include");
        copy_headers(&src, Path::new(&dest)).expect("failed to copy generated headers");
    }
}

/// Copy `.h` files from `src` to `dest`, following symlinks, and only writing
/// files whose contents changed so Make doesn't rebuild needlessly.
fn copy_headers(src: &Path, dest: &Path) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let (src, dest) = (entry.path(), dest.join(entry.file_name()));
        if fs::metadata(&src)?.is_dir() {
            copy_headers(&src, &dest)?;
        } else if src.extension().is_some_and(|ext| ext == "h") {
            let contents = fs::read(&src)?;
            if fs::read(&dest).ok().as_ref() != Some(&contents) {
                fs::create_dir_all(dest.parent().unwrap())?;
                fs::write(&dest, contents)?;
            }
        }
    }
    Ok(())
}
