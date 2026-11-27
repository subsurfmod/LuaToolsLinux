//! SteamStub variant unpackers for steamflatline
//!
//! Implements unpackers for SteamStub variants 1.0, 2.0, 2.1, 3.0, 3.1 (x86 and x64).

use crate::steamflatline::pe::{PeFile, Architecture};
use crate::steamflatline::crypto::{steam_xor, xtea_decrypt_pass1, aes256_cbc_decrypt, find_pattern};
use crate::steamflatline::error::{SteamFlatlineError, Result};
use std::path::{Path, PathBuf};

struct TempPeGuard(PathBuf);

impl TempPeGuard {
    fn create(data: &[u8]) -> Result<Self> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "steamflatline_{}_{}_{}.exe",
            std::process::id(),
            stamp,
            count
        ));
        std::fs::write(&path, data)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

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
        if !data.windows(5).any(|w| w == b".bind") {
            return false;
        }

        // Check for v3.0 signature 0xC0DEC0DE at entry point - 4
        // We need to find entry point first - for a quick check, look for the signature in the file
        data.windows(4).any(|w| w == &[0xDE, 0xC0, 0xDE, 0xC0])
    }

    fn process(&self, data: &[u8], output_path: &Path) -> Result<()> {
        // Write to unique temp file and parse with PeFile (auto-cleaned up on drop)
        let temp_guard = TempPeGuard::create(data)?;
        let mut pe = PeFile::parse(temp_guard.path())?;

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
        let file_data = data;
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
        let p_offset = payload_offset as usize;
        let p_size = payload_size as usize;
        if p_offset.checked_add(p_size).map_or(true, |end| end > file_data.len()) {
            return Err(SteamFlatlineError::PayloadDecryptionFailed("Payload exceeds file size".into()));
        }
        let mut payload = file_data[p_offset..p_offset + p_size].to_vec();

        // XOR decrypt payload
        steam_xor(&mut payload, xor_key);

        // Extract SteamDRMP.dll from payload
        let drmp_offset = drmp_dll_offset as usize;
        let drmp_size = drmp_dll_size as usize;

        if drmp_offset.checked_add(drmp_size).map_or(true, |end| end > payload.len()) {
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

        let code_data = if is_encrypted {
            // Read encrypted section data
            let encrypted_size = code_section_raw_size as usize;
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            if section_file_offset.checked_add(encrypted_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::AesDecryptionFailed("Code section exceeds file".into()));
            }
            let mut encrypted_data = vec![0u8; encrypted_size + 16];
            encrypted_data[0..16].copy_from_slice(&stolen_data);
            encrypted_data[16..].copy_from_slice(&file_data[section_file_offset..section_file_offset + encrypted_size]);

            // AES decrypt (IV is modified during RebuildIv)
            aes256_cbc_decrypt(&mut encrypted_data, &aes_key, &mut aes_iv)?;
            encrypted_data
        } else {
            // Not encrypted, read as-is
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            let section_size = code_section.size_of_raw_data as usize;
            if section_file_offset.checked_add(section_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::InvalidPeFormat("Code section exceeds file".into()));
            }
            file_data[section_file_offset..section_file_offset + section_size].to_vec()
        };

        // Compute new entry point RVA before mutating pe
        // original_entry_point in SteamStub header is already an RVA, not a VA
        let image_base = match &pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe64(h) => h.image_base,
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        };
        let new_entry_rva = if (original_entry_point as u64) >= image_base {
            (original_entry_point as u64 - image_base) as u32
        } else {
            original_entry_point
        };

        // Update entry point to original BEFORE removing .bind section (indices stay stable)
        match &mut pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe64(h) => {
                h.address_of_entry_point = new_entry_rva;
                h.check_sum = 0;
            }
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        }

        // Update code section data - use full code_data without truncation
        if code_section_idx < pe.section_data.len() {
            pe.section_data[code_section_idx] = code_data.clone();
            // Update section header sizes
            pe.sections[code_section_idx].size_of_raw_data = code_data.len() as u32;
            pe.sections[code_section_idx].virtual_size = code_data.len() as u32;
        }

        // Remove .bind section if present (after updating entry point and code data)
        if pe.has_section(".bind") {
            let bind_idx = pe.sections.iter().position(|s| s.name_str() == ".bind")
                .ok_or(SteamFlatlineError::MissingSection(".bind".into()))?;
            pe.sections.remove(bind_idx);
            pe.section_data.remove(bind_idx);
            pe.file_header.number_of_sections -= 1;

            // Recalculate size_of_image after removing .bind section
            let section_alignment = match &pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe64(h) => h.section_alignment,
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
            let max_va_end = pe.sections.iter()
                .map(|s| s.virtual_address + s.virtual_size.max(s.size_of_raw_data))
                .max()
                .unwrap_or(0);
            let aligned_size_of_image = ((max_va_end + section_alignment - 1) / section_alignment) * section_alignment;
            match &mut pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe64(h) => {
                    h.size_of_image = aligned_size_of_image;
                }
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
        }

        // Write unpacked file
        pe.write(output_path)?;

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

        if !data.windows(5).any(|w| w == b".bind") {
            return false;
        }

        // Check for v3.0 signature 0xC0DEC0DE
        data.windows(4).any(|w| w == &[0xDE, 0xC0, 0xDE, 0xC0])
    }

    fn process(&self, data: &[u8], output_path: &Path) -> Result<()> {
        // Write to unique temp file and parse with PeFile (auto-cleaned up on drop)
        let temp_guard = TempPeGuard::create(data)?;
        let mut pe = PeFile::parse(temp_guard.path())?;

        if !matches!(pe.architecture, Architecture::Pe32) {
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
        let file_data = data;
        if entry_offset as usize + 4 > file_data.len() {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }
        sig_bytes.copy_from_slice(&file_data[entry_offset as usize - 4..entry_offset as usize]);
        let signature = u32::from_le_bytes(sig_bytes);

        if signature != 0xC0DEC0DE {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }

        // Parse the SteamStub v3.0 header (at entry point) - 32-bit version
        // SteamStub32Var30Header is 196 bytes (smaller than x64 due to 32-bit image_base)
        let header_size = 196;
        let header_offset = entry_offset as usize;

        if header_offset + header_size > file_data.len() {
            return Err(SteamFlatlineError::HeaderParsingFailed("Header exceeds file size".into()));
        }

        let header_data = &file_data[header_offset..header_offset + header_size];

        // Parse header fields (SteamStub32Var30Header)
        let xor_key = u32::from_le_bytes([header_data[0], header_data[1], header_data[2], header_data[3]]);
        let _signature = u32::from_le_bytes([header_data[4], header_data[5], header_data[6], header_data[7]]);
        let _image_base = u32::from_le_bytes([header_data[8], header_data[9], header_data[10], header_data[11]]);
        let _address_of_entry_point = u32::from_le_bytes([header_data[12], header_data[13], header_data[14], header_data[15]]);
        let bind_section_offset = u32::from_le_bytes([header_data[16], header_data[17], header_data[18], header_data[19]]);
        let _unknown0000 = u32::from_le_bytes([header_data[20], header_data[21], header_data[22], header_data[23]]);
        let original_entry_point = u32::from_le_bytes([header_data[24], header_data[25], header_data[26], header_data[27]]);
        let _unknown0001 = u32::from_le_bytes([header_data[28], header_data[29], header_data[30], header_data[31]]);
        let payload_size = u32::from_le_bytes([header_data[32], header_data[33], header_data[34], header_data[35]]);
        let drmp_dll_offset = u32::from_le_bytes([header_data[36], header_data[37], header_data[38], header_data[39]]);
        let drmp_dll_size = u32::from_le_bytes([header_data[40], header_data[41], header_data[42], header_data[43]]);
        let _steam_app_id = u32::from_le_bytes([header_data[44], header_data[45], header_data[46], header_data[47]]);
        let flags = u32::from_le_bytes([header_data[48], header_data[49], header_data[50], header_data[51]]);
        let _bind_section_virtual_size = u32::from_le_bytes([header_data[52], header_data[53], header_data[54], header_data[55]]);
        let _unknown0002 = u32::from_le_bytes([header_data[56], header_data[57], header_data[58], header_data[59]]);
        let code_section_va = u32::from_le_bytes([header_data[60], header_data[61], header_data[62], header_data[63]]);
        let code_section_raw_size = u32::from_le_bytes([header_data[64], header_data[65], header_data[66], header_data[67]]);

        // AES Key (32 bytes at offset 68)
        let mut aes_key = [0u8; 32];
        aes_key.copy_from_slice(&header_data[68..100]);

        // AES IV (16 bytes at offset 100)
        let mut aes_iv = [0u8; 16];
        aes_iv.copy_from_slice(&header_data[100..116]);

        // Code section stolen data (16 bytes at offset 116)
        let mut stolen_data = [0u8; 16];
        stolen_data.copy_from_slice(&header_data[116..132]);

        // Encryption keys (4 x u32 at offset 132)
        let mut enc_keys = [0u32; 4];
        for i in 0..4 {
            let off = 132 + i * 4;
            enc_keys[i] = u32::from_le_bytes([
                header_data[off], header_data[off+1], header_data[off+2], header_data[off+3]
            ]);
        }

        // Find payload data (at entry - bind_section_offset)
        let payload_rva = entry_rva.wrapping_sub(bind_section_offset);
        let payload_offset = pe.rva_to_file_offset(payload_rva)
            .ok_or(SteamFlatlineError::MissingSection("Payload".into()))?;

        // Read and decrypt payload
        let p_offset = payload_offset as usize;
        let p_size = payload_size as usize;
        if p_offset.checked_add(p_size).map_or(true, |end| end > file_data.len()) {
            return Err(SteamFlatlineError::PayloadDecryptionFailed("Payload exceeds file size".into()));
        }
        let mut payload = file_data[p_offset..p_offset + p_size].to_vec();

        // XOR decrypt payload
        steam_xor(&mut payload, xor_key);

        // Extract SteamDRMP.dll from payload
        let drmp_offset = drmp_dll_offset as usize;
        let drmp_size = drmp_dll_size as usize;

        if drmp_offset.checked_add(drmp_size).map_or(true, |end| end > payload.len()) {
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

        let code_data = if is_encrypted {
            // Read encrypted section data
            let encrypted_size = code_section_raw_size as usize;
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            if section_file_offset.checked_add(encrypted_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::AesDecryptionFailed("Code section exceeds file".into()));
            }
            let mut encrypted_data = vec![0u8; encrypted_size + 16];
            encrypted_data[0..16].copy_from_slice(&stolen_data);
            encrypted_data[16..].copy_from_slice(&file_data[section_file_offset..section_file_offset + encrypted_size]);

            // AES decrypt
            aes256_cbc_decrypt(&mut encrypted_data, &aes_key, &mut aes_iv)?;
            encrypted_data
        } else {
            // Not encrypted, read as-is
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            let section_size = code_section.size_of_raw_data as usize;
            if section_file_offset.checked_add(section_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::InvalidPeFormat("Code section exceeds file".into()));
            }
            file_data[section_file_offset..section_file_offset + section_size].to_vec()
        };

        // Compute new entry point RVA before mutating pe
        // original_entry_point in SteamStub header is already an RVA, not a VA
        let image_base = match &pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe32(h) => h.image_base as u64,
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        };
        let new_entry_rva = if (original_entry_point as u64) >= image_base {
            (original_entry_point as u64 - image_base) as u32
        } else {
            original_entry_point
        };

        // Update entry point to original BEFORE removing .bind section (indices stay stable)
        match &mut pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe32(h) => {
                h.address_of_entry_point = new_entry_rva;
                h.check_sum = 0;
            }
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        }

        // Update code section data - use full code_data without truncation
        if code_section_idx < pe.section_data.len() {
            pe.section_data[code_section_idx] = code_data.clone();
            // Update section header sizes
            pe.sections[code_section_idx].size_of_raw_data = code_data.len() as u32;
            pe.sections[code_section_idx].virtual_size = code_data.len() as u32;
        }

        // Remove .bind section if present (after updating entry point and code data)
        if pe.has_section(".bind") {
            let bind_idx = pe.sections.iter().position(|s| s.name_str() == ".bind")
                .ok_or(SteamFlatlineError::MissingSection(".bind".into()))?;
            pe.sections.remove(bind_idx);
            pe.section_data.remove(bind_idx);
            pe.file_header.number_of_sections -= 1;

            // Recalculate size_of_image after removing .bind section
            let section_alignment = match &pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe32(h) => h.section_alignment,
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
            let max_va_end = pe.sections.iter()
                .map(|s| s.virtual_address + s.virtual_size.max(s.size_of_raw_data))
                .max()
                .unwrap_or(0);
            let aligned_size_of_image = ((max_va_end + section_alignment - 1) / section_alignment) * section_alignment;
            match &mut pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe32(h) => {
                    h.size_of_image = aligned_size_of_image;
                }
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
        }

        // Write unpacked file
        pe.write(output_path)?;

        Ok(())
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
        // Similar to v3.0 x64 but with different header - v3.1 signature is 0xC0DEC0D1
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

        // Check for .bind section
        if !data.windows(5).any(|w| w == b".bind") {
            return false;
        }

        // Check for v3.1 signature 0xC0DEC0D1
        // Look for the signature in the file
        data.windows(4).any(|w| w == &[0xD1, 0xC0, 0xDE, 0xC0])
    }

    fn process(&self, data: &[u8], output_path: &Path) -> Result<()> {
        // Write to unique temp file and parse with PeFile (auto-cleaned up on drop)
        let temp_guard = TempPeGuard::create(data)?;
        let mut pe = PeFile::parse(temp_guard.path())?;

        if !matches!(pe.architecture, Architecture::Pe64) {
            return Err(SteamFlatlineError::UnsupportedArchitecture);
        }

        // Check for signature at entry point - 4
        // v3.1 uses signature 0xC0DEC0D1 at entry - 4
        let entry_rva = pe.optional_header.address_of_entry_point();
        let entry_offset = pe.rva_to_file_offset(entry_rva)
            .ok_or(SteamFlatlineError::MissingSection("Entry point".into()))?;

        if entry_offset < 4 {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }

        // Read the dword at entry - 4
        let mut sig_bytes = [0u8; 4];
        let file_data = data;
        if entry_offset as usize + 4 > file_data.len() {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }
        sig_bytes.copy_from_slice(&file_data[entry_offset as usize - 4..entry_offset as usize]);
        let signature = u32::from_le_bytes(sig_bytes);

        // v3.1 signature is 0xC0DEC0D1
        if signature != 0xC0DEC0D1 {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }

        // Parse the SteamStub v3.1 header (at entry point)
        // SteamStub64Var31Header is 224 bytes
        let header_size = 224;
        let header_offset = entry_offset as usize;

        if header_offset + header_size > file_data.len() {
            return Err(SteamFlatlineError::HeaderParsingFailed("Header exceeds file size".into()));
        }

        let header_data = &file_data[header_offset..header_offset + header_size];

        // Parse header fields (SteamStub64Var31Header)
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

        // Additional v3.1 fields (offset 152 onwards)
        let _unknown0003 = u32::from_le_bytes([header_data[152], header_data[153], header_data[154], header_data[155]]);
        let _unknown0004 = u32::from_le_bytes([header_data[156], header_data[157], header_data[158], header_data[159]]);
        let _unknown0005 = u32::from_le_bytes([header_data[160], header_data[161], header_data[162], header_data[163]]);
        let _unknown0006 = u32::from_le_bytes([header_data[164], header_data[165], header_data[166], header_data[167]]);
        let _unknown0007 = u32::from_le_bytes([header_data[168], header_data[169], header_data[170], header_data[171]]);
        let _unknown0008 = u32::from_le_bytes([header_data[172], header_data[173], header_data[174], header_data[175]]);
        let _unknown0009 = u32::from_le_bytes([header_data[176], header_data[177], header_data[178], header_data[179]]);
        let _unknown0010 = u32::from_le_bytes([header_data[180], header_data[181], header_data[182], header_data[183]]);
        let _unknown0011 = u32::from_le_bytes([header_data[184], header_data[185], header_data[186], header_data[187]]);
        let _unknown0012 = u32::from_le_bytes([header_data[188], header_data[189], header_data[190], header_data[191]]);
        let _unknown0013 = u32::from_le_bytes([header_data[192], header_data[193], header_data[194], header_data[195]]);
        let _unknown0014 = u32::from_le_bytes([header_data[196], header_data[197], header_data[198], header_data[199]]);
        let _unknown0015 = u32::from_le_bytes([header_data[200], header_data[201], header_data[202], header_data[203]]);
        let _unknown0016 = u32::from_le_bytes([header_data[204], header_data[205], header_data[206], header_data[207]]);
        let _unknown0017 = u32::from_le_bytes([header_data[208], header_data[209], header_data[210], header_data[211]]);
        let _unknown0018 = u32::from_le_bytes([header_data[212], header_data[213], header_data[214], header_data[215]]);
        let _unknown0019 = u32::from_le_bytes([header_data[216], header_data[217], header_data[218], header_data[219]]);
        let _unknown0020 = u32::from_le_bytes([header_data[220], header_data[221], header_data[222], header_data[223]]);

        // Find payload data (at entry - bind_section_offset)
        let payload_rva = entry_rva.wrapping_sub(bind_section_offset);
        let payload_offset = pe.rva_to_file_offset(payload_rva)
            .ok_or(SteamFlatlineError::MissingSection("Payload".into()))?;

        // Read and decrypt payload
        let p_offset = payload_offset as usize;
        let p_size = payload_size as usize;
        if p_offset.checked_add(p_size).map_or(true, |end| end > file_data.len()) {
            return Err(SteamFlatlineError::PayloadDecryptionFailed("Payload exceeds file size".into()));
        }
        let mut payload = file_data[p_offset..p_offset + p_size].to_vec();

        // XOR decrypt payload
        steam_xor(&mut payload, xor_key);

        // Extract SteamDRMP.dll from payload
        let drmp_offset = drmp_dll_offset as usize;
        let drmp_size = drmp_dll_size as usize;

        if drmp_offset.checked_add(drmp_size).map_or(true, |end| end > payload.len()) {
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

        let code_data = if is_encrypted {
            // Read encrypted section data
            let encrypted_size = code_section_raw_size as usize;
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            if section_file_offset.checked_add(encrypted_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::AesDecryptionFailed("Code section exceeds file".into()));
            }
            let mut encrypted_data = vec![0u8; encrypted_size + 16];
            encrypted_data[0..16].copy_from_slice(&stolen_data);
            encrypted_data[16..].copy_from_slice(&file_data[section_file_offset..section_file_offset + encrypted_size]);

            // AES decrypt
            aes256_cbc_decrypt(&mut encrypted_data, &aes_key, &mut aes_iv)?;
            encrypted_data
        } else {
            // Not encrypted, read as-is
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            let section_size = code_section.size_of_raw_data as usize;
            if section_file_offset.checked_add(section_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::InvalidPeFormat("Code section exceeds file".into()));
            }
            file_data[section_file_offset..section_file_offset + section_size].to_vec()
        };

        // Compute new entry point RVA before mutating pe
        // original_entry_point in SteamStub header is already an RVA, not a VA
        let image_base = match &pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe64(h) => h.image_base,
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        };
        let new_entry_rva = if (original_entry_point as u64) >= image_base {
            (original_entry_point as u64 - image_base) as u32
        } else {
            original_entry_point
        };

        // Update entry point to original BEFORE removing .bind section (indices stay stable)
        match &mut pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe64(h) => {
                h.address_of_entry_point = new_entry_rva;
                h.check_sum = 0;
            }
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        }

        // Update code section data - use full code_data without truncation
        if code_section_idx < pe.section_data.len() {
            pe.section_data[code_section_idx] = code_data.clone();
            // Update section header sizes
            pe.sections[code_section_idx].size_of_raw_data = code_data.len() as u32;
            pe.sections[code_section_idx].virtual_size = code_data.len() as u32;
        }

        // Remove .bind section if present (after updating entry point and code data)
        if pe.has_section(".bind") {
            let bind_idx = pe.sections.iter().position(|s| s.name_str() == ".bind")
                .ok_or(SteamFlatlineError::MissingSection(".bind".into()))?;
            pe.sections.remove(bind_idx);
            pe.section_data.remove(bind_idx);
            pe.file_header.number_of_sections -= 1;

            // Recalculate size_of_image after removing .bind section
            let section_alignment = match &pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe64(h) => h.section_alignment,
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
            let max_va_end = pe.sections.iter()
                .map(|s| s.virtual_address + s.virtual_size.max(s.size_of_raw_data))
                .max()
                .unwrap_or(0);
            let aligned_size_of_image = ((max_va_end + section_alignment - 1) / section_alignment) * section_alignment;
            match &mut pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe64(h) => {
                    h.size_of_image = aligned_size_of_image;
                }
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
        }

        // Write unpacked file
        pe.write(output_path)?;

        Ok(())
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

        if !data.windows(5).any(|w| w == b".bind") {
            return false;
        }

        // Check for v3.1 signature 0xC0DEC0D1
        data.windows(4).any(|w| w == &[0xD1, 0xC0, 0xDE, 0xC0])
    }

    fn process(&self, data: &[u8], output_path: &Path) -> Result<()> {
        // Write to unique temp file and parse with PeFile (auto-cleaned up on drop)
        let temp_guard = TempPeGuard::create(data)?;
        let mut pe = PeFile::parse(temp_guard.path())?;

        if !matches!(pe.architecture, Architecture::Pe32) {
            return Err(SteamFlatlineError::UnsupportedArchitecture);
        }

        // Check for signature at entry point - 4
        // v3.1 uses signature 0xC0DEC0D1 at entry - 4
        let entry_rva = pe.optional_header.address_of_entry_point();
        let entry_offset = pe.rva_to_file_offset(entry_rva)
            .ok_or(SteamFlatlineError::MissingSection("Entry point".into()))?;

        if entry_offset < 4 {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }

        // Read the dword at entry - 4
        let mut sig_bytes = [0u8; 4];
        let file_data = data;
        if entry_offset as usize + 4 > file_data.len() {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }
        sig_bytes.copy_from_slice(&file_data[entry_offset as usize - 4..entry_offset as usize]);
        let signature = u32::from_le_bytes(sig_bytes);

        // v3.1 signature is 0xC0DEC0D1
        if signature != 0xC0DEC0D1 {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }

        // Parse the SteamStub v3.1 header (at entry point) - 32-bit version
        // SteamStub32Var31Header is 212 bytes (smaller than x64 due to 32-bit image_base)
        let header_size = 212;
        let header_offset = entry_offset as usize;

        if header_offset + header_size > file_data.len() {
            return Err(SteamFlatlineError::HeaderParsingFailed("Header exceeds file size".into()));
        }

        let header_data = &file_data[header_offset..header_offset + header_size];

        // Parse header fields (SteamStub32Var31Header)
        let xor_key = u32::from_le_bytes([header_data[0], header_data[1], header_data[2], header_data[3]]);
        let _signature = u32::from_le_bytes([header_data[4], header_data[5], header_data[6], header_data[7]]);
        let _image_base = u32::from_le_bytes([header_data[8], header_data[9], header_data[10], header_data[11]]);
        let _address_of_entry_point = u32::from_le_bytes([header_data[12], header_data[13], header_data[14], header_data[15]]);
        let bind_section_offset = u32::from_le_bytes([header_data[16], header_data[17], header_data[18], header_data[19]]);
        let _unknown0000 = u32::from_le_bytes([header_data[20], header_data[21], header_data[22], header_data[23]]);
        let original_entry_point = u32::from_le_bytes([header_data[24], header_data[25], header_data[26], header_data[27]]);
        let _unknown0001 = u32::from_le_bytes([header_data[28], header_data[29], header_data[30], header_data[31]]);
        let payload_size = u32::from_le_bytes([header_data[32], header_data[33], header_data[34], header_data[35]]);
        let drmp_dll_offset = u32::from_le_bytes([header_data[36], header_data[37], header_data[38], header_data[39]]);
        let drmp_dll_size = u32::from_le_bytes([header_data[40], header_data[41], header_data[42], header_data[43]]);
        let _steam_app_id = u32::from_le_bytes([header_data[44], header_data[45], header_data[46], header_data[47]]);
        let flags = u32::from_le_bytes([header_data[48], header_data[49], header_data[50], header_data[51]]);
        let _bind_section_virtual_size = u32::from_le_bytes([header_data[52], header_data[53], header_data[54], header_data[55]]);
        let _unknown0002 = u32::from_le_bytes([header_data[56], header_data[57], header_data[58], header_data[59]]);
        let code_section_va = u32::from_le_bytes([header_data[60], header_data[61], header_data[62], header_data[63]]);
        let code_section_raw_size = u32::from_le_bytes([header_data[64], header_data[65], header_data[66], header_data[67]]);

        // AES Key (32 bytes at offset 68)
        let mut aes_key = [0u8; 32];
        aes_key.copy_from_slice(&header_data[68..100]);

        // AES IV (16 bytes at offset 100)
        let mut aes_iv = [0u8; 16];
        aes_iv.copy_from_slice(&header_data[100..116]);

        // Code section stolen data (16 bytes at offset 116)
        let mut stolen_data = [0u8; 16];
        stolen_data.copy_from_slice(&header_data[116..132]);

        // Encryption keys (4 x u32 at offset 132)
        let mut enc_keys = [0u32; 4];
        for i in 0..4 {
            let off = 132 + i * 4;
            enc_keys[i] = u32::from_le_bytes([
                header_data[off], header_data[off+1], header_data[off+2], header_data[off+3]
            ]);
        }

        // Additional v3.1 fields (offset 148 onwards)
        let _unknown0003 = u32::from_le_bytes([header_data[148], header_data[149], header_data[150], header_data[151]]);
        let _unknown0004 = u32::from_le_bytes([header_data[152], header_data[153], header_data[154], header_data[155]]);
        let _unknown0005 = u32::from_le_bytes([header_data[156], header_data[157], header_data[158], header_data[159]]);
        let _unknown0006 = u32::from_le_bytes([header_data[160], header_data[161], header_data[162], header_data[163]]);
        let _unknown0007 = u32::from_le_bytes([header_data[164], header_data[165], header_data[166], header_data[167]]);
        let _unknown0008 = u32::from_le_bytes([header_data[168], header_data[169], header_data[170], header_data[171]]);
        let _unknown0009 = u32::from_le_bytes([header_data[172], header_data[173], header_data[174], header_data[175]]);
        let _unknown0010 = u32::from_le_bytes([header_data[176], header_data[177], header_data[178], header_data[179]]);
        let _unknown0011 = u32::from_le_bytes([header_data[180], header_data[181], header_data[182], header_data[183]]);
        let _unknown0012 = u32::from_le_bytes([header_data[184], header_data[185], header_data[186], header_data[187]]);
        let _unknown0013 = u32::from_le_bytes([header_data[188], header_data[189], header_data[190], header_data[191]]);
        let _unknown0014 = u32::from_le_bytes([header_data[192], header_data[193], header_data[194], header_data[195]]);
        let _unknown0015 = u32::from_le_bytes([header_data[196], header_data[197], header_data[198], header_data[199]]);
        let _unknown0016 = u32::from_le_bytes([header_data[200], header_data[201], header_data[202], header_data[203]]);
        let _unknown0017 = u32::from_le_bytes([header_data[204], header_data[205], header_data[206], header_data[207]]);
        let _unknown0018 = u32::from_le_bytes([header_data[208], header_data[209], header_data[210], header_data[211]]);

        // Find payload data (at entry - bind_section_offset)
        let payload_rva = entry_rva.wrapping_sub(bind_section_offset);
        let payload_offset = pe.rva_to_file_offset(payload_rva)
            .ok_or(SteamFlatlineError::MissingSection("Payload".into()))?;

        // Read and decrypt payload
        let p_offset = payload_offset as usize;
        let p_size = payload_size as usize;
        if p_offset.checked_add(p_size).map_or(true, |end| end > file_data.len()) {
            return Err(SteamFlatlineError::PayloadDecryptionFailed("Payload exceeds file size".into()));
        }
        let mut payload = file_data[p_offset..p_offset + p_size].to_vec();

        // XOR decrypt payload
        steam_xor(&mut payload, xor_key);

        // Extract SteamDRMP.dll from payload
        let drmp_offset = drmp_dll_offset as usize;
        let drmp_size = drmp_dll_size as usize;

        if drmp_offset.checked_add(drmp_size).map_or(true, |end| end > payload.len()) {
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

        let code_data = if is_encrypted {
            // Read encrypted section data
            let encrypted_size = code_section_raw_size as usize;
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            if section_file_offset.checked_add(encrypted_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::AesDecryptionFailed("Code section exceeds file".into()));
            }
            let mut encrypted_data = vec![0u8; encrypted_size + 16];
            encrypted_data[0..16].copy_from_slice(&stolen_data);
            encrypted_data[16..].copy_from_slice(&file_data[section_file_offset..section_file_offset + encrypted_size]);

            // AES decrypt
            aes256_cbc_decrypt(&mut encrypted_data, &aes_key, &mut aes_iv)?;
            encrypted_data
        } else {
            // Not encrypted, read as-is
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            let section_size = code_section.size_of_raw_data as usize;
            if section_file_offset.checked_add(section_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::InvalidPeFormat("Code section exceeds file".into()));
            }
            file_data[section_file_offset..section_file_offset + section_size].to_vec()
        };

        // Compute new entry point RVA before mutating pe
        // original_entry_point in SteamStub header is already an RVA, not a VA
        let image_base = match &pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe32(h) => h.image_base as u64,
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        };
        let new_entry_rva = if (original_entry_point as u64) >= image_base {
            (original_entry_point as u64 - image_base) as u32
        } else {
            original_entry_point
        };

        // Update entry point to original BEFORE removing .bind section (indices stay stable)
        match &mut pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe32(h) => {
                h.address_of_entry_point = new_entry_rva;
                h.check_sum = 0;
            }
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        }

        // Update code section data - use full code_data without truncation
        if code_section_idx < pe.section_data.len() {
            pe.section_data[code_section_idx] = code_data.clone();
            // Update section header sizes
            pe.sections[code_section_idx].size_of_raw_data = code_data.len() as u32;
            pe.sections[code_section_idx].virtual_size = code_data.len() as u32;
        }

        // Remove .bind section if present (after updating entry point and code data)
        if pe.has_section(".bind") {
            let bind_idx = pe.sections.iter().position(|s| s.name_str() == ".bind")
                .ok_or(SteamFlatlineError::MissingSection(".bind".into()))?;
            pe.sections.remove(bind_idx);
            pe.section_data.remove(bind_idx);
            pe.file_header.number_of_sections -= 1;

            // Recalculate size_of_image after removing .bind section
            let section_alignment = match &pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe32(h) => h.section_alignment,
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
            let max_va_end = pe.sections.iter()
                .map(|s| s.virtual_address + s.virtual_size.max(s.size_of_raw_data))
                .max()
                .unwrap_or(0);
            let aligned_size_of_image = ((max_va_end + section_alignment - 1) / section_alignment) * section_alignment;
            match &mut pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe32(h) => {
                    h.size_of_image = aligned_size_of_image;
                }
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
        }

        // Write unpacked file
        pe.write(output_path)?;

        Ok(())
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

    fn process(&self, data: &[u8], output_path: &Path) -> Result<()> {
        // Write to unique temp file and parse with PeFile (auto-cleaned up on drop)
        let temp_guard = TempPeGuard::create(data)?;
        let mut pe = PeFile::parse(temp_guard.path())?;

        if !matches!(pe.architecture, Architecture::Pe32) {
            return Err(SteamFlatlineError::UnsupportedArchitecture);
        }

        let file_data = data;

        // Find the .bind section which contains the stub code and encrypted payload
        let bind_section_idx = pe.sections.iter().position(|s| s.name_str() == ".bind")
            .ok_or(SteamFlatlineError::MissingSection(".bind".into()))?;

        let bind_section = &pe.sections[bind_section_idx];
        let bind_section_offset = bind_section.pointer_to_raw_data as usize;
        let bind_section_size = bind_section.size_of_raw_data as usize;

        if bind_section_offset + bind_section_size > file_data.len() {
            return Err(SteamFlatlineError::InvalidStubFormat);
        }

        let bind_data = &file_data[bind_section_offset..bind_section_offset + bind_section_size];

        // Find the SteamStub v2.1 header structure by locating the pattern
        // The pattern is: 53 51 52 56 57 55 8B EC 81 EC 00 10 00 00 C7
        let pattern_offset = find_pattern(bind_data, "53 51 52 56 57 55 8B EC 81 EC 00 10 00 00 C7");
        if pattern_offset < 0 {
            return Err(SteamFlatlineError::PatternNotFound("SteamStub v2.1 entry pattern not found".into()));
        }

        // The header is typically before this pattern.
        // v2.1 uses a different approach - the header is at the beginning of the .bind section
        // or at a fixed offset. Let's parse from the start of the .bind section.

        // SteamStub v2.1 header structure (at start of .bind section):
        // The first few bytes contain the header information
        if bind_data.len() < 100 {
            return Err(SteamFlatlineError::HeaderParsingFailed(".bind section too small".into()));
        }

        // Parse the header from the beginning of .bind section
        // v2.1 header layout (offsets from start of .bind section):
        // offset 0: xor_key (4 bytes)
        // offset 4: signature (4 bytes) - typically 0xDEADFACE or similar
        // offset 8: image_base (4 bytes)
        // offset 12: address_of_entry_point (4 bytes)
        // offset 16: original_entry_point (4 bytes)
        // offset 20: payload_size (4 bytes)
        // offset 24: drmp_dll_offset (4 bytes)
        // offset 28: drmp_dll_size (4 bytes)
        // offset 32: steam_app_id (4 bytes)
        // offset 36: flags (4 bytes)
        // offset 40: code_section_va (4 bytes)
        // offset 44: code_section_raw_size (4 bytes)
        // offset 48: aes_key (32 bytes)
        // offset 80: aes_iv (16 bytes)
        // offset 96: stolen_data (16 bytes)
        // offset 112: enc_keys (16 bytes = 4 x u32)

        let xor_key = u32::from_le_bytes([bind_data[0], bind_data[1], bind_data[2], bind_data[3]]);
        let _signature = u32::from_le_bytes([bind_data[4], bind_data[5], bind_data[6], bind_data[7]]);
        let _image_base = u32::from_le_bytes([bind_data[8], bind_data[9], bind_data[10], bind_data[11]]);
        let _address_of_entry_point = u32::from_le_bytes([bind_data[12], bind_data[13], bind_data[14], bind_data[15]]);
        let original_entry_point = u32::from_le_bytes([bind_data[16], bind_data[17], bind_data[18], bind_data[19]]);
        let payload_size = u32::from_le_bytes([bind_data[20], bind_data[21], bind_data[22], bind_data[23]]);
        let drmp_dll_offset = u32::from_le_bytes([bind_data[24], bind_data[25], bind_data[26], bind_data[27]]);
        let drmp_dll_size = u32::from_le_bytes([bind_data[28], bind_data[29], bind_data[30], bind_data[31]]);
        let _steam_app_id = u32::from_le_bytes([bind_data[32], bind_data[33], bind_data[34], bind_data[35]]);
        let flags = u32::from_le_bytes([bind_data[36], bind_data[37], bind_data[38], bind_data[39]]);
        let code_section_va = u32::from_le_bytes([bind_data[40], bind_data[41], bind_data[42], bind_data[43]]);
        let code_section_raw_size = u32::from_le_bytes([bind_data[44], bind_data[45], bind_data[46], bind_data[47]]);

        // AES Key (32 bytes at offset 48)
        let mut aes_key = [0u8; 32];
        aes_key.copy_from_slice(&bind_data[48..80]);

        // AES IV (16 bytes at offset 80)
        let mut aes_iv = [0u8; 16];
        aes_iv.copy_from_slice(&bind_data[80..96]);

        // Code section stolen data (16 bytes at offset 96)
        let mut stolen_data = [0u8; 16];
        stolen_data.copy_from_slice(&bind_data[96..112]);

        // Encryption keys (4 x u32 at offset 112)
        let mut enc_keys = [0u32; 4];
        for i in 0..4 {
            let off = 112 + i * 4;
            enc_keys[i] = u32::from_le_bytes([
                bind_data[off], bind_data[off+1], bind_data[off+2], bind_data[off+3]
            ]);
        }

        // The payload is after the header in the .bind section
        let header_size = 128; // Size of SteamStub32Var21Header
        let payload_offset = bind_section_offset + header_size;

        let p_size = payload_size as usize;
        if payload_offset.checked_add(p_size).map_or(true, |end| end > file_data.len()) {
            return Err(SteamFlatlineError::PayloadDecryptionFailed("Payload exceeds file size".into()));
        }

        let mut payload = file_data[payload_offset..payload_offset + p_size].to_vec();

        // XOR decrypt payload
        steam_xor(&mut payload, xor_key);

        // Extract SteamDRMP.dll from payload
        let drmp_offset = drmp_dll_offset as usize;
        let drmp_size = drmp_dll_size as usize;

        if drmp_offset.checked_add(drmp_size).map_or(true, |end| end > payload.len()) {
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

        let code_data = if is_encrypted {
            // Read encrypted section data
            let encrypted_size = code_section_raw_size as usize;
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            if section_file_offset.checked_add(encrypted_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::AesDecryptionFailed("Code section exceeds file".into()));
            }
            let mut encrypted_data = vec![0u8; encrypted_size + 16];
            encrypted_data[0..16].copy_from_slice(&stolen_data);
            encrypted_data[16..].copy_from_slice(&file_data[section_file_offset..section_file_offset + encrypted_size]);

            // AES decrypt
            aes256_cbc_decrypt(&mut encrypted_data, &aes_key, &mut aes_iv)?;
            encrypted_data
        } else {
            // Not encrypted, read as-is
            let section_file_offset = code_section.pointer_to_raw_data as usize;
            let section_size = code_section.size_of_raw_data as usize;
            if section_file_offset.checked_add(section_size).map_or(true, |end| end > file_data.len()) {
                return Err(SteamFlatlineError::InvalidPeFormat("Code section exceeds file".into()));
            }
            file_data[section_file_offset..section_file_offset + section_size].to_vec()
        };

        // Compute new entry point RVA before mutating pe
        // original_entry_point in SteamStub header is already an RVA, not a VA
        let image_base = match &pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe32(h) => h.image_base as u64,
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        };
        let new_entry_rva = if (original_entry_point as u64) >= image_base {
            (original_entry_point as u64 - image_base) as u32
        } else {
            original_entry_point
        };

        // Update entry point to original BEFORE removing .bind section (indices stay stable)
        match &mut pe.optional_header {
            crate::steamflatline::pe::OptionalHeader::Pe32(h) => {
                h.address_of_entry_point = new_entry_rva;
                h.check_sum = 0;
            }
            _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
        }

        // Update code section data - use full code_data without truncation
        if code_section_idx < pe.section_data.len() {
            pe.section_data[code_section_idx] = code_data.clone();
            // Update section header sizes
            pe.sections[code_section_idx].size_of_raw_data = code_data.len() as u32;
            pe.sections[code_section_idx].virtual_size = code_data.len() as u32;
        }

        // Remove .bind section if present (after updating entry point and code data)
        if pe.has_section(".bind") {
            let bind_idx = pe.sections.iter().position(|s| s.name_str() == ".bind")
                .ok_or(SteamFlatlineError::MissingSection(".bind".into()))?;
            pe.sections.remove(bind_idx);
            pe.section_data.remove(bind_idx);
            pe.file_header.number_of_sections -= 1;

            // Recalculate size_of_image after removing .bind section
            let section_alignment = match &pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe32(h) => h.section_alignment,
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
            let max_va_end = pe.sections.iter()
                .map(|s| s.virtual_address + s.virtual_size.max(s.size_of_raw_data))
                .max()
                .unwrap_or(0);
            let aligned_size_of_image = ((max_va_end + section_alignment - 1) / section_alignment) * section_alignment;
            match &mut pe.optional_header {
                crate::steamflatline::pe::OptionalHeader::Pe32(h) => {
                    h.size_of_image = aligned_size_of_image;
                }
                _ => return Err(SteamFlatlineError::UnsupportedArchitecture),
            };
        }

        // Write unpacked file
        pe.write(output_path)?;

        Ok(())
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
        // v2.0 uses signature 0xDEADFACE in the .bind section header
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

        // Check for .bind section
        if !data.windows(5).any(|w| w == b".bind") {
            return false;
        }

        // Check for v2.0 signature 0xDEADFACE
        data.windows(4).any(|w| w == &[0xCE, 0xFA, 0xAD, 0xDE])
    }

    fn process(&self, data: &[u8], output_path: &Path) -> Result<()> {
        // Delegate to v2.1 unpacker as they share the same structure
        StubVariant21x86::new().process(data, output_path)
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

        // Detect legacy SteamStub 1.0 (x86) binaries without .bind sections
        let has_legacy_steam_stub = data.windows(12).any(|w| w == b"SteamDRM.dll" || w == b"SteamStub10");
        let has_no_bind = !data.windows(5).any(|w| w == b".bind");
        has_legacy_steam_stub && has_no_bind
    }

    fn process(&self, _data: &[u8], _output_path: &Path) -> Result<()> {
        Err(SteamFlatlineError::HeaderParsingFailed(
            "SteamStub 1.0 (legacy) unpacking is not supported for this binary; v1.0 lacks a .bind section".into(),
        ))
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

    #[test]
    fn test_x86_v30_v31_dispatch_isolation() {
        // Construct minimal mock PE32 data
        let mut mock_pe = vec![0u8; 256];
        mock_pe[0..2].copy_from_slice(b"MZ");
        mock_pe[60..64].copy_from_slice(&64u32.to_le_bytes()); // PE offset at 64
        mock_pe[64..68].copy_from_slice(b"PE\0\0");
        mock_pe[68..70].copy_from_slice(&0x014cu16.to_le_bytes()); // Machine: x86 (0x014c)
        mock_pe[100..105].copy_from_slice(b".bind");

        let u30 = StubVariant30x86::new();
        let u31 = StubVariant31x86::new();

        // Without magic signature, neither 3.0 nor 3.1 should match
        assert!(!u30.can_process(&mock_pe));
        assert!(!u31.can_process(&mock_pe));

        // Inject 3.0 signature 0xC0DEC0DE
        mock_pe[120..124].copy_from_slice(&[0xDE, 0xC0, 0xDE, 0xC0]);
        assert!(u30.can_process(&mock_pe));
        assert!(!u31.can_process(&mock_pe));

        // Inject 3.1 signature 0xC0DEC0D1
        mock_pe[120..124].copy_from_slice(&[0xD1, 0xC0, 0xDE, 0xC0]);
        assert!(!u30.can_process(&mock_pe));
        assert!(u31.can_process(&mock_pe));
    }
}