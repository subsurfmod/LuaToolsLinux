use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn resolve_git_dirs() -> (Option<PathBuf>, Option<PathBuf>) {
    let dot_git = Path::new(".git");
    let git_dir = if dot_git.is_dir() {
        Some(dot_git.to_path_buf())
    } else if dot_git.is_file() {
        println!("cargo:rerun-if-changed=.git");
        fs::read_to_string(dot_git).ok().and_then(|content| {
            content.lines().find_map(|line| {
                let raw = line.trim().strip_prefix("gitdir:")?.trim();
                if raw.is_empty() {
                    return None;
                }
                let p = PathBuf::from(raw);
                Some(if p.is_absolute() {
                    p
                } else {
                    Path::new(".").join(p)
                })
            })
        })
    } else {
        None
    };

    let common_dir = git_dir.as_ref().and_then(|gd| {
        let commondir_file = gd.join("commondir");
        if commondir_file.is_file() {
            fs::read_to_string(&commondir_file).ok().and_then(|raw| {
                let rel = raw.trim();
                if rel.is_empty() {
                    return None;
                }
                let p = PathBuf::from(rel);
                Some(if p.is_absolute() { p } else { gd.join(p) })
            })
        } else {
            Some(gd.clone())
        }
    });

    (git_dir, common_dir)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let (git_dir, common_dir) = resolve_git_dirs();
    let mut watched = BTreeSet::new();
    let mut watch_if_exists = |path: PathBuf| {
        if path.exists() && watched.insert(path.clone()) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    };

    if let Some(ref gd) = git_dir {
        let head_path = gd.join("HEAD");
        watch_if_exists(head_path.clone());
        watch_if_exists(gd.join("logs/HEAD"));
        watch_if_exists(gd.join("packed-refs"));

        if let Some(ref cd) = common_dir {
            watch_if_exists(cd.join("packed-refs"));
        }

        if let Ok(head) = fs::read_to_string(&head_path) {
            if let Some(ref_path) = head.trim().strip_prefix("ref:") {
                let ref_path = ref_path.trim();
                if !ref_path.is_empty() {
                    watch_if_exists(gd.join(ref_path));
                    if let Some(ref cd) = common_dir {
                        watch_if_exists(cd.join(ref_path));
                    }
                }
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
