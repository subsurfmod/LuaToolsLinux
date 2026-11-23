//! ELF binary reading and pattern scanning for Steam Linux binaries.
//!
//! Reads ELF headers, section headers, symbol tables, and dynamic symbols from
//! Steam Linux binaries (`steamclient.so`, `steamui.so`, `steam`) so that
//! integration points can be located and Steam build compatibility verified
//! via SHA-256 pattern matching.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

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

#[derive(Debug, Clone)]
pub struct BinaryFile {
    pub path: std::path::PathBuf,
    pub class: ElfClass,
    pub data: ElfData,
    pub header: BinaryHeader64,
    pub sections: Vec<SectionHeader64>,
    pub section_names: Vec<String>,
    pub symbols: BTreeMap<String, Symbol64>,
    pub dyn_symbols: BTreeMap<String, Symbol64>,
    pub raw_bytes: Vec<u8>,
    pub sha256: String,
}

impl BinaryFile {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, BinaryError> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)?;

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

        let entry = u64::from_le_bytes(raw[24..32].try_into().unwrap());
        let phoff = u64::from_le_bytes(raw[32..40].try_into().unwrap());
        let shoff = u64::from_le_bytes(raw[40..48].try_into().unwrap());
        let phnum = u16::from_le_bytes(raw[56..58].try_into().unwrap());
        let shnum = u16::from_le_bytes(raw[58..60].try_into().unwrap());
        let shstrndx = u16::from_le_bytes(raw[60..62].try_into().unwrap());

        let header = BinaryHeader64 {
            entry,
            phoff,
            shoff,
            phnum,
            shnum,
            shstrndx,
        };

        let shentsize = u16::from_le_bytes(raw[62..64].try_into().unwrap()) as usize;
        let mut sections = Vec::with_capacity(shnum as usize);
        for i in 0..shnum as usize {
            let off = shoff as usize + i * shentsize;
            if off + 64 > raw.len() {
                return Err(BinaryError::Parse("section header truncated".into()));
            }
            sections.push(SectionHeader64 {
                name_offset: u32::from_le_bytes(raw[off..off + 4].try_into().unwrap()),
                r#type: u32::from_le_bytes(raw[off + 4..off + 8].try_into().unwrap()),
                flags: u64::from_le_bytes(raw[off + 8..off + 16].try_into().unwrap()),
                addr: u64::from_le_bytes(raw[off + 16..off + 24].try_into().unwrap()),
                offset: u64::from_le_bytes(raw[off + 24..off + 32].try_into().unwrap()),
                size: u64::from_le_bytes(raw[off + 32..off + 40].try_into().unwrap()),
                link: u32::from_le_bytes(raw[off + 40..off + 44].try_into().unwrap()),
                info: u32::from_le_bytes(raw[off + 44..off + 48].try_into().unwrap()),
                align: u64::from_le_bytes(raw[off + 48..off + 56].try_into().unwrap()),
                entsize: u64::from_le_bytes(raw[off + 56..off + 64].try_into().unwrap()),
            });
        }

        let section_names = if (header.shstrndx as usize) < sections.len() {
            let strtab_sec = &sections[header.shstrndx as usize];
            let strtab_data =
                &raw[strtab_sec.offset as usize..(strtab_sec.offset + strtab_sec.size) as usize];
            Self::read_section_names(&sections, strtab_data, &raw)
        } else {
            Vec::new()
        };

        let mut symbols = BTreeMap::new();
        let mut dyn_symbols = BTreeMap::new();

        for sec in sections.iter() {
            if sec.r#type == 2 || sec.r#type == 11 {
                let strtab_link = sec.link as usize;
                let strtab_data = if strtab_link < sections.len() {
                    let st = &sections[strtab_link];
                    &raw[st.offset as usize..(st.offset + st.size) as usize]
                } else {
                    &[]
                };

                let sym_count = sec.size / sec.entsize.max(1);
                let entry_size = sec.entsize as usize;
                for j in 0..sym_count as usize {
                    let off = sec.offset as usize + j * entry_size;
                    if off + 24 > raw.len() {
                        break;
                    }
                    let name_offset = u32::from_le_bytes(raw[off..off + 4].try_into().unwrap());
                    let info = raw[off + 4];
                    let _other = raw[off + 5];
                    let shndx = u16::from_le_bytes(raw[off + 6..off + 8].try_into().unwrap());
                    let value = u64::from_le_bytes(raw[off + 8..off + 16].try_into().unwrap());
                    let size = u64::from_le_bytes(raw[off + 16..off + 24].try_into().unwrap());

                    let name = Self::read_null_str(strtab_data, name_offset as usize);
                    if !name.is_empty() {
                        let sym = Symbol64 {
                            name_offset,
                            info,
                            other: _other,
                            shndx,
                            value,
                            size,
                        };
                        if sec.r#type == 11 {
                            dyn_symbols.insert(name.clone(), sym);
                        } else {
                            symbols.insert(name.clone(), sym);
                        }
                    }
                }
            }
        }

        Ok(BinaryFile {
            path,
            class,
            data,
            header,
            sections,
            section_names,
            symbols,
            dyn_symbols,
            raw_bytes: raw,
            sha256,
        })
    }

    fn read_section_names(
        sections: &[SectionHeader64],
        strtab: &[u8],
        _raw: &[u8],
    ) -> Vec<String> {
        let mut names = Vec::new();
        for sec in sections {
            let name = Self::read_null_str(strtab, sec.name_offset as usize);
            names.push(name);
        }
        names
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

    /// Resolve a byte pattern to a virtual address.
    /// Scans the named section (or whole file if None), applies the optional offset,
    /// and converts the file offset to a virtual address.
    pub fn resolve_pattern_address(
        &self,
        pattern: &[u8],
        mask: Option<&[u8]>,
        offset: i64,
        section: Option<&str>,
    ) -> Option<u64> {
        let (start, end) = match section {
            Some(name) => {
                let sec = self.sections.iter().find(|s| {
                    self.section_names
                        .get(s.name_offset as usize)
                        .map_or(false, |n| n == name)
                })?;
                (sec.offset as usize, (sec.offset + sec.size) as usize)
            }
            None => (0, self.raw_bytes.len()),
        };
        let file_off = self.scan_range(&self.raw_bytes[start..end], pattern, mask)?;
        let abs_file_off = start + file_off;
        self.file_offset_to_vaddr(abs_file_off)
            .map(|vaddr| vaddr.wrapping_add(offset as u64))
    }

    pub fn scan_section(
        &self,
        section_name: &str,
        pattern: &[u8],
        mask: Option<&[u8]>,
    ) -> Option<usize> {
        let sec = self.sections.iter().find(|s| {
            self.section_names
                .get(s.name_offset as usize)
                .map_or(false, |n| n == section_name)
        });
        let (start, end) = match sec {
            Some(s) => (s.offset as usize, (s.offset + s.size) as usize),
            None => (0, self.raw_bytes.len()),
        };
        self.scan_range(&self.raw_bytes[start..end], pattern, mask)
            .map(|off| start + off)
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
        let default_mask = vec![0xFFu8; pattern.len()];
        let mask = mask.unwrap_or(&default_mask);
        for i in 0..=haystack.len().saturating_sub(pattern.len()) {
            let mut matched = true;
            for j in 0..pattern.len() {
                if (haystack[i + j] & mask[j]) != (pattern[j] & mask[j]) {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Some(i);
            }
        }
        None
    }

    pub fn file_offset_to_vaddr(&self, file_offset: usize) -> Option<u64> {
        for sec in &self.sections {
            if (file_offset as u64) >= sec.offset
                && (file_offset as u64) < sec.offset + sec.size
            {
                return Some(sec.addr + (file_offset as u64 - sec.offset));
            }
        }
        None
    }

    pub fn sha256_hex(&self) -> &str {
        &self.sha256
    }
}

pub fn scan_steam_binaries(steam_root: &Path) -> Vec<BinaryFile> {
    let mut out = Vec::new();
    let candidates = [
        "steamclient.so",
        "steamui.so",
        "steam",
        "steam.sh",
    ];
    let search_dirs = [
        steam_root.to_path_buf(),
        steam_root.join("bin"),
        steam_root.join("ubuntu12_32"),
        steam_root.join("ubuntu12_64"),
        steam_root.join("linux64"),
    ];
    for dir in &search_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if candidates.contains(&name) || name.ends_with(".so") {
                        if let Ok(elf) = BinaryFile::open(&p) {
                            out.push(elf);
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_deterministic() {
        let data = b"hello world";
        let h1 = format!("{:x}", Sha256::digest(data));
        let h2 = format!("{:x}", Sha256::digest(data));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }
}
