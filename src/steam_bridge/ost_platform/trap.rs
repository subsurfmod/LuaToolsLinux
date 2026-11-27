//! Linux implementation of OSTPlatform::Trap
//!
//! Provides POSIX signal-backed vectored exception handling on Linux, matching the
//! C++ interface at `usefulsteamtools/src/OSTPlatform/include/Trap.h`.
//!
//! Installs a real `SA_SIGINFO | SA_NODEFER` `sigaction(SIGTRAP, ...)` handler,
//! extracts registers according to the **Linux System V AMD64 ABI** (`RDI`, `RSI`,
//! `RDX`, `RCX`, `R8`, `R9`, `RSP`, `RBP`, `RIP`, `EFL` on `x86_64`) or 32-bit x86
//! cdecl ABI (`EIP`, `ESP`, `EBP`, `EFL` on `x86`), and writes modified instruction
//! pointers and single-step (`TF = 0x100`) flags back into `ucontext_t` before
//! returning from the signal frame.

use std::cell::UnsafeCell;
use std::fs;
use std::os::raw::{c_int, c_ulong, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::sync::Mutex;
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

    #[error("Failed to install/restore POSIX sigaction: {0}")]
    SigActionError(String),

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

/// Exception kinds matching `OSTPlatform::Trap::ExceptionKind`.
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
/// Matches `OSTPlatform::Trap::HandlerHandle = void*`.
pub type HandlerHandle = *mut c_void;

/// x86/x86_64 `EFLAGS`/`RFLAGS` Trap Flag (`TF`) bit mask enabling single-step execution.
const X86_EFLAGS_TF_MASK: u64 = 0x100;

/// Exception context matching `OSTPlatform::Trap::Context`.
///
/// On `x86_64` Linux (**System V AMD64 ABI**), `regs` stores:
/// - `regs[0]` = `RDI` (argument 1 / arg0)
/// - `regs[1]` = `RSI` (argument 2 / arg1)
/// - `regs[2]` = `RDX` (argument 3 / arg2)
/// - `regs[3]` = `RCX` (argument 4 / arg3)
/// - `regs[4]` = `R8`  (argument 5 / arg4)
/// - `regs[5]` = `R9`  (argument 6 / arg5)
/// - `regs[6]` = `RSP` (stack pointer)
/// - `regs[7]` = `RBP` (frame/base pointer)
#[derive(Debug, Clone)]
pub struct Context {
    /// Raw flag indicating if context is valid
    valid: bool,
    /// Instruction pointer (`RIP` on x86_64, `EIP` on x86)
    ip: u64,
    /// General purpose registers (`[RDI, RSI, RDX, RCX, R8, R9, RSP, RBP]` on x86_64)
    regs: [u64; 8],
    /// Stack pointer (`RSP` on x86_64, `ESP` on x86) for reading stack arguments
    rsp: u64,
    /// Whether the handler requested single-stepping (`TF` in `RFLAGS`/`EFLAGS`)
    single_step: bool,
}

fn is_readable_signal_safe(ptr: *const c_void, len: usize) -> bool {
    if ptr.is_null() || len == 0 {
        return false;
    }
    let mut buf = [0u8; 8];
    let check_len = len.min(buf.len());
    let local = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: check_len,
    };
    let remote = libc::iovec {
        iov_base: ptr as *mut libc::c_void,
        iov_len: check_len,
    };
    let pid = unsafe { libc::getpid() };
    let res = unsafe { libc::process_vm_readv(pid, &local, 1, &remote, 1, 0) };
    res == check_len as isize
}

impl Context {
    /// Create a new `Context` from raw register values.
    /// On `x86_64` Linux System V ABI, `regs[0..6]` correspond to `[RDI, RSI, RDX, RCX, R8, R9]`
    /// and `regs[6..8]` correspond to `[RSP, RBP]`.
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

    /// Get the instruction pointer (`RIP`/`EIP`/`PC`).
    /// Matches `OSTPlatform::Trap::Context::InstructionPointer()`.
    pub fn instruction_pointer(&self) -> u64 {
        self.ip
    }

    /// Update the instruction pointer to be written back into `ucontext_t` when the
    /// trap handler returns `true`.
    pub fn set_instruction_pointer(&mut self, ip: u64) {
        self.ip = ip;
    }

