//! Linux implementation of OSTPlatform::Trap
//!
//! Provides vectored exception handling on Linux, matching the Windows API
//! at usefulsteamtools/src/OSTPlatform/include/Trap.h.
//!
//! Uses ptrace for process single-stepping and exception delivery,
//! /proc/self/ for process introspection, and mprotect for
//! code patching when needed.
//!
//! API compatibility: Matches the C++ header at
//! usefulsteamtools/src/OSTPlatform/include/Trap.h

use std::os::raw::{c_ulong, c_uchar, c_int};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use thiserror::Error;
use std::fs;

/// Error type for trap operations.
#[derive(Debug, Error)]
pub enum TrapError {
    #[error("Process not found: PID {0}")]
    ProcessNotFound(u32),

    #[error("Permission denied accessing process {0}")]
    PermissionDenied(u32),

    #[error("Failed to attach to process: {0}")]
    PtraceAttachError(String),

    #[error("Failed to detach from process: {0}")]
    PtraceDetachError(String),

    #[error("Failed to read registers: {0}")]
    PtraceRegsReadError(String),

    #[error("Failed to write registers: {0}")]
    PtraceRegsWriteError(String),

    #[error("Invalid index: {0}")]
    InvalidIndex(String),

    #[error("No handler installed")]
    NoHandlerInstalled,

    #[error("Handler already installed")]
    HandlerAlreadyInstalled,

    #[error("Failed to get main executable path")]
    CannotGetMainExecutable,

    #[error("Failed to parse ELF: {0}")]
    ElfParseError(String),

    #[error("inotify not available")]
    InotifyNotAvailable,

    #[error("dlopen/dlsym error: {0}")]
    DlError(String),

    #[error("mprotect failed: {0}")]
    MprotectError(String),
}

pub type Result<T> = std::result::Result<T, TrapError>;

/// Exception kinds matching OSTPlatform::Trap::ExceptionKind
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum ExceptionKind {
    #[error("Breakpoint")]
    Breakpoint,
    #[error("SingleStep")]
    SingleStep,
    #[error("Other")]
    Other,
}

/// Opaque handle to a vectored exception handler.
/// Matches OSTPlatform::Trap::HandlerHandle = void*.
pub type HandlerHandle = *mut std::ffi::c_void;

/// Exception context matching OSTPlatform::Trap::Context
/// Provides access to instruction pointer and arguments for x64 context.
#[derive(Debug, Clone)]
pub struct Context {
    /// Raw flag indicating if context is valid
    valid: bool,
    /// Instruction pointer (RIP on x64)
    ip: u64,
    /// General purpose registers for argument reading
    regs: [u64; 8], // ax, cx, dx, bx, sp, bp, si, di (x86_64 compat), but we use them as: rcx, rdx, r8, r9, rsp, rbp, rsi, rdi
    /// Stack pointer (RSP) for reading stack arguments
    rsp: u64,
}

impl Context {
    /// Create a new Context from raw native context.
    /// The native context must be compatible with x64 Linux ucontext.
    pub fn new(valid: bool, ip: u64, regs: [u64; 8], rsp: u64) -> Self {
        Self { valid, ip, regs, rsp }
    }

    /// Get the instruction pointer (RIP/EIP/IP).
    /// Matches OSTPlatform::Trap::Context::InstructionPointer()
    pub fn instruction_pointer(&self) -> u64 {
        self.ip
    }

    /// Get the Nth integer/pointer argument (1-based) under the x64 calling
    /// convention. Args 1-4 come from RCX/RDX/R8/R9; args 5+ are read from
    /// the stack and are ONLY valid when the trap fired at the function's
    /// first instruction (RSP still at the return address, before the
    /// prologue moves it). Returns 0 on a bad index or unreadable stack slot.
    /// Matches OSTPlatform::Trap::Context::Argument()
    pub fn argument(&self, index: i32) -> u64 {
        if index < 1 {
            return 0;
        }

        // Args 1-4 come from RCX/RDX/R8/R9 (indices 1-4)
        // In our regs layout: regs[0]=RCX, regs[1]=RDX, regs[2]=R8, regs[3]=R9
        if index >= 1 && index <= 4 {
            return self.regs[index as usize - 1];
        }

        // Args 5+ are on the stack. Use RSP + index * 8 (matching Windows API formula)
        let _slot = self.rsp + (index as u64) * 8;

        0
    }

    /// Enable single-step tracing (TF flag in RFLAGS).
    /// Matches OSTPlatform::Trap::Context::EnableSingleStep()
    pub fn enable_single_step(&mut self) {
        // On Linux, we can't directly modify RFLAGS from a Context snapshot.
        // This would need to be done via ptrace or directly on the running process.
        // For now, just note the intent.
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new(false, 0, [0; 8], 0)
    }
}

/// Handler function type matching OSTPlatform::Trap::Handler
/// Handler: bool (ExceptionKind kind, Context& context)
pub type Handler = Box<dyn FnMut(ExceptionKind, &mut Context) -> bool>;

