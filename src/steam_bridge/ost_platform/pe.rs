//! Linux implementation of OSTPlatform::PE
//!
//! Provides PE/COFF file parsing compatible with the Windows API,
//! using Linux primitives: dlopen/dlsym, /proc/self/mmap, mprotect, and ELF/PE parsing.
//!
//! References the Windows implementation at:
//!   usefulsteamtools/src/OSTPlatform/Windows/PE.cpp
//!   usefulsteamtools/src/OSTPlatform/include/PE.h

use std::fs;
use std::path::Path;
use std::ptr;

use thiserror::Error;

// PE constants matching Windows headers
const IMAGE_DOS_SIGNATURE: u16 = 0x5A4D;
const IMAGE_NT_SIGNATURE: u32 = 0x00004550; // PE00

const IMAGE_SIZEOF_FILE_HEADER: usize = 20;
const IMAGE_SIZEOF_SECTION_HEADER: usize = 40;
const IMAGE_SIZEOF_EXPORT_DIRECTORY: usize = 40;
const PE32_EXPORT_DIR_OFFSET: usize = 96;
const PE32_NUM_RVA_AND_SIZES_OFFSET: usize = 92;
const PE64_EXPORT_DIR_OFFSET: usize = 112;
const PE64_NUM_RVA_AND_SIZES_OFFSET: usize = 108;

// Maximum length for a PE section name (8 bytes)
const PE_SECTION_NAME_LEN: usize = 8;
const MAX_EXPORT_CSTR_LEN: usize = 4096;