    /// Get the stack pointer (`RSP`/`ESP`/`SP`).
    pub fn stack_pointer(&self) -> u64 {
        self.rsp
    }

    /// Update the stack pointer to be written back into `ucontext_t` when handled.
    pub fn set_stack_pointer(&mut self, rsp: u64) {
        self.rsp = rsp;
        self.regs[6] = rsp;
    }

    /// Get the frame/base pointer (`RBP`/`EBP`).
    pub fn base_pointer(&self) -> u64 {
        self.regs[7]
    }

    /// Raw slice of captured general-purpose registers (`[RDI, RSI, RDX, RCX, R8, R9, RSP, RBP]` on x86_64).
    pub fn registers(&self) -> &[u64; 8] {
        &self.regs
    }

    /// Get the Nth integer/pointer argument (1-based) under the target Linux calling convention.
    ///
    /// - **Linux System V AMD64 ABI (`x86_64`)**:
    ///   Arguments `1..=6` come from `RDI`, `RSI`, `RDX`, `RCX`, `R8`, `R9`.
    ///   Arguments `7+` lie on the stack starting at `[RSP + 8]` (`RSP + (index - 6) * 8`)
    ///   when trapped at function entry (`[RSP]` holds the return address; no Win64 shadow space).
    /// - **Linux i386 cdecl ABI (`x86`)**:
    ///   Arguments `1+` lie on the stack starting at `[ESP + 4]` (`ESP + index * 4`) when
    ///   `ESP` is non-zero and readable (falling back to `regs` for synthetic contexts with `rsp == 0`).
    ///
    /// Returns `0` on an invalid index (`< 1`) or unreadable stack slot.
    pub fn argument(&self, index: i32) -> u64 {
        if !self.valid || index < 1 {
            return 0;
        }

        #[cfg(target_arch = "x86")]
        {
            if self.rsp != 0 {
                let Some(byte_offset) = (index as u64).checked_mul(4) else {
                    return 0;
                };
                let Some(slot) = self.rsp.checked_add(byte_offset) else {
                    return 0;
                };
                if !is_readable_signal_safe(
                    slot as *const c_void,
                    std::mem::size_of::<u32>(),
                ) {
                    return 0;
                }
                return unsafe { ptr::read_unaligned(slot as *const u32) as u64 };
            }
            if (1..=6).contains(&index) {
                return self.regs[index as usize - 1];
            }
            return 0;
        }

        #[cfg(not(target_arch = "x86"))]
        {
            // Linux System V AMD64 ABI: args 1..=6 in RDI, RSI, RDX, RCX, R8, R9
            if (1..=6).contains(&index) {
                return self.regs[index as usize - 1];
            }

            // Args 7+ are on the stack at RSP + (index - 6) * 8 (return address is at [RSP]).
            let stack_arg_ordinal = (index - 6) as u64;
            let Some(byte_offset) = stack_arg_ordinal.checked_mul(8) else {
                return 0;
            };
            let Some(slot) = self.rsp.checked_add(byte_offset) else {
                return 0;
            };
            if slot == 0
                || !is_readable_signal_safe(
                    slot as *const c_void,
                    std::mem::size_of::<u64>(),
                )
            {
                return 0;
            }

            unsafe { ptr::read_unaligned(slot as *const u64) }
        }
    }

    /// Enable single-step tracing (`TF` flag `0x100` in `RFLAGS`/`EFLAGS`).
    /// Matches `OSTPlatform::Trap::Context::EnableSingleStep()`.
    pub fn enable_single_step(&mut self) {
        self.single_step = true;
    }

