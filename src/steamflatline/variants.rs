//! SteamStub variant unpackers for steamflatline
//!
//! Implements unpackers for SteamStub variants 1.0, 2.0, 2.1, 3.0, 3.1 (x86 and x64).

use crate::steamflatline::pe::{PeFile, Architecture};
use crate::steamflatline::crypto::{steam_xor, xtea_decrypt_pass1, aes256_cbc_decrypt};
use crate::steamflatline::error::{SteamFlatlineError, Result};
use std::path::Path;

/// Trait for variant-specific unpackers
pub trait Unpacker: Send + Sync {
    /// Check if this unpacker can process the given file data
    fn can_process(&self, data: &[u8]) -> bool;

    /// Process the file and write unpacked output
    fn process(&self, data: &[u8], output_path: &Path) -> Result<()>;

    /// Get the variant name
    fn name(&self) -> &'static str;

    /// Get the variant description
    fn description(&self) -> &'static str;
}

/// All available unpackers
pub fn all() -> Vec<Box<dyn Unpacker>> {
    vec![
        Box::new(StubVariant30x64::new()),
        Box::new(StubVariant30x86::new()),
        Box::new(StubVariant31x64::new()),
        Box::new(StubVariant31x86::new()),
        Box::new(StubVariant21x86::new()),
        Box::new(StubVariant20x86::new()),
        Box::new(StubVariant10x86::new()),
    ]
}

/// SteamStub Variant 3.0 x64 unpacker
pub struct StubVariant30x64 {
    // Signature: 0xC0DEC0DE at entry point - 4
}

impl StubVariant30x64 {
    pub fn new() -> Self {
        Self {}
    }
}

