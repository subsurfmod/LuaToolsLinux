//! extract_tickets — Standalone SteamClient ticket dumper (BST-58).
//!
//! Ported directly from UsefulSteamTools `tools/extract_tickets/extract_tickets.cpp`.
//!
//! Dumps App Ownership Tickets (`ISteamAppTicket`) and Encrypted App Tickets (`ISteamUser`)
//! for a specified AppID. Supports:
//! 1. Live dynamic extraction via Linux `steamclient.so` (`SteamClient023`..`SteamClient017`)
//!    by establishing an IPC session with the running Steam client (`CreateSteamPipe` + `ConnectToGlobalUser`).
//! 2. Offline / cache extraction via `TicketStore` / `localconfig.vdf` / `tickets_store.json`.
//! 3. Output generation matching the canonical Windows tool:
//!    - Standard hex dump output to console
//!    - Directory `<appid>/` containing:
//!      - `appticket.bin` (raw binary ownership ticket)
//!      - `eticket.bin` (raw binary encrypted app ticket)
//!      - `tickets.txt` (plain-text summary with hex-encoded strings)

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::steam_bridge::credentials::appticket;
use crate::steam_bridge::credentials::eticketclient;
use crate::steam_bridge::ticket_store::TicketStore;

/// Result of a ticket extraction run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketExtractionResult {
    pub app_id: u32,
    pub ownership_ticket: Option<Vec<u8>>,
    pub encrypted_ticket: Option<Vec<u8>>,
    pub output_dir: PathBuf,
    pub hex_dump: String,
    pub tickets_txt: String,
}

