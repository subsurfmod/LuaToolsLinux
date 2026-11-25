//! PE (Portable Executable) parsing for steamflatline
//!
//! Provides parsing of PE32 and PE64 files including sections, headers, and data directories.

use std::io::{Read, Seek, SeekFrom, Write};
use std::fs::File;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    Pe32,
    Pe64,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineType {
    // 32-bit x86 variants
    I386 = 0x014c,
    I486 = 0x014d,
    I860 = 0x014e,
    // 64-bit x86
    AMD64 = 0x8664,
    // Other common architectures
    ARM = 0x01c0,
    ARM64 = 0xaa64,
    ARMNT = 0x01c4,
    IA64 = 0x0200,
    MIPS16 = 0x0266,
    MIPSFPU = 0x0366,
    MIPSFPU16 = 0x0466,
    POWERPC = 0x01f0,
    POWERPCFP = 0x01f1,
    R3000 = 0x0162,
    R4000 = 0x0166,
    R10000 = 0x0168,
    WCEMIPSV2 = 0x0169,
    ALPHA = 0x0184,
    SH3 = 0x01a2,
    SH3DSP = 0x01a3,
    SH4 = 0x01a6,
    SH5 = 0x01a8,
    THUMB = 0x01c2,
    AM33 = 0x01d3,
    M32R = 0x9041,
    CEE = 0xc0ee,
    Unknown(u16),
}

impl MachineType {
    /// Returns true if this machine type represents a 32-bit architecture
    pub fn is_32bit(&self) -> bool {
        matches!(
            self,
            MachineType::I386
                | MachineType::I486
                | MachineType::I860
                | MachineType::ARM
                | MachineType::ARMNT
                | MachineType::MIPS16
                | MachineType::MIPSFPU
                | MachineType::MIPSFPU16
                | MachineType::POWERPC
                | MachineType::POWERPCFP
                | MachineType::R3000
                | MachineType::R4000
                | MachineType::R10000
                | MachineType::WCEMIPSV2
                | MachineType::ALPHA
                | MachineType::SH3
                | MachineType::SH3DSP
                | MachineType::SH4
                | MachineType::SH5
                | MachineType::THUMB
                | MachineType::AM33
                | MachineType::M32R
                | MachineType::CEE
        )
    }

    /// Returns true if this machine type represents a 64-bit architecture
    pub fn is_64bit(&self) -> bool {
        matches!(
            self,
            MachineType::AMD64 | MachineType::ARM64 | MachineType::IA64
        )
    }
}

