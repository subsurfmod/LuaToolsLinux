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
    if has_gui_display() {
        if show_with_yad(title, message, "warning") {
            return Ok(());
        }
        if show_with_zenity(title, message, "--warning") {
            return Ok(());
        }
        if show_with_kdialog(title, message, "--sorry") {
            return Ok(());
        }
    }
    eprintln!("WARNING: [{}] {}", title, message);
    Ok(())
}

/// Show an informational dialog (OK, info icon). Blocks until dismissed.
pub fn show_info(title: &str, message: &str) -> Result<(), DialogError> {
    if has_gui_display() {
        if show_with_yad(title, message, "info") {
            return Ok(());
        }
        if show_with_zenity(title, message, "--info") {
            return Ok(());
        }
        if show_with_kdialog(title, message, "--msgbox") {
            return Ok(());
        }
    }
    eprintln!("INFO: [{}] {}", title, message);
    Ok(())
}

/// Show a Yes/No confirmation dialog. Returns `true` when the user chooses "Yes".
pub fn show_confirm(title: &str, message: &str) -> Result<bool, DialogError> {
    if has_gui_display() {
        if let Some(result) = show_confirm_with_yad(title, message) {
            return Ok(result);
        }
        if let Some(result) = show_confirm_with_zenity(title, message) {
            return Ok(result);
        }
        if let Some(result) = show_confirm_with_kdialog(title, message) {
            return Ok(result);
        }
    }
    eprintln!("CONFIRM: [{}] {}", title, message);
    eprintln!("Enter 'y' or 'yes' for Yes, anything else for No:");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(parse_confirm_input(&input))
}

fn has_gui_display() -> bool {
    has_gui_display_values(
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
    )
}

fn has_gui_display_values(
    display: Option<&std::ffi::OsStr>,
    wayland_display: Option<&std::ffi::OsStr>,
) -> bool {
    display.is_some_and(|v| !v.is_empty()) || wayland_display.is_some_and(|v| !v.is_empty())
}

fn parse_confirm_input(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn stderr_indicates_display_error(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    text.contains("cannot open display")
        || text.contains("could not connect to display")
        || text.contains("failed to open display")
        || text.contains("no protocol specified")
        || text.contains("gtk-warning")
}

fn build_yad_notice_args(title: &str, message: &str, dialog_type: &str) -> Option<Vec<String>> {
    let icon = match dialog_type {
        "warning" => "warning",
        "info" => "info",
        _ => return None,
    };
    Some(vec![
        format!("--title={}", title),
        format!("--text={}", message),
        "--button=OK:0".to_string(),
        format!("--icon={}", icon),
        "--center".to_string(),
        "--timeout=30".to_string(),
    ])
}

fn interpret_yad_notice_status(code: Option<i32>, stderr: &[u8]) -> bool {
    // yad: 0 = OK, 70 = timeout, 252 = ESC/closed window
    matches!(code, Some(0 | 70 | 252)) && !stderr_indicates_display_error(stderr)
}

fn interpret_yad_confirm_status(code: Option<i32>, stderr: &[u8]) -> Option<bool> {
    if stderr_indicates_display_error(stderr) {
        return None;
    }
    match code {
        Some(0) => Some(true),
        Some(1 | 70 | 252) => Some(false),
        _ => None,
    }
}

fn interpret_zenity_notice_status(code: Option<i32>, stderr: &[u8]) -> bool {
    // zenity: 0 = OK, 1 = closed/ESC, 5 = timeout
    matches!(code, Some(0 | 1 | 5)) && !stderr_indicates_display_error(stderr)
}

fn interpret_zenity_confirm_status(code: Option<i32>, stderr: &[u8]) -> Option<bool> {
    if stderr_indicates_display_error(stderr) {
        return None;
    }
    match code {
        Some(0) => Some(true),
        Some(1 | 5) => Some(false),
        _ => None,
    }
}

fn interpret_kdialog_confirm_status(code: Option<i32>, stderr: &[u8]) -> Option<bool> {
    if stderr_indicates_display_error(stderr) {
        return None;
    }
    match code {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn show_with_yad(title: &str, message: &str, dialog_type: &str) -> bool {
    let Some(args) = build_yad_notice_args(title, message, dialog_type) else {
        return false;
    };

    std::process::Command::new("yad")
        .args(&args)
        .output()
        .map(|o| interpret_yad_notice_status(o.status.code(), &o.stderr))
        .unwrap_or(false)
}

fn show_with_zenity(title: &str, message: &str, flag: &str) -> bool {
    let args = [
        flag.to_string(),
        format!("--title={}", title),
        format!("--text={}", message),
        "--timeout=30".to_string(),
    ];
    std::process::Command::new("zenity")
        .args(&args)
        .output()
        .map(|o| interpret_zenity_notice_status(o.status.code(), &o.stderr))
        .unwrap_or(false)
}

fn show_with_kdialog(title: &str, message: &str, flag: &str) -> bool {
    std::process::Command::new("kdialog")
        .args(["--title", title, flag, message])
        .output()
        .map(|o| o.status.success() && !stderr_indicates_display_error(&o.stderr))
        .unwrap_or(false)
}

fn show_confirm_with_zenity(title: &str, message: &str) -> Option<bool> {
    let args = [
        "--question".to_string(),
        format!("--title={}", title),
        format!("--text={}", message),
        "--timeout=30".to_string(),
    ];
    let out = std::process::Command::new("zenity")
        .args(&args)
        .output()
        .ok()?;
    interpret_zenity_confirm_status(out.status.code(), &out.stderr)
}

fn show_confirm_with_kdialog(title: &str, message: &str) -> Option<bool> {
    let out = std::process::Command::new("kdialog")
        .args(["--title", title, "--yesno", message])
        .output()
        .ok()?;
    interpret_kdialog_confirm_status(out.status.code(), &out.stderr)
}

fn show_confirm_with_yad(title: &str, message: &str) -> Option<bool> {
    let args = [
        format!("--title={}", title),
        format!("--text={}", message),
        "--button=Yes:0".to_string(),
        "--button=No:1".to_string(),
        "--icon=question".to_string(),
        "--center".to_string(),
        "--timeout=30".to_string(),
    ];
    let out = std::process::Command::new("yad")
        .args(&args)
        .output()
        .ok()?;
    interpret_yad_confirm_status(out.status.code(), &out.stderr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn test_has_gui_display_values() {
        assert!(!has_gui_display_values(None, None));
        assert!(!has_gui_display_values(
            Some(OsStr::new("")),
            Some(OsStr::new(""))
        ));
        assert!(has_gui_display_values(Some(OsStr::new(":0")), None));
        assert!(has_gui_display_values(None, Some(OsStr::new("wayland-0"))));
    }

    #[test]
    fn test_parse_confirm_input() {
        assert!(parse_confirm_input("y\n"));
        assert!(parse_confirm_input("  YES \r\n"));
        assert!(!parse_confirm_input("n\n"));
        assert!(!parse_confirm_input("no\n"));
        assert!(!parse_confirm_input(""));
        assert!(!parse_confirm_input("yep"));
    }

    #[test]
    fn test_build_yad_notice_args_prevents_flag_injection() {
        let args = build_yad_notice_args("--evil", "--bad-flag", "warning").unwrap();
        assert!(args.iter().any(|a| a == "--title=--evil"));
        assert!(args.iter().any(|a| a == "--text=--bad-flag"));
        assert!(build_yad_notice_args("t", "m", "unknown").is_none());
    }

    #[test]
    fn test_interpret_dialog_statuses() {
        assert!(interpret_yad_notice_status(Some(0), b""));
        assert!(interpret_yad_notice_status(Some(70), b""));
        assert!(interpret_yad_notice_status(Some(252), b""));
        assert!(!interpret_yad_notice_status(Some(1), b""));
        assert!(!interpret_yad_notice_status(
            Some(0),
            b"Gtk-WARNING: cannot open display"
        ));

        assert_eq!(interpret_yad_confirm_status(Some(0), b""), Some(true));
        assert_eq!(interpret_yad_confirm_status(Some(1), b""), Some(false));
        assert_eq!(interpret_yad_confirm_status(Some(252), b""), Some(false));
        assert_eq!(
            interpret_yad_confirm_status(Some(1), b"cannot open display"),
            None
        );

        assert_eq!(interpret_zenity_confirm_status(Some(0), b""), Some(true));
        assert_eq!(interpret_zenity_confirm_status(Some(1), b""), Some(false));
        assert_eq!(interpret_zenity_confirm_status(Some(5), b""), Some(false));
        assert_eq!(interpret_zenity_confirm_status(Some(255), b""), None);
        assert_eq!(
            interpret_zenity_confirm_status(Some(1), b"cannot open display: :99"),
            None
        );

        assert_eq!(interpret_kdialog_confirm_status(Some(0), b""), Some(true));
        assert_eq!(interpret_kdialog_confirm_status(Some(1), b""), Some(false));
        assert_eq!(
            interpret_kdialog_confirm_status(Some(1), b"qt.qpa.xcb: could not connect to display"),
            None
        );
    }
}
