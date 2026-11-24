//! Linux implementation of OSTPlatform::Thread
//!
//! Provides POSIX thread functionality compatible with the Windows API.
//! Uses pthread_create for thread creation.

use std::os::raw::c_uint;
use std::os::unix::thread::JoinHandleExt;
use std::ptr;
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
/// Uses `std::thread::Builder::spawn` so thread creation failures return null
/// instead of panicking, and returns the spawned thread's real `pthread_t`
/// handle before detaching it.
///
/// # Arguments
/// * `entry` - Function to run in the new thread returning a `u32` exit code.
///
/// # Returns
/// The native thread handle on success, or null on failure.
pub fn start_detached(entry: fn() -> c_uint) -> NativeThreadHandle {
    match std_thread::Builder::new().spawn(move || {
        let _return_value = entry();
    }) {
        Ok(join_handle) => {
            let pthread_id = join_handle.as_pthread_t();
            // Dropping join_handle detaches the thread cleanly.
            drop(join_handle);
            pthread_id as NativeThreadHandle
        }
        Err(_) => ptr::null_mut(),
    }
}

/// Start a detached thread matching the Windows `StartDetached` API.
///
/// Equivalent to: `bool StartDetached(std::function<uint32_t()> entry)`
pub fn start_detached_thread(entry: fn() -> c_uint) -> bool {
    !start_detached(entry).is_null()
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
    start_detached_thread(entry)
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

    #[test]
    fn test_start_detached_returns_distinct_real_pthread_handle() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        static WORKER_HANDLE: AtomicUsize = AtomicUsize::new(0);
        WORKER_HANDLE.store(0, Ordering::SeqCst);

        let caller_handle = current_native_thread_handle();
        let spawned_handle = start_detached(|| {
            WORKER_HANDLE.store(current_native_thread_handle() as usize, Ordering::SeqCst);
            std_thread::sleep(Duration::from_millis(30));
            7
        });
        assert!(!spawned_handle.is_null());
        assert_ne!(
            spawned_handle, caller_handle,
            "Spawned thread must have a different native handle than caller"
        );

        for _ in 0..50 {
            let observed = WORKER_HANDLE.load(Ordering::SeqCst);
            if observed != 0 {
                assert_eq!(
                    observed, spawned_handle as usize,
                    "start_detached must return the worker's real pthread_t handle"
                );
                return;
            }
            std_thread::sleep(Duration::from_millis(5));
        }
        panic!("Detached worker thread did not record its handle in time");
    }
}