impl From<u16> for MachineType {
    fn from(val: u16) -> Self {
        match val {
            0x014c => MachineType::I386,
            0x014d => MachineType::I486,
            0x014e => MachineType::I860,
            0x8664 => MachineType::AMD64,
            0x01c0 => MachineType::ARM,
            0xaa64 => MachineType::ARM64,
            0x01c4 => MachineType::ARMNT,
            0x0200 => MachineType::IA64,
            0x0266 => MachineType::MIPS16,
            0x0366 => MachineType::MIPSFPU,
            0x0466 => MachineType::MIPSFPU16,
            0x01f0 => MachineType::POWERPC,
            0x01f1 => MachineType::POWERPCFP,
            0x0162 => MachineType::R3000,
            0x0166 => MachineType::R4000,
            0x0168 => MachineType::R10000,
            0x0169 => MachineType::WCEMIPSV2,
            0x0184 => MachineType::ALPHA,
            0x01a2 => MachineType::SH3,
            0x01a3 => MachineType::SH3DSP,
            0x01a6 => MachineType::SH4,
            0x01a8 => MachineType::SH5,
            0x01c2 => MachineType::THUMB,
            0x01d3 => MachineType::AM33,
            0x9041 => MachineType::M32R,
            0xc0ee => MachineType::CEE,
            other => MachineType::Unknown(other),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DosHeader {
    pub e_magic: u16,
    pub e_cblp: u16,
    pub e_cp: u16,
    pub e_crlc: u16,
    pub e_cparhdr: u16,
    pub e_minalloc: u16,
    pub e_maxalloc: u16,
    pub e_ss: u16,
    pub e_sp: u16,
    pub e_csum: u16,
    pub e_ip: u16,
    pub e_cs: u16,
    pub e_lfarlc: u16,
    pub e_ovno: u16,
    pub e_res: [u16; 4],
    pub e_oemid: u16,
    pub e_oeminfo: u16,
    pub e_res2: [u16; 10],
    pub e_lfanew: i32,
}

impl DosHeader {
    pub fn parse<R: Read + Seek>(reader: &mut R) -> Result<Self, crate::steamflatline::error::SteamFlatlineError> {
        let mut buf = [0u8; 64];
        reader.read_exact(&mut buf)?;

        if u16::from_le_bytes([buf[0], buf[1]]) != 0x5A4D { // "MZ"
            return Err(crate::steamflatline::error::SteamFlatlineError::InvalidPeFormat("Not a valid PE file (missing MZ signature)".into()));
        }

        Ok(DosHeader {
            e_magic: u16::from_le_bytes([buf[0], buf[1]]),
            e_cblp: u16::from_le_bytes([buf[2], buf[3]]),
            e_cp: u16::from_le_bytes([buf[4], buf[5]]),
            e_crlc: u16::from_le_bytes([buf[6], buf[7]]),
            e_cparhdr: u16::from_le_bytes([buf[8], buf[9]]),
            e_minalloc: u16::from_le_bytes([buf[10], buf[11]]),
            e_maxalloc: u16::from_le_bytes([buf[12], buf[13]]),
            e_ss: u16::from_le_bytes([buf[14], buf[15]]),
            e_sp: u16::from_le_bytes([buf[16], buf[17]]),
            e_csum: u16::from_le_bytes([buf[18], buf[19]]),
            e_ip: u16::from_le_bytes([buf[20], buf[21]]),
            e_cs: u16::from_le_bytes([buf[22], buf[23]]),
            e_lfarlc: u16::from_le_bytes([buf[24], buf[25]]),
            e_ovno: u16::from_le_bytes([buf[26], buf[27]]),
            e_res: [
                u16::from_le_bytes([buf[28], buf[29]]),
                u16::from_le_bytes([buf[30], buf[31]]),
                u16::from_le_bytes([buf[32], buf[33]]),
                u16::from_le_bytes([buf[34], buf[35]]),
            ],
            e_oemid: u16::from_le_bytes([buf[36], buf[37]]),
            e_oeminfo: u16::from_le_bytes([buf[38], buf[39]]),
            e_res2: [
                u16::from_le_bytes([buf[40], buf[41]]),
                u16::from_le_bytes([buf[42], buf[43]]),
                u16::from_le_bytes([buf[44], buf[45]]),
                u16::from_le_bytes([buf[46], buf[47]]),
                u16::from_le_bytes([buf[48], buf[49]]),
                u16::from_le_bytes([buf[50], buf[51]]),
                u16::from_le_bytes([buf[52], buf[53]]),
                u16::from_le_bytes([buf[54], buf[55]]),
                u16::from_le_bytes([buf[56], buf[57]]),
                u16::from_le_bytes([buf[58], buf[59]]),
            ],
            e_lfanew: i32::from_le_bytes([buf[60], buf[61], buf[62], buf[63]]),
        })
    }
}

#[derive(Debug, Clone)]
pub struct FileHeader {
    pub machine: MachineType,
    pub number_of_sections: u16,
    pub time_date_stamp: u32,
    pub pointer_to_symbol_table: u32,
    pub number_of_symbols: u32,
    pub size_of_optional_header: u16,
    pub characteristics: u16,
}

impl FileHeader {
    pub fn parse<R: Read>(reader: &mut R) -> Result<Self, crate::steamflatline::error::SteamFlatlineError> {
        let mut buf = [0u8; 20];
        reader.read_exact(&mut buf)?;

        let machine = MachineType::from(u16::from_le_bytes([buf[0], buf[1]]));

        // Accept known 32-bit and 64-bit architectures, allow unknown but warn
        if matches!(machine, MachineType::Unknown(_)) {
            // For unknown architectures, we still try to parse but assume 32-bit if the optional header size matches PE32
            // This allows forward compatibility
        }

        Ok(FileHeader {
            machine,
            number_of_sections: u16::from_le_bytes([buf[2], buf[3]]),
            time_date_stamp: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            pointer_to_symbol_table: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            number_of_symbols: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            size_of_optional_header: u16::from_le_bytes([buf[16], buf[17]]),
            characteristics: u16::from_le_bytes([buf[18], buf[19]]),
        })
    }
}

#[derive(Debug, Clone)]
pub struct OptionalHeader32 {
    pub magic: u16,
    pub major_linker_version: u8,
    pub minor_linker_version: u8,
    pub size_of_code: u32,
    pub size_of_initialized_data: u32,
    pub size_of_uninitialized_data: u32,
    pub address_of_entry_point: u32,
    pub base_of_code: u32,
    pub base_of_data: u32,
    pub image_base: u32,
    pub section_alignment: u32,
    pub file_alignment: u32,
    pub major_os_version: u16,
    pub minor_os_version: u16,
    pub major_image_version: u16,
    pub minor_image_version: u16,
    pub major_subsystem_version: u16,
    pub minor_subsystem_version: u16,
    pub win32_version_value: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub check_sum: u32,
    pub subsystem: u16,
    pub dll_characteristics: u16,
    pub size_of_stack_reserve: u32,
    pub size_of_stack_commit: u32,
    pub size_of_heap_reserve: u32,
    pub size_of_heap_commit: u32,
    pub loader_flags: u32,
    pub number_of_rva_and_sizes: u32,
    pub data_directories: [DataDirectory; 16],
}

#[derive(Debug, Clone)]
pub struct OptionalHeader64 {
    pub magic: u16,
    pub major_linker_version: u8,
    pub minor_linker_version: u8,
    pub size_of_code: u32,
    pub size_of_initialized_data: u32,
    pub size_of_uninitialized_data: u32,
    pub address_of_entry_point: u32,
    pub base_of_code: u32,
    pub image_base: u64,
    pub section_alignment: u32,
    pub file_alignment: u32,
    pub major_os_version: u16,
    pub minor_os_version: u16,
    pub major_image_version: u16,
    pub minor_image_version: u16,
    pub major_subsystem_version: u16,
    pub minor_subsystem_version: u16,
    pub win32_version_value: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub check_sum: u32,
    pub subsystem: u16,
    pub dll_characteristics: u16,
    pub size_of_stack_reserve: u64,
    pub size_of_stack_commit: u64,
    pub size_of_heap_reserve: u64,
    pub size_of_heap_commit: u64,
    pub loader_flags: u32,
    pub number_of_rva_and_sizes: u32,
    pub data_directories: [DataDirectory; 16],
}

#[derive(Debug, Clone, Copy)]
pub struct DataDirectory {
    pub virtual_address: u32,
    pub size: u32,
}

impl DataDirectory {
    pub fn parse<R: Read>(reader: &mut R) -> Result<Self, crate::steamflatline::error::SteamFlatlineError> {
        let mut buf = [0u8; 8];
        reader.read_exact(&mut buf)?;
        Ok(DataDirectory {
            virtual_address: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            size: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SectionHeader {
    pub name: [u8; 8],
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub pointer_to_relocations: u32,
    pub pointer_to_linenumbers: u32,
    pub number_of_relocations: u16,
    pub number_of_linenumbers: u16,
    pub characteristics: u32,
}

impl SectionHeader {
    pub fn parse<R: Read>(reader: &mut R) -> Result<Self, crate::steamflatline::error::SteamFlatlineError> {
        let mut buf = [0u8; 40];
        reader.read_exact(&mut buf)?;

        let mut name = [0u8; 8];
        name.copy_from_slice(&buf[0..8]);

        Ok(SectionHeader {
            name,
            virtual_size: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            virtual_address: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            size_of_raw_data: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
            pointer_to_raw_data: u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]),
            pointer_to_relocations: u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]),
            pointer_to_linenumbers: u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]),
            number_of_relocations: u16::from_le_bytes([buf[32], buf[33]]),
            number_of_linenumbers: u16::from_le_bytes([buf[34], buf[35]]),
            characteristics: u32::from_le_bytes([buf[36], buf[37], buf[38], buf[39]]),
        })
    }

    pub fn name_str(&self) -> String {
        String::from_utf8_lossy(&self.name).trim_end_matches('\0').to_string()
    }
}

#[derive(Debug, Clone)]
pub enum OptionalHeader {
    Pe32(OptionalHeader32),
    Pe64(OptionalHeader64),
}

impl OptionalHeader {
    pub fn address_of_entry_point(&self) -> u32 {
        match self {
            OptionalHeader::Pe32(h) => h.address_of_entry_point,
            OptionalHeader::Pe64(h) => h.address_of_entry_point,
        }
    }

