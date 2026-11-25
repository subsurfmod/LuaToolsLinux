//! Error types for steamflatline

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum SteamFlatlineError {
    IoError(io::Error),
    InvalidPeFormat(String),
    UnsupportedFormat,
    UnsupportedArchitecture,
    MissingSection(String),
    HeaderParsingFailed(String),
    PayloadDecryptionFailed(String),
    DrmpDecryptionFailed(String),
    AesDecryptionFailed(String),
    PatternNotFound(String),
    DisassemblyFailed(String),
    InvalidStubFormat,
    FileWriteFailed(String),
    ChecksumCalculationFailed,
}

impl fmt::Display for SteamFlatlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SteamFlatlineError::IoError(e) => write!(f, "IO error: {}", e),
            SteamFlatlineError::InvalidPeFormat(msg) => write!(f, "Invalid PE format: {}", msg),
            SteamFlatlineError::UnsupportedFormat => write!(f, "File is not protected with a supported SteamStub variant"),
            SteamFlatlineError::UnsupportedArchitecture => write!(f, "Unsupported architecture (only x86 and x64 supported)"),
            SteamFlatlineError::MissingSection(name) => write!(f, "Required section not found: {}", name),
            SteamFlatlineError::HeaderParsingFailed(msg) => write!(f, "Failed to parse SteamStub header: {}", msg),
            SteamFlatlineError::PayloadDecryptionFailed(msg) => write!(f, "Payload decryption failed: {}", msg),
            SteamFlatlineError::DrmpDecryptionFailed(msg) => write!(f, "SteamDRMP.dll decryption failed: {}", msg),
            SteamFlatlineError::AesDecryptionFailed(msg) => write!(f, "AES decryption failed: {}", msg),
            SteamFlatlineError::PatternNotFound(msg) => write!(f, "Required pattern not found: {}", msg),
            SteamFlatlineError::DisassemblyFailed(msg) => write!(f, "Disassembly failed: {}", msg),
            SteamFlatlineError::InvalidStubFormat => write!(f, "Invalid SteamStub format"),
            SteamFlatlineError::FileWriteFailed(msg) => write!(f, "Failed to write unpacked file: {}", msg),
            SteamFlatlineError::ChecksumCalculationFailed => write!(f, "Failed to calculate PE checksum"),
        }
    }
}

impl std::error::Error for SteamFlatlineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SteamFlatlineError::IoError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for SteamFlatlineError {
    fn from(err: io::Error) -> Self {
        SteamFlatlineError::IoError(err)
    }
}

pub type Result<T> = std::result::Result<T, SteamFlatlineError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = SteamFlatlineError::InvalidPeFormat("test".into());
        assert!(err.to_string().contains("Invalid PE format"));
    }
}