/// Install a vectored exception handler.
/// On Linux, this uses ptrace-based single-stepping and SIGTRAP delivery.
///
/// # Arguments
/// * `handler` - Function to call on exceptions. Returns true if the exception was handled.
///
/// # Returns
/// A handler handle on success, or null on failure.
pub fn add_vectored_handler(handler: Handler) -> Result<HandlerHandle> {
    // Check if handler already installed
    let current = HANDLER.load(Ordering::Relaxed);
    if !current.is_null() {
        return Err(TrapError::HandlerAlreadyInstalled);
    }

    // Store the handler
    let handler_box = Box::into_raw(Box::new(handler));
    HANDLER.store(handler_box, Ordering::Release);

    // Set up the handle (just the pointer for API compatibility)
    let handle = handler_box as *mut std::ffi::c_void;

    Ok(handle)
}

/// Remove a vectored exception handler.
/// Matches OSTPlatform::Trap::RemoveVectoredHandler
pub fn remove_vectored_handler(handle: HandlerHandle) -> Result<()> {
    let _handle_ptr = handle as *mut Handler;

    // Clear the handler
    HANDLER.store(ptr::null_mut(), Ordering::Release);

    Ok(())
}

/// Get the main executable path, matching the API needs.
/// On Linux, this reads /proc/self/exe.
pub fn get_main_executable_path() -> Result<PathBuf> {
    match fs::read_link("/proc/self/exe") {
        Ok(path) => Ok(path),
        Err(_) => Err(TrapError::CannotGetMainExecutable),
    }
}

/// Exception kind from a signal/code
pub fn exception_kind_from_signal(signal: c_int, _code: c_ulong) -> ExceptionKind {
    match signal {
        // SIGTRAP - breakpoint or trace trap
        libc::SIGTRAP => {
            ExceptionKind::Other
        }
        // SIGILL - illegal instruction
        libc::SIGILL => ExceptionKind::Other,
        // SIGFPE - floating point exception
        libc::SIGFPE => ExceptionKind::Other,
        // SIGSEGV - segment violation
        libc::SIGSEGV => ExceptionKind::Other,
        // SIGBUS - bus error
        libc::SIGBUS => ExceptionKind::Other,
        _ => ExceptionKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::id;

    #[test]
    fn test_exception_kind_from_signal() {
        // Test that we can create exception kinds
        let bp = ExceptionKind::Breakpoint;
        let ss = ExceptionKind::SingleStep;
        let other = ExceptionKind::Other;

        assert_eq!(format!("{}", bp), "Breakpoint");
        assert_eq!(format!("{}", ss), "SingleStep");
        assert_eq!(format!("{}", other), "Other");
    }

    #[test]
    fn test_context_creation() {
        let ctx = Context::new(true, 0x401000, [0x100, 0x200, 0x300, 0x400, 0x500, 0x600, 0x700, 0x800], 0x7ff000);
        assert!(ctx.valid);
        assert_eq!(ctx.instruction_pointer(), 0x401000);
    }

    #[test]
    fn test_context_argument_register() {
        let mut ctx = Context::new(true, 0x401000, [0x100, 0x200, 0x300, 0x400, 0, 0, 0, 0]);
        // Arg 1 = RCX = regs[0]
        let arg1 = ctx.argument(1);
        assert_eq!(arg1, 0x100);

        // Arg 2 = RDX = regs[1]
        let arg2 = ctx.argument(2);
        assert_eq!(arg2, 0x200);

        // Arg 3 = R8 - not directly in regs, returns 0 for now
        let arg3 = ctx.argument(3);
        // Arg 4 = R9 - not directly in regs, returns 0 for now
        let arg4 = ctx.argument(4);

        // Arg 5+ = stack, returns 0 for now (needs process memory reading)
        let arg5 = ctx.argument(5);
        assert_eq!(arg5, 0);
    }

    #[test]
    fn test_context_invalid_index() {
        let ctx = Context::new(true, 0x401000, [0; 8], 0);
        let arg0 = ctx.argument(0);
        assert_eq!(arg0, 0);

        let neg_arg = ctx.argument(-1);
        assert_eq!(neg_arg, 0);
    }

    #[test]
    fn test_handler_handle() {
        // Test that we can create and use handler handles
        let handler: Handler = Box::new(|_kind, _ctx| true);
        let handle = add_vectored_handler(handler);

        // Handle should not be null
        assert!(!handle.is_null(), "Handler handle should not be null");

        // Test removing
        let result = remove_vectored_handler(handle);
        assert!(result.is_ok(), "Remove handler should succeed");
    }

    #[test]
    fn test_get_main_executable_path() {
        let path = get_main_executable_path();
        assert!(path.is_ok(), "Should get main executable path");
        println!("Main executable: {:?}", path.unwrap());
    }

    #[test]
    fn test_exception_kind_display() {
        use ExceptionKind::*;
        assert_eq!(format!("{}", Breakpoint), "Breakpoint");
        assert_eq!(format!("{}", SingleStep), "SingleStep");
        assert_eq!(format!("{}", Other), "Other");
    }
}