    pub fn image_base(&self) -> u64 {
        match self {
            OptionalHeader::Pe32(h) => h.image_base as u64,
            OptionalHeader::Pe64(h) => h.image_base,
        }
    }

    pub fn section_alignment(&self) -> u32 {
        match self {
            OptionalHeader::Pe32(h) => h.section_alignment,
            OptionalHeader::Pe64(h) => h.section_alignment,
        }
    }

    pub fn data_directories(&self) -> &[DataDirectory] {
        match self {
            OptionalHeader::Pe32(h) => &h.data_directories,
            OptionalHeader::Pe64(h) => &h.data_directories,
        }
    }

    pub fn is_64bit(&self) -> bool {
        matches!(self, OptionalHeader::Pe64(_))
    }

    pub fn parse<R: Read + Seek>(reader: &mut R, is_64bit: bool, size: u16) -> Result<Self, crate::steamflatline::error::SteamFlatlineError> {
        if is_64bit {
            let mut buf = Vec::with_capacity(size as usize);
            buf.resize(size as usize, 0);
            reader.read_exact(&mut buf)?;

            if u16::from_le_bytes([buf[0], buf[1]]) != 0x20b {
                return Err(crate::steamflatline::error::SteamFlatlineError::InvalidPeFormat("Invalid PE64 magic".into()));
            }

            // PE64 optional header is 224 bytes + data directories
            let mut offset = 0;
            let magic = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let major_linker_version = buf[offset]; offset += 1;
            let minor_linker_version = buf[offset]; offset += 1;
            let size_of_code = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_initialized_data = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_uninitialized_data = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let address_of_entry_point = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let base_of_code = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let image_base = u64::from_le_bytes([
                buf[offset], buf[offset+1], buf[offset+2], buf[offset+3],
                buf[offset+4], buf[offset+5], buf[offset+6], buf[offset+7],
            ]); offset += 8;
            let section_alignment = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let file_alignment = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let major_os_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let minor_os_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let major_image_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let minor_image_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let major_subsystem_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let minor_subsystem_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let win32_version_value = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_image = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_headers = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let check_sum = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let subsystem = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let dll_characteristics = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let size_of_stack_reserve = u64::from_le_bytes([
                buf[offset], buf[offset+1], buf[offset+2], buf[offset+3],
                buf[offset+4], buf[offset+5], buf[offset+6], buf[offset+7],
            ]); offset += 8;
            let size_of_stack_commit = u64::from_le_bytes([
                buf[offset], buf[offset+1], buf[offset+2], buf[offset+3],
                buf[offset+4], buf[offset+5], buf[offset+6], buf[offset+7],
            ]); offset += 8;
            let size_of_heap_reserve = u64::from_le_bytes([
                buf[offset], buf[offset+1], buf[offset+2], buf[offset+3],
                buf[offset+4], buf[offset+5], buf[offset+6], buf[offset+7],
            ]); offset += 8;
            let size_of_heap_commit = u64::from_le_bytes([
                buf[offset], buf[offset+1], buf[offset+2], buf[offset+3],
                buf[offset+4], buf[offset+5], buf[offset+6], buf[offset+7],
            ]); offset += 8;
            let loader_flags = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let number_of_rva_and_sizes = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;

            let mut data_directories = [DataDirectory { virtual_address: 0, size: 0 }; 16];
            for i in 0..16 {
                if offset + 8 <= buf.len() {
                    data_directories[i] = DataDirectory {
                        virtual_address: u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]),
                        size: u32::from_le_bytes([buf[offset+4], buf[offset+5], buf[offset+6], buf[offset+7]]),
                    };
                    offset += 8;
                }
            }

