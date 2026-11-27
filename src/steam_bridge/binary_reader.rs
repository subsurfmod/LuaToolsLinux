//! ELF binary reading and pattern scanning for Steam Linux binaries (32-bit `ELFCLASS32` & 64-bit `ELFCLASS64`).
//!
//! Reads ELF headers, program headers, section headers, symbol tables, and dynamic
//! symbols from Steam Linux binaries (`steamclient.so`, `steamui.so`, `steam` across both
//! `ubuntu12_32` and `linux64`/`ubuntu12_64`) so that integration points can be located,
//! ASLR runtime addresses resolved via `/proc/self/maps`, and Steam build compatibility
//! verified via SHA-256 pattern matching.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::steam_bridge::ost_platform::{byte_search, hash};

const ELF32_HEADER_LEN: usize = 52;
const ELF32_PHDR_MIN_LEN: usize = 32;
const ELF32_SHDR_MIN_LEN: usize = 40;
const ELF32_SYM_MIN_LEN: usize = 16;

const ELF64_HEADER_LEN: usize = 64;
const ELF64_PHDR_MIN_LEN: usize = 56;
const ELF64_SHDR_MIN_LEN: usize = 64;
const ELF64_SYM_MIN_LEN: usize = 24;

const PT_LOAD: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_NOBITS: u32 = 8;
const SHT_DYNSYM: u32 = 11;
const SHF_ALLOC: u64 = 0x2;

/// Error type for binary reading operations.
#[derive(Debug, thiserror::Error)]
pub enum BinaryError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Not a valid ELF file")]
    NotElf,
    #[error("Unsupported ELF class: {0}")]
    UnsupportedClass(u8),
    #[error("Unsupported ELF data encoding: {0}")]
    UnsupportedData(u8),
    #[error("Section not found: {0}")]
    SectionNotFound(String),
    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),
    #[error("Parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfClass {
    Class32,
    Class64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfData {
    Lsb,
    Msb,
}

#[derive(Debug, Clone)]
pub struct BinaryHeader64 {
    pub entry: u64,
    pub phoff: u64,
    pub shoff: u64,
    pub phnum: u16,
    pub shnum: u16,
    pub shstrndx: u16,
}

#[derive(Debug, Clone)]
pub struct ProgramHeader64 {
    pub r#type: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
}

#[derive(Debug, Clone)]
pub struct SectionHeader64 {
    pub name_offset: u32,
    pub r#type: u32,
    pub flags: u64,
    pub addr: u64,
    pub offset: u64,
    pub size: u64,
    pub link: u32,
    pub info: u32,
    pub align: u64,
    pub entsize: u64,
}

#[derive(Debug, Clone)]
pub struct Symbol64 {
    pub name_offset: u32,
    pub info: u8,
    pub other: u8,
    pub shndx: u16,
    pub value: u64,
    pub size: u64,
}

/// Live runtime mapping of a loaded ELF module discovered from `/proc/self/maps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedModuleMapping {
    /// Virtual load base (`mapping_start - file_offset`) for translating ELF virtual addresses.
    pub base_addr: u64,
    /// Mapped readable+executable (`r-x` / `rwx`) virtual address ranges `[start, end)`.
    pub exec_ranges: Vec<(u64, u64)>,
}

impl LoadedModuleMapping {
    /// Returns true if `[addr, addr + len)` lies completely within an executable mapping.
    pub fn contains_executable_range(&self, addr: u64, len: usize) -> bool {
        let Some(end) = addr.checked_add(len as u64) else {
            return false;
        };
        if addr == 0 || len == 0 {
            return false;
        }
        self.exec_ranges
            .iter()
            .any(|&(start, stop)| addr >= start && end <= stop)
    }
}

#[derive(Debug, Clone)]
pub struct BinaryFile {
    pub path: PathBuf,
    pub class: ElfClass,
    pub data: ElfData,
    pub header: BinaryHeader64,
    pub program_headers: Vec<ProgramHeader64>,
    pub min_load_vaddr: u64,
    pub sections: Vec<SectionHeader64>,
    pub section_names: Vec<String>,
    pub symbols: BTreeMap<String, Symbol64>,
    pub dyn_symbols: BTreeMap<String, Symbol64>,
    pub raw_bytes: Vec<u8>,
    pub sha256: String,
}

/// Unified ELF binary image alias supporting both `ELFCLASS32` and `ELFCLASS64`.
pub type BinaryImage = BinaryFile;

