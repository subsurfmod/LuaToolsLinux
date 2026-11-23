//! Goldberg Steam Emulator provider (supports classic Goldberg & gbe_fork Linux/Proton layouts)

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GoldbergProvider {
    path: PathBuf,
    regular_x86: PathBuf,
    regular_x64: PathBuf,
    experimental_x86: PathBuf,
    experimental_x64: PathBuf,
    regular_dll_x86: PathBuf,
    regular_dll_x64: PathBuf,
    experimental_dll_x86: PathBuf,
    experimental_dll_x64: PathBuf,
}

fn resolve_first_existing(base: &Path, rel_candidates: &[&str], fallback: &str) -> PathBuf {
    for rel in rel_candidates {
        let candidate = base.join(rel);
        if candidate.is_file() {
            return candidate;
        }
    }
    base.join(fallback)
}

impl GoldbergProvider {
    pub fn new(base_path: &Path) -> anyhow::Result<Self> {
        let path = base_path.to_path_buf();

        let regular_x86 = resolve_first_existing(
            &path,
            &[
                "regular/x86/steam_api.so",
                "regular/x86/libsteam_api.so",
                "regular/x32/libsteam_api.so",
                "linux/x86/libsteam_api.so",
                "release/regular/x32/libsteam_api.so",
                "release/regular/x86/libsteam_api.so",
            ],
            "regular/x86/steam_api.so",
        );

        let regular_x64 = resolve_first_existing(
            &path,
            &[
                "regular/x64/steam_api64.so",
                "regular/x64/libsteam_api.so",
                "linux/x86_64/libsteam_api.so",
                "release/regular/x64/libsteam_api.so",
                "libsteam_api.so",
                "steam_api64.so",
            ],
            "regular/x64/steam_api64.so",
        );

        let experimental_x86 = resolve_first_existing(
            &path,
            &[
                "experimental/x86/steam_api.so",
                "experimental/x86/libsteam_api.so",
                "experimental/x32/libsteam_api.so",
                "release/experimental/x32/libsteam_api.so",
            ],
            "experimental/x86/steam_api.so",
        );

        let experimental_x64 = resolve_first_existing(
            &path,
            &[
                "experimental/x64/steam_api64.so",
                "experimental/x64/libsteam_api.so",
                "release/experimental/x64/libsteam_api.so",
            ],
            "experimental/x64/steam_api64.so",
        );

        let regular_dll_x86 = resolve_first_existing(
            &path,
            &[
                "regular/x86/steam_api.dll",
                "regular/x32/steam_api.dll",
                "release/regular/x32/steam_api.dll",
                "steam_api.dll",
            ],
            "regular/x86/steam_api.dll",
        );

        let regular_dll_x64 = resolve_first_existing(
            &path,
            &[
                "regular/x64/steam_api64.dll",
                "release/regular/x64/steam_api64.dll",
                "steam_api64.dll",
            ],
            "regular/x64/steam_api64.dll",
        );

        let experimental_dll_x86 = resolve_first_existing(
            &path,
            &[
                "experimental/x86/steam_api.dll",
                "experimental/x32/steam_api.dll",
                "release/experimental/x32/steam_api.dll",
            ],
            "experimental/x86/steam_api.dll",
        );

        let experimental_dll_x64 = resolve_first_existing(
            &path,
            &[
                "experimental/x64/steam_api64.dll",
                "release/experimental/x64/steam_api64.dll",
            ],
            "experimental/x64/steam_api64.dll",
        );

        Ok(Self {
            path,
            regular_x86,
            regular_x64,
            experimental_x86,
            experimental_x64,
            regular_dll_x86,
            regular_dll_x64,
            experimental_dll_x86,
            experimental_dll_x64,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn regular_x86(&self) -> &Path {
        &self.regular_x86
    }

    pub fn regular_x64(&self) -> &Path {
        &self.regular_x64
    }

    pub fn experimental_x86(&self) -> &Path {
        &self.experimental_x86
    }

    pub fn experimental_x64(&self) -> &Path {
        &self.experimental_x64
    }

    pub fn is_valid(&self) -> bool {
        self.regular_x64.is_file()
            || self.regular_x86.is_file()
            || self.experimental_x64.is_file()
            || self.experimental_x86.is_file()
            || self.regular_dll_x64.is_file()
            || self.regular_dll_x86.is_file()
    }

    pub fn get_so_path(&self, is_64bit: bool, experimental: bool) -> &Path {
        match (is_64bit, experimental) {
            (true, true) if self.experimental_x64.is_file() => &self.experimental_x64,
            (true, _) => &self.regular_x64,
            (false, true) if self.experimental_x86.is_file() => &self.experimental_x86,
            (false, _) => &self.regular_x86,
        }
    }

    pub fn get_dll_path(&self, is_64bit: bool, experimental: bool) -> &Path {
        match (is_64bit, experimental) {
            (true, true) if self.experimental_dll_x64.is_file() => &self.experimental_dll_x64,
            (true, _) => &self.regular_dll_x64,
            (false, true) if self.experimental_dll_x86.is_file() => &self.experimental_dll_x86,
            (false, _) => &self.regular_dll_x86,
        }
    }
}