            Ok(OptionalHeader::Pe64(OptionalHeader64 {
                magic,
                major_linker_version,
                minor_linker_version,
                size_of_code,
                size_of_initialized_data,
                size_of_uninitialized_data,
                address_of_entry_point,
                base_of_code,
                image_base,
                section_alignment,
                file_alignment,
                major_os_version,
                minor_os_version,
                major_image_version,
                minor_image_version,
                major_subsystem_version,
                minor_subsystem_version,
                win32_version_value,
                size_of_image,
                size_of_headers,
                check_sum,
                subsystem,
                dll_characteristics,
                size_of_stack_reserve,
                size_of_stack_commit,
                size_of_heap_reserve,
                size_of_heap_commit,
                loader_flags,
                number_of_rva_and_sizes,
                data_directories,
            }))
        } else {
            let mut buf = Vec::with_capacity(size as usize);
            buf.resize(size as usize, 0);
            reader.read_exact(&mut buf)?;

            if u16::from_le_bytes([buf[0], buf[1]]) != 0x10b {
                return Err(crate::steamflatline::error::SteamFlatlineError::InvalidPeFormat("Invalid PE32 magic".into()));
            }

            // PE32 optional header is 208 bytes + data directories
            let mut offset = 0;
            let magic = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let major_linker_version = buf[offset]; offset += 1;
            let minor_linker_version = buf[offset]; offset += 1;
            let size_of_code = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_initialized_data = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_uninitialized_data = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let address_of_entry_point = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let base_of_code = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let base_of_data = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let image_base = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let section_alignment = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let file_alignment = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let major_os_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let minor_os_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let major_image_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let minor_image_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let major_subsystem_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let minor_subsystem_version = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let win32_version_value = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_image = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_headers = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let check_sum = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let subsystem = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let dll_characteristics = u16::from_le_bytes([buf[offset], buf[offset+1]]); offset += 2;
            let size_of_stack_reserve = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_stack_commit = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_heap_reserve = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let size_of_heap_commit = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let loader_flags = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;
            let number_of_rva_and_sizes = u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]); offset += 4;

            let mut data_directories = [DataDirectory { virtual_address: 0, size: 0 }; 16];
            for i in 0..16 {
                if offset + 8 <= buf.len() {
                    data_directories[i] = DataDirectory {
                        virtual_address: u32::from_le_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]),
                        size: u32::from_le_bytes([buf[offset+4], buf[offset+5], buf[offset+6], buf[offset+7]]),
                    };
                    offset += 8;
                }
            }

            Ok(OptionalHeader::Pe32(OptionalHeader32 {
                magic,
                major_linker_version,
                minor_linker_version,
                size_of_code,
                size_of_initialized_data,
                size_of_uninitialized_data,
                address_of_entry_point,
                base_of_code,
                base_of_data,
                image_base,
                section_alignment,
                file_alignment,
                major_os_version,
                minor_os_version,
                major_image_version,
                minor_image_version,
                major_subsystem_version,
                minor_subsystem_version,
                win32_version_value,
                size_of_image,
                size_of_headers,
                check_sum,
                subsystem,
                dll_characteristics,
                size_of_stack_reserve,
                size_of_stack_commit,
                size_of_heap_reserve,
                size_of_heap_commit,
                loader_flags,
                number_of_rva_and_sizes,
                data_directories,
            }))
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeFile {
    pub dos_header: DosHeader,
    pub dos_stub: Vec<u8>,
    pub file_header: FileHeader,
    pub optional_header: OptionalHeader,
    pub sections: Vec<SectionHeader>,
    pub section_data: Vec<Vec<u8>>,
    pub overlay_data: Option<Vec<u8>>,
    pub architecture: Architecture,
}