impl Unpacker for StubVariant30x64 {
    fn name(&self) -> &'static str {
        "SteamStub Variant 3.0 (x64)"
    }

    fn description(&self) -> &'static str {
        "Unpacker for 64-bit SteamStub variant 3.0"
    }

    fn can_process(&self, data: &[u8]) -> bool {
        // Quick check: must be PE64 with .bind section
        if data.len() < 100 {
            return false;
        }

        // Check for MZ header
        if data[0..2] != [0x4D, 0x5A] {
            return false;
        }

        // Get PE offset
        let pe_offset = u32::from_le_bytes([data[60], data[61], data[62], data[63]]) as usize;
        if pe_offset + 4 > data.len() {
            return false;
        }

        // Check PE signature
        if &data[pe_offset..pe_offset+4] != b"PE\0\0" {
            return false;
        }

        // Check machine type (should be AMD64 = 0x8664)
        if pe_offset + 6 > data.len() {
            return false;
        }
        let machine = u16::from_le_bytes([data[pe_offset+4], data[pe_offset+5]]);
        if machine != 0x8664 {
            return false;
        }

        // Check for .bind section - need to parse sections
        // For quick check, look for .bind in the file
        data.windows(5).any(|w| w == b".bind")
    }

    fn process(&self, data: &[u8], output_path: &Path) -> Result<()> {
        // Write to temp file and parse with PeFile
        let temp_path = std::env::temp_dir().join(format!("steamflatline_{}.exe", std::process::id()));
        std::fs::write(&temp_path, data)?;

        let mut pe = PeFile::parse(&temp_path)?;

        if !matches!(pe.architecture, Architecture::Pe64) {
            return Err(SteamFlatlineError::UnsupportedArchitecture);
        }

        // Check for signature at entry point - 4
        let entry_rva = pe.optional_header.address_of_entry_point();
        let entry_offset = pe.rva_to_file_offset(entry_rva)
            .ok_or(SteamFlatlineError::MissingSection("Entry point".into()))?;

        if entry_offset < 4 {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }

        // Read the dword at entry - 4
        let mut sig_bytes = [0u8; 4];
        // We need to read from the file data
        let file_data = std::fs::read(&temp_path)?;
        if entry_offset as usize + 4 > file_data.len() {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }
        sig_bytes.copy_from_slice(&file_data[entry_offset as usize - 4..entry_offset as usize]);
        let signature = u32::from_le_bytes(sig_bytes);

        if signature != 0xC0DEC0DE {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }

        // Parse the SteamStub v3.0 header (at entry point)
        let header_size = 208; // Size of SteamStub64Var30Header
        let header_offset = entry_offset as usize;

        if header_offset + header_size > file_data.len() {
            return Err(SteamFlatlineError::HeaderParsingFailed("Header exceeds file size".into()));
        }

        let header_data = &file_data[header_offset..header_offset + header_size];

        // Parse header fields (SteamStub64Var30Header)
        let xor_key = u32::from_le_bytes([header_data[0], header_data[1], header_data[2], header_data[3]]);
        let _signature = u32::from_le_bytes([header_data[4], header_data[5], header_data[6], header_data[7]]);
        let _image_base = u64::from_le_bytes([
            header_data[8], header_data[9], header_data[10], header_data[11],
            header_data[12], header_data[13], header_data[14], header_data[15],
        ]);
        let _address_of_entry_point = u32::from_le_bytes([header_data[16], header_data[17], header_data[18], header_data[19]]);
        let bind_section_offset = u32::from_le_bytes([header_data[20], header_data[21], header_data[22], header_data[23]]);
        let _unknown0000 = u32::from_le_bytes([header_data[24], header_data[25], header_data[26], header_data[27]]);
        let original_entry_point = u32::from_le_bytes([header_data[28], header_data[29], header_data[30], header_data[31]]);
        let _unknown0001 = u32::from_le_bytes([header_data[32], header_data[33], header_data[34], header_data[35]]);
        let payload_size = u32::from_le_bytes([header_data[36], header_data[37], header_data[38], header_data[39]]);
        let drmp_dll_offset = u32::from_le_bytes([header_data[40], header_data[41], header_data[42], header_data[43]]);
        let drmp_dll_size = u32::from_le_bytes([header_data[44], header_data[45], header_data[46], header_data[47]]);
        let _steam_app_id = u32::from_le_bytes([header_data[48], header_data[49], header_data[50], header_data[51]]);
        let flags = u32::from_le_bytes([header_data[52], header_data[53], header_data[54], header_data[55]]);
        let _bind_section_virtual_size = u32::from_le_bytes([header_data[56], header_data[57], header_data[58], header_data[59]]);
        let _unknown0002 = u32::from_le_bytes([header_data[60], header_data[61], header_data[62], header_data[63]]);
        let code_section_va = u32::from_le_bytes([header_data[64], header_data[65], header_data[66], header_data[67]]);
        let code_section_raw_size = u32::from_le_bytes([header_data[68], header_data[69], header_data[70], header_data[71]]);

        // AES Key (32 bytes at offset 72)
        let mut aes_key = [0u8; 32];
        aes_key.copy_from_slice(&header_data[72..104]);

        // AES IV (16 bytes at offset 104)
        let mut aes_iv = [0u8; 16];
        aes_iv.copy_from_slice(&header_data[104..120]);

        // Code section stolen data (16 bytes at offset 120)
        let mut stolen_data = [0u8; 16];
        stolen_data.copy_from_slice(&header_data[120..136]);

        // Encryption keys (4 x u32 at offset 136)
        let mut enc_keys = [0u32; 4];
        for i in 0..4 {
            let off = 136 + i * 4;
            enc_keys[i] = u32::from_le_bytes([
                header_data[off], header_data[off+1], header_data[off+2], header_data[off+3]
            ]);
        }

        // Find payload data (at entry - bind_section_offset)
        let payload_rva = entry_rva.wrapping_sub(bind_section_offset);
        let payload_offset = pe.rva_to_file_offset(payload_rva)
            .ok_or(SteamFlatlineError::MissingSection("Payload".into()))?;

        // Read and decrypt payload
        let mut payload = vec![0u8; payload_size as usize];
        if payload_offset as usize + payload_size as usize > file_data.len() {
            return Err(SteamFlatlineError::PayloadDecryptionFailed("Payload exceeds file size".into()));
        }
        payload.copy_from_slice(&file_data[payload_offset as usize..payload_offset as usize + payload_size as usize]);

        // XOR decrypt payload
        steam_xor(&mut payload, xor_key);

        // Extract SteamDRMP.dll from payload
        let drmp_offset = drmp_dll_offset as usize;
        let drmp_size = drmp_dll_size as usize;

        if drmp_offset + drmp_size > payload.len() {
            return Err(SteamFlatlineError::DrmpDecryptionFailed("DRMP data exceeds payload".into()));
        }

        let mut drmp_data = payload[drmp_offset..drmp_offset + drmp_size].to_vec();

        // Decrypt SteamDRMP.dll using XTEA
        xtea_decrypt_pass1(&mut drmp_data, &enc_keys);

        // Find code section
        let code_section_idx = pe.sections.iter().position(|s| s.virtual_address == code_section_va)
            .ok_or(SteamFlatlineError::MissingSection(".text".into()))?;

        let code_section = &pe.sections[code_section_idx];

        // Check if encrypted
        const DRM_FLAG_NO_ENCRYPTION: u32 = 0x1;
        let is_encrypted = (flags & DRM_FLAG_NO_ENCRYPTION) == 0;

        let mut code_data = if is_encrypted {
            // Read encrypted section data
            let encrypted_size = code_section_raw_size;
            let mut encrypted_data = vec![0u8; (encrypted_size + 16) as usize];
            encrypted_data[0..16].copy_from_slice(&stolen_data);

            let section_file_offset = code_section.pointer_to_raw_data as usize;
            if section_file_offset + encrypted_size as usize > file_data.len() {
                return Err(SteamFlatlineError::AesDecryptionFailed("Code section exceeds file".into()));
            }
            encrypted_data[16..].copy_from_slice(&file_data[section_file_offset..section_file_offset + encrypted_size as usize]);

            // AES decrypt
            aes256_cbc_decrypt(&mut encrypted_data, &aes_key, &aes_iv)?;
            encrypted_data
        } else {
            // Not encrypted, read as-is
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            let section_size = code_section.size_of_raw_data as usize;
            file_data[section_file_offset..section_file_offset + section_size].to_vec()
        };

        // Compute new entry point RVA before mutating pe
        let new_entry_rva = pe.va_to_rva(original_entry_point as u64) as u32;

        // Update entry point to original BEFORE removing .bind section (indices stay stable)
        match &mut pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe64(h) => {
                h.address_of_entry_point = new_entry_rva;
                h.check_sum = 0;
            }
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        }

        // Update code section data
        if code_section_idx < pe.section_data.len() {
            let new_size = code_data.len().min(pe.section_data[code_section_idx].len());
            pe.section_data[code_section_idx][..new_size].copy_from_slice(&code_data[..new_size]);
        }

        // Remove .bind section if present (after updating entry point and code data)
        if pe.has_section(".bind") {
            let bind_idx = pe.sections.iter().position(|s| s.name_str() == ".bind")
                .ok_or(SteamFlatlineError::MissingSection(".bind".into()))?;
            pe.sections.remove(bind_idx);
            pe.section_data.remove(bind_idx);
            pe.file_header.number_of_sections -= 1;
        }

        // Write unpacked file
        pe.write(output_path)?;

        // Clean up temp
        let _ = std::fs::remove_file(&temp_path);

        Ok(())
    }
}