fn read_u16_le(raw: &[u8], off: usize) -> Result<u16, BinaryError> {
    let end = off
        .checked_add(2)
        .ok_or_else(|| BinaryError::Parse("u16 offset overflow".into()))?;
    let bytes: [u8; 2] = raw
        .get(off..end)
        .ok_or_else(|| BinaryError::Parse("unexpected EOF reading u16".into()))?
        .try_into()
        .map_err(|_| BinaryError::Parse("invalid u16 slice".into()))?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_le(raw: &[u8], off: usize) -> Result<u32, BinaryError> {
    let end = off
        .checked_add(4)
        .ok_or_else(|| BinaryError::Parse("u32 offset overflow".into()))?;
    let bytes: [u8; 4] = raw
        .get(off..end)
        .ok_or_else(|| BinaryError::Parse("unexpected EOF reading u32".into()))?
        .try_into()
        .map_err(|_| BinaryError::Parse("invalid u32 slice".into()))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64_le(raw: &[u8], off: usize) -> Result<u64, BinaryError> {
    let end = off
        .checked_add(8)
        .ok_or_else(|| BinaryError::Parse("u64 offset overflow".into()))?;
    let bytes: [u8; 8] = raw
        .get(off..end)
        .ok_or_else(|| BinaryError::Parse("unexpected EOF reading u64".into()))?
        .try_into()
        .map_err(|_| BinaryError::Parse("invalid u64 slice".into()))?;
    Ok(u64::from_le_bytes(bytes))
}

impl BinaryFile {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, BinaryError> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)?;
        Self::from_bytes(path, raw)
    }

    pub fn from_bytes(path: PathBuf, raw: Vec<u8>) -> Result<Self, BinaryError> {
        let sha256 = hash::sha256_of_buffer(&raw)
            .unwrap_or_else(|_| format!("{:x}", Sha256::digest(&raw)));

        if raw.len() < 16 || &raw[0..4] != b"\x7fELF" {
            return Err(BinaryError::NotElf);
        }

        let class = match raw[4] {
            1 => ElfClass::Class32,
            2 => ElfClass::Class64,
            c => return Err(BinaryError::UnsupportedClass(c)),
        };
        let data = match raw[5] {
            1 => ElfData::Lsb,
            2 => ElfData::Msb,
            d => return Err(BinaryError::UnsupportedData(d)),
        };

        if data != ElfData::Lsb {
            return Err(BinaryError::UnsupportedData(raw[5]));
        }

        let (header, phentsize, shentsize) = match class {
            ElfClass::Class32 => {
                if raw.len() < ELF32_HEADER_LEN {
                    return Err(BinaryError::Parse("ELF32 header truncated".into()));
                }
                // Standard Elf32_Ehdr byte layout (little-endian, 52 bytes):
                //   24..28: e_entry (u32)
                //   28..32: e_phoff (u32)
                //   32..36: e_shoff (u32)
                //   42..44: e_phentsize (u16)
                //   44..46: e_phnum (u16)
                //   46..48: e_shentsize (u16)
                //   48..50: e_shnum (u16)
                //   50..52: e_shstrndx (u16)
                let entry = read_u32_le(&raw, 24)? as u64;
                let phoff = read_u32_le(&raw, 28)? as u64;
                let shoff = read_u32_le(&raw, 32)? as u64;
                let phentsize = read_u16_le(&raw, 42)? as usize;
                let phnum = read_u16_le(&raw, 44)?;
                let shentsize = read_u16_le(&raw, 46)? as usize;
                let shnum = read_u16_le(&raw, 48)?;
                let shstrndx = read_u16_le(&raw, 50)?;
                (
                    BinaryHeader64 {
                        entry,
                        phoff,
                        shoff,
                        phnum,
                        shnum,
                        shstrndx,
                    },
                    phentsize,
                    shentsize,
                )
            }
            ElfClass::Class64 => {
                if raw.len() < ELF64_HEADER_LEN {
                    return Err(BinaryError::Parse("ELF64 header truncated".into()));
                }
                // Standard Elf64_Ehdr byte layout (little-endian, 64 bytes):
                //   24..32: e_entry (u64)
                //   32..40: e_phoff (u64)
                //   40..48: e_shoff (u64)
                //   54..56: e_phentsize (u16)
                //   56..58: e_phnum (u16)
                //   58..60: e_shentsize (u16)
                //   60..62: e_shnum (u16)
                //   62..64: e_shstrndx (u16)
                let entry = read_u64_le(&raw, 24)?;
                let phoff = read_u64_le(&raw, 32)?;
                let shoff = read_u64_le(&raw, 40)?;
                let phentsize = read_u16_le(&raw, 54)? as usize;
                let phnum = read_u16_le(&raw, 56)?;
                let shentsize = read_u16_le(&raw, 58)? as usize;
                let shnum = read_u16_le(&raw, 60)?;
                let shstrndx = read_u16_le(&raw, 62)?;
                (
                    BinaryHeader64 {
                        entry,
                        phoff,
                        shoff,
                        phnum,
                        shnum,
                        shstrndx,
                    },
                    phentsize,
                    shentsize,
                )
            }
        };

        // Parse program headers (`Elf32_Phdr` 32B / `Elf64_Phdr` 56B) and determine lowest PT_LOAD p_vaddr
        // so relative virtual addresses translate cleanly to live ASLR runtime addresses.
        let min_phdr_len = match class {
            ElfClass::Class32 => ELF32_PHDR_MIN_LEN,
            ElfClass::Class64 => ELF64_PHDR_MIN_LEN,
        };
        let mut program_headers = Vec::new();
        let mut min_load_vaddr: Option<u64> = None;
        if header.phnum > 0 && phentsize >= min_phdr_len {
            if let Ok(phoff_usize) = usize::try_from(header.phoff) {
                for i in 0..(header.phnum as usize) {
                    let Some(delta) = i.checked_mul(phentsize) else {
                        break;
                    };
                    let Some(off) = phoff_usize.checked_add(delta) else {
                        break;
                    };
                    let Some(end) = off.checked_add(min_phdr_len) else {
                        break;
                    };
                    if end > raw.len() {
                        break;
                    }
                    let phdr = match class {
                        ElfClass::Class32 => {
                            // Elf32_Phdr: p_type @0, p_offset @4, p_vaddr @8, p_paddr @12,
                            //             p_filesz @16, p_memsz @20, p_flags @24, p_align @28
                            ProgramHeader64 {
                                r#type: read_u32_le(&raw, off)?,
                                offset: read_u32_le(&raw, off + 4)? as u64,
                                vaddr: read_u32_le(&raw, off + 8)? as u64,
                                filesz: read_u32_le(&raw, off + 16)? as u64,
                                memsz: read_u32_le(&raw, off + 20)? as u64,
                                flags: read_u32_le(&raw, off + 24)?,
                            }
                        }
                        ElfClass::Class64 => {
                            // Elf64_Phdr: p_type @0, p_flags @4, p_offset @8, p_vaddr @16,
                            //             p_paddr @24, p_filesz @32, p_memsz @40
                            ProgramHeader64 {
                                r#type: read_u32_le(&raw, off)?,
                                flags: read_u32_le(&raw, off + 4)?,
                                offset: read_u64_le(&raw, off + 8)?,
                                vaddr: read_u64_le(&raw, off + 16)?,
                                filesz: read_u64_le(&raw, off + 32)?,
                                memsz: read_u64_le(&raw, off + 40)?,
                            }
                        }
                    };
                    if phdr.r#type == PT_LOAD {
                        min_load_vaddr = Some(match min_load_vaddr {
                            Some(cur) => cur.min(phdr.vaddr),
                            None => phdr.vaddr,
                        });
                    }
                    program_headers.push(phdr);
                }
            }
        }
        let min_load_vaddr = min_load_vaddr.unwrap_or(0);

        let min_shdr_len = match class {
            ElfClass::Class32 => ELF32_SHDR_MIN_LEN,
            ElfClass::Class64 => ELF64_SHDR_MIN_LEN,
        };
        let mut sections = Vec::new();
        if header.shnum > 0 {
            if shentsize < min_shdr_len {
                return Err(BinaryError::Parse(format!(
                    "invalid {:?} section header entry size: {}",
                    class, shentsize
                )));
            }
            let shoff_usize = usize::try_from(header.shoff)
                .map_err(|_| BinaryError::Parse("section header table offset overflow".into()))?;
            let table_bytes = (header.shnum as usize)
                .checked_mul(shentsize)
                .ok_or_else(|| BinaryError::Parse("section header table size overflow".into()))?;
            let table_end = shoff_usize
                .checked_add(table_bytes)
                .ok_or_else(|| BinaryError::Parse("section header table end overflow".into()))?;
            if table_end > raw.len() {
                return Err(BinaryError::Parse("section header truncated".into()));
            }

            sections.reserve(header.shnum as usize);
            for i in 0..(header.shnum as usize) {
                let off = shoff_usize + i * shentsize;
                let shdr = match class {
                    ElfClass::Class32 => {
                        // Elf32_Shdr (40 bytes):
                        //   0..4 name, 4..8 type, 8..12 flags, 12..16 addr, 16..20 offset,
                        //   20..24 size, 24..28 link, 28..32 info, 32..36 addralign, 36..40 entsize
                        SectionHeader64 {
                            name_offset: read_u32_le(&raw, off)?,
                            r#type: read_u32_le(&raw, off + 4)?,
                            flags: read_u32_le(&raw, off + 8)? as u64,
                            addr: read_u32_le(&raw, off + 12)? as u64,
                            offset: read_u32_le(&raw, off + 16)? as u64,
                            size: read_u32_le(&raw, off + 20)? as u64,
                            link: read_u32_le(&raw, off + 24)?,
                            info: read_u32_le(&raw, off + 28)?,
                            align: read_u32_le(&raw, off + 32)? as u64,
                            entsize: read_u32_le(&raw, off + 36)? as u64,
                        }
                    }
                    ElfClass::Class64 => SectionHeader64 {
                        name_offset: read_u32_le(&raw, off)?,
                        r#type: read_u32_le(&raw, off + 4)?,
                        flags: read_u64_le(&raw, off + 8)?,
                        addr: read_u64_le(&raw, off + 16)?,
                        offset: read_u64_le(&raw, off + 24)?,
                        size: read_u64_le(&raw, off + 32)?,
                        link: read_u32_le(&raw, off + 40)?,
                        info: read_u32_le(&raw, off + 44)?,
                        align: read_u64_le(&raw, off + 48)?,
                        entsize: read_u64_le(&raw, off + 56)?,
                    },
                };
                sections.push(shdr);
            }
        }

        let section_names = if let Some(strtab_sec) = sections.get(header.shstrndx as usize) {
            let strtab_data = Self::section_slice(&raw, strtab_sec);
            Self::read_section_names(&sections, strtab_data)
        } else {
            Vec::new()
        };

        let min_sym_len = match class {
            ElfClass::Class32 => ELF32_SYM_MIN_LEN,
            ElfClass::Class64 => ELF64_SYM_MIN_LEN,
        };
        let mut symbols = BTreeMap::new();
        let mut dyn_symbols = BTreeMap::new();

        for sec in &sections {
            if sec.r#type != SHT_SYMTAB && sec.r#type != SHT_DYNSYM {
                continue;
            }
            // Guard against malformed or zero entsize (which previously caused unbounded loops).
            if sec.entsize < min_sym_len as u64 {
                continue;
            }
            let Ok(sec_off) = usize::try_from(sec.offset) else {
                continue;
            };
            if sec_off >= raw.len() {
                continue;
            }
            let Ok(entry_size) = usize::try_from(sec.entsize) else {
                continue;
            };

            let strtab_data = sections
                .get(sec.link as usize)
                .map(|st| Self::section_slice(&raw, st))
                .unwrap_or(&[]);

            let max_avail = (raw.len() - sec_off) as u64;
            let effective_size = sec.size.min(max_avail);
            let sym_count = (effective_size / sec.entsize) as usize;

            for j in 0..sym_count {
                let Some(delta) = j.checked_mul(entry_size) else {
                    break;
                };
                let Some(off) = sec_off.checked_add(delta) else {
                    break;
                };
                let Some(end) = off.checked_add(min_sym_len) else {
                    break;
                };
                if end > raw.len() {
                    break;
                }

                let sym = match class {
                    ElfClass::Class32 => {
                        // Elf32_Sym (16 bytes):
                        //   st_name (u32 @0), st_value (u32 @4), st_size (u32 @8),
                        //   st_info (u8 @12), st_other (u8 @13), st_shndx (u16 @14)
                        let Ok(name_offset) = read_u32_le(&raw, off) else {
                            break;
                        };
                        let Ok(value) = read_u32_le(&raw, off + 4) else {
                            break;
                        };
                        let Ok(size) = read_u32_le(&raw, off + 8) else {
                            break;
                        };
                        let info = raw[off + 12];
                        let other = raw[off + 13];
                        let Ok(shndx) = read_u16_le(&raw, off + 14) else {
                            break;
                        };
                        Symbol64 {
                            name_offset,
                            info,
                            other,
                            shndx,
                            value: value as u64,
                            size: size as u64,
                        }
                    }
                    ElfClass::Class64 => {
                        let Ok(name_offset) = read_u32_le(&raw, off) else {
                            break;
                        };
                        let info = raw[off + 4];
                        let other = raw[off + 5];
                        let Ok(shndx) = read_u16_le(&raw, off + 6) else {
                            break;
                        };
                        let Ok(value) = read_u64_le(&raw, off + 8) else {
                            break;
                        };
                        let Ok(size) = read_u64_le(&raw, off + 16) else {
                            break;
                        };
                        Symbol64 {
                            name_offset,
                            info,
                            other,
                            shndx,
                            value,
                            size,
                        }
                    }
                };

                let name = Self::read_null_str(strtab_data, sym.name_offset as usize);
                if !name.is_empty() {
                    if sec.r#type == SHT_DYNSYM {
                        dyn_symbols.insert(name, sym);
                    } else {
                        symbols.insert(name, sym);
                    }
                }
            }
        }

        Ok(BinaryFile {
            path,
            class,
            data,
            header,
            program_headers,
            min_load_vaddr,
            sections,
            section_names,
            symbols,
            dyn_symbols,
            raw_bytes: raw,
            sha256,
        })
    }

    fn section_slice<'a>(raw: &'a [u8], sec: &SectionHeader64) -> &'a [u8] {
        if sec.r#type == SHT_NOBITS {
            return &[];
        }
        let Ok(start) = usize::try_from(sec.offset) else {
            return &[];
        };
        if start >= raw.len() {
            return &[];
        }
        let end = sec.offset.saturating_add(sec.size).min(raw.len() as u64) as usize;
        raw.get(start..end).unwrap_or(&[])
    }

    fn read_section_names(sections: &[SectionHeader64], strtab: &[u8]) -> Vec<String> {
        sections
            .iter()
            .map(|sec| Self::read_null_str(strtab, sec.name_offset as usize))
            .collect()
    }

    fn read_null_str(buf: &[u8], offset: usize) -> String {
        if offset >= buf.len() {
            return String::new();
        }
        let mut end = offset;
        while end < buf.len() && buf[end] != 0 {
            end += 1;
        }
        String::from_utf8_lossy(&buf[offset..end]).to_string()
    }

    pub fn symbol_address(&self, name: &str) -> Option<u64> {
        self.symbols
            .get(name)
            .map(|s| s.value)
            .or_else(|| self.dyn_symbols.get(name).map(|s| s.value))
    }

    pub fn symbol_size(&self, name: &str) -> Option<u64> {
        self.symbols
            .get(name)
            .map(|s| s.size)
            .or_else(|| self.dyn_symbols.get(name).map(|s| s.size))
    }

    /// Resolve a byte pattern to an ELF virtual address (RVA for shared libraries/PIE).
    /// Scans the named section (or whole file if None), applies the optional offset,
    /// and converts the file offset to an ELF virtual address.
    pub fn resolve_pattern_address(
        &self,
        pattern: &[u8],
        mask: Option<&[u8]>,
        offset: i64,
        section: Option<&str>,
    ) -> Option<u64> {
        let (start, end) = match section {
            Some(name) => {
                let (_, sec) = self
                    .sections
                    .iter()
                    .enumerate()
                    .find(|(idx, _)| self.section_names.get(*idx).is_some_and(|n| n == name))?;
                if sec.r#type == SHT_NOBITS {
                    return None;
                }
                let s = usize::try_from(sec.offset).ok()?.min(self.raw_bytes.len());
                let e = sec
                    .offset
                    .saturating_add(sec.size)
                    .min(self.raw_bytes.len() as u64) as usize;
                (s, e)
            }
            None => (0, self.raw_bytes.len()),
        };
        let slice = self.raw_bytes.get(start..end)?;
        let file_off = self.scan_range(slice, pattern, mask)?;
        let abs_file_off = start.checked_add(file_off)?;
        self.file_offset_to_vaddr(abs_file_off)
            .and_then(|vaddr| vaddr.checked_add_signed(offset))
    }

    /// Translates an ELF virtual address from this binary to a live runtime virtual address
    /// using the module's mapping from `/proc/self/maps`, verifying that at least `min_exec_bytes`
    /// lie inside an executable (`r-x` / `rwx`) segment of the loaded module.
    pub fn vaddr_to_runtime_addr(
        &self,
        vaddr: u64,
        mapping: &LoadedModuleMapping,
        min_exec_bytes: usize,
    ) -> Option<u64> {
        let rel = vaddr.checked_sub(self.min_load_vaddr)?;
        let runtime_addr = mapping.base_addr.checked_add(rel)?;
        if mapping.contains_executable_range(runtime_addr, min_exec_bytes) {
            Some(runtime_addr)
        } else {
            None
        }
    }

    pub fn scan_section(
        &self,
        section_name: &str,
        pattern: &[u8],
        mask: Option<&[u8]>,
    ) -> Option<usize> {
        let sec = self
            .sections
            .iter()
            .enumerate()
            .find(|(idx, _)| {
                self.section_names
                    .get(*idx)
                    .is_some_and(|n| n == section_name)
            })
            .map(|(_, s)| s);
        let (start, end) = match sec {
            Some(s) => {
                if s.r#type == SHT_NOBITS {
                    return None;
                }
                (
                    usize::try_from(s.offset).ok()?.min(self.raw_bytes.len()),
                    s.offset
                        .saturating_add(s.size)
                        .min(self.raw_bytes.len() as u64) as usize,
                )
            }
            None => (0, self.raw_bytes.len()),
        };
        let slice = self.raw_bytes.get(start..end)?;
        self.scan_range(slice, pattern, mask)
            .and_then(|off| start.checked_add(off))
    }

    pub fn scan_range(
        &self,
        haystack: &[u8],
        pattern: &[u8],
        mask: Option<&[u8]>,
    ) -> Option<usize> {
        match mask {
            Some(m) => byte_search::find_with_mask(haystack, pattern, m),
            None => byte_search::find(haystack, pattern),
        }
    }

    pub fn file_offset_to_vaddr(&self, file_offset: usize) -> Option<u64> {
        let foff = file_offset as u64;
        for sec in &self.sections {
            if sec.r#type == SHT_NOBITS || (sec.flags & SHF_ALLOC) == 0 {
                continue;
            }
            let Some(sec_end) = sec.offset.checked_add(sec.size) else {
                continue;
            };
            if foff >= sec.offset && foff < sec_end {
                let delta = foff - sec.offset;
                return sec.addr.checked_add(delta);
            }
        }
        // Fallback to PT_LOAD segments for stripped ELFs lacking section headers
        for ph in &self.program_headers {
            if ph.r#type != PT_LOAD || ph.filesz == 0 {
                continue;
            }
            let Some(seg_end) = ph.offset.checked_add(ph.filesz) else {
                continue;
            };
            if foff >= ph.offset && foff < seg_end {
                let delta = foff - ph.offset;
                return ph.vaddr.checked_add(delta);
            }
        }
        None
    }

    pub fn sha256_hex(&self) -> &str {
        &self.sha256
    }
}

