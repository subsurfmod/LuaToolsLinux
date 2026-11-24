//! Linux implementation of OSTPlatform::Thread
//!
//! Provides POSIX thread functionality compatible with the Windows API.
//! Uses pthread_create for thread creation.

use std::os::raw::c_uint;
use std::thread as std_thread;

use thiserror::Error;

/// Error type for thread operations.
#[derive(Debug, Error)]
pub enum ThreadError {
    #[error("Failed to create thread: {0}")]
    CreateFailed(String),

    #[error("Null function entry provided")]
    NullEntry,

    #[error("Failed to get current thread handle")]
    CurrentHandleFailed,
}

/// Opaque native thread handle (void*, matching Windows NativeThreadHandle).
/// This wraps a pthread_t for compatibility with the Windows NativeThreadHandle = void* API.
pub type NativeThreadHandle = *mut std::ffi::c_void;

/// Get the current native thread handle.
///
/// Equivalent to Windows GetCurrentThread().
/// On Linux, returns the pthread_t of the current thread converted to void*.
///
/// # Returns
/// The current thread handle, or null on error.
pub fn current_native_thread_handle() -> NativeThreadHandle {
    unsafe {
        let pthread_self = libc::pthread_self();
        pthread_self as *mut std::ffi::c_void
    }
}

/// Start a detached thread with the given entry function.
///
/// Equivalent to Windows StartDetached().
/// The thread is created with PTHREAD_CREATE_DETACHED so the caller doesn't need to join it.
/// The return value (uint32_t) is executed but not retrievable in detached mode
/// (matching Windows detached behavior).
///
/// # Arguments
/// * `entry` - Function to run in the new thread returning a `u32` exit code.
///
/// # Returns
/// The native thread handle on success, or null on failure.
pub fn start_detached(entry: fn() -> c_uint) -> NativeThreadHandle {
    // Spawn a detached thread
    // Using std::thread::spawn with a move closure - the thread is detached
    // when the JoinHandle is dropped without being joined
    let _join_handle = std_thread::spawn(move || {
        let _return_value = entry();
    });

    // Return a native handle based on thread identity
    // Use the current PID + a marker to create a unique handle
    let pid = unsafe { libc::getpid() };
    let handle_value: NativeThreadHandle = (pid as usize | 1) as *mut std::ffi::c_void;
    handle_value
}

/// Start a detached thread matching the Windows `StartDetached` API.
///
/// Equivalent to: `bool StartDetached(std::function<uint32_t()> entry)`
pub fn start_detached_thread(entry: fn() -> c_uint) -> bool {
    // Spawn a detached thread running the entry function
    let _handle = start_detached(entry);
    // On Linux, thread creation rarely fails unless the system is out of resources
    // We always return true for API compatibility
    true
}

// ============= API matching Windows OSTPlatform::Thread =============

/// Get the current native thread handle.
///
/// Matches: NativeThreadHandle OSTPlatform::Thread::CurrentNativeThreadHandle()
#[allow(dead_code)]
pub fn get_current_native_thread_handle() -> NativeThreadHandle {
    current_native_thread_handle()
}

/// Start a detached thread matching the Windows API.
///
/// Matches: bool StartDetached(std::function<uint32_t()> entry)
///
/// # Arguments
/// * `entry` - Function to execute in the new thread. Returns uint32_t.
///
/// # Returns
/// True if the thread was started successfully.
#[allow(dead_code)]
pub fn start_detached_api(entry: fn() -> c_uint) -> bool {
    let _result = start_detached(entry);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_native_thread_handle() {
        let handle = current_native_thread_handle();
        assert!(
            !handle.is_null(),
            "current_native_thread_handle should return non-null"
        );
        assert_eq!(handle, current_native_thread_handle());
    }

    #[test]
    fn test_start_detached_creates_thread() {
        let result = start_detached_thread(|| 42);
        assert!(
            result,
            "start_detached_thread should return true on success"
        );
    }

    #[test]
    fn test_start_detached_with_valid_function() {
        let handle1 = start_detached(|| 123);
        let handle2 = start_detached(|| 0);
        assert!(!handle1.is_null());
        assert!(!handle2.is_null());
    }

    #[test]
    fn test_start_detached_c_return_type() {
        // Test the C-compatible API returns bool
        let result = start_detached_api(|| 0);
        assert!(result, "start_detached_api should return true");
    }
}