/// SteamStub Variant 3.0 x86 unpacker
pub struct StubVariant30x86;

impl StubVariant30x86 {
    pub fn new() -> Self {
        Self {}
    }
}

impl Unpacker for StubVariant30x86 {
    fn name(&self) -> &'static str {
        "SteamStub Variant 3.0 (x86)"
    }

    fn description(&self) -> &'static str {
        "Unpacker for 32-bit SteamStub variant 3.0"
    }

    fn can_process(&self, data: &[u8]) -> bool {
        if data.len() < 100 {
            return false;
        }

        if data[0..2] != [0x4D, 0x5A] {
            return false;
        }

        let pe_offset = u32::from_le_bytes([data[60], data[61], data[62], data[63]]) as usize;
        if pe_offset + 4 > data.len() {
            return false;
        }

        if &data[pe_offset..pe_offset+4] != b"PE\0\0" {
            return false;
        }

        if pe_offset + 6 > data.len() {
            return false;
        }
        let machine = u16::from_le_bytes([data[pe_offset+4], data[pe_offset+5]]);
        if machine != 0x014c {
            return false;
        }

        data.windows(5).any(|w| w == b".bind")
    }

    fn process(&self, _data: &[u8], _output_path: &Path) -> Result<()> {
        // TODO: Implement v3.0 x86 (similar to x64 but with 32-bit header)
        Err(SteamFlatlineError::UnsupportedFormat)
    }
}

/// SteamStub Variant 3.1 x64 unpacker
pub struct StubVariant31x64;

impl StubVariant31x64 {
    pub fn new() -> Self {
        Self {}
    }
}

impl Unpacker for StubVariant31x64 {
    fn name(&self) -> &'static str {
        "SteamStub Variant 3.1 (x64)"
    }

    fn description(&self) -> &'static str {
        "Unpacker for 64-bit SteamStub variant 3.1"
    }

    fn can_process(&self, data: &[u8]) -> bool {
        // Similar to v3.0 x64 but with different header
        StubVariant30x64::new().can_process(data)
    }

    fn process(&self, _data: &[u8], _output_path: &Path) -> Result<()> {
        Err(SteamFlatlineError::UnsupportedFormat)
    }
}

/// SteamStub Variant 3.1 x86 unpacker
pub struct StubVariant31x86;

impl StubVariant31x86 {
    pub fn new() -> Self {
        Self {}
    }
}