/// Parses `/proc/<pid>/maps` text and locates the live load base and executable ranges
/// for a module matching `module_filename` (exact basename match, e.g. `"steamclient.so"`).
pub fn parse_proc_maps_for_module(
    maps: &str,
    module_filename: &str,
) -> Option<LoadedModuleMapping> {
    let mut base_addr: Option<u64> = None;
    let mut exec_ranges = Vec::new();

    for line in maps.lines() {
        let mut parts = line.split_whitespace();
        let Some(range_str) = parts.next() else {
            continue;
        };
        let Some(perms) = parts.next() else {
            continue;
        };
        let Some(offset_str) = parts.next() else {
            continue;
        };
        // Skip dev and inode
        let _dev = parts.next();
        let _inode = parts.next();
        let path_part = parts.collect::<Vec<_>>().join(" ");
        let path_clean = path_part.trim_end_matches(" (deleted)").trim();
        if path_clean.is_empty() {
            continue;
        }

        let basename = Path::new(path_clean)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if basename != module_filename {
            continue;
        }

        let Some((start_hex, end_hex)) = range_str.split_once('-') else {
            continue;
        };
        let Ok(start) = u64::from_str_radix(start_hex, 16) else {
            continue;
        };
        let Ok(end) = u64::from_str_radix(end_hex, 16) else {
            continue;
        };
        let Ok(file_off) = u64::from_str_radix(offset_str, 16) else {
            continue;
        };

        if let Some(candidate_base) = start.checked_sub(file_off) {
            base_addr = Some(match base_addr {
                Some(cur) => cur.min(candidate_base),
                None => candidate_base,
            });
        }
        if perms.contains('r') && perms.contains('x') {
            exec_ranges.push((start, end));
        }
    }

    base_addr.map(|base| LoadedModuleMapping {
        base_addr: base,
        exec_ranges,
    })
}