    /// Clear single-step tracing (`TF` flag `0x100` in `RFLAGS`/`EFLAGS`).
    pub fn disable_single_step(&mut self) {
        self.single_step = false;
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

/// Handler function type matching `OSTPlatform::Trap::Handler`:
/// `bool (ExceptionKind kind, Context& context)`
pub type Handler = Box<dyn FnMut(ExceptionKind, &mut Context) -> bool>;

struct SavedSigAction(UnsafeCell<libc::sigaction>);
// Safety: written only while holding `INSTALL_LOCK` prior to `HAS_PREV_SIGTRAP = true` (`Release`),
// and read either under `INSTALL_LOCK` or in signal fallback after checking `HAS_PREV_SIGTRAP` (`Acquire`).
unsafe impl Sync for SavedSigAction {}

static HANDLER: AtomicPtr<Handler> = AtomicPtr::new(ptr::null_mut());
static ACTIVE_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static HANDLER_MUTEX: AtomicBool = AtomicBool::new(false);
static INSTALL_LOCK: Mutex<()> = Mutex::new(());
static HAS_PREV_SIGTRAP: AtomicBool = AtomicBool::new(false);
static PREV_SIGTRAP: SavedSigAction = SavedSigAction(UnsafeCell::new(unsafe { std::mem::zeroed() }));

#[cfg(target_arch = "x86_64")]
unsafe fn context_from_ucontext(uctx: *mut c_void) -> Context {
    if uctx.is_null() {
        return Context::default();
    }
    let uc = &*(uctx as *const libc::ucontext_t);
    let gregs = &uc.uc_mcontext.gregs;
    let ip = gregs[libc::REG_RIP as usize] as u64;
    let rsp = gregs[libc::REG_RSP as usize] as u64;
    let rbp = gregs[libc::REG_RBP as usize] as u64;
    let regs = [
        gregs[libc::REG_RDI as usize] as u64,
        gregs[libc::REG_RSI as usize] as u64,
        gregs[libc::REG_RDX as usize] as u64,
        gregs[libc::REG_RCX as usize] as u64,
        gregs[libc::REG_R8 as usize] as u64,
        gregs[libc::REG_R9 as usize] as u64,
        rsp,
        rbp,
    ];
    Context::new(true, ip, regs, rsp)
}

#[cfg(target_arch = "x86_64")]
unsafe fn write_context_to_ucontext(ctx: &Context, uctx: *mut c_void) {
    if !ctx.valid || uctx.is_null() {
        return;
    }
    let uc = &mut *(uctx as *mut libc::ucontext_t);
    let gregs = &mut uc.uc_mcontext.gregs;
    gregs[libc::REG_RIP as usize] = ctx.ip as libc::greg_t;
    gregs[libc::REG_RSP as usize] = ctx.rsp as libc::greg_t;
    gregs[libc::REG_RDI as usize] = ctx.regs[0] as libc::greg_t;
    gregs[libc::REG_RSI as usize] = ctx.regs[1] as libc::greg_t;
    gregs[libc::REG_RDX as usize] = ctx.regs[2] as libc::greg_t;
    gregs[libc::REG_RCX as usize] = ctx.regs[3] as libc::greg_t;
    gregs[libc::REG_R8 as usize] = ctx.regs[4] as libc::greg_t;
    gregs[libc::REG_R9 as usize] = ctx.regs[5] as libc::greg_t;
    gregs[libc::REG_RBP as usize] = ctx.regs[7] as libc::greg_t;
    if ctx.single_step {
        gregs[libc::REG_EFL as usize] |= X86_EFLAGS_TF_MASK as libc::greg_t;
    } else {
        gregs[libc::REG_EFL as usize] &= !(X86_EFLAGS_TF_MASK as libc::greg_t);
    }
}

#[cfg(target_arch = "x86")]
unsafe fn context_from_ucontext(uctx: *mut c_void) -> Context {
    if uctx.is_null() {
        return Context::default();
    }
    let uc = &*(uctx as *const libc::ucontext_t);
    let gregs = &uc.uc_mcontext.gregs;
    let ip = (gregs[libc::REG_EIP as usize] as u32) as u64;
    let esp = (gregs[libc::REG_ESP as usize] as u32) as u64;
    let ebp = (gregs[libc::REG_EBP as usize] as u32) as u64;
    let regs = [
        (gregs[libc::REG_EDI as usize] as u32) as u64,
        (gregs[libc::REG_ESI as usize] as u32) as u64,
        (gregs[libc::REG_EDX as usize] as u32) as u64,
        (gregs[libc::REG_ECX as usize] as u32) as u64,
        (gregs[libc::REG_EBX as usize] as u32) as u64,
        (gregs[libc::REG_EAX as usize] as u32) as u64,
        esp,
        ebp,
    ];
    Context::new(true, ip, regs, esp)
}

#[cfg(target_arch = "x86")]
unsafe fn write_context_to_ucontext(ctx: &Context, uctx: *mut c_void) {
    if !ctx.valid || uctx.is_null() {
        return;
    }
    let uc = &mut *(uctx as *mut libc::ucontext_t);
    let gregs = &mut uc.uc_mcontext.gregs;
    gregs[libc::REG_EIP as usize] = (ctx.ip as u32) as libc::greg_t;
    gregs[libc::REG_EDI as usize] = (ctx.regs[0] as u32) as libc::greg_t;
    gregs[libc::REG_ESI as usize] = (ctx.regs[1] as u32) as libc::greg_t;
    gregs[libc::REG_EDX as usize] = (ctx.regs[2] as u32) as libc::greg_t;
    gregs[libc::REG_ECX as usize] = (ctx.regs[3] as u32) as libc::greg_t;
    gregs[libc::REG_EBX as usize] = (ctx.regs[4] as u32) as libc::greg_t;
    gregs[libc::REG_EAX as usize] = (ctx.regs[5] as u32) as libc::greg_t;
    gregs[libc::REG_ESP as usize] = (ctx.rsp as u32) as libc::greg_t;
    gregs[libc::REG_EBP as usize] = (ctx.regs[7] as u32) as libc::greg_t;
    if ctx.single_step {
        gregs[libc::REG_EFL as usize] |= (X86_EFLAGS_TF_MASK as u32) as libc::greg_t;
    } else {
        gregs[libc::REG_EFL as usize] &= !((X86_EFLAGS_TF_MASK as u32) as libc::greg_t);
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
unsafe fn context_from_ucontext(uctx: *mut c_void) -> Context {
    Context::new(!uctx.is_null(), 0, [0; 8], 0)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
unsafe fn write_context_to_ucontext(_ctx: &Context, _uctx: *mut c_void) {}

extern "C" fn posix_trap_sigaction(
    sig: c_int,
    info: *mut libc::siginfo_t,
    uctx: *mut c_void,
) {
    ACTIVE_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
    let handler_ptr = HANDLER.load(Ordering::Acquire);
    if !handler_ptr.is_null() {
        // Spinlock to prevent concurrent mutation of FnMut
        while HANDLER_MUTEX
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }

        let code_raw = if info.is_null() {
            0
        } else {
            let sc = unsafe { (*info).si_code };
            sc as c_ulong
        };
        let kind = exception_kind_from_signal(sig, code_raw);
        let mut ctx = unsafe { context_from_ucontext(uctx) };

        let handled = panic::catch_unwind(AssertUnwindSafe(|| unsafe {
            (*handler_ptr)(kind, &mut ctx)
        }))
        .unwrap_or(false);

        HANDLER_MUTEX.store(false, Ordering::Release);

        if handled {
            unsafe {
                write_context_to_ucontext(&ctx, uctx);
            }
            ACTIVE_INVOCATIONS.fetch_sub(1, Ordering::SeqCst);
            return;
        }
    }
    ACTIVE_INVOCATIONS.fetch_sub(1, Ordering::SeqCst);

    // Chain to the previously registered handler if our vectored handler declined the trap.
    if HAS_PREV_SIGTRAP.load(Ordering::Acquire) {
        unsafe {
            let prev = *PREV_SIGTRAP.0.get();
            let prev_fn = prev.sa_sigaction;
            if prev_fn != libc::SIG_IGN && prev_fn != libc::SIG_DFL && prev_fn != 0 {
                if (prev.sa_flags & libc::SA_SIGINFO) != 0 {
                    let f: extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) =
                        std::mem::transmute(prev_fn);
                    f(sig, info, uctx);
                    return;
                }
                let f: extern "C" fn(c_int) = std::mem::transmute(prev_fn);
                f(sig);
                return;
            }
            if prev_fn == libc::SIG_IGN {
                return;
            }
        }
    }

    // Restore default disposition and re-raise unhandled fatal trap.
    unsafe {
        let mut dfl: libc::sigaction = std::mem::zeroed();
        libc::sigemptyset(&mut dfl.sa_mask);
        dfl.sa_sigaction = libc::SIG_DFL;
        libc::sigaction(sig, &dfl, ptr::null_mut());
        libc::raise(sig);
    }
}

/// Install a vectored exception handler backed by POSIX `sigaction(SIGTRAP, ...)`.
///
/// Matches `OSTPlatform::Trap::AddVectoredHandler`.
pub fn add_vectored_handler(handler: Handler) -> Result<HandlerHandle> {
    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    if !HANDLER.load(Ordering::Acquire).is_null() {
        return Err(TrapError::HandlerAlreadyInstalled);
    }

    let handler_box = Box::into_raw(Box::new(handler));

    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaddset(&mut sa.sa_mask, libc::SIGTRAP);
    }
    let fn_ptr: extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) = posix_trap_sigaction;
    sa.sa_sigaction = fn_ptr as libc::sighandler_t;
    sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;

    let mut old_sa: libc::sigaction = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::sigaction(libc::SIGTRAP, &sa, &mut old_sa) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        unsafe {
            drop(Box::from_raw(handler_box));
        }
        return Err(TrapError::SigActionError(err.to_string()));
    }

