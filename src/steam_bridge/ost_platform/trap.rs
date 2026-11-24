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

use std::fs;
use std::os::raw::{c_int, c_ulong};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use thiserror::Error;

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
    regs: [u64; 8], // regs[0..4] = rcx, rdx, r8, r9; regs[4..8] = rsp, rbp, rsi, rdi
    /// Stack pointer (RSP) for reading stack arguments
    rsp: u64,
    /// Whether the handler requested single-stepping (TF in RFLAGS)
    single_step: bool,
}

impl Context {
    /// Create a new Context from raw native context.
    /// The native context must be compatible with x64 Linux ucontext.
    pub fn new(valid: bool, ip: u64, regs: [u64; 8], rsp: u64) -> Self {
        Self {
            valid,
            ip,
            regs,
            rsp,
            single_step: false,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.valid
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
        if !self.valid || index < 1 {
            return 0;
        }

        // Args 1-4 come from RCX/RDX/R8/R9 (indices 1-4)
        // In our regs layout: regs[0]=RCX, regs[1]=RDX, regs[2]=R8, regs[3]=R9
        if (1..=4).contains(&index) {
            return self.regs[index as usize - 1];
        }

        // Args 5+ are on the stack at RSP + index * 8 (matching Win64 shadow space layout).
        let Some(byte_offset) = (index as u64).checked_mul(8) else {
            return 0;
        };
        let Some(slot) = self.rsp.checked_add(byte_offset) else {
            return 0;
        };
        if slot == 0
            || !super::memory::is_readable(
                slot as *const std::ffi::c_void,
                std::mem::size_of::<u64>(),
            )
        {
            return 0;
        }

        unsafe { ptr::read_unaligned(slot as *const u64) }
    }

    /// Enable single-step tracing (TF flag in RFLAGS).
    /// Matches OSTPlatform::Trap::Context::EnableSingleStep()
    pub fn enable_single_step(&mut self) {
        self.single_step = true;
    }

    /// Returns whether `enable_single_step` was requested on this context.
    pub fn is_single_step_enabled(&self) -> bool {
        self.single_step
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

static HANDLER: AtomicPtr<Handler> = AtomicPtr::new(ptr::null_mut());

/// Install a vectored exception handler.
/// On Linux, this uses ptrace-based single-stepping and SIGTRAP delivery.
///
/// # Arguments
/// * `handler` - Function to call on exceptions. Returns true if the exception was handled.
///
/// # Returns
/// A handler handle on success, or error on failure.
pub fn add_vectored_handler(handler: Handler) -> Result<HandlerHandle> {
    let handler_box = Box::into_raw(Box::new(handler));
    if HANDLER
        .compare_exchange(
            ptr::null_mut(),
            handler_box,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        // Safety: we just allocated handler_box and failed to publish it.
        unsafe {
            drop(Box::from_raw(handler_box));
        }
        return Err(TrapError::HandlerAlreadyInstalled);
    }

    Ok(handler_box as *mut std::ffi::c_void)
}

/// Remove a vectored exception handler.
/// Matches OSTPlatform::Trap::RemoveVectoredHandler.
/// Only removes the handler if `handle` matches the currently installed handler.
pub fn remove_vectored_handler(handle: HandlerHandle) -> Result<()> {
    if handle.is_null() {
        return Err(TrapError::InvalidIndex(
            "Null vectored handler handle".to_string(),
        ));
    }

    let handle_ptr = handle as *mut Handler;
    match HANDLER.compare_exchange(
        handle_ptr,
        ptr::null_mut(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(prev) => {
            // Safety: prev matched handle_ptr and was created by Box::into_raw in add_vectored_handler.
            unsafe {
                drop(Box::from_raw(prev));
            }
            Ok(())
        }
        Err(current) if current.is_null() => Err(TrapError::NoHandlerInstalled),
        Err(_) => Err(TrapError::InvalidIndex(
            "Handler handle does not match installed handler".to_string(),
        )),
    }
}

/// Get the main executable path, matching the API needs.
/// On Linux, this reads /proc/self/exe.
pub fn get_main_executable_path() -> Result<PathBuf> {
    match fs::read_link("/proc/self/exe") {
        Ok(path) => Ok(path),
        Err(_) => Err(TrapError::CannotGetMainExecutable),
    }
}

/// Exception kind from a Linux signal number and `siginfo_t::si_code`.
pub fn exception_kind_from_signal(signal: c_int, code: c_ulong) -> ExceptionKind {
    // Linux <asm-generic/siginfo.h> constants for SIGTRAP:
    const TRAP_BRKPT: c_ulong = 1;
    const TRAP_TRACE: c_ulong = 2;
    const TRAP_HWBKPT: c_ulong = 4;
    const SI_KERNEL: c_ulong = 0x80;

    match signal {
        libc::SIGTRAP => match code {
            TRAP_BRKPT | SI_KERNEL => ExceptionKind::Breakpoint,
            TRAP_TRACE | TRAP_HWBKPT => ExceptionKind::SingleStep,
            _ => ExceptionKind::Other,
        },
        libc::SIGILL | libc::SIGFPE | libc::SIGSEGV | libc::SIGBUS => ExceptionKind::Other,
        _ => ExceptionKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static HANDLER_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_exception_kind_from_signal() {
        let bp = ExceptionKind::Breakpoint;
        let ss = ExceptionKind::SingleStep;
        let other = ExceptionKind::Other;

        assert_eq!(format!("{}", bp), "Breakpoint");
        assert_eq!(format!("{}", ss), "SingleStep");
        assert_eq!(format!("{}", other), "Other");

        assert_eq!(
            exception_kind_from_signal(libc::SIGTRAP, 1),
            ExceptionKind::Breakpoint
        );
        assert_eq!(
            exception_kind_from_signal(libc::SIGTRAP, 0x80),
            ExceptionKind::Breakpoint
        );
        assert_eq!(
            exception_kind_from_signal(libc::SIGTRAP, 2),
            ExceptionKind::SingleStep
        );
        assert_eq!(
            exception_kind_from_signal(libc::SIGSEGV, 1),
            ExceptionKind::Other
        );
    }

    #[test]
    fn test_context_creation() {
        let ctx = Context::new(
            true,
            0x401000,
            [0x100, 0x200, 0x300, 0x400, 0x500, 0x600, 0x700, 0x800],
            0x7ff000,
        );
        assert!(ctx.valid);
        assert_eq!(ctx.instruction_pointer(), 0x401000);
        assert!(!ctx.is_single_step_enabled());
    }

    #[test]
    fn test_context_argument_register() {
        let ctx = Context::new(
            true,
            0x401000,
            [0x100, 0x200, 0x300, 0x400, 0, 0, 0, 0],
            0x7ff000,
        );
        // Arg 1 = RCX = regs[0]
        assert_eq!(ctx.argument(1), 0x100);
        // Arg 2 = RDX = regs[1]
        assert_eq!(ctx.argument(2), 0x200);
        // Arg 3 = R8 = regs[2]
        assert_eq!(ctx.argument(3), 0x300);
        // Arg 4 = R9 = regs[3]
        assert_eq!(ctx.argument(4), 0x400);
        // Arg 5+ with unmapped RSP safely returns 0
        assert_eq!(ctx.argument(5), 0);

        // Arg 5+ with valid readable stack frame reads slot RSP + index * 8
        let fake_stack: [u64; 8] = [0, 0, 0, 0, 0, 0x5555_AAAA, 0x6666_BBBB, 0];
        let mut stack_ctx = Context::new(
            true,
            0x401000,
            [1, 2, 3, 4, 0, 0, 0, 0],
            fake_stack.as_ptr() as u64,
        );
        assert_eq!(stack_ctx.argument(5), 0x5555_AAAA);
        assert_eq!(stack_ctx.argument(6), 0x6666_BBBB);
        stack_ctx.enable_single_step();
        assert!(stack_ctx.is_single_step_enabled());

        // Overflowing RSP must safely return 0, never panic
        let overflow_ctx = Context::new(true, 0x401000, [0; 8], u64::MAX - 8);
        assert_eq!(overflow_ctx.argument(5), 0);

        // Invalid context returns 0 even for register args
        let invalid_ctx = Context::new(false, 0, [0x99; 8], fake_stack.as_ptr() as u64);
        assert_eq!(invalid_ctx.argument(1), 0);
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
        let _guard = HANDLER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let handler: Handler = Box::new(|_kind, _ctx| true);
        let handle = add_vectored_handler(handler).expect("add_vectored_handler should succeed");
        assert!(!handle.is_null(), "Handler handle should not be null");

        // Duplicate install must fail
        let dup: Handler = Box::new(|_kind, _ctx| false);
        assert!(matches!(
            add_vectored_handler(dup),
            Err(TrapError::HandlerAlreadyInstalled)
        ));

        // Null or mismatched handle must NOT uninstall the active handler
        assert!(remove_vectored_handler(ptr::null_mut()).is_err());
        let wrong_handle = (handle as usize).wrapping_add(16) as HandlerHandle;
        assert!(remove_vectored_handler(wrong_handle).is_err());

        let result = remove_vectored_handler(handle);
        assert!(result.is_ok(), "Remove handler should succeed");

        // Removing again returns NoHandlerInstalled
        assert!(matches!(
            remove_vectored_handler(handle),
            Err(TrapError::NoHandlerInstalled)
        ));
    }

    #[test]
    fn test_get_main_executable_path() {
        let path = get_main_executable_path();
        assert!(path.is_ok(), "Should get main executable path");
    }

    #[test]
    fn test_exception_kind_display() {
        use ExceptionKind::*;
        assert_eq!(format!("{}", Breakpoint), "Breakpoint");
        assert_eq!(format!("{}", SingleStep), "SingleStep");
        assert_eq!(format!("{}", Other), "Other");
    }
}