/// Looks up a loaded shared library's ASLR base address and executable segments
/// from `/proc/self/maps`.
pub fn find_loaded_module_mapping(module_filename: &str) -> Option<LoadedModuleMapping> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    parse_proc_maps_for_module(&maps, module_filename)
}

/// Scans only the target Steam client binaries (`steamclient.so`, `steamui.so`, `steam`),
/// avoiding huge unrelated libraries such as `libcef.so` and deduplicating canonical paths.
pub fn scan_steam_binaries(steam_root: &Path) -> Vec<BinaryFile> {
    let mut out = Vec::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    let target_names = ["steamclient.so", "steamui.so", "steam"];
    let search_dirs = if cfg!(target_arch = "x86") {
        vec![
            steam_root.join("ubuntu12_32"),
            steam_root.join("linux32"),
            steam_root.join("linux64"),
            steam_root.join("ubuntu12_64"),
            steam_root.join("steamrt64"),
            steam_root.join("bin"),
            steam_root.to_path_buf(),
        ]
    } else {
        vec![
            steam_root.join("linux64"),
            steam_root.join("ubuntu12_64"),
            steam_root.join("steamrt64"),
            steam_root.join("ubuntu12_32"),
            steam_root.join("linux32"),
            steam_root.join("bin"),
            steam_root.to_path_buf(),
        ]
    };

    for dir in &search_dirs {
        for name in &target_names {
            let candidate = dir.join(name);
            if !candidate.is_file() {
                continue;
            }
            let canonical = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
            if !seen_paths.insert(canonical) {
                continue;
            }
            if let Ok(elf) = BinaryFile::open(&candidate) {
                out.push(elf);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_synthetic_elf64(
        text_bytes: &[u8],
        sym_entsize: u64,
        corrupt_sym_size: Option<u64>,
    ) -> Vec<u8> {
        let text_off: u64 = 0x1000;
        let text_vaddr: u64 = 0x1000;
        let shstrtab: &[u8] = b"\0.text\0.symtab\0.strtab\0.shstrtab\0";
        let strtab: &[u8] = b"\0test_func\0";

        let shstr_off: u64 = 0x1100;
        let strtab_off: u64 = 0x1140;
        let symtab_off: u64 = 0x1180;
        let shoff: u64 = 0x1200;
        let phoff: u64 = 0x40;

        let mut buf = vec![0u8; 0x1400];
        // ELF magic & 64-bit LSB
        buf[0..4].copy_from_slice(b"\x7fELF");
        buf[4] = 2; // ELFCLASS64
        buf[5] = 1; // ELFDATA2LSB
        buf[6] = 1; // EV_CURRENT
        buf[24..32].copy_from_slice(&text_vaddr.to_le_bytes()); // e_entry
        buf[32..40].copy_from_slice(&phoff.to_le_bytes()); // e_phoff
        buf[40..48].copy_from_slice(&shoff.to_le_bytes()); // e_shoff
        buf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        buf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        buf[58..60].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize
        buf[60..62].copy_from_slice(&5u16.to_le_bytes()); // e_shnum
        buf[62..64].copy_from_slice(&4u16.to_le_bytes()); // e_shstrndx

        // 1 PT_LOAD header at phoff (vaddr = 0x0, offset = 0x0)
        let ph = phoff as usize;
        buf[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        buf[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X
        buf[ph + 8..ph + 16].copy_from_slice(&0u64.to_le_bytes()); // p_offset
        buf[ph + 16..ph + 24].copy_from_slice(&0u64.to_le_bytes()); // p_vaddr

        // Section contents
        buf[text_off as usize..text_off as usize + text_bytes.len()].copy_from_slice(text_bytes);
        buf[shstr_off as usize..shstr_off as usize + shstrtab.len()].copy_from_slice(shstrtab);
        buf[strtab_off as usize..strtab_off as usize + strtab.len()].copy_from_slice(strtab);

        // One symbol at index 1 in .symtab (index 0 is STN_UNDEF)
        let s1 = (symtab_off + 24) as usize;
        buf[s1..s1 + 4].copy_from_slice(&1u32.to_le_bytes()); // st_name -> "test_func"
        buf[s1 + 4] = 0x12; // STB_GLOBAL | STT_FUNC
        buf[s1 + 6..s1 + 8].copy_from_slice(&1u16.to_le_bytes()); // shndx = .text
        buf[s1 + 8..s1 + 16].copy_from_slice(&text_vaddr.to_le_bytes()); // st_value
        buf[s1 + 16..s1 + 24].copy_from_slice(&(text_bytes.len() as u64).to_le_bytes()); // st_size

        let write_shdr = |buf: &mut [u8],
                          idx: usize,
                          name: u32,
                          ty: u32,
                          flags: u64,
                          addr: u64,
                          off: u64,
                          size: u64,
                          link: u32,
                          entsize: u64| {
            let o = shoff as usize + idx * 64;
            buf[o..o + 4].copy_from_slice(&name.to_le_bytes());
            buf[o + 4..o + 8].copy_from_slice(&ty.to_le_bytes());
            buf[o + 8..o + 16].copy_from_slice(&flags.to_le_bytes());
            buf[o + 16..o + 24].copy_from_slice(&addr.to_le_bytes());
            buf[o + 24..o + 32].copy_from_slice(&off.to_le_bytes());
            buf[o + 32..o + 40].copy_from_slice(&size.to_le_bytes());
            buf[o + 40..o + 44].copy_from_slice(&link.to_le_bytes());
            buf[o + 56..o + 64].copy_from_slice(&entsize.to_le_bytes());
        };

        // 0: SHT_NULL
        write_shdr(&mut buf, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        // 1: .text
        write_shdr(
            &mut buf,
            1,
            1,
            1, // SHT_PROGBITS
            SHF_ALLOC | 0x4,
            text_vaddr,
            text_off,
            text_bytes.len() as u64,
            0,
            0,
        );
        // 2: .symtab (link = 3 -> .strtab)
        write_shdr(
            &mut buf,
            2,
            7,
            SHT_SYMTAB,
            0,
            0,
            symtab_off,
            corrupt_sym_size.unwrap_or(48),
            3,
            sym_entsize,
        );
        // 3: .strtab
        write_shdr(
            &mut buf,
            3,
            15,
            3, // SHT_STRTAB
            0,
            0,
            strtab_off,
            strtab.len() as u64,
            0,
            0,
        );
        // 4: .shstrtab
        write_shdr(
            &mut buf,
            4,
            23,
            3, // SHT_STRTAB
            0,
            0,
            shstr_off,
            shstrtab.len() as u64,
            0,
            0,
        );

        buf
    }

    /// Constructs a valid minimal 32-bit `ELFCLASS32` shared object containing `.text`, `.rodata`,
    /// `.dynsym`, `.dynstr`, and `.shstrtab`, plus a 32-byte `PT_LOAD` program header.
    fn build_synthetic_elf32(text_bytes: &[u8], rodata_bytes: &[u8]) -> Vec<u8> {
        let load_base_vaddr: u32 = 0x0001_0000;
        let text_off: u32 = 0x1000;
        let text_vaddr: u32 = load_base_vaddr + text_off;
        let rodata_off: u32 = 0x1080;
        let rodata_vaddr: u32 = load_base_vaddr + rodata_off;
        let dynsym_off: u32 = 0x1100;
        let dynstr_off: u32 = 0x1140;
        let shstr_off: u32 = 0x1180;
        let shoff: u32 = 0x1200;
        let phoff: u32 = 52; // Immediately after 52-byte Elf32_Ehdr

        // Offsets in shstrtab:
        // 0: ""
        // 1: ".text"
        // 7: ".rodata"
        // 15: ".dynsym"
        // 23: ".dynstr"
        // 31: ".shstrtab"
        let shstrtab: &[u8] = b"\0.text\0.rodata\0.dynsym\0.dynstr\0.shstrtab\0";
        // Offsets in dynstr:
        // 0: ""
        // 1: "CreateInterface"
        // 17: "SteamUI_Main"
        let dynstr: &[u8] = b"\0CreateInterface\0SteamUI_Main\0";

        let mut buf = vec![0u8; 0x1400];
        // Elf32_Ehdr (52 bytes)
        buf[0..4].copy_from_slice(b"\x7fELF");
        buf[4] = 1; // ELFCLASS32
        buf[5] = 1; // ELFDATA2LSB
        buf[6] = 1; // EV_CURRENT
        buf[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        buf[18..20].copy_from_slice(&3u16.to_le_bytes()); // EM_386
        buf[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
        buf[24..28].copy_from_slice(&text_vaddr.to_le_bytes()); // e_entry @24
        buf[28..32].copy_from_slice(&phoff.to_le_bytes()); // e_phoff @28
        buf[32..36].copy_from_slice(&shoff.to_le_bytes()); // e_shoff @32
        buf[40..42].copy_from_slice(&52u16.to_le_bytes()); // e_ehsize @40
        buf[42..44].copy_from_slice(&32u16.to_le_bytes()); // e_phentsize @42
        buf[44..46].copy_from_slice(&1u16.to_le_bytes()); // e_phnum @44
        buf[46..48].copy_from_slice(&40u16.to_le_bytes()); // e_shentsize @46
        buf[48..50].copy_from_slice(&6u16.to_le_bytes()); // e_shnum @48
        buf[50..52].copy_from_slice(&5u16.to_le_bytes()); // e_shstrndx @50

        // Elf32_Phdr (32 bytes): p_type @0, p_offset @4, p_vaddr @8, p_paddr @12,
        //                        p_filesz @16, p_memsz @20, p_flags @24, p_align @28
        let ph = phoff as usize;
        buf[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        buf[ph + 4..ph + 8].copy_from_slice(&0u32.to_le_bytes()); // p_offset
        buf[ph + 8..ph + 12].copy_from_slice(&load_base_vaddr.to_le_bytes()); // p_vaddr
        buf[ph + 12..ph + 16].copy_from_slice(&load_base_vaddr.to_le_bytes()); // p_paddr
        buf[ph + 16..ph + 20].copy_from_slice(&0x1400u32.to_le_bytes()); // p_filesz
        buf[ph + 20..ph + 24].copy_from_slice(&0x1400u32.to_le_bytes()); // p_memsz
        buf[ph + 24..ph + 28].copy_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X
        buf[ph + 28..ph + 32].copy_from_slice(&0x1000u32.to_le_bytes()); // p_align

        // Populate section payloads
        buf[text_off as usize..text_off as usize + text_bytes.len()].copy_from_slice(text_bytes);
        buf[rodata_off as usize..rodata_off as usize + rodata_bytes.len()]
            .copy_from_slice(rodata_bytes);
        buf[dynstr_off as usize..dynstr_off as usize + dynstr.len()].copy_from_slice(dynstr);
        buf[shstr_off as usize..shstr_off as usize + shstrtab.len()].copy_from_slice(shstrtab);

        // Populate .dynsym (3 entries * 16 bytes = 48 bytes; entry 0 is STN_UNDEF)
        // Elf32_Sym: st_name @0, st_value @4, st_size @8, st_info @12, st_other @13, st_shndx @14
        let s1 = (dynsym_off + 16) as usize;
        buf[s1..s1 + 4].copy_from_slice(&1u32.to_le_bytes()); // "CreateInterface"
        buf[s1 + 4..s1 + 8].copy_from_slice(&text_vaddr.to_le_bytes());
        buf[s1 + 8..s1 + 12].copy_from_slice(&(text_bytes.len() as u32).to_le_bytes());
        buf[s1 + 12] = 0x12; // STB_GLOBAL | STT_FUNC
        buf[s1 + 14..s1 + 16].copy_from_slice(&1u16.to_le_bytes()); // .text

        let s2 = (dynsym_off + 32) as usize;
        buf[s2..s2 + 4].copy_from_slice(&17u32.to_le_bytes()); // "SteamUI_Main"
        buf[s2 + 4..s2 + 8].copy_from_slice(&(text_vaddr + 4).to_le_bytes());
        buf[s2 + 8..s2 + 12].copy_from_slice(&8u32.to_le_bytes());
        buf[s2 + 12] = 0x12;
        buf[s2 + 14..s2 + 16].copy_from_slice(&1u16.to_le_bytes());

        let write_shdr32 = |buf: &mut [u8],
                            idx: usize,
                            name: u32,
                            ty: u32,
                            flags: u32,
                            addr: u32,
                            off: u32,
                            size: u32,
                            link: u32,
                            entsize: u32| {
            let o = shoff as usize + idx * 40;
            buf[o..o + 4].copy_from_slice(&name.to_le_bytes());
            buf[o + 4..o + 8].copy_from_slice(&ty.to_le_bytes());
            buf[o + 8..o + 12].copy_from_slice(&flags.to_le_bytes());
            buf[o + 12..o + 16].copy_from_slice(&addr.to_le_bytes());
            buf[o + 16..o + 20].copy_from_slice(&off.to_le_bytes());
            buf[o + 20..o + 24].copy_from_slice(&size.to_le_bytes());
            buf[o + 24..o + 28].copy_from_slice(&link.to_le_bytes());
            buf[o + 36..o + 40].copy_from_slice(&entsize.to_le_bytes());
        };

        // 0: SHT_NULL
        write_shdr32(&mut buf, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        // 1: .text
        write_shdr32(
            &mut buf,
            1,
            1,
            1, // SHT_PROGBITS
            (SHF_ALLOC as u32) | 0x4,
            text_vaddr,
            text_off,
            text_bytes.len() as u32,
            0,
            0,
        );
        // 2: .rodata
        write_shdr32(
            &mut buf,
            2,
            7,
            1, // SHT_PROGBITS
            SHF_ALLOC as u32,
            rodata_vaddr,
            rodata_off,
            rodata_bytes.len() as u32,
            0,
            0,
        );
        // 3: .dynsym (link = 4 -> .dynstr, entsize = 16)
        write_shdr32(
            &mut buf,
            3,
            15,
            SHT_DYNSYM,
            SHF_ALLOC as u32,
            load_base_vaddr + dynsym_off,
            dynsym_off,
            48,
            4,
            16,
        );
        // 4: .dynstr
        write_shdr32(
            &mut buf,
            4,
            23,
            3, // SHT_STRTAB
            SHF_ALLOC as u32,
            load_base_vaddr + dynstr_off,
            dynstr_off,
            dynstr.len() as u32,
            0,
            0,
        );
        // 5: .shstrtab
        write_shdr32(
            &mut buf,
            5,
            31,
            3, // SHT_STRTAB
            0,
            0,
            shstr_off,
            shstrtab.len() as u32,
            0,
            0,
        );

        buf
    }

    #[test]
    fn sha256_is_deterministic() {
        let data = b"hello world";
        let h1 = format!("{:x}", Sha256::digest(data));
        let h2 = format!("{:x}", Sha256::digest(data));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn parses_elf64_headers_sections_symbols_and_patterns_accurately() {
        let code = [
            0x55, 0x48, 0x89, 0xE5, 0xB8, 0x2A, 0x00, 0x00, 0x00, 0x5D, 0xC3,
        ];
        let raw = build_synthetic_elf64(&code, 24, None);
        let bin = BinaryFile::from_bytes(PathBuf::from("steamclient.so"), raw).unwrap();

        assert_eq!(bin.header.shnum, 5);
        assert_eq!(bin.header.shstrndx, 4);
        assert_eq!(bin.section_names.get(1).map(String::as_str), Some(".text"));
        assert_eq!(bin.symbol_address("test_func"), Some(0x1000));
        assert_eq!(bin.symbol_size("test_func"), Some(code.len() as u64));

        // Pattern match with wildcard mask resolves virtual address inside .text
        let pat = [0x55, 0x48, 0x00, 0xE5];
        let mask = [0xFF, 0xFF, 0x00, 0xFF];
        assert_eq!(
            bin.resolve_pattern_address(&pat, Some(&mask), 0, Some(".text")),
            Some(0x1000)
        );

        // Short mask must return None instead of panicking
        assert_eq!(bin.scan_range(&code, &pat, Some(&[0xFF, 0xFF])), None);
    }

    #[test]
    fn parses_elf32_headers_sections_rodata_dynsym_rva_and_patterns() {
        // IA-32 function prologue + body (`push ebp; mov ebp, esp; sub esp, 0x18; mov eax, 42; leave; ret`)
        let code32 = [
            0x55, 0x89, 0xE5, 0x83, 0xEC, 0x18, 0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC9, 0xC3,
        ];
        let rodata32 = b"SteamClient021\0ubuntu12_32\0";
        let raw = build_synthetic_elf32(&code32, rodata32);

        let bin = BinaryImage::from_bytes(PathBuf::from("ubuntu12_32/steamclient.so"), raw).unwrap();
        assert_eq!(bin.class, ElfClass::Class32);
        assert_eq!(bin.data, ElfData::Lsb);
        assert_eq!(bin.header.entry, 0x0001_1000);
        assert_eq!(bin.header.phnum, 1);
        assert_eq!(bin.header.shnum, 6);
        assert_eq!(bin.min_load_vaddr, 0x0001_0000);

        // Verify sections (.text, .rodata, .dynsym)
        assert_eq!(bin.section_names.get(1).map(String::as_str), Some(".text"));
        assert_eq!(
            bin.section_names.get(2).map(String::as_str),
            Some(".rodata")
        );
        assert_eq!(
            bin.section_names.get(3).map(String::as_str),
            Some(".dynsym")
        );

        // Verify file offset -> ELF vaddr & ASLR runtime RVA translation
        assert_eq!(bin.file_offset_to_vaddr(0x1000), Some(0x0001_1000));
        assert_eq!(bin.file_offset_to_vaddr(0x1080), Some(0x0001_1080));

        let mapping32 = LoadedModuleMapping {
            base_addr: 0xf710_0000,
            exec_ranges: vec![(0xf710_1000, 0xf715_0000)],
        };
        assert_eq!(
            bin.vaddr_to_runtime_addr(0x0001_1000, &mapping32, 6),
            Some(0xf710_1000)
        );

        // Verify pattern lookup in .text (with wildcard mask) and .rodata
        let pat = [0x55, 0x89, 0xE5, 0x83, 0x00, 0x18, 0xB8];
        let mask = [0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xFF, 0xFF];
        assert_eq!(
            bin.resolve_pattern_address(&pat, Some(&mask), 0, Some(".text")),
            Some(0x0001_1000)
        );
        assert_eq!(
            bin.scan_section(".rodata", b"SteamClient021", None),
            Some(0x1080)
        );
        assert_eq!(
            bin.resolve_pattern_address(b"ubuntu12_32", None, 0, Some(".rodata")),
            Some(0x0001_1080 + 15)
        );

        // Verify .dynsym Elf32_Sym lookup
        assert_eq!(bin.symbol_address("CreateInterface"), Some(0x0001_1000));
        assert_eq!(
            bin.symbol_size("CreateInterface"),
            Some(code32.len() as u64)
        );
        assert_eq!(bin.symbol_address("SteamUI_Main"), Some(0x0001_1004));
        assert_eq!(bin.symbol_size("SteamUI_Main"), Some(8));

        // Truncated ELF32 (< 52 bytes) must be rejected cleanly
        let mut trunc32 = vec![0u8; 36];
        trunc32[0..4].copy_from_slice(b"\x7fELF");
        trunc32[4] = 1;
        trunc32[5] = 1;
        assert!(BinaryImage::from_bytes(PathBuf::from("trunc32.so"), trunc32).is_err());
    }

    #[test]
    fn rejects_truncated_elf64_and_survives_zero_entsize_symbol_table() {
        let mut trunc = vec![0u8; 32];
        trunc[0..4].copy_from_slice(b"\x7fELF");
        trunc[4] = 2;
        trunc[5] = 1;
        assert!(BinaryFile::from_bytes(PathBuf::from("trunc.so"), trunc).is_err());

        // Malformed symtab with entsize == 0 and huge size must NOT hang or panic
        let raw = build_synthetic_elf64(&[0x90; 16], 0, Some(u64::MAX / 2));
        let bin = BinaryFile::from_bytes(PathBuf::from("malformed.so"), raw).unwrap();
        assert!(bin.symbols.is_empty());
    }

    #[test]
    fn parses_real_current_executable_without_error() {
        let exe = std::env::current_exe().unwrap();
        let bin = BinaryFile::open(&exe).unwrap();
        assert_eq!(bin.class, ElfClass::Class64);
        assert!(!bin.sections.is_empty());
        assert!(bin.section_names.iter().any(|n| n == ".text"));
    }

    #[test]
    fn parses_proc_maps_and_validates_runtime_address() {
        let maps = "\
7f1d00000000-7f1d00010000 r--p 00000000 08:01 1234 /home/user/.local/share/Steam/linux64/steamclient.so
7f1d00010000-7f1d00200000 r-xp 00010000 08:01 1234 /home/user/.local/share/Steam/linux64/steamclient.so
7f1d00200000-7f1d00210000 r--p 00200000 08:01 1234 /home/user/.local/share/Steam/linux64/steamclient.so
7f1d90000000-7f1d90010000 r-xp 00000000 08:01 5678 /home/user/.local/share/Steam/linux64/steamclient.so.bak
";
        let mapping = parse_proc_maps_for_module(maps, "steamclient.so").unwrap();
        assert_eq!(mapping.base_addr, 0x7f1d_0000_0000);
        assert_eq!(
            mapping.exec_ranges,
            vec![(0x7f1d_0001_0000, 0x7f1d_0020_0000)]
        );

        let raw = build_synthetic_elf64(&[0x90; 32], 24, None);
        let bin = BinaryFile::from_bytes(PathBuf::from("steamclient.so"), raw).unwrap();
        // Inside r-xp range -> Ok
        assert_eq!(
            bin.vaddr_to_runtime_addr(0x10000, &mapping, 14),
            Some(0x7f1d_0001_0000)
        );
        // Inside r--p data range -> rejected!
        assert_eq!(bin.vaddr_to_runtime_addr(0x1000, &mapping, 14), None);
        // Crossing end of r-xp range -> rejected!
        assert_eq!(bin.vaddr_to_runtime_addr(0x1ffff8, &mapping, 14), None);
    }
}
