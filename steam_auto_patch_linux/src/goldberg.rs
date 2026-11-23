//! Goldberg Steam Emulator provider

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct GoldbergProvider {
    path: PathBuf,
    regular_x86: PathBuf,
    regular_x64: PathBuf,
    experimental_x86: PathBuf,
    experimental_x64: PathBuf,
}

impl GoldbergProvider {
    pub fn new(base_path: &PathBuf) -> anyhow::Result<Self> {
        let path = base_path.clone();
        let regular_x86 = path.join("regular").join("x86").join("steam_api.so");
        let regular_x64 = path.join("regular").join("x64").join("steam_api64.so");
        let experimental_x86 = path.join("experimental").join("x86").join("steam_api.so");
        let experimental_x64 = path.join("experimental").join("x64").join("steam_api64.so");

        Ok(Self {
            path,
            regular_x86,
            regular_x64,
            experimental_x86,
            experimental_x64,
        })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn regular_x86(&self) -> &PathBuf {
        &self.regular_x86
    }

    pub fn regular_x64(&self) -> &PathBuf {
        &self.regular_x64
    }

    pub fn experimental_x86(&self) -> &PathBuf {
        &self.experimental_x86
    }

    pub fn experimental_x64(&self) -> &PathBuf {
        &self.experimental_x64
    }

    pub fn is_valid(&self) -> bool {
        self.regular_x86.exists()
            && self.regular_x64.exists()
            && self.experimental_x86.exists()
            && self.experimental_x64.exists()
    }

    pub fn get_so_path(&self, is_64bit: bool, experimental: bool) -> &PathBuf {
        match (is_64bit, experimental) {
            (true, true) => &self.experimental_x64,
            (true, false) => &self.regular_x64,
            (false, true) => &self.experimental_x86,
            (false, false) => &self.regular_x86,
        }
    }
}