impl PeFile {
    pub fn parse<P: AsRef<Path>>(path: P) -> Result<Self, crate::steamflatline::error::SteamFlatlineError> {
        let mut file = File::open(path)?;
        let file_size = file.metadata()?.len();

        // Parse DOS header
        let dos_header = DosHeader::parse(&mut file)?;

        // Read DOS stub (between DOS header and PE header)
        let pe_offset = dos_header.e_lfanew as u64;
        let stub_size = if pe_offset > 64 { pe_offset - 64 } else { 0 };
        let mut dos_stub = vec![0u8; stub_size as usize];
        if stub_size > 0 {
            file.seek(SeekFrom::Start(64))?;
            file.read_exact(&mut dos_stub)?;
        }

        // Seek to PE header
        file.seek(SeekFrom::Start(pe_offset))?;

        // Check PE signature
        let mut pe_sig = [0u8; 4];
        file.read_exact(&mut pe_sig)?;
        if &pe_sig != b"PE\0\0" {
            return Err(crate::steamflatline::error::SteamFlatlineError::InvalidPeFormat("Missing PE signature".into()));
        }

        // Parse file header
        let file_header = FileHeader::parse(&mut file)?;

        // Determine architecture based on machine type
        let is_64bit = file_header.machine.is_64bit();
        let architecture = if is_64bit { Architecture::Pe64 } else { Architecture::Pe32 };

        // Parse optional header
        let optional_header = OptionalHeader::parse(&mut file, is_64bit, file_header.size_of_optional_header)?;

        // Parse section headers
        let mut sections = Vec::with_capacity(file_header.number_of_sections as usize);
        for _ in 0..file_header.number_of_sections {
            sections.push(SectionHeader::parse(&mut file)?);
        }

        // Read section data
        let mut section_data = Vec::with_capacity(sections.len());
        for section in &sections {
            if section.size_of_raw_data > 0 && section.pointer_to_raw_data > 0 {
                let mut data = vec![0u8; section.size_of_raw_data as usize];
                file.seek(SeekFrom::Start(section.pointer_to_raw_data as u64))?;
                file.read_exact(&mut data)?;
                section_data.push(data);
            } else {
                section_data.push(Vec::new());
            }
        }

        // Read overlay data (anything after the last section)
        let last_section_end = sections.iter()
            .map(|s| s.pointer_to_raw_data as u64 + s.size_of_raw_data as u64)
            .max()
            .unwrap_or(0);

        let overlay_data = if last_section_end < file_size {
            let mut data = vec![0u8; (file_size - last_section_end) as usize];
            file.seek(SeekFrom::Start(last_section_end))?;
            file.read_exact(&mut data)?;
            Some(data)
        } else {
            None
        };

        Ok(PeFile {
            dos_header,
            dos_stub,
            file_header,
            optional_header,
            sections,
            section_data,
            overlay_data,
            architecture,
        })
    }

