//! Linux implementation of `OSTPlatform::Dialog`.
//!
//! Provides Linux equivalents of the Windows `OSTPlatform::Dialog` API using
//! available system dialog tools (`yad` / `zenity` / `kdialog`) with terminal fallback.

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
}

/// Show a warning dialog (OK, warning icon). Blocks until dismissed.
pub fn show_warning(title: &str, message: &str) -> Result<(), DialogError> {
    if show_with_yad(title, message, "warning") {
        return Ok(());
    }
    if show_with_zenity(title, message, "--warning") {
        return Ok(());
    }
    if show_with_kdialog(title, message, "--sorry") {
        return Ok(());
    }
    eprintln!("WARNING: [{}] {}", title, message);
    Ok(())
}

/// Show an informational dialog (OK, info icon). Blocks until dismissed.
pub fn show_info(title: &str, message: &str) -> Result<(), DialogError> {
    if show_with_yad(title, message, "info") {
        return Ok(());
    }
    if show_with_zenity(title, message, "--info") {
        return Ok(());
    }
    if show_with_kdialog(title, message, "--msgbox") {
        return Ok(());
    }
    eprintln!("INFO: [{}] {}", title, message);
    Ok(())
}

/// Show a Yes/No confirmation dialog. Returns `true` when the user chooses "Yes".
pub fn show_confirm(title: &str, message: &str) -> Result<bool, DialogError> {
    if let Some(result) = show_confirm_with_yad(title, message) {
        return Ok(result);
    }
    if let Some(result) = show_confirm_with_zenity(title, message) {
        return Ok(result);
    }
    if let Some(result) = show_confirm_with_kdialog(title, message) {
        return Ok(result);
    }
    eprintln!("CONFIRM: [{}] {}", title, message);
    eprintln!("Enter 'y' or 'yes' for Yes, anything else for No:");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    Ok(input == "y" || input == "yes")
}

fn show_with_yad(title: &str, message: &str, dialog_type: &str) -> bool {
    let args: Vec<&str> = match dialog_type {
        "warning" => vec![
            "--title",
            title,
            "--text",
            message,
            "--button=OK:0",
            "--icon=warning",
            "--center",
            "--timeout=30",
        ],
        "info" => vec![
            "--title",
            title,
            "--text",
            message,
            "--button=OK:0",
            "--icon=info",
            "--center",
            "--timeout=30",
        ],
        _ => return false,
    };

    std::process::Command::new("yad")
        .args(&args)
        .output()
        .is_ok()
}

fn show_with_zenity(title: &str, message: &str, flag: &str) -> bool {
    std::process::Command::new("zenity")
        .args([flag, "--title", title, "--text", message, "--timeout=30"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn show_with_kdialog(title: &str, message: &str, flag: &str) -> bool {
    std::process::Command::new("kdialog")
        .args(["--title", title, flag, message])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn show_confirm_with_zenity(title: &str, message: &str) -> Option<bool> {
    std::process::Command::new("zenity")
        .args([
            "--question",
            "--title",
            title,
            "--text",
            message,
            "--timeout=30",
        ])
        .output()
        .ok()
        .map(|o| o.status.success())
}

fn show_confirm_with_kdialog(title: &str, message: &str) -> Option<bool> {
    std::process::Command::new("kdialog")
        .args(["--title", title, "--yesno", message])
        .output()
        .ok()
        .map(|o| o.status.success())
}

fn show_confirm_with_yad(title: &str, message: &str) -> Option<bool> {
    let args = [
        "--title",
        title,
        "--text",
        message,
        "--button=Yes:0",
        "--button=No:1",
        "--icon=question",
        "--center",
        "--timeout=30",
    ];
    let out = std::process::Command::new("yad").args(args).output().ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}
