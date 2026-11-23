//! ELF binary reading and pattern scanning for Steam Linux binaries.
//!
//! Reads ELF headers, program headers, section headers, symbol tables, and dynamic
//! symbols from Steam Linux binaries (`steamclient.so`, `steamui.so`, `steam`) so that
//! integration points can be located, ASLR runtime addresses resolved via `/proc/self/maps`,
//! and Steam build compatibility verified via SHA-256 pattern matching.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

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
    pub min_load_vaddr: u64,
    pub sections: Vec<SectionHeader64>,
    pub section_names: Vec<String>,
    pub symbols: BTreeMap<String, Symbol64>,
    pub dyn_symbols: BTreeMap<String, Symbol64>,
    pub raw_bytes: Vec<u8>,
    pub sha256: String,
}

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
        let sha256 = format!("{:x}", Sha256::digest(&raw));

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

        if class != ElfClass::Class64 {
            return Err(BinaryError::UnsupportedClass(raw[4]));
        }
        if data != ElfData::Lsb {
            return Err(BinaryError::UnsupportedData(raw[5]));
        }
        if raw.len() < ELF64_HEADER_LEN {
            return Err(BinaryError::Parse("ELF64 header truncated".into()));
        }

        // Standard Elf64_Ehdr byte layout (little-endian):
        //   24..32: e_entry
        //   32..40: e_phoff
        //   40..48: e_shoff
        //   54..56: e_phentsize
        //   56..58: e_phnum
        //   58..60: e_shentsize
        //   60..62: e_shnum
        //   62..64: e_shstrndx
        let entry = read_u64_le(&raw, 24)?;
        let phoff = read_u64_le(&raw, 32)?;
        let shoff = read_u64_le(&raw, 40)?;
        let phentsize = read_u16_le(&raw, 54)? as usize;
        let phnum = read_u16_le(&raw, 56)?;
        let shentsize = read_u16_le(&raw, 58)? as usize;
        let shnum = read_u16_le(&raw, 60)?;
        let shstrndx = read_u16_le(&raw, 62)?;

        let header = BinaryHeader64 {
            entry,
            phoff,
            shoff,
            phnum,
            shnum,
            shstrndx,
        };

        // Determine the lowest PT_LOAD p_vaddr so relative virtual addresses translate cleanly
        // to live ASLR runtime addresses (0 for PIE/DSOs such as steamclient.so).
        let mut min_load_vaddr: Option<u64> = None;
        if phnum > 0 && phentsize >= ELF64_PHDR_MIN_LEN {
            if let Ok(phoff_usize) = usize::try_from(phoff) {
                for i in 0..(phnum as usize) {
                    let Some(delta) = i.checked_mul(phentsize) else {
                        break;
                    };
                    let Some(off) = phoff_usize.checked_add(delta) else {
                        break;
                    };
                    let Some(end) = off.checked_add(ELF64_PHDR_MIN_LEN) else {
                        break;
                    };
                    if end > raw.len() {
                        break;
                    }
                    let p_type = read_u32_le(&raw, off)?;
                    if p_type == PT_LOAD {
                        let p_vaddr = read_u64_le(&raw, off + 16)?;
                        min_load_vaddr = Some(match min_load_vaddr {
                            Some(cur) => cur.min(p_vaddr),
                            None => p_vaddr,
                        });
                    }
                }
            }
        }
        let min_load_vaddr = min_load_vaddr.unwrap_or(0);

        let mut sections = Vec::new();
        if shnum > 0 {
            if shentsize < ELF64_SHDR_MIN_LEN {
                return Err(BinaryError::Parse(format!(
                    "invalid ELF64 section header entry size: {}",
                    shentsize
                )));
            }
            let shoff_usize = usize::try_from(shoff)
                .map_err(|_| BinaryError::Parse("section header table offset overflow".into()))?;
            let table_bytes = (shnum as usize)
                .checked_mul(shentsize)
                .ok_or_else(|| BinaryError::Parse("section header table size overflow".into()))?;
            let table_end = shoff_usize
                .checked_add(table_bytes)
                .ok_or_else(|| BinaryError::Parse("section header table end overflow".into()))?;
            if table_end > raw.len() {
                return Err(BinaryError::Parse("section header truncated".into()));
            }

            sections.reserve(shnum as usize);
            for i in 0..(shnum as usize) {
                let off = shoff_usize + i * shentsize;
                sections.push(SectionHeader64 {
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
                });
            }
        }

        let section_names = if let Some(strtab_sec) = sections.get(header.shstrndx as usize) {
            let strtab_data = Self::section_slice(&raw, strtab_sec);
            Self::read_section_names(&sections, strtab_data)
        } else {
            Vec::new()
        };

        let mut symbols = BTreeMap::new();
        let mut dyn_symbols = BTreeMap::new();

        for sec in &sections {
            if sec.r#type != SHT_SYMTAB && sec.r#type != SHT_DYNSYM {
                continue;
            }
            // Guard against malformed or zero entsize (which previously caused unbounded loops).
            if sec.entsize < ELF64_SYM_MIN_LEN as u64 {
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
                let Some(end) = off.checked_add(ELF64_SYM_MIN_LEN) else {
                    break;
                };
                if end > raw.len() {
                    break;
                }

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

                let name = Self::read_null_str(strtab_data, name_offset as usize);
                if !name.is_empty() {
                    let sym = Symbol64 {
                        name_offset,
                        info,
                        other,
                        shndx,
                        value,
                        size,
                    };
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
        if pattern.is_empty() || haystack.len() < pattern.len() {
            return None;
        }
        if let Some(m) = mask {
            if m.len() < pattern.len() {
                return None;
            }
            for i in 0..=haystack.len() - pattern.len() {
                let mut matched = true;
                for j in 0..pattern.len() {
                    if (haystack[i + j] & m[j]) != (pattern[j] & m[j]) {
                        matched = false;
                        break;
                    }
                }
                if matched {
                    return Some(i);
                }
            }
            return None;
        }

        haystack
            .windows(pattern.len())
            .position(|window| window == pattern)
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
    let search_dirs = [
        steam_root.join("linux64"),
        steam_root.join("ubuntu12_64"),
        steam_root.join("steamrt64"),
        steam_root.join("ubuntu12_32"),
        steam_root.join("bin"),
        steam_root.to_path_buf(),
    ];

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