    pub fn get_section(&self, name: &str) -> Option<(&SectionHeader, &[u8])> {
        for (i, section) in self.sections.iter().enumerate() {
            if section.name_str() == name {
                return Some((section, &self.section_data[i]));
            }
        }
        None
    }

    pub fn get_section_mut(&mut self, name: &str) -> Option<(&mut SectionHeader, &mut Vec<u8>)> {
        for (i, section) in self.sections.iter_mut().enumerate() {
            if section.name_str() == name {
                return Some((section, &mut self.section_data[i]));
            }
        }
        None
    }

    pub fn has_section(&self, name: &str) -> bool {
        self.sections.iter().any(|s| s.name_str() == name)
    }

    pub fn rva_to_file_offset(&self, rva: u32) -> Option<u64> {
        for (i, section) in self.sections.iter().enumerate() {
            let start = section.virtual_address;
            let end = start + section.virtual_size.max(section.size_of_raw_data);
            if rva >= start && rva < end {
                let offset_in_section = rva - start;
                return Some(self.sections[i].pointer_to_raw_data as u64 + offset_in_section as u64);
            }
        }
        None
    }

    pub fn va_to_rva(&self, va: u64) -> u32 {
        (va - self.optional_header.image_base()) as u32
    }

    pub fn rva_to_va(&self, rva: u32) -> u64 {
        self.optional_header.image_base() + rva as u64
    }

