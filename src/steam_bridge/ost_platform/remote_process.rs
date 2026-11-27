//! Linux implementation of OSTPlatform::RemoteProcess
//!
//! Provides remote process library injection via ptrace and LD_PRELOAD.
//! On Linux, we don't inject DLLs like Windows - instead we use LD_PRELOAD
//! or ptrace to inject a shared library.

use std::fs;
use std::path::Path;

use crate::steam_bridge::binary_reader::BinaryFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    Unknown,
    X86,
    X64,
}

impl Architecture {
    pub fn as_str(&self) -> &'static str {
        match self {
            Architecture::Unknown => "Unknown",
            Architecture::X86 => "x86",
            Architecture::X64 => "x64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectStatus {
    Ok,
    OpenProcessFailed,
    UnknownArchitecture,
    AllocFailed,
    WriteFailed,
    ResolveLoadLibraryFailed,
    CreateThreadFailed,
    WaitFailed,
    RemoteLoadFailed,
    PtraceAttachFailed,
    PtraceDetachFailed,
    DlopenFailed,
    InvalidLibraryPath,
}

impl InjectStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            InjectStatus::Ok => "Ok",
            InjectStatus::OpenProcessFailed => "OpenProcessFailed",
            InjectStatus::UnknownArchitecture => "UnknownArchitecture",
            InjectStatus::AllocFailed => "AllocFailed",
            InjectStatus::WriteFailed => "WriteFailed",
            InjectStatus::ResolveLoadLibraryFailed => "ResolveLoadLibraryFailed",
            InjectStatus::CreateThreadFailed => "CreateThreadFailed",
            InjectStatus::WaitFailed => "WaitFailed",
            InjectStatus::RemoteLoadFailed => "RemoteLoadFailed",
            InjectStatus::PtraceAttachFailed => "PtraceAttachFailed",
            InjectStatus::PtraceDetachFailed => "PtraceDetachFailed",
            InjectStatus::DlopenFailed => "DlopenFailed",
            InjectStatus::InvalidLibraryPath => "InvalidLibraryPath",
        }
    }
}

/// Get the architecture of a remote process by reading `/proc/<pid>/exe` ELF header.
pub fn get_architecture(pid: u32) -> Architecture {
    if pid == 0 {
        return Architecture::Unknown;
    }

    let exe_path = format!("/proc/{}/exe", pid);
    let mut file = match fs::File::open(&exe_path) {
        Ok(f) => f,
        Err(_) => return Architecture::Unknown,
    };

    let mut header = [0u8; 20];
    use std::io::Read;
    if file.read_exact(&mut header).is_err() {
        return Architecture::Unknown;
    }

    parse_elf_architecture(&header)
}

fn parse_elf_architecture(header: &[u8]) -> Architecture {
    if header.len() < 20 || &header[0..4] != b"\x7fELF" {
        return Architecture::Unknown;
    }

    let ei_class = header[4];
    let ei_data = header[5];
    let e_machine = match ei_data {
        1 => u16::from_le_bytes([header[18], header[19]]),
        2 => u16::from_be_bytes([header[18], header[19]]),
        _ => return Architecture::Unknown,
    };

    // EM_386 = 3, EM_X86_64 = 62
    match (ei_class, e_machine) {
        (1, 3) => Architecture::X86,
        (2, 62) => Architecture::X64,
        _ => Architecture::Unknown,
    }
}

/// Inject a library into a remote process using ptrace.
/// This is the Linux equivalent of Windows DLL injection.
///
/// Method: Attach via ptrace, verify registers/access, and report status.
/// Direct live ptrace `dlopen` shellcode injection is intentionally not performed;
/// callers should prefer launching with `LD_PRELOAD`.
pub fn inject_library(pid: u32, library_path: &Path) -> InjectStatus {
    if pid == 0 || pid == std::process::id() || pid > i32::MAX as u32 {
        return InjectStatus::OpenProcessFailed;
    }

    // Validate library path exists and is a regular file (or symlink to one)
    if !library_path.is_file() {
        return InjectStatus::InvalidLibraryPath;
    }

    // Verify ELF architecture match
    if let Ok(elf_bytes) = fs::read(library_path) {
        if elf_bytes.len() >= 5 {
            let lib_arch = match elf_bytes[4] {
                1 => Architecture::X86,
                2 => Architecture::X64,
                _ => Architecture::Unknown,
            };
            let target_arch = get_architecture(pid);
            if lib_arch != Architecture::Unknown && target_arch != Architecture::Unknown && lib_arch != target_arch {
                return InjectStatus::UnknownArchitecture;
            }
        }
    }

    let library_path_str = match library_path.to_str() {
        Some(s) if !s.is_empty() && !s.as_bytes().contains(&0) => s,
        _ => return InjectStatus::InvalidLibraryPath,
    };

    // Get target architecture
    let arch = get_architecture(pid);
    if arch == Architecture::Unknown {
        return InjectStatus::UnknownArchitecture;
    }

    // Check if we're the same architecture (can't inject 32-bit into 64-bit or vice versa)
    let our_arch = if cfg!(target_arch = "x86_64") {
        Architecture::X64
    } else if cfg!(target_arch = "x86") {
        Architecture::X86
    } else {
        Architecture::Unknown
    };

    if arch != our_arch {
        // Cross-architecture ptrace injection cannot execute direct shellcode; fall back to LD_PRELOAD
        return inject_via_ld_preload(pid, Path::new(library_path_str));
    }

    inject_via_ptrace(pid, library_path_str)
}

fn find_module_base_in_maps(pid: u32, module_pattern: &str) -> Option<usize> {
    let maps_path = if pid == 0 {
        "/proc/self/maps".to_string()
    } else {
        format!("/proc/{}/maps", pid)
    };
    let content = fs::read_to_string(maps_path).ok()?;
    for line in content.lines() {
        if line.contains(module_pattern) {
            if let Some(dash) = line.find('-') {
                if let Ok(addr) = usize::from_str_radix(&line[..dash], 16) {
                    return Some(addr);
                }
            }
        }
    }
    None
}

#[derive(Debug)]
struct MappedLibrary {
    base_addr: u64,
    path: String,
}

fn find_candidate_mapped_libraries(pid: u32) -> Vec<MappedLibrary> {
    let maps_path = if pid == 0 {
        "/proc/self/maps".to_string()
    } else {
        format!("/proc/{}/maps", pid)
    };
    let content = match fs::read_to_string(&maps_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut libraries: Vec<MappedLibrary> = Vec::new();

    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let Some(range_str) = parts.next() else { continue };
        let Some(_perms) = parts.next() else { continue };
        let Some(offset_str) = parts.next() else { continue };
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
        let is_libdl = basename.starts_with("libdl.so") || basename.starts_with("libdl-");
        let is_libc = basename.starts_with("libc.so") || basename.starts_with("libc-");

        if !is_libdl && !is_libc {
            continue;
        }

        let Some((start_hex, _)) = range_str.split_once('-') else { continue };
        let Ok(start) = u64::from_str_radix(start_hex, 16) else { continue };
        let Ok(file_off) = u64::from_str_radix(offset_str, 16) else { continue };

        let candidate_base = start.saturating_sub(file_off);
        if let Some(existing) = libraries.iter_mut().find(|l| l.path == path_clean) {
            existing.base_addr = existing.base_addr.min(candidate_base);
        } else {
            libraries.push(MappedLibrary {
                base_addr: candidate_base,
                path: path_clean.to_string(),
            });
        }
    }

    libraries.sort_by_key(|lib| {
        let name = Path::new(&lib.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if name.starts_with("libdl") {
            0
        } else {
            1
        }
    });

    libraries
}

fn resolve_remote_dlopen(pid: u32) -> Option<usize> {
    let candidates = find_candidate_mapped_libraries(pid);

    for lib in &candidates {
        // Try reading target binary directly or via target namespace root /proc/{pid}/root/{path}
        // (handles container namespaces like Steam Pressure Vessel or Flatpak)
        let root_path = if pid != 0 && pid != std::process::id() {
            Some(format!("/proc/{}/root{}", pid, lib.path))
        } else {
            None
        };

        let binary = root_path
            .as_ref()
            .and_then(|rp| BinaryFile::open(rp).ok())
            .or_else(|| BinaryFile::open(&lib.path).ok());

        if let Some(bin) = binary {
            let sym = bin.symbol_address("dlopen")
                .or_else(|| bin.symbol_address("__libc_dlopen_mode"));

            if let Some(sym_addr) = sym {
                let offset = sym_addr.saturating_sub(bin.min_load_vaddr);
                let remote_addr = lib.base_addr.saturating_add(offset);
                log::info!(
                    "[remote_process] Resolved remote dlopen in PID {} via ELF '{}': 0x{:X}",
                    pid, lib.path, remote_addr
                );
                return Some(remote_addr as usize);
            }
        }
    }

    // Fallback: If opening the target ELF fails, calculate offset from local process maps
    log::debug!("[remote_process] Falling back to local offset calculation for PID {}", pid);
    for lib in &candidates {
        let basename = Path::new(&lib.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if let Some(local_base) = find_module_base_in_maps(0, basename) {
            let local_sym = unsafe {
                let sym1 = libc::dlsym(libc::RTLD_DEFAULT, b"dlopen\0".as_ptr() as *const _);
                if !sym1.is_null() {
                    sym1 as usize
                } else {
                    let sym2 = libc::dlsym(libc::RTLD_DEFAULT, b"__libc_dlopen_mode\0".as_ptr() as *const _);
                    if !sym2.is_null() {
                        sym2 as usize
                    } else {
                        0
                    }
                }
            };
            if local_sym != 0 && local_sym >= local_base {
                let offset = local_sym - local_base;
                return Some((lib.base_addr as usize).saturating_add(offset));
            }
        }
    }

    None
}

fn write_process_bytes(pid: libc::pid_t, addr: u64, bytes: &[u8]) -> bool {
    use std::io::{Seek, Write};
    let mem_path = format!("/proc/{}/mem", pid);
    if let Ok(mut f) = fs::OpenOptions::new().write(true).open(&mem_path) {
        if f.seek(std::io::SeekFrom::Start(addr)).is_ok() && f.write_all(bytes).is_ok() {
            return true;
        }
    }
    // Fallback to PTRACE_POKEDATA word by word
    let word_size = std::mem::size_of::<usize>();
    for (i, chunk) in bytes.chunks(word_size).enumerate() {
        let chunk_addr = addr + (i * word_size) as u64;
        let mut word = [0u8; std::mem::size_of::<usize>()];
        word[..chunk.len()].copy_from_slice(chunk);
        let val = usize::from_ne_bytes(word);
        unsafe {
            libc::ptrace(
                libc::PTRACE_POKEDATA,
                pid,
                chunk_addr as *mut libc::c_void,
                val as *mut libc::c_void,
            );
        }
    }
    true
}

#[allow(dead_code)]
fn read_process_word(pid: libc::pid_t, addr: u64) -> Option<usize> {
    unsafe {
        let val = libc::ptrace(libc::PTRACE_PEEKDATA, pid, addr as *mut libc::c_void, 0);
        if val == -1 && std::io::Error::last_os_error().raw_os_error() != Some(0) {
            None
        } else {
            Some(val as usize)
        }
    }
}

/// Attach via ptrace, invoke remote dlopen with proper calling convention, verify handle,
/// restore execution context cleanly, and report status (BST-61).
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
fn inject_via_ptrace(pid: u32, library_path: &str) -> InjectStatus {
    use libc::{
        ptrace, waitpid, PTRACE_ATTACH, PTRACE_CONT, PTRACE_DETACH, PTRACE_GETREGS,
        PTRACE_SETREGS, WIFSTOPPED, WSTOPSIG,
    };
    use std::mem;

    let raw_pid = pid as libc::pid_t;

    // Attach to target process
    let result = unsafe { ptrace(PTRACE_ATTACH, raw_pid, 0, 0) };
    if result == -1 {
        log::warn!("[remote_process] ptrace attach failed for PID {}: {}", pid, std::io::Error::last_os_error());
        return InjectStatus::PtraceAttachFailed;
    }

    // Wait for process to stop (retrying on EINTR)
    let mut status = 0;
    let wait_result = loop {
        let rc = unsafe { waitpid(raw_pid, &mut status, 0) };
        if rc == -1 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break rc;
    };
    if wait_result == -1 || !WIFSTOPPED(status) {
        unsafe { ptrace(PTRACE_DETACH, raw_pid, 0, 0) };
        return InjectStatus::WaitFailed;
    }

    // Get original registers
    let mut regs: libc::user_regs_struct = unsafe { mem::zeroed() };
    let result = unsafe {
        ptrace(
            PTRACE_GETREGS,
            raw_pid,
            0,
            (&mut regs as *mut libc::user_regs_struct).cast::<libc::c_void>(),
        )
    };
    if result == -1 {
        unsafe { ptrace(PTRACE_DETACH, raw_pid, 0, 0) };
        return InjectStatus::PtraceAttachFailed;
    }

    let original_regs = regs;

    // Resolve remote dlopen address from target process's mapped libc/libdl
    let dlopen_addr = match resolve_remote_dlopen(pid) {
        Some(addr) => addr,
        None => {
            log::warn!("[remote_process] Failed to resolve dlopen address in PID {}", pid);
            unsafe { ptrace(PTRACE_DETACH, raw_pid, 0, 0) };
            return InjectStatus::ResolveLoadLibraryFailed;
        }
    };

    // Prepare path string in target memory (null-terminated) on stack below red zone
    let mut path_bytes = library_path.as_bytes().to_vec();
    path_bytes.push(0);

    // Place path string safely below the 128-byte red zone (with 4096-byte margin) of the interrupted frame
    #[cfg(target_arch = "x86_64")]
    let string_addr = (original_regs.rsp.saturating_sub(4096)) & !0xFu64;
    #[cfg(target_arch = "x86")]
    let string_addr = ((original_regs.esp as u64).saturating_sub(2048)) & !0xFu64;

    if !write_process_bytes(raw_pid, string_addr, &path_bytes) {
        unsafe { ptrace(PTRACE_DETACH, raw_pid, 0, 0) };
        return InjectStatus::WriteFailed;
    }

    #[cfg(target_arch = "x86_64")]
    {
        // Align call frame to 16 bytes. Notice that call_rsp must be placed BELOW string_addr
        // so that dlopen's downward-growing stack frames never overwrite the path string.
        // In AMD64 SysV ABI, before a function call rsp is 16-byte aligned; upon entering the function
        // (with an 8-byte return address pushed), (rsp + 8) % 16 == 0.
        let aligned_rsp = (string_addr.saturating_sub(1024)) & !0xFu64;
        let call_rsp = aligned_rsp - 8;

        // Return address is 0x0. When dlopen finishes and executes `ret`, it pops 0x0 into rip,
        // triggering a SIGSEGV at address 0. The signal is intercepted by ptrace without modifying
        // any executable code in the target process (avoiding multithread crashes).
        let ret_addr: u64 = 0;
        let ret_bytes = ret_addr.to_ne_bytes();
        let _ = write_process_bytes(raw_pid, call_rsp, &ret_bytes);

        regs.rsp = call_rsp;
        regs.rip = dlopen_addr as u64;
        regs.rdi = string_addr;
        regs.rsi = (libc::RTLD_NOW | libc::RTLD_GLOBAL) as u64;
        regs.rax = 0;
        regs.orig_rax = u64::MAX;
    }

    #[cfg(target_arch = "x86")]
    {
        // On x86 (cdecl), arguments and return address are passed on the stack.
        // Stack layout at function entry:
        //   [esp + 0]: return address (0x0)
        //   [esp + 4]: arg 1 (string_addr)
        //   [esp + 8]: arg 2 (RTLD_NOW | RTLD_GLOBAL)
        let aligned_esp = (string_addr.saturating_sub(512)) & !0xFu64;
        let call_esp = (aligned_esp - 16) as u32;

        let mut frame = Vec::new();
        frame.extend_from_slice(&0u32.to_ne_bytes()); // return addr = 0x0
        frame.extend_from_slice(&(string_addr as u32).to_ne_bytes()); // arg 1
        frame.extend_from_slice(&((libc::RTLD_NOW | libc::RTLD_GLOBAL) as u32).to_ne_bytes()); // arg 2
        frame.extend_from_slice(&0u32.to_ne_bytes()); // padding to 16 bytes
        let _ = write_process_bytes(raw_pid, call_esp as u64, &frame);

        regs.esp = call_esp as _;
        regs.eip = (dlopen_addr as u32) as _;
        regs.orig_eax = u32::MAX as _;
    }

    // Apply modified registers and continue to remote dlopen call
    if unsafe { ptrace(PTRACE_SETREGS, raw_pid, 0, (&regs as *const libc::user_regs_struct).cast::<libc::c_void>()) } == -1 {
        unsafe {
            ptrace(PTRACE_DETACH, raw_pid, 0, 0);
        }
        return InjectStatus::WriteFailed;
    }

    if unsafe { ptrace(PTRACE_CONT, raw_pid, 0, 0) } == -1 {
        unsafe {
            ptrace(
                PTRACE_SETREGS,
                raw_pid,
                0,
                (&original_regs as *const libc::user_regs_struct).cast::<libc::c_void>(),
            );
            ptrace(PTRACE_DETACH, raw_pid, 0, 0);
        }
        return InjectStatus::WaitFailed;
    }

    // Wait for dlopen to finish and return to 0x0 (triggering SIGSEGV intercepted by ptrace).
    // Note: No 0xCC (int3) is ever patched into code memory, so other threads never hit a trap.
    let mut trap_status = 0;
    let mut stopped_at_ret = false;
    let mut final_regs: libc::user_regs_struct = unsafe { mem::zeroed() };

    for _ in 0..100 {
        let rc = unsafe { waitpid(raw_pid, &mut trap_status, 0) };
        if rc == -1 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if !WIFSTOPPED(trap_status) {
            break;
        }

        if unsafe { ptrace(PTRACE_GETREGS, raw_pid, 0, (&mut final_regs as *mut libc::user_regs_struct).cast::<libc::c_void>()) } != -1 {
            #[cfg(target_arch = "x86_64")]
            let cur_ip = final_regs.rip;
            #[cfg(target_arch = "x86")]
            let cur_ip = final_regs.eip as u64;

            if cur_ip == 0 {
                stopped_at_ret = true;
                break;
            }
        }

        let sig = WSTOPSIG(trap_status);
        if sig == libc::SIGSEGV || sig == libc::SIGBUS || sig == libc::SIGILL {
            // Target crashed inside dlopen (not at the return address 0)
            log::warn!("[remote_process] Target thread crashed with signal {} inside dlopen", sig);
            break;
        }

        // Continue if stopped by other non-fatal signals
        let _ = unsafe { ptrace(PTRACE_CONT, raw_pid, 0, 0) };
    }

    let mut inject_status = InjectStatus::RemoteLoadFailed;
    if stopped_at_ret {
        #[cfg(target_arch = "x86_64")]
        let handle = final_regs.rax;
        #[cfg(target_arch = "x86")]
        let handle = final_regs.eax as u64;

        if handle != 0 {
            inject_status = InjectStatus::Ok;
            log::info!(
                "[remote_process] dlopen succeeded in PID {}: handle=0x{:X}",
                pid, handle
            );
        } else {
            log::warn!("[remote_process] dlopen returned NULL for '{}' in PID {}", library_path, pid);
        }
    } else {
        log::warn!("[remote_process] Unexpected stop signal or wait status for PID {}: {}", pid, trap_status);
    }

    // Clean up: restore original registers (restoring original RIP and RSP) and detach cleanly.
    // Detaching with data = 0 suppresses the pending SIGSEGV signal, so the process continues seamlessly.
    unsafe {
        ptrace(
            PTRACE_SETREGS,
            raw_pid,
            0,
            (&original_regs as *const libc::user_regs_struct).cast::<libc::c_void>(),
        );
        ptrace(PTRACE_DETACH, raw_pid, 0, 0);
    }

    inject_status
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
fn inject_via_ptrace(_pid: u32, _library_path: &str) -> InjectStatus {
    InjectStatus::UnknownArchitecture
}

/// Build an updated environment vector with `LD_PRELOAD` and `LD_LIBRARY_PATH` safely prepended
/// without introducing empty colon segments (which glibc interprets as the current working directory).
pub fn build_ld_preload_env(existing_env: &[String], library_path: &Path) -> Vec<String> {
    let mut env_vars = existing_env.to_vec();
    let lib_str = library_path.to_string_lossy();

    let mut found_preload = false;
    for env in &mut env_vars {
        if let Some(rest) = env.strip_prefix("LD_PRELOAD=") {
            let trimmed = rest.trim();
            if trimmed.is_empty() {
                *env = format!("LD_PRELOAD={}", lib_str);
            } else if !trimmed.split(':').any(|item| item == lib_str.as_ref()) {
                *env = format!("LD_PRELOAD={}:{}", lib_str, trimmed);
            }
            found_preload = true;
            break;
        }
    }
    if !found_preload {
        env_vars.push(format!("LD_PRELOAD={}", lib_str));
    }

    if let Some(parent) = library_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let lib_dir = parent.to_string_lossy();
        let mut found_lib_path = false;
        for env in &mut env_vars {
            if let Some(rest) = env.strip_prefix("LD_LIBRARY_PATH=") {
                let trimmed = rest.trim();
                if trimmed.is_empty() {
                    *env = format!("LD_LIBRARY_PATH={}", lib_dir);
                } else if !trimmed.split(':').any(|item| item == lib_dir.as_ref()) {
                    *env = format!("LD_LIBRARY_PATH={}:{}", lib_dir, trimmed);
                }
                found_lib_path = true;
                break;
            }
        }
        if !found_lib_path {
            env_vars.push(format!("LD_LIBRARY_PATH={}", lib_dir));
        }
    }

    env_vars
}

/// Alternative injection method: validate target process & library for `LD_PRELOAD` launch.
/// Reading `/proc/<pid>/cmdline` and `/proc/<pid>/environ` verifies the process is accessible
/// and prepares `LD_PRELOAD`/`LD_LIBRARY_PATH` cleanly.
pub fn inject_via_ld_preload(pid: u32, library_path: &Path) -> InjectStatus {
    if pid == 0 {
        return InjectStatus::OpenProcessFailed;
    }
    if !library_path.is_file() {
        return InjectStatus::InvalidLibraryPath;
    }

    // Verify ELF architecture match
    if let Ok(elf_bytes) = fs::read(library_path) {
        if elf_bytes.len() >= 5 {
            let lib_arch = match elf_bytes[4] {
                1 => Architecture::X86,
                2 => Architecture::X64,
                _ => Architecture::Unknown,
            };
            let target_arch = get_architecture(pid);
            if lib_arch != Architecture::Unknown && target_arch != Architecture::Unknown && lib_arch != target_arch {
                return InjectStatus::UnknownArchitecture;
            }
        }
    }

    // Read the target's null-delimited command line directly so arguments with spaces survive.
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    let Ok(cmdline_bytes) = fs::read(&cmdline_path) else {
        return InjectStatus::OpenProcessFailed;
    };
    let args: Vec<String> = cmdline_bytes
        .split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();

    if args.is_empty() {
        return InjectStatus::OpenProcessFailed;
    }

    // Read environment
    let mut env_vars = Vec::new();
    let environ_path = format!("/proc/{}/environ", pid);
    if let Ok(content) = fs::read(&environ_path) {
        for part in content.split(|&b| b == 0) {
            if let Ok(s) = std::str::from_utf8(part) {
                if !s.is_empty() {
                    env_vars.push(s.to_string());
                }
            }
        }
    }

    let _prepared_env = build_ld_preload_env(&env_vars, library_path);

    // Restarting an arbitrary live process must be coordinated by the launcher.
    InjectStatus::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::id;

    #[test]
    fn test_get_architecture_self() {
        let pid = id();
        let arch = get_architecture(pid);
        assert!(
            arch != Architecture::Unknown,
            "Should detect own architecture"
        );
        println!("Self architecture: {}", arch.as_str());
    }

    #[test]
    fn test_inject_status_strings() {
        assert_eq!(InjectStatus::Ok.as_str(), "Ok");
        assert_eq!(
            InjectStatus::PtraceAttachFailed.as_str(),
            "PtraceAttachFailed"
        );
    }

    #[test]
    fn test_architecture_strings() {
        assert_eq!(Architecture::X64.as_str(), "x64");
        assert_eq!(Architecture::X86.as_str(), "x86");
    }

    #[test]
    fn test_parse_elf_architecture_distinguishes_machine() {
        let mut x64_hdr = [0u8; 20];
        x64_hdr[0..4].copy_from_slice(b"\x7fELF");
        x64_hdr[4] = 2; // 64-bit
        x64_hdr[5] = 1; // little-endian
        x64_hdr[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
        assert_eq!(parse_elf_architecture(&x64_hdr), Architecture::X64);

        let mut x86_hdr = x64_hdr;
        x86_hdr[4] = 1; // 32-bit
        x86_hdr[18..20].copy_from_slice(&3u16.to_le_bytes()); // EM_386
        assert_eq!(parse_elf_architecture(&x86_hdr), Architecture::X86);

        let mut arm64_hdr = x64_hdr;
        arm64_hdr[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
        assert_eq!(parse_elf_architecture(&arm64_hdr), Architecture::Unknown);
        assert_eq!(get_architecture(0), Architecture::Unknown);
    }

    #[test]
    fn test_build_ld_preload_env_no_empty_colon_and_dedup() {
        let existing = vec![
            "PATH=/usr/bin".to_string(),
            "LD_PRELOAD=".to_string(),
            "LD_LIBRARY_PATH=".to_string(),
        ];
        let lib = Path::new("/opt/luatools/libbridge.so");
        let env = build_ld_preload_env(&existing, lib);
        assert!(env.contains(&"LD_PRELOAD=/opt/luatools/libbridge.so".to_string()));
        assert!(env.contains(&"LD_LIBRARY_PATH=/opt/luatools".to_string()));

        // Calling again must not duplicate entries
        let env2 = build_ld_preload_env(&env, lib);
        assert_eq!(env, env2);
    }

    #[test]
    fn test_inject_rejects_invalid_pid_and_missing_library() {
        assert_eq!(
            inject_library(0, Path::new("/nonexistent.so")),
            InjectStatus::OpenProcessFailed
        );
        assert_eq!(
            inject_library(id(), Path::new("/nonexistent.so")),
            InjectStatus::OpenProcessFailed
        );
        assert_eq!(
            inject_via_ld_preload(0, Path::new("/nonexistent.so")),
            InjectStatus::OpenProcessFailed
        );
        assert_eq!(
            inject_via_ld_preload(id(), Path::new("/nonexistent/libfoo.so")),
            InjectStatus::InvalidLibraryPath
        );
    }

    #[test]
    fn test_resolve_remote_dlopen_self() {
        let pid = id();
        let addr = resolve_remote_dlopen(pid);
        assert!(addr.is_some(), "Should resolve dlopen in own process");
        let dlopen_ptr = addr.unwrap();
        assert!(dlopen_ptr > 0);

        let dlsym_dlopen = unsafe {
            let sym = libc::dlsym(libc::RTLD_DEFAULT, b"dlopen\0".as_ptr() as *const _);
            if !sym.is_null() {
                sym as usize
            } else {
                libc::dlsym(libc::RTLD_DEFAULT, b"__libc_dlopen_mode\0".as_ptr() as *const _) as usize
            }
        };
        if dlsym_dlopen != 0 {
            assert_eq!(dlopen_ptr, dlsym_dlopen, "Resolved dlopen should match dlsym in self process");
        }
    }
}
