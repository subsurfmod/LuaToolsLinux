//! Linux implementation of OSTPlatform::Dialog
//!
//! Provides Linux equivalents of the Windows OSTPlatform::Dialog API using
//! available system dialog tools (yad/zenity/kdialog) or terminal-based
//! alternatives, with fallback to direct terminal output.
//!
//! # API Compatibility
//!
//! This module implements the same API as:
//!   usefulsteamtools/src/OSTPlatform/include/Dialog.h
//!   usefulsteamtools/src/OSTPlatform/Windows/Dialog.cpp

use std::ffi::CString;
use thiserror::Error;

/// Error type for dialog operations.
#[derive(Debug, Error)]
pub enum DialogError {
    #[error("Failed to show dialog: {0}")]
    ShowFailed(String),

    #[error("Dialog tool not available")]
    ToolNotAvailable,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("CString conversion error")]
    CStringError(#[from] std::ffi::NulError),

    #[cfg(test)]
    #[error("Test error: {0}")]
    TestError(String),
}

/// Show a warning dialog (OK, warning icon). Blocks until dismissed.
///
/// Equivalent to Windows: MessageBoxA(nullptr, message, title, MB_OK | MB_ICONWARNING | MB_TOPMOST)
pub fn show_warning(title: &str, message: &str) -> Result<(), DialogError> {
    // Try yad first (most feature-rich, combines zenity and kdialog features)
    if show_with_yad(title, message, "warning") {
        return Ok(());
    }

    // Fall back to zenity
    if show_with_zenity(title, message, "--warning") {
        return Ok(());
    }

    // Fall back to kdialog
    if show_with_kdialog(title, message, "--warning") {
        return Ok(());
    }

    // Fallback: direct terminal output
    eprintln!("WARNING: [{}] {}", title, message);
    Ok(())
}

/// Show an informational dialog (OK, info icon). Blocks until dismissed.
///
/// Equivalent to Windows: MessageBoxA(nullptr, message, title, MB_OK | MB_ICONINFORMATION | MB_TOPMOST)
pub fn show_info(title: &str, message: &str) -> Result<(), DialogError> {
    // Try yad first
    if show_with_yad(title, message, "info") {
        return Ok(());
    }

    // Fall back to zenity
    if show_with_zenity(title, message, "--information") {
        return Ok(());
    }

    // Fall back to kdialog
    if show_with_kdialog(title, message, "--info") {
        return Ok(());
    }

    // Fallback: direct terminal output
    eprintln!("INFO: [{}] {}", title, message);
    Ok(())
}

/// Show a Yes/No confirmation dialog. Returns true when the user chooses "Yes".
///
/// Equivalent to Windows: MessageBoxA(nullptr, message, title, MB_YESNO | MB_ICONINFORMATION) == IDYES
/// Blocks the calling thread until dismissed — never call from DllMain.
pub fn show_confirm(title: &str, message: &str) -> Result<bool, DialogError> {
    // Try yad first (supports --button=yes:0,no:1)
    if let Some(result) = show_confirm_with_yad(title, message) {
        return Ok(result);
    }

    // Fall back to zenity
    if let Some(result) = show_confirm_with_zenity(title, message) {
        return Ok(result);
    }

    // Fall back to kdialog
    if let Some(result) = show_confirm_with_kdialog(title, message) {
        return Ok(result);
    }

    // Fallback: direct terminal output - ask user and read input
    eprintln!("CONFIRM: [{}] {}", title, message);
    eprintln!("Enter 'y' or 'yes' for Yes, anything else for No:");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    Ok(input == "y" || input == "yes")
}

/// Try to show a dialog using yad
fn show_with_yad(title: &str, message: &str, dialog_type: &str) -> bool {
    let args: Vec<&str> = match dialog_type {
        "warning" => {
            vec!["--title", title, "--text", message, "--button=OK:0", "--icon=warning", "--center", "--timeout=30"]
        }
        "info" => {
            vec!["--title", title, "--text", message, "--button=OK:0", "--icon=info", "--center", "--timeout=30"]
        }
        "question" => {
            vec!["--title", title, "--text", message, "--button=Yes:0", "--button=No:1", "--icon=question", "--center", "--timeout=30"]
        }
        _ => return false,
    };

    let output = std::process::Command::new("yad")
        .args(&args)
        .output();

    match output {
        Ok(output) => {
            // yad exit code 0 means a button was clicked
            if dialog_type == "question" {
                // yad with --button=Yes:0,No:1 returns exit code based on button
                // yad's exit code is the button number: 0 = Yes, 1 = No (or timeout/esc)
                match output.status.code() {
                    Some(0) => true,   // Yes clicked
                    Some(1) => false,  // No clicked
                    _ => false,        // timeout or other
                }
            } else {
                // For OK dialogs, any click = true
                true
            }
        }
        Err(_) => false,
    }
}

/// Try to show a dialog using zenity
fn show_with_zenity(title: &str, message: &str, flag: &str) -> bool {
    let output = std::process::Command::new("zenity")
        .args(&["--", flag, "--title", title, "--text", message, "--timeout=30"])
        .output();

    match output.status {
        Some(status) => status.success(),
        None => false,
    }
}

/// Try to show a dialog using kdialog
fn show_with_kdialog(title: &str, message: &str, flag: &str) -> bool {
    let output = std::process::Command::new("kdialog")
        .args(&["--title", title, "--msgbox", message, "--timeout=30"])
        .output();

    match output.status {
        Some(status) => status.success(),
        None => false,
    }
}

/// Try to show a Yes/No confirmation using zenity
fn show_confirm_with_zenity(title: &str, message: &str) -> Option<bool> {
    let output = std::process::Command::new("zenity")
        .args(&["--question", "--title", title, "--text", message, "--timeout=30"])
        .output();

    match output.status {
        Some(status) => Some(status.success()),
        None => None,
    }
}

/// Try to show a Yes/No confirmation using kdialog
fn show_confirm_with_kdialog(title: &str, message: &str) -> Option<bool> {
    let output = std::process::Command::new("kdialog")
        .args(&["--title", title, "--yesno", message, "--timeout=30"])
        .output();

    match output.status {
        Some(status) => Some(status.success()),
        None => None,
    }
}

/// Try to show a Yes/No confirmation using yad
fn show_confirm_with_yad(title: &str, message: &str) -> Option<bool> {
    let args: Vec<&str> = vec!["--title", title, "--text", message, "--button=Yes:0", "--button=No:1", "--icon=question", "--center", "--timeout=30"];

    let output = std::process::Command::new("yad")
        .args(&args)
        .output();

    match output.status.code() {
        Some(0) => Some(true),   // Yes clicked
        Some(1) => Some(false),  // No clicked
        _ => None,               // timeout or other
    }
}