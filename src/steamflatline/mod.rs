//! steamflatline - Native Rust port of Steamless (SteamStub DRM removal)
//!
//! This module provides a native Rust implementation for unpacking SteamStub-protected
//! executables (variants 1.0, 2.0, 2.1, 3.0, 3.1 for both x86 and x64).
//!
//! Based on atom0s/Steamless (CC BY-NC-ND 4.0) - ported to Rust with permission for
//! interoperability within LuaTools Linux.

pub mod pe;
pub mod crypto;
pub mod variants;
pub mod error;

use std::path::Path;
use crate::steamflatline::error::SteamFlatlineError;
use crate::steamflatline::variants::{Unpacker, all};

/// Main entry point for steamflatline unpacking.
///
/// Attempts to unpack a SteamStub-protected executable using all known variants.
/// Returns the path to the unpacked executable on success.
pub fn unpack<P: AsRef<Path>>(exe_path: P) -> Result<std::path::PathBuf, SteamFlatlineError> {
    let exe_path = exe_path.as_ref();

    // Read the file
    let data = std::fs::read(exe_path)?;

    // Try each variant unpacker
    let unpackers = all();
    for unpacker in unpackers {
        if unpacker.can_process(&data) {
            let output_path = exe_path.with_extension("unpacked.exe");
            unpacker.process(&data, &output_path)?;
            return Ok(output_path);
        }
    }

    Err(SteamFlatlineError::UnsupportedFormat)
}

/// Check if a file is SteamStub-protected without unpacking.
pub fn is_protected<P: AsRef<Path>>(exe_path: P) -> bool {
    let exe_path = exe_path.as_ref();
    if let Ok(data) = std::fs::read(exe_path) {
        all().iter().any(|u| u.can_process(&data))
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unpacker_module_compiles() {
        // Just verify the module compiles
        assert!(true);
    }
}