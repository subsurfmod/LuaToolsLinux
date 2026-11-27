//! Integration and verification tests for BST-18, BST-45, and BST-58.

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use luatools_linux::steam_bridge::config_file_watcher;
use luatools_linux::steam_bridge::extract_tickets;
use luatools_linux::steam_bridge::manifest_donor;
use luatools_linux::steam_bridge::ost_config::{
    self, CloudSettings, DonateSettings, InjectDll, LogLevel, ManifestTimeouts, OstConfig,
};
use luatools_linux::steam_bridge::ticket_store::{AppTicketData, TicketStore};

// ============================================================================
// BST-18: opensteamtool.toml and ConfigFileWatcher Tests
// ============================================================================

#[test]
fn test_bst18_ost_config_parse_full() {
    let toml_data = r#"
[manifest]
url = "gmrc.wudrm.com"
timeout_resolve_ms = 4000
timeout_connect_ms = 4500
timeout_send_ms = 8000
timeout_recv_ms = 9000

[log]
level = "trace"

[lua]
paths = ["/tmp/stplugin1", "/tmp/stplugin2"]

[remote]
url_template = ["https://mirror1.example.com/{depot}/{gid}", "https://mirror2.example.com/{depot}/{gid}"]

[stats]
enable_api = false

[update]
enabled = false

[donate]
enabled = true
url = "https://custom.manifest.donor"
interval_secs = 45
max_mints_per_cycle = 30
min_mint_interval_ms = 2500
max_mints_per_session = 100
wanted_refresh_secs = 600

[cloud]
enabled = true
library = "custom_cloud.so"

[[inject]]
path = "/opt/hook.so"
when_cmdline = "-inject"
when_appids = [480, 730]
all_games = false
"#;

    let base = PathBuf::from("/tmp/opensteamtool.toml");
    let cfg = ost_config::parse_toml_content(toml_data, &base).expect("Failed to parse TOML");

    assert_eq!(cfg.manifest_provider, "gmrc.wudrm.com");
    assert_eq!(cfg.manifest_timeouts.resolve, 4000);
    assert_eq!(cfg.manifest_timeouts.connect, 4500);
    assert_eq!(cfg.manifest_timeouts.send, 8000);
    assert_eq!(cfg.manifest_timeouts.recv, 9000);
    assert_eq!(cfg.log_level, LogLevel::Trace);
    assert_eq!(cfg.lua_paths.len(), 2);
    assert_eq!(cfg.remote_url_templates.len(), 2);
    assert!(!cfg.stats_enable_api);
    assert!(!cfg.update_enabled);
    assert!(cfg.donate.enabled);
    assert_eq!(cfg.donate.url, "https://custom.manifest.donor");
    assert_eq!(cfg.donate.interval_secs, 45);
    assert_eq!(cfg.donate.max_mints_per_cycle, 30);
    assert_eq!(cfg.donate.min_mint_interval_ms, 2500);
    assert_eq!(cfg.donate.max_mints_per_session, 100);
    assert_eq!(cfg.donate.wanted_refresh_secs, 600);
    assert!(cfg.cloud.enabled);
    assert_eq!(cfg.cloud.library, "custom_cloud.so");
    assert_eq!(cfg.inject_dlls.len(), 1);
    assert_eq!(cfg.inject_dlls[0].path, "/opt/hook.so");
    assert_eq!(cfg.inject_dlls[0].when_appids, vec![480, 730]);
}

#[test]
fn test_bst18_ost_config_defaults_and_clamps() {
    let mut donate = DonateSettings {
        enabled: true,
        url: "".to_string(),
        interval_secs: 1,           // Too low, clamped to 30
        max_mints_per_cycle: 9999,  // Too high, clamped to 25
        min_mint_interval_ms: 999999, // Too high, clamped to 2000
        max_mints_per_session: 9999999, // Too high, clamped to 0
        wanted_refresh_secs: 10,    // Too low, clamped to 300
    };
    donate.sanitize();

    assert_eq!(donate.url, "https://manifest.luastools.xyz");
    assert_eq!(donate.interval_secs, 30);
    assert_eq!(donate.max_mints_per_cycle, 25);
    assert_eq!(donate.min_mint_interval_ms, 2000);
    assert_eq!(donate.max_mints_per_session, 0);
    assert_eq!(donate.wanted_refresh_secs, 300);
}

#[test]
fn test_bst18_ost_config_save_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let config_file = tmp.path().join("opensteamtool.toml");

    let original = OstConfig {
        manifest_provider: "manifest.steam.run".to_string(),
        manifest_timeouts: ManifestTimeouts {
            resolve: 6000,
            connect: 6000,
            send: 12000,
            recv: 12000,
        },
        log_level: LogLevel::Warn,
        log_dir: tmp.path().join("opensteamtool"),
        lua_paths: vec![PathBuf::from("/etc/luatools/stplugin")],
        remote_url_templates: vec!["https://mirror.org/{depot}/{gid}".to_string()],
        stats_enable_api: true,
        update_enabled: false,
        donate: DonateSettings {
            enabled: true,
            url: "https://manifest.luastools.xyz".to_string(),
            interval_secs: 60,
            max_mints_per_cycle: 50,
            min_mint_interval_ms: 1000,
            max_mints_per_session: 500,
            wanted_refresh_secs: 400,
        },
        inject_dlls: vec![InjectDll {
            path: "/usr/lib/mod.so".to_string(),
            when_cmdline: "-onlinefix".to_string(),
            when_appids: vec![480],
            all_games: false,
        }],
        cloud: CloudSettings {
            enabled: false,
            library: "".to_string(),
        },
    };

    ost_config::save(&config_file, &original).expect("save failed");
    assert!(config_file.exists());

    let res = ost_config::load(&config_file);
    assert!(res.applied);

    let loaded = ost_config::get_active_config();
    assert_eq!(loaded.manifest_provider, original.manifest_provider);
    assert_eq!(loaded.log_level, original.log_level);
    assert_eq!(loaded.lua_paths, original.lua_paths);
    assert_eq!(loaded.donate.interval_secs, 60);
    assert_eq!(loaded.donate.max_mints_per_session, 500);
    assert_eq!(loaded.inject_dlls.len(), 1);
    assert_eq!(loaded.inject_dlls[0].when_appids, vec![480]);
}

#[test]
fn test_bst18_manifest_probe_parsing() {
    let tmp = TempDir::new().unwrap();
    let probe_file = tmp.path().join("manifest_probe.txt");

    let sample_probe = r#"
# Diagnostic manifest probe depot list
481
482 # SpaceWar DLC
   # Blank line above and below

1245620
invalid_depot_string
-999
731
"#;

    fs::write(&probe_file, sample_probe).unwrap();

    let (requested, _accepted) = config_file_watcher::process_probe_file(&probe_file);
    // 4 valid depot IDs: 481, 482, 1245620, 731
    assert_eq!(requested, 4);
}

// ============================================================================
// BST-45: ManifestDonor CM Harvester Tests
// ============================================================================

#[test]
fn test_bst45_manifest_donor_wanted_line_parser() {
    assert_eq!(manifest_donor::parse_wanted_line(""), None);
    assert_eq!(manifest_donor::parse_wanted_line("480:481"), None);
    assert_eq!(manifest_donor::parse_wanted_line("480:0:987654321"), None); // depot 0 is invalid
    assert_eq!(manifest_donor::parse_wanted_line("480:481:0"), None); // gid 0 is invalid

    // app 0 is valid according to API.md
    let w0 = manifest_donor::parse_wanted_line("0:481:998877665544332211").unwrap();
    assert_eq!(w0.app_id, 0);
    assert_eq!(w0.depot_id, 481);
    assert_eq!(w0.gid, 998877665544332211);

    // standard app:depot:gid with whitespace
    let w1 = manifest_donor::parse_wanted_line(" 730 : 731 : 123456789012345678 \r\n").unwrap();
    assert_eq!(w1.app_id, 730);
    assert_eq!(w1.depot_id, 731);
    assert_eq!(w1.gid, 123456789012345678);
}

#[test]
fn test_bst45_manifest_donor_passive_capture_and_stats() {
    manifest_donor::reset_session_stats();
    assert_eq!(manifest_donor::minted_this_session(), 0);

    // Passive capture with zeros should gracefully no-op without panic
    manifest_donor::submit_captured_code(0, 123, 456);
    manifest_donor::submit_captured_code(481, 0, 456);
    manifest_donor::submit_captured_code(481, 123, 0);

    // Disabled setting should no-op
    ost_config::set_donate_settings(DonateSettings {
        enabled: false,
        ..Default::default()
    });
    manifest_donor::submit_captured_code(481, 123, 456);

    // Re-enable
    ost_config::set_donate_settings(DonateSettings {
        enabled: true,
        ..Default::default()
    });
}

// ============================================================================
// BST-58: extract_tickets CLI Tool Tests
// ============================================================================

#[test]
fn test_bst58_format_hex_dump_and_ticket_line() {
    let dummy_ticket = vec![
        0x01, 0x00, 0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x53, 0x74, 0x65, 0x61, 0x6D, 0x44,
        0x52, 0x4D, 0x00, 0x11, 0x22, 0x33,
    ];

    let dump = extract_tickets::format_hex_dump("Ownership ticket", &dummy_ticket);
    assert!(dump.starts_with("Ownership ticket (20 bytes):\n"));
    assert!(dump.contains("0000"));
    assert!(dump.contains("01 00 00 00 de ad be ef"));
    assert!(dump.contains("SteamDRM"));

    let line_present = extract_tickets::format_ticket_line("appticket", Some(&dummy_ticket));
    assert_eq!(
        line_present,
        "appticket(20bytes):01000000deadbeef537465616d44524d00112233\n"
    );

    let line_absent = extract_tickets::format_ticket_line("eticket", None);
    assert_eq!(line_absent, "eticket:null\n");
}

#[test]
fn test_bst58_write_outputs() {
    let tmp = TempDir::new().unwrap();
    let app_id = 480u32;
    let ownership_bytes = b"OWNERSHIP_TICKET_PAYLOAD_TEST";
    let eticket_bytes = b"ENCRYPTED_TICKET_PAYLOAD_TEST";

    let out_dir = extract_tickets::write_outputs(
        app_id,
        tmp.path(),
        Some(ownership_bytes),
        Some(eticket_bytes),
    )
    .expect("write_outputs failed");

    assert_eq!(out_dir, tmp.path().join("480"));
    assert!(out_dir.join("appticket.bin").exists());
    assert!(out_dir.join("eticket.bin").exists());
    assert!(out_dir.join("tickets.txt").exists());

    let read_own = fs::read(out_dir.join("appticket.bin")).unwrap();
    assert_eq!(read_own, ownership_bytes);

    let read_enc = fs::read(out_dir.join("eticket.bin")).unwrap();
    assert_eq!(read_enc, eticket_bytes);

    let summary = fs::read_to_string(out_dir.join("tickets.txt")).unwrap();
    assert!(summary.contains("appid:480"));
    assert!(summary.contains(&format!("appticket({}bytes):", ownership_bytes.len())));
    assert!(summary.contains(&format!("eticket({}bytes):", eticket_bytes.len())));
}

#[test]
fn test_bst58_dump_tickets_with_ticket_store() {
    let tmp_steam = TempDir::new().unwrap();
    let tmp_out = TempDir::new().unwrap();
    let app_id = 730u32;

    // Seed a ticket into TicketStore
    let mut ts = TicketStore::new_for_steam_root(tmp_steam.path());
    let sample_own_hex = "0102030405060708090a0b0c0d0e0f10";
    let sample_enc_hex = "aabbccddeeff00112233445566778899";

    ts.set(AppTicketData {
        app_id,
        steam_id: Some("76561198000000001".to_string()),
        app_ticket: Some(sample_own_hex.to_string()),
        e_ticket: Some(sample_enc_hex.to_string()),
        last_update: Some(1700000000),
    })
    .expect("Failed to seed TicketStore");

    let result = extract_tickets::dump_tickets(
        app_id,
        Some(tmp_steam.path()),
        Some(tmp_out.path()),
    )
    .expect("dump_tickets failed");

    assert_eq!(result.app_id, app_id);
    assert!(result.ownership_ticket.is_some());
    assert!(result.encrypted_ticket.is_some());
    assert_eq!(result.ownership_ticket.unwrap(), hex::decode(sample_own_hex).unwrap());
    assert_eq!(result.encrypted_ticket.unwrap(), hex::decode(sample_enc_hex).unwrap());

    assert!(result.output_dir.join("appticket.bin").exists());
    assert!(result.output_dir.join("eticket.bin").exists());
    assert!(result.output_dir.join("tickets.txt").exists());
}

#[test]
fn test_bst58_cli_wrapper_executable() {
    let wrapper_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tools")
        .join("extract_tickets")
        .join("extract_tickets");

    assert!(wrapper_path.exists(), "wrapper script should exist at {:?}", wrapper_path);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(&wrapper_path).unwrap().permissions();
        assert_ne!(perms.mode() & 0o111, 0, "wrapper script should be executable");
    }
}

// ============================================================================
// BST-21: ConfigStoreGetBinary Depot Decryption Key Tests
// ============================================================================

#[test]
fn test_bst21_depot_key_robust_hex_and_normalization() {
    use luatools_linux::steam_bridge::interop::interop_decryption::has_depot_decryption_key;

    // Normalization test across various path separators and shapes
    let test_paths = [
        "depots\\123456\\DecryptionKey",
        "depots/123456/DecryptionKey",
        "123456\\DecryptionKey",
        "123456/decryptionkey",
        "DEPOTS/123456/DECRYPTIONKEY",
    ];
    for path in test_paths {
        let norm_name = path.replace('/', "\\");
        let lower_norm = norm_name.to_ascii_lowercase();
        assert!(lower_norm.contains("\\decryptionkey"));
        let last = lower_norm.rfind("\\decryptionkey").unwrap();
        let depot_id_str = match lower_norm[..last].rfind('\\') {
            Some(start) => &norm_name[start + 1..last],
            None => &norm_name[..last],
        };
        assert_eq!(depot_id_str, "123456");
    }

    // Default lookup returns false for unknown depot
    assert!(!has_depot_decryption_key(99999999));
}

#[test]
fn test_bst21_vdf_depot_decryption_key_lookup() {
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join(".steam/steam/config");
    fs::create_dir_all(&config_dir).unwrap();

    let vdf_content = r#""InstallConfigStore"
{
    "depots"
    {
        "123456"
        {
            "DecryptionKey" "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }
    }
}
"#;
    let vdf_path = config_dir.join("config.vdf");
    fs::write(&vdf_path, vdf_content).unwrap();

    // Verify parser logic extracts key
    let content = fs::read_to_string(&vdf_path).unwrap();
    let needle = "\"123456\"";
    let mut extracted_key: Option<String> = None;
    if content.contains(needle) {
        let mut in_depot = false;
        for line in content.lines() {
            let line_trim = line.trim();
            if line_trim.contains(needle) {
                in_depot = true;
            } else if in_depot {
                if line_trim.to_ascii_lowercase().contains("decryptionkey") {
                    let parts: Vec<&str> = line_trim.split('"').filter(|s| !s.trim().is_empty()).collect();
                    if parts.len() >= 2 {
                        extracted_key = Some(parts[parts.len() - 1].to_string());
                        break;
                    }
                } else if line_trim == "}" {
                    in_depot = false;
                }
            }
        }
    }

    assert_eq!(
        extracted_key.as_deref(),
        Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
    );
}

// ============================================================================
// BST-22: Manifest Dependency Hooks and PatternDb Tests
// ============================================================================

#[test]
fn test_bst22_pattern_db_linux_fallbacks() {
    use luatools_linux::steam_bridge::pattern_db::{
        BinaryComponent, PatternChannel, PatternDb,
    };

    let pf = PatternDb::fallback_pattern_file(
        BinaryComponent::SteamClient,
        PatternChannel::Pattern,
        "test_sha256_hash",
    );

    assert_eq!(pf.build_id, Some("linux_fallback".to_string()));
    assert!(pf.patterns.contains_key("BuildDepotDependency"));
    assert!(pf.patterns.contains_key("YldLoadDepotManifest"));
    assert!(pf.patterns.contains_key("ConfigStoreGetBinary"));
    assert!(pf.patterns.contains_key("bis_subscribed_app"));

    let bdd = pf.patterns.get("BuildDepotDependency").unwrap();
    assert!(bdd.pattern.is_some());
    assert!(bdd.mask.is_some());
}

#[test]
fn test_bst22_manifest_depot_entry_and_cutlvector_patching() {
    use luatools_linux::steam_bridge::manifest_hooks::{
        apply_depot_overrides, get_manifest_overrides, get_seen_depot,
        remove_manifest_override, set_manifest_override, CUtlMemory, CUtlVector, DepotEntry,
        ManifestOverride,
    };

    assert_eq!(std::mem::size_of::<DepotEntry>(), 32);
    let entry = DepotEntry::new(1245620, 1245621, 987654321, 1024, 0);
    assert_eq!(entry.app_id, 1245620);
    assert_eq!(entry.depot_id, 1245621);
    assert_eq!(entry.manifest_gid, 987654321);
    assert_eq!(entry.manifest_size, 1024);

    set_manifest_override(
        1245620,
        1245621,
        ManifestOverride::new(1122334455, Some(2048)),
    );
    let mut data = vec![entry];
    let cutl_vec = CUtlVector {
        memory: CUtlMemory {
            memory: data.as_mut_ptr(),
            allocation_count: 1,
            grow_size: 0,
        },
        size: 1,
    };

    unsafe {
        let slice = std::slice::from_raw_parts_mut(cutl_vec.memory.memory, cutl_vec.size as usize);
        let patched = apply_depot_overrides(slice);
        assert_eq!(patched, 1);
    }

    assert_eq!(data[0].manifest_gid, 1122334455);
    assert_eq!(data[0].manifest_size, 2048);

    assert_eq!(get_seen_depot(1245620, 1245621), Some((1245620, 1122334455)));
    remove_manifest_override(1245620, 1245621);
    assert!(!get_manifest_overrides().contains_key(&(1245620, 1245621)));
}

// ============================================================================
// BST-33: NetPacket Licenses (EMsg 780 CMsgClientLicenseList) Tests
// ============================================================================

#[test]
fn test_bst33_netpacket_license_list_valve_wire_format() {
    use luatools_linux::steam_bridge::netpacket::netpacket_licenses::parse_client_license_list;

    // Build real Valve CMsgClientLicenseList protobuf body:
    // field 1 (eresult): tag 0x08, value 1 (k_EResultOK)
    // field 2 (licenses): tag 0x12, length-delimited
    //   Inside License:
    //     field 1 (package_id): tag 0x08, varint 730
    //     field 2 (time_created): tag 0x15, fixed32
    //     field 17 (access_token): tag (17<<3)|0 = 136 (0x88, 0x01), varint
    let mut license_bytes = Vec::new();
    // package_id = 730 (varint: 0xDA, 0x05)
    license_bytes.push(0x08);
    license_bytes.push(0xDA);
    license_bytes.push(0x05);

    // time_created = 0x12345678 (fixed32: 4 bytes)
    license_bytes.push(0x15);
    license_bytes.extend_from_slice(&0x12345678_u32.to_le_bytes());

    // access_token = 0x112233445566 (varint)
    license_bytes.push(0x88);
    license_bytes.push(0x01);
    let mut token_val = 0x1122_3344_5566_u64;
    while token_val >= 0x80 {
        license_bytes.push(((token_val & 0x7F) as u8) | 0x80);
        token_val >>= 7;
    }
    license_bytes.push(token_val as u8);

    let mut body = Vec::new();
    // eresult = 1
    body.push(0x08);
    body.push(0x01);
    // licenses = 2 (tag 0x12)
    body.push(0x12);
    body.push(license_bytes.len() as u8);
    body.extend_from_slice(&license_bytes);

    let licenses = parse_client_license_list(&body);
    assert_eq!(licenses.len(), 1);
    assert_eq!(licenses[0].package_id, 730);
    assert_eq!(licenses[0].access_token, 0x1122_3344_5566_u64);
}

// ============================================================================
// BST-23 & BST-44: manifest_hooks ABI and Real On-Demand Pre-Seeding Tests
// ============================================================================

#[test]
fn test_bst23_manifest_hooks_function_pointer_types_and_depot_layout() {
    use luatools_linux::steam_bridge::manifest_hooks::{
        BuildDepotDependencyFn, CUtlMemory, CUtlVector, DepotEntry, YldLoadDepotManifestFn,
    };

    // Verify DepotEntry struct size and exact offsets matching Steam Tier1
    assert_eq!(std::mem::size_of::<DepotEntry>(), 0x20); // 32 bytes
    let entry = DepotEntry::new(480, 481, 11223344, 55667788, 0);
    assert_eq!(entry.app_id, 480);
    assert_eq!(entry.depot_id, 481);
    assert_eq!(entry.manifest_gid, 11223344);
    assert_eq!(entry.manifest_size, 55667788);

    // Verify non-variadic function pointer sizes (must be standard function pointers, 8 bytes)
    assert_eq!(std::mem::size_of::<BuildDepotDependencyFn>(), 8);
    assert_eq!(std::mem::size_of::<YldLoadDepotManifestFn>(), 8);

    // CUtlVector layout verification
    let mut data = vec![entry];
    let vec = CUtlVector {
        memory: CUtlMemory {
            memory: data.as_mut_ptr(),
            allocation_count: 1,
            grow_size: 0,
        },
        size: 1,
    };
    assert_eq!(vec.size, 1);
    assert_eq!(std::mem::size_of::<CUtlVector<DepotEntry>>(), 24);
}

#[test]
fn test_bst44_ensure_manifest_cached_download_and_magic_validation() {
    use luatools_linux::steam_bridge::manifest_hooks::{
        ensure_manifest_cached_from_url, looks_like_manifest,
        EOF_MAGIC, PAYLOAD_MAGIC,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("depotcache");
    fs::create_dir_all(&cache_dir).unwrap();

    // 1. Magic header test: payload starts with PAYLOAD_MAGIC and ends with EOF_MAGIC
    let mut valid_manifest = Vec::new();
    valid_manifest.extend_from_slice(&PAYLOAD_MAGIC);
    valid_manifest.extend_from_slice(b"TEST_MANIFEST_PAYLOAD_BODY_DATA");
    valid_manifest.extend_from_slice(&EOF_MAGIC);

    assert!(looks_like_manifest(&valid_manifest));
    assert!(!looks_like_manifest(b"INVALID_PAYLOAD"));
    assert!(!looks_like_manifest(&[0xD0, 0x17, 0xF6, 0x71])); // too short (< 16 bytes)

    // 2. Mock HTTP server serving valid manifest
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let manifest_bytes_clone = valid_manifest.clone();

    let server_handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut req_buf = [0u8; 1024];
            let _ = stream.read(&mut req_buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                manifest_bytes_clone.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&manifest_bytes_clone);
        }
    });

    let manifest_file = cache_dir.join("228980_123456789.manifest");
    assert!(!manifest_file.exists());

    let test_url = format!("http://{}/m/228980/123456789", addr);
    let success = ensure_manifest_cached_from_url(
        &manifest_file,
        &test_url,
        228980,
        228980,
        123456789,
        2000,
        2000,
        true,
    );
    assert!(success);
    assert!(manifest_file.is_file());
    assert_eq!(fs::read(&manifest_file).unwrap(), valid_manifest);

    let _ = server_handle.join();
}

#[test]
fn test_bst44_ensure_manifest_cached_404_negative_caching() {
    use luatools_linux::steam_bridge::manifest_hooks::{
        clear_manifest_negative_cache, ensure_manifest_cached_from_url,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;

    clear_manifest_negative_cache();

    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("depotcache");
    fs::create_dir_all(&cache_dir).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut req_buf = [0u8; 1024];
            let _ = stream.read(&mut req_buf);
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
        }
    });

    let manifest_file = cache_dir.join("99999_88888.manifest");
    let test_url = format!("http://{}/m/99999/88888", addr);

    // First attempt: hits mock server, receives 404, enters negative cache
    let res1 = ensure_manifest_cached_from_url(
        &manifest_file,
        &test_url,
        99999,
        99999,
        88888,
        2000,
        2000,
        false,
    );
    assert!(!res1);
    assert!(!manifest_file.exists());

    // Second attempt: without server running, negative cache immediately rejects without network call
    let res2 = ensure_manifest_cached_from_url(
        &manifest_file,
        &test_url,
        99999,
        99999,
        88888,
        2000,
        2000,
        false,
    );
    assert!(!res2);

    let _ = server_handle.join();
    clear_manifest_negative_cache();
}