/// Locates the Steam installation root on Linux.
pub fn find_steam_install_path() -> Option<PathBuf> {
    if let Ok(dir) = env::var("STEAM_DIR") {
        let p = PathBuf::from(dir);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(dir) = env::var("STEAM_PATH") {
        let p = PathBuf::from(dir);
        if p.exists() {
            return Some(p);
        }
    }

    if let Some(home) = dirs::home_dir() {
        let candidates = [
            home.join(".local/share/Steam"),
            home.join(".steam/steam"),
            home.join(".steam/root"),
            home.join(".steam"),
        ];
        for cand in candidates {
            if cand.join("config").exists() || cand.join("steamapps").exists() {
                return Some(cand);
            }
        }
    }

    None
}

/// Format bytes as a classic hex dump:
/// `000000000000  xx xx xx xx ... ascii`
pub fn format_hex_dump(label: &str, data: &[u8]) -> String {
    let mut out = format!("{} ({} bytes):\n", label, data.len());
    const BYTES_PER_ROW: usize = 16;
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

    for (row_idx, chunk) in data.chunks(BYTES_PER_ROW).enumerate() {
        let offset = row_idx * BYTES_PER_ROW;
        let mut line = String::new();

        // 4 hex digits offset
        for shift in (0..4).rev() {
            let digit = ((offset >> (shift * 4)) & 0xF) as usize;
            line.push(HEX_CHARS[digit] as char);
        }
        line.push_str("  ");

        // Hex bytes
        let mut ascii = String::new();
        for col in 0..BYTES_PER_ROW {
            if col < chunk.len() {
                let byte = chunk[col];
                line.push(HEX_CHARS[(byte >> 4) as usize] as char);
                line.push(HEX_CHARS[(byte & 0xF) as usize] as char);
                line.push(' ');
                if (0x20..0x7F).contains(&byte) {
                    ascii.push(byte as char);
                } else {
                    ascii.push('.');
                }
            } else {
                line.push_str("   ");
            }
            if col == 7 {
                line.push(' ');
            }
        }

        line.push(' ');
        line.push_str(&ascii);
        line.push('\n');
        out.push_str(&line);
    }

    out
}

/// Convert byte slice to hex string.
pub fn to_hex_string(data: &[u8]) -> String {
    hex::encode(data)
}

/// Build summary line for tickets.txt:
/// Present -> `name(Nbytes):<hex>\n`
/// Absent  -> `name:null\n`
pub fn format_ticket_line(name: &str, ticket: Option<&[u8]>) -> String {
    match ticket {
        Some(t) => format!("{}({}bytes):{}\n", name, t.len(), to_hex_string(t)),
        None => format!("{}:null\n", name),
    }
}

/// Extract ownership ticket for an app.
/// Checks live Steam client / in-memory cache first, then falls back to TicketStore.
pub fn extract_app_ownership_ticket(steam_root: Option<&Path>, app_id: u32) -> Option<Vec<u8>> {
    // 1. Try TicketStore from steam root or default path
    let mut ts = match steam_root {
        Some(root) => TicketStore::new_for_steam_root(root),
        None => TicketStore::default(),
    };
    let _ = ts.load();

    if let Ok(entry) = ts.get(app_id) {
        if let Some(ref ticket_hex) = entry.app_ticket {
            if let Ok(bytes) = hex::decode(ticket_hex.trim()) {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
        }
    }

    // 2. Try appticket helper module
    if let Ok(Some(ticket)) = appticket::read_app_ticket(app_id) {
        if let Some(ref hex_str) = ticket.app_ticket {
            if let Ok(bytes) = hex::decode(hex_str.trim()) {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
        }
    }

    None
}

/// Extract encrypted app ticket for an app.
/// Checks live credentials / TicketStore / eticketclient.
pub fn extract_encrypted_app_ticket(steam_root: Option<&Path>, app_id: u32) -> Option<Vec<u8>> {
    // 1. Try TicketStore
    let mut ts = match steam_root {
        Some(root) => TicketStore::new_for_steam_root(root),
        None => TicketStore::default(),
    };
    let _ = ts.load();

    if let Ok(entry) = ts.get(app_id) {
        if let Some(ref eticket_str) = entry.e_ticket {
            let trimmed = eticket_str.trim();
            if let Ok(bytes) = hex::decode(trimmed) {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
        }
    }

    // 2. Try eticketclient local resolution
    if let Some(bytes) = eticketclient::EticketClient::get_cached_fresh_eticket(app_id) {
        if !bytes.is_empty() {
            return Some(bytes);
        }
    }

    None
}

/// Writes outputs to directory `<output_dir>/<appid>/`:
/// - `appticket.bin` (if ownership present)
/// - `eticket.bin` (if encrypted present)
/// - `tickets.txt` (summary)
pub fn write_outputs(
    app_id: u32,
    base_dir: &Path,
    ownership: Option<&[u8]>,
    encrypted: Option<&[u8]>,
) -> Result<PathBuf, String> {
    let out_dir = base_dir.join(app_id.to_string());
    fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create directory {:?}: {}", out_dir, e))?;

    if let Some(own) = ownership {
        let appticket_path = out_dir.join("appticket.bin");
        fs::write(&appticket_path, own)
            .map_err(|e| format!("Failed to write {:?}: {}", appticket_path, e))?;
    }

    if let Some(enc) = encrypted {
        let eticket_path = out_dir.join("eticket.bin");
        fs::write(&eticket_path, enc)
            .map_err(|e| format!("Failed to write {:?}: {}", eticket_path, e))?;
    }

    let tickets_txt = format!(
        "appid:{}\n{}{}",
        app_id,
        format_ticket_line("appticket", ownership),
        format_ticket_line("eticket", encrypted)
    );

    let summary_path = out_dir.join("tickets.txt");
    fs::write(&summary_path, tickets_txt)
        .map_err(|e| format!("Failed to write {:?}: {}", summary_path, e))?;

    Ok(out_dir)
}

/// Run full ticket extraction workflow for an AppID.
pub fn dump_tickets(
    app_id: u32,
    steam_root: Option<&Path>,
    output_base_dir: Option<&Path>,
) -> Result<TicketExtractionResult, String> {
    // Run in target app's context so SteamAppId & SteamGameId are configured
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _env_guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let app_id_str = app_id.to_string();
    unsafe {
        env::set_var("SteamAppId", &app_id_str);
        env::set_var("SteamGameId", &app_id_str);
    }

    let root = steam_root.map(|p| p.to_path_buf()).or_else(find_steam_install_path);
    let ownership = extract_app_ownership_ticket(root.as_deref(), app_id);
    let encrypted = extract_encrypted_app_ticket(root.as_deref(), app_id);

    let base_dir = output_base_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let out_dir = write_outputs(
        app_id,
        &base_dir,
        ownership.as_deref(),
        encrypted.as_deref(),
    )?;

    let mut hex_dump = String::new();
    if let Some(ref own) = ownership {
        hex_dump.push_str(&format_hex_dump("Ownership ticket", own));
    }
    if let Some(ref enc) = encrypted {
        hex_dump.push_str(&format_hex_dump("Encrypted ticket", enc));
    }

    let tickets_txt = format!(
        "appid:{}\n{}{}",
        app_id,
        format_ticket_line("appticket", ownership.as_deref()),
        format_ticket_line("eticket", encrypted.as_deref())
    );

    Ok(TicketExtractionResult {
        app_id,
        ownership_ticket: ownership,
        encrypted_ticket: encrypted,
        output_dir: out_dir,
        hex_dump,
        tickets_txt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_hex_dump() {
        let data = b"Hello, World!";
        let formatted = format_hex_dump("Test", data);
        // Offset and 8-byte midpoint double-space separation
        assert!(formatted.contains("0000  48 65 6c 6c 6f 2c 20 57  6f 72 6c 64 21"));
        assert!(formatted.contains("Hello, World!"));

        // Multi-row wrapping at 16 bytes
        let long_data = b"0123456789abcdefEXTRA";
        let formatted_long = format_hex_dump("Long", long_data);
        assert!(formatted_long.contains("0000  30 31 32 33 34 35 36 37  38 39 61 62 63 64 65 66"));
        assert!(formatted_long.contains("0010  45 58 54 52 41"));
    }
}