    pub fn write<P: AsRef<Path>>(&self, path: P) -> Result<(), crate::steamflatline::error::SteamFlatlineError> {
        let mut file = File::create(path)?;

        // Write DOS header
        let mut dos_header_bytes = Vec::with_capacity(64);
        dos_header_bytes.extend_from_slice(&self.dos_header.e_magic.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_cblp.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_cp.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_crlc.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_cparhdr.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_minalloc.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_maxalloc.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_ss.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_sp.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_csum.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_ip.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_cs.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_lfarlc.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_ovno.to_le_bytes());
        for v in &self.dos_header.e_res { dos_header_bytes.extend_from_slice(&v.to_le_bytes()); }
        dos_header_bytes.extend_from_slice(&self.dos_header.e_oemid.to_le_bytes());
        dos_header_bytes.extend_from_slice(&self.dos_header.e_oeminfo.to_le_bytes());
        for v in &self.dos_header.e_res2 { dos_header_bytes.extend_from_slice(&v.to_le_bytes()); }
        dos_header_bytes.extend_from_slice(&self.dos_header.e_lfanew.to_le_bytes());
        file.write_all(&dos_header_bytes)?;

        // Write DOS stub
        if !self.dos_stub.is_empty() {
            file.write_all(&self.dos_stub)?;
        }

        // Write PE signature
        file.write_all(b"PE\0\0")?;

        // Write file header
        let mut file_header_bytes = Vec::with_capacity(20);
        file_header_bytes.extend_from_slice(&match self.file_header.machine {
            MachineType::I386 => 0x014cu16,
            MachineType::I486 => 0x014du16,
            MachineType::I860 => 0x014eu16,
            MachineType::AMD64 => 0x8664u16,
            MachineType::ARM => 0x01c0u16,
            MachineType::ARM64 => 0xaa64u16,
            MachineType::ARMNT => 0x01c4u16,
            MachineType::IA64 => 0x0200u16,
            MachineType::MIPS16 => 0x0266u16,
            MachineType::MIPSFPU => 0x0366u16,
            MachineType::MIPSFPU16 => 0x0466u16,
            MachineType::POWERPC => 0x01f0u16,
            MachineType::POWERPCFP => 0x01f1u16,
            MachineType::R3000 => 0x0162u16,
            MachineType::R4000 => 0x0166u16,
            MachineType::R10000 => 0x0168u16,
            MachineType::WCEMIPSV2 => 0x0169u16,
            MachineType::ALPHA => 0x0184u16,
            MachineType::SH3 => 0x01a2u16,
            MachineType::SH3DSP => 0x01a3u16,
            MachineType::SH4 => 0x01a6u16,
            MachineType::SH5 => 0x01a8u16,
            MachineType::THUMB => 0x01c2u16,
            MachineType::AM33 => 0x01d3u16,
            MachineType::M32R => 0x9041u16,
            MachineType::CEE => 0xc0eeu16,
            MachineType::Unknown(v) => v,
        }.to_le_bytes());
        file_header_bytes.extend_from_slice(&self.file_header.number_of_sections.to_le_bytes());
        file_header_bytes.extend_from_slice(&self.file_header.time_date_stamp.to_le_bytes());
        file_header_bytes.extend_from_slice(&self.file_header.pointer_to_symbol_table.to_le_bytes());
        file_header_bytes.extend_from_slice(&self.file_header.number_of_symbols.to_le_bytes());
        file_header_bytes.extend_from_slice(&self.file_header.size_of_optional_header.to_le_bytes());
        file_header_bytes.extend_from_slice(&self.file_header.characteristics.to_le_bytes());
        file.write_all(&file_header_bytes)?;

        // Write optional header
        match &self.optional_header {
            OptionalHeader::Pe32(h) => {
                let mut opt_bytes = Vec::with_capacity(h.number_of_rva_and_sizes as usize * 8 + 96);
                opt_bytes.extend_from_slice(&h.magic.to_le_bytes());
                opt_bytes.push(h.major_linker_version);
                opt_bytes.push(h.minor_linker_version);
                opt_bytes.extend_from_slice(&h.size_of_code.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_initialized_data.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_uninitialized_data.to_le_bytes());
                opt_bytes.extend_from_slice(&h.address_of_entry_point.to_le_bytes());
                opt_bytes.extend_from_slice(&h.base_of_code.to_le_bytes());
                opt_bytes.extend_from_slice(&h.base_of_data.to_le_bytes());
                opt_bytes.extend_from_slice(&h.image_base.to_le_bytes());
                opt_bytes.extend_from_slice(&h.section_alignment.to_le_bytes());
                opt_bytes.extend_from_slice(&h.file_alignment.to_le_bytes());
                opt_bytes.extend_from_slice(&h.major_os_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.minor_os_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.major_image_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.minor_image_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.major_subsystem_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.minor_subsystem_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.win32_version_value.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_image.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_headers.to_le_bytes());
                opt_bytes.extend_from_slice(&h.check_sum.to_le_bytes());
                opt_bytes.extend_from_slice(&h.subsystem.to_le_bytes());
                opt_bytes.extend_from_slice(&h.dll_characteristics.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_stack_reserve.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_stack_commit.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_heap_reserve.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_heap_commit.to_le_bytes());
                opt_bytes.extend_from_slice(&h.loader_flags.to_le_bytes());
                opt_bytes.extend_from_slice(&h.number_of_rva_and_sizes.to_le_bytes());
                for dd in &h.data_directories {
                    opt_bytes.extend_from_slice(&dd.virtual_address.to_le_bytes());
                    opt_bytes.extend_from_slice(&dd.size.to_le_bytes());
                }
                file.write_all(&opt_bytes)?;
            }
            OptionalHeader::Pe64(h) => {
                let mut opt_bytes = Vec::with_capacity(h.number_of_rva_and_sizes as usize * 8 + 112);
                opt_bytes.extend_from_slice(&h.magic.to_le_bytes());
                opt_bytes.push(h.major_linker_version);
                opt_bytes.push(h.minor_linker_version);
                opt_bytes.extend_from_slice(&h.size_of_code.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_initialized_data.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_uninitialized_data.to_le_bytes());
                opt_bytes.extend_from_slice(&h.address_of_entry_point.to_le_bytes());
                opt_bytes.extend_from_slice(&h.base_of_code.to_le_bytes());
                opt_bytes.extend_from_slice(&h.image_base.to_le_bytes());
                opt_bytes.extend_from_slice(&h.section_alignment.to_le_bytes());
                opt_bytes.extend_from_slice(&h.file_alignment.to_le_bytes());
                opt_bytes.extend_from_slice(&h.major_os_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.minor_os_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.major_image_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.minor_image_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.major_subsystem_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.minor_subsystem_version.to_le_bytes());
                opt_bytes.extend_from_slice(&h.win32_version_value.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_image.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_headers.to_le_bytes());
                opt_bytes.extend_from_slice(&h.check_sum.to_le_bytes());
                opt_bytes.extend_from_slice(&h.subsystem.to_le_bytes());
                opt_bytes.extend_from_slice(&h.dll_characteristics.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_stack_reserve.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_stack_commit.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_heap_reserve.to_le_bytes());
                opt_bytes.extend_from_slice(&h.size_of_heap_commit.to_le_bytes());
                opt_bytes.extend_from_slice(&h.loader_flags.to_le_bytes());
                opt_bytes.extend_from_slice(&h.number_of_rva_and_sizes.to_le_bytes());
                for dd in &h.data_directories {
                    opt_bytes.extend_from_slice(&dd.virtual_address.to_le_bytes());
                    opt_bytes.extend_from_slice(&dd.size.to_le_bytes());
                }
                file.write_all(&opt_bytes)?;
            }
        }

        // Write section headers
        for section in &self.sections {
            let mut sec_bytes = Vec::with_capacity(40);
            sec_bytes.extend_from_slice(&section.name);
            sec_bytes.extend_from_slice(&section.virtual_size.to_le_bytes());
            sec_bytes.extend_from_slice(&section.virtual_address.to_le_bytes());
            sec_bytes.extend_from_slice(&section.size_of_raw_data.to_le_bytes());
            sec_bytes.extend_from_slice(&section.pointer_to_raw_data.to_le_bytes());
            sec_bytes.extend_from_slice(&section.pointer_to_relocations.to_le_bytes());
            sec_bytes.extend_from_slice(&section.pointer_to_linenumbers.to_le_bytes());
            sec_bytes.extend_from_slice(&section.number_of_relocations.to_le_bytes());
            sec_bytes.extend_from_slice(&section.number_of_linenumbers.to_le_bytes());
            sec_bytes.extend_from_slice(&section.characteristics.to_le_bytes());
            file.write_all(&sec_bytes)?;
        }

        // Write section data (aligned to file alignment)
        let file_alignment = match &self.optional_header {
            OptionalHeader::Pe32(h) => h.file_alignment,
            OptionalHeader::Pe64(h) => h.file_alignment,
        };

        for (i, section) in self.sections.iter().enumerate() {
            if section.size_of_raw_data > 0 {
                // Seek to the section's raw data offset
                file.seek(SeekFrom::Start(section.pointer_to_raw_data as u64))?;
                file.write_all(&self.section_data[i])?;
            }
        }

        // Write overlay data
        if let Some(overlay) = &self.overlay_data {
            file.write_all(overlay)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pe_module_compiles() {
        assert!(true);
    }
}