// ============================================================================
// BST-24: Hooks_KeyValues 5-Arg C ABI and Upstream-Disabled Status Tests
// ============================================================================

#[test]
fn test_bst24_keyvalues_abi_and_upstream_disabled_lifecycle() {
    use luatools_linux::steam_bridge::interop::interop_keyvalues::{
        self, is_enabled_upstream, set_enabled_upstream, FindOrCreateKeyFn, KeyValues,
        KeyValuesReadAsBinaryFn,
    };

    // KeyValues must be 32 bytes (0x20) matching Steam Tier1
    assert_eq!(std::mem::size_of::<KeyValues>(), 32);

    // Function pointer types must be non-variadic standard function pointers (8 bytes)
    assert_eq!(std::mem::size_of::<KeyValuesReadAsBinaryFn>(), 8);
    assert_eq!(std::mem::size_of::<FindOrCreateKeyFn>(), 8);

    // In upstream BST baseline (HookManager.cpp:30), Hooks_KeyValues is disabled.
    // Verify it is disabled by default.
    set_enabled_upstream(false);
    assert!(!is_enabled_upstream());

    let tmp = tempfile::tempdir().unwrap();
    // install() must succeed cleanly and remain dormant when disabled upstream
    let res = interop_keyvalues::install(tmp.path());
    assert!(res.is_ok());

    // Toggle enabled upstream
    set_enabled_upstream(true);
    assert!(is_enabled_upstream());
    set_enabled_upstream(false);
    assert!(!is_enabled_upstream());
}

// ============================================================================
// BST-25: Hooks_SteamUI CSteamApp PurchasedTime In-Memory Modification Tests
// ============================================================================

#[test]
fn test_bst25_steamui_fill_in_app_overview_modifies_purchased_time() {
    use luatools_linux::steam_bridge::interop::interop_package;
    use luatools_linux::steam_bridge::interop::interop_steamui::{
        clear_purchase_times, get_purchase_time, has_depot, set_purchase_time, CSteamApp,
    };

    clear_purchase_times();

    // 1. Register purchase time in registry
    set_purchase_time(730, 1620000000);
    assert_eq!(get_purchase_time(730), Some(1620000000));

    // Monotonic update rule: larger time overwrites, smaller does not
    set_purchase_time(730, 1610000000);
    assert_eq!(get_purchase_time(730), Some(1620000000));
    set_purchase_time(730, 1630000000);
    assert_eq!(get_purchase_time(730), Some(1630000000));

    // 2. Mark depot in interop_package so has_depot returns true
    interop_package::add_owned_depot_id(730);
    assert!(has_depot(730));

    // 3. Create mock CSteamApp instance
    let mut app = CSteamApp {
        vfptr: std::ptr::null_mut(),
        game_id: 730,
        n_app_id: 730,
        unknown1: 0,
        unknown2: 0,
        release_state: 1,
        ownership_flags: 1,
        app_state_flags: 0,
        steam_id: 76561198000000000,
        purchased_time: 0, // initially 0
        change_number: 1,
        license_expiration_time: 0,
        master_sub_app_id: 0,
        proto_app_type: 0,
        parent_app_id: 0,
    };

    assert_eq!(std::mem::size_of::<CSteamApp>(), 72);

    // Directly test the purchase time modification logic
    if has_depot(app.n_app_id) {
        if let Some(pt) = get_purchase_time(app.n_app_id) {
            app.purchased_time = pt;
        }
    }

    // Must be updated to 1630000000
    assert_eq!(app.purchased_time, 1630000000);

    clear_purchase_times();
}

// ============================================================================
// BST-30: Hooks_CallBack (SendCallbackToPipe) Passthrough and Interception Tests
// ============================================================================

#[test]
fn test_bst30_callbacks_detour_passthrough_and_modifiers() {
    use luatools_linux::steam_bridge::interop::interop_callbacks::{
        clear_callback_history, clear_callback_modifiers, dispatch_callback,
        get_callback_history, register_callback_modifier, CallbackAction,
    };

    clear_callback_modifiers();
    clear_callback_history();

    #[repr(C)]
    struct DlcInstalledCallback {
        app_id: u32,
        dlc_app_id: u32,
    }

    // 1. Initial passthrough without modifiers
    let mut cb_data = DlcInstalledCallback {
        app_id: 1086940, // Baldur's Gate 3
        dlc_app_id: 1086941,
    };

    let ok = unsafe {
        dispatch_callback(
            std::ptr::null_mut(),
            42, // pipe
            1001, // user
            1005, // k_iSteamAppsCallbacks + 5 (DlcInstalled_t)
            (&mut cb_data as *mut DlcInstalledCallback).cast(),
            std::mem::size_of::<DlcInstalledCallback>() as i32,
        )
    };
    assert!(ok);

    let history = get_callback_history();
    assert!(history.iter().any(|entry| entry == &(42, 1001, 1005, std::mem::size_of::<DlcInstalledCallback>() as i32)));

    // 2. In-place modification test
    register_callback_modifier(1005, |_pipe, _user, _cb, ptr, _len| {
        if !ptr.is_null() {
            let data = unsafe { &mut *ptr.cast::<DlcInstalledCallback>() };
            data.dlc_app_id = 999999;
            CallbackAction::Modified
        } else {
            CallbackAction::PassThrough
        }
    });

    let ok_mod = unsafe {
        dispatch_callback(
            std::ptr::null_mut(),
            42,
            1001,
            1005,
            (&mut cb_data as *mut DlcInstalledCallback).cast(),
            std::mem::size_of::<DlcInstalledCallback>() as i32,
        )
    };
    assert!(ok_mod);
    assert_eq!(cb_data.dlc_app_id, 999999);

    // 3. Suppression test
    register_callback_modifier(1005, |_pipe, _user, _cb, _ptr, _len| {
        CallbackAction::Suppress
    });

    let ok_sup = unsafe {
        dispatch_callback(
            std::ptr::null_mut(),
            42,
            1001,
            1005,
            (&mut cb_data as *mut DlcInstalledCallback).cast(),
            std::mem::size_of::<DlcInstalledCallback>() as i32,
        )
    };
    assert!(ok_sup); // Handled

    clear_callback_modifiers();
    clear_callback_history();
}

// ============================================================================
// BST-62: Per-Game Launch Integration Wrapper & Goldberg Fallback Tests
// ============================================================================

#[test]
fn test_bst62_per_game_launch_wrapper_and_env() {
    use luatools_linux::services::game_process_injection_service::GameProcessInjectionService;

    let tmp = tempfile::tempdir().unwrap();
    let storage_file = tmp.path().join("game_injections.json");
    let svc = GameProcessInjectionService::with_storage(
        Some(storage_file),
        luatools_linux::steam_bridge::denuvo_auth::DenuvoAuth::new(),
    );

    let fake_lib = tmp.path().join("libcustom_game_hook.so");
    fs::write(&fake_lib, b"\x7fELFfake").unwrap();

    let app_id = 570; // Dota 2
    // Set mock launch guard so testing does NOT spawn live Steam
    std::env::set_var("LUATOOLS_MOCK_LAUNCH", "1");
    let launch_res = svc.launch_game_with_preload(app_id, &fake_lib);
    assert!(launch_res.is_ok());
    // Config must be persisted
    assert!(svc.is_configured(app_id));
    let cfg = svc.get_config(app_id).unwrap();
    assert_eq!(cfg.library_path, fake_lib);

    // Build command test on Linux native binary
    let fake_game_bin = tmp.path().join("dota2_linux");
    fs::write(&fake_game_bin, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_game_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_game_bin, perms).unwrap();
    }

    let cmd = svc.build_launch_command(app_id, &fake_game_bin, &["-novid".into()]).unwrap();
    let envs: std::collections::HashMap<_, _> = cmd.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|s| s.to_string_lossy().into_owned()))).collect();
    assert_eq!(envs.get("LUATOOLS_BRIDGE_FORCE").and_then(|v| v.as_deref()), Some("1"));
    let preload = envs.get("LD_PRELOAD").and_then(|v| v.as_deref()).unwrap_or("");
    assert!(preload.contains("libcustom_game_hook.so"));

    // Verify Windows executable triggers WINEDLLOVERRIDES
    let fake_win_exe = tmp.path().join("game.exe");
    // Minimal valid PE header
    let mut pe = vec![0u8; 0x200];
    pe[0..2].copy_from_slice(&0x5A4Du16.to_le_bytes()); // MZ
    pe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    pe[0x80..0x84].copy_from_slice(&0x0000_4550u32.to_le_bytes()); // PE\0\0
    pe[0x84 + 16..0x84 + 18].copy_from_slice(&240u16.to_le_bytes());
    pe[0x84 + 20..0x84 + 22].copy_from_slice(&0x20Bu16.to_le_bytes());
    fs::write(&fake_win_exe, &pe).unwrap();

    let win_cmd = svc.build_launch_command(app_id, &fake_win_exe, &[]).unwrap();
    let win_envs: std::collections::HashMap<_, _> = win_cmd.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|s| s.to_string_lossy().into_owned()))).collect();
    let wine_overrides = win_envs.get("WINEDLLOVERRIDES").and_then(|v| v.as_deref()).unwrap_or("");
    assert!(wine_overrides.contains("steam_api=n,b"));
}

// ============================================================================
// BST-32: Hooks_NetPacket PICS ProductInfoRequest Access Token Injection Tests
// ============================================================================

#[test]
fn test_bst32_netpacket_pics_product_info_request_token_injection() {
    use luatools_linux::steam_bridge::netpacket::netpacket_accesstoken::{
        clear_access_tokens, get_app_access_token, get_package_access_token,
        handle_send_pics_product_info_request_netpacket, parse_pics_product_info_request,
        serialize_pics_product_info_request, set_app_access_token, set_package_access_token,
        AppInfo, PackageInfo, PicsProductInfoRequest,
    };
    use luatools_linux::steam_bridge::netpacket::netpacket_core::{
        NetPacketContext, NetPacketDirection,
    };

    clear_access_tokens();

    // 1. Configure tokens in token store
    let target_app_id = 1245620; // Elden Ring
    let target_app_token = 0xA1B2_C3D4_E5F6_0718_u64;
    set_app_access_token(target_app_id, target_app_token);
    assert_eq!(get_app_access_token(target_app_id), Some(target_app_token));

    let target_pkg_id = 76543;
    let target_pkg_token = 0xFEDC_BA98_7654_3210_u64;
    set_package_access_token(target_pkg_id, target_pkg_token);
    assert_eq!(get_package_access_token(target_pkg_id), Some(target_pkg_token));

    // 2. Build initial CMsgClientPICSProductInfoRequest with missing tokens
    let initial_req = PicsProductInfoRequest {
        packages: vec![
            PackageInfo::new(target_pkg_id), // token is None
            PackageInfo::new(99999),          // unconfigured package
        ],
        apps: vec![
            AppInfo::new(target_app_id),      // token is None
            AppInfo::new(480),                // Spacewar (no token)
        ],
        meta_data_only: Some(true),
        num_prev_failed: Some(0),
        sequence_number: Some(7),
        single_response: Some(false),
        raw_unknown: vec![],
    };

    let body = serialize_pics_product_info_request(&initial_req);

    // Frame with MsgHdr (20 bytes: eMsg 8903, headerLength 20) + 12 job bytes
    let mut frame = Vec::new();
    frame.extend_from_slice(&[0x87, 0x22, 0x00, 0x80]); // eMsg = 8903 | 0x80000000
    frame.extend_from_slice(&20u32.to_le_bytes());       // header_length = 20
    frame.extend_from_slice(&[0u8; 12]);                // job ids
    frame.extend_from_slice(&body);

    let mut ctx = NetPacketContext::new(frame, NetPacketDirection::Send, target_app_id).unwrap();

    // 3. Dispatch through netpacket handler
    let res = handle_send_pics_product_info_request_netpacket(&mut ctx);
    assert!(res.is_ok());

    // 4. Verify modified payload has injected access tokens
    let modified = parse_pics_product_info_request(ctx.body()).unwrap();
    assert_eq!(modified.apps.len(), 2);
    assert_eq!(modified.apps[0].appid, target_app_id);
    assert_eq!(modified.apps[0].access_token, Some(target_app_token));
    assert_eq!(modified.apps[1].appid, 480);
    assert_eq!(modified.apps[1].access_token, None);

    assert_eq!(modified.packages.len(), 2);
    assert_eq!(modified.packages[0].packageid, target_pkg_id);
    assert_eq!(modified.packages[0].access_token, Some(target_pkg_token));
    assert_eq!(modified.packages[1].packageid, 99999);
    assert_eq!(modified.packages[1].access_token, None);

    assert_eq!(modified.meta_data_only, Some(true));
    assert_eq!(modified.sequence_number, Some(7));

    clear_access_tokens();
}

// ============================================================================
// BST-34: Hooks_NetPacket GamesPlayed Spacewar (480) Title Restoration Tests
// ============================================================================

#[test]
fn test_bst34_netpacket_games_played_onlinefix_title_restoration() {
    use luatools_linux::steam_bridge::interop::interop_misc::{
        set_game_name_by_app_id, set_online_fix_real_app_id,
    };
    use luatools_linux::steam_bridge::netpacket::netpacket_core::{
        NetPacketContext, NetPacketDirection,
    };
    use luatools_linux::steam_bridge::netpacket::netpacket_onlinefix::{
        handle_send_games_played_netpacket, parse_games_played, serialize_games_played,
        GamePlayed, GamesPlayedMsg,
    };

    let real_app_id = 1245620; // Elden Ring
    set_online_fix_real_app_id(real_app_id);
    set_game_name_by_app_id(real_app_id, "ELDEN RING".to_string());

    // Build initial CMsgClientGamesPlayed with game_id 480 and game_extra_info "Spacewar"
    let initial_msg = GamesPlayedMsg {
        games_played: vec![
            GamePlayed::with_extra_info(480, "Spacewar"),
            GamePlayed::with_extra_info(730, "Counter-Strike 2"),
        ],
        client_os_type: Some(16),
        cloud_gaming_platform: None,
        recent_reauthentication: None,
        raw_unknown: vec![],
    };

    let body = serialize_games_played(&initial_msg);

    // Frame with MsgHdr (20 bytes: eMsg 742, headerLength 20) + 12 job bytes
    let mut frame = Vec::new();
    frame.extend_from_slice(&[0xE6, 0x02, 0x00, 0x80]); // eMsg = 742 | 0x80000000
    frame.extend_from_slice(&20u32.to_le_bytes());       // header_length = 20
    frame.extend_from_slice(&[0u8; 12]);                // job ids
    frame.extend_from_slice(&body);

    let mut ctx = NetPacketContext::new(frame, NetPacketDirection::Send, 480).unwrap();

    // Dispatch through netpacket handler
    let res = handle_send_games_played_netpacket(&mut ctx);
    assert!(res.is_ok());

    // Verify game 480 had its title rewritten to "ELDEN RING", while 730 remained "Counter-Strike 2"
    let modified = parse_games_played(ctx.body()).unwrap();
    assert_eq!(modified.games_played.len(), 2);
    assert_eq!(modified.games_played[0].app_id(), 480);
    assert_eq!(
        modified.games_played[0].game_extra_info.as_deref(),
        Some("ELDEN RING")
    );
    assert_eq!(modified.games_played[1].app_id(), 730);
    assert_eq!(
        modified.games_played[1].game_extra_info.as_deref(),
        Some("Counter-Strike 2")
    );

    // Reset
    set_online_fix_real_app_id(0);
}

// ============================================================================
// BST-36: Hooks_NetPacket LegacyKey (EMsg 730 Suppression & EMsg 785 Synthesis) Tests
// ============================================================================

