// Embed the version like the C++ Makefile does: the .version file at the
// repository root if present, `git describe --tags HEAD` otherwise
fn main() {
    let version = std::fs::read_to_string("../.version")
        .ok()
        .map(|v| v.trim().to_string())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["describe", "--tags", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_start_matches('v').to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KAK_VERSION={version}");
    println!("cargo:rerun-if-changed=../.version");
    println!("cargo:rerun-if-changed=../.git/HEAD");
}
