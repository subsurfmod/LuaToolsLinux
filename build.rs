use std::fs;
use std::process::Command;

fn main() {
    for path in [
        "build.rs",
        ".git/HEAD",
        ".git/logs/HEAD",
        ".git/packed-refs",
        ".git/refs/heads",
        ".git/refs/heads/main",
    ] {
        println!("cargo:rerun-if-changed={}", path);
    }

    if let Ok(head) = fs::read_to_string(".git/HEAD") {
        if let Some(ref_path) = head.trim().strip_prefix("ref:") {
            let ref_path = ref_path.trim();
            if !ref_path.is_empty() && ref_path != "refs/heads/main" {
                println!("cargo:rerun-if-changed=.git/{}", ref_path);
            }
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