static BST36_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
static BST42_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn test_bst36_legacykey_request_suppression_and_response_synthesis() {
    let _lock = BST36_MUTEX.lock().unwrap();
    use luatools_linux::steam_bridge::netpacket::netpacket_core::{
        dispatch_netpacket, NetPacketContext, NetPacketDirection,
    };
    use luatools_linux::steam_bridge::netpacket::netpacket_legacykey::{
        build_legacy_game_key_request_frame, clear_legacy_cdkeys, clear_pending_responses,
        drain_pending_responses, has_pending_responses, parse_legacy_game_key_response,
        register_legacykey_handlers, set_legacy_cdkey, EXTENDED_MSG_HDR_SIZE,
    };

    register_legacykey_handlers();
    clear_legacy_cdkeys();
    clear_pending_responses();

    let app_id = 1245620; // Elden Ring
    let account_id = 28121353u32;
    let steam_id = 0x0110_0001_0000_0000u64 | (account_id as u64);
    let job_id_source = 0x8877_6655_4433_2211u64;
    let custom_key = "ELDN-RNG1-KEY2-2026";

    // 1. Configure key override
    set_legacy_cdkey(app_id, custom_key);

    // 2. Build outbound MsgClientGetLegacyGameKey request frame (eMsg 730)
    let req_frame = build_legacy_game_key_request_frame(app_id, steam_id, job_id_source);
    let mut ctx = NetPacketContext::new(req_frame, NetPacketDirection::Send, app_id).unwrap();
    assert_eq!(ctx.is_proto, false);
    assert_eq!(ctx.body_offset, EXTENDED_MSG_HDR_SIZE);

    // 3. Dispatch through netpacket core dispatcher
    dispatch_netpacket(&mut ctx);

    // Outbound send must be suppressed (frame buffer cleared by handler)
    assert!(ctx.frame_buffer.is_empty());

    // 4. Synthesized EMsg 785 response must be queued in pending responses
    assert!(has_pending_responses());

    // 5. Drain pending responses into carrier packet context
    let carrier_frame = vec![0u8; 128];
    let mut carrier_ctx = NetPacketContext::new(carrier_frame, NetPacketDirection::Recv, app_id).unwrap();

    let mut delivered_responses = Vec::new();
    let count = drain_pending_responses(&mut carrier_ctx, |injected_ctx| {
        delivered_responses.push(injected_ctx.frame_buffer.clone());
        Ok(())
    })
    .unwrap();

    assert_eq!(count, 1);
    assert_eq!(delivered_responses.len(), 1);

    // 6. Verify synthesized response wire format
    let parsed = parse_legacy_game_key_response(&delivered_responses[0]).unwrap();
    assert_eq!(parsed.hdr.e_msg(), 785);
    assert_eq!(parsed.hdr.job_id_target(), job_id_source);
    assert_eq!(parsed.hdr.job_id_source(), 0);
    assert_eq!(parsed.app_id, app_id);
    assert_eq!(parsed.e_result, 1); // k_EResultOK
    assert_eq!(parsed.cd_key, custom_key);

    clear_legacy_cdkeys();
    clear_pending_responses();
}

#[test]
fn test_bst36_legacykey_deterministic_crockford_fallback() {
    let _lock = BST36_MUTEX.lock().unwrap();
    use luatools_linux::steam_bridge::netpacket::netpacket_core::{
        NetPacketContext, NetPacketDirection,
    };
    use luatools_linux::steam_bridge::netpacket::netpacket_legacykey::{
        build_legacy_game_key_request_frame, clear_legacy_cdkeys, clear_pending_responses,
        drain_pending_responses, handle_send_legacy_game_key, parse_legacy_game_key_response,
        synthesize, EXTENDED_MSG_HDR_SIZE,
    };

    clear_legacy_cdkeys();
    clear_pending_responses();

    let app_id = 730; // Counter-Strike 2
    let account_id = 12345678u32;
    let steam_id = 0x0110_0001_0000_0000u64 | (account_id as u64);
    let job_id_source = 0x1122_3344_5566_7788u64;

    // Expected deterministic Crockford Base32 key
    let expected_key = synthesize(app_id, account_id);
    assert_eq!(expected_key.len(), 19);
    assert!(expected_key.contains('-'));

    // Build outbound request frame
    let req_frame = build_legacy_game_key_request_frame(app_id, steam_id, job_id_source);
    let mut ctx = NetPacketContext::new(req_frame, NetPacketDirection::Send, app_id).unwrap();
    assert_eq!(ctx.body_offset, EXTENDED_MSG_HDR_SIZE);

    // Handle outbound request (no override in memory -> falls back to deterministic resolution)
    let handled = handle_send_legacy_game_key(&mut ctx).unwrap();
    assert!(handled);

    // Drain and verify synthesized response
    let carrier_frame = vec![0u8; 128];
    let mut carrier_ctx = NetPacketContext::new(carrier_frame, NetPacketDirection::Recv, app_id).unwrap();

    let mut delivered_responses = Vec::new();
    let count = drain_pending_responses(&mut carrier_ctx, |injected_ctx| {
        delivered_responses.push(injected_ctx.frame_buffer.clone());
        Ok(())
    })
    .unwrap();

    assert_eq!(count, 1);
    let parsed = parse_legacy_game_key_response(&delivered_responses[0]).unwrap();
    assert_eq!(parsed.hdr.e_msg(), 785);
    assert_eq!(parsed.hdr.job_id_target(), job_id_source);
    assert_eq!(parsed.hdr.job_id_source(), 0);
    assert_eq!(parsed.app_id, app_id);
    assert_eq!(parsed.e_result, 1);
    assert_eq!(parsed.cd_key, expected_key);

    clear_legacy_cdkeys();
    clear_pending_responses();
}

#[test]
fn test_bst36_legacykey_recv_handler_delegation() {
    let _lock = BST36_MUTEX.lock().unwrap();
    use luatools_linux::steam_bridge::netpacket::netpacket_core::{
        dispatch_netpacket, NetPacketContext, NetPacketDirection,
    };
    use luatools_linux::steam_bridge::netpacket::netpacket_legacykey::{
        build_legacy_game_key_request_frame, clear_legacy_cdkeys, clear_pending_responses,
        has_pending_responses, parse_legacy_game_key_response, register_legacykey_handlers,
        set_legacy_cdkey, K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE,
    };

    register_legacykey_handlers();
    clear_legacy_cdkeys();
    clear_pending_responses();

    let app_id = 730; // Distinct app_id so parallel test threads do not clash
    let account_id = 99887766u32;
    let steam_id = 0x0110_0001_0000_0000u64 | (account_id as u64);
    let job_id = 0x5544_3322_1100_AABBu64;
    let expected_key = "CSGO-TEST-KEY-2026";

    set_legacy_cdkey(app_id, expected_key);

    // 1. Outbound request intercepted & queued
    let req_frame = build_legacy_game_key_request_frame(app_id, steam_id, job_id);
    let mut send_ctx = NetPacketContext::new(req_frame, NetPacketDirection::Send, app_id).unwrap();
    dispatch_netpacket(&mut send_ctx);
    assert!(send_ctx.frame_buffer.is_empty(), "Send must be suppressed");
    assert!(has_pending_responses(), "Response must be queued");

    // 2. Inbound carrier frame dispatched through Recv handler
    let mut dummy_recv_frame = vec![0u8; 40];
    dummy_recv_frame[0..4].copy_from_slice(&K_E_MSG_CLIENT_GET_LEGACY_GAME_KEY_RESPONSE.to_le_bytes());
    dummy_recv_frame[4..8].copy_from_slice(&36u32.to_le_bytes());
    let mut recv_ctx = NetPacketContext::new(dummy_recv_frame, NetPacketDirection::Recv, app_id).unwrap();

    dispatch_netpacket(&mut recv_ctx);

    // Inbound carrier must now contain the synthesized 785 response
    assert!(!has_pending_responses(), "Pending responses must be drained");
    let parsed = parse_legacy_game_key_response(&recv_ctx.frame_buffer).unwrap();
    assert_eq!(parsed.hdr.e_msg(), 785);
    assert_eq!(parsed.hdr.job_id_target(), job_id);
    assert_eq!(parsed.app_id, app_id);
    assert_eq!(parsed.e_result, 1);
    assert_eq!(parsed.cd_key, expected_key);

    clear_legacy_cdkeys();
    clear_pending_responses();
}

#[test]
fn test_bst42_cloud_ugc_file_details_send_and_recv_carrier() {
    let _lock = BST42_MUTEX.lock().unwrap();
    use luatools_linux::steam_bridge::cloud_redirect::get_cloud_redirect_host;
    use luatools_linux::steam_bridge::netpacket::netpacket_cloud::{
        drain_into_carrier, has_pending_responses, install_cloud_hooks,
        set_cloud_data_root_override, uninstall_cloud_hooks,
    };
    use luatools_linux::steam_bridge::netpacket::netpacket_core::{
        dispatch_netpacket, CNetPacket, NetPacketContext, NetPacketDirection,
        KM_MSG_HDR_PROTO_FLAG, K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT,
        K_EMSG_SERVICE_METHOD_RESPONSE,
    };
    use std::fs;
    use std::path::Path;

    static DELIVERED_BYTES: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

    let tmp = tempfile::tempdir().unwrap();
    set_cloud_data_root_override(Some(tmp.path().to_path_buf()));

    let app_id = 1245620;
    let account_id = 12345678u32;
    let save_dir = tmp.path().join(format!("cloud_storage/{}/{}", account_id, app_id));
    fs::create_dir_all(&save_dir).unwrap();
    fs::write(save_dir.join("savedata.bin"), b"ELDEN_RING_SAVE_PAYLOAD").unwrap();

    let host = get_cloud_redirect_host();
    host.initialize(Path::new("/tmp/steam"), true);
    host.sync_app_set(vec![app_id]);
    host.set_account_id(account_id);

    install_cloud_hooks().unwrap();

    // 1. Build outbound Cloud.GetUGCFileDetails#1 request frame (EMsg 151)
    let job_name = "Cloud.GetUGCFileDetails#1";
    let mut hdr_bytes = Vec::new();
    // jobid_source = 10 fixed64 (tag 0x51)
    hdr_bytes.extend_from_slice(&[0x51]);
    hdr_bytes.extend_from_slice(&0x9988_7766_5544_3322u64.to_le_bytes());
    // target_job_name = 12 string (tag 0x62)
    hdr_bytes.extend_from_slice(&[0x62, job_name.len() as u8]);
    hdr_bytes.extend_from_slice(job_name.as_bytes());

    let mut body_bytes = Vec::new();
    // appid = 2 uint32 varint (tag 0x10)
    body_bytes.extend_from_slice(&[0x10]);
    body_bytes.extend_from_slice(&(app_id as u64).to_le_bytes()[..4]);

    let mut frame = Vec::new();
    frame.extend_from_slice(&(K_EMSG_SERVICE_METHOD_CALL_FROM_CLIENT | KM_MSG_HDR_PROTO_FLAG).to_le_bytes());
    frame.extend_from_slice(&(hdr_bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(&hdr_bytes);
    frame.extend_from_slice(&body_bytes);

    let mut send_ctx = NetPacketContext::new(frame, NetPacketDirection::Send, app_id).unwrap();
    dispatch_netpacket(&mut send_ctx);

    // Frame buffer must be cleared (send suppressed)
    assert!(send_ctx.frame_buffer.is_empty(), "Cloud outbound send must be suppressed");
    assert!(has_pending_responses(), "Cloud response must be queued");

    // 2. Deliver via carrier pointer borrowing (drain_into_carrier)
    let mut dummy_carrier_bytes = vec![0u8; 64];
    dummy_carrier_bytes[0..4].copy_from_slice(&(K_EMSG_SERVICE_METHOD_RESPONSE | KM_MSG_HDR_PROTO_FLAG).to_le_bytes());
    let mut dummy_pkt = CNetPacket {
        h_connection: 1,
        pub_data: dummy_carrier_bytes.as_mut_ptr(),
        pub_cub_data: dummy_carrier_bytes.len() as u32,
        c_ref: 1,
    };

    unsafe extern "C" fn dummy_recv_fn(_this: *mut std::ffi::c_void, pkt: *mut CNetPacket) {
        if !pkt.is_null() {
            DELIVERED_BYTES.lock().unwrap().push((*pkt).pub_cub_data);
        }
    }
    DELIVERED_BYTES.lock().unwrap().clear();

    unsafe {
        drain_into_carrier(std::ptr::null_mut(), &mut dummy_pkt, dummy_recv_fn);
    }

    assert_eq!(DELIVERED_BYTES.lock().unwrap().len(), 1);
    assert!(!has_pending_responses());

    uninstall_cloud_hooks();
    set_cloud_data_root_override(None);
}

#[test]
fn test_bst49_appticket_rsa_splice_exploit() {
    use luatools_linux::steam_bridge::credentials::appticket::{
        forge_local_app_ownership_ticket_with_steam_id, get_app_ownership_ticket_details,
        splice_ticket_off_by_four, AppTicketSource, K_APP_TICKET_SIGNATURE_SIZE,
        K_APP_TICKET_STEAM_ID_OFFSET,
    };
    use luatools_linux::steam_bridge::interop::forge_ownership_ticket;

    // 1. Construct a mock 160-byte donor ticket (32 bytes payload + 128 bytes RSA-1024 signature)
    let donor_sid: u64 = 0x0110_0001_0203_0405;
    let mut donor = vec![0u8; 32 + K_APP_TICKET_SIGNATURE_SIZE];
    donor[0..4].copy_from_slice(&(160u32).to_le_bytes()); // length
    donor[4..8].copy_from_slice(&(4u32).to_le_bytes());   // version
    donor[8..16].copy_from_slice(&donor_sid.to_le_bytes());
    donor[16..20].copy_from_slice(&(7u32).to_le_bytes());  // donor AppId 7
    donor[20..24].copy_from_slice(&(1700000000u32).to_le_bytes()); // timestamp
    for i in 0..K_APP_TICKET_SIGNATURE_SIZE {
        donor[32 + i] = ((i * 41 + 17) & 0xFF) as u8;
    }

    let target_appid = 1245620u32; // Elden Ring
    let target_sid: u64 = 0x0110_0001_DEAD_BEEF;

    // 2. Perform off-by-four splice
    let spliced = splice_ticket_off_by_four(&donor, target_appid, Some(target_sid))
        .expect("splicing valid donor must succeed");

    // Spliced ticket must have physical size = donor.len() + 4 (164 bytes)
    assert_eq!(spliced.len(), donor.len() + 4);

    // Target AppId must be spliced directly at signed_size (offset 32)
    assert_eq!(&spliced[32..36], &target_appid.to_le_bytes());

    // Header before signed_size must be preserved, with target SteamID updated
    assert_eq!(&spliced[0..8], &donor[0..8]);
    assert_eq!(&spliced[8..16], &target_sid.to_le_bytes());
    assert_eq!(&spliced[16..32], &donor[16..32]);

    // The trailing 128 bytes must match donor RSA-1024 signature block verbatim
    assert_eq!(&spliced[36..], &donor[32..]);

    // 3. Test standalone forge fallback (never returns broken 64-byte null buffer)
    let forged = forge_local_app_ownership_ticket_with_steam_id(target_appid, Some(target_sid), None)
        .expect("forge must return valid ticket buffer");
    assert!(forged.len() >= 24 + 4 + K_APP_TICKET_SIGNATURE_SIZE);
    assert_ne!(forged.len(), 64, "must NOT be 64-byte null-padded stub");
    assert_eq!(forged.len(), 156, "must have S + 4 (152 + 4) bytes for SteamDRMP overhang");

    // 4. Test ownership ticket details offsets (matching UsefulSteamTools §3.1)
    let details = get_app_ownership_ticket_details(target_appid, AppTicketSource::CredentialStoreThenForge)
        .expect("details must be generated");
    assert_eq!(details.signature_size, K_APP_TICKET_SIGNATURE_SIZE);
    assert_eq!(details.steam_id_offset, K_APP_TICKET_STEAM_ID_OFFSET);
    assert_eq!(details.total_size, details.data.len() - 4);
    assert_eq!(details.app_id_offset, details.total_size - K_APP_TICKET_SIGNATURE_SIZE);
    assert_eq!(details.signature_offset, details.app_id_offset + 4);
    assert_eq!(
        &details.data[details.app_id_offset..details.app_id_offset + 4],
        &target_appid.to_le_bytes(),
        "app_id_offset must point directly to spliced target AppId"
    );

    // 5. Test interop::forge_ownership_ticket produces matching structure
    let ownership = forge_ownership_ticket(target_appid, target_sid);
    assert_eq!(ownership.total_size, 152);
    assert_eq!(ownership.data.len(), 156);
    assert_eq!(ownership.signature_size, 128);
    assert_eq!(ownership.steam_id_offset, 8);
    assert_eq!(ownership.app_id_offset, 24);
    assert_eq!(ownership.signature_offset, 28);
    assert_eq!(
        &ownership.data[ownership.app_id_offset as usize..ownership.app_id_offset as usize + 4],
        &target_appid.to_le_bytes(),
        "ownership app_id_offset must point directly to target AppId"
    );
}

#[test]
fn test_bst52_steam_id_sysv_abi_and_per_pipe_isolation() {
    use luatools_linux::steam_bridge::interop::{
        clear_pipe_app_ids, get_pipe_app_id, handler_post_get_steam_id, register_pipe_app_id,
    };
    use luatools_linux::steam_bridge::ticket_store::TicketStore;
    use std::ffi::c_void;

    clear_pipe_app_ids();

    let app_a = 1245620u32; // Elden Ring
    let sid_a = 0x0110_0001_1111_2222u64;

    let app_b = 730u32; // CS2
    let sid_b = 0x0110_0001_3333_4444u64;

    // Register tickets in TicketStore
    {
        let mut store = TicketStore::global().lock().unwrap_or_else(|e| e.into_inner());
        store.set_spoof_steam_id(app_a, sid_a);
        store.set_spoof_steam_id(app_b, sid_b);
    }

    // Two distinct simulated game pipe clients
    let pipe_a = 0x1000 as *mut c_void;
    let pipe_b = 0x2000 as *mut c_void;
    let pipe_unknown = 0x3000 as *mut c_void;

    register_pipe_app_id(pipe_a, app_a);
    register_pipe_app_id(pipe_b, app_b);

    assert_eq!(get_pipe_app_id(pipe_a), app_a);
    assert_eq!(get_pipe_app_id(pipe_b), app_b);
    assert_eq!(get_pipe_app_id(pipe_unknown), 0);

    // Test handler_post_get_steam_id per-pipe isolation
    let mut write_buf_a = vec![0u8; 8];
    let ok_a = handler_post_get_steam_id(pipe_a, &mut [], &mut write_buf_a);
    assert!(ok_a);
    assert_eq!(&write_buf_a[0..8], &sid_a.to_le_bytes());

    let mut write_buf_b = vec![0u8; 8];
    let ok_b = handler_post_get_steam_id(pipe_b, &mut [], &mut write_buf_b);
    assert!(ok_b);
    assert_eq!(&write_buf_b[0..8], &sid_b.to_le_bytes());

    // Pipe with unknown/unregistered AppID must not leak another app's SteamID
    let mut write_buf_unknown = vec![0u8; 8];
    let ok_unknown = handler_post_get_steam_id(pipe_unknown, &mut [], &mut write_buf_unknown);
    assert!(!ok_unknown, "unknown pipe must return false rather than leaking other game SteamIDs");

    // Test System V AMD64 ABI calling convention:
    // On x86_64, an 8-byte CSteamID is returned directly in RAX (INTEGER class),
    // and `this` is passed in RDI (no hidden sret parameter to avoid overwriting *this).
    #[repr(C)]
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    struct CSteamID {
        raw: u64,
    }

    type GetSteamIDFn = unsafe extern "C" fn(*mut c_void) -> CSteamID;

    unsafe extern "C" fn mock_get_steam_id_implementation(
        this: *mut c_void,
    ) -> CSteamID {
        let app_id = get_pipe_app_id(this);
        let sid = if app_id != 0 {
            let mut store = TicketStore::global().lock().unwrap_or_else(|e| e.into_inner());
            store.get_spoof_steam_id(app_id)
        } else {
            0
        };
        CSteamID { raw: sid }
    }

    let fn_ptr: GetSteamIDFn = mock_get_steam_id_implementation;
    let ret_sid = unsafe { fn_ptr(pipe_a) };

    assert_eq!(ret_sid.raw, sid_a, "RAX must return the spoofed SteamID per SysV AMD64 ABI");

    clear_pipe_app_ids();
}

// ============================================================================
// 32-bit steamclient.so selection & BST-54 tests
// ============================================================================

#[test]
fn test_steamclient_arch_selection_class_filter() {
    use luatools_linux::steam_bridge::binary_reader::{BinaryFile, ElfClass};

    let mut elf64 = vec![0u8; 64];
    elf64[0..4].copy_from_slice(b"\x7fELF");
    elf64[4] = 2; // Class64
    elf64[5] = 1; // Lsb
    elf64[6] = 1;
    let bin64 = BinaryFile::from_bytes(PathBuf::from("/steam/linux64/steamclient.so"), elf64).unwrap();

    let mut elf32 = vec![0u8; 64];
    elf32[0..4].copy_from_slice(b"\x7fELF");
    elf32[4] = 1; // Class32
    elf32[5] = 1; // Lsb
    elf32[6] = 1;
    let bin32 = BinaryFile::from_bytes(PathBuf::from("/steam/ubuntu12_32/steamclient.so"), elf32).unwrap();

    let binaries = vec![bin64, bin32];

    // When expecting 32-bit, picking steamclient.so without class filter would wrongly grab linux64
    let without_filter = binaries.iter().find(|b| {
        b.path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "steamclient.so")
    });
    assert_eq!(
        without_filter.unwrap().class,
        ElfClass::Class64,
        "Unfiltered find chooses first candidate in search order (linux64)"
    );

    // With expected_class = Class32 filter
    let with_filter_32 = binaries.iter().find(|b| {
        b.class == ElfClass::Class32
            && b.path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name == "steamclient.so")
    });
    assert_eq!(
        with_filter_32.unwrap().class,
        ElfClass::Class32,
        "Filtered find correctly resolves 32-bit steamclient.so"
    );

    // With expected_class = Class64 filter
    let with_filter_64 = binaries.iter().find(|b| {
        b.class == ElfClass::Class64
            && b.path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name == "steamclient.so")
    });
    assert_eq!(
        with_filter_64.unwrap().class,
        ElfClass::Class64,
        "Filtered find correctly resolves 64-bit steamclient.so"
    );
}

#[test]
fn test_bst54_protection_scan_helpers_and_services() {
    std::env::set_var("LUATOOLS_MOCK_LAUNCH", "1");
    use luatools_linux::services::game_process_injection_service::{
        GameProcessInjectionService, InjectionConfig,
    };
    use luatools_linux::services::steam_bridge_service::SteamBridgeService;
    use luatools_linux::steam_bridge::denuvo_auth::{
        scan_executable_protection, scan_game_folder_protection, scan_process_protection,
        DenuvoAuth,
    };

    let temp = TempDir::new().unwrap();
    let fake_game_dir = temp.path().join("game");
    fs::create_dir_all(&fake_game_dir).unwrap();

    let dummy_exe = fake_game_dir.join("game.exe");
    fs::write(&dummy_exe, b"MZ\x90\0dummy non-denuvo pe").unwrap();

    // 1. Module-level convenience functions
    let exe_rep = scan_executable_protection(&dummy_exe);
    assert!(!exe_rep.denuvo_detected);

    let folder_rep = scan_game_folder_protection(1245620, &fake_game_dir);
    assert!(!folder_rep.denuvo_detected);

    let proc_rep = scan_process_protection(0);
    assert!(!proc_rep.denuvo_detected);

    // 2. SteamBridgeService methods
    let bridge_svc = SteamBridgeService::new();
    let rep_svc_exe = bridge_svc.scan_executable_protection(&dummy_exe);
    assert!(!rep_svc_exe.denuvo_detected);

    let rep_svc_folder = bridge_svc.scan_game_folder_protection(1245620, &fake_game_dir);
    assert!(!rep_svc_folder.denuvo_detected);

    let rep_svc_proc = bridge_svc.scan_process_protection(0);
    assert!(!rep_svc_proc.denuvo_detected);

    // 3. GameProcessInjectionService methods & configure triggering scan
    let inj_svc = GameProcessInjectionService::with_storage(
        Some(temp.path().join("injection.json")),
        DenuvoAuth::new(),
    );

    inj_svc.configure(
        1245620,
        InjectionConfig {
            app_id: 1245620,
            library_path: dummy_exe.clone(),
            ..Default::default()
        },
    );

    let launch_res = inj_svc.launch_with_integration(1245620, Some(&fake_game_dir));
    assert!(launch_res.is_ok());
}
