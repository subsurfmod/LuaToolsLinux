use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for git_path in [".git/HEAD", ".git/refs/heads/main", ".git/packed-refs"] {
        if Path::new(git_path).exists() {
            println!("cargo:rerun-if-changed={}", git_path);
        }
    }

    let git_sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    println!("cargo:rustc-env=LUATOOLS_BUILD_GIT_SHA={}", git_sha);
}