    unsafe {
        *PREV_SIGTRAP.0.get() = old_sa;
    }
    HAS_PREV_SIGTRAP.store(true, Ordering::Release);
    HANDLER.store(handler_box, Ordering::Release);

    Ok(handler_box as HandlerHandle)
}

/// Alias for `add_vectored_handler` installing the `SIGTRAP` vectored handler.
pub fn install(handler: Handler) -> Result<HandlerHandle> {
    add_vectored_handler(handler)
}

/// Remove a vectored exception handler and restore the previous `SIGTRAP` `sigaction`.
///
/// Matches `OSTPlatform::Trap::RemoveVectoredHandler`.
/// Only removes the handler if `handle` matches the currently installed handler.
pub fn remove_vectored_handler(handle: HandlerHandle) -> Result<()> {
    if handle.is_null() {
        return Err(TrapError::InvalidIndex(
            "Null vectored handler handle".to_string(),
        ));
    }

    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let handle_ptr = handle as *mut Handler;

    match HANDLER.compare_exchange(
        handle_ptr,
        ptr::null_mut(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(prev) => {
            if HAS_PREV_SIGTRAP.swap(false, Ordering::AcqRel) {
                let restore_rc = unsafe {
                    libc::sigaction(libc::SIGTRAP, PREV_SIGTRAP.0.get(), ptr::null_mut())
                };
                if restore_rc != 0 {
                    let err = std::io::Error::last_os_error();
                    return Err(TrapError::SigActionError(err.to_string()));
                }
            }
            // Wait for any in-flight signal handlers to finish before dropping the Box
            while ACTIVE_INVOCATIONS.load(Ordering::SeqCst) > 0 {
                std::thread::yield_now();
            }
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

/// Alias for `remove_vectored_handler`.
pub fn uninstall(handle: HandlerHandle) -> Result<()> {
    remove_vectored_handler(handle)
}

/// Get the main executable path via `/proc/self/exe`.
pub fn get_main_executable_path() -> Result<PathBuf> {
    match fs::read_link("/proc/self/exe") {
        Ok(path) => Ok(path),
        Err(_) => Err(TrapError::CannotGetMainExecutable),
    }
}

/// Exception kind from a Linux signal number and `siginfo_t::si_code`.
pub fn exception_kind_from_signal(signal: c_int, code: c_ulong) -> ExceptionKind {
    // Linux <asm-generic/siginfo.h> constants for SIGTRAP:
    const SI_USER: c_ulong = 0;
    const TRAP_BRKPT: c_ulong = 1;
    const TRAP_TRACE: c_ulong = 2;
    const TRAP_HWBKPT: c_ulong = 4;
    const SI_KERNEL: c_ulong = 0x80;
    const SI_TKILL: c_ulong = (-6i32) as c_ulong;

    match signal {
        libc::SIGTRAP => match code {
            TRAP_BRKPT | SI_KERNEL | SI_USER | SI_TKILL => ExceptionKind::Breakpoint,
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
    use std::sync::atomic::AtomicUsize;

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
        let mut ctx = Context::new(
            true,
            0x401000,
            [0x100, 0x200, 0x300, 0x400, 0x500, 0x600, 0x7ff000, 0x7ff080],
            0x7ff000,
        );
        assert!(ctx.is_valid());
        assert_eq!(ctx.instruction_pointer(), 0x401000);
        assert_eq!(ctx.stack_pointer(), 0x7ff000);
        assert_eq!(ctx.base_pointer(), 0x7ff080);
        assert!(!ctx.is_single_step_enabled());
        ctx.set_instruction_pointer(0x401004);
        assert_eq!(ctx.instruction_pointer(), 0x401004);
    }

    #[test]
    fn test_context_sysv_amd64_argument_registers_and_stack() {
        // Linux System V AMD64 ABI: args 1..=6 in RDI, RSI, RDX, RCX, R8, R9;
        // [RSP] = return address, [RSP + 8] = arg 7, [RSP + 16] = arg 8.
        let fake_stack: [u64; 4] = [0xDEAD_BEEF_CAFE_0000, 0x7777_AAAA, 0x8888_BBBB, 0];
        let rsp = fake_stack.as_ptr() as u64;
        let mut ctx = Context::new(
            true,
            0x401000,
            [0x101, 0x202, 0x303, 0x404, 0x505, 0x606, rsp, 0],
            rsp,
        );

        #[cfg(not(target_arch = "x86"))]
        {
            assert_eq!(ctx.argument(1), 0x101); // RDI
            assert_eq!(ctx.argument(2), 0x202); // RSI
            assert_eq!(ctx.argument(3), 0x303); // RDX
            assert_eq!(ctx.argument(4), 0x404); // RCX
            assert_eq!(ctx.argument(5), 0x505); // R8
            assert_eq!(ctx.argument(6), 0x606); // R9
            assert_eq!(ctx.argument(7), 0x7777_AAAA); // [RSP + 8]
            assert_eq!(ctx.argument(8), 0x8888_BBBB); // [RSP + 16]
        }

        // Unmapped RSP for stack argument safely returns 0
        let unmapped_ctx = Context::new(true, 0x401000, [1, 2, 3, 4, 5, 6, 0, 0], 0);
        assert_eq!(unmapped_ctx.argument(7), 0);

        ctx.enable_single_step();
        assert!(ctx.is_single_step_enabled());
        ctx.disable_single_step();
        assert!(!ctx.is_single_step_enabled());

        // Overflowing RSP must safely return 0, never panic
        let overflow_ctx = Context::new(true, 0x401000, [0; 8], u64::MAX - 4);
        assert_eq!(overflow_ctx.argument(7), 0);

        // Invalid context returns 0 even for register args
        let invalid_ctx = Context::new(false, 0, [0x99; 8], rsp);
        assert_eq!(invalid_ctx.argument(1), 0);
    }

    #[test]
    fn test_context_invalid_index() {
        let ctx = Context::new(true, 0x401000, [0; 8], 0);
        assert_eq!(ctx.argument(0), 0);
        assert_eq!(ctx.argument(-1), 0);
    }

    #[test]
    fn test_handler_install_and_sigaction_restoration() {
        let _guard = HANDLER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut before_sa: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGTRAP, ptr::null(), &mut before_sa) },
            0
        );

        let handler: Handler = Box::new(|_kind, _ctx| true);
        let handle = install(handler).expect("install should register SIGTRAP sigaction");
        assert!(!handle.is_null(), "Handler handle should not be null");

        let mut installed_sa: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGTRAP, ptr::null(), &mut installed_sa) },
            0
        );
        let expected_fn: extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) =
            posix_trap_sigaction;
        assert_eq!(installed_sa.sa_sigaction, expected_fn as libc::sighandler_t);
        assert_ne!(installed_sa.sa_flags & libc::SA_SIGINFO, 0);

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

        uninstall(handle).expect("uninstall should succeed and restore SIGTRAP");

        let mut after_sa: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGTRAP, ptr::null(), &mut after_sa) },
            0
        );
        assert_eq!(after_sa.sa_sigaction, before_sa.sa_sigaction);

        // Removing again returns NoHandlerInstalled
        assert!(matches!(
            remove_vectored_handler(handle),
            Err(TrapError::NoHandlerInstalled)
        ));
    }

    #[test]
    fn test_real_sigtrap_raise_and_hardware_int3_rip_modification() {
        let _guard = HANDLER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        static TRAP_HITS: AtomicUsize = AtomicUsize::new(0);
        TRAP_HITS.store(0, Ordering::SeqCst);

        // 1. Verify software libc::raise(libc::SIGTRAP) invokes the installed POSIX sigaction
        let raise_handle = add_vectored_handler(Box::new(|kind, ctx| {
            assert_eq!(kind, ExceptionKind::Breakpoint);
            assert!(ctx.is_valid());
            TRAP_HITS.fetch_add(1, Ordering::SeqCst);
            true
        }))
        .expect("install raise handler");

        assert_eq!(unsafe { libc::raise(libc::SIGTRAP) }, 0);
        assert_eq!(TRAP_HITS.load(Ordering::SeqCst), 1);
        remove_vectored_handler(raise_handle).expect("remove raise handler");

        // 2. On x86_64 Linux, execute a real `int3` followed immediately by `ud2` (0x0f, 0x0b),
        //    verify System V AMD64 registers RDI..R9, advance RIP by +2 over `ud2` AND enable
        //    single-stepping (`TF = 0x100`) so the following `nop` fires a real `SingleStep` trap!
        #[cfg(target_arch = "x86_64")]
        {
            static BREAKPOINT_SEEN: AtomicUsize = AtomicUsize::new(0);
            static SINGLESTEP_SEEN: AtomicUsize = AtomicUsize::new(0);
            BREAKPOINT_SEEN.store(0, Ordering::SeqCst);
            SINGLESTEP_SEEN.store(0, Ordering::SeqCst);

            let hw_handle = add_vectored_handler(Box::new(|kind, ctx| {
                assert!(ctx.is_valid());
                match kind {
                    ExceptionKind::Breakpoint => {
                        assert_eq!(ctx.argument(1), 0x1111_2222_3333_4444);
                        assert_eq!(ctx.argument(2), 0x5555_6666_7777_8888);
                        assert_eq!(ctx.argument(3), 0x9999_AAAA_BBBB_CCCC);
                        assert_eq!(ctx.argument(4), 0xDDDD_EEEE_FFFF_0001);
                        assert_eq!(ctx.argument(5), 0x1234_5678_9ABC_DEF0);
                        assert_eq!(ctx.argument(6), 0x0FED_CBA9_8765_4321);

                        // RIP points at the 2-byte `ud2` (0x0f, 0x0b) right after `int3`.
                        // Advancing RIP by +2 skips `ud2` (preventing SIGILL) onto `nop`.
                        let next_bytes = unsafe {
                            std::slice::from_raw_parts(ctx.instruction_pointer() as *const u8, 2)
                        };
                        assert_eq!(next_bytes, &[0x0f, 0x0b]);
                        ctx.set_instruction_pointer(ctx.instruction_pointer() + 2);
                        ctx.enable_single_step();
                        BREAKPOINT_SEEN.fetch_add(1, Ordering::SeqCst);
                        true
                    }
                    ExceptionKind::SingleStep => {
                        // Do not re-enable single_step so TF (0x100) clears on return.
                        SINGLESTEP_SEEN.fetch_add(1, Ordering::SeqCst);
                        true
                    }
                    _ => false,
                }
            }))
            .expect("install hardware int3 handler");

            unsafe {
                core::arch::asm!(
                    "int3",
                    ".byte 0x0f, 0x0b", // ud2 — skipped only if RIP writeback works!
                    "nop",              // single-stepped via RFLAGS TF (0x100)!
                    in("rdi") 0x1111_2222_3333_4444u64,
                    in("rsi") 0x5555_6666_7777_8888u64,
                    in("rdx") 0x9999_AAAA_BBBB_CCCCu64,
                    in("rcx") 0xDDDD_EEEE_FFFF_0001u64,
                    in("r8")  0x1234_5678_9ABC_DEF0u64,
                    in("r9")  0x0FED_CBA9_8765_4321u64,
                    options(nostack),
                );
            }

            remove_vectored_handler(hw_handle).expect("remove hardware int3 handler");
            assert_eq!(BREAKPOINT_SEEN.load(Ordering::SeqCst), 1);
            assert_eq!(SINGLESTEP_SEEN.load(Ordering::SeqCst), 1);
        }
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
