//! extract_tickets — Standalone ticket extraction CLI utility (BST-58).
//!
//! Usage:
//!     extract_tickets [AppID]
//!
//! If no AppID is specified on the command line, prompts:
//!     AppID:
//! and reads from standard input.
//!
//! Generates:
//! - Hex dump to stdout
//! - Output directory `<appid>/`:
//!   - `appticket.bin` (ownership ticket)
//!   - `eticket.bin` (encrypted ticket)
//!   - `tickets.txt` (summary)

use std::env;
use std::io::{self, Write};
use std::process;

use luatools_linux::steam_bridge::extract_tickets;

fn parse_app_id(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    trimmed.parse::<u32>().ok()
}

fn read_app_id_from_console() -> Option<u32> {
    print!("AppID: ");
    let _ = io::stdout().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return None;
    }
    parse_app_id(&input)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let app_id = if args.len() >= 2 {
        match parse_app_id(&args[1]) {
            Some(id) => id,
            None => {
                eprintln!("Invalid AppID: {}", args[1]);
                process::exit(1);
            }
        }
    } else {
        match read_app_id_from_console() {
            Some(id) => id,
            None => {
                eprintln!("Invalid AppID.");
                process::exit(1);
            }
        }
    };

    println!("AppID: {}", app_id);

    match extract_tickets::dump_tickets(app_id, None, None) {
        Ok(result) => {
            if !result.hex_dump.is_empty() {
                print!("{}", result.hex_dump);
            } else {
                println!("No cached tickets found for AppID {}", app_id);
            }
            println!("Wrote {}/", result.output_dir.display());
            process::exit(0);
        }
        Err(e) => {
            eprintln!("Error extracting tickets: {}", e);
            process::exit(1);
        }
    }
}