impl Unpacker for StubVariant31x86 {
    fn name(&self) -> &'static str {
        "SteamStub Variant 3.1 (x86)"
    }

    fn description(&self) -> &'static str {
        "Unpacker for 32-bit SteamStub variant 3.1"
    }

    fn can_process(&self, data: &[u8]) -> bool {
        StubVariant30x86::new().can_process(data)
    }

    fn process(&self, _data: &[u8], _output_path: &Path) -> Result<()> {
        Err(SteamFlatlineError::UnsupportedFormat)
    }
}

/// SteamStub Variant 2.1 x86 unpacker
pub struct StubVariant21x86;

impl StubVariant21x86 {
    pub fn new() -> Self {
        Self {}
    }
}

impl Unpacker for StubVariant21x86 {
    fn name(&self) -> &'static str {
        "SteamStub Variant 2.1 (x86)"
    }

    fn description(&self) -> &'static str {
        "Unpacker for 32-bit SteamStub variant 2.1"
    }

    fn can_process(&self, data: &[u8]) -> bool {
        if data.len() < 100 {
            return false;
        }

        if data[0..2] != [0x4D, 0x5A] {
            return false;
        }

        let pe_offset = u32::from_le_bytes([data[60], data[61], data[62], data[63]]) as usize;
        if pe_offset + 4 > data.len() {
            return false;
        }

        if &data[pe_offset..pe_offset+4] != b"PE\0\0" {
            return false;
        }

        if pe_offset + 6 > data.len() {
            return false;
        }
        let machine = u16::from_le_bytes([data[pe_offset+4], data[pe_offset+5]]);
        if machine != 0x014c {
            return false;
        }

        // Check for v2.1 pattern in .bind section: 53 51 52 56 57 55 8B EC 81 EC 00 10 00 00 C7
        data.windows(14).any(|w| w == [0x53, 0x51, 0x52, 0x56, 0x57, 0x55, 0x8B, 0xEC, 0x81, 0xEC, 0x00, 0x10, 0x00, 0x00])
    }

    fn process(&self, _data: &[u8], _output_path: &Path) -> Result<()> {
        // TODO: Implement v2.1 x86 (requires disassembly for header parsing)
        Err(SteamFlatlineError::UnsupportedFormat)
    }
}

/// SteamStub Variant 2.0 x86 unpacker
pub struct StubVariant20x86;

impl StubVariant20x86 {
    pub fn new() -> Self {
        Self {}
    }
}

impl Unpacker for StubVariant20x86 {
    fn name(&self) -> &'static str {
        "SteamStub Variant 2.0 (x86)"
    }

    fn description(&self) -> &'static str {
        "Unpacker for 32-bit SteamStub variant 2.0"
    }

    fn can_process(&self, data: &[u8]) -> bool {
        StubVariant21x86::new().can_process(data)
    }

    fn process(&self, _data: &[u8], _output_path: &Path) -> Result<()> {
        Err(SteamFlatlineError::UnsupportedFormat)
    }
}

/// SteamStub Variant 1.0 x86 unpacker
pub struct StubVariant10x86;

impl StubVariant10x86 {
    pub fn new() -> Self {
        Self {}
    }
}

impl Unpacker for StubVariant10x86 {
    fn name(&self) -> &'static str {
        "SteamStub Variant 1.0 (x86)"
    }

    fn description(&self) -> &'static str {
        "Unpacker for 32-bit SteamStub variant 1.0"
    }

    fn can_process(&self, data: &[u8]) -> bool {
        if data.len() < 100 {
            return false;
        }

        if data[0..2] != [0x4D, 0x5A] {
            return false;
        }

        let pe_offset = u32::from_le_bytes([data[60], data[61], data[62], data[63]]) as usize;
        if pe_offset + 4 > data.len() {
            return false;
        }

        if &data[pe_offset..pe_offset+4] != b"PE\0\0" {
            return false;
        }

        if pe_offset + 6 > data.len() {
            return false;
        }
        let machine = u16::from_le_bytes([data[pe_offset+4], data[pe_offset+5]]);
        if machine != 0x014c {
            return false;
        }

        // v1.0 has different characteristics - no .bind section typically
        // For now, return false to avoid false positives
        false
    }

    fn process(&self, _data: &[u8], _output_path: &Path) -> Result<()> {
        Err(SteamFlatlineError::UnsupportedFormat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unpackers_exist() {
        let unpackers = all();
        assert_eq!(unpackers.len(), 7);
        assert!(unpackers.iter().any(|u| u.name().contains("3.0")));
        assert!(unpackers.iter().any(|u| u.name().contains("2.1")));
    }
}