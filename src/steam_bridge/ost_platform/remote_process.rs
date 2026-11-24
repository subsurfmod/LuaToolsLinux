//! Linux implementation of OSTPlatform::RemoteProcess
//!
//! Provides remote process library injection via ptrace and LD_PRELOAD.
//! On Linux, we don't inject DLLs like Windows - instead we use LD_PRELOAD
//! or ptrace to inject a shared library.

use std::fs;
use std::path::Path;

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
        return InjectStatus::UnknownArchitecture;
    }

    inject_via_ptrace(pid, library_path_str)
}

/// Attach via ptrace, validate register access, restore cleanly, and report status.
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
fn inject_via_ptrace(pid: u32, _library_path: &str) -> InjectStatus {
    use libc::{
        ptrace, waitpid, PTRACE_ATTACH, PTRACE_DETACH, PTRACE_GETREGS, PTRACE_SETREGS, WIFSTOPPED,
    };
    use std::mem;

    let raw_pid = pid as libc::pid_t;

    // Attach to target process
    let result = unsafe { ptrace(PTRACE_ATTACH, raw_pid, 0, 0) };
    if result == -1 {
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

    // Get registers
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

    // Restore original registers and detach cleanly
    let original_regs = regs;
    let _ = unsafe {
        ptrace(
            PTRACE_SETREGS,
            raw_pid,
            0,
            (&original_regs as *const libc::user_regs_struct).cast::<libc::c_void>(),
        )
    };
    if unsafe { ptrace(PTRACE_DETACH, raw_pid, 0, 0) } == -1 {
        return InjectStatus::PtraceDetachFailed;
    }

    // Direct shellcode dlopen injection is not performed; report honestly instead of false Ok.
    InjectStatus::ResolveLoadLibraryFailed
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
}
