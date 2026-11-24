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
const IMAGE_SIZEOF_OPTIONAL_HEADER64: usize = 224;
const IMAGE_SIZEOF_OPTIONAL_HEADER32: usize = 240;

// Maximum length for a PE section name (8 bytes)
const PE_SECTION_NAME_LEN: usize = 8;

// Owning byte buffer for bulk file reads.
/// Unlike Vec<uint8_t>, sizing it does NOT initialize the storage — it uses
/// a single uninitialized allocation. Move-only; converts to a span for scanning/parsing.
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

// Removed Clone impl to avoid conflicting impl issues - use from_slice instead

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
        let size = self.virtual_size.max(self.raw_size);
        let end = self.virtual_address + size;
        end >= self.virtual_address && rva >= self.virtual_address && rva < end
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
        let dos_magic = u16::from_le_bytes([data[0], data[1]]);
        if dos_magic != IMAGE_DOS_SIGNATURE {
            return Err(PError::InvalidDosMagic { got: dos_magic });
        }

        // Check NT signature at e_lfanew offset
        let lfanew = u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;

        if lfanew + 4 > data.len() {
            return Err(PError::FileTooSmall);
        }

        let nt_signature = u32::from_le_bytes([
            data[lfanew],
            data[lfanew + 1],
            data[lfanew + 2],
            data[lfanew + 3],
        ]);
        if nt_signature != IMAGE_NT_SIGNATURE {
            return Err(PError::InvalidNtSignature);
        }

        // Parse file header
        let file_header_offset = lfanew + 4;
        if file_header_offset + IMAGE_SIZEOF_FILE_HEADER > data.len() {
            return Err(PError::FileTooSmall);
        }

        let file_header = ImageFileHeader {
            machine: u16::from_le_bytes([data[file_header_offset], data[file_header_offset + 1]]),
            number_of_sections: u16::from_le_bytes([
                data[file_header_offset + 2],
                data[file_header_offset + 3],
            ]),
            time_date_stamp: u32::from_le_bytes([
                data[file_header_offset + 4],
                data[file_header_offset + 5],
                data[file_header_offset + 6],
                data[file_header_offset + 7],
            ]),
            pointer_to_symbol_table: u32::from_le_bytes([
                data[file_header_offset + 8],
                data[file_header_offset + 9],
                data[file_header_offset + 10],
                data[file_header_offset + 11],
            ]),
            number_of_symbols: u32::from_le_bytes([
                data[file_header_offset + 12],
                data[file_header_offset + 13],
                data[file_header_offset + 14],
                data[file_header_offset + 15],
            ]),
            size_of_optional_header: u16::from_le_bytes([
                data[file_header_offset + 16],
                data[file_header_offset + 17],
            ]),
            characteristics: u16::from_le_bytes([
                data[file_header_offset + 18],
                data[file_header_offset + 19],
            ]),
        };

        // Parse optional header
        let optional_offset = file_header_offset + IMAGE_SIZEOF_FILE_HEADER;
        if optional_offset + file_header.size_of_optional_header as usize > data.len() {
            return Err(PError::FileTooSmall);
        }

        let magic = u16::from_le_bytes([data[optional_offset], data[optional_offset + 1]]);
        let mut entry_point_rva: u32 = 0;
        let mut export_dir_rva_val: u32 = 0;
        let mut export_dir_size_val: u32 = 0;
        let mut sections_offset = optional_offset + 2; // Skip magic

        let is_64bit = magic == 0x20B; // IMAGE_NT_OPTIONAL_HDR64_MAGIC
        let is_32bit = magic == 0x10B; // IMAGE_NT_OPTIONAL_HDR32_MAGIC

        if is_64bit || is_32bit {
            if is_64bit {
                // 64-bit optional header
                if optional_offset + IMAGE_SIZEOF_OPTIONAL_HEADER64 > data.len() {
                    return Err(PError::FileTooSmall);
                }

                entry_point_rva = u32::from_le_bytes([
                    data[optional_offset + 16],
                    data[optional_offset + 17],
                    data[optional_offset + 18],
                    data[optional_offset + 19],
                ]);
                export_dir_rva_val = u32::from_le_bytes([
                    data[optional_offset + 96 + 16],
                    data[optional_offset + 96 + 17],
                    data[optional_offset + 96 + 18],
                    data[optional_offset + 96 + 19],
                ]);
                export_dir_size_val = u32::from_le_bytes([
                    data[optional_offset + 96 + 20],
                    data[optional_offset + 96 + 21],
                    data[optional_offset + 96 + 22],
                    data[optional_offset + 96 + 23],
                ]);

                // Parse section headers - they come after the optional header
                sections_offset = optional_offset + IMAGE_SIZEOF_OPTIONAL_HEADER64;
            } else {
                // 32-bit optional header
                if optional_offset + IMAGE_SIZEOF_OPTIONAL_HEADER32 > data.len() {
                    return Err(PError::FileTooSmall);
                }

                entry_point_rva = u32::from_le_bytes([
                    data[optional_offset + 16],
                    data[optional_offset + 17],
                    data[optional_offset + 18],
                    data[optional_offset + 19],
                ]);
                export_dir_rva_val = u32::from_le_bytes([
                    data[optional_offset + 92 + 16],
                    data[optional_offset + 92 + 17],
                    data[optional_offset + 92 + 18],
                    data[optional_offset + 92 + 19],
                ]);
                export_dir_size_val = u32::from_le_bytes([
                    data[optional_offset + 92 + 20],
                    data[optional_offset + 92 + 21],
                    data[optional_offset + 92 + 22],
                    data[optional_offset + 92 + 23],
                ]);

                // Parse section headers - they come after the optional header
                sections_offset = optional_offset + IMAGE_SIZEOF_OPTIONAL_HEADER32;
            }
        }

        // Parse section headers
        let num_sections = file_header.number_of_sections as usize;
        let mut sections = Vec::with_capacity(num_sections);

        for i in 0..num_sections {
            let section_offset = sections_offset + i * 40; // IMAGE_SIZEOF_SECTION_HEADER = 40
            if section_offset + 40 > data.len() {
                break;
            }

            let section_data = &data[section_offset..section_offset + 40];

            // Parse section name (8 bytes, null-terminated)
            let mut name_bytes = [0u8; PE_SECTION_NAME_LEN];
            name_bytes.copy_from_slice(&section_data[0..PE_SECTION_NAME_LEN]);
            let name = std::str::from_utf8(&name_bytes)
                .unwrap_or("")
                .trim_end_matches('\0')
                .to_string();

            let virtual_size = u32::from_le_bytes([
                section_data[8],
                section_data[9],
                section_data[10],
                section_data[11],
            ]);
            let virtual_address = u32::from_le_bytes([
                section_data[12],
                section_data[13],
                section_data[14],
                section_data[15],
            ]);
            let raw_data_pointer = u32::from_le_bytes([
                section_data[16],
                section_data[17],
                section_data[18],
                section_data[19],
            ]);
            let raw_data_size = u32::from_le_bytes([
                section_data[20],
                section_data[21],
                section_data[22],
                section_data[23],
            ]);
            let characteristics = u32::from_le_bytes([
                section_data[24],
                section_data[25],
                section_data[26],
                section_data[27],
            ]);

            sections.push(Section {
                name,
                virtual_address,
                virtual_size,
                raw_offset: raw_data_pointer,
                raw_size: raw_data_size,
                characteristics,
            });
        }

        let valid = true;

        Ok(Image {
            path: path_str,
            bytes,
            file_size,
            sections,
            entry_point_rva,
            export_directory_rva: export_dir_rva_val,
            export_directory_size: export_dir_size_val,
            valid,
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
        if let Some(section) = self.section_containing_rva(rva) {
            let delta = rva - section.virtual_address;
            if delta >= section.raw_size {
                return None;
            }
            let offset = section.raw_offset as usize + delta as usize;
            if offset > self.file_size as usize {
                return None;
            }
            Some(offset)
        } else {
            None
        }
    }

    /// Converts a raw file offset to an RVA.
    pub fn raw_offset_to_rva(&self, raw_offset: usize) -> Option<u32> {
        for section in &self.sections {
            let section_raw = section.raw_offset as usize;
            let section_end = section_raw + section.raw_size as usize;
            if section_end < section_raw || raw_offset < section_raw || raw_offset >= section_end {
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
        if offset > self.file_size as usize {
            return Err(PError::IoError(std::io::Error::other(format!(
                "Offset {} exceeds file size {}",
                offset, self.file_size
            ))));
        }
        let actual_size = (self.file_size as usize).saturating_sub(offset);
        let size = size.min(actual_size);
        let data = self
            .bytes
            .as_slice()
            .get(offset..offset + size)
            .unwrap_or(&[])
            .to_vec();
        Ok(ByteBuffer::from_slice(&data[..]))
    }

    /// Reads all bytes of the image.
    pub fn read_all_bytes(&self) -> ByteBuffer {
        ByteBuffer::from_slice(self.bytes.as_slice())
    }

    /// Finds an export by name.
    pub fn find_export(&self, symbol_name: &str) -> Result<Export, PError> {
        if !self.valid {
            return Err(PError::ExportNotFound(symbol_name.to_string()));
        }

        if self.export_directory_rva == 0 || self.export_directory_size == 0 {
            return Err(PError::ExportNotFound(symbol_name.to_string()));
        }

        // Read the export directory
        let export_offset = self
            .rva_to_offset(self.export_directory_rva)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
        let export_bytes =
            self.read_raw_bytes(export_offset, self.export_directory_size as usize)?;

        // Parse IMAGE_EXPORT_DIRECTORY
        let export_dir_size = std::mem::size_of::<IMAGE_EXPORT_DIRECTORY>();
        if export_bytes.size() < export_dir_size {
            return Err(PError::ExportNotFound(symbol_name.to_string()));
        }

        let export_data = export_bytes.as_slice();
        let exports = unsafe { &*(export_data.as_ptr() as *const IMAGE_EXPORT_DIRECTORY) };

        if exports.number_of_names == 0 || exports.number_of_functions == 0 {
            return Err(PError::ExportNotFound(symbol_name.to_string()));
        }

        // Helper to read a DWORD from a file offset
        let read_dword = |file_offset: usize| -> u32 {
            if file_offset + 4 > export_data.len() {
                return 0;
            }
            u32::from_le_bytes([
                export_data[file_offset],
                export_data[file_offset + 1],
                export_data[file_offset + 2],
                export_data[file_offset + 3],
            ])
        };

        // Read RVAs for the three arrays
        let names_rva = read_dword(exports.address_of_names as usize);
        let ordinals_rva = read_dword(exports.address_of_name_ordinals as usize);
        let functions_rva = read_dword(exports.address_of_functions as usize);

        let names_offset = self
            .rva_to_offset(names_rva)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
        let ordinals_offset = self
            .rva_to_offset(ordinals_rva)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
        let functions_offset = self
            .rva_to_offset(functions_rva)
            .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;

        // Read the ordinals array
        let ordinals_data =
            &export_data[ordinals_offset..ordinals_offset + (exports.number_of_names as usize) * 2];

        // Read the functions array
        let functions_data = &export_data
            [functions_offset..functions_offset + (exports.number_of_functions as usize) * 4];

        for i in 0..exports.number_of_names {
            // Get the RVA of the name at index i
            let name_rva_offset = (i as usize) * 4;
            let name_rva = read_dword(names_offset + name_rva_offset);

            let name_file_offset = self
                .rva_to_offset(name_rva)
                .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
            let name_data = self.read_raw_bytes(name_file_offset, 256)?;
            let name_bytes = name_data.as_slice();

            // Find null terminator and convert to string
            let name_str: String = name_bytes
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .collect();

            if name_str == symbol_name {
                // Get the ordinal for this name
                let ordinal_offset = (i as usize) * 2;
                let ordinal = u16::from_le_bytes([
                    ordinals_data[ordinal_offset],
                    ordinals_data[ordinal_offset + 1],
                ]) as u32;

                // Get the function RVA using the ordinal (base + index)
                let func_index = ordinal + exports.base;
                let func_rva_offset = (func_index as usize) * 4;

                if func_rva_offset + 4 > functions_data.len() {
                    return Err(PError::ExportNotFound(symbol_name.to_string()));
                }

                let func_rva = read_dword(functions_offset + func_rva_offset);

                // Check if this export has a forwarder
                let export_end = self.export_directory_rva + self.export_directory_size;
                let func_within_export =
                    func_rva >= self.export_directory_rva && func_rva < export_end;

                let forwarder = if func_within_export {
                    let fwd_file_offset = self
                        .rva_to_offset(func_rva)
                        .ok_or_else(|| PError::ExportNotFound(symbol_name.to_string()))?;
                    let fwd_buf = self.read_raw_bytes(fwd_file_offset, 256)?;
                    let fwd_bytes = fwd_buf.as_slice();
                    let fwd_str: String = fwd_bytes
                        .iter()
                        .take_while(|&&b| b != 0)
                        .map(|&b| b as char)
                        .collect();
                    if !fwd_str.is_empty() {
                        Some(fwd_str)
                    } else {
                        None
                    }
                } else {
                    None
                };

                return Ok(Export {
                    rva: func_rva,
                    forwarder: forwarder.unwrap_or(String::new()),
                });
            }
        }

        Err(PError::ExportNotFound(symbol_name.to_string()))
    }
}

// PE structures needed for export parsing

/// IMAGE_FILE_HEADER corresponds to the COFF file header
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ImageFileHeader {
    machine: u16,
    number_of_sections: u16,
    time_date_stamp: u32,
    pointer_to_symbol_table: u32,
    number_of_symbols: u32,
    size_of_optional_header: u16,
    characteristics: u16,
}

/// IMAGE_EXPORT_DIRECTORY corresponds to the export directory in a PE file
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct IMAGE_EXPORT_DIRECTORY {
    characteristics: u32,
    time_date_stamp: u32,
    major_version: u16,
    minor_version: u16,
    name: u32,
    base: u32,
    number_of_functions: u32,
    number_of_names: u32,
    address_of_functions: u32,
    address_of_name_ordinals: u32,
    address_of_names: u32,
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
    }
}