fn read_u16_le(data: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let slice = data.get(offset..end)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_le(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice = data.get(offset..end)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_cstr_at(data: &[u8], offset: usize, max_len: usize) -> Option<String> {
    let tail = data.get(offset..)?;
    let bounded = &tail[..tail.len().min(max_len)];
    let nul_pos = bounded
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(bounded.len());
    Some(String::from_utf8_lossy(&bounded[..nul_pos]).into_owned())
}

/// Owning byte buffer for bulk file reads.
#[derive(Debug, Clone)]
pub struct ByteBuffer {
    data: Option<Box<[u8]>>,
    size: usize,
}

impl ByteBuffer {
    pub fn new(size: usize) -> Self {
        let data = if size > 0 {
            Some(vec![0u8; size].into_boxed_slice())
        } else {
            None
        };
        ByteBuffer { data, size }
    }

    pub fn from_slice(data: &[u8]) -> Self {
        if data.is_empty() {
            return ByteBuffer::new(0);
        }
        let size = data.len();
        let boxed = Some(data.to_vec().into_boxed_slice());
        ByteBuffer { data: boxed, size }
    }

    pub fn data(&self) -> *const u8 {
        self.data.as_ref().map_or(ptr::null(), |d| d.as_ptr())
    }

    pub fn data_mut(&mut self) -> *mut u8 {
        self.data
            .as_mut()
            .map_or(ptr::null_mut(), |d| d.as_mut_ptr())
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn empty(&self) -> bool {
        self.size == 0
    }

    /// Get a slice reference to the data
    pub fn as_slice(&self) -> &[u8] {
        if let Some(ref d) = self.data {
            &d[..self.size]
        } else {
            &[]
        }
    }
}

impl Default for ByteBuffer {
    fn default() -> Self {
        ByteBuffer::new(0)
    }
}

impl From<Vec<u8>> for ByteBuffer {
    fn from(v: Vec<u8>) -> Self {
        ByteBuffer::from_slice(&v)
    }
}

/// A section of a PE/COFF file.
#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_offset: u32,
    pub raw_size: u32,
    pub characteristics: u32,
}

impl Section {
    /// Returns true if this section has the IMAGE_SCN_MEM_EXECUTE characteristic.
    pub fn is_executable(&self) -> bool {
        (self.characteristics & 0x20000000u32) != 0
    }

    /// Returns true if this section has the IMAGE_SCN_MEM_WRITE characteristic.
    pub fn is_writable(&self) -> bool {
        (self.characteristics & 0x80000000u32) != 0
    }

    /// Returns true if the section is both executable and writable (W^X violation).
    pub fn is_executable_and_writable(&self) -> bool {
        self.is_executable() && self.is_writable()
    }

    /// Returns true if this section contains the given RVA.
    pub fn contains_rva(&self, rva: u32) -> bool {
        let size = u64::from(self.virtual_size.max(self.raw_size));
        let start = u64::from(self.virtual_address);
        let end = start + size;
        let rva64 = u64::from(rva);
        rva64 >= start && rva64 < end
    }
}

/// An exported symbol from a PE file.
#[derive(Debug, Clone)]
pub struct Export {
    pub rva: u32,
    /// If this is a forwarder, the forwarder string (e.g., "dll.function")
    pub forwarder: String,
}

impl Export {
    /// Returns true if this export is a forwarder to another function.
    pub fn is_forwarder(&self) -> bool {
        !self.forwarder.is_empty()
    }
}

/// Error type for PE operations.
#[derive(Debug, Error)]
pub enum PError {
    #[error("Invalid DOS magic: expected 0x5A4D, got 0x{:04X}", got)]
    InvalidDosMagic { got: u16 },

    #[error("Invalid NT signature")]
    InvalidNtSignature,

    #[error("File too small to contain PE header")]
    FileTooSmall,

    #[error("Invalid PE file: {0}")]
    InvalidFormat(String),

    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    #[error("Export not found: {0}")]
    ExportNotFound(String),

    #[error("RVA not found in any section")]
    RvaNotFound,

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Symbol ordinal out of range: {0}")]
    OrdinalOutOfRange(u32),
}

/// The PE/COFF image representation.
/// Parses a PE file and provides access to its headers, sections, and exports.
#[derive(Debug)]
pub struct Image {
    path: String,
    bytes: ByteBuffer,
    file_size: u64,
    sections: Vec<Section>,
    entry_point_rva: u32,
    export_directory_rva: u32,
    export_directory_size: u32,
    valid: bool,
}

impl Image {
    /// Creates a new Image from the given file path.
    pub fn new(path: &str) -> Result<Self, PError> {
        let path = Path::new(path);
        let path_str = path.to_string_lossy().to_string();

        // Read the entire file
        let data = fs::read(path).map_err(PError::IoError)?;
        let file_size = data.len() as u64;
        let bytes = ByteBuffer::from_slice(&data);

        if data.len() < 64 {
            return Err(PError::FileTooSmall);
        }

        // Check DOS header signature
        let dos_magic = read_u16_le(&data, 0).ok_or(PError::FileTooSmall)?;
        if dos_magic != IMAGE_DOS_SIGNATURE {
            return Err(PError::InvalidDosMagic { got: dos_magic });
        }

        // Check NT signature at e_lfanew offset
        let lfanew = read_u32_le(&data, 0x3C).ok_or(PError::FileTooSmall)? as usize;
        let nt_signature = read_u32_le(&data, lfanew).ok_or(PError::FileTooSmall)?;
        if nt_signature != IMAGE_NT_SIGNATURE {
            return Err(PError::InvalidNtSignature);
        }

        // Parse COFF file header
        let file_header_offset = lfanew.checked_add(4).ok_or(PError::FileTooSmall)?;
        let optional_offset = file_header_offset
            .checked_add(IMAGE_SIZEOF_FILE_HEADER)
            .ok_or(PError::FileTooSmall)?;
        if optional_offset > data.len() {
            return Err(PError::FileTooSmall);
        }

        let number_of_sections =
            read_u16_le(&data, file_header_offset + 2).ok_or(PError::FileTooSmall)?;
        let size_of_optional_header =
            read_u16_le(&data, file_header_offset + 16).ok_or(PError::FileTooSmall)? as usize;

        let sections_offset = optional_offset
            .checked_add(size_of_optional_header)
            .ok_or(PError::FileTooSmall)?;
        if sections_offset > data.len() {
            return Err(PError::FileTooSmall);
        }

        let mut entry_point_rva: u32 = 0;
        let mut export_dir_rva_val: u32 = 0;
        let mut export_dir_size_val: u32 = 0;

        if size_of_optional_header >= 2 {
            let magic = read_u16_le(&data, optional_offset).ok_or(PError::FileTooSmall)?;
            let is_64bit = magic == 0x20B; // IMAGE_NT_OPTIONAL_HDR64_MAGIC (PE32+)
            let is_32bit = magic == 0x10B; // IMAGE_NT_OPTIONAL_HDR32_MAGIC (PE32)

            if is_64bit || is_32bit {
                if size_of_optional_header >= 20 {
                    entry_point_rva = read_u32_le(&data, optional_offset + 16).unwrap_or(0);
                }

                let (num_dirs_off, export_dir_off) = if is_64bit {
                    (PE64_NUM_RVA_AND_SIZES_OFFSET, PE64_EXPORT_DIR_OFFSET)
                } else {
                    (PE32_NUM_RVA_AND_SIZES_OFFSET, PE32_EXPORT_DIR_OFFSET)
                };

                if size_of_optional_header >= export_dir_off + 8 {
                    let num_rva_and_sizes =
                        read_u32_le(&data, optional_offset + num_dirs_off).unwrap_or(0);
                    if num_rva_and_sizes >= 1 {
                        export_dir_rva_val =
                            read_u32_le(&data, optional_offset + export_dir_off).unwrap_or(0);
                        export_dir_size_val =
                            read_u32_le(&data, optional_offset + export_dir_off + 4).unwrap_or(0);
                    }
                }
            }
        }

        // Parse section headers (IMAGE_SECTION_HEADER, 40 bytes each)
        let num_sections = number_of_sections as usize;
        let max_possible_sections =
            data.len().saturating_sub(sections_offset) / IMAGE_SIZEOF_SECTION_HEADER;
        let mut sections = Vec::with_capacity(num_sections.min(max_possible_sections));

        for i in 0..num_sections {
            let Some(rel_off) = i.checked_mul(IMAGE_SIZEOF_SECTION_HEADER) else {
                break;
            };
            let Some(section_offset) = sections_offset.checked_add(rel_off) else {
                break;
            };
            let Some(section_end) = section_offset.checked_add(IMAGE_SIZEOF_SECTION_HEADER) else {
                break;
            };
            if section_end > data.len() {
                break;
            }

            let section_data = &data[section_offset..section_end];

            // Section name (8 bytes, null-padded)
            let name_raw = &section_data[0..PE_SECTION_NAME_LEN];
            let name_end = name_raw
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(PE_SECTION_NAME_LEN);
            let name = String::from_utf8_lossy(&name_raw[..name_end]).into_owned();

            // Standard PE/COFF IMAGE_SECTION_HEADER field offsets:
            //  8..12: VirtualSize
            // 12..16: VirtualAddress
            // 16..20: SizeOfRawData
            // 20..24: PointerToRawData
            // 36..40: Characteristics
            let virtual_size = read_u32_le(section_data, 8).unwrap_or(0);
            let virtual_address = read_u32_le(section_data, 12).unwrap_or(0);
            let raw_data_size = read_u32_le(section_data, 16).unwrap_or(0);
            let raw_data_pointer = read_u32_le(section_data, 20).unwrap_or(0);
            let characteristics = read_u32_le(section_data, 36).unwrap_or(0);

            sections.push(Section {
                name,
                virtual_address,
                virtual_size,
                raw_offset: raw_data_pointer,
                raw_size: raw_data_size,
                characteristics,
            });
        }

        Ok(Image {
            path: path_str,
            bytes,
            file_size,
            sections,
            entry_point_rva,
            export_directory_rva: export_dir_rva_val,
            export_directory_size: export_dir_size_val,
            valid: true,
        })
    }

    /// Returns true if the image was parsed successfully.
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// Returns the path of the PE file.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the file size.
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Returns the entry point RVA.
    pub fn entry_point_rva(&self) -> u32 {
        self.entry_point_rva
    }

    /// Returns all sections of the image.
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Returns true if the image has a section with the given name.
    pub fn has_section(&self, name: &str) -> bool {
        self.sections.iter().any(|s| s.name == name)
    }

    /// Returns the section containing the given RVA, or None if not found.
    pub fn section_containing_rva(&self, rva: u32) -> Option<&Section> {
        self.sections.iter().find(|s| s.contains_rva(rva))
    }

    /// Converts an RVA to a file offset.
    pub fn rva_to_offset(&self, rva: u32) -> Option<usize> {
        let section = self.section_containing_rva(rva)?;
        let delta = rva.checked_sub(section.virtual_address)?;
        if delta >= section.raw_size {
            return None;
        }
        let offset = (section.raw_offset as usize).checked_add(delta as usize)?;
        if offset > self.bytes.size() {
            return None;
        }
        Some(offset)
    }

    /// Converts a raw file offset to an RVA.
    pub fn raw_offset_to_rva(&self, raw_offset: usize) -> Option<u32> {
        for section in &self.sections {
            let section_raw = section.raw_offset as usize;
            let Some(section_end) = section_raw.checked_add(section.raw_size as usize) else {
                continue;
            };
            if raw_offset < section_raw || raw_offset >= section_end {
                continue;
            }
            let delta_usize = raw_offset - section_raw;
            let delta = u32::try_from(delta_usize).ok()?;
            return section.virtual_address.checked_add(delta);
        }
        None
    }

    /// Reads raw bytes at the given offset and size.
    pub fn read_raw_bytes(&self, offset: usize, size: usize) -> Result<ByteBuffer, PError> {
        let file_len = self.bytes.size();
        if offset > file_len {
            return Err(PError::IoError(std::io::Error::other(format!(
                "Offset {} exceeds file size {}",
                offset, self.file_size
            ))));
        }
        let actual_size = file_len.saturating_sub(offset);
        let size = size.min(actual_size);
        let end = offset.checked_add(size).unwrap_or(file_len);
        let data = self.bytes.as_slice().get(offset..end).unwrap_or(&[]);
        Ok(ByteBuffer::from_slice(data))
    }

    /// Reads all bytes of the image.
    pub fn read_all_bytes(&self) -> ByteBuffer {
        ByteBuffer::from_slice(self.bytes.as_slice())
    }

    /// Finds an export by name.
    pub fn find_export(&self, symbol_name: &str) -> Result<Export, PError> {
        if !self.valid || self.export_directory_rva == 0 || self.export_directory_size == 0 {
            return Err(PError::ExportNotFound(symbol_name.to_string()));
        }

        let file_bytes = self.bytes.as_slice();
        let export_offset = self
            .rva_to_offset(self.export_directory_rva)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;

        let export_dir_end = export_offset
            .checked_add(IMAGE_SIZEOF_EXPORT_DIRECTORY)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
        if export_dir_end > file_bytes.len() {
            return Err(PError::ExportNotFound(symbol_name.to_string()));
        }

        let dir = &file_bytes[export_offset..export_dir_end];
        let number_of_functions = read_u32_le(dir, 20).unwrap_or(0) as usize;
        let number_of_names = read_u32_le(dir, 24).unwrap_or(0) as usize;
        let address_of_functions_rva = read_u32_le(dir, 28).unwrap_or(0);
        let address_of_names_rva = read_u32_le(dir, 32).unwrap_or(0);
        let address_of_name_ordinals_rva = read_u32_le(dir, 36).unwrap_or(0);

        if number_of_names == 0 || number_of_functions == 0 {
            return Err(PError::ExportNotFound(symbol_name.to_string()));
        }

        let names_offset = self
            .rva_to_offset(address_of_names_rva)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
        let ordinals_offset = self
            .rva_to_offset(address_of_name_ordinals_rva)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
        let functions_offset = self
            .rva_to_offset(address_of_functions_rva)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;

        let names_end = number_of_names
            .checked_mul(4)
            .and_then(|n| names_offset.checked_add(n))
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
        let ordinals_end = number_of_names
            .checked_mul(2)
            .and_then(|n| ordinals_offset.checked_add(n))
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
        let functions_end = number_of_functions
            .checked_mul(4)
            .and_then(|n| functions_offset.checked_add(n))
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;

        if names_end > file_bytes.len()
            || ordinals_end > file_bytes.len()
            || functions_end > file_bytes.len()
        {
            return Err(PError::ExportNotFound(symbol_name.to_string()));
        }

        let export_end =
            u64::from(self.export_directory_rva) + u64::from(self.export_directory_size);

        for i in 0..number_of_names {
            let name_rva = read_u32_le(file_bytes, names_offset + i * 4)
                .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
            let Some(name_file_offset) = self.rva_to_offset(name_rva) else {
                continue;
            };
            let Some(name_str) = read_cstr_at(file_bytes, name_file_offset, MAX_EXPORT_CSTR_LEN)
            else {
                continue;
            };

            if name_str == symbol_name {
                // AddressOfNameOrdinals entries are 0-based indices into AddressOfFunctions.
                let func_index = read_u16_le(file_bytes, ordinals_offset + i * 2)
                    .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?
                    as usize;
                if func_index >= number_of_functions {
                    return Err(PError::OrdinalOutOfRange(func_index as u32));
                }

                let func_rva = read_u32_le(file_bytes, functions_offset + func_index * 4)
                    .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;

                let func_rva64 = u64::from(func_rva);
                let func_within_export =
                    func_rva64 >= u64::from(self.export_directory_rva) && func_rva64 < export_end;

                let forwarder = if func_within_export {
                    let fwd_file_offset = self
                        .rva_to_offset(func_rva)
                        .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
                    read_cstr_at(file_bytes, fwd_file_offset, MAX_EXPORT_CSTR_LEN)
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                return Ok(Export {
                    rva: func_rva,
                    forwarder,
                });
            }
        }

        Err(PError::ExportNotFound(symbol_name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pe_byte_buffer() {
        let bb = ByteBuffer::new(16);
        assert!(!bb.empty());
        assert_eq!(bb.size(), 16);
        assert!(ByteBuffer::new(0).empty());

        let data: &[u8] = &[1, 2, 3, 4];
        let bb2 = ByteBuffer::from_slice(data);
        assert!(!bb2.empty());
        assert_eq!(bb2.size(), 4);
        assert_eq!(*bb2.as_slice(), [1, 2, 3, 4]);
    }

    #[test]
    fn test_pe_section_methods() {
        let section = Section {
            name: ".text".to_string(),
            virtual_address: 0x1000,
            virtual_size: 0x1000,
            raw_offset: 0x400,
            raw_size: 0x800,
            characteristics: 0xE000_0020, // executable + readable + writable
        };

        assert!(section.is_executable());
        assert!(section.is_writable());
        assert!(section.is_executable_and_writable());
    }

    #[test]
    fn test_section_contains_rva() {
        let section = Section {
            name: ".text".to_string(),
            virtual_address: 0x1000,
            virtual_size: 0x1000,
            raw_offset: 0x400,
            raw_size: 0x800,
            characteristics: 0x0,
        };

        // RVA inside the section
        assert!(section.contains_rva(0x1000));
        assert!(section.contains_rva(0x1500));
        assert!(!section.contains_rva(0xFFF));
        assert!(!section.contains_rva(0x2000));
    }

    #[test]
    fn test_byte_buffer_from_vec() {
        let vec = vec![10, 20, 30];
        let bb: ByteBuffer = vec.into();
        assert_eq!(bb.size(), 3);
        assert_eq!(*bb.as_slice(), [10, 20, 30]);
        assert!(ByteBuffer::from_slice(&[]).data().is_null());
    }

    fn build_synthetic_pe(is_64bit: bool) -> Vec<u8> {
        let mut buf = vec![0u8; 0x1000];
        // DOS header
        buf[0..2].copy_from_slice(&IMAGE_DOS_SIGNATURE.to_le_bytes());
        let lfanew: usize = 0x80;
        buf[0x3C..0x40].copy_from_slice(&(lfanew as u32).to_le_bytes());

        // NT signature
        buf[lfanew..lfanew + 4].copy_from_slice(&IMAGE_NT_SIGNATURE.to_le_bytes());

        // COFF FileHeader (20 bytes)
        let fh = lfanew + 4;
        let opt_size: u16 = if is_64bit { 240 } else { 224 };
        buf[fh + 2..fh + 4].copy_from_slice(&2u16.to_le_bytes()); // NumberOfSections = 2
        buf[fh + 16..fh + 18].copy_from_slice(&opt_size.to_le_bytes());

        // Optional header
        let opt = fh + 20;
        let magic: u16 = if is_64bit { 0x20B } else { 0x10B };
        buf[opt..opt + 2].copy_from_slice(&magic.to_le_bytes());
        buf[opt + 16..opt + 20].copy_from_slice(&0x1010u32.to_le_bytes()); // AddressOfEntryPoint

        let (num_dirs_off, export_dir_off) = if is_64bit {
            (PE64_NUM_RVA_AND_SIZES_OFFSET, PE64_EXPORT_DIR_OFFSET)
        } else {
            (PE32_NUM_RVA_AND_SIZES_OFFSET, PE32_EXPORT_DIR_OFFSET)
        };
        buf[opt + num_dirs_off..opt + num_dirs_off + 4].copy_from_slice(&16u32.to_le_bytes());
        // Export directory at RVA 0x2000, size 0x180
        buf[opt + export_dir_off..opt + export_dir_off + 4]
            .copy_from_slice(&0x2000u32.to_le_bytes());
        buf[opt + export_dir_off + 4..opt + export_dir_off + 8]
            .copy_from_slice(&0x180u32.to_le_bytes());

        // Section 0: .text (VA 0x1000, VSize 0x200, RawSize 0x200, RawPtr 0x400, RX)
        let sec0 = opt + opt_size as usize;
        buf[sec0..sec0 + 5].copy_from_slice(b".text");
        buf[sec0 + 8..sec0 + 12].copy_from_slice(&0x200u32.to_le_bytes());
        buf[sec0 + 12..sec0 + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        buf[sec0 + 16..sec0 + 20].copy_from_slice(&0x200u32.to_le_bytes()); // SizeOfRawData
        buf[sec0 + 20..sec0 + 24].copy_from_slice(&0x400u32.to_le_bytes()); // PointerToRawData
        buf[sec0 + 36..sec0 + 40].copy_from_slice(&0x6000_0020u32.to_le_bytes()); // Characteristics

        // Section 1: .edata (VA 0x2000, VSize 0x200, RawSize 0x200, RawPtr 0x600, R)
        let sec1 = sec0 + 40;
        buf[sec1..sec1 + 6].copy_from_slice(b".edata");
        buf[sec1 + 8..sec1 + 12].copy_from_slice(&0x200u32.to_le_bytes());
        buf[sec1 + 12..sec1 + 16].copy_from_slice(&0x2000u32.to_le_bytes());
        buf[sec1 + 16..sec1 + 20].copy_from_slice(&0x200u32.to_le_bytes());
        buf[sec1 + 20..sec1 + 24].copy_from_slice(&0x600u32.to_le_bytes());
        buf[sec1 + 36..sec1 + 40].copy_from_slice(&0x4000_0040u32.to_le_bytes());

        // IMAGE_EXPORT_DIRECTORY at file offset 0x600 (RVA 0x2000)
        // Base = 1, NumberOfFunctions = 2, NumberOfNames = 2
        // AddressOfFunctions = 0x2040, AddressOfNames = 0x2050, AddressOfNameOrdinals = 0x2060
        let ed = 0x600;
        buf[ed + 16..ed + 20].copy_from_slice(&1u32.to_le_bytes()); // Base = 1
        buf[ed + 20..ed + 24].copy_from_slice(&2u32.to_le_bytes()); // NumberOfFunctions
        buf[ed + 24..ed + 28].copy_from_slice(&2u32.to_le_bytes()); // NumberOfNames
        buf[ed + 28..ed + 32].copy_from_slice(&0x2040u32.to_le_bytes()); // AddressOfFunctions
        buf[ed + 32..ed + 36].copy_from_slice(&0x2050u32.to_le_bytes()); // AddressOfNames
        buf[ed + 36..ed + 40].copy_from_slice(&0x2060u32.to_le_bytes()); // AddressOfNameOrdinals

        // Functions table at 0x640 (RVA 0x2040):
        // [0] -> normal function RVA 0x1050
        // [1] -> forwarded export RVA 0x20A0 (inside export dir 0x2000..0x2180)
        buf[0x640..0x644].copy_from_slice(&0x1050u32.to_le_bytes());
        buf[0x644..0x648].copy_from_slice(&0x20A0u32.to_le_bytes());

        // Names table at 0x650 (RVA 0x2050):
        buf[0x650..0x654].copy_from_slice(&0x2080u32.to_le_bytes()); // "SteamAPI_Init"
        buf[0x654..0x658].copy_from_slice(&0x2090u32.to_le_bytes()); // "ForwardedFn"

        // Name ordinals table at 0x660 (RVA 0x2060) — 0-based indices into AddressOfFunctions:
        buf[0x660..0x662].copy_from_slice(&0u16.to_le_bytes());
        buf[0x662..0x664].copy_from_slice(&1u16.to_le_bytes());

        // Strings
        buf[0x680..0x68E].copy_from_slice(b"SteamAPI_Init\0");
        buf[0x690..0x69C].copy_from_slice(b"ForwardedFn\0");
        buf[0x6A0..0x6B4].copy_from_slice(b"other.TargetExport\0\0");

        buf
    }

    #[test]
    fn test_parse_pe32_and_pe64_sections_and_exports() {
        let dir = tempfile::tempdir().unwrap();
        for (name, is_64) in [("sample32.dll", false), ("sample64.dll", true)] {
            let path = dir.path().join(name);
            fs::write(&path, build_synthetic_pe(is_64)).unwrap();

            let img = Image::new(path.to_str().unwrap()).expect("valid PE");
            assert!(img.is_valid());
            assert_eq!(img.entry_point_rva(), 0x1010);
            assert_eq!(img.sections().len(), 2);
            assert!(img.has_section(".text"));
            assert!(img.has_section(".edata"));

            let text = &img.sections()[0];
            assert_eq!(text.raw_offset, 0x400);
            assert_eq!(text.raw_size, 0x200);
            assert!(text.is_executable());
            assert!(!text.is_writable());

            assert_eq!(img.rva_to_offset(0x1050), Some(0x450));
            assert_eq!(img.raw_offset_to_rva(0x450), Some(0x1050));

            let exp = img.find_export("SteamAPI_Init").expect("normal export");
            assert_eq!(exp.rva, 0x1050);
            assert!(!exp.is_forwarder());

            let fwd = img.find_export("ForwardedFn").expect("forwarded export");
            assert!(fwd.is_forwarder());
            assert_eq!(fwd.forwarder, "other.TargetExport");

            assert!(matches!(
                img.find_export("MissingExport"),
                Err(PError::ExportNotFound(_))
            ));
        }
    }

    #[test]
    fn test_section_contains_rva_no_overflow() {
        let sec = Section {
            name: ".high".into(),
            virtual_address: u32::MAX - 0x10,
            virtual_size: 0x100,
            raw_offset: 0,
            raw_size: 0,
            characteristics: 0,
        };
        assert!(sec.contains_rva(u32::MAX));
        assert!(!sec.contains_rva(0));
    }
}
