// Embed the version like the C++ Makefile does (git describe --tags HEAD)
fn main() {
    let version = std::process::Command::new("git")
        .args(["describe", "--tags", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_start_matches('v').to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KAK_VERSION={version}");
    println!("cargo:rerun-if-changed=../.git/HEAD